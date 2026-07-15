// call_native: 모든 네이티브(내장) 메서드/함수 디스패치. interp/mod.rs 에서 분리.
use super::*;
use super::net::{build_query, parse_query};
use super::value::*;
use std::cell::RefCell;
use std::rc::Rc;

// URI 인코딩: 비예약문자(A-Za-z0-9 -_.!~*'()) 와 extra_safe 는 보존, 나머지는
// UTF-8 바이트별 %XX. encodeURI 는 예약문자를 extra_safe 로 넘겨 보존한다.
// 배열을 제자리에서 바꾸는 연산인가. 읽기 전용 연산은 array-like 대상을 건드리면 안 된다
// (indexOf.call(obj,x) 가 obj 에 own length/인덱스를 심는 부작용이 있었다).
fn is_mutating_arr_op(op: ArrOp) -> bool {
    matches!(
        op,
        ArrOp::Pop
            | ArrOp::Splice
            | ArrOp::Shift
            | ArrOp::Unshift
            | ArrOp::Reverse
            | ArrOp::Sort
            | ArrOp::Fill
    )
}

// wasm 인자 헬퍼: n번째 인자를 수로 (없으면 0)
fn num_arg(args: &[Value], i: usize) -> f64 {
    args.get(i).map(to_num).unwrap_or(0.0)
}

// 바이트 배열 값 → Vec<u8>. 프렐류드의 __kWasmBytes 가 항상 평범한 숫자 배열로 정규화해
// 넘겨 주므로 여기서 프록시/뷰를 다시 풀 필요가 없다.
fn bytes_of(v: Option<&Value>) -> Vec<u8> {
    match v {
        Some(Value::Arr(a)) => a
            .borrow()
            .iter()
            .map(|x| match x {
                Value::Num(n) => *n as i64 as u8,
                _ => 0,
            })
            .collect(),
        _ => Vec::new(),
    }
}

// 저장 키("\u{0}@@…")에서 Symbol 값을 복원한다 — getOwnPropertySymbols 용.
// 일반 심볼 키는 "\0@@sym:<n>:<desc>", 레지스트리는 "\0@@for:<k>", 그 외는 잘 알려진 심볼.
fn symbol_from_key(key: &str) -> Value {
    let body = key.strip_prefix("\u{0}@@").unwrap_or(key);
    let desc = if let Some(rest) = body.strip_prefix("sym:") {
        // "<n>:<desc>" — 설명은 비어 있을 수 있다(Symbol())
        rest.split_once(':').map(|(_, d)| d.to_string()).filter(|d| !d.is_empty())
    } else if let Some(k) = body.strip_prefix("for:") {
        Some(k.to_string())
    } else {
        Some(format!("Symbol.{}", body)) // 잘 알려진 심볼
    };
    Value::Symbol(Rc::new(super::SymbolData { key: key.to_string(), desc }))
}

// 문서 순서(preorder) 인덱스 — compareDocumentPosition 용.
fn preorder_index(
    dom: &crate::dom::Dom,
    id: crate::dom::NodeId,
    target: crate::dom::NodeId,
    counter: &mut usize,
) -> Option<usize> {
    let my = *counter;
    *counter += 1;
    if id == target {
        return Some(my);
    }
    for &c in &dom.get(id).children {
        if let Some(r) = preorder_index(dom, c, target, counter) {
            return Some(r);
        }
    }
    None
}

// ── 배열 메서드의 generic(array-like) 지원 ─────────────────────────
// 표준의 배열 메서드는 배열이 아닌 "length 를 가진 객체"에도 동작한다.
// jQuery 가 이걸 핵심으로 쓴다: `var push = arr.push; push.apply(jqObj, elems)`
// (jqObj 는 length 만 있는 array-like). 예전엔 "push 는 배열 메서드" 로 즉사했다.

// length 를 프로토타입 체인까지 따라 찾는다. jQuery 객체는 length:0 이 own 이 아니라
// 프로토타입(jQuery.fn)에 있어서, own 만 보면 array-like 로 인식되지 않는다.
fn lookup_length(o: &Rc<RefCell<ObjMap>>) -> Option<f64> {
    let mut cur = o.clone();
    for _ in 0..100 {
        let (len, proto) = {
            let b = cur.borrow();
            (b.get("length").map(to_num), b.get("__proto__").cloned())
        };
        if let Some(n) = len {
            return Some(n);
        }
        match proto {
            Some(Value::Obj(p)) => cur = p,
            _ => return None,
        }
    }
    None
}

fn is_array_like(o: &Rc<RefCell<ObjMap>>) -> bool {
    matches!(lookup_length(o), Some(n) if n.is_finite() && n >= 0.0)
}

// Number.prototype.toString(radix) 의 진법 변환 (§21.1.3.6). 정수부와 소수부를
// 모두 변환한다. 예전엔 정수만 변환하고 소수는 base-10 으로 흘려 (10.5).toString(2)
// 가 "10.5" 였다.
fn num_to_radix(n: f64, radix: u32) -> String {
    if n.is_nan() {
        return "NaN".to_string();
    }
    if n.is_infinite() {
        return if n < 0.0 { "-Infinity" } else { "Infinity" }.to_string();
    }
    if n == 0.0 {
        return "0".to_string();
    }
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let neg = n < 0.0;
    let x = n.abs();
    let mut int_part = x.trunc();
    let mut frac = x - int_part;
    // 정수부
    let mut int_buf = Vec::new();
    if int_part == 0.0 {
        int_buf.push(b'0');
    } else {
        while int_part > 0.0 {
            let d = (int_part % radix as f64) as usize;
            int_buf.push(DIGITS[d]);
            int_part = (int_part / radix as f64).trunc();
        }
        int_buf.reverse();
    }
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    out.push_str(&String::from_utf8_lossy(&int_buf));
    // 소수부: radix 를 곱해가며 자릿수 추출. f64 정밀도(약 1100비트까지 가능하나
    // 실용상 제한)까지, 0 이 되면 종료.
    if frac > 0.0 {
        out.push('.');
        let mut count = 0;
        // f64 유효 정밀도에 맞춰 최대 자릿수 제한 (repeating 소수 무한루프 방지)
        let max_digits = (1100.0 / (radix as f64).log2()).min(1100.0) as usize;
        while frac > 0.0 && count < max_digits {
            frac *= radix as f64;
            let d = frac.trunc() as usize;
            out.push(DIGITS[d.min(35)] as char);
            frac -= d as f64;
            count += 1;
        }
    }
    out
}

// array-like 의 length 를 실제 길이로. 배열 최대 길이(2^32-1)를 넘으면 RangeError 다
// (표준 §10.4.2.2 ArrayCreate). 예전엔 상한이 없어서 core-js 의 기능 탐지
// (Array.from({length: 2**32}))가 40억 개 할당을 시도해 프로세스가 통째로 죽었다.
const MAX_ARRAY_LEN: f64 = 4_294_967_295.0; // 2^32 - 1

fn array_like_len(len: f64) -> Result<usize, String> {
    if !len.is_finite() || len <= 0.0 {
        return Ok(0);
    }
    if len > MAX_ARRAY_LEN {
        return Err("RangeError: Invalid array length".to_string());
    }
    Ok(len as usize)
}

fn array_like_to_vec(o: &Rc<RefCell<ObjMap>>) -> Result<Vec<Value>, String> {
    let len = array_like_len(lookup_length(o).unwrap_or(0.0))?;
    let b = o.borrow();
    Ok((0..len).map(|i| b.get(&i.to_string()).cloned().unwrap_or(Value::Undefined)).collect())
}

// 변형 결과를 array-like 객체에 되쓴다 (인덱스 + length, 남는 인덱스는 제거).
fn write_back_array_like(o: &Rc<RefCell<ObjMap>>, items: &[Value]) {
    let old = lookup_length(o).unwrap_or(0.0);
    let old = if old.is_finite() && old > 0.0 { old as usize } else { 0 };
    let mut b = o.borrow_mut();
    for i in items.len()..old {
        b.remove(&i.to_string());
    }
    for (i, v) in items.iter().enumerate() {
        b.insert(i.to_string(), v.clone());
    }
    b.insert("length".to_string(), Value::Num(items.len() as f64));
}

// 값의 own enumerable 프로퍼티 (키, 값) — Object.assign/스프레드의 소스 열거.
// 엔진 내부 마커(__proto__/@@심볼 등)는 제외한다.
// 부모 생성자가 만들어 돌려준 객체를 this 로 옮길 때 쓰는 own 프로퍼티 전량.
// 열거 가능 여부와 무관하게 다 옮긴다 — Error 의 message/stack 은 비열거라서,
// 열거 가능한 것만 옮기면 `class E extends Error` 인스턴스의 message 가 사라진다.
// 비열거 표식(\0ne:*)도 함께 옮겨 속성까지 보존한다. __proto__ 는 제외(파생 클래스의
// 프로토타입 체인을 덮어쓰면 안 된다).
pub(super) fn own_entries_all(v: &Value) -> Vec<(String, Value)> {
    match v {
        Value::Obj(m) => m
            .borrow()
            .iter()
            .filter(|(k, _)| k.as_str() != "__proto__")
            .map(|(k, val)| (k.clone(), val.clone()))
            .collect(),
        other => own_enumerable_entries(other),
    }
}

pub(super) fn own_enumerable_entries(v: &Value) -> Vec<(String, Value)> {
    match v {
        Value::Obj(m) => enumerable_entries(m),
        // 배열: 인덱스 + own-property (push 재정의 등)
        Value::Arr(a) => {
            let mut out: Vec<(String, Value)> = a
                .borrow()
                .iter()
                .enumerate()
                .map(|(i, val)| (i.to_string(), val.clone()))
                .collect();
            out.extend(a.own_props());
            out
        }
        Value::Instance(i) => {
            let f = i.fields.borrow();
            f.iter()
                .filter(|(k, _)| {
                    !is_internal_key(k) && !f.contains_key(&nonenum_marker(k))
                })
                .map(|(k, val)| (k.clone(), val.clone()))
                .collect()
        }
        // 함수도 객체 — F.staticProp 복사 (번들의 Object.assign(Fn, {...}) 패턴)
        Value::Fn(f) => f
            .props
            .borrow()
            .iter()
            .filter(|(k, _)| k.as_str() != "prototype")
            .map(|(k, val)| (k.clone(), val.clone()))
            .collect(),
        Value::Class(c) => {
            let st = c.statics.borrow();
            st.iter()
                .filter(|(k, _)| {
                    !is_internal_key(k)
                        && !is_private_name(k)
                        && !st.contains_key(&nonenum_marker(k))
                })
                .map(|(k, val)| (k.clone(), val.clone()))
                .collect()
        }
        Value::Str(s) => s
            .chars()
            .enumerate()
            .map(|(i, ch)| (i.to_string(), Value::Str(ch.to_string())))
            .collect(),
        _ => Vec::new(),
    }
}

// 대상에 own 프로퍼티 설정 (Object.assign 의 대상 쓰기). frozen/sealed 는 존중.
// (Interp 메서드로 이동 — 무결성(freeze/seal) 상태를 봐야 하므로 self 가 필요하다)

pub(super) fn uri_encode(s: &str, extra_safe: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() || "-_.!~*'()".contains(ch) || extra_safe.contains(ch) {
            out.push(ch);
        } else {
            let mut buf = [0u8; 4];
            for &b in ch.encode_utf8(&mut buf).as_bytes() {
                out.push('%');
                out.push(char::from_digit((b >> 4) as u32, 16).unwrap().to_ascii_uppercase());
                out.push(char::from_digit((b & 0xf) as u32, 16).unwrap().to_ascii_uppercase());
            }
        }
    }
    out
}

// URI 디코딩: %XX 를 바이트로 모아 UTF-8 해석. 유효하지 않은 %는 그대로 통과.
pub(super) fn uri_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// 수신자 객체의 문자열 프로퍼티 (URL/URLSearchParams 네이티브 메서드용).
fn recv_prop_str(recv: &Option<Value>, key: &str) -> String {
    if let Some(Value::Obj(o)) = recv {
        if let Some(v) = o.borrow().get(key) {
            return to_display(v);
        }
    }
    String::new()
}





// DocumentFragment 판별 (센티널 태그)
fn is_fragment(dom: &crate::dom::Dom, id: crate::dom::NodeId) -> bool {
    matches!(&dom.get(id).node_type,
        crate::dom::NodeType::Element(e) if e.tag_name == "#document-fragment")
}

// 문서의 body (없으면 루트)
fn find_body(dom: &crate::dom::Dom) -> crate::dom::NodeId {
    fn walk(dom: &crate::dom::Dom, id: crate::dom::NodeId) -> Option<crate::dom::NodeId> {
        if let crate::dom::NodeType::Element(e) = &dom.get(id).node_type {
            if e.tag_name == "body" {
                return Some(id);
            }
        }
        dom.get(id).children.iter().find_map(|&c| walk(dom, c))
    }
    walk(dom, dom.root).unwrap_or(dom.root)
}

// 서브트리의 <script> 노드를 문서 순서로 모은다
fn collect_script_nodes(
    dom: &crate::dom::Dom,
    id: crate::dom::NodeId,
    out: &mut Vec<crate::dom::NodeId>,
) {
    if let crate::dom::NodeType::Element(e) = &dom.get(id).node_type {
        if e.tag_name == "script" {
            out.push(id);
        }
    }
    for &c in &dom.get(id).children {
        collect_script_nodes(dom, c, out);
    }
}

impl Interp {
    // JSON.stringify 의 재귀 직렬화 (표준 §25.5.2). toJSON → replacer 함수 → 직렬화 순서.
    // replacer 배열이면 객체 키를 그 목록으로 거른다. indent 가 있으면 표준 들여쓰기.
    #[allow(clippy::too_many_arguments)]
    fn json_ser(
        &mut self,
        v: &Value,
        key: &str,
        holder: &Value,
        fnrep: &Option<Value>,
        keys: &Option<Vec<String>>,
        indent: &str,
        depth: usize,
        path: &mut Vec<usize>,
    ) -> Result<Option<String>, String> {
        // 1) toJSON(key) 가 있으면 그 반환값을 직렬화한다 (Date 등).
        let mut v = v.clone();
        if matches!(v, Value::Obj(_) | Value::Instance(_) | Value::Arr(_)) {
            let tj = self.member_get(&v, "toJSON").unwrap_or(Value::Undefined);
            if matches!(tj, Value::Fn(_) | Value::Native(_) | Value::Bound(_)) {
                v = self.call_value(tj, Some(v.clone()), vec![Value::Str(key.to_string())])?;
            }
        }
        // 2) replacer 함수는 모든 (키, 값) 쌍에 적용된다 (루트는 키 "").
        if let Some(f) = fnrep {
            v = self.call_value(
                f.clone(),
                Some(holder.clone()),
                vec![Value::Str(key.to_string()), v.clone()],
            )?;
        }
        // 3) 순환 검사 (신원 기반 — 깊이 가드로는 자기참조 다중 분기를 못 막는다)
        let ident: Option<usize> = match &v {
            Value::Obj(m) => Some(Rc::as_ptr(m) as usize),
            Value::Arr(a) => Some(Rc::as_ptr(a) as usize),
            Value::Instance(i) => Some(Rc::as_ptr(i) as usize),
            _ => None,
        };
        if let Some(id) = ident {
            if path.contains(&id) {
                return Err(JSON_CYCLE_MSG.to_string());
            }
            path.push(id);
        }
        let out = self.json_ser_body(&v, fnrep, keys, indent, depth, path);
        if ident.is_some() {
            path.pop();
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn json_ser_body(
        &mut self,
        v: &Value,
        fnrep: &Option<Value>,
        keys: &Option<Vec<String>>,
        indent: &str,
        depth: usize,
        path: &mut Vec<usize>,
    ) -> Result<Option<String>, String> {
        // 들여쓰기 조각: 여는 괄호 뒤 개행 + (depth+1) 단, 닫는 괄호 앞 개행 + depth 단.
        let (nl, pad_in, pad_out, colon) = if indent.is_empty() {
            (String::new(), String::new(), String::new(), ":".to_string())
        } else {
            (
                "\n".to_string(),
                indent.repeat(depth + 1),
                indent.repeat(depth),
                ": ".to_string(),
            )
        };
        let wrap = |parts: Vec<String>, open: char, close: char| -> String {
            if parts.is_empty() {
                return format!("{}{}", open, close);
            }
            format!(
                "{}{}{}{}{}{}{}",
                open,
                nl,
                pad_in,
                parts.join(&format!(",{}{}", nl, pad_in)),
                nl,
                pad_out,
                close
            )
        };
        if let Value::BigInt(_) = v {
            // §25.5.2: BigInt 직렬화는 TypeError.
            return Err(self.throw_error("TypeError", "Do not know how to serialize a BigInt"));
        }
        Ok(match v {
            Value::Undefined
            | Value::Fn(_)
            | Value::Native(_)
            | Value::Dom(_)
            | Value::Attr(_, _)
            | Value::Sheet(_)
            | Value::CssRule(_, _)
            | Value::RuleStyle(_, _)
            | Value::Class(_)
            | Value::Bound(_)
            | Value::Accessor(_)
            | Value::MapVal(_)
            | Value::SetVal(_)
            | Value::Style(_)
            | Value::Dataset(_)
            | Value::ClassList(_)
            | Value::Proxy(_)
            | Value::Gen(_)
            | Value::Symbol(_)
            | Value::ComputedStyle(_) => None,
            | Value::BigInt(_) => None, // 위에서 TypeError 로 처리됨
            Value::Null => Some("null".to_string()),
            Value::Bool(b) => Some(b.to_string()),
            Value::Num(n) => Some(json_num(*n)),
            Value::Str(s) => Some(json_quote_pub(s)),
            Value::Arr(a) => {
                let items = a.borrow().clone();
                let mut parts = Vec::with_capacity(items.len());
                for (i, item) in items.iter().enumerate() {
                    let s = self
                        .json_ser(item, &i.to_string(), v, fnrep, keys, indent, depth + 1, path)?
                        .unwrap_or_else(|| "null".to_string()); // 배열의 직렬화 불가 항목은 null
                    parts.push(s);
                }
                Some(wrap(parts, '[', ']'))
            }
            // Date 는 toJSON 규약대로 ISO 문자열 (내부 마커 노출 아님)
            Value::Obj(map) if json_is_date(map) => Some(
                json_date_iso(map).map(|s| json_quote_pub(&s)).unwrap_or_else(|| "null".to_string()),
            ),
            Value::Obj(map) => {
                let entries: Vec<(String, Value)> = enumerable_entries(map);
                let mut parts = Vec::new();
                for (k, val) in &entries {
                    if let Some(ks) = keys {
                        if !ks.contains(k) {
                            continue; // replacer 배열에 없는 키는 제외
                        }
                    }
                    if let Some(s) = self.json_ser(val, k, v, fnrep, keys, indent, depth + 1, path)? {
                        parts.push(format!("{}{}{}", json_quote_pub(k), colon, s));
                    }
                }
                Some(wrap(parts, '{', '}'))
            }
            Value::Instance(inst) => {
                let m = inst.fields.borrow().clone();
                // 내부 키(private 이름, 비열거 표식)는 직렬화하지 않는다
                let mut ks: Vec<&String> =
                    m.keys().filter(|k| !is_internal_key(k) && !m.contains_key(&nonenum_marker(k))).collect();
                ks.sort();
                let mut parts = Vec::new();
                for k in ks {
                    if let Some(list) = keys {
                        if !list.contains(k) {
                            continue;
                        }
                    }
                    if let Some(s) =
                        self.json_ser(&m[k], k, v, fnrep, keys, indent, depth + 1, path)?
                    {
                        parts.push(format!("{}{}{}", json_quote_pub(k), colon, s));
                    }
                }
                Some(wrap(parts, '{', '}'))
            }
        })
    }



    // 정규식 매치 → [full, g1, ...] 배열 (+ index/input/groups own-property)
    pub(super) fn regex_match_array(
        &self,
        chars: &[char],
        mt: &crate::js::regex::Match,
        group_names: &[(String, usize)],
    ) -> Value {
        let mut items = Vec::new();
        for g in &mt.groups {
            match g {
                Some((a, b)) => items.push(Value::Str(chars[*a..*b].iter().collect())),
                None => items.push(Value::Undefined),
            }
        }
        let arr = ArrayObj::new(items);
        // index 는 UTF-16 코드 유닛 오프셋(정규식 엔진은 코드포인트 인덱스).
        let u16_index: usize = chars[..mt.start].iter().map(|c| c.len_utf16()).sum();
        arr.set_prop("index".to_string(), Value::Num(u16_index as f64));
        arr.set_prop("input".to_string(), Value::Str(chars.iter().collect()));
        // groups: 이름 있는 그룹의 {name: value} (없으면 undefined)
        let groups = if group_names.is_empty() {
            Value::Undefined
        } else {
            let mut g = ObjMap::new();
            for (name, idx) in group_names {
                let v = mt
                    .groups
                    .get(*idx)
                    .and_then(|o| o.as_ref())
                    .map(|(a, b)| Value::Str(chars[*a..*b].iter().collect()))
                    .unwrap_or(Value::Undefined);
                g.insert(name.clone(), v);
            }
            Value::Obj(Rc::new(RefCell::new(g)))
        };
        arr.set_prop("groups".to_string(), groups);
        Value::Arr(arr)
    }

    // str.replace/replaceAll: 패턴(문자열/정규식) + 치환(문자열/함수)
    fn str_replace(
        &mut self,
        s: &str,
        pat: &Value,
        repl: &Value,
        all: bool,
    ) -> Result<String, String> {
        let chars: Vec<char> = s.chars().collect();
        if let Some((src, flags)) = regex_src_flags(pat) {
            let re = crate::js::regex::Regex::compile_pattern(&src, &flags)
                .map_err(|e| format!("정규식: {}", e))?;
            let global = all || re.global;
            let mut out = String::new();
            let mut pos = 0usize;
            loop {
                match re.find(&chars, pos) {
                    Some(mt) => {
                        out.extend(&chars[pos..mt.start]);
                        let rep = if is_callable(repl) {
                            let mut cargs =
                                vec![Value::Str(chars[mt.start..mt.end].iter().collect())];
                            for g in mt.groups.iter().skip(1) {
                                cargs.push(match g {
                                    Some((a, b)) => Value::Str(chars[*a..*b].iter().collect()),
                                    None => Value::Undefined,
                                });
                            }
                            cargs.push(Value::Num(mt.start as f64));
                            cargs.push(Value::Str(s.to_string()));
                            to_display(&self.call_value(repl.clone(), None, cargs)?)
                        } else {
                            expand_replacement(&to_display(repl), &chars, &mt)
                        };
                        out.push_str(&rep);
                        // 빈 매치는 한 글자 진행(무한 루프 방지)
                        if mt.end > mt.start {
                            pos = mt.end;
                        } else {
                            if mt.end < chars.len() {
                                out.push(chars[mt.end]);
                            }
                            pos = mt.end + 1;
                        }
                        if !global {
                            out.extend(chars.get(pos.min(chars.len())..).unwrap_or(&[]));
                            break;
                        }
                        if pos > chars.len() {
                            break;
                        }
                    }
                    None => {
                        out.extend(chars.get(pos.min(chars.len())..).unwrap_or(&[]));
                        break;
                    }
                }
            }
            Ok(out)
        } else {
            let needle = to_display(pat);
            if needle.is_empty() {
                return Ok(s.to_string());
            }
            let rep_fn = is_callable(repl);
            let mut out = String::new();
            let mut rest = s;
            let mut consumed = 0usize;
            loop {
                match rest.find(&needle) {
                    Some(idx) => {
                        out.push_str(&rest[..idx]);
                        let at = consumed + idx;
                        if rep_fn {
                            let r = self.call_value(
                                repl.clone(),
                                None,
                                vec![
                                    Value::Str(needle.clone()),
                                    Value::Num(s[..at].encode_utf16().count() as f64), // UTF-16 인덱스
                                    Value::Str(s.to_string()),
                                ],
                            )?;
                            out.push_str(&to_display(&r));
                        } else {
                            out.push_str(&to_display(repl));
                        }
                        let adv = idx + needle.len();
                        consumed = at + needle.len();
                        rest = &rest[adv..];
                        if !all {
                            out.push_str(rest);
                            break;
                        }
                    }
                    None => {
                        out.push_str(rest);
                        break;
                    }
                }
            }
            Ok(out)
        }
    }



    // 체크박스/라디오면 checked 를 뒤집는다. 뒤집었으면 true.
    // 라디오는 **같은 그룹의 다른 라디오를 해제**해야 한다 (HTML §4.10.5.1.12) —
    // 예전엔 그냥 자기만 켜서 두 개가 동시에 켜져 있었다.
    fn pre_click_toggle(&mut self, id: crate::dom::NodeId) -> bool {
        let (ty, name) = {
            let Ok(dom) = self.dom_arena() else { return false };
            let crate::dom::NodeType::Element(e) = &dom.get(id).node_type else { return false };
            if e.tag_name != "input" {
                return false;
            }
            let ty = e.attributes.get("type").map(|t| t.to_ascii_lowercase()).unwrap_or_default();
            if ty != "checkbox" && ty != "radio" {
                return false;
            }
            (ty, e.attributes.get("name").cloned().unwrap_or_default())
        };
        if ty == "radio" {
            // 같은 폼(없으면 문서) 안의 같은 이름 라디오를 모두 해제
            let scope = self.owner_form(id);
            let dom = self.dom_arena().ok().map(|d| d as *mut crate::dom::Dom);
            if let Some(dp) = dom {
                let dom = unsafe { &mut *dp };
                let root = scope.unwrap_or(dom.root);
                let mut peers = Vec::new();
                collect_radio_peers(dom, root, &name, &mut peers);
                for p in peers {
                    if p != id {
                        dom.remove_attr(p, "checked");
                    }
                }
            }
        }
        let dom = match self.dom_arena() {
            Ok(d) => d,
            Err(_) => return false,
        };
        let crate::dom::NodeType::Element(e) = &dom.get(id).node_type else { return false };
        let was = e.attributes.contains_key("checked");
        if was && ty == "checkbox" {
            dom.remove_attr(id, "checked");
        } else {
            dom.set_attr(id, "checked", String::new());
        }
        true
    }

    // click() 의 기본 동작 (preventDefault 안 됐을 때). HTML §8.3.5 의 activation behavior.
    // 체크박스/라디오 토글은 표준상 디스패치 **전**이지만, preventDefault 시 되돌려야 하므로
    // 여기서 한 번에 처리한다 (관측 가능한 결과는 같다 — 핸들러가 checked 를 읽는 경우만
    // 다르고, 그 경우는 아래에서 미리 토글해 맞춘다).
    fn click_default_action(&mut self, id: crate::dom::NodeId) {
        let (tag, ty, href) = {
            let Ok(dom) = self.dom_arena() else { return };
            match &dom.get(id).node_type {
                crate::dom::NodeType::Element(e) => (
                    e.tag_name.clone(),
                    e.attributes.get("type").map(|t| t.to_ascii_lowercase()).unwrap_or_default(),
                    e.attributes.get("href").cloned(),
                ),
                _ => return,
            }
        };
        match tag.as_str() {
            // 링크: 이동 요청 (호출측 렌더러가 새 URL 로 다시 그린다)
            "a" | "area" => {
                if let Some(h) = href {
                    if !h.starts_with('#') && !h.trim().is_empty() {
                        let abs = self.absolute_url(&h);
                        self.navigate_to = Some(abs);
                    }
                }
            }
            // 체크박스/라디오: 토글 후 input 과 change 를 쏜다 (둘 다 버블링).
            // 예전엔 아무 이벤트도 안 쏴서 oninput/onchange 핸들러가 영영 안 돌았다.
            "input" if ty == "checkbox" || ty == "radio" => {
                for kind in ["input", "change"] {
                    let evt = self.make_event(kind, id);
                    self.dispatch_event_value(id, kind, evt);
                }
            }
            // 리셋 버튼: 폼에 reset 이벤트 (HTML §4.10.21.2)
            "button" | "input" if ty == "reset" => {
                if let Some(f) = self.owner_form(id) {
                    let evt = self.make_event("reset", f);
                    self.dispatch_event_value(f, "reset", evt);
                }
            }
            // 제출 버튼 (type=image 도 제출한다 — 표준)
            "button" | "input"
                if ty == "submit" || ty == "image" || (tag == "button" && ty.is_empty()) =>
            {
                if let Some(f) = self.owner_form(id) {
                    let evt = self.make_event("submit", f);
                    self.dispatch_event_value(f, "submit", evt);
                }
            }
            // <summary>: 부모 <details> 의 open 을 뒤집고 toggle 을 쏜다
            "summary" => {
                let parent = {
                    let Ok(dom) = self.dom_arena() else { return };
                    dom.get(id).parent.filter(|&p| {
                        matches!(&dom.get(p).node_type,
                            crate::dom::NodeType::Element(e) if e.tag_name == "details")
                    })
                };
                if let Some(d) = parent {
                    let dom = match self.dom_arena() {
                        Ok(x) => x,
                        Err(_) => return,
                    };
                    let open = matches!(&dom.get(d).node_type,
                        crate::dom::NodeType::Element(e) if e.attributes.contains_key("open"));
                    if open {
                        dom.remove_attr(d, "open");
                    } else {
                        dom.set_attr(d, "open", String::new());
                    }
                    let evt = self.make_event("toggle", d);
                    self.dispatch_event_value(d, "toggle", evt);
                }
            }
            _ => {}
        }
    }

    // 이 요소를 감싸는 <form> (없으면 None)
    fn owner_form(&mut self, id: crate::dom::NodeId) -> Option<crate::dom::NodeId> {
        let dom = self.dom_arena().ok()?;
        let mut cur = Some(id);
        while let Some(n) = cur {
            if let crate::dom::NodeType::Element(e) = &dom.get(n).node_type {
                if e.tag_name == "form" {
                    return Some(n);
                }
            }
            cur = dom.get(n).parent;
        }
        None
    }


    // 소켓 이벤트 배달: 스크립트/타이머 드레인 구간에서 호출된다.
    // (연결 직후 등록된 onopen/onmessage 가 실제로 불리도록)
    pub fn pump_websockets(&mut self) {
        for v in std::mem::take(&mut self.pending_ws_open) {
            if let Value::Obj(o) = &v {
                self.ws_fire(o, "open", Vec::new());
            }
        }
        for v in std::mem::take(&mut self.pending_ws_error) {
            if let Value::Obj(o) = &v {
                self.ws_fire(o, "error", Vec::new());
                self.ws_fire(o, "close", Vec::new());
            }
        }
        for i in 0..self.sockets.len() {
            let events = self.sockets[i].0.poll();
            if events.is_empty() {
                continue;
            }
            let obj = match &self.sockets[i].1 {
                Value::Obj(o) => o.clone(),
                _ => continue,
            };
            for ev in events {
                match ev {
                    crate::websocket::Event::Message(s) => {
                        let mut em = ObjMap::new();
                        em.insert("data".to_string(), Value::Str(s));
                        em.insert("type".to_string(), Value::Str("message".to_string()));
                        let evt = Value::Obj(Rc::new(RefCell::new(em)));
                        self.ws_fire(&obj, "message", vec![evt]);
                    }
                    crate::websocket::Event::Binary(b) => {
                        let buf = self.make_array_buffer(&b).unwrap_or(Value::Undefined);
                        let mut em = ObjMap::new();
                        em.insert("data".to_string(), buf);
                        em.insert("type".to_string(), Value::Str("message".to_string()));
                        let evt = Value::Obj(Rc::new(RefCell::new(em)));
                        self.ws_fire(&obj, "message", vec![evt]);
                    }
                    crate::websocket::Event::Close(code, reason) => {
                        obj.borrow_mut().insert("readyState".to_string(), Value::Num(3.0));
                        let mut em = ObjMap::new();
                        em.insert("code".to_string(), Value::Num(code as f64));
                        em.insert("reason".to_string(), Value::Str(reason));
                        em.insert("wasClean".to_string(), Value::Bool(code == 1000));
                        let evt = Value::Obj(Rc::new(RefCell::new(em)));
                        self.ws_fire(&obj, "close", vec![evt]);
                    }
                }
            }
        }
    }



    // new Date(...) / Date(...) 인자 처리
    fn make_date_from_args(&self, args: &[Value]) -> Value {
        let millis = match args.len() {
            0 => now_millis(),
            1 => match &args[0] {
                Value::Num(n) => *n,
                Value::Str(s) => parse_date_string(s).unwrap_or(f64::NAN),
                Value::Obj(m) if is_date_obj(m) => match m.borrow().get("\u{0}time") {
                    Some(Value::Num(n)) => *n,
                    _ => f64::NAN,
                },
                v => to_num(v),
            },
            _ => {
                let g = |i: usize, dflt: f64| args.get(i).map(to_num).unwrap_or(dflt);
                // (year, month[0기준], day, h, m, s, ms)
                date_to_millis(
                    g(0, 1970.0) as i64,
                    g(1, 0.0) as i64 + 1,
                    g(2, 1.0) as i64,
                    g(3, 0.0) as i64,
                    g(4, 0.0) as i64,
                    g(5, 0.0) as i64,
                    g(6, 0.0) as i64,
                )
            }
        };
        make_date(millis)
    }




    // OrdinaryDefineOwnProperty (§10.1.6): 서술자를 검증하고 적용한다.
    // configurable:false 프로퍼티는 재정의를 거부하고(configurable/enumerable 변경 불가,
    // writable:false 로만 강등 가능, 값은 writable 일 때만), 없던 프로퍼티는 새로 만든다.
    // 예전엔 writable/configurable 을 통째로 무시하고 값만 덮었다 — 서술자가 이름만 있고
    // 강제되지 않는 편법이었다.
    pub(super) fn ordinary_define(
        &mut self,
        map: &Rc<RefCell<ObjMap>>,
        key: &str,
        desc: &Value,
    ) -> Result<(), String> {
        // ToPropertyDescriptor (§10.2.4): 서술자는 임의의 객체다(함수/배열/인스턴스
        // 포함). 필드는 HasProperty + Get 으로 읽어 상속·getter 도 반영한다. 예전엔
        // Value::Obj 만 받아 함수 서술자를 거부하고 상속 필드를 무시했다.
        if !is_object(desc) {
            return Err(self.throw_error("TypeError", "Property description must be an object"));
        }
        // 필드별 (존재 여부, 값). 존재는 HasProperty, 값은 member_get(getter 호출).
        let field = |me: &mut Self, k: &str| -> Result<(bool, Value), String> {
            if me.has_property(desc, k) {
                Ok((true, me.member_get(desc, k)?))
            } else {
                Ok((false, Value::Undefined))
            }
        };
        let (has_value, new_value) = field(self, "value")?;
        let (has_get, gv) = field(self, "get")?;
        let get = if has_get { Some(gv) } else { None };
        let (has_set, sv) = field(self, "set")?;
        let set = if has_set { Some(sv) } else { None };
        let (has_w, wv) = field(self, "writable")?;
        let w = to_bool(&wv);
        let (has_e, ev) = field(self, "enumerable")?;
        let e = to_bool(&ev);
        let (has_c, cv) = field(self, "configurable")?;
        let c = to_bool(&cv);
        let is_accessor_desc = has_get || has_set;
        let is_data_desc = has_value || has_w;
        if is_accessor_desc && is_data_desc {
            return Err(self.throw_error(
                "TypeError",
                "Invalid property descriptor. Cannot both specify accessors and a value or writable",
            ));
        }
        if let Some(g) = &get {
            if !matches!(g, Value::Undefined) && !is_callable(g) {
                return Err(self.throw_error("TypeError", "Getter must be a function"));
            }
        }
        if let Some(s) = &set {
            if !matches!(s, Value::Undefined) && !is_callable(s) {
                return Err(self.throw_error("TypeError", "Setter must be a function"));
            }
        }

        let existing = map.borrow().get(key).cloned();
        let cur_attrs = existing.as_ref().map(|_| prop_attrs(&map.borrow(), key));

        if let (Some(cur_val), Some(cur)) = (&existing, cur_attrs) {
            let configurable = cur & ATTR_CONFIGURABLE != 0;
            let cur_is_accessor = matches!(cur_val, Value::Accessor(_));
            if !configurable {
                if has_c && c {
                    return Err(self.redefine_err());
                }
                if has_e && (e != (cur & ATTR_ENUMERABLE != 0)) {
                    return Err(self.redefine_err());
                }
                if is_accessor_desc && !cur_is_accessor {
                    return Err(self.redefine_err());
                }
                if is_data_desc && cur_is_accessor {
                    return Err(self.redefine_err());
                }
                if !cur_is_accessor {
                    let cur_w = cur & ATTR_WRITABLE != 0;
                    if !cur_w && has_w && w {
                        return Err(self.redefine_err());
                    }
                    if !cur_w && has_value && !same_value(cur_val, &new_value) {
                        return Err(self.redefine_err());
                    }
                } else if let Value::Accessor(acc) = cur_val {
                    if has_get
                        && !same_value(&get.clone().unwrap_or(Value::Undefined),
                                       &acc.get.clone().unwrap_or(Value::Undefined))
                    {
                        return Err(self.redefine_err());
                    }
                    if has_set
                        && !same_value(&set.clone().unwrap_or(Value::Undefined),
                                       &acc.set.clone().unwrap_or(Value::Undefined))
                    {
                        return Err(self.redefine_err());
                    }
                }
            }
        }

        let mut attrs = cur_attrs.unwrap_or(0);
        if has_w { if w { attrs |= ATTR_WRITABLE } else { attrs &= !ATTR_WRITABLE } }
        if has_e { if e { attrs |= ATTR_ENUMERABLE } else { attrs &= !ATTR_ENUMERABLE } }
        if has_c { if c { attrs |= ATTR_CONFIGURABLE } else { attrs &= !ATTR_CONFIGURABLE } }

        let final_val = if is_accessor_desc {
            let g = get.filter(is_callable);
            let s = set.filter(is_callable);
            let (og, os) = match &existing {
                Some(Value::Accessor(a)) => (a.get.clone(), a.set.clone()),
                _ => (None, None),
            };
            Value::Accessor(Rc::new(super::AccessorPair {
                get: if has_get { g } else { og },
                set: if has_set { s } else { os },
            }))
        } else if has_value {
            new_value
        } else {
            match &existing {
                Some(v) if !matches!(v, Value::Accessor(_)) => v.clone(),
                _ => Value::Undefined,
            }
        };

        let mut m = map.borrow_mut();
        m.insert(key.to_string(), final_val);
        set_prop_attrs(&mut m, key, attrs);
        Ok(())
    }

    fn redefine_err(&mut self) -> String {
        self.throw_error("TypeError", "Cannot redefine property")
    }

    pub(super) fn call_native(
        &mut self,
        n: Native,
        recv: Option<Value>,
        args: Vec<Value>,
    ) -> Result<Value, String> {
        let r = self.call_native_inner(n, recv, args);
        self.schedule_mutation_delivery();
        r
    }

    fn call_native_inner(
        &mut self,
        n: Native,
        recv: Option<Value>,
        args: Vec<Value>,
    ) -> Result<Value, String> {
        match n {
            // Proxy 는 new 로만 생성 (construct 에서 처리). 함수 호출은 무의미.
            Native::ProxyCtor => Err("Proxy 는 new 로 생성해야 함".to_string()),
            Native::ConsoleLog => {
                let line = args.iter().map(to_display).collect::<Vec<_>>().join(" ");
                self.console.push(line);
                Ok(Value::Undefined)
            }
            // print(x): 셸 print. 인자 하나를 그대로 캡처 버퍼로 (async 하네스 통로).
            Native::Print => {
                let line = args.iter().map(to_display).collect::<Vec<_>>().join(" ");
                self.console.push(line);
                Ok(Value::Undefined)
            }
            Native::ArrayPush => match recv {
                Some(Value::Arr(a)) => {
                    // 얼었거나 봉인/확장금지면 새 항목을 추가하지 않는다(표준).
                    if self.is_nonextensible_val(&Value::Arr(a.clone())) {
                        return Ok(Value::Num(a.borrow().len() as f64));
                    }
                    for v in args {
                        a.borrow_mut().push(v);
                    }
                    Ok(Value::Num(a.borrow().len() as f64))
                }
                // array-like(length 보유 객체)에도 동작 — jQuery 의 push.apply(jqObj, …)
                Some(Value::Obj(o)) if is_array_like(&o) => {
                    let mut items = array_like_to_vec(&o)?;
                    items.extend(args);
                    let n = items.len();
                    write_back_array_like(&o, &items);
                    Ok(Value::Num(n as f64))
                }
                _ => Err("push 는 배열 메서드".to_string()),
            },
            Native::GetElementById => self.dom_get_element_by_id(args),
            Native::AddEventListener => {
                let event = args.first().map(to_display).unwrap_or_default();
                let listener = args.get(1).cloned().unwrap_or(Value::Undefined);
                if !is_callable(&listener) {
                    return Ok(Value::Undefined); // null/undefined 리스너는 무시 (표준)
                }
                // 3번째 인자: true 또는 {capture: true, once: true, passive: …} (표준)
                let (capture, once) = match args.get(2) {
                    Some(Value::Bool(b)) => (*b, false),
                    Some(Value::Obj(o)) => (
                        matches!(o.borrow().get("capture"), Some(v) if to_bool(v)),
                        matches!(o.borrow().get("once"), Some(v) if to_bool(v)),
                    ),
                    _ => (false, false),
                };
                match recv {
                    Some(Value::Dom(id)) => {
                        self.handlers.push((id, event, listener, capture, once));
                        Ok(Value::Undefined)
                    }
                    // EventTarget 은 요소 전용이 아니다. XHR 등 객체 수신자는 리스너를
                    // 객체 안(내부 키)에 보관한다. 예전엔 여기서 던져서 xhr.addEventListener
                    // 한 줄에 스크립트 전체가 죽었다.
                    Some(Value::Obj(o)) => {
                        let key = obj_listener_key(&event);
                        let existing = o.borrow().get(&key).cloned();
                        let list = match existing {
                            Some(Value::Arr(a)) => a,
                            _ => {
                                let a = ArrayObj::new(Vec::new());
                                o.borrow_mut().insert(key, Value::Arr(a.clone()));
                                a
                            }
                        };
                        list.borrow_mut().push(listener);
                        Ok(Value::Undefined)
                    }
                    _ => Ok(Value::Undefined),
                }
            }
            // removeEventListener — 참조 동일한 리스너만 제거 (표준).
            // 예전엔 요소엔 메서드 자체가 없어 TypeError, document/window/XHR 은 무동작
            // 스텁이었다 — "제거했다"고 믿는 코드에서 핸들러가 계속 발화했다.
            Native::RemoveEventListener => {
                let event = args.first().map(to_display).unwrap_or_default();
                let listener = args.get(1).cloned().unwrap_or(Value::Undefined);
                match recv {
                    Some(Value::Dom(id)) => {
                        self.handlers.retain(|(hid, t, f, _, _)| {
                            !(*hid == id && *t == event && same_callable(f, &listener))
                        });
                    }
                    Some(Value::Obj(o)) => {
                        let key = obj_listener_key(&event);
                        let list = match o.borrow().get(&key) {
                            Some(Value::Arr(a)) => Some(a.clone()),
                            _ => None,
                        };
                        if let Some(a) = list {
                            a.borrow_mut().retain(|f| !same_callable(f, &listener));
                        }
                    }
                    _ => {}
                }
                Ok(Value::Undefined)
            }
            // document/window.removeEventListener
            Native::RemoveGlobalListener => {
                let event = args.first().map(to_display).unwrap_or_default();
                let listener = args.get(1).cloned().unwrap_or(Value::Undefined);
                self.global_handlers
                    .retain(|(t, f)| !(*t == event && same_callable(f, &listener)));
                Ok(Value::Undefined)
            }
            // document/window.dispatchEvent — 전역 핸들러를 실제로 부른다.
            // 예전엔 메서드 자체가 없어 커스텀 이벤트를 문서에 쏘면 TypeError 였다.
            Native::DispatchGlobalEvent => {
                let evt = args.first().cloned().unwrap_or(Value::Undefined);
                let ty = match &evt {
                    Value::Obj(o) => o.borrow().get("type").map(to_display).unwrap_or_default(),
                    _ => to_display(&evt),
                };
                let to_run: Vec<Value> = self
                    .global_handlers
                    .iter()
                    .filter(|(t, _)| *t == ty)
                    .map(|(_, f)| f.clone())
                    .collect();
                for f in to_run {
                    if let Err(e) = self.call_value(f, None, vec![evt.clone()]) {
                        println!("[js error] {}", e);
                    }
                }
                let prevented = match &evt {
                    Value::Obj(o) => {
                        matches!(o.borrow().get("defaultPrevented"), Some(Value::Bool(true)))
                    }
                    _ => false,
                };
                Ok(Value::Bool(!prevented))
            }
            // document/window.addEventListener — 전역 핸들러로 등록 (recv 무시)
            Native::AddGlobalListener => {
                let event = args.first().map(to_display).unwrap_or_default();
                if let Some(f) = args.get(1) {
                    if is_callable(f) {
                        self.global_handlers.push((event, f.clone()));
                    }
                }
                Ok(Value::Undefined)
            }
            // fn.call(thisArg, a, b, ...) — recv 가 대상 함수
            Native::FnCall => {
                let target = recv.ok_or("call 대상 함수 없음")?;
                let mut it = args.into_iter();
                let this_arg = it.next().unwrap_or(Value::Undefined);
                self.call_value(target, Some(this_arg), it.collect())
            }
            // fn.apply(thisArg, [args]) — 두 번째 인자는 배열 또는 유사배열(arguments)
            Native::FnApply => {
                let target = recv.ok_or("apply 대상 함수 없음")?;
                let mut it = args.into_iter();
                let this_arg = it.next().unwrap_or(Value::Undefined);
                let call_args = match it.next() {
                    Some(Value::Arr(a)) => a.borrow().clone(),
                    _ => Vec::new(),
                };
                self.call_value(target, Some(this_arg), call_args)
            }
            // fn.bind(thisArg, ...partial) → 바운드 함수
            Native::FnBind => {
                let target = recv.ok_or("bind 대상 함수 없음")?;
                let mut it = args.into_iter();
                let this_arg = it.next().unwrap_or(Value::Undefined);
                let partial: Vec<Value> = it.collect();
                Ok(Value::Bound(Rc::new((target, this_arg, partial))))
            }
            Native::FunctionCtor => self.make_function(args),
            // 간접 eval: 전역 스코프에서 평가 (§19.2.1)
            Native::Eval => {
                let a = args.into_iter().next().unwrap_or(Value::Undefined);
                let g = self.global.clone();
                self.do_eval(a, &g, &g)
            }
            // Object.getOwnPropertyDescriptor(o, k) — 접근자면 get/set, 값이면 value.
            // 예전엔 프렐류드 폴리필이 {value: o[k], enumerable: true} 를 만들었다:
            //   1) 게터 프로퍼티의 디스크립터에 get 이 없다 (게터를 **실행해** 값만 준다).
            //   2) enumerable 이 항상 true (비열거를 구분 못 한다).
            //   3) 배열 length / 함수 prototype 이 undefined.
            // 라이브러리가 d.get / d.enumerable 을 보고 분기하므로 조용히 틀린 길로 간다.
            Native::ObjectGetOwnPropertyDescriptor => {
                let target = args.first().cloned().unwrap_or(Value::Undefined);
                let key = match args.get(1).cloned() {
                    Some(k) => self.to_property_key(k)?,
                    None => String::new(),
                };
                let mut d = ObjMap::new();
                let found = match &target {
                    Value::Obj(m) => {
                        // 실제 저장된 속성 비트를 읽어 정확히 보고한다 (§10.1.5.1).
                        // 예전엔 writable 을 항상 true 로, configurable 도 무조건 true 로
                        // 거짓말했다.
                        let b = m.borrow();
                        match b.get(&key) {
                            Some(_) if is_internal_key(&key) => false,
                            Some(Value::Accessor(acc)) => {
                                let attrs = prop_attrs(&b, &key);
                                d.insert("get".to_string(), acc.get.clone().unwrap_or(Value::Undefined));
                                d.insert("set".to_string(), acc.set.clone().unwrap_or(Value::Undefined));
                                d.insert("enumerable".to_string(), Value::Bool(attrs & ATTR_ENUMERABLE != 0));
                                d.insert("configurable".to_string(), Value::Bool(attrs & ATTR_CONFIGURABLE != 0));
                                return Ok(Value::Obj(Rc::new(RefCell::new(d))));
                            }
                            Some(v) => {
                                let attrs = prop_attrs(&b, &key);
                                d.insert("value".to_string(), v.clone());
                                d.insert("writable".to_string(), Value::Bool(attrs & ATTR_WRITABLE != 0));
                                d.insert("enumerable".to_string(), Value::Bool(attrs & ATTR_ENUMERABLE != 0));
                                d.insert("configurable".to_string(), Value::Bool(attrs & ATTR_CONFIGURABLE != 0));
                                return Ok(Value::Obj(Rc::new(RefCell::new(d))));
                            }
                            None => false,
                        }
                    }
                    Value::Arr(a) => {
                        if key == "length" {
                            d.insert("value".to_string(), Value::Num(a.borrow().len() as f64));
                            d.insert("writable".to_string(), Value::Bool(true));
                            // length 는 비열거다 (표준)
                            d.insert("enumerable".to_string(), Value::Bool(false));
                            d.insert("configurable".to_string(), Value::Bool(false));
                            return Ok(Value::Obj(Rc::new(RefCell::new(d))));
                        }
                        match key.parse::<usize>().ok().and_then(|i| a.borrow().get(i).cloned()) {
                            Some(v) => {
                                d.insert("value".to_string(), v);
                                d.insert("writable".to_string(), Value::Bool(true));
                                true
                            }
                            None => match a.get_prop(&key) {
                                Some(v) => {
                                    d.insert("value".to_string(), v);
                                    d.insert("writable".to_string(), Value::Bool(true));
                                    true
                                }
                                None => false,
                            },
                        }
                    }
                    Value::Fn(f) => {
                        // 함수의 name/length/prototype 은 own 프로퍼티다 (§10.2.4~10.2.9)
                        let v = match key.as_str() {
                            "prototype" => Some(self.member_get(&target, "prototype")?),
                            "name" => Some(Value::Str(f.name.borrow().clone())),
                            "length" => Some(Value::Num(f.params.len() as f64)),
                            _ => f.props.borrow().get(&key).cloned(),
                        };
                        // name/length 는 읽기 전용이다 (§10.2.4)
                        if matches!(key.as_str(), "name" | "length") {
                            if let Some(val) = v {
                                d.insert("value".to_string(), val);
                                d.insert("writable".to_string(), Value::Bool(false));
                                d.insert("enumerable".to_string(), Value::Bool(false));
                                d.insert("configurable".to_string(), Value::Bool(true));
                                return Ok(Value::Obj(Rc::new(RefCell::new(d))));
                            }
                        }
                        match v {
                            Some(Value::Accessor(acc)) => {
                                d.insert(
                                    "get".to_string(),
                                    acc.get.clone().unwrap_or(Value::Undefined),
                                );
                                d.insert(
                                    "set".to_string(),
                                    acc.set.clone().unwrap_or(Value::Undefined),
                                );
                                true
                            }
                            Some(v) => {
                                d.insert("value".to_string(), v);
                                d.insert("writable".to_string(), Value::Bool(true));
                                true
                            }
                            None => false,
                        }
                    }
                    Value::Instance(inst) => {
                        // private 이름은 프로퍼티가 아니다 — 서술자도 없다
                        let fk = field_key(&key, self.priv_id);
                        match inst.fields.borrow().get(&fk) {
                            Some(v) if !is_private_name(&key) => {
                                d.insert("value".to_string(), v.clone());
                                d.insert("writable".to_string(), Value::Bool(true));
                                true
                            }
                            _ => false,
                        }
                    }
                    // 클래스의 static 멤버는 클래스 객체의 own 프로퍼티다 (§15.7.14).
                    // 예전엔 여기 자체가 없어서 hasOwnProperty(C, 'm') 이 false 였고,
                    // getOwnPropertyDescriptor(C, 'm') 이 undefined 였다.
                    Value::Class(c) => {
                        if let Some(g) = c.static_getters.get(&key) {
                            d.insert("get".to_string(), Value::Fn(g.clone()));
                            d.insert(
                                "set".to_string(),
                                c.static_setters
                                    .get(&key)
                                    .map(|s| Value::Fn(s.clone()))
                                    .unwrap_or(Value::Undefined),
                            );
                            true
                        } else if let Some(st) = c.static_setters.get(&key) {
                            d.insert("get".to_string(), Value::Undefined);
                            d.insert("set".to_string(), Value::Fn(st.clone()));
                            true
                        } else if key == "prototype" {
                            d.insert("value".to_string(), self.member_get(&target, "prototype")?);
                            d.insert("writable".to_string(), Value::Bool(false));
                            true
                        } else if key == "name" {
                            d.insert("value".to_string(), Value::Str(c.name.borrow().clone()));
                            d.insert("writable".to_string(), Value::Bool(false));
                            true
                        } else {
                            match c.statics.borrow().get(&key) {
                                Some(v) if !is_private_name(&key) => {
                                    d.insert("value".to_string(), v.clone());
                                    d.insert("writable".to_string(), Value::Bool(true));
                                    true
                                }
                                _ => false,
                            }
                        }
                    }
                    // 내장/바운드 함수의 name/length 는 own 프로퍼티다 (§17):
                    // { writable:false, enumerable:false, configurable:true }.
                    // delete 로 지워졌으면 서술자 없음(undefined).
                    Value::Native(_) | Value::Bound(_)
                        if matches!(key.as_str(), "name" | "length")
                            && !self.native_prop_deleted(&target, &key) =>
                    {
                        let val = if key == "name" {
                            Value::Str(self.native_fn_name(&target))
                        } else {
                            Value::Num(self.native_fn_length(&target))
                        };
                        d.insert("value".to_string(), val);
                        d.insert("writable".to_string(), Value::Bool(false));
                        d.insert("enumerable".to_string(), Value::Bool(false));
                        d.insert("configurable".to_string(), Value::Bool(true));
                        return Ok(Value::Obj(Rc::new(RefCell::new(d))));
                    }
                    // 내장 생성자의 정적 메서드/상수/prototype 도 own 프로퍼티 (§17/§20~22).
                    Value::Native(_) => match self.native_own_descriptor(&target, &key)? {
                        Some(desc) => return Ok(desc),
                        None => false,
                    }
                    _ => false,
                };
                if !found {
                    return Ok(Value::Undefined);
                }
                // 열거 가능 여부 (표준):
                //  - 일반 객체: 비열거 표식으로 판정
                //  - 클래스의 static 멤버, 함수의 name/length/prototype: 전부 비열거
                let enumerable = match &target {
                    Value::Obj(m) => !m.borrow().contains_key(&nonenum_marker(&key)),
                    // static 메서드/접근자/prototype/name 은 비열거, static 필드는 열거 가능
                    Value::Class(c) => {
                        !matches!(key.as_str(), "prototype" | "name")
                            && !c.static_getters.contains_key(&key)
                            && !c.static_setters.contains_key(&key)
                            && !c.statics.borrow().contains_key(&nonenum_marker(&key))
                    }
                    Value::Fn(_) => !matches!(key.as_str(), "prototype" | "name" | "length"),
                    _ => true,
                };
                // 함수의 prototype 은 재설정 불가다 (§10.2.4). name/length 는 재설정 가능.
                let configurable = !(matches!(&target, Value::Fn(_) | Value::Class(_))
                    && key == "prototype");
                d.insert("enumerable".to_string(), Value::Bool(enumerable));
                d.insert("configurable".to_string(), Value::Bool(configurable));
                Ok(Value::Obj(Rc::new(RefCell::new(d))))
            }
            // Object.defineProperty(target, key, {get|value}) — 접근자/값 정의
            Native::ObjectDefineProperty => {
                let target = args.first().cloned().unwrap_or(Value::Undefined);
                // §20.1.2.4: 대상이 객체가 아니면 TypeError. 예전엔 조용히 무시했다.
                if !is_object(&target) {
                    return Err(self.throw_error(
                        "TypeError",
                        "Object.defineProperty called on non-object",
                    ));
                }
                let key = match args.get(1).cloned() {
                    Some(k) => self.to_property_key(k)?,
                    None => String::new(),
                };
                let Some(desc) = args.get(2).cloned() else {
                    return Err(self.throw_error("TypeError", "Property description must be an object"));
                };
                // Value::Obj 는 표준 OrdinaryDefineOwnProperty (§10.1.6) 로 처리한다.
                // 그 외 대상(Fn/Instance/Arr/Class)은 속성 강제 없이 값만 넣는 근사 유지.
                if let Value::Obj(map) = &target {
                    self.ordinary_define(map, &key, &desc)?;
                    return Ok(target);
                }
                // ── 근사 경로 (표준 강제 없음) ──
                let d = if let Value::Obj(d) = &desc { d.borrow() } else {
                    return Ok(target);
                };
                let g = d.get("get").cloned().filter(is_callable);
                let st = d.get("set").cloned().filter(is_callable);
                let entry = if g.is_some() || st.is_some() {
                    Some(Value::Accessor(Rc::new(super::AccessorPair { get: g, set: st })))
                } else {
                    d.get("value").cloned()
                };
                let enumerable = matches!(d.get("enumerable"), Some(v) if to_bool(v));
                drop(d);
                if let Some(val) = entry {
                    match &target {
                        Value::Fn(func) => {
                            func.props.borrow_mut().insert(key, val);
                        }
                        Value::Instance(inst) => {
                            let marker = nonenum_marker(&key);
                            inst.fields.borrow_mut().insert(key, val);
                            if enumerable {
                                inst.fields.borrow_mut().remove(&marker);
                            } else {
                                inst.fields.borrow_mut().insert(marker, Value::Bool(true));
                            }
                        }
                        Value::Arr(a) => {
                            if let Ok(i) = key.parse::<usize>() {
                                let mut items = a.borrow_mut();
                                while items.len() <= i {
                                    items.push(Value::Undefined);
                                }
                                items[i] = val;
                            } else {
                                a.set_prop(key, val);
                            }
                        }
                        Value::Class(c) => {
                            c.statics.borrow_mut().insert(key, val);
                        }
                        _ => {}
                    }
                }
                Ok(target)
            }
            // Object.create(proto) — proto 의 얕은 복사 기반 새 객체 (관용)
            Native::ObjectCreate => {
                // §20.1.2.2: proto 는 Object 또는 null 이어야 한다, 아니면 TypeError.
                let proto = args.first().cloned().unwrap_or(Value::Undefined);
                if !is_object(&proto) && !matches!(proto, Value::Null) {
                    return Err(self.throw_error(
                        "TypeError",
                        "Object prototype may only be an Object or null",
                    ));
                }
                // proto 를 __proto__ 로 링크(스냅샷 복사 아님). Object.create(null) 은 링크 없음.
                let mut map = ObjMap::new();
                if let Value::Obj(_) = &proto {
                    map.insert("__proto__".to_string(), proto.clone());
                }
                let obj = Value::Obj(Rc::new(RefCell::new(map)));
                // 2번째 인자(프로퍼티 서술자): defineProperties 에 위임 → get/set/속성 전부 반영.
                if let Some(props) = args.get(1) {
                    if !matches!(props, Value::Undefined) {
                        self.call_native(
                            Native::ObjectDefineProperties,
                            None,
                            vec![obj.clone(), props.clone()],
                        )?;
                    }
                }
                Ok(obj)
            }
            // freeze 는 그대로 반환(불변성 미구현)
            // Object.setPrototypeOf(o, proto) — 예전엔 no-op 로 객체만 돌려줬다(거짓말).
            Native::ObjectSetPrototypeOf => {
                let target = args.first().cloned().unwrap_or(Value::Undefined);
                let proto = args.get(1).cloned().unwrap_or(Value::Null);
                if let Value::Obj(m) = &target {
                    match proto {
                        Value::Obj(_) => {
                            m.borrow_mut().insert("__proto__".to_string(), proto);
                        }
                        // null → 프로토타입 없음
                        _ => {
                            m.borrow_mut().remove("__proto__");
                        }
                    }
                }
                Ok(target)
            }
            // Object.defineProperties(o, {k: desc, …}) — 예전엔 defineProperty 별칭이라
            // 시그니처가 달라 아무것도 정의되지 않았다.
            Native::ObjectDefineProperties => {
                let target = args.first().cloned().unwrap_or(Value::Undefined);
                // §20.1.2.5: 대상이 객체가 아니면 TypeError.
                if !is_object(&target) {
                    return Err(self.throw_error(
                        "TypeError",
                        "Object.defineProperties called on non-object",
                    ));
                }
                let props = args.get(1).cloned().unwrap_or(Value::Undefined);
                // §20.1.2.3.1: Properties 의 열거 가능한 own 키를 돌며, 각 서술자는
                // Get(Properties, key)(getter 호출)으로 읽는다. 예전엔 own 값을 raw 로
                // 순회해 getter 서술자를 Accessor 그대로 넘겨 거부됐다.
                let keys: Vec<String> = match &props {
                    Value::Obj(d) => enumerable_keys(d),
                    _ => Vec::new(),
                };
                for k in keys {
                    let desc = self.member_get(&props, &k)?;
                    self.call_native(
                        Native::ObjectDefineProperty,
                        None,
                        vec![target.clone(), Value::Str(k), desc],
                    )?;
                }
                Ok(target)
            }
            // Object.getOwnPropertySymbols(o) — 예전엔 항상 [] 였다(거짓말).
            // 심볼 키는 "\0@@…" 로 저장되므로 그 키들에서 심볼을 복원한다.
            Native::ObjectGetOwnPropertySymbols => {
                let keys: Vec<String> = match args.first() {
                    Some(Value::Obj(m)) => m
                        .borrow()
                        .keys()
                        .filter(|k| k.starts_with("\u{0}@@"))
                        .cloned()
                        .collect(),
                    _ => Vec::new(),
                };
                let syms: Vec<Value> = keys.into_iter().map(|k| symbol_from_key(&k)).collect();
                Ok(Value::Arr(ArrayObj::new(syms)))
            }
            // proto.isPrototypeOf(obj) — 예전엔 항상 false 였다(거짓말).
            Native::ObjectIsPrototypeOf => {
                let (Some(proto), Some(Value::Obj(target))) = (&recv, args.first()) else {
                    return Ok(Value::Bool(false));
                };
                let mut cur = target.borrow().get("__proto__").cloned();
                for _ in 0..100 {
                    match cur {
                        Some(p) => {
                            if strict_eq(&p, proto) {
                                return Ok(Value::Bool(true));
                            }
                            cur = match &p {
                                Value::Obj(pm) => pm.borrow().get("__proto__").cloned(),
                                _ => None,
                            };
                        }
                        None => break,
                    }
                }
                Ok(Value::Bool(false))
            }
            // Object(x) / Array(a,b,…) — 전역이 Native 생성자라 호출이 여기로 온다.
            Native::ObjectCtor | Native::ArrayCtor => {
                Ok(self.coerce_object_call(&Value::Native(n), &args).unwrap_or(Value::Undefined))
            }
            // freeze/seal/preventExtensions — 모든 객체 종류(Obj/Arr/Fn/Instance/Class/Map/Set)에
            // 통일된 무결성 테이블로 상태를 남긴다. 대입 경로가 이 상태를 보고 변경을 막는다.
            Native::ObjectFreeze | Native::ObjectSeal | Native::ObjectPreventExt => {
                let arg = args.into_iter().next().unwrap_or(Value::Undefined);
                let bit = match n {
                    Native::ObjectFreeze => super::INTEG_FROZEN,
                    Native::ObjectSeal => super::INTEG_SEALED,
                    _ => super::INTEG_NONEXT,
                };
                self.set_integrity(&arg, bit);
                Ok(arg)
            }
            // isFrozen/isSealed/isExtensible.
            // 원시값(비객체)은 frozen·sealed=true, extensible=false (표준).
            // 예전엔 인스턴스/함수/Map 도 "비객체" 취급해 안 얼렸는데 true 를 반환했다(거짓말).
            Native::ObjectIsFrozen | Native::ObjectIsSealed | Native::ObjectIsExtensible => {
                let arg = args.first().cloned().unwrap_or(Value::Undefined);
                let r = if super::integrity_ptr(&arg).is_none() {
                    !matches!(n, Native::ObjectIsExtensible) // 원시값
                } else {
                    let b = self.integrity_bits(&arg);
                    let frozen = b & super::INTEG_FROZEN != 0;
                    let sealed = frozen || b & super::INTEG_SEALED != 0;
                    let nonext = sealed || b & super::INTEG_NONEXT != 0;
                    match n {
                        Native::ObjectIsFrozen => frozen,
                        Native::ObjectIsSealed => sealed,
                        _ => !nonext,
                    }
                };
                Ok(Value::Bool(r))
            }
            // Object.getPrototypeOf (표준 §20.1.2.12). 예전엔 __proto__ 링크가 없으면
            // 무조건 null 을 돌려줬다 — 평범한 객체·배열·인스턴스가 전부 null 이었다.
            // regenerator/babel 런타임이 getProto(getProto(values([]))) 로 내장 프로토타입을
            // 캐내는데, null 이 나오면 그 위에 세우는 이터레이터 체인이 통째로 무너진다
            // (naver 가 여기서 죽고 메모리까지 터졌다).
            Native::ObjectGetPrototypeOf => {
                let obj_proto = self.member_get(&self.object_ns.clone(), "prototype")?;
                Ok(match args.first() {
                    Some(Value::Obj(m)) => {
                        // Object.prototype 자신의 프로토타입은 **null** 이다 (체인의 끝).
                        // 자기 자신을 돌려주면 체인을 걷는 코드가 무한 루프에 빠진다.
                        if let Value::Obj(op) = &obj_proto {
                            if Rc::ptr_eq(m, op) {
                                return Ok(Value::Null);
                            }
                        }
                        match m.borrow().get("__proto__") {
                            Some(p) => p.clone(),
                            // 링크가 없으면 Object.prototype (표준)
                            None => obj_proto,
                        }
                    }
                    Some(Value::Arr(_)) => {
                        self.member_get(&self.array_ns.clone(), "prototype")?
                    }
                    // 인스턴스의 프로토타입은 그 클래스의 prototype 객체
                    Some(Value::Instance(inst)) => {
                        self.member_get(&Value::Class(inst.class.clone()), "prototype")?
                    }
                    // NativeError 생성자의 [[Prototype]] 은 **Error 생성자**다 (§20.5.6.2).
                    // 없으면 TypeError 가 Error 의 서브타입임을 확인하는 코드
                    // (testharness 의 assert_throws_js 가 정확히 이 체인을 걷는다)가
                    // "Error 의 서브타입이 아니다" 라고 판정한다.
                    Some(Value::Native(Native::ErrorCtor(n))) if *n != "Error" => {
                        env_get(&self.global, "Error").unwrap_or_else(|| self.fn_proto.clone())
                    }
                    Some(Value::Fn(_)) | Some(Value::Native(_)) | Some(Value::Bound(_)) => {
                        self.fn_proto.clone()
                    }
                    Some(Value::Str(_)) => self.string_proto.clone(),
                    Some(Value::Num(_)) => self.number_proto.clone(),
                    Some(Value::Bool(_)) => self.boolean_proto.clone(),
                    Some(Value::MapVal(_)) => self.map_proto.clone(),
                    Some(Value::SetVal(_)) => self.set_proto.clone(),
                    // 제너레이터/그 밖의 객체형은 Object.prototype 으로 (null 보다 정확하다)
                    Some(Value::Gen(_)) | Some(Value::Class(_)) | Some(Value::Proxy(_)) => {
                        obj_proto
                    }
                    // null/undefined 는 TypeError 지만 관대하게 null
                    _ => Value::Null,
                })
            }
            // Object.prototype.hasOwnProperty.call(obj, key) / obj.hasOwnProperty(key)
            Native::HasOwnProperty => {
                let key = match args.first().cloned() {
                    Some(k) => self.to_property_key(k)?,
                    None => String::new(),
                };
                let has = match &recv {
                    // __proto__ 는 own 프로퍼티 아님(상속 accessor)
                    Some(Value::Obj(m)) => {
                        (!is_internal_key(&key) && m.borrow().contains_key(&key))
                            || self.global_has(m, &key)
                    }
                    // 인스턴스는 own 필드만 own 프로퍼티(메서드는 프로토타입 격)
                    Some(Value::Instance(i)) => i.fields.borrow().contains_key(&key),
                    Some(Value::Arr(a)) => {
                        key.parse::<usize>().map(|i| i < a.borrow().len()).unwrap_or(false)
                            || a.get_prop(&key).is_some()
                            || key == "length"
                    }
                    // 클래스의 static 멤버는 클래스 객체의 own 프로퍼티다
                    Some(Value::Class(c)) => {
                        !is_private_name(&key)
                            && (c.statics.borrow().contains_key(&key)
                                || c.static_getters.contains_key(&key)
                                || c.static_setters.contains_key(&key)
                                || key == "prototype"
                                || key == "name")
                    }
                    Some(Value::Fn(f)) => {
                        f.props.borrow().contains_key(&key)
                            || matches!(key.as_str(), "prototype" | "name" | "length")
                    }
                    // 내장/바운드 함수도 name/length 를 own 프로퍼티로 가진다 (§17).
                    // 내장 생성자는 정적 메서드/상수/prototype 도 own. delete 된 name/length 는 제외.
                    Some(v @ (Value::Native(_) | Value::Bound(_))) => {
                        if matches!(key.as_str(), "name" | "length") {
                            !self.native_prop_deleted(v, &key)
                        } else if let Value::Native(n) = v {
                            self.native_ctor_own_keys(n)
                                .map(|ks| ks.iter().any(|k| *k == key))
                                .unwrap_or(false)
                        } else {
                            false
                        }
                    }
                    _ => false,
                };
                Ok(Value::Bool(has))
            }
            // Object.prototype.toString.call(x) → "[object Array]" 등 (타입 판별 관용)
            // Error.prototype.toString (§20.5.3.4): name 과 message 를 ": " 로 잇되,
            // 한쪽이 비면 다른 쪽만. 둘 다 프로토타입 체인에서 읽는다.
            Native::ErrorToString => {
                let this = recv.clone().unwrap_or(Value::Undefined);
                let get = |me: &mut Self, k: &str| -> String {
                    match me.member_get(&this, k) {
                        Ok(Value::Undefined) | Err(_) => String::new(),
                        Ok(v) => to_display(&v),
                    }
                };
                let name = {
                    let n = get(self, "name");
                    if n.is_empty() { "Error".to_string() } else { n }
                };
                let msg = get(self, "message");
                Ok(Value::Str(if msg.is_empty() {
                    name
                } else if name.is_empty() {
                    msg
                } else {
                    format!("{}: {}", name, msg)
                }))
            }
            Native::ObjToString => {
                // §20.1.3.6: 빌트인 태그를 정한 뒤 Symbol.toStringTag(문자열)로 덮어쓴다.
                let tag: String = match &recv {
                    Some(Value::Arr(_)) => "Array".into(),
                    None | Some(Value::Undefined) => "Undefined".into(),
                    Some(Value::Null) => "Null".into(),
                    Some(Value::Str(_)) => "String".into(),
                    Some(Value::Num(_)) => "Number".into(),
                    Some(Value::Bool(_)) => "Boolean".into(),
                    Some(Value::Fn(_))
                    | Some(Value::Native(_))
                    | Some(Value::Bound(_))
                    | Some(Value::Class(_)) => "Function".into(),
                    Some(Value::MapVal(_)) => "Map".into(),
                    Some(Value::SetVal(_)) => "Set".into(),
                    Some(Value::Obj(o)) => {
                        let b = o.borrow();
                        // Symbol.toStringTag 우선 (문자열이면)
                        if let Some(Value::Str(t)) = b.get("\u{0}@@toStringTag") {
                            t.clone()
                        } else if let Some(Value::Str(t)) = b.get("\u{0}class") {
                            // 원시 래퍼(new String/Number/Boolean)
                            t.clone()
                        } else if is_regex_obj(o) {
                            "RegExp".into()
                        } else if b.contains_key("\u{0}isDate") {
                            "Date".into()
                        } else if b.contains_key("\u{0}items") || b.contains_key("next") {
                            // 반복자류는 그대로 Object (Arguments 등은 미구분)
                            "Object".into()
                        } else {
                            "Object".into()
                        }
                    }
                    _ => "Object".into(),
                };
                Ok(Value::Str(format!("[object {}]", tag)))
            }
            Native::ReturnTrue => Ok(Value::Bool(true)),
            Native::ReturnThis => Ok(recv.unwrap_or(Value::Undefined)),
            Native::FnToString => Ok(Value::Str("function () { [native code] }".to_string())),
            // obj[Symbol.iterator]() → 반복자 객체 { next(), value/done }
            Native::MakeIter => {
                let items: Vec<Value> = match &recv {
                    Some(Value::Arr(a)) => a.borrow().clone(),
                    Some(Value::Str(s)) => s.chars().map(|c| Value::Str(c.to_string())).collect(),
                    Some(Value::SetVal(s)) => s.borrow().clone(),
                    Some(Value::MapVal(m)) => m
                        .borrow()
                        .iter()
                        .map(|(k, v)| Value::Arr(ArrayObj::new(vec![k.clone(), v.clone()])))
                        .collect(),
                    _ => Vec::new(),
                };
                let mut it = ObjMap::new();
                it.insert("\u{0}items".to_string(), Value::Arr(ArrayObj::new(items)));
                it.insert("\u{0}i".to_string(), Value::Num(0.0));
                it.insert("next".to_string(), Value::Native(Native::IterNext));
                Ok(Value::Obj(Rc::new(RefCell::new(it))))
            }
            // 반복자.next() → { value, done }
            Native::IterNext => {
                let mut res = ObjMap::new();
                if let Some(Value::Obj(o)) = &recv {
                    let (items, i) = {
                        let b = o.borrow();
                        (b.get("\u{0}items").cloned(), b.get("\u{0}i").cloned())
                    };
                    if let (Some(Value::Arr(items)), Some(Value::Num(i))) = (items, i) {
                        let idx = i as usize;
                        let len = items.borrow().len();
                        if idx < len {
                            res.insert("value".to_string(), items.borrow()[idx].clone());
                            res.insert("done".to_string(), Value::Bool(false));
                            o.borrow_mut().insert("\u{0}i".to_string(), Value::Num((idx + 1) as f64));
                        } else {
                            res.insert("value".to_string(), Value::Undefined);
                            res.insert("done".to_string(), Value::Bool(true));
                        }
                    }
                }
                Ok(Value::Obj(Rc::new(RefCell::new(res))))
            }
            // 지연 제너레이터: next(v)/return(v)/throw(e) — 다음 yield 까지 재개 실행.
            Native::GenNext | Native::GenReturn | Native::GenThrow => {
                if let Some(Value::Gen(gs)) = &recv {
                    let arg = args.into_iter().next().unwrap_or(Value::Undefined);
                    let mode = match n {
                        Native::GenReturn => super::generator::ResumeMode::Return(arg.clone()),
                        Native::GenThrow => super::generator::ResumeMode::Throw(arg.clone()),
                        _ => super::generator::ResumeMode::Next,
                    };
                    let gs = gs.clone();
                    self.gen_resume(&gs, arg, mode)
                } else {
                    Ok(Value::Undefined)
                }
            }
            // Symbol(desc) — 고유 심볼 원시값 생성.
            Native::SymbolCtor => {
                self.sym_counter += 1;
                let desc = match args.first() {
                    Some(Value::Undefined) | None => None,
                    Some(v) => Some(to_display(v)),
                };
                Ok(Value::Symbol(Rc::new(super::SymbolData {
                    // desc 를 키에 담아 둔다 — getOwnPropertySymbols 가 심볼을 복원할 때
                    // 설명까지 되살릴 수 있어야 한다. 고유성은 카운터가 보장.
                    key: format!("\u{0}@@sym:{}:{}", self.sym_counter, desc.clone().unwrap_or_default()),
                    desc,
                })))
            }
            // Symbol.for(k) — 전역 레지스트리에서 공유 심볼.
            Native::SymbolFor => {
                let k = args.first().map(to_display).unwrap_or_else(|| "undefined".to_string());
                if let Some(sym) = self.sym_registry.get(&k) {
                    return Ok(sym.clone());
                }
                let sym = Value::Symbol(Rc::new(super::SymbolData {
                    key: format!("\u{0}@@for:{}", k),
                    desc: Some(k.clone()),
                }));
                self.sym_registry.insert(k, sym.clone());
                Ok(sym)
            }
            // Symbol.keyFor(sym) — 레지스트리 심볼이면 키, 아니면 undefined.
            Native::SymbolKeyFor => Ok(match args.first() {
                Some(Value::Symbol(s)) => s
                    .key
                    .strip_prefix("\u{0}@@for:")
                    .map(|k| Value::Str(k.to_string()))
                    .unwrap_or(Value::Undefined),
                _ => Value::Undefined,
            }),
            // a.compareDocumentPosition(b) — 문서(preorder) 순서 비트마스크.
            // 4 = b 가 a 뒤(FOLLOWING), 2 = b 가 a 앞(PRECEDING), 0 = 동일.
            // jQuery 의 sortOrder 가 결과 집합 정렬에 쓴다.
            Native::CompareDocPosition => {
                let (Some(Value::Dom(a)), Some(Value::Dom(b))) = (&recv, args.first()) else {
                    return Ok(Value::Num(0.0));
                };
                let (a, b) = (*a, *b);
                if a == b {
                    return Ok(Value::Num(0.0));
                }
                let dom = self.dom_arena()?;
                let root = dom.root;
                let ia = preorder_index(dom, root, a, &mut 0);
                let ib = preorder_index(dom, root, b, &mut 0);
                Ok(Value::Num(match (ia, ib) {
                    (Some(x), Some(y)) if y > x => 4.0,
                    (Some(x), Some(y)) if y < x => 2.0,
                    _ => 0.0,
                }))
            }
            // document.implementation.createHTMLDocument(title) — 분리된 문서.
            // html>head+body 를 아레나에 만들어(문서 트리엔 안 붙임) 문서형 객체로 돌려준다.
            // jQuery 가 support.createHTMLDocument 판정과 parseHTML 컨텍스트에 쓴다.
            Native::CreateHTMLDocument => {
                let dom = self.dom_arena()?;
                let html = dom.create_element("html");
                let head = dom.create_element("head");
                let body = dom.create_element("body");
                dom.append_child(html, head);
                dom.append_child(html, body);
                let mut d = ObjMap::new();
                d.insert("nodeType".to_string(), Value::Num(9.0));
                d.insert("documentElement".to_string(), Value::Dom(html));
                d.insert("head".to_string(), Value::Dom(head));
                d.insert("body".to_string(), Value::Dom(body));
                d.insert("createElement".to_string(), Value::Native(Native::CreateElement));
                d.insert("createTextNode".to_string(), Value::Native(Native::CreateTextNode));
                d.insert(
                    "createDocumentFragment".to_string(),
                    Value::Native(Native::CreateDocumentFragment),
                );
                Ok(Value::Obj(Rc::new(RefCell::new(d))))
            }
            // getComputedStyle(el) → 계산 스타일 뷰.
            // import('./m.js') → 모듈 네임스페이스로 이행되는 Promise
            Native::DynamicImport => {
                let spec = args.first().map(to_display).unwrap_or_default();
                let url = self.absolute_url(&spec);
                let p = self.new_promise();
                match self.run_module(&url) {
                    Ok(ns) => self.resolve_promise(&p, ns),
                    Err(e) => {
                        let err = Value::Str(format!("동적 import 실패: {}", e));
                        self.reject_promise(&p, err);
                    }
                }
                Ok(p)
            }
            Native::GetComputedStyle => Ok(self.get_computed_style(args.first())),
            // queueMicrotask(fn) — 마이크로태스크 큐에 직접 넣는다.
            Native::QueueMicrotask => {
                let f = args.into_iter().next().unwrap_or(Value::Undefined);
                if !is_callable(&f) {
                    return Err(self.throw_error(
                        "TypeError",
                        "queueMicrotask requires a callable argument",
                    ));
                }
                self.microtasks.push_back((f, Value::Undefined, Value::Undefined, false));
                Ok(Value::Undefined)
            }
            // el.animate(keyframes, opts) — Web Animations.
            // 정적 렌더에는 시간축이 없다. fill 이 forwards/both 면 마지막 키프레임을
            // 실제로 적용하고(끝 상태 = 우리가 그리는 프레임), finished 는 즉시 이행한다.
            // 아무것도 안 하고 finished 를 영영 안 주면, 이걸로 콘텐츠를 드러내는
            // 코드에서 그 콘텐츠가 영영 안 나온다.
            Native::ElementAnimate => {
                let Some(Value::Dom(id)) = recv else { return Ok(Value::Undefined) };
                let fill = match args.get(1) {
                    Some(Value::Obj(o)) => {
                        o.borrow().get("fill").map(to_display).unwrap_or_default()
                    }
                    _ => String::new(),
                };
                if fill == "forwards" || fill == "both" {
                    // 마지막 키프레임의 프로퍼티를 인라인 스타일로
                    let last = match args.first() {
                        Some(Value::Arr(a)) => a.borrow().last().cloned(),
                        Some(v @ Value::Obj(_)) => Some(v.clone()),
                        _ => None,
                    };
                    if let Some(Value::Obj(kf)) = last {
                        let props: Vec<(String, String)> = kf
                            .borrow()
                            .iter()
                            .filter(|(k, _)| {
                                !matches!(k.as_str(), "offset" | "easing" | "composite")
                                    && !is_internal_key(k)
                            })
                            .map(|(k, v)| (camel_to_dashed(k), to_display(v)))
                            .collect();
                        for (k, v) in props {
                            self.style_set(id, &k, &v);
                        }
                    }
                }
                let done = self.new_promise();
                self.resolve_promise(&done, Value::Undefined);
                let mut m = ObjMap::new();
                m.insert("finished".to_string(), done.clone());
                m.insert("ready".to_string(), done);
                m.insert("playState".to_string(), Value::Str("finished".to_string()));
                m.insert("currentTime".to_string(), Value::Num(0.0));
                for k in ["play", "pause", "cancel", "finish", "reverse", "addEventListener",
                          "removeEventListener"] {
                    m.insert(k.to_string(), Value::Native(Native::Noop));
                }
                Ok(Value::Obj(Rc::new(RefCell::new(m))))
            }
            Native::GetAttributeNames => {
                let Some(Value::Dom(id)) = recv else { return Ok(Value::Undefined) };
                let dom = self.dom_arena()?;
                let names: Vec<Value> = match &dom.get(id).node_type {
                    crate::dom::NodeType::Element(e) => {
                        e.attributes.keys().map(|k| Value::Str(k.clone())).collect()
                    }
                    _ => Vec::new(),
                };
                Ok(Value::Arr(ArrayObj::new(names)))
            }
            Native::HasAttributes => {
                let Some(Value::Dom(id)) = recv else { return Ok(Value::Bool(false)) };
                let dom = self.dom_arena()?;
                Ok(Value::Bool(matches!(&dom.get(id).node_type,
                    crate::dom::NodeType::Element(e) if !e.attributes.is_empty())))
            }
            // toggleAttribute(name[, force]) — 있으면 제거, 없으면 추가. 반환은 최종 존재 여부.
            Native::ToggleAttribute => {
                let Some(Value::Dom(id)) = recv else { return Ok(Value::Bool(false)) };
                let raw = args.first().map(to_display).unwrap_or_default();
                let name = self.attr_name(id, &raw)?;
                let force = args.get(1).map(to_bool);
                let dom = self.dom_arena()?;
                let has = matches!(&dom.get(id).node_type,
                    crate::dom::NodeType::Element(e) if e.attributes.contains_key(&name));
                let want = force.unwrap_or(!has);
                if want {
                    dom.set_attr(id, &name, String::new());
                } else {
                    dom.remove_attr(id, &name);
                }
                Ok(Value::Bool(want))
            }
            // replaceChildren(...nodes) — 자식을 전부 갈아끼운다
            Native::ReplaceChildren => {
                let Some(Value::Dom(id)) = recv else { return Ok(Value::Undefined) };
                let dom = self.dom_arena()?;
                dom.clear_children(id);
                for a in &args {
                    match a {
                        Value::Dom(c) => dom.append_child(id, *c),
                        other => {
                            let t = dom.create_text(to_display(other));
                            dom.append_child(id, t);
                        }
                    }
                }
                Ok(Value::Undefined)
            }
            // getAnimations() — 정적 렌더에는 진행 중인 애니메이션이 없다(빈 목록).
            Native::GetAnimations => Ok(Value::Arr(ArrayObj::new(Vec::new()))),
            // attachShadow({mode}) — 섀도 트리를 따로 두지 않고 요소 자신을 섀도 루트로
            // 돌려준다. shadowRoot.innerHTML 로 넣은 콘텐츠는 실제로 렌더된다.
            // 스타일 격리(:host, 캡슐화)는 없다 — 근사임을 문서화한다.
            Native::AttachShadow => {
                let Some(Value::Dom(id)) = recv else { return Ok(Value::Undefined) };
                self.shadow_hosts.insert(id);
                Ok(Value::Dom(id))
            }
            // CSS.supports('display','grid') 또는 CSS.supports('(display: grid)')
            Native::CssSupports => {
                let cond = match args.len() {
                    0 => return Ok(Value::Bool(false)),
                    1 => {
                        let c = to_display(&args[0]);
                        if c.trim().starts_with('(') {
                            c
                        } else {
                            format!("({})", c)
                        }
                    }
                    _ => format!("({}: {})", to_display(&args[0]), to_display(&args[1])),
                };
                Ok(Value::Bool(crate::css::supports_condition(&cond)))
            }
            // new DOMParser() → parseFromString 을 가진 객체
            Native::DomParserCtor => {
                let mut m = ObjMap::new();
                m.insert(
                    "parseFromString".to_string(),
                    Value::Native(Native::DomParserParse),
                );
                Ok(Value::Obj(Rc::new(RefCell::new(m))))
            }
            // parseFromString(html) → 분리된 <html><body>…</body></html> 서브트리.
            // 부모가 없으니 렌더 트리에 들어가지 않는다. querySelector/body 등이 동작한다.
            Native::DomParserParse => {
                let src = args.first().map(to_display).unwrap_or_default();
                let dom = self.dom_arena()?;
                let root = dom.create_element("html");
                let body = dom.create_element("body");
                dom.append_child(root, body);
                for tree in crate::html::parse_fragment(src) {
                    let sub = dom.insert_tree(tree, None);
                    dom.append_child(body, sub);
                }
                Ok(Value::Dom(root))
            }
            // window.scrollTo(x, y) 또는 scrollTo({top, left}) — 실제 스크롤 상태를 바꾼다.
            // 호스트가 이 값을 렌더에 반영한다(스크롤된 화면). 예전엔 메서드 자체가 없어
            // TypeError 로 스크립트가 죽었다.
            Native::ScrollTo | Native::ScrollBy => {
                let (mut x, mut y) = match args.first() {
                    Some(Value::Obj(o)) => {
                        let m = o.borrow();
                        (
                            m.get("left").map(to_num).unwrap_or(0.0) as f32,
                            m.get("top").map(to_num).unwrap_or(0.0) as f32,
                        )
                    }
                    Some(v) => (
                        to_num(v) as f32,
                        args.get(1).map(to_num).unwrap_or(0.0) as f32,
                    ),
                    None => (0.0, 0.0),
                };
                if matches!(n, Native::ScrollBy) {
                    x += self.scroll_x;
                    y += self.scroll_y;
                }
                self.set_scroll(x, y);
                Ok(Value::Undefined)
            }
            // el.scrollIntoView() — 그 요소가 뷰포트 위쪽에 오도록 스크롤.
            // HTMLElement.focus()/blur(): document.activeElement 를 갱신하고
            // focus/blur 이벤트를 실제로 발화한다. 예전엔 메서드 자체가 없어서
            // el?.focus() 가 "함수 아님" 으로 죽었다 (go.dev 의 키보드 내비게이션).
            // location.href 읽기 / toString (stringifier)
            Native::LocationHref => Ok(match recv {
                Some(v) => self.member_get(&v, "\u{0}href")?,
                None => Value::Str(self.base_url.clone().unwrap_or_default()),
            }),
            // location.href = … / assign(…) / replace(…) → 내비게이션 요청
            Native::LocationHrefSet | Native::LocationAssign => {
                let target = args.first().map(to_display).unwrap_or_default();
                if !target.trim().is_empty() {
                    self.navigate_to = Some(self.absolute_url(&target));
                }
                Ok(Value::Undefined)
            }
            // escape(s): A-Za-z0-9@*_+-./ 는 그대로, 나머지는 %XX / %uXXXX (Annex B.2.1)
            Native::Escape => {
                let s = args.first().map(to_display).unwrap_or_default();
                let mut out = String::with_capacity(s.len());
                for c in s.chars() {
                    let u = c as u32;
                    if c.is_ascii_alphanumeric() || "@*_+-./".contains(c) {
                        out.push(c);
                    } else if u < 256 {
                        out.push_str(&format!("%{:02X}", u));
                    } else {
                        out.push_str(&format!("%u{:04X}", u));
                    }
                }
                Ok(Value::Str(out))
            }
            // unescape(s): %XX / %uXXXX 를 되돌린다 (Annex B.2.2)
            Native::Unescape => {
                let s: Vec<char> = args.first().map(to_display).unwrap_or_default().chars().collect();
                let mut out = String::with_capacity(s.len());
                let mut i = 0;
                while i < s.len() {
                    if s[i] == '%' {
                        if i + 5 < s.len() && s[i + 1] == 'u' {
                            let hex: String = s[i + 2..i + 6].iter().collect();
                            if let Ok(v) = u32::from_str_radix(&hex, 16) {
                                if let Some(c) = char::from_u32(v) {
                                    out.push(c);
                                    i += 6;
                                    continue;
                                }
                            }
                        } else if i + 2 < s.len() {
                            let hex: String = s[i + 1..i + 3].iter().collect();
                            if let Ok(v) = u32::from_str_radix(&hex, 16) {
                                if let Some(c) = char::from_u32(v) {
                                    out.push(c);
                                    i += 3;
                                    continue;
                                }
                            }
                        }
                    }
                    out.push(s[i]);
                    i += 1;
                }
                Ok(Value::Str(out))
            }
            Native::LocationReload => {
                self.navigate_to = self.base_url.clone();
                Ok(Value::Undefined)
            }
            // BigInt(x): 문자열/불리언/정수 Number → BigInt. 소수면 RangeError (표준).
            Native::BigIntCtor => {
                use crate::js::bigint::BigInt as BI;
                let v = args.first().cloned().unwrap_or(Value::Undefined);
                let b = match &v {
                    Value::BigInt(b) => Some((**b).clone()),
                    Value::Num(n) => BI::from_f64(*n),
                    Value::Bool(b) => Some(BI::from_i64(if *b { 1 } else { 0 })),
                    Value::Str(s) => BI::parse(s),
                    _ => None,
                };
                match b {
                    Some(b) => Ok(Value::BigInt(Rc::new(b))),
                    None => Err(format!(
                        "RangeError: {} 은(는) BigInt 로 변환할 수 없음",
                        to_display(&v)
                    )),
                }
            }
            // BigInt.prototype.toString(radix)
            Native::BigIntToString => {
                let radix = match args.first() {
                    Some(Value::Num(n)) if (2.0..=36.0).contains(n) => *n as u32,
                    _ => 10,
                };
                match recv {
                    Some(Value::BigInt(b)) => Ok(Value::Str(b.to_string_radix(radix))),
                    _ => Ok(Value::Str("0".to_string())),
                }
            }
            // document.write(html): 파서 삽입 지점(= 실행 중인 스크립트 자리)에 조각을 넣는다.
            // 스크립트를 다 돌린 뒤라 삽입 지점을 모르면 body 끝에 붙인다.
            // 써진 <script> 는 큐에 넣어 호출측이 순서대로 실행한다.
            Native::DocWrite => {
                let html: String = args.iter().map(to_display).collect();
                if html.trim().is_empty() {
                    return Ok(Value::Undefined);
                }
                let Some(dom_ptr) = self.dom else { return Ok(Value::Undefined) };
                let dom = unsafe { &mut *dom_ptr };
                // 삽입 부모/기준: 스크립트의 부모에서 스크립트 바로 뒤
                let (parent, before) = match self.current_script {
                    Some(sid) => {
                        let p = dom.get(sid).parent;
                        match p {
                            Some(p) => {
                                let idx = dom.get(p).children.iter().position(|&c| c == sid);
                                let next = idx
                                    .and_then(|i| dom.get(p).children.get(i + 1).copied());
                                (p, next)
                            }
                            None => (find_body(dom), None),
                        }
                    }
                    None => (find_body(dom), None),
                };
                let mut new_scripts: Vec<crate::dom::NodeId> = Vec::new();
                for tree in crate::html::parse_fragment(html) {
                    let sub = dom.insert_tree(tree, Some(parent));
                    dom.insert_before(parent, sub, before);
                    collect_script_nodes(dom, sub, &mut new_scripts);
                }
                for sid in new_scripts {
                    let (src, code) = match &dom.get(sid).node_type {
                        crate::dom::NodeType::Element(e) => (
                            e.attributes.get("src").cloned(),
                            dom.text_content(sid),
                        ),
                        _ => (None, String::new()),
                    };
                    self.written_scripts.push((src, code));
                }
                Ok(Value::Undefined)
            }
            // document.cookie (HTML §7.11.2). 항아리는 HTTP 계층과 공유한다.
            // NamedNodeMap.getNamedItem(name) — 속성 목록의 이름 접근 (DOM 표준).
            Native::GetNamedItem => {
                let name = args.first().map(to_display).unwrap_or_default();
                Ok(match recv {
                    Some(Value::Arr(a)) => a.get_prop(&name).unwrap_or(Value::Null),
                    _ => Value::Null,
                })
            }
            Native::CookieGet => {
                let (host, path) = self.page_host_path();
                Ok(Value::Str(crate::http::cookies_for(&host, &path)))
            }
            Native::CookieSet => {
                let line = args.first().map(to_display).unwrap_or_default();
                let (host, _) = self.page_host_path();
                crate::http::store_set_cookie(&line, &host);
                Ok(Value::Undefined)
            }
            Native::ActiveElement => Ok(match self.active_element {
                Some(id) => Value::Dom(id),
                None => self.call_native(Native::DocQuery("body"), None, vec![])?,
            }),
            // el.click(): 신뢰되지 않은(isTrusted=false) 클릭 이벤트를 캡처→타깃→버블로
            // 디스패치한다. 기본 동작(링크 이동/폼 제출)은 preventDefault 되지 않았을 때만.
            Native::CurrentScript => Ok(match self.current_script {
                Some(id) => Value::Dom(id),
                None => Value::Null, // 실행 중인 클래식 스크립트가 없으면 null (표준)
            }),
            Native::ElementClick => {
                let Some(Value::Dom(id)) = recv else {
                    return Err("click 은 요소 메서드".to_string());
                };
                // 체크박스/라디오는 디스패치 **전에** 토글된다 (표준: pre-click activation).
                // 핸들러가 e.target.checked 를 읽으면 이미 바뀐 값이어야 한다.
                let toggled = self.pre_click_toggle(id);
                let evt = self.make_event("click", id);
                if let Value::Obj(o) = &evt {
                    o.borrow_mut().insert("isTrusted".to_string(), Value::Bool(false));
                }
                self.dispatch_event_value(id, "click", evt.clone());
                let prevented = match &evt {
                    Value::Obj(o) => {
                        matches!(o.borrow().get("defaultPrevented"), Some(Value::Bool(true)))
                    }
                    _ => false,
                };
                if prevented {
                    // 취소되면 토글을 되돌린다 (표준: canceled activation behavior)
                    if toggled {
                        self.pre_click_toggle(id);
                    }
                } else {
                    self.click_default_action(id);
                }
                Ok(Value::Undefined)
            }
            Native::Focus => {
                if let Some(Value::Dom(id)) = recv {
                    let prev = self.active_element;
                    if prev != Some(id) {
                        if let Some(p) = prev {
                            let e = self.make_event("blur", p);
                            self.dispatch_event_value(p, "blur", e);
                        }
                        self.active_element = Some(id);
                        let e = self.make_event("focus", id);
                        self.dispatch_event_value(id, "focus", e);
                    }
                }
                Ok(Value::Undefined)
            }
            Native::Blur => {
                if let Some(Value::Dom(id)) = recv {
                    if self.active_element == Some(id) {
                        self.active_element = None;
                        let e = self.make_event("blur", id);
                        self.dispatch_event_value(id, "blur", e);
                    }
                }
                Ok(Value::Undefined)
            }
            Native::ScrollIntoView => {
                self.ensure_layout();
                if let Some(Value::Dom(id)) = recv {
                    if let Some(&(_, y, _, _)) = self.layout_rects.get(&id) {
                        self.set_scroll(self.scroll_x, y);
                    }
                }
                Ok(Value::Undefined)
            }
            // history.pushState(state, title, url) — location 을 실제로 갱신한다.
            Native::HistoryPushState | Native::HistoryReplaceState => {
                let state = args.first().cloned().unwrap_or(Value::Null);
                let url = args.get(2).map(to_display).unwrap_or_default();
                if !url.is_empty() {
                    self.update_location(&url);
                }
                if let Some(Value::Obj(h)) = env_get(&self.global, "history") {
                    let mut m = h.borrow_mut();
                    m.insert("state".to_string(), state);
                    if matches!(n, Native::HistoryPushState) {
                        let len = m.get("length").map(to_num).unwrap_or(1.0);
                        m.insert("length".to_string(), Value::Num(len + 1.0));
                    }
                }
                Ok(Value::Undefined)
            }
            // 프렐류드의 MutationObserver 배달 함수가 호출한다. 쌓인 DOM 변형 기록을
            // JS MutationRecord 배열로 만들어 넘기고 큐를 비운다.
            Native::TakeMutations => {
                self.mutation_scheduled = false;
                let recs = match self.dom {
                    Some(p) => unsafe { (*p).take_records() },
                    None => Vec::new(),
                };
                let mut out = Vec::with_capacity(recs.len());
                for r in recs {
                    let mut m = ObjMap::new();
                    m.insert("type".to_string(), Value::Str(r.kind.to_string()));
                    m.insert("target".to_string(), Value::Dom(r.target));
                    m.insert(
                        "attributeName".to_string(),
                        r.attr.map(Value::Str).unwrap_or(Value::Null),
                    );
                    m.insert(
                        "oldValue".to_string(),
                        r.old_value.map(Value::Str).unwrap_or(Value::Null),
                    );
                    m.insert(
                        "addedNodes".to_string(),
                        Value::Arr(ArrayObj::new(r.added.into_iter().map(Value::Dom).collect())),
                    );
                    m.insert(
                        "removedNodes".to_string(),
                        Value::Arr(ArrayObj::new(r.removed.into_iter().map(Value::Dom).collect())),
                    );
                    out.push(Value::Obj(Rc::new(RefCell::new(m))));
                }
                Ok(Value::Arr(ArrayObj::new(out)))
            }
            // window.matchMedia(query) — CSS @media 와 동일한 평가기로 실제 판정한다.
            // 정적 렌더에는 뷰포트 변화가 없으므로 change 리스너는 발화하지 않는다(정직).
            Native::MatchMedia => {
                let q = args.first().map(to_display).unwrap_or_default();
                let (vw, vh) = self.viewport();
                let matches = crate::css::media_matches_vp(&q, vw, vh);
                let mut m = ObjMap::new();
                m.insert("matches".to_string(), Value::Bool(matches));
                m.insert("media".to_string(), Value::Str(q));
                m.insert("onchange".to_string(), Value::Null);
                for k in ["addListener", "removeListener", "addEventListener", "removeEventListener"]
                {
                    m.insert(k.to_string(), Value::Native(Native::Noop));
                }
                m.insert("dispatchEvent".to_string(), Value::Native(Native::Noop));
                Ok(Value::Obj(Rc::new(RefCell::new(m))))
            }
            // computedStyle.getPropertyValue('background-color') → CSS 텍스트.
            Native::ComputedGetProperty => {
                self.ensure_layout();
                let name = args.first().map(to_display).unwrap_or_default();
                Ok(match &recv {
                    Some(Value::ComputedStyle(id)) => Value::Str(
                        self.computed_styles
                            .get(id)
                            .and_then(|m| m.get(&name))
                            .cloned()
                            .unwrap_or_default(),
                    ),
                    _ => Value::Str(String::new()),
                })
            }
            Native::MapCtor => self.make_map(args),
            Native::SetCtor => self.make_set(args),
            Native::ErrorCtor(name) => {
                // Error('m') 은 new Error('m') 과 같다 (§20.5.1.1). message 는 인자가
                // 있을 때만 own 프로퍼티이고 비열거 — 객체 생성은 make_error 한 곳에서만.
                let msg = match args.first() {
                    None | Some(Value::Undefined) => None,
                    Some(v) => Some(to_display(v)),
                };
                Ok(self.make_error(name, msg))
            }
            Native::Map(op) => {
                let Some(Value::MapVal(m)) = recv else {
                    return Err("Map 메서드".to_string());
                };
                self.map_method(m, op, args)
            }
            Native::Set(op) => {
                let Some(Value::SetVal(s)) = recv else {
                    return Err("Set 메서드".to_string());
                };
                Ok(self.set_method(s, op, args))
            }
            // createElementNS(ns, name): 우리 DOM 은 네임스페이스를 따로 두지 않는다.
            // 태그 이름으로 만들고 (svg/rect 등) 그대로 렌더 파이프라인을 태운다.
            // createElementNS(namespace, qualifiedName) — DOM §4.5.
            // 예전엔 네임스페이스를 **버리고** create_element 로 넘겨서 소문자
            // HTML 요소를 만들었다. SVG 의 <linearGradient> 가 <lineargradient> 가 됐다.
            Native::CreateElementNS => {
                let ns = match args.first() {
                    None | Some(Value::Null) | Some(Value::Undefined) => None,
                    Some(v) => {
                        let s = to_display(v);
                        if s.is_empty() { None } else { Some(s) }
                    }
                };
                let qname = args.get(1).map(to_display).unwrap_or_default();
                self.validate_qualified_name(&qname, ns.as_deref())?;
                let dom = self.dom_arena()?;
                Ok(Value::Dom(dom.create_element_ns(ns.as_deref(), &qname)))
            }
            Native::CreateElement => {
                let tag = args.first().map(to_display).unwrap_or_default();
                self.validate_element_name(&tag)?;
                let dom = self.dom_arena()?;
                Ok(Value::Dom(dom.create_element(&tag)))
            }
            Native::WindowSelf => {
                Ok(env_get(&self.global, "window").unwrap_or(Value::Undefined))
            }
            // CharacterData 메서드 (§4.9). 오프셋은 UTF-16 코드 단위. 범위를 벗어나면
            // IndexSizeError — 조용히 자르면 편집기가 엉뚱한 곳을 고친다.
            Native::CharData(op) => {
                let Some(Value::Dom(id)) = recv else {
                    return Err(self.throw_error("TypeError", "CharacterData 메서드"));
                };
                let dom = self.dom_arena()?;
                let cur: Vec<u16> = match &dom.get(id).node_type {
                    crate::dom::NodeType::Text(t) => t.encode_utf16().collect(),
                    crate::dom::NodeType::Comment(c) => c.encode_utf16().collect(),
                    crate::dom::NodeType::Element(_) => {
                        return Err(self.throw_error("TypeError", "요소에는 문자 데이터가 없다"))
                    }
                };
                let len = cur.len();
                let num = |v: Option<&Value>| -> f64 { v.map(to_num).unwrap_or(0.0) };
                let (offset, count, data) = match op {
                    CharDataOp::Append => (len, 0usize, args.first().map(to_display).unwrap_or_default()),
                    CharDataOp::Insert => (
                        num(args.first()).max(0.0) as usize,
                        0,
                        args.get(1).map(to_display).unwrap_or_default(),
                    ),
                    CharDataOp::Substring | CharDataOp::Delete => (
                        num(args.first()).max(0.0) as usize,
                        num(args.get(1)).max(0.0) as usize,
                        String::new(),
                    ),
                    CharDataOp::Replace => (
                        num(args.first()).max(0.0) as usize,
                        num(args.get(1)).max(0.0) as usize,
                        args.get(2).map(to_display).unwrap_or_default(),
                    ),
                };
                if offset > len {
                    return Err(self.throw_dom("IndexSizeError", "offset 이 데이터 길이를 넘음"));
                }
                let count = count.min(len - offset);
                if matches!(op, CharDataOp::Substring) {
                    let sub = String::from_utf16_lossy(&cur[offset..offset + count]);
                    return Ok(Value::Str(sub));
                }
                let mut next: Vec<u16> = cur[..offset].to_vec();
                next.extend(data.encode_utf16());
                next.extend_from_slice(&cur[offset + count..]);
                let s = String::from_utf16_lossy(&next);
                let dom = self.dom_arena()?;
                dom.set_char_data(id, s);
                Ok(Value::Undefined)
            }
            // Text.splitText(offset) (§4.10): offset 부터를 새 텍스트 노드로 떼어
            // 바로 뒤 형제로 넣고 그 노드를 반환한다.
            Native::SplitText => {
                let Some(Value::Dom(id)) = recv else {
                    return Err(self.throw_error("TypeError", "splitText 는 Text 메서드"));
                };
                let offset = args.first().map(to_num).unwrap_or(0.0).max(0.0) as usize;
                let dom = self.dom_arena()?;
                let cur: Vec<u16> = match &dom.get(id).node_type {
                    crate::dom::NodeType::Text(t) => t.encode_utf16().collect(),
                    _ => return Err(self.throw_error("TypeError", "splitText 는 Text 메서드")),
                };
                if offset > cur.len() {
                    return Err(self.throw_dom("IndexSizeError", "offset 이 데이터 길이를 넘음"));
                }
                let head = String::from_utf16_lossy(&cur[..offset]);
                let tail = String::from_utf16_lossy(&cur[offset..]);
                let dom = self.dom_arena()?;
                dom.set_char_data(id, head);
                let new_id = dom.create_text(tail);
                if let Some(parent) = dom.get(id).parent {
                    // 원래 노드 바로 뒤에 넣는다 (다음 형제 앞, 없으면 끝)
                    let sibs = &dom.get(parent).children;
                    let next = sibs
                        .iter()
                        .position(|&c| c == id)
                        .and_then(|i| sibs.get(i + 1).copied());
                    dom.insert_before(parent, new_id, next);
                }
                Ok(Value::Dom(new_id))
            }
            Native::BindElementClass => {
                if let (Some(Value::Dom(id)), Some(ctor)) = (args.first(), args.get(1)) {
                    self.element_classes.insert(*id, ctor.clone());
                }
                Ok(Value::Undefined)
            }
            // 네임스페이스 조회 (DOM §4.4 "locate a namespace" / "locate a namespace prefix").
            // 요소에서 시작해 조상으로 올라가며 xmlns / xmlns:prefix 선언을 찾는다.
            // 예전엔 아예 없어서 XML/SVG 를 다루는 코드가 그 줄에서 죽었다.
            Native::LookupNamespaceURI | Native::IsDefaultNamespace => {
                let Some(Value::Dom(id)) = recv else { return Ok(Value::Null) };
                let want_default = matches!(n, Native::IsDefaultNamespace);
                let arg = args.first().cloned().unwrap_or(Value::Undefined);
                let prefix = match (&arg, want_default) {
                    // isDefaultNamespace(ns) 는 인자가 네임스페이스다 (접두사가 아니다)
                    (_, true) => String::new(),
                    (Value::Null | Value::Undefined, false) => String::new(),
                    (v, false) => to_display(v),
                };
                let ns = self.locate_namespace(id, &prefix)?;
                if want_default {
                    let want = match &arg {
                        Value::Null | Value::Undefined => None,
                        v => {
                            let s = to_display(v);
                            if s.is_empty() { None } else { Some(s) }
                        }
                    };
                    return Ok(Value::Bool(ns == want));
                }
                Ok(ns.map(Value::Str).unwrap_or(Value::Null))
            }
            Native::LookupPrefix => {
                let Some(Value::Dom(id)) = recv else { return Ok(Value::Null) };
                let ns = match args.first() {
                    None | Some(Value::Null) | Some(Value::Undefined) => return Ok(Value::Null),
                    Some(v) => to_display(v),
                };
                if ns.is_empty() {
                    return Ok(Value::Null);
                }
                Ok(self.locate_prefix(id, &ns)?.map(Value::Str).unwrap_or(Value::Null))
            }
            // getAttributeNode(name) → Attr 노드 또는 null (§4.9.2)
            Native::GetAttributeNode => {
                let Some(Value::Dom(id)) = recv else { return Ok(Value::Null) };
                let name = args.first().map(to_display).unwrap_or_default().to_ascii_lowercase();
                let dom = self.dom_arena()?;
                let has = matches!(&dom.get(id).node_type,
                    crate::dom::NodeType::Element(e) if e.attributes.get(&name).is_some());
                Ok(if has { Value::Attr(id, name) } else { Value::Null })
            }
            // setAttributeNode(attr) → 같은 이름의 기존 Attr 를 반환(없으면 null)
            Native::SetAttributeNode => {
                let Some(Value::Dom(id)) = recv else { return Ok(Value::Null) };
                let Some(Value::Attr(src, name)) = args.first().cloned() else {
                    return Err(self.throw_error("TypeError", "setAttributeNode 인자는 Attr"));
                };
                let dom = self.dom_arena()?;
                let value = match &dom.get(src).node_type {
                    crate::dom::NodeType::Element(e) => {
                        e.attributes.get(&name).cloned().unwrap_or_default()
                    }
                    _ => String::new(),
                };
                let old = matches!(&dom.get(id).node_type,
                    crate::dom::NodeType::Element(e) if e.attributes.get(&name).is_some());
                dom.set_attr(id, &name, value);
                Ok(if old { Value::Attr(id, name) } else { Value::Null })
            }
            Native::RemoveAttributeNode => {
                let Some(Value::Dom(id)) = recv else { return Ok(Value::Null) };
                let Some(Value::Attr(_, name)) = args.first().cloned() else {
                    return Err(self.throw_error("TypeError", "removeAttributeNode 인자는 Attr"));
                };
                let dom = self.dom_arena()?;
                dom.remove_attr(id, &name);
                Ok(Value::Attr(id, name))
            }
            // document.styleSheets — 저작자 시트 목록 (문서 순서)
            Native::StyleSheets => {
                self.sync_sheets();
                let n = self.sheets().map(|s| s.len()).unwrap_or(0);
                let list: Vec<Value> = (0..n).map(Value::Sheet).collect();
                let arr = ArrayObj::new(list);
                arr.set_prop("item".to_string(), Value::Native(Native::ListItem));
                Ok(Value::Arr(arr))
            }
            // 컬렉션의 item(i) — 범위를 벗어나면 null (표준)
            Native::ListItem => {
                let i = args.first().map(to_num).unwrap_or(0.0);
                let Some(Value::Arr(a)) = recv else { return Ok(Value::Null) };
                if i < 0.0 {
                    return Ok(Value::Null);
                }
                let v = a.borrow().get(i as usize).cloned();
                Ok(v.unwrap_or(Value::Null))
            }
            // 플랫폼 객체의 인터페이스 상속 체인 (WebIDL). instanceof 와 constructor 가
            // 이걸 쓴다 — 예전엔 오리 판별(프로퍼티 존재 여부)이라 평범한 객체도 통과했고,
            // 요소에는 constructor 자체가 없었다.
            Native::Brand => {
                let chain: Vec<&str> = match args.first() {
                    Some(Value::Dom(id)) => {
                        let dom = self.dom_arena()?;
                        match &dom.get(*id).node_type {
                            crate::dom::NodeType::Element(e) => {
                                let iface = crate::dom::Dom::element_interface(&e.tag_name);
                                let mut c = vec![iface];
                                if iface.starts_with("SVG") {
                                    if iface != "SVGElement" {
                                        c.push("SVGElement");
                                    }
                                } else {
                                    if iface != "HTMLElement" {
                                        c.push("HTMLElement");
                                    }
                                }
                                c.extend(["Element", "Node", "EventTarget"]);
                                c
                            }
                            crate::dom::NodeType::Text(_) => {
                                vec!["Text", "CharacterData", "Node", "EventTarget"]
                            }
                            crate::dom::NodeType::Comment(_) => {
                                vec!["Comment", "CharacterData", "Node", "EventTarget"]
                            }
                        }
                    }
                    Some(Value::Attr(_, _)) => vec!["Attr", "Node", "EventTarget"],
                    Some(Value::Sheet(_)) => vec!["CSSStyleSheet", "StyleSheet"],
                    Some(Value::CssRule(_, _)) => vec!["CSSStyleRule", "CSSRule"],
                    Some(Value::RuleStyle(_, _))
                    | Some(Value::Style(_))
                    | Some(Value::ComputedStyle(_)) => vec!["CSSStyleDeclaration"],
                    _ => vec![],
                };
                Ok(Value::Arr(ArrayObj::new(
                    chain.into_iter().map(|s| Value::Str(s.to_string())).collect(),
                )))
            }
            // CSSOM 메서드
            Native::SheetInsertRule => {
                let Some(Value::Sheet(si)) = recv else { return Ok(Value::Num(0.0)) };
                let text = args.first().map(to_display).unwrap_or_default();
                let idx = args.get(1).map(to_num).unwrap_or(0.0).max(0.0) as usize;
                self.sheet_insert_rule(si, &text, idx)
            }
            Native::SheetDeleteRule => {
                let Some(Value::Sheet(si)) = recv else { return Ok(Value::Undefined) };
                let idx = args.first().map(to_num).unwrap_or(0.0).max(0.0) as usize;
                self.sheet_delete_rule(si, idx)
            }
            Native::RuleStyleGet => {
                let Some(Value::RuleStyle(si, ri)) = recv else { return Ok(Value::Str(String::new())) };
                let prop = args.first().map(to_display).unwrap_or_default();
                Ok(Value::Str(self.rule_prop(si, ri, &prop)))
            }
            Native::RuleStyleSet => {
                let Some(Value::RuleStyle(si, ri)) = recv else { return Ok(Value::Undefined) };
                let prop = args.first().map(to_display).unwrap_or_default();
                let val = args.get(1).map(to_display).unwrap_or_default();
                self.rule_set_prop(si, ri, &prop, &val);
                Ok(Value::Undefined)
            }
            Native::RuleStyleRemove => {
                let Some(Value::RuleStyle(si, ri)) = recv else { return Ok(Value::Str(String::new())) };
                let prop = args.first().map(to_display).unwrap_or_default();
                let old = self.rule_prop(si, ri, &prop);
                self.rule_set_prop(si, ri, &prop, "");
                Ok(Value::Str(old))
            }
            Native::RuleStyleItem => {
                let Some(Value::RuleStyle(si, ri)) = recv else { return Ok(Value::Str(String::new())) };
                let i = args.first().map(to_num).unwrap_or(0.0).max(0.0) as usize;
                let key = i.to_string();
                self.cssom_get(&Value::RuleStyle(si, ri), &key)
            }
            Native::CreateComment => {
                let data = args.first().map(to_display).unwrap_or_default();
                let dom = self.dom_arena()?;
                Ok(Value::Dom(dom.create_comment(data)))
            }
            // 네임스페이스 속성 (§4.9.2). 우리 AttrMap 은 정규화된 이름(qualified name)을
            // 키로 쓴다 — setAttributeNS(ns, 'xlink:href', v) 는 'xlink:href' 로 저장되고,
            // getAttributeNS(ns, 'href') 는 로컬 이름으로 되찾는다.
            Native::SetAttributeNS => {
                let Some(Value::Dom(id)) = recv else {
                    return Err(self.throw_error("TypeError", "setAttributeNS 는 요소 메서드"));
                };
                let qname = args.get(1).map(to_display).unwrap_or_default();
                let value = args.get(2).map(to_display).unwrap_or_default();
                if qname.is_empty() {
                    return Err(self.throw_dom("InvalidCharacterError", "속성 이름이 비었다"));
                }
                let dom = self.dom_arena()?;
                dom.set_attr(id, &qname, value);
                Ok(Value::Undefined)
            }
            Native::GetAttributeNS | Native::HasAttributeNS | Native::RemoveAttributeNS => {
                let Some(Value::Dom(id)) = recv else { return Ok(Value::Null) };
                let local = args.get(1).map(to_display).unwrap_or_default();
                let dom = self.dom_arena()?;
                // 로컬 이름 또는 prefix:local 로 저장된 속성을 찾는다
                let found = {
                    let node = dom.get(id);
                    match &node.node_type {
                        crate::dom::NodeType::Element(e) => e
                            .attributes
                            .iter()
                            .find(|(k, _)| {
                                k.as_str() == local
                                    || k.rsplit(':').next() == Some(local.as_str())
                            })
                            .map(|(k, v)| (k.clone(), v.clone())),
                        _ => None,
                    }
                };
                match n {
                    Native::HasAttributeNS => Ok(Value::Bool(found.is_some())),
                    Native::RemoveAttributeNS => {
                        if let Some((k, _)) = found {
                            dom.remove_attr(id, &k);
                        }
                        Ok(Value::Undefined)
                    }
                    _ => Ok(found.map(|(_, v)| Value::Str(v)).unwrap_or(Value::Null)),
                }
            }
            Native::CreateTextNode => {
                let text = args.first().map(to_display).unwrap_or_default();
                let dom = self.dom_arena()?;
                Ok(Value::Dom(dom.create_text(text)))
            }
            Native::CreateDocumentFragment => {
                // 프래그먼트: 센티널 태그 컨테이너. appendChild 시 자식만 옮겨진다.
                let dom = self.dom_arena()?;
                Ok(Value::Dom(dom.create_element("#document-fragment")))
            }
            // style.setProperty(name, value) / getPropertyValue(name) / removeProperty(name)
            Native::StyleSetProperty => {
                if let Some(Value::Style(id)) = recv {
                    let name = args.first().map(to_display).unwrap_or_default();
                    let value = args.get(1).map(to_display).unwrap_or_default();
                    self.style_set(id, &name, &value);
                }
                Ok(Value::Undefined)
            }
            Native::StyleGetProperty => {
                if let Some(Value::Style(id)) = recv {
                    let name = args.first().map(to_display).unwrap_or_default();
                    return Ok(Value::Str(self.style_get(id, &name)));
                }
                Ok(Value::Str(String::new()))
            }
            Native::StyleRemoveProperty => {
                if let Some(Value::Style(id)) = recv {
                    let name = args.first().map(to_display).unwrap_or_default();
                    self.style_set(id, &name, "");
                }
                Ok(Value::Undefined)
            }
            // DOMTokenList (§7.1). 토큰 검증 → 순서 보존 → update steps.
            // 예전엔 검증이 없었고(빈 토큰/공백 든 토큰을 조용히 통과), add 가 기존
            // 토큰을 지웠다 다시 붙여 **순서를 바꿨다**. toggle 의 force 규칙도 틀렸다.
            Native::ClassAdd | Native::ClassRemove => {
                let Some(Value::ClassList(id)) = recv else { return Ok(Value::Undefined) };
                let names: Vec<String> = args.iter().map(to_display).collect();
                self.validate_tokens(&names)?;
                let mut tokens = self.class_tokens(id);
                for name in &names {
                    if matches!(n, Native::ClassAdd) {
                        // 이미 있으면 **그 자리에 그대로** 둔다 (순서 보존)
                        if !tokens.iter().any(|t| t == name) {
                            tokens.push(name.clone());
                        }
                    } else {
                        tokens.retain(|t| t != name);
                    }
                }
                self.set_class_tokens(id, tokens);
                Ok(Value::Undefined)
            }
            // toggle(token[, force]) — force 가 주어지면:
            //   있고 force=true  → 아무것도 안 하고 true (속성도 안 건드린다)
            //   있고 force=false → 제거하고 false
            //   없고 force=true  → 추가하고 true
            //   없고 force=false → 아무것도 안 하고 false
            Native::ClassToggle => {
                let Some(Value::ClassList(id)) = recv else { return Ok(Value::Bool(false)) };
                let name = args.first().map(to_display).unwrap_or_default();
                self.validate_tokens(std::slice::from_ref(&name))?;
                let mut tokens = self.class_tokens(id);
                let present = tokens.iter().any(|t| t == &name);
                // 인자를 아예 안 준 경우와 undefined 를 준 경우는 다르다 (표준)
                let force = args.get(1).filter(|v| !matches!(v, Value::Undefined)).map(to_bool);
                match (present, force) {
                    (true, Some(true)) => Ok(Value::Bool(true)), // 변경 없음
                    (true, _) => {
                        tokens.retain(|t| t != &name);
                        self.set_class_tokens(id, tokens);
                        Ok(Value::Bool(false))
                    }
                    (false, Some(false)) => Ok(Value::Bool(false)), // 변경 없음
                    (false, _) => {
                        tokens.push(name);
                        self.set_class_tokens(id, tokens);
                        Ok(Value::Bool(true))
                    }
                }
            }
            // replace(old, new) — old 가 없으면 false (속성도 안 건드린다).
            // 있으면 **그 자리에서** new 로 바꾼다 (뒤에 붙이지 않는다).
            Native::ClassReplace => {
                let Some(Value::ClassList(id)) = recv else { return Ok(Value::Bool(false)) };
                let old = args.first().map(to_display).unwrap_or_default();
                let new = args.get(1).map(to_display).unwrap_or_default();
                self.validate_tokens(&[old.clone(), new.clone()])?;
                let mut tokens = self.class_tokens(id);
                if !tokens.iter().any(|t| t == &old) {
                    return Ok(Value::Bool(false)); // 없으면 속성도 안 건드린다
                }
                // 순서 집합의 replace (Infra): old 또는 new 와 같은 항목을 모두 없애고,
                // **둘 중 먼저 나온 위치**에 new 를 하나 넣는다.
                //   "c b a" 에서 c → a 는 "a b" 다 ("b a" 가 아니다)
                let pos = tokens
                    .iter()
                    .position(|t| t == &old || t == &new)
                    .unwrap_or(0);
                let mut out: Vec<String> = Vec::with_capacity(tokens.len());
                for (i, t) in tokens.drain(..).enumerate() {
                    if i == pos {
                        out.push(new.clone());
                    } else if t != old && t != new {
                        out.push(t);
                    }
                }
                self.set_class_tokens(id, out);
                Ok(Value::Bool(true))
            }
            // supports(token) — class 속성에는 지원 토큰 목록이 없다 → TypeError (표준)
            Native::ClassSupports => {
                Err(self.throw_error("TypeError", "class 속성은 지원 토큰 목록이 없다"))
            }
            Native::ClassItem => {
                let Some(Value::ClassList(id)) = recv else { return Ok(Value::Null) };
                let i = args.first().map(to_num).unwrap_or(0.0);
                if i < 0.0 {
                    return Ok(Value::Null);
                }
                let t = self.class_tokens(id);
                Ok(t.get(i as usize).cloned().map(Value::Str).unwrap_or(Value::Null))
            }
            Native::ClassValue => {
                let Some(Value::ClassList(id)) = recv else { return Ok(Value::Str(String::new())) };
                Ok(Value::Str(self.class_attr(id)))
            }
            Native::ClassContains => {
                let Some(Value::ClassList(id)) = recv else { return Ok(Value::Bool(false)) };
                let name = args.first().map(to_display).unwrap_or_default();
                Ok(Value::Bool(self.class_tokens(id).iter().any(|t| t == &name)))
            }
            // event.preventDefault() / stopPropagation() — recv 가 이벤트 객체
            // getElementsByClassName / getElementsByTagName — 서브트리 수집
            Native::GetElementsByClass | Native::GetElementsByTag => {
                let scope = match &recv {
                    Some(Value::Dom(id)) => Some(*id),
                    _ => None, // document 등 → 루트
                };
                let query = args.first().map(to_display).unwrap_or_default();
                let by_class = matches!(n, Native::GetElementsByClass);
                let dom = self.dom_arena()?;
                let root = scope.unwrap_or(dom.root);
                let mut out = Vec::new();
                collect_elements(dom, root, scope.is_some(), &query, by_class, &mut out);
                Ok(Value::Arr(ArrayObj::new(out)))
            }
            Native::EventPreventDefault => {
                if let Some(Value::Obj(o)) = &recv {
                    o.borrow_mut().insert("defaultPrevented".to_string(), Value::Bool(true));
                }
                Ok(Value::Undefined)
            }
            Native::EventStopProp => {
                if let Some(Value::Obj(o)) = &recv {
                    o.borrow_mut().insert("\u{0}stopProp".to_string(), Value::Bool(true));
                }
                Ok(Value::Undefined)
            }
            Native::XhrCtor => Ok(self.make_xhr()),
            Native::WebSocketCtor => Ok(self.make_websocket(args)),
            Native::WsSend => {
                let idx = match &recv {
                    Some(Value::Obj(o)) => match o.borrow().get("\u{0}sock") {
                        Some(Value::Num(n)) => *n as usize,
                        _ => usize::MAX,
                    },
                    _ => usize::MAX,
                };
                let Some((ws, _)) = self.sockets.get_mut(idx) else {
                    return Err("WebSocket 이 연결돼 있지 않다".to_string());
                };
                let data = args.first().cloned().unwrap_or(Value::Undefined);
                let r = match &data {
                    Value::Arr(a) => {
                        let bytes: Vec<u8> = a
                            .borrow()
                            .iter()
                            .map(|v| match v {
                                Value::Num(n) => *n as i64 as u8,
                                _ => 0,
                            })
                            .collect();
                        ws.send_binary(&bytes)
                    }
                    other => ws.send_text(&to_display(other)),
                };
                r.map(|_| Value::Undefined)
            }
            Native::WsClose => {
                if let Some(Value::Obj(o)) = &recv {
                    let idx = match o.borrow().get("\u{0}sock") {
                        Some(Value::Num(n)) => *n as usize,
                        _ => usize::MAX,
                    };
                    if let Some((ws, _)) = self.sockets.get_mut(idx) {
                        ws.close();
                    }
                    o.borrow_mut().insert("readyState".to_string(), Value::Num(3.0));
                }
                Ok(Value::Undefined)
            }
            // XHR: open(method, url) → __method/__url 저장, readyState=1
            Native::XhrOpen => {
                if let Some(Value::Obj(o)) = &recv {
                    let method = args.first().map(to_display).unwrap_or_else(|| "GET".to_string());
                    let raw = args.get(1).map(to_display).unwrap_or_default();
                    let url = self.absolute_url(&raw);
                    let mut b = o.borrow_mut();
                    b.insert("\u{0}method".to_string(), Value::Str(method));
                    b.insert("\u{0}url".to_string(), Value::Str(url));
                    b.insert("readyState".to_string(), Value::Num(1.0));
                }
                Ok(Value::Undefined)
            }
            // setRequestHeader 를 버리면 Content-Type/Authorization 없이 요청이 나간다 —
            // 서버는 다른 답을 주고, 사이트는 그걸 자기 요청의 답이라고 믿는다.
            Native::XhrSetHeader => {
                if let Some(Value::Obj(o)) = &recv {
                    let k = args.first().map(to_display).unwrap_or_default();
                    let v = args.get(1).map(to_display).unwrap_or_default();
                    let key = "\u{0}reqheaders".to_string();
                    let existing = o.borrow().get(&key).cloned();
                    let arr = match existing {
                        Some(Value::Arr(a)) => a,
                        _ => {
                            let a = ArrayObj::new(Vec::new());
                            o.borrow_mut().insert(key, Value::Arr(a.clone()));
                            a
                        }
                    };
                    arr.borrow_mut()
                        .push(Value::Arr(ArrayObj::new(vec![Value::Str(k), Value::Str(v)])));
                }
                Ok(Value::Undefined)
            }
            // getResponseHeader(name) — 실제 응답 헤더. 예전엔 무조건 null 이었다
            // (Content-Type 을 보고 분기하는 코드가 전부 틀린 길로 갔다).
            Native::XhrGetHeader => {
                let Some(Value::Obj(o)) = &recv else { return Ok(Value::Null) };
                let want = args.first().map(to_display).unwrap_or_default();
                let hs = o.borrow().get("\u{0}resheaders").cloned();
                let Some(Value::Arr(a)) = hs else { return Ok(Value::Null) };
                // 인자가 없으면 getAllResponseHeaders() — 전체를 한 문자열로
                if want.is_empty() {
                    let mut out = String::new();
                    for row in a.borrow().iter() {
                        if let Value::Arr(kv) = row {
                            let kv = kv.borrow();
                            out.push_str(&format!(
                                "{}: {}\r\n",
                                to_display(&kv[0]),
                                to_display(&kv[1])
                            ));
                        }
                    }
                    return Ok(Value::Str(out));
                }
                for row in a.borrow().iter() {
                    if let Value::Arr(kv) = row {
                        let kv = kv.borrow();
                        if to_display(&kv[0]).eq_ignore_ascii_case(&want) {
                            return Ok(kv[1].clone());
                        }
                    }
                }
                Ok(Value::Null)
            }
            // XHR: send() → 동기 HTTP, 필드 설정 후 readystatechange/load 발화
            Native::XhrSend => {
                let obj = match &recv {
                    Some(Value::Obj(o)) => o.clone(),
                    _ => return Ok(Value::Undefined),
                };
                let url = match obj.borrow().get("\u{0}url") {
                    Some(Value::Str(u)) => u.clone(),
                    _ => String::new(),
                };
                let full = self.resolve_url(&url);
                // 메서드/헤더/본문을 실제로 보낸다. 예전엔 전부 무시하고 GET 을 보냈다.
                let method = match obj.borrow().get("\u{0}method") {
                    Some(Value::Str(m)) => m.clone(),
                    _ => "GET".to_string(),
                };
                let headers: Vec<(String, String)> = match obj.borrow().get("\u{0}reqheaders") {
                    Some(Value::Arr(a)) => a
                        .borrow()
                        .iter()
                        .filter_map(|row| match row {
                            Value::Arr(kv) => {
                                let kv = kv.borrow();
                                Some((to_display(&kv[0]), to_display(&kv[1])))
                            }
                            _ => None,
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                let body_arg = args.first().cloned().unwrap_or(Value::Undefined);
                let body = match &body_arg {
                    Value::Undefined | Value::Null => None,
                    v => Some(to_display(v).into_bytes()),
                };
                let req = crate::http::Request { method, headers, body };
                match crate::http::fetch_req(&full, &req) {
                    Ok(r) => {
                        let body = String::from_utf8_lossy(&r.body).to_string();
                        let res_headers: Vec<Value> = r
                            .headers
                            .iter()
                            .map(|(k, v)| {
                                Value::Arr(ArrayObj::new(vec![
                                    Value::Str(k.clone()),
                                    Value::Str(v.clone()),
                                ]))
                            })
                            .collect();
                        let mut b = obj.borrow_mut();
                        b.insert(
                            "\u{0}resheaders".to_string(),
                            Value::Arr(ArrayObj::new(res_headers)),
                        );
                        b.insert("status".to_string(), Value::Num(r.status as f64));
                        b.insert(
                            "statusText".to_string(),
                            Value::Str(if r.status == 200 { "OK".into() } else { String::new() }),
                        );
                        b.insert("responseText".to_string(), Value::Str(body.clone()));
                        b.insert("response".to_string(), Value::Str(body));
                        b.insert("readyState".to_string(), Value::Num(4.0));
                    }
                    Err(e) => {
                        self.console.push(format!("XHR 실패: {:?}", e));
                        let mut b = obj.borrow_mut();
                        b.insert("status".to_string(), Value::Num(0.0));
                        b.insert("readyState".to_string(), Value::Num(4.0));
                    }
                }
                // 발화 순서: readystatechange → load
                self.xhr_fire(&obj, "readystatechange");
                self.xhr_fire(&obj, "load");
                self.xhr_fire(&obj, "loadend");
                Ok(Value::Undefined)
            }
            Native::DateNow => Ok(Value::Num(now_millis())),
            // Date.parse(str) → 밀리초(파싱 불가 시 NaN)
            Native::DateParse => {
                let millis = match args.first() {
                    Some(Value::Str(s)) => parse_date_string(s).unwrap_or(f64::NAN),
                    Some(v) => parse_date_string(&to_display(v)).unwrap_or(f64::NAN),
                    None => f64::NAN,
                };
                Ok(Value::Num(millis))
            }
            // Date.UTC(year, month[0기준], day, h, m, s, ms) → 밀리초(UTC)
            Native::DateUTC => {
                let g = |i: usize, dflt: f64| args.get(i).map(to_num).unwrap_or(dflt);
                if args.is_empty() {
                    return Ok(Value::Num(f64::NAN));
                }
                let millis = date_to_millis(
                    g(0, 1970.0) as i64,
                    g(1, 0.0) as i64 + 1,
                    g(2, 1.0) as i64,
                    g(3, 0.0) as i64,
                    g(4, 0.0) as i64,
                    g(5, 0.0) as i64,
                    g(6, 0.0) as i64,
                );
                Ok(Value::Num(millis))
            }
            Native::DateCtor => Ok(self.make_date_from_args(&args)),
            // date.getFullYear() 등 — recv 가 Date 객체
            Native::DateMethod(field) => {
                let millis = match &recv {
                    Some(Value::Obj(m)) => match m.borrow().get("\u{0}time") {
                        Some(Value::Num(n)) => *n,
                        _ => f64::NAN,
                    },
                    _ => f64::NAN,
                };
                let (y, mo, d, h, mi, s, ms, wd) = date_parts(millis);
                // setter: 현재 값을 구성요소로 풀고 해당 필드만 바꿔 다시 조립한다.
                // 반환값은 새 타임스탬프 (표준), 객체 자신은 제자리에서 바뀐다.
                if matches!(
                    field,
                    DateField::SetTime
                        | DateField::SetFullYear
                        | DateField::SetMonth
                        | DateField::SetDate
                        | DateField::SetHours
                        | DateField::SetMinutes
                        | DateField::SetSeconds
                        | DateField::SetMs
                ) {
                    let a = |i: usize| args.get(i).map(to_num);
                    let new_millis = match field {
                        DateField::SetTime => a(0).unwrap_or(f64::NAN),
                        DateField::SetFullYear => date_to_millis(
                            a(0).unwrap_or(y as f64) as i64,
                            a(1).map(|v| v as i64 + 1).unwrap_or(mo as i64),
                            a(2).map(|v| v as i64).unwrap_or(d as i64),
                            h as i64,
                            mi as i64,
                            s as i64,
                            ms as i64,
                        ),
                        DateField::SetMonth => date_to_millis(
                            y,
                            a(0).unwrap_or((mo - 1) as f64) as i64 + 1,
                            a(1).map(|v| v as i64).unwrap_or(d as i64),
                            h as i64,
                            mi as i64,
                            s as i64,
                            ms as i64,
                        ),
                        DateField::SetDate => date_to_millis(
                            y,
                            mo as i64,
                            a(0).unwrap_or(d as f64) as i64,
                            h as i64,
                            mi as i64,
                            s as i64,
                            ms as i64,
                        ),
                        DateField::SetHours => date_to_millis(
                            y,
                            mo as i64,
                            d as i64,
                            a(0).unwrap_or(h as f64) as i64,
                            a(1).map(|v| v as i64).unwrap_or(mi as i64),
                            a(2).map(|v| v as i64).unwrap_or(s as i64),
                            a(3).map(|v| v as i64).unwrap_or(ms as i64),
                        ),
                        DateField::SetMinutes => date_to_millis(
                            y,
                            mo as i64,
                            d as i64,
                            h as i64,
                            a(0).unwrap_or(mi as f64) as i64,
                            a(1).map(|v| v as i64).unwrap_or(s as i64),
                            a(2).map(|v| v as i64).unwrap_or(ms as i64),
                        ),
                        DateField::SetSeconds => date_to_millis(
                            y,
                            mo as i64,
                            d as i64,
                            h as i64,
                            mi as i64,
                            a(0).unwrap_or(s as f64) as i64,
                            a(1).map(|v| v as i64).unwrap_or(ms as i64),
                        ),
                        _ => date_to_millis(
                            y,
                            mo as i64,
                            d as i64,
                            h as i64,
                            mi as i64,
                            s as i64,
                            a(0).unwrap_or(ms as f64) as i64,
                        ),
                    };
                    if let Some(Value::Obj(m)) = &recv {
                        m.borrow_mut()
                            .insert("\u{0}time".to_string(), Value::Num(new_millis));
                    }
                    return Ok(Value::Num(new_millis));
                }
                Ok(match field {
                    // setter 는 위에서 이미 처리하고 반환했다
                    DateField::SetTime
                    | DateField::SetFullYear
                    | DateField::SetMonth
                    | DateField::SetDate
                    | DateField::SetHours
                    | DateField::SetMinutes
                    | DateField::SetSeconds
                    | DateField::SetMs => Value::Undefined,
                    DateField::Time => Value::Num(millis),
                    DateField::FullYear => Value::Num(y as f64),
                    DateField::Month => Value::Num((mo - 1) as f64), // JS 는 0 기준
                    DateField::Date => Value::Num(d as f64),
                    DateField::Day => Value::Num(wd as f64),
                    DateField::Hours => Value::Num(h as f64),
                    DateField::Minutes => Value::Num(mi as f64),
                    DateField::Seconds => Value::Num(s as f64),
                    DateField::Ms => Value::Num(ms as f64),
                    DateField::TimezoneOffset => Value::Num(0.0),
                    DateField::ToIso => Value::Str(date_iso(millis)),
                    DateField::ToStr => Value::Str(date_string(millis)),
                    DateField::ToDateStr => Value::Str(date_string(millis)),
                })
            }
            // String(x)/Number(x)/Boolean(x) 변환 생성자
            Native::StringCtor => {
                // ToString: 객체는 ToPrimitive(hint string) → toString/valueOf 호출.
                let s = match args.into_iter().next() {
                    Some(v) => {
                        let p = self.to_primitive(v, true);
                        to_display(&p)
                    }
                    None => String::new(),
                };
                Ok(Value::Str(s))
            }
            Native::NumberCtor => Ok(Value::Num(match args.first() {
                Some(v) => to_num(v),
                None => 0.0,
            })),
            Native::BooleanCtor => Ok(Value::Bool(args.first().map(to_bool).unwrap_or(false))),
            Native::StrFromCharCode => {
                let s: String = args
                    .iter()
                    .filter_map(|a| char::from_u32(to_num(a) as u32))
                    .collect();
                Ok(Value::Str(s))
            }
            // String.raw(template, ...subs) (§22.1.2.4): template.raw 의 각 세그먼트를
            // 치환값과 번갈아 잇는다. 태그된 템플릿의 원시 문자열용.
            Native::StrRaw => {
                let template = args.first().cloned().unwrap_or(Value::Undefined);
                let raw = self.member_get(&template, "raw")?;
                // raw.length
                let len = match self.member_get(&raw, "length") {
                    Ok(v) => to_num(&v),
                    Err(_) => f64::NAN,
                };
                let len = if len.is_finite() && len > 0.0 { len as usize } else { 0 };
                let subs = &args[1.min(args.len())..];
                let mut out = String::new();
                for i in 0..len {
                    let seg = self.member_get(&raw, &i.to_string())?;
                    out.push_str(&to_display(&seg));
                    if i + 1 == len {
                        break;
                    }
                    if let Some(s) = subs.get(i) {
                        out.push_str(&to_display(s));
                    }
                }
                Ok(Value::Str(out))
            }
            Native::NumIsInteger => {
                let ok = matches!(args.first(), Some(Value::Num(n)) if n.fract() == 0.0 && n.is_finite());
                Ok(Value::Bool(ok))
            }
            Native::NumIsFinite => {
                let ok = matches!(args.first(), Some(Value::Num(n)) if n.is_finite());
                Ok(Value::Bool(ok))
            }
            Native::NumIsNaN => {
                let ok = matches!(args.first(), Some(Value::Num(n)) if n.is_nan());
                Ok(Value::Bool(ok))
            }
            // recv.toString([radix]) / valueOf()
            Native::ValueToStr => {
                let v0 = recv.unwrap_or(Value::Undefined);
                // 원시 래퍼는 내부 슬롯을 문자열화 대상으로
                let v = wrapper_primitive(&v0).unwrap_or(v0);
                // 숫자 + radix 면 진법 변환 (§21.1.3.6). radix 는 2..=36, 아니면 RangeError.
                if let (Value::Num(n), Some(rv)) = (&v, args.first()) {
                    // radix 인자가 undefined 면 10 (기본)
                    if !matches!(rv, Value::Undefined) {
                        let radix = to_num(rv);
                        let radix = if radix.is_nan() { 10 } else { radix as i64 };
                        if !(2..=36).contains(&radix) {
                            return Err(self.throw_error(
                                "RangeError",
                                "toString() radix must be between 2 and 36",
                            ));
                        }
                        if radix != 10 {
                            return Ok(Value::Str(num_to_radix(*n, radix as u32)));
                        }
                    }
                }
                Ok(Value::Str(to_display(&v)))
            }
            Native::ValueOfSelf => {
                let this = recv.unwrap_or(Value::Undefined);
                // 원시 래퍼(new Number 등)의 valueOf 는 내부 슬롯을 돌려준다.
                Ok(wrapper_primitive(&this).unwrap_or(this))
            }
            // 원시 래퍼 프로토타입의 brand-checked valueOf (thisBooleanValue 등, §20.3.3.3).
            Native::PrimValueOf(brand) => {
                let this = recv.unwrap_or(Value::Undefined);
                self.this_prim_value(&this, brand)
            }
            // 원시 래퍼 프로토타입의 brand-checked toString (§20.3.3.2/§21.1.3.6/§22.1.3.28).
            Native::PrimToString(brand) => {
                let this = recv.unwrap_or(Value::Undefined);
                let prim = self.this_prim_value(&this, brand)?;
                match brand {
                    PrimBrand::Boolean => Ok(Value::Str(
                        if matches!(prim, Value::Bool(true)) { "true" } else { "false" }.to_string(),
                    )),
                    PrimBrand::String => Ok(Value::Str(match prim {
                        Value::Str(s) => s,
                        _ => String::new(),
                    })),
                    PrimBrand::Number => {
                        let n = match prim {
                            Value::Num(n) => n,
                            _ => f64::NAN,
                        };
                        // radix 인자 (§21.1.3.6): 2..=36, 아니면 RangeError.
                        if let Some(rv) = args.first() {
                            if !matches!(rv, Value::Undefined) {
                                let radix = to_num(rv);
                                let radix = if radix.is_nan() { 10 } else { radix as i64 };
                                if !(2..=36).contains(&radix) {
                                    return Err(self.throw_error(
                                        "RangeError",
                                        "toString() radix must be between 2 and 36",
                                    ));
                                }
                                if radix != 10 {
                                    return Ok(Value::Str(num_to_radix(n, radix as u32)));
                                }
                            }
                        }
                        Ok(Value::Str(to_display(&Value::Num(n))))
                    }
                }
            }
            // n.toFixed(digits) — recv 가 숫자
            Native::NumToFixed => {
                let n = match recv {
                    Some(v) => to_num(&v),
                    None => 0.0,
                };
                let digits = args.first().map(to_num).unwrap_or(0.0).clamp(0.0, 100.0) as usize;
                Ok(Value::Str(format!("{:.*}", digits, n)))
            }
            // RegExp(pattern, flags) — 문자열/정규식 → 정규식 객체
            Native::RegExpCtor => {
                let (src, flags) = match args.first() {
                    Some(v) if regex_src_flags(v).is_some() => {
                        let (s, f) = regex_src_flags(v).unwrap();
                        let f = args.get(1).map(to_display).unwrap_or(f);
                        (s, f)
                    }
                    // undefined 패턴은 빈 문자열로 (§22.2.3.1)
                    Some(Value::Undefined) | None => {
                        (String::new(), args.get(1).map(to_display).unwrap_or_default())
                    }
                    Some(v) => (to_display(v), args.get(1).map(to_display).unwrap_or_default()),
                };
                // 표준 §22.2.3.1 RegExpInitialize: 플래그와 패턴을 생성 시점에 검증하고
                // 잘못됐으면 SyntaxError. 예전엔 검증 없이 객체만 만들어, new RegExp("(")
                // 같은 잘못된 패턴이 조용히 통과했다.
                if let Some(bad) = invalid_regex_flags(&flags) {
                    return Err(self.throw_error(
                        "SyntaxError",
                        format!("Invalid regular expression flags: {}", bad),
                    ));
                }
                if let Err(e) = crate::js::regex::Regex::compile_pattern(&src, &flags) {
                    return Err(self.throw_error(
                        "SyntaxError",
                        format!("Invalid regular expression: /{}/: {}", src, e),
                    ));
                }
                Ok(make_regex_obj(&src, &flags))
            }
            // regex.test(str) → bool
            // RegExp.escape(S) (§22.2.5.2, ES2025): 문자열 S 를 정규식 안에서 리터럴로
            // 매칭되도록 이스케이프한다. 인자가 문자열이 아니면 TypeError.
            Native::RegExpEscape => {
                let s = match args.first() {
                    Some(Value::Str(s)) => s.clone(),
                    _ => {
                        return Err(self.throw_error(
                            "TypeError",
                            "RegExp.escape argument must be a string",
                        ))
                    }
                };
                Ok(Value::Str(regexp_escape(&s)))
            }
            // RegExp.prototype 의 접근자 getter (§22.2.6): flags/source/각 플래그를
            // this 정규식에서 계산한다. RegExp.prototype 자신(정규식 아님)엔 스펙 기본값.
            Native::RegexGet(kind) => {
                use crate::js::interp::natives::RegexAccessor as RA;
                let this = recv.unwrap_or(Value::Undefined);
                let sf = regex_src_flags(&this);
                Ok(match kind {
                    RA::Source => Value::Str(match &sf {
                        Some((s, _)) if !s.is_empty() => s.clone(),
                        // 빈 패턴/프로토타입 → "(?:)" (§22.2.6.13)
                        _ => "(?:)".to_string(),
                    }),
                    RA::Flags => Value::Str(match &sf {
                        // 표준 순서 d,g,i,m,s,u,v,y 로 정렬 (§22.2.6.4)
                        Some((_, f)) => "dgimsuvy".chars().filter(|c| f.contains(*c)).collect(),
                        None => String::new(),
                    }),
                    // 개별 플래그: this 가 정규식이면 flags 포함 여부, 프로토타입/비정규식이면 undefined
                    _ => match &sf {
                        Some((_, f)) => {
                            let ch = RA::table()
                                .iter()
                                .find(|(_, k, _)| *k == kind)
                                .and_then(|(_, _, c)| *c)
                                .unwrap();
                            Value::Bool(f.contains(ch))
                        }
                        None => Value::Undefined,
                    },
                })
            }
            // RegExp.prototype[Symbol.match/replace/split/search/matchAll]: this=정규식,
            // args=[문자열, ...]. 기존 String 측 구현으로 위임한다(수신자/인자 교환).
            Native::RegexSym(op) => {
                let re = recv.unwrap_or(Value::Undefined);
                let s = args.first().map(to_display).unwrap_or_default();
                // str.<op>(re, ...rest): String 수신자 + 정규식 인자 + 나머지(치환/limit)
                let mut fwd = vec![re];
                fwd.extend(args.into_iter().skip(1));
                self.call_native(Native::Str(op), Some(Value::Str(s)), fwd)
            }
            Native::RegexTest => {
                let (src, flags) = recv.as_ref().and_then(regex_src_flags).ok_or("test 대상이 정규식 아님")?;
                let text = args.first().map(to_display).unwrap_or_default();
                let re = crate::js::regex::Regex::compile_pattern(&src, &flags)
                    .map_err(|e| format!("SyntaxError: Invalid regular expression: /{}/: {}", src, e))?;
                let chars: Vec<char> = text.chars().collect();
                Ok(Value::Bool(re.find(&chars, 0).is_some()))
            }
            // regex.exec(str) → [full, g1, ...] with .index, or null. global 이면 lastIndex 갱신.
            Native::RegexExec => {
                let recv_obj = recv.clone();
                let (src, flags) = recv.as_ref().and_then(regex_src_flags).ok_or("exec 대상이 정규식 아님")?;
                let text = args.first().map(to_display).unwrap_or_default();
                let re = crate::js::regex::Regex::compile_pattern(&src, &flags)
                    .map_err(|e| format!("SyntaxError: Invalid regular expression: /{}/: {}", src, e))?;
                let chars: Vec<char> = text.chars().collect();
                let from = if re.global {
                    match &recv_obj {
                        Some(Value::Obj(m)) => match m.borrow().get("lastIndex") {
                            Some(Value::Num(n)) => *n as usize,
                            _ => 0,
                        },
                        _ => 0,
                    }
                } else {
                    0
                };
                match re.find(&chars, from.min(chars.len())) {
                    Some(mt) => {
                        if re.global {
                            if let Some(Value::Obj(m)) = &recv_obj {
                                m.borrow_mut()
                                    .insert("lastIndex".to_string(), Value::Num(mt.end as f64));
                            }
                        }
                        Ok(self.regex_match_array(&chars, &mt, &re.group_names))
                    }
                    None => {
                        if re.global {
                            if let Some(Value::Obj(m)) = &recv_obj {
                                m.borrow_mut().insert("lastIndex".to_string(), Value::Num(0.0));
                            }
                        }
                        Ok(Value::Null)
                    }
                }
            }
            // document.body/head/documentElement (라이브 접근자)
            Native::DocQuery(tag) => {
                let dom = self.dom_arena()?;
                let root = dom.root;
                Ok(find_tag(dom, root, tag).map(Value::Dom).unwrap_or(Value::Null))
            }
            // parent.insertBefore(newNode, referenceNode)
            Native::InsertBefore => match (recv, args.first()) {
                (Some(Value::Dom(parent)), Some(Value::Dom(child))) => {
                    let child = *child;
                    let reference = match args.get(1) {
                        Some(Value::Dom(r)) => Some(*r),
                        _ => None,
                    };
                    let dom = self.dom_arena()?;
                    dom.insert_before(parent, child, reference);
                    Ok(Value::Dom(child))
                }
                _ => Err("insertBefore 는 요소 인자가 필요".to_string()),
            },
            Native::Matches => {
                let sel = args.first().map(to_display).unwrap_or_default();
                match recv {
                    Some(Value::Dom(id)) => {
                        let dom = self.dom_arena()?;
                        let ok = crate::css::parse_selector_list(&sel)
                            .map(|sels| crate::style::element_matches(dom, id, &sels))
                            .unwrap_or(false);
                        Ok(Value::Bool(ok))
                    }
                    _ => Ok(Value::Bool(false)),
                }
            }
            Native::Closest => {
                let sel = args.first().map(to_display).unwrap_or_default();
                let start = match recv {
                    Some(Value::Dom(id)) => id,
                    _ => return Ok(Value::Null),
                };
                let dom = self.dom_arena()?;
                let Some(sels) = crate::css::parse_selector_list(&sel) else {
                    return Ok(Value::Null);
                };
                // 자신부터 조상까지 올라가며 첫 매칭 반환
                let mut chain = vec![start];
                chain.extend(dom.ancestors(start));
                for id in chain {
                    if matches!(dom.get(id).node_type, crate::dom::NodeType::Element(_))
                        && crate::style::element_matches(dom, id, &sels)
                    {
                        return Ok(Value::Dom(id));
                    }
                }
                Ok(Value::Null)
            }
            Native::DomContains => {
                let other = match args.first() {
                    Some(Value::Dom(o)) => *o,
                    _ => return Ok(Value::Bool(false)),
                };
                match recv {
                    Some(Value::Dom(id)) => {
                        let dom = self.dom_arena()?;
                        // other == id 이거나 id 의 자손이면 true
                        let contained = other == id || dom.ancestors(other).contains(&id);
                        Ok(Value::Bool(contained))
                    }
                    _ => Ok(Value::Bool(false)),
                }
            }
            Native::CanvasGetContext => {
                // canvas.getContext('2d') → 상태 + 메서드를 담은 컨텍스트 객체
                let canvas_id = match recv {
                    Some(Value::Dom(id)) => id,
                    _ => return Ok(Value::Null),
                };
                self.canvas_cmds.entry(canvas_id).or_default();
                let mut m = ObjMap::new();
                m.insert("\u{0}canvas".to_string(), Value::Num(canvas_id as f64));
                m.insert("fillStyle".to_string(), Value::Str("#000000".to_string()));
                m.insert("strokeStyle".to_string(), Value::Str("#000000".to_string()));
                m.insert("lineWidth".to_string(), Value::Num(1.0));
                m.insert("font".to_string(), Value::Str("10px sans-serif".to_string()));
                m.insert("globalAlpha".to_string(), Value::Num(1.0));
                m.insert("textAlign".to_string(), Value::Str("start".to_string()));
                m.insert("textBaseline".to_string(), Value::Str("alphabetic".to_string()));
                m.insert("lineCap".to_string(), Value::Str("butt".to_string()));
                m.insert("lineJoin".to_string(), Value::Str("miter".to_string()));
                m.insert(
                    "globalCompositeOperation".to_string(),
                    Value::Str("source-over".to_string()),
                );
                m.insert("shadowBlur".to_string(), Value::Num(0.0));
                m.insert("shadowColor".to_string(), Value::Str("rgba(0,0,0,0)".to_string()));
                m.insert("shadowOffsetX".to_string(), Value::Num(0.0));
                m.insert("shadowOffsetY".to_string(), Value::Num(0.0));
                m.insert("miterLimit".to_string(), Value::Num(10.0));
                m.insert("canvas".to_string(), Value::Dom(canvas_id));
                // CTM 초기값(단위행렬). 없으면 save() 가 undefined 를 저장하고
                // restore() 가 변환을 되돌리지 못한다 (변환이 영원히 남는다).
                m.insert(
                    "\u{0}ctm".to_string(),
                    Value::Arr(ArrayObj::new(vec![
                        Value::Num(1.0),
                        Value::Num(0.0),
                        Value::Num(0.0),
                        Value::Num(1.0),
                        Value::Num(0.0),
                        Value::Num(0.0),
                    ])),
                );
                m.insert("\u{0}path".to_string(), Value::Arr(ArrayObj::new(Vec::new())));
                use CanvasMethod::*;
                for (name, meth) in [
                    ("fillRect", FillRect),
                    ("clearRect", ClearRect),
                    ("strokeRect", StrokeRect),
                    ("beginPath", BeginPath),
                    ("moveTo", MoveTo),
                    ("lineTo", LineTo),
                    ("arc", Arc),
                    ("rect", Rect),
                    ("closePath", ClosePath),
                    ("fill", Fill),
                    ("stroke", Stroke),
                    ("fillText", FillText),
                    ("strokeText", FillText),
                    ("save", Save),
                    ("restore", Restore),
                    ("scale", Scale),
                    ("translate", Translate),
                    ("rotate", Rotate),
                    ("transform", Transform),
                    ("setTransform", SetTransform),
                    ("resetTransform", ResetTransform),
                    ("measureText", MeasureText),
                    ("drawImage", DrawImage),
                    ("ellipse", Ellipse),
                    ("roundRect", RoundRect),
                    ("setLineDash", Noop), // 점선은 시각 세부 (그림은 나온다)
                    ("clip", Clip),
                    ("createLinearGradient", CreateLinearGradient),
                    ("createRadialGradient", CreateRadialGradient),
                    ("createPattern", CreatePattern),
                    ("bezierCurveTo", BezierCurveTo),
                    ("quadraticCurveTo", QuadraticCurveTo),
                    ("putImageData", PutImageData),
                    ("getImageData", GetImageData),
                    ("createImageData", CreateImageData),
                ] {
                    m.insert(name.to_string(), Value::Native(Native::Canvas(meth)));
                }
                Ok(Value::Obj(Rc::new(RefCell::new(m))))
            }
            Native::Canvas(method) => self.canvas_method(method, recv, args),
            Native::CloneNode => {
                let deep = args.first().map(to_bool).unwrap_or(false);
                match recv {
                    Some(Value::Dom(id)) => {
                        let dom = self.dom_arena()?;
                        Ok(Value::Dom(dom.clone_node(id, deep)))
                    }
                    _ => Err("cloneNode 는 요소 메서드".to_string()),
                }
            }
            Native::DispatchEvent => {
                let node = match recv {
                    Some(Value::Dom(id)) => id,
                    // 객체 EventTarget(XHR 등): 그 객체에 붙은 리스너를 부른다
                    Some(Value::Obj(o)) => {
                        let evt = args.first().cloned().unwrap_or(Value::Undefined);
                        let ty = match &evt {
                            Value::Obj(e) => {
                                e.borrow().get("type").map(to_display).unwrap_or_default()
                            }
                            other => to_display(other),
                        };
                        let listeners: Vec<Value> = match o.borrow().get(&obj_listener_key(&ty)) {
                            Some(Value::Arr(a)) => a.borrow().clone(),
                            _ => Vec::new(),
                        };
                        for l in listeners {
                            if let Err(e) =
                                self.call_value(l, Some(Value::Obj(o.clone())), vec![evt.clone()])
                            {
                                println!("[js error] {}", e);
                            }
                        }
                        return Ok(Value::Bool(true));
                    }
                    _ => return Ok(Value::Bool(false)),
                };
                let evt = args.first().cloned().unwrap_or(Value::Undefined);
                let etype = match &evt {
                    Value::Obj(o) => o.borrow().get("type").map(to_display).unwrap_or_default(),
                    other => to_display(other),
                };
                // Obj 이벤트면 그대로 사용, 아니면 표준 이벤트 객체 생성
                let evt = if matches!(evt, Value::Obj(_)) {
                    evt
                } else {
                    self.make_event(&etype, node)
                };
                self.dispatch_event_value(node, &etype, evt.clone());
                // !defaultPrevented 근사
                let prevented = matches!(&evt,
                    Value::Obj(o) if matches!(o.borrow().get("defaultPrevented"), Some(Value::Bool(true))));
                Ok(Value::Bool(!prevented))
            }
            Native::EventCtor(iface) => {
                // new Event(type, opts) / new MouseEvent(type, {...}) → 이벤트 객체.
                // 인터페이스별 prototype 을 붙인다 — 그래야 instanceof 와
                // Object.getPrototypeOf 가 표준대로 답한다.
                let etype = args.first().map(to_display).unwrap_or_default();
                let mut m = ObjMap::new();
                if let Some(p) = self.event_proto(iface) {
                    m.insert("__proto__".to_string(), p);
                }
                m.insert("type".to_string(), Value::Str(etype));
                // 스크립트가 만든 이벤트는 신뢰되지 않는다 (표준)
                m.insert("isTrusted".to_string(), Value::Bool(false));
                // init 딕셔너리의 **모든 멤버**가 이벤트의 프로퍼티가 된다 (DOM 표준 §2.2).
                // 예전엔 detail/bubbles 만 베껴서 KeyboardEvent 의 key, MouseEvent 의
                // clientX/ctrlKey 같은 것이 통째로 사라졌다 — 키 핸들러가 조용히 안 먹는다.
                let mut bubbles = false;
                let mut cancelable = false;
                if let Some(Value::Obj(o)) = args.get(1) {
                    let entries: Vec<(String, Value)> =
                        o.borrow().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                    for (k, v) in entries {
                        if k == "bubbles" {
                            bubbles = to_bool(&v);
                            continue;
                        }
                        if k == "cancelable" {
                            cancelable = to_bool(&v);
                            continue;
                        }
                        m.insert(k, v);
                    }
                }
                m.insert("bubbles".to_string(), Value::Bool(bubbles));
                m.insert("cancelable".to_string(), Value::Bool(cancelable));
                m.insert("defaultPrevented".to_string(), Value::Bool(false));
                m.insert("preventDefault".to_string(), Value::Native(Native::EventPreventDefault));
                m.insert("stopPropagation".to_string(), Value::Native(Native::EventStopProp));
                m.insert(
                    "stopImmediatePropagation".to_string(),
                    Value::Native(Native::EventStopProp),
                );
                Ok(Value::Obj(Rc::new(RefCell::new(m))))
            }
            Native::GetBoundingClientRect => {
                self.ensure_layout();
                let (x, y, w, h) = match recv {
                    Some(Value::Dom(id)) => {
                        self.layout_rects.get(&id).copied().unwrap_or((0.0, 0.0, 0.0, 0.0))
                    }
                    _ => (0.0, 0.0, 0.0, 0.0),
                };
                let mut m = ObjMap::new();
                m.insert("x".to_string(), Value::Num(x as f64));
                m.insert("y".to_string(), Value::Num(y as f64));
                m.insert("left".to_string(), Value::Num(x as f64));
                m.insert("top".to_string(), Value::Num(y as f64));
                m.insert("right".to_string(), Value::Num((x + w) as f64));
                m.insert("bottom".to_string(), Value::Num((y + h) as f64));
                m.insert("width".to_string(), Value::Num(w as f64));
                m.insert("height".to_string(), Value::Num(h as f64));
                Ok(Value::Obj(Rc::new(RefCell::new(m))))
            }
            Native::AppendChild => match (recv, args.first()) {
                (Some(Value::Dom(parent)), Some(Value::Dom(child))) => {
                    let child = *child;
                    let dom = self.dom_arena()?;
                    // DocumentFragment 는 자신이 아니라 자식들을 옮긴다
                    if is_fragment(dom, child) {
                        for c in dom.get(child).children.clone() {
                            dom.append_child(parent, c);
                        }
                    } else {
                        dom.append_child(parent, child);
                    }
                    Ok(Value::Dom(child))
                }
                _ => Err("appendChild 는 요소 인자가 필요".to_string()),
            },
            // ParentNode/ChildNode 삽입 (append/prepend/before/after/replaceWith).
            // 가변 인자, 문자열은 텍스트 노드로. append/prepend 는 recv 가 부모,
            // before/after/replaceWith 는 recv 의 부모에 삽입.
            Native::NodeAppend => {
                if let Some(Value::Dom(target)) = recv {
                    let ids = self.nodes_from_args(&args)?;
                    let dom = self.dom_arena()?;
                    for id in ids {
                        dom.insert_before(target, id, None);
                    }
                }
                Ok(Value::Undefined)
            }
            Native::NodePrepend => {
                if let Some(Value::Dom(target)) = recv {
                    let ids = self.nodes_from_args(&args)?;
                    let dom = self.dom_arena()?;
                    let first = dom.get(target).children.first().copied();
                    for id in ids {
                        dom.insert_before(target, id, first);
                    }
                }
                Ok(Value::Undefined)
            }
            Native::NodeBefore => {
                if let Some(Value::Dom(target)) = recv {
                    let ids = self.nodes_from_args(&args)?;
                    let dom = self.dom_arena()?;
                    if let Some(parent) = dom.get(target).parent {
                        for id in ids {
                            dom.insert_before(parent, id, Some(target));
                        }
                    }
                }
                Ok(Value::Undefined)
            }
            Native::NodeAfter => {
                if let Some(Value::Dom(target)) = recv {
                    let ids = self.nodes_from_args(&args)?;
                    let dom = self.dom_arena()?;
                    if let Some(parent) = dom.get(target).parent {
                        let kids = dom.get(parent).children.clone();
                        let next = kids
                            .iter()
                            .position(|&c| c == target)
                            .and_then(|i| kids.get(i + 1).copied());
                        for id in ids {
                            dom.insert_before(parent, id, next);
                        }
                    }
                }
                Ok(Value::Undefined)
            }
            Native::NodeReplaceWith => {
                if let Some(Value::Dom(target)) = recv {
                    let ids = self.nodes_from_args(&args)?;
                    let dom = self.dom_arena()?;
                    if let Some(parent) = dom.get(target).parent {
                        for id in ids {
                            dom.insert_before(parent, id, Some(target));
                        }
                        dom.detach(target);
                    }
                }
                Ok(Value::Undefined)
            }
            // el.insertAdjacentHTML(position, html) / insertAdjacentElement(position, el)
            // 예전엔 메서드가 없어 TypeError 로 스크립트 전체가 죽었다.
            Native::InsertAdjacentHTML | Native::InsertAdjacentElement => {
                let Some(Value::Dom(id)) = recv else { return Ok(Value::Undefined) };
                let pos = args.first().map(to_display).unwrap_or_default().to_ascii_lowercase();
                // 삽입할 노드들: HTML 이면 조각 파싱, Element 면 그 노드
                let nodes: Vec<crate::dom::NodeId> =
                    if matches!(n, Native::InsertAdjacentElement) {
                        match args.get(1) {
                            Some(Value::Dom(e)) => vec![*e],
                            _ => Vec::new(),
                        }
                    } else {
                        let html = args.get(1).map(to_display).unwrap_or_default();
                        let dom = self.dom_arena()?;
                        crate::html::parse_fragment(html)
                            .into_iter()
                            .map(|t| dom.insert_tree(t, None))
                            .collect()
                    };
                let dom = self.dom_arena()?;
                let parent = dom.get(id).parent;
                for node in nodes {
                    match pos.as_str() {
                        "beforebegin" => {
                            if let Some(p) = parent {
                                dom.insert_before(p, node, Some(id));
                            }
                        }
                        "afterbegin" => {
                            let first = dom.get(id).children.first().copied();
                            dom.insert_before(id, node, first);
                        }
                        "beforeend" => dom.append_child(id, node),
                        "afterend" => {
                            if let Some(p) = parent {
                                // id 의 다음 형제 앞에 (없으면 끝)
                                let next = dom
                                    .get(p)
                                    .children
                                    .iter()
                                    .position(|&c| c == id)
                                    .and_then(|i| dom.get(p).children.get(i + 1).copied());
                                dom.insert_before(p, node, next);
                            }
                        }
                        _ => {}
                    }
                }
                Ok(Value::Undefined)
            }
            Native::RemoveElement => match recv {
                Some(Value::Dom(id)) => {
                    let dom = self.dom_arena()?;
                    dom.detach(id);
                    Ok(Value::Undefined)
                }
                _ => Err("remove 는 요소 메서드".to_string()),
            },
            Native::RemoveAttribute => {
                if let Some(Value::Dom(id)) = recv {
                    let raw = args.first().map(to_display).unwrap_or_default();
                    // removeAttribute 는 이름을 검증하지 않는다 (표준) — 소문자화만 한다
                    let name = {
                        let dom = self.dom_arena()?;
                        let html_ns = matches!(&dom.get(id).node_type,
                            crate::dom::NodeType::Element(e) if e.namespace.is_none());
                        if html_ns { raw.to_ascii_lowercase() } else { raw }
                    };
                    let dom = self.dom_arena()?;
                    dom.remove_attr(id, &name);
                }
                Ok(Value::Undefined)
            }
            Native::HasAttribute => {
                let has = if let Some(Value::Dom(id)) = recv {
                    let raw = args.first().map(to_display).unwrap_or_default();
                    let name = {
                        let dom = self.dom_arena()?;
                        let html_ns = matches!(&dom.get(id).node_type,
                            crate::dom::NodeType::Element(e) if e.namespace.is_none());
                        if html_ns { raw.to_ascii_lowercase() } else { raw }
                    };
                    let dom = self.dom_arena()?;
                    matches!(&dom.get(id).node_type,
                        crate::dom::NodeType::Element(e) if e.attributes.contains_key(&name))
                } else {
                    false
                };
                Ok(Value::Bool(has))
            }
            Native::RemoveChild => {
                // parent.removeChild(child) → child 를 트리에서 분리, child 반환
                let child = args.into_iter().next().unwrap_or(Value::Undefined);
                if let Value::Dom(cid) = child {
                    let dom = self.dom_arena()?;
                    dom.detach(cid);
                }
                Ok(child)
            }
            Native::SetAttribute => match recv {
                Some(Value::Dom(id)) => {
                    let raw = args.first().map(to_display).unwrap_or_default();
                    let name = self.attr_name(id, &raw)?;
                    let value = args.get(1).map(to_display).unwrap_or_default();
                    let dom = self.dom_arena()?;
                    dom.set_attr(id, &name, value);
                    Ok(Value::Undefined)
                }
                _ => Err(self.throw_error("TypeError", "setAttribute 는 요소 메서드")),
            },
            Native::QuerySelector | Native::QuerySelectorAll => {
                let all = n == Native::QuerySelectorAll;
                let sel = args.first().map(to_display).unwrap_or_default();
                // 요소 수신자면 그 서브트리(자신 제외), document 면 문서 전체
                let scope = match recv {
                    Some(Value::Dom(id)) => Some(id),
                    _ => None,
                };
                self.dom_query(scope, &sel, all)
            }
            Native::Math(op) => {
                let a = args.first().map(to_num).unwrap_or(f64::NAN);
                Ok(Value::Num(match op {
                    MathOp::Floor => a.floor(),
                    MathOp::Ceil => a.ceil(),
                    // JS Math.round: floor(x+0.5) — 반올림이 +∞ 방향 (round(-2.5)=-2, not -3)
                    MathOp::Round => {
                        if a.is_nan() || a.is_infinite() {
                            a
                        } else {
                            (a + 0.5).floor()
                        }
                    }
                    MathOp::Abs => a.abs(),
                    MathOp::Sqrt => a.sqrt(),
                    MathOp::Pow => a.powf(args.get(1).map(to_num).unwrap_or(f64::NAN)),
                    // JS min/max: 인자에 NaN 있으면 NaN (Rust min/max 는 NaN 무시하므로 직접)
                    MathOp::Min => {
                        let vs = args.iter().map(to_num);
                        vs.fold(f64::INFINITY, |acc, x| {
                            if acc.is_nan() || x.is_nan() { f64::NAN } else { acc.min(x) }
                        })
                    }
                    MathOp::Max => {
                        let vs = args.iter().map(to_num);
                        vs.fold(f64::NEG_INFINITY, |acc, x| {
                            if acc.is_nan() || x.is_nan() { f64::NAN } else { acc.max(x) }
                        })
                    }
                    MathOp::Trunc => a.trunc(),
                    MathOp::Sign => {
                        if a.is_nan() {
                            f64::NAN
                        } else if a > 0.0 {
                            1.0
                        } else if a < 0.0 {
                            -1.0
                        } else {
                            a // ±0 유지
                        }
                    }
                    MathOp::Cbrt => a.cbrt(),
                    MathOp::Log => a.ln(),
                    MathOp::Log2 => a.log2(),
                    MathOp::Log10 => a.log10(),
                    MathOp::Exp => a.exp(),
                    MathOp::Sin => a.sin(),
                    MathOp::Cos => a.cos(),
                    MathOp::Tan => a.tan(),
                    MathOp::Asin => a.asin(),
                    MathOp::Acos => a.acos(),
                    MathOp::Atan => a.atan(),
                    MathOp::Atan2 => a.atan2(args.get(1).map(to_num).unwrap_or(f64::NAN)),
                    MathOp::Hypot => args.iter().map(to_num).fold(0.0f64, |acc, x| acc.hypot(x)),
                    MathOp::Random => {
                        // xorshift64*
                        self.rng ^= self.rng << 13;
                        self.rng ^= self.rng >> 7;
                        self.rng ^= self.rng << 17;
                        (self.rng >> 11) as f64 / (1u64 << 53) as f64
                    }
                }))
            }
            Native::Str(op) => {
                // String.prototype 메서드는 generic 하다 (§22.1.3): this 를
                // ToString(RequireObjectCoercible(this)) 로 강제한다. null/undefined 는
                // TypeError. 예전엔 진짜 문자열이 아니면 일반 Error 를 던져서
                // "".trim.call(42) 이 죽고, null 에 대해 TypeError 가 아니라 Error 였다.
                let s = match recv {
                    None | Some(Value::Undefined) | Some(Value::Null) => {
                        return Err(self.throw_error(
                            "TypeError",
                            "String.prototype method called on null or undefined",
                        ));
                    }
                    Some(Value::Str(s)) => s,
                    Some(Value::Symbol(_)) => {
                        return Err(self.throw_error(
                            "TypeError",
                            "Cannot convert a Symbol value to a string",
                        ));
                    }
                    Some(other) => {
                        // ToString(this): poisoned toString/valueOf 는 그대로 전파해야 한다
                        // (§22.1.3 의 this-value-tostring-throws 검사). Symbol 은 TypeError.
                        let prim = self.to_primitive_or_throw(other, true)?;
                        if let Value::Symbol(_) = prim {
                            return Err(self.throw_error(
                                "TypeError",
                                "Cannot convert a Symbol value to a string",
                            ));
                        }
                        to_display(&prim)
                    }
                };
                let chars: Vec<char> = s.chars().collect();
                // JS 문자열은 UTF-16 코드 유닛 열 — 길이/인덱스는 코드 유닛 기준.
                let units: Vec<u16> = s.encode_utf16().collect();
                let arg_str = |i: usize| args.get(i).map(to_display).unwrap_or_default();
                Ok(match op {
                    StrOp::Upper => Value::Str(s.to_uppercase()),
                    StrOp::Lower => Value::Str(s.to_lowercase()),
                    StrOp::Trim => Value::Str(s.trim().to_string()),
                    StrOp::CharAt => {
                        // UTF-16 코드 유닛 하나(범위 밖은 ""). 짝 없는 서로게이트는 U+FFFD.
                        let i = args.first().map(to_num).unwrap_or(0.0);
                        let out = if i >= 0.0 && (i as usize) < units.len() {
                            String::from_utf16_lossy(&units[i as usize..i as usize + 1])
                        } else {
                            String::new()
                        };
                        Value::Str(out)
                    }
                    StrOp::IndexOf => {
                        // UTF-16 코드 유닛 인덱스. fromIndex 2번째 인자.
                        let ndl: Vec<u16> = arg_str(0).encode_utf16().collect();
                        let from = args
                            .get(1)
                            .map(to_num)
                            .filter(|n| !n.is_nan())
                            .map(|n| n.max(0.0) as usize)
                            .unwrap_or(0);
                        Value::Num(utf16_index_of(&units, &ndl, from).map(|i| i as f64).unwrap_or(-1.0))
                    }
                    StrOp::LastIndexOf => {
                        let ndl: Vec<u16> = arg_str(0).encode_utf16().collect();
                        Value::Num(utf16_last_index_of(&units, &ndl).map(|i| i as f64).unwrap_or(-1.0))
                    }
                    // includes/startsWith/endsWith 는 정규식 인자를 거부한다 (§22.1.3.7/.8/.23:
                    // IsRegExp(searchString) 이면 TypeError). 예전엔 정규식을 문자열화해 통과시켰다.
                    StrOp::Includes => {
                        if self.is_regexp(args.first().unwrap_or(&Value::Undefined)) {
                            return Err(self.throw_error(
                                "TypeError",
                                "First argument to String.prototype.includes must not be a regular expression",
                            ));
                        }
                        Value::Bool(s.contains(&arg_str(0)))
                    }
                    StrOp::StartsWith => {
                        if self.is_regexp(args.first().unwrap_or(&Value::Undefined)) {
                            return Err(self.throw_error(
                                "TypeError",
                                "First argument to String.prototype.startsWith must not be a regular expression",
                            ));
                        }
                        Value::Bool(s.starts_with(&arg_str(0)))
                    }
                    StrOp::EndsWith => {
                        if self.is_regexp(args.first().unwrap_or(&Value::Undefined)) {
                            return Err(self.throw_error(
                                "TypeError",
                                "First argument to String.prototype.endsWith must not be a regular expression",
                            ));
                        }
                        Value::Bool(s.ends_with(&arg_str(0)))
                    }
                    StrOp::Replace => {
                        let pat = args.first().cloned().unwrap_or(Value::Undefined);
                        let repl = args.get(1).cloned().unwrap_or(Value::Undefined);
                        Value::Str(self.str_replace(&s, &pat, &repl, false)?)
                    }
                    StrOp::ReplaceAll => {
                        let pat = args.first().cloned().unwrap_or(Value::Undefined);
                        let repl = args.get(1).cloned().unwrap_or(Value::Undefined);
                        Value::Str(self.str_replace(&s, &pat, &repl, true)?)
                    }
                    StrOp::Search => {
                        let (src, flags) = args
                            .first()
                            .and_then(regex_src_flags)
                            .unwrap_or_else(|| (regex_escape(&arg_str(0)), String::new()));
                        let re = crate::js::regex::Regex::compile_pattern(&src, &flags)
                            .map_err(|e| format!("정규식: {}", e))?;
                        match re.find(&chars, 0) {
                            // 정규식 엔진은 코드포인트 인덱스 → UTF-16 유닛 오프셋으로 변환
                            Some(mt) => {
                                let u16_start: usize =
                                    chars[..mt.start].iter().map(|c| c.len_utf16()).sum();
                                Value::Num(u16_start as f64)
                            }
                            None => Value::Num(-1.0),
                        }
                    }
                    StrOp::Match => {
                        let (src, flags) = args
                            .first()
                            .and_then(regex_src_flags)
                            .unwrap_or_else(|| (regex_escape(&arg_str(0)), String::new()));
                        let re = crate::js::regex::Regex::compile_pattern(&src, &flags)
                            .map_err(|e| format!("정규식: {}", e))?;
                        if re.global {
                            // 전역: 매치 문자열들의 배열
                            let mut out = Vec::new();
                            let mut pos = 0;
                            while let Some(mt) = re.find(&chars, pos) {
                                out.push(Value::Str(chars[mt.start..mt.end].iter().collect()));
                                pos = if mt.end > mt.start { mt.end } else { mt.end + 1 };
                                if pos > chars.len() {
                                    break;
                                }
                            }
                            if out.is_empty() {
                                Value::Null
                            } else {
                                Value::Arr(ArrayObj::new(out))
                            }
                        } else {
                            match re.find(&chars, 0) {
                                Some(mt) => self.regex_match_array(&chars, &mt, &re.group_names),
                                None => Value::Null,
                            }
                        }
                    }
                    StrOp::MatchAll => {
                        let (src, flags) = args
                            .first()
                            .and_then(regex_src_flags)
                            .unwrap_or_else(|| (regex_escape(&arg_str(0)), String::new()));
                        let re = crate::js::regex::Regex::compile_pattern(&src, &flags)
                            .map_err(|e| format!("정규식: {}", e))?;
                        let mut out = Vec::new();
                        let mut pos = 0;
                        while let Some(mt) = re.find(&chars, pos) {
                            out.push(self.regex_match_array(&chars, &mt, &re.group_names));
                            pos = if mt.end > mt.start { mt.end } else { mt.end + 1 };
                            if pos > chars.len() {
                                break;
                            }
                        }
                        Value::Arr(ArrayObj::new(out))
                    }
                    StrOp::Slice => {
                        // UTF-16 코드 유닛 기준 슬라이스(음수 인덱스는 끝에서).
                        let len = units.len() as isize;
                        let clampi = |v: f64| -> usize {
                            let i = v as isize;
                            (if i < 0 { len + i } else { i }).clamp(0, len) as usize
                        };
                        let start = clampi(args.first().map(to_num).unwrap_or(0.0));
                        let end = clampi(args.get(1).map(to_num).unwrap_or(len as f64));
                        Value::Str(String::from_utf16_lossy(&units[start..end.max(start)]))
                    }
                    StrOp::Split => {
                        // 정규식 구분자 지원
                        if let Some((src, flags)) = args.first().and_then(regex_src_flags) {
                            let re = crate::js::regex::Regex::compile_pattern(&src, &flags)
                                .map_err(|e| format!("정규식: {}", e))?;
                            let mut parts = Vec::new();
                            let mut last = 0;
                            let mut pos = 0;
                            while pos <= chars.len() {
                                match re.find(&chars, pos) {
                                    Some(mt) if mt.end > mt.start => {
                                        parts.push(Value::Str(chars[last..mt.start].iter().collect()));
                                        last = mt.end;
                                        pos = mt.end;
                                    }
                                    _ => break,
                                }
                            }
                            parts.push(Value::Str(chars[last..].iter().collect()));
                            Value::Arr(ArrayObj::new(parts))
                        } else {
                            let sep = arg_str(0);
                            let mut parts: Vec<Value> = if args.is_empty() {
                                vec![Value::Str(s.clone())]
                            } else if sep.is_empty() {
                                chars.iter().map(|c| Value::Str(c.to_string())).collect()
                            } else {
                                s.split(&sep).map(|p| Value::Str(p.to_string())).collect()
                            };
                            // limit (2번째 인자): 결과를 그 개수로 자름
                            if let Some(lim) = args.get(1).map(to_num).filter(|n| !n.is_nan() && *n >= 0.0) {
                                parts.truncate(lim as usize);
                            }
                            Value::Arr(ArrayObj::new(parts))
                        }
                    }
                    StrOp::TrimStart => Value::Str(s.trim_start().to_string()),
                    StrOp::TrimEnd => Value::Str(s.trim_end().to_string()),
                    StrOp::Repeat => {
                        let n = args.first().map(to_num).unwrap_or(0.0).max(0.0) as usize;
                        Value::Str(s.repeat(n))
                    }
                    StrOp::PadStart | StrOp::PadEnd => {
                        let target = args.first().map(to_num).unwrap_or(0.0) as usize;
                        let pad = if args.len() > 1 { arg_str(1) } else { " ".to_string() };
                        let cur = chars.len();
                        if cur >= target || pad.is_empty() {
                            Value::Str(s.clone())
                        } else {
                            let need = target - cur;
                            let padchars: Vec<char> = pad.chars().collect();
                            let fill: String =
                                (0..need).map(|i| padchars[i % padchars.len()]).collect();
                            Value::Str(if matches!(op, StrOp::PadStart) {
                                format!("{}{}", fill, s)
                            } else {
                                format!("{}{}", s, fill)
                            })
                        }
                    }
                    StrOp::CharCodeAt => {
                        // i번째 UTF-16 코드 유닛(u16). 범위 밖 NaN.
                        let i = args.first().map(to_num).unwrap_or(0.0);
                        if i < 0.0 {
                            Value::Num(f64::NAN)
                        } else {
                            match units.get(i as usize) {
                                Some(u) => Value::Num(*u as f64),
                                None => Value::Num(f64::NAN),
                            }
                        }
                    }
                    StrOp::CodePointAt => {
                        // i번째 UTF-16 위치에서 시작하는 코드 포인트(서로게이트쌍 결합). 범위 밖 undefined.
                        let i = args.first().map(to_num).unwrap_or(0.0);
                        if i < 0.0 {
                            Value::Undefined
                        } else {
                            let idx = i as usize;
                            match units.get(idx).copied() {
                                None => Value::Undefined,
                                Some(hi) if (0xD800..=0xDBFF).contains(&hi) => {
                                    let cp = units
                                        .get(idx + 1)
                                        .copied()
                                        .filter(|lo| (0xDC00..=0xDFFF).contains(lo))
                                        .map(|lo| {
                                            0x10000 + ((hi as u32 - 0xD800) << 10) + (lo as u32 - 0xDC00)
                                        })
                                        .unwrap_or(hi as u32);
                                    Value::Num(cp as f64)
                                }
                                Some(u) => Value::Num(u as f64),
                            }
                        }
                    }
                    StrOp::Concat => {
                        let mut out = s.clone();
                        for a in &args {
                            out.push_str(&to_display(a));
                        }
                        Value::Str(out)
                    }
                    StrOp::LocaleCompare => {
                        // 로케일 콜레이션 근사(코드포인트 순서). -1/0/1.
                        let other = arg_str(0);
                        Value::Num(match s.as_str().cmp(other.as_str()) {
                            std::cmp::Ordering::Less => -1.0,
                            std::cmp::Ordering::Equal => 0.0,
                            std::cmp::Ordering::Greater => 1.0,
                        })
                    }
                    StrOp::At => {
                        // str.at(i): UTF-16 유닛, 음수는 끝에서. 범위 밖 undefined.
                        let len = units.len() as isize;
                        let i = args.first().map(to_num).unwrap_or(0.0) as isize;
                        let idx = if i < 0 { len + i } else { i };
                        if idx >= 0 && idx < len {
                            Value::Str(String::from_utf16_lossy(&units[idx as usize..idx as usize + 1]))
                        } else {
                            Value::Undefined
                        }
                    }
                })
            }
            Native::Arr(op) => {
                // 배열이면 그대로. array-like(length 보유 객체)면 임시 배열로 옮겨 실행하고
                // 결과를 되쓴다 → 표준의 generic 배열 메서드(jQuery 가 이걸 의존).
                let (a, write_back) = match &recv {
                    // 얼린 배열은 제자리 변형을 무시한다(표준: 조용히 실패).
                    Some(Value::Arr(a))
                        if is_mutating_arr_op(op)
                            && self.is_frozen_val(&Value::Arr(a.clone())) =>
                    {
                        return Ok(Value::Arr(a.clone()))
                    }
                    Some(Value::Arr(a)) => (a.clone(), None),
                    // 읽기 전용 연산은 array-like 대상을 변형하지 않는다(되쓰기 없음).
                    Some(Value::Obj(o)) if is_array_like(o) => (
                        ArrayObj::new(array_like_to_vec(o)?),
                        if is_mutating_arr_op(op) { Some(o.clone()) } else { None },
                    ),
                    // 배열 메서드는 generic 하다 (§23.1.3): this 를 ToObject 로 강제한다.
                    // null/undefined 는 TypeError (§7.1.18). 예전엔 일반 Error 를 던져서
                    // "TypeError 를 기대" 하는 코드가 조용히 어긋났다.
                    None | Some(Value::Undefined) | Some(Value::Null) => {
                        return Err(self.throw_error(
                            "TypeError",
                            "Array.prototype method called on null or undefined",
                        ));
                    }
                    // 문자열: 각 코드유닛을 원소로 (length 있는 array-like)
                    Some(Value::Str(s)) => {
                        let items: Vec<Value> =
                            s.chars().map(|c| Value::Str(c.to_string())).collect();
                        (ArrayObj::new(items), None)
                    }
                    // 그 외 원시값/객체(length 없음): ToObject 후 length 0 → 빈 배열로 근사
                    _ => (ArrayObj::new(Vec::new()), None),
                };
                let out = match op {
                    ArrOp::Join => {
                        let sep = args.first().map(to_display).unwrap_or(",".to_string());
                        Value::Str(
                            a.borrow().iter().map(to_display).collect::<Vec<_>>().join(&sep),
                        )
                    }
                    ArrOp::Pop => a.borrow_mut().pop().unwrap_or(Value::Undefined),
                    ArrOp::IndexOf => {
                        let needle = args.first().cloned().unwrap_or(Value::Undefined);
                        match a.borrow().iter().position(|v| strict_eq(v, &needle)) {
                            Some(i) => Value::Num(i as f64),
                            None => Value::Num(-1.0),
                        }
                    }
                    ArrOp::Slice => {
                        let items = a.borrow();
                        let len = items.len() as isize;
                        let clampi = |v: f64| -> usize {
                            let i = v as isize;
                            (if i < 0 { len + i } else { i }).clamp(0, len) as usize
                        };
                        let start = clampi(args.first().map(to_num).unwrap_or(0.0));
                        let end = clampi(args.get(1).map(to_num).unwrap_or(len as f64));
                        Value::Arr(ArrayObj::new(items[start..end.max(start)].to_vec()))
                    }
                    ArrOp::ForEach | ArrOp::Map | ArrOp::Filter | ArrOp::FlatMap => {
                        let f = args.first().cloned().ok_or("콜백이 필요")?;
                        // 표준: 콜백은 (값, 인덱스, **배열**) 로 부르고, 2번째 인자는 thisArg 다.
                        // 예전엔 (값, 인덱스) 만 넘겨서 a[i-1] 같은 관용 코드가 죽었다
                        // (IntersectionObserver 폴리필이 정확히 그 모양이다).
                        let this_arg = args.get(1).cloned();
                        let arr_val = Value::Arr(a.clone());
                        let snapshot: Vec<Value> = a.borrow().clone();
                        let mut out = Vec::new();
                        for (i, item) in snapshot.into_iter().enumerate() {
                            let r = self.call_value(
                                f.clone(),
                                this_arg.clone(),
                                vec![item.clone(), Value::Num(i as f64), arr_val.clone()],
                            )?;
                            match op {
                                ArrOp::Map => out.push(r),
                                // flatMap: 콜백 결과가 배열이면 한 단계 펼침
                                ArrOp::FlatMap => match r {
                                    Value::Arr(inner) => out.extend(inner.borrow().iter().cloned()),
                                    other => out.push(other),
                                },
                                ArrOp::Filter => {
                                    if to_bool(&r) {
                                        out.push(item);
                                    }
                                }
                                _ => {}
                            }
                        }
                        match op {
                            ArrOp::ForEach => Value::Undefined,
                            _ => Value::Arr(ArrayObj::new(out)),
                        }
                    }
                    ArrOp::Some | ArrOp::Every | ArrOp::Find | ArrOp::FindIndex => {
                        let f = args.first().cloned().ok_or("콜백이 필요")?;
                        let this_arg = args.get(1).cloned();
                        let arr_val = Value::Arr(a.clone());
                        let snapshot: Vec<Value> = a.borrow().clone();
                        let mut result = Value::Undefined;
                        let mut found = false;
                        for (i, item) in snapshot.into_iter().enumerate() {
                            let r = self.call_value(
                                f.clone(),
                                this_arg.clone(),
                                vec![item.clone(), Value::Num(i as f64), arr_val.clone()],
                            )?;
                            let truthy = to_bool(&r);
                            match op {
                                ArrOp::Some if truthy => {
                                    result = Value::Bool(true);
                                    found = true;
                                    break;
                                }
                                ArrOp::Every if !truthy => {
                                    result = Value::Bool(false);
                                    found = true;
                                    break;
                                }
                                ArrOp::Find if truthy => {
                                    result = item;
                                    found = true;
                                    break;
                                }
                                ArrOp::FindIndex if truthy => {
                                    result = Value::Num(i as f64);
                                    found = true;
                                    break;
                                }
                                _ => {}
                            }
                        }
                        if found {
                            result
                        } else {
                            match op {
                                ArrOp::Some => Value::Bool(false),
                                ArrOp::Every => Value::Bool(true),
                                ArrOp::FindIndex => Value::Num(-1.0),
                                _ => Value::Undefined,
                            }
                        }
                    }
                    ArrOp::Reduce => {
                        let f = args.first().cloned().ok_or("콜백이 필요")?;
                        let arr_val = Value::Arr(a.clone()); // 콜백 4번째 인자 (표준)
                        let snapshot: Vec<Value> = a.borrow().clone();
                        let mut iter = snapshot.into_iter().enumerate();
                        let mut acc = match args.get(1) {
                            Some(init) => init.clone(),
                            None => match iter.next() {
                                Some((_, v)) => v,
                                // 표준 §23.1.3.24: 초기값 없는 빈 배열 reduce 는 TypeError.
                                None => return Err(self.throw_error(
                                    "TypeError",
                                    "Reduce of empty array with no initial value",
                                )),
                            },
                        };
                        for (i, item) in iter {
                            acc = self.call_value(
                                f.clone(),
                                None,
                                vec![acc, item, Value::Num(i as f64), arr_val.clone()],
                            )?;
                        }
                        acc
                    }
                    ArrOp::ReduceRight => {
                        let f = args.first().cloned().ok_or("콜백이 필요")?;
                        let arr_val = Value::Arr(a.clone());
                        let snapshot: Vec<Value> = a.borrow().clone();
                        let mut idx = snapshot.len();
                        let mut acc = match args.get(1) {
                            Some(init) => init.clone(),
                            None => {
                                if idx == 0 {
                                    return Err(self.throw_error(
                                        "TypeError",
                                        "Reduce of empty array with no initial value",
                                    ));
                                }
                                idx -= 1;
                                snapshot[idx].clone()
                            }
                        };
                        while idx > 0 {
                            idx -= 1;
                            acc = self.call_value(
                                f.clone(),
                                None,
                                vec![
                                    acc,
                                    snapshot[idx].clone(),
                                    Value::Num(idx as f64),
                                    arr_val.clone(),
                                ],
                            )?;
                        }
                        acc
                    }
                    ArrOp::FindLast | ArrOp::FindLastIndex => {
                        let f = args.first().cloned().ok_or("콜백이 필요")?;
                        let snapshot: Vec<Value> = a.borrow().clone();
                        let mut result = if matches!(op, ArrOp::FindLastIndex) {
                            Value::Num(-1.0)
                        } else {
                            Value::Undefined
                        };
                        for i in (0..snapshot.len()).rev() {
                            let r = self.call_value(
                                f.clone(),
                                None,
                                vec![snapshot[i].clone(), Value::Num(i as f64)],
                            )?;
                            if to_bool(&r) {
                                result = if matches!(op, ArrOp::FindLastIndex) {
                                    Value::Num(i as f64)
                                } else {
                                    snapshot[i].clone()
                                };
                                break;
                            }
                        }
                        result
                    }
                    ArrOp::Concat => {
                        let mut out: Vec<Value> = a.borrow().clone();
                        for arg in &args {
                            match arg {
                                Value::Arr(b) => out.extend(b.borrow().iter().cloned()),
                                other => out.push(other.clone()),
                            }
                        }
                        Value::Arr(ArrayObj::new(out))
                    }
                    ArrOp::Includes => {
                        let needle = args.first().cloned().unwrap_or(Value::Undefined);
                        Value::Bool(a.borrow().iter().any(|v| strict_eq(v, &needle)))
                    }
                    ArrOp::Splice => {
                        // splice(start, deleteCount, ...items) → 제거분 배열 반환, 원본 변형
                        let mut arr = a.borrow_mut();
                        let len = arr.len() as isize;
                        let start = {
                            let s = args.first().map(to_num).unwrap_or(0.0) as isize;
                            (if s < 0 { len + s } else { s }).clamp(0, len) as usize
                        };
                        let del = match args.get(1) {
                            Some(v) => (to_num(v) as isize).clamp(0, len - start as isize) as usize,
                            None => arr.len() - start,
                        };
                        let removed: Vec<Value> = arr.splice(
                            start..start + del,
                            args.iter().skip(2).cloned(),
                        ).collect();
                        Value::Arr(ArrayObj::new(removed))
                    }
                    ArrOp::Shift => {
                        let mut arr = a.borrow_mut();
                        if arr.is_empty() {
                            Value::Undefined
                        } else {
                            arr.remove(0)
                        }
                    }
                    ArrOp::Unshift => {
                        let mut arr = a.borrow_mut();
                        for (i, v) in args.iter().cloned().enumerate() {
                            arr.insert(i, v);
                        }
                        Value::Num(arr.len() as f64)
                    }
                    ArrOp::Reverse => {
                        a.borrow_mut().reverse();
                        Value::Arr(a.clone())
                    }
                    // keys/values 는 **이터레이터**다 (배열이 아니다 — 표준).
                    // 배열을 주면 .next() 가 없어서 이터레이터 프로토콜을 직접 쓰는
                    // 코드(core-js/regenerator)가 "next 가 undefined" 로 죽는다.
                    ArrOp::Keys => {
                        let n = a.borrow().len();
                        self.make_iter_from_vec((0..n).map(|i| Value::Num(i as f64)).collect())
                    }
                    ArrOp::Values => {
                        let items = a.borrow().clone();
                        self.make_iter_from_vec(items)
                    }
                    ArrOp::Entries => {
                        let items: Vec<Value> = a
                            .borrow()
                            .iter()
                            .enumerate()
                            .map(|(i, v)| {
                                Value::Arr(ArrayObj::new(vec![Value::Num(i as f64), v.clone()]))
                            })
                            .collect();
                        self.make_iter_from_vec(items)
                    }
                    ArrOp::Sort => {
                        // 제자리 정렬 후 같은 배열 반환. 비교자 있으면 부호, 없으면 문자열 비교.
                        // 비교자가 Result 를 반환하므로 삽입정렬로 에러를 전파한다.
                        let cmp = args.first().cloned().filter(|v| !matches!(v, Value::Undefined));
                        let mut items: Vec<Value> = a.borrow().clone();
                        let n = items.len();
                        for i in 1..n {
                            let mut j = i;
                            while j > 0 {
                                let ord = match &cmp {
                                    Some(f) => {
                                        let r = self.call_value(
                                            f.clone(),
                                            None,
                                            vec![items[j - 1].clone(), items[j].clone()],
                                        )?;
                                        to_num(&r)
                                    }
                                    None => {
                                        let x = to_display(&items[j - 1]);
                                        let y = to_display(&items[j]);
                                        if x < y {
                                            -1.0
                                        } else if x > y {
                                            1.0
                                        } else {
                                            0.0
                                        }
                                    }
                                };
                                if ord > 0.0 {
                                    items.swap(j - 1, j);
                                    j -= 1;
                                } else {
                                    break;
                                }
                            }
                        }
                        *a.borrow_mut() = items;
                        Value::Arr(a.clone())
                    }
                    ArrOp::Flat => {
                        // depth 인자만큼 재귀적으로 펼친다 (§23.1.3.11). 기본 1.
                        // Infinity 면 완전 평탄화. 예전엔 인자를 무시하고 항상 1단계였다.
                        let depth = match args.first() {
                            None | Some(Value::Undefined) => 1i32,
                            Some(v) => {
                                let n = to_num(v);
                                if n.is_nan() || n <= 0.0 {
                                    0
                                } else if n > i32::MAX as f64 {
                                    i32::MAX
                                } else {
                                    n as i32
                                }
                            }
                        };
                        fn flatten(items: &[Value], depth: i32, out: &mut Vec<Value>) {
                            for v in items {
                                match v {
                                    Value::Arr(inner) if depth > 0 => {
                                        flatten(&inner.borrow(), depth - 1, out)
                                    }
                                    other => out.push(other.clone()),
                                }
                            }
                        }
                        let mut out = Vec::new();
                        flatten(&a.borrow(), depth, &mut out);
                        Value::Arr(ArrayObj::new(out))
                    }
                    ArrOp::At => {
                        // arr.at(i): 음수는 끝에서. 범위 밖 undefined.
                        let len = a.borrow().len() as isize;
                        let i = args.first().map(to_num).unwrap_or(0.0) as isize;
                        let idx = if i < 0 { len + i } else { i };
                        if idx >= 0 && idx < len {
                            a.borrow().get(idx as usize).cloned().unwrap_or(Value::Undefined)
                        } else {
                            Value::Undefined
                        }
                    }
                    ArrOp::Fill => {
                        // arr.fill(value, start?, end?): 제자리 채우고 배열 반환.
                        let val = args.first().cloned().unwrap_or(Value::Undefined);
                        let len = a.borrow().len() as isize;
                        let clampi = |v: f64| -> usize {
                            let i = v as isize;
                            (if i < 0 { len + i } else { i }).clamp(0, len) as usize
                        };
                        let start = clampi(args.get(1).map(to_num).unwrap_or(0.0));
                        let end = clampi(args.get(2).map(to_num).unwrap_or(len as f64));
                        {
                            let mut b = a.borrow_mut();
                            for i in start..end {
                                if i < b.len() {
                                    b[i] = val.clone();
                                }
                            }
                        }
                        Value::Arr(a.clone())
                    }
                    // arr.with(index, value) (§23.1.3.39): 원본 불변, 새 배열 반환.
                    // 범위 밖 인덱스면 RangeError.
                    ArrOp::With => {
                        let mut items = a.borrow().clone();
                        let len = items.len() as isize;
                        let n = args.first().map(to_num).unwrap_or(0.0);
                        let k = if n.is_nan() { 0 } else { n.trunc() as isize };
                        let idx = if k < 0 { len + k } else { k };
                        if idx < 0 || idx >= len {
                            return Err(self.throw_error("RangeError", "Invalid index"));
                        }
                        items[idx as usize] = args.get(1).cloned().unwrap_or(Value::Undefined);
                        Value::Arr(ArrayObj::new(items))
                    }
                    // arr.toSpliced(start, skipCount, ...items) (§23.1.3.35): splice 비변형판.
                    ArrOp::ToSpliced => {
                        let src = a.borrow().clone();
                        let len = src.len() as isize;
                        let start = {
                            let n = args.first().map(to_num).unwrap_or(0.0);
                            let k = if n.is_nan() { 0 } else { n.trunc() as isize };
                            (if k < 0 { len + k } else { k }).clamp(0, len) as usize
                        };
                        let skip = if args.is_empty() {
                            0
                        } else if args.len() == 1 {
                            src.len() - start
                        } else {
                            let n = args.get(1).map(to_num).unwrap_or(0.0);
                            let k = if n.is_nan() { 0.0 } else { n.trunc() };
                            (k.max(0.0) as usize).min(src.len() - start)
                        };
                        let mut out: Vec<Value> = Vec::with_capacity(src.len());
                        out.extend_from_slice(&src[..start]);
                        if args.len() > 2 {
                            out.extend(args[2..].iter().cloned());
                        }
                        out.extend_from_slice(&src[start + skip..]);
                        Value::Arr(ArrayObj::new(out))
                    }
                };
                // 제자리 변형(push/pop/splice/sort/reverse/fill 등)을 array-like 로 되쓴다.
                if let Some(o) = write_back {
                    let items = a.borrow().clone();
                    write_back_array_like(&o, &items);
                }
                Ok(out)
            }
            Native::JsonParse => {
                let src = args.first().map(to_display).unwrap_or_default();
                json_parse(&src)
            }
            // 순환 구조면 TypeError 를 던진다(표준). 조용히 폭발/무한재귀하지 않는다.
            // replacer(배열/함수)와 space(들여쓰기)도 표준대로 처리한다 — 예전엔 둘 다
            // 조용히 무시해서 JSON.stringify(o, null, 2) 가 한 줄로 나왔다.
            Native::JsonStringify => {
                let v = args.first().cloned().unwrap_or(Value::Undefined);
                let replacer = args.get(1).cloned().unwrap_or(Value::Undefined);
                let indent = match args.get(2) {
                    Some(Value::Num(n)) if *n >= 1.0 => " ".repeat((*n as usize).min(10)),
                    Some(Value::Str(s)) => s.chars().take(10).collect(),
                    _ => String::new(),
                };
                let keys: Option<Vec<String>> = match &replacer {
                    Value::Arr(a) => Some(a.borrow().iter().map(to_display).collect()),
                    _ => None,
                };
                let fnrep = if matches!(replacer, Value::Fn(_) | Value::Native(_) | Value::Bound(_)) {
                    Some(replacer.clone())
                } else {
                    None
                };
                let mut path = Vec::new();
                let holder = Value::Obj(Rc::new(RefCell::new(ObjMap::new())));
                match self.json_ser(&v, "", &holder, &fnrep, &keys, &indent, 0, &mut path) {
                    Ok(s) => Ok(s.map(Value::Str).unwrap_or(Value::Undefined)),
                    Err(msg) => Err(self.throw_error("TypeError", msg)),
                }
            }
            Native::ParseInt => {
                let s = args.first().map(to_display).unwrap_or_default();
                let t = s.trim();
                let (neg, mut body) = match t.strip_prefix('-') {
                    Some(rest) => (true, rest),
                    None => (false, t.strip_prefix('+').unwrap_or(t)),
                };
                // radix: 인자 있으면 사용, 0/미지정이면 자동(0x→16, 아니면 10)
                let mut radix = args.get(1).map(|v| to_num(v) as i64).unwrap_or(0);
                if (radix == 16 || radix == 0)
                    && (body.starts_with("0x") || body.starts_with("0X"))
                {
                    body = &body[2..];
                    radix = 16;
                }
                if radix == 0 {
                    radix = 10;
                }
                if !(2..=36).contains(&radix) {
                    return Ok(Value::Num(f64::NAN));
                }
                // 자리별 f64 누적 (오버플로 없이 임의 길이, JS 동일 근사)
                let mut val = 0.0f64;
                let mut any = false;
                for c in body.chars() {
                    match c.to_digit(radix as u32) {
                        Some(d) => {
                            val = val * radix as f64 + d as f64;
                            any = true;
                        }
                        None => break,
                    }
                }
                if !any {
                    return Ok(Value::Num(f64::NAN));
                }
                Ok(Value::Num(if neg { -val } else { val }))
            }
            Native::ParseFloat => {
                let s = args.first().map(to_display).unwrap_or_default();
                let t = s.trim();
                // 앞부분의 유효한 수 프리픽스만
                let mut end = 0;
                let bytes = t.as_bytes();
                let mut seen_dot = false;
                if end < bytes.len() && (bytes[end] == b'-' || bytes[end] == b'+') {
                    end += 1;
                }
                while end < bytes.len()
                    && (bytes[end].is_ascii_digit() || (bytes[end] == b'.' && !seen_dot))
                {
                    if bytes[end] == b'.' {
                        seen_dot = true;
                    }
                    end += 1;
                }
                Ok(t[..end].parse::<f64>().map(Value::Num).unwrap_or(Value::Num(f64::NAN)))
            }
            Native::EncodeUri => {
                // encodeURI: 예약문자(;,/?:@&=+$#) 와 비예약문자는 보존
                Ok(Value::Str(uri_encode(&args.first().map(to_display).unwrap_or_default(), ";,/?:@&=+$#")))
            }
            Native::EncodeUriComponent => {
                // encodeURIComponent: 비예약문자만 보존 (예약문자도 인코딩)
                Ok(Value::Str(uri_encode(&args.first().map(to_display).unwrap_or_default(), "")))
            }
            Native::DecodeUri | Native::DecodeUriComponent => {
                Ok(Value::Str(uri_decode(&args.first().map(to_display).unwrap_or_default())))
            }
            Native::UrlCtor => self.make_url(args), // URL(x) (new 없이) — 관대하게 생성
            Native::UrlToString => Ok(Value::Str(recv_prop_str(&recv, "href"))),
            Native::UrlSearchToString => Ok(Value::Str(recv_prop_str(&recv, "\u{0}query"))),
            Native::UrlSearchGet => {
                let key = args.first().map(to_display).unwrap_or_default();
                Ok(parse_query(&recv_prop_str(&recv, "\u{0}query"))
                    .into_iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| Value::Str(v))
                    .unwrap_or(Value::Null))
            }
            Native::UrlSearchGetAll => {
                let key = args.first().map(to_display).unwrap_or_default();
                let vals: Vec<Value> = parse_query(&recv_prop_str(&recv, "\u{0}query"))
                    .into_iter()
                    .filter(|(k, _)| *k == key)
                    .map(|(_, v)| Value::Str(v))
                    .collect();
                Ok(Value::Arr(ArrayObj::new(vals)))
            }
            Native::UrlSearchHas => {
                let key = args.first().map(to_display).unwrap_or_default();
                Ok(Value::Bool(
                    parse_query(&recv_prop_str(&recv, "\u{0}query")).iter().any(|(k, _)| *k == key),
                ))
            }
            Native::UrlSearchSet => {
                if let Some(Value::Obj(o)) = &recv {
                    let mut pairs = parse_query(&recv_prop_str(&recv, "\u{0}query"));
                    let key = args.first().map(to_display).unwrap_or_default();
                    let val = args.get(1).map(to_display).unwrap_or_default();
                    pairs.retain(|(k, _)| *k != key);
                    pairs.push((key, val));
                    o.borrow_mut().insert("\u{0}query".to_string(), Value::Str(build_query(&pairs)));
                }
                Ok(Value::Undefined)
            }
            Native::UrlSearchAppend => {
                if let Some(Value::Obj(o)) = &recv {
                    let mut pairs = parse_query(&recv_prop_str(&recv, "\u{0}query"));
                    let key = args.first().map(to_display).unwrap_or_default();
                    let val = args.get(1).map(to_display).unwrap_or_default();
                    pairs.push((key, val));
                    o.borrow_mut().insert("\u{0}query".to_string(), Value::Str(build_query(&pairs)));
                }
                Ok(Value::Undefined)
            }
            Native::UrlSearchDelete => {
                if let Some(Value::Obj(o)) = &recv {
                    let mut pairs = parse_query(&recv_prop_str(&recv, "\u{0}query"));
                    let key = args.first().map(to_display).unwrap_or_default();
                    pairs.retain(|(k, _)| *k != key);
                    o.borrow_mut().insert("\u{0}query".to_string(), Value::Str(build_query(&pairs)));
                }
                Ok(Value::Undefined)
            }
            Native::IsNaN => {
                Ok(Value::Bool(args.first().map(to_num).unwrap_or(f64::NAN).is_nan()))
            }
            Native::StructuredClone => {
                Ok(deep_clone(args.first().unwrap_or(&Value::Undefined), 0))
            }
            Native::ReflectGet => {
                let target = args.first().cloned().unwrap_or(Value::Undefined);
                let key = args.get(1).map(to_display).unwrap_or_default();
                self.member_get(&target, &key)
            }
            Native::ReflectSet => {
                let key = args.get(1).map(to_display).unwrap_or_default();
                let val = args.get(2).cloned().unwrap_or(Value::Undefined);
                let mut ok = false;
                match args.first() {
                    Some(Value::Obj(o)) => {
                        o.borrow_mut().insert(key, val);
                        ok = true;
                    }
                    Some(Value::Instance(i)) => {
                        i.fields.borrow_mut().insert(key, val);
                        ok = true;
                    }
                    _ => {}
                }
                Ok(Value::Bool(ok))
            }
            Native::ReflectHas => {
                let key = args.get(1).map(to_display).unwrap_or_default();
                let has = match args.first() {
                    Some(Value::Obj(m)) => {
                        !is_internal_key(&key)
                            && (m.borrow().contains_key(&key)
                                || self
                                    .proto_chain_lookup(m, &key, args.first().unwrap())
                                    .map(|v| v.is_some())
                                    .unwrap_or(false))
                    }
                    Some(Value::Instance(i)) => i.fields.borrow().contains_key(&key),
                    _ => false,
                };
                Ok(Value::Bool(has))
            }
            Native::ReflectDeleteProperty => {
                let key = args.get(1).map(to_display).unwrap_or_default();
                if let Some(Value::Obj(o)) = args.first() {
                    o.borrow_mut().remove(&key);
                }
                Ok(Value::Bool(true))
            }
            Native::ReflectApply => {
                // Reflect.apply(fn, thisArg, argsList)
                let f = args.first().cloned().unwrap_or(Value::Undefined);
                let this = args.get(1).cloned();
                let arg_list = match args.get(2) {
                    Some(Value::Arr(a)) => a.borrow().clone(),
                    _ => Vec::new(),
                };
                self.call_value(f, this, arg_list)
            }
            Native::ReflectConstruct => {
                // Reflect.construct(target, argumentsList[, newTarget]) (§26.1.2).
                let f = args.first().cloned().unwrap_or(Value::Undefined);
                // step 1: target 이 생성자가 아니면 TypeError.
                if !self.is_constructor(&f) {
                    return Err(self.throw_error(
                        "TypeError",
                        "Reflect.construct target is not a constructor",
                    ));
                }
                let arg_list = match args.get(1) {
                    Some(Value::Arr(a)) => a.borrow().clone(),
                    _ => Vec::new(),
                };
                // step 3: newTarget(주어졌으면)도 생성자여야 한다. isConstructor 하네스가
                // 이 검사에 의존한다 — Reflect.construct(function(){}, [], method) 가 던져야
                // isConstructor(method)===false 가 된다.
                if let Some(nt) = args.get(2) {
                    if !self.is_constructor(nt) {
                        return Err(self.throw_error(
                            "TypeError",
                            "Reflect.construct newTarget is not a constructor",
                        ));
                    }
                }
                self.construct(f, arg_list)
            }
            Native::LsGetItem => {
                let k = args.first().map(to_display).unwrap_or_default();
                Ok(self
                    .storage
                    .iter()
                    .find(|(sk, _)| *sk == k)
                    .map(|(_, v)| Value::Str(v.clone()))
                    .unwrap_or(Value::Null))
            }
            // Storage.length / Storage.key(i) — 삽입 순서 기준 (표준 §12.2)
            Native::LsLength => Ok(Value::Num(self.storage.len() as f64)),
            // Headers.get/has (Fetch 표준). 이름은 대소문자를 구분하지 않는다.
            Native::HeadersGet | Native::HeadersHas => {
                let name = args
                    .first()
                    .map(to_display)
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                let key = format!("\u{0}h:{}", name);
                let found = match &recv {
                    Some(Value::Obj(o)) => o.borrow().get(&key).cloned(),
                    _ => None,
                };
                Ok(match n {
                    Native::HeadersHas => Value::Bool(found.is_some()),
                    _ => found.unwrap_or(Value::Null),
                })
            }
            Native::LsKey => {
                let i = args.first().map(to_num).unwrap_or(f64::NAN);
                if !i.is_finite() || i < 0.0 {
                    return Ok(Value::Null);
                }
                Ok(self
                    .storage
                    .get(i as usize)
                    .map(|(k, _)| Value::Str(k.clone()))
                    .unwrap_or(Value::Null))
            }
            Native::LsSetItem => {
                let k = args.first().map(to_display).unwrap_or_default();
                let v = args.get(1).map(to_display).unwrap_or_default();
                match self.storage.iter_mut().find(|(sk, _)| *sk == k) {
                    Some(slot) => slot.1 = v,
                    None => self.storage.push((k, v)),
                }
                Ok(Value::Undefined)
            }
            Native::LsRemoveItem => {
                let k = args.first().map(to_display).unwrap_or_default();
                self.storage.retain(|(sk, _)| *sk != k);
                Ok(Value::Undefined)
            }
            Native::LsClear => {
                self.storage.clear();
                Ok(Value::Undefined)
            }
            Native::Alert => {
                let msg = args.iter().map(to_display).collect::<Vec<_>>().join(" ");
                self.console.push(format!("[alert] {}", msg));
                Ok(Value::Undefined)
            }
            Native::Noop => Ok(Value::Undefined),
            Native::ObjectKeys => match args.first() {
                // Proxy: ownKeys 트랩 (없으면 타깃 위임). Object.keys(proxy) 가 빈 배열을
                // 돌려주던 문제 — 반응성 프록시의 키 열거가 통째로 안 됐다.
                Some(Value::Proxy(p)) => {
                    let (t, h) = (p.0.clone(), p.1.clone());
                    let trap = self.member_get(&h, "ownKeys")?;
                    if is_callable(&trap) {
                        let res = self.call_value(trap, Some(h), vec![t])?;
                        return Ok(res);
                    }
                    self.call_native(Native::ObjectKeys, None, vec![t])
                }
                Some(Value::Obj(m)) => {
                    let keys: Vec<Value> =
                        enumerable_keys(m).into_iter().map(Value::Str).collect();
                    Ok(Value::Arr(ArrayObj::new(keys)))
                }
                Some(Value::Arr(a)) => {
                    let keys: Vec<Value> =
                        (0..a.borrow().len()).map(|i| Value::Str(i.to_string())).collect();
                    Ok(Value::Arr(ArrayObj::new(keys)))
                }
                Some(v @ (Value::Instance(_) | Value::Class(_))) => {
                    let keys: Vec<Value> = own_enumerable_entries(v)
                        .into_iter()
                        .map(|(k, _)| Value::Str(k))
                        .collect();
                    Ok(Value::Arr(ArrayObj::new(keys)))
                }
                _ => Ok(Value::Arr(ArrayObj::new(Vec::new()))),
            },
            // Object.getOwnPropertyNames(o) — 열거 여부 무관 모든 own 문자열 키 (§20.1.2.10).
            // 예전엔 Object.keys 별칭이라 non-enumerable(내장 메서드 등)을 빠뜨렸다.
            Native::ObjectGetOwnPropertyNames => {
                let names: Vec<Value> = match args.first() {
                    Some(Value::Obj(m)) => {
                        let b = m.borrow();
                        b.keys()
                            // 내부 키(\0…, __proto__)와 심볼(\0@@)/마커 제외. 실제
                            // 문자열 프로퍼티만. 마커/심볼은 is_internal_key 로 걸러진다.
                            .filter(|k| !is_internal_key(k))
                            .cloned()
                            .map(Value::Str)
                            .collect()
                    }
                    Some(Value::Arr(a)) => {
                        let n = a.borrow().len();
                        let mut v: Vec<Value> =
                            (0..n).map(|i| Value::Str(i.to_string())).collect();
                        // 배열은 own "length" 를 가진다.
                        v.push(Value::Str("length".to_string()));
                        v
                    }
                    Some(v @ (Value::Instance(_) | Value::Class(_))) => own_enumerable_entries(v)
                        .into_iter()
                        .map(|(k, _)| Value::Str(k))
                        .collect(),
                    // 내장 함수/생성자: length·name(삭제 안 됐으면) + 정적/상수/prototype (§17).
                    Some(v @ Value::Native(_)) => {
                        let mut out: Vec<Value> = Vec::new();
                        for k in ["length", "name"] {
                            if !self.native_prop_deleted(v, k) {
                                out.push(Value::Str(k.to_string()));
                            }
                        }
                        if let Value::Native(n) = v {
                            if let Some(keys) = self.native_ctor_own_keys(n) {
                                out.extend(keys.into_iter().map(Value::Str));
                            }
                        }
                        out
                    }
                    _ => Vec::new(),
                };
                Ok(Value::Arr(ArrayObj::new(names)))
            }
            Native::ObjectValues => {
                let vals: Vec<Value> = match args.first() {
                    Some(Value::Obj(m)) => {
                        enumerable_entries(m).into_iter().map(|(_, v)| v).collect()
                    }
                    Some(Value::Arr(a)) => a.borrow().clone(),
                    Some(v @ Value::Instance(_)) => {
                        own_enumerable_entries(v).into_iter().map(|(_, v)| v).collect()
                    }
                    _ => Vec::new(),
                };
                Ok(Value::Arr(ArrayObj::new(vals)))
            }
            Native::ObjectEntries => {
                let pair = |k: &str, v: &Value| {
                    Value::Arr(ArrayObj::new(vec![Value::Str(k.to_string()), v.clone()]))
                };
                let entries: Vec<Value> = match args.first() {
                    Some(Value::Obj(m)) => {
                        enumerable_entries(m).iter().map(|(k, v)| pair(k, v)).collect()
                    }
                    Some(Value::Arr(a)) => a
                        .borrow()
                        .iter()
                        .enumerate()
                        .map(|(i, v)| pair(&i.to_string(), v))
                        .collect(),
                    Some(v @ Value::Instance(_)) => {
                        own_enumerable_entries(v).iter().map(|(k, v)| pair(k, v)).collect()
                    }
                    _ => Vec::new(),
                };
                Ok(Value::Arr(ArrayObj::new(entries)))
            }
            Native::ObjectFromEntries => {
                // [[k,v], ...] 또는 Map → 객체. 이터러블 순회.
                let mut map = ObjMap::new();
                if let Some(src) = args.first() {
                    for entry in self.iterate_to_vec(src)? {
                        let (k, v) = match &entry {
                            Value::Arr(a) => {
                                let b = a.borrow();
                                (b.first().cloned(), b.get(1).cloned())
                            }
                            _ => (None, None),
                        };
                        if let Some(k) = k {
                            map.insert(to_display(&k), v.unwrap_or(Value::Undefined));
                        }
                    }
                }
                Ok(Value::Obj(Rc::new(RefCell::new(map))))
            }
            // Object.assign(target, ...sources) — 표준 ToObject 의미론.
            // 대상은 객체/배열/함수/인스턴스/클래스 모두 가능(번들의 Object.assign(Fn, {...})
            // 정적 복사 패턴). null/undefined 만 TypeError. 소스도 같은 범위에서 열거.
            Native::ObjectAssign => {
                let target = args.first().cloned().unwrap_or(Value::Undefined);
                if matches!(target, Value::Undefined | Value::Null) {
                    // §20.1.2.1: ToObject(target) 가 null/undefined 면 TypeError.
                    return Err(self.throw_error(
                        "TypeError",
                        "Cannot convert undefined or null to object",
                    ));
                }
                // §20.1.2.1: 각 소스의 열거 가능한 own 프로퍼티를 Set(Throw=true)로
                // 대상에 복사. 실패(read-only/non-extensible/getter-only)면 TypeError.
                for src in &args[1..] {
                    for (k, v) in own_enumerable_entries(src) {
                        if !self.set_own_property(&target, k.clone(), v) {
                            return Err(self.throw_error(
                                "TypeError",
                                format!("Cannot assign to read only property '{}' of object", k),
                            ));
                        }
                    }
                }
                Ok(target)
            }
            Native::ArrayIsArray => {
                Ok(Value::Bool(matches!(args.first(), Some(Value::Arr(_)))))
            }
            Native::ArrayOf => Ok(Value::Arr(ArrayObj::new(args))),
            Native::ArrayFrom => {
                let src = args.first().cloned().unwrap_or(Value::Undefined);
                let map_fn = args.get(1).cloned().filter(is_callable);
                // 이터러블(배열/문자열/Set/Map/제너레이터/반복자/사용자 [Symbol.iterator])이면
                // 프로토콜로, 아니면 array-like(length + 인덱스).
                let items: Vec<Value> = match &src {
                    Value::Arr(_)
                    | Value::Str(_)
                    | Value::SetVal(_)
                    | Value::MapVal(_)
                    | Value::Gen(_) => self.iterate_to_vec(&src)?,
                    Value::Obj(o)
                        if o.borrow().contains_key("\u{0}items") || o.borrow().contains_key("next") =>
                    {
                        self.iterate_to_vec(&src)?
                    }
                    _ if self.try_get_iterator(&src)?.is_some() => self.iterate_to_vec(&src)?,
                    // array-like: length + 인덱스 프로퍼티 (길이 상한은 표준대로 RangeError)
                    Value::Obj(o) => array_like_to_vec(o)?,
                    _ => Vec::new(),
                };
                // mapFn(value, index) 적용
                let out = match map_fn {
                    Some(f) => {
                        let mut r = Vec::with_capacity(items.len());
                        for (i, v) in items.into_iter().enumerate() {
                            r.push(self.call_value(f.clone(), None, vec![v, Value::Num(i as f64)])?);
                        }
                        r
                    }
                    None => items,
                };
                Ok(Value::Arr(ArrayObj::new(out)))
            }
            Native::SetTimeout | Native::SetInterval => {
                let callback = args.first().cloned().unwrap_or(Value::Undefined);
                if !matches!(callback, Value::Fn(_) | Value::Native(_)) {
                    return Ok(Value::Num(0.0)); // 콜백 아니면 무시
                }
                let delay_ms = args.get(1).map(to_num).unwrap_or(0.0).max(0.0);
                let id = self.next_timer_id;
                self.next_timer_id += 1;
                self.timers.push(Timer {
                    id,
                    callback,
                    delay_ms,
                    repeat: n == Native::SetInterval,
                });
                Ok(Value::Num(id as f64))
            }
            Native::ClearTimer => {
                if let Some(id) = args.first().map(to_num) {
                    let id = id as u64;
                    self.cleared.insert(id);
                    self.timers.retain(|t| t.id != id);
                }
                Ok(Value::Undefined)
            }
            // Promise(executor) (new 없이 호출) — 관대하게 생성자처럼
            Native::PromiseCtor => self.construct(Value::Native(Native::PromiseCtor), args),
            // executor 의 resolve(v): this(=promise)를 v 로 이행
            Native::PromiseSettleResolve => {
                let v = args.into_iter().next().unwrap_or(Value::Undefined);
                if let Some(p) = recv {
                    // 이미 정착된 promise 는 무시 (한 번만)
                    let pending = matches!(&p, Value::Obj(o)
                        if matches!(o.borrow().get("\u{0}state"), Some(Value::Str(s)) if s == "pending"));
                    if pending {
                        self.resolve_promise(&p, v);
                    }
                }
                Ok(Value::Undefined)
            }
            // executor 의 reject(e): this(=promise)를 거부 상태로
            Native::PromiseSettleReject => {
                let v = args.into_iter().next().unwrap_or(Value::Undefined);
                if let Some(Value::Obj(o)) = recv {
                    let mut m = o.borrow_mut();
                    if matches!(m.get("\u{0}state"), Some(Value::Str(s)) if s == "pending") {
                        m.insert("\u{0}state".to_string(), Value::Str("rejected".to_string()));
                        m.insert("\u{0}value".to_string(), v);
                    }
                }
                Ok(Value::Undefined)
            }
            Native::PromiseResolve => {
                let v = args.into_iter().next().unwrap_or(Value::Undefined);
                let p = self.new_promise();
                self.resolve_promise(&p, v);
                Ok(p)
            }
            Native::PromiseReject => {
                let v = args.into_iter().next().unwrap_or(Value::Undefined);
                let p = self.new_promise();
                if let Value::Obj(o) = &p {
                    let mut m = o.borrow_mut();
                    m.insert("\u{0}state".to_string(), Value::Str("rejected".to_string()));
                    m.insert("\u{0}value".to_string(), v);
                }
                Ok(p)
            }
            Native::PromiseAll | Native::PromiseAllSettled => {
                let all_settled = matches!(n, Native::PromiseAllSettled);
                let items = match args.into_iter().next() {
                    Some(Value::Arr(a)) => a.borrow().clone(),
                    _ => Vec::new(),
                };
                let p = self.new_promise();
                let mut out = Vec::new();
                for item in items {
                    let (fulfilled, v) = self.promise_settle_state(&item);
                    if all_settled {
                        // allSettled: 항목마다 {status, value|reason}
                        let mut m = ObjMap::new();
                        if fulfilled {
                            m.insert("status".to_string(), Value::Str("fulfilled".to_string()));
                            m.insert("value".to_string(), v);
                        } else {
                            m.insert("status".to_string(), Value::Str("rejected".to_string()));
                            m.insert("reason".to_string(), v);
                        }
                        out.push(Value::Obj(Rc::new(RefCell::new(m))));
                    } else if !fulfilled {
                        // all: 하나라도 거부되면 그 이유로 즉시 거부
                        self.reject_promise(&p, v);
                        return Ok(p);
                    } else {
                        out.push(v);
                    }
                }
                self.resolve_promise(&p, Value::Arr(ArrayObj::new(out)));
                Ok(p)
            }
            Native::PromiseRace => {
                let items = match args.into_iter().next() {
                    Some(Value::Arr(a)) => a.borrow().clone(),
                    _ => Vec::new(),
                };
                let p = self.new_promise();
                // 첫 항목의 정착 상태를 채택(이행/거부)
                if let Some(first) = items.first() {
                    let (fulfilled, v) = self.promise_settle_state(first);
                    if fulfilled {
                        self.resolve_promise(&p, v);
                    } else {
                        self.reject_promise(&p, v);
                    }
                }
                Ok(p)
            }
            Native::PromiseThen => {
                let p = recv.unwrap_or(Value::Undefined);
                let mut it = args.into_iter();
                let on_f = it.next().unwrap_or(Value::Undefined);
                let on_r = it.next().unwrap_or(Value::Undefined);
                let dep = self.new_promise();
                Ok(self.promise_then(&p, on_f, on_r, dep))
            }
            // p.catch(onR) = then(undefined, onR)
            Native::PromiseCatch => {
                let p = recv.unwrap_or(Value::Undefined);
                let on_r = args.into_iter().next().unwrap_or(Value::Undefined);
                let dep = self.new_promise();
                Ok(self.promise_then(&p, Value::Undefined, on_r, dep))
            }
            // p.finally(cb): 이행/거부 모두 cb 실행 후 원 결과(값/거부)를 전파.
            // 동기 모델: 대기 마이크로태스크를 먼저 흘려 p 를 정착시킨 뒤 cb 실행하고
            // p 를 그대로 반환(정착 상태 — 이행값 또는 거부이유 — 유지).
            Native::PromiseFinally => {
                let p = recv.unwrap_or(Value::Undefined);
                if let Some(cb) = args.into_iter().next() {
                    if is_callable(&cb) {
                        self.drain_microtasks();
                        let _ = self.call_value(cb, None, vec![]);
                    }
                }
                Ok(p)
            }
            Native::Identity => Ok(args.into_iter().next().unwrap_or(Value::Undefined)),
            Native::Fetch => {
                let raw = args.first().map(to_display).unwrap_or_default();
                // 상대 URL 은 문서 URL 기준으로 절대화한다. 예전엔 그대로 넘겨
                // fetch('/api/x') 가 Url(NoScheme) 로 실패했다 — SPA 는 거의 다 상대경로다.
                let url = self.absolute_url(&raw);
                // init: { method, headers, body } — 예전엔 통째로 무시하고 **GET 을 보냈다**.
                // 서버는 다른 걸 돌려주고, 사이트는 그걸 자기 POST 의 답이라고 믿는다.
                let init = args.get(1).cloned().unwrap_or(Value::Undefined);
                let mut req = crate::http::Request::default();
                if !matches!(init, Value::Undefined | Value::Null) {
                    let m = self.member_get(&init, "method")?;
                    if !matches!(m, Value::Undefined | Value::Null) {
                        req.method = to_display(&m);
                    }
                    let h = self.member_get(&init, "headers")?;
                    match &h {
                        // Headers 객체(우리 표현: "\0h:name" 키) 또는 평범한 객체
                        Value::Obj(o) => {
                            for (k, v) in o.borrow().iter() {
                                if let Some(name) = k.strip_prefix("\u{0}h:") {
                                    req.headers.push((name.to_string(), to_display(v)));
                                } else if !k.starts_with('\u{0}') && !is_callable(v) {
                                    req.headers.push((k.clone(), to_display(v)));
                                }
                            }
                        }
                        Value::Arr(a) => {
                            for row in a.borrow().iter() {
                                if let Value::Arr(kv) = row {
                                    let kv = kv.borrow();
                                    if kv.len() >= 2 {
                                        req.headers
                                            .push((to_display(&kv[0]), to_display(&kv[1])));
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                    let b = self.member_get(&init, "body")?;
                    if !matches!(b, Value::Undefined | Value::Null) {
                        req.body = Some(to_display(&b).into_bytes());
                    }
                }
                let resp = self.new_promise();
                match crate::http::fetch_req(&url, &req) {
                    Ok(r) => {
                        let body = String::from_utf8_lossy(&r.body).to_string();
                        let mut m = ObjMap::new();
                        m.insert("status".to_string(), Value::Num(r.status as f64));
                        m.insert(
                            "ok".to_string(),
                            Value::Bool(r.status >= 200 && r.status < 300),
                        );
                        m.insert("\u{0}body".to_string(), Value::Str(body));
                        m.insert("text".to_string(), Value::Native(Native::ResponseText));
                        m.insert("json".to_string(), Value::Native(Native::ResponseJson));
                        // 원본 바이트를 따로 붙든다 — 위의 body 는 lossy UTF-8 이라
                        // 바이너리(wasm/이미지)를 그걸로 되돌리면 조용히 망가진다.
                        self.fetch_bodies.push(Rc::new(r.body.clone()));
                        m.insert(
                            "\u{0}raw".to_string(),
                            Value::Num((self.fetch_bodies.len() - 1) as f64),
                        );
                        m.insert(
                            "arrayBuffer".to_string(),
                            Value::Native(Native::ResponseArrayBuffer),
                        );
                        // Response 는 headers/url/statusText/type/redirected 를 갖는다 (Fetch 표준).
                        // 없으면 r.headers.get('content-type') 같은 흔한 코드가 즉사한다.
                        m.insert("url".to_string(), Value::Str(url.clone()));
                        m.insert("redirected".to_string(), Value::Bool(false));
                        m.insert("type".to_string(), Value::Str("basic".to_string()));
                        m.insert(
                            "statusText".to_string(),
                            Value::Str(
                                match r.status {
                                    200 => "OK",
                                    201 => "Created",
                                    204 => "No Content",
                                    301 => "Moved Permanently",
                                    302 => "Found",
                                    304 => "Not Modified",
                                    400 => "Bad Request",
                                    401 => "Unauthorized",
                                    403 => "Forbidden",
                                    404 => "Not Found",
                                    500 => "Internal Server Error",
                                    _ => "",
                                }
                                .to_string(),
                            ),
                        );
                        let mut hm = ObjMap::new();
                        let mut pairs: Vec<Value> = Vec::new();
                        for (k, v) in &r.headers {
                            hm.insert(
                                format!("\u{0}h:{}", k.to_ascii_lowercase()),
                                Value::Str(v.clone()),
                            );
                            pairs.push(Value::Arr(ArrayObj::new(vec![
                                Value::Str(k.to_ascii_lowercase()),
                                Value::Str(v.clone()),
                            ])));
                        }
                        hm.insert("get".to_string(), Value::Native(Native::HeadersGet));
                        hm.insert("has".to_string(), Value::Native(Native::HeadersHas));
                        hm.insert("\u{0}items".to_string(), Value::Arr(ArrayObj::new(pairs)));
                        m.insert("headers".to_string(), Value::Obj(Rc::new(RefCell::new(hm))));
                        let response = Value::Obj(Rc::new(RefCell::new(m)));
                        self.resolve_promise(&resp, response);
                    }
                    Err(e) => {
                        self.console.push(format!("fetch 실패: {:?}", e));
                        self.resolve_promise(&resp, Value::Undefined);
                    }
                }
                Ok(resp)
            }
            Native::ResponseText | Native::ResponseJson => {
                let body = match &recv {
                    Some(Value::Obj(o)) => match o.borrow().get("\u{0}body") {
                        Some(Value::Str(s)) => s.clone(),
                        _ => String::new(),
                    },
                    _ => String::new(),
                };
                let val = if matches!(n, Native::ResponseJson) {
                    json_parse(&body).unwrap_or(Value::Null)
                } else {
                    Value::Str(body)
                };
                let p = self.new_promise();
                self.resolve_promise(&p, val);
                Ok(p)
            }
            // Response.arrayBuffer() — 받은 바이트 그대로 ArrayBuffer 로.
            Native::ResponseArrayBuffer => {
                let idx = match &recv {
                    Some(Value::Obj(o)) => match o.borrow().get("\u{0}raw") {
                        Some(Value::Num(i)) => *i as usize,
                        _ => usize::MAX,
                    },
                    _ => usize::MAX,
                };
                let bytes = self
                    .fetch_bodies
                    .get(idx)
                    .cloned()
                    .ok_or("arrayBuffer: 원본 바이트가 없다")?;
                let buf = self.make_array_buffer(&bytes)?;
                let p = self.new_promise();
                self.resolve_promise(&p, buf);
                Ok(p)
            }

            Native::ZeroBytes => {
                let n = num_arg(&args, 0).max(0.0) as usize;
                if n > 512 * 1024 * 1024 {
                    return Err(self.throw_error("RangeError", "Invalid array buffer length"));
                }
                Ok(Value::Arr(ArrayObj::new(vec![Value::Num(0.0); n])))
            }

            // ── WebAssembly ────────────────────────────────────────────────
            Native::WasmValidate => {
                let bytes = bytes_of(args.first());
                Ok(Value::Bool(crate::wasm::parse(&bytes).is_ok()))
            }
            Native::WasmCompile => {
                let bytes = bytes_of(args.first());
                let m = crate::wasm::parse(&bytes)
                    .map_err(|e| format!("WebAssembly.CompileError: {}", e))?;
                self.wasm_modules.push(Rc::new(m));
                Ok(Value::Num((self.wasm_modules.len() - 1) as f64))
            }
            // 모듈이 스스로 정의한 메모리의 최소 페이지 수 (없거나 임포트면 -1)
            Native::WasmMemPages => {
                let i = num_arg(&args, 0) as usize;
                let m = self.wasm_modules.get(i).ok_or("wasm: 모듈 없음")?;
                Ok(Value::Num(match m.mem_pages {
                    Some(p) => p as f64,
                    None => -1.0,
                }))
            }
            // JS 가 만든 ArrayBuffer 의 바이트 배열을 wasm 선형 메모리로 등록한다.
            // (배열을 **공유**한다 — 복사가 아니다)
            Native::WasmRegisterMemory => {
                let arr = match args.first() {
                    Some(Value::Arr(a)) => a.clone(),
                    _ => return Err("wasm: 메모리 등록에 바이트 배열이 필요하다".to_string()),
                };
                let obj = args.get(1).cloned().unwrap_or(Value::Undefined);
                self.wasm_memories
                    .push((Rc::new(RefCell::new(arr)), obj));
                Ok(Value::Num((self.wasm_memories.len() - 1) as f64))
            }
            // memory.grow — 이전 페이지 수 (실패하면 -1). JS 쪽 buffer 재바인딩까지 여기서.
            Native::WasmGrow => {
                let i = num_arg(&args, 0) as usize;
                let pages = num_arg(&args, 1) as u32;
                let (mem, _) = self.wasm_memories.get(i).ok_or("wasm: 메모리 없음")?;
                let mem = mem.clone();
                let old = crate::wasm::grow_mem(&mem, pages);
                if old < 0 {
                    return Ok(Value::Num(-1.0));
                }
                self.sync_wasm_memories();
                Ok(Value::Num(old as f64))
            }
            Native::WasmInstantiate => {
                let mi = num_arg(&args, 0) as usize;
                let imports = args.get(1).cloned().unwrap_or(Value::Undefined);
                let mem_idx = num_arg(&args, 2);
                self.wasm_instantiate(mi, imports, mem_idx)
            }
            Native::WasmCall(inst, func) => {
                let wi = self
                    .wasm_instances
                    .get(inst as usize)
                    .cloned()
                    .ok_or("wasm: 인스턴스 없음")?;
                let ft = wi
                    .inst
                    .func_type(func)
                    .cloned()
                    .ok_or("wasm: 함수 타입 없음")?;
                let vals: Vec<crate::wasm::Val> = ft
                    .params
                    .iter()
                    .enumerate()
                    .map(|(k, t)| {
                        super::js_to_wasm_typed(args.get(k).unwrap_or(&Value::Undefined), *t)
                    })
                    .collect();
                let module = wi.inst.module.clone();
                let mut host = super::WasmHost {
                    interp: self,
                    imports: wi.imports.clone(),
                    module,
                };
                let out = wi
                    .inst
                    .call(&mut host, func, &vals)
                    .map_err(|e| format!("WebAssembly.RuntimeError: {}", e));
                // 호출 중 memory.grow 가 있었으면 JS 의 memory.buffer 를 새 배열로 다시 묶는다
                self.sync_wasm_memories();
                let out = out?;
                Ok(match out.len() {
                    0 => Value::Undefined,
                    1 => super::wasm_val_to_js(&out[0]),
                    _ => Value::Arr(ArrayObj::new(
                        out.iter().map(super::wasm_val_to_js).collect(),
                    )),
                })
            }
            Native::WasmGlobalGet(inst, g) => {
                let wi = self
                    .wasm_instances
                    .get(inst as usize)
                    .cloned()
                    .ok_or("wasm: 인스턴스 없음")?;
                let v = *wi
                    .inst
                    .globals
                    .borrow()
                    .get(g as usize)
                    .ok_or("wasm: 전역 없음")?;
                Ok(super::wasm_val_to_js(&v))
            }
            Native::WasmGlobalSet(inst, g) => {
                let wi = self
                    .wasm_instances
                    .get(inst as usize)
                    .cloned()
                    .ok_or("wasm: 인스턴스 없음")?;
                let t = wi.inst.module.global_type(g as usize);
                let v = super::js_to_wasm_typed(args.first().unwrap_or(&Value::Undefined), t);
                let mut gs = wi.inst.globals.borrow_mut();
                *gs.get_mut(g as usize).ok_or("wasm: 전역 없음")? = v;
                Ok(Value::Undefined)
            }
            // 내보내진 테이블의 table.get(i) — 함수 참조를 호출 가능한 값으로 돌려준다
            Native::WasmTableGet(inst) => {
                let wi = self
                    .wasm_instances
                    .get(inst as usize)
                    .cloned()
                    .ok_or("wasm: 인스턴스 없음")?;
                let i = num_arg(&args, 0) as usize;
                let f = wi.inst.table.borrow().get(i).copied().flatten();
                Ok(match f {
                    Some(fi) => Value::Native(Native::WasmCall(inst, fi)),
                    None => Value::Null,
                })
            }

            Native::GetAttribute => match recv {
                Some(Value::Dom(id)) => {
                    let raw = args.first().map(to_display).unwrap_or_default();
                    let name = {
                        let dom = self.dom_arena()?;
                        let html_ns = matches!(&dom.get(id).node_type,
                            crate::dom::NodeType::Element(e) if e.namespace.is_none());
                        if html_ns { raw.to_ascii_lowercase() } else { raw }
                    };
                    let dom = self.dom_arena()?;
                    match &dom.get(id).node_type {
                        crate::dom::NodeType::Element(e) => Ok(e
                            .attributes
                            .get(&name)
                            .map(|v| Value::Str(v.clone()))
                            .unwrap_or(Value::Null)),
                        _ => Ok(Value::Null),
                    }
                }
                other => Err(format!(
                    "getAttribute 는 요소 메서드 (수신자={})",
                    other.map(|v| type_of(&v)).unwrap_or("없음")
                )),
            },
        }
    }
}

// 같은 이름의 라디오 버튼을 모은다 (라디오 그룹).
fn collect_radio_peers(
    dom: &crate::dom::Dom,
    id: crate::dom::NodeId,
    name: &str,
    out: &mut Vec<crate::dom::NodeId>,
) {
    if let crate::dom::NodeType::Element(e) = &dom.get(id).node_type {
        if e.tag_name == "input"
            && e.attributes.get("type").map(|t| t.eq_ignore_ascii_case("radio")).unwrap_or(false)
            && e.attributes.get("name").map(|n| n == name).unwrap_or(name.is_empty())
        {
            out.push(id);
        }
    }
    for &c in &dom.get(id).children {
        collect_radio_peers(dom, c, name, out);
    }
}
