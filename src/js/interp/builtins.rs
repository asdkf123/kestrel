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

// ToLength (§7.1.20): NaN/음수 → 0, 아니면 floor 후 2^53-1 로 클램프.
// §7.1.6 ToUint16: NaN/±0/±∞→0, 그 밖은 trunc 후 2^16 모듈로.
fn to_uint16(n: f64) -> u16 {
    if !n.is_finite() {
        return 0;
    }
    n.trunc().rem_euclid(65536.0) as u16
}

// Number::exponentiate (§6.1.6.1.3): Math.pow/** 의 표준 특수 케이스. Rust powf 는 IEEE
// pow 라 pow(1,NaN)=1, pow(-1,±∞)=1 을 내지만 JS 는 NaN 이어야 한다.
fn math_pow(base: f64, exp: f64) -> f64 {
    if exp == 0.0 {
        return 1.0; // ±0 지수 → 1 (base 가 NaN 이어도)
    }
    if exp.is_nan() || base.is_nan() {
        return f64::NAN;
    }
    if base.abs() == 1.0 && exp.is_infinite() {
        return f64::NAN; // ±1 ** ±∞ → NaN
    }
    base.powf(exp)
}

// §22.1.3.29 WhiteSpace + LineTerminator: JS 문자열 trim 대상. Rust 의 char::is_whitespace
// 와 달리 U+FEFF(ZWNBSP/BOM)를 포함하고, U+200B(zero-width space)는 제외한다.
fn is_js_ws(c: char) -> bool {
    matches!(c,
        '\u{0009}' | '\u{000A}' | '\u{000B}' | '\u{000C}' | '\u{000D}'
        | '\u{0020}' | '\u{00A0}' | '\u{1680}'
        | '\u{2000}'..='\u{200A}'
        | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}'
        | '\u{FEFF}'
    )
}

fn to_length(n: f64) -> f64 {
    if n.is_nan() || n <= 0.0 {
        0.0
    } else {
        n.floor().min(9_007_199_254_740_991.0)
    }
}

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
            // 구멍 인덱스는 열거 대상이 아니다 (희소 배열).
            let b = a.borrow();
            let mut out: Vec<(String, Value)> = a
                .present_indices()
                .into_iter()
                .map(|i| (i.to_string(), b[i].clone()))
                .collect();
            drop(b);
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

// WHATWG 퍼센트 디코딩(관대): %XX 는 hex 면 바이트로, 아니면 그대로 통과. UTF-8 은 lossy.
// URLSearchParams 등 폼 파싱용 — decodeURI 처럼 URIError 를 던지지 않는다. 바이트 파싱이라
// 멀티바이트 문자에서도 안전(문자열 슬라이스 금지).
pub(super) fn percent_decode_lossy(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// decodeURI 가 디코드하지 않고 %XX 로 남기는 예약 문자 (uriReserved + '#', §19.2.6.1).
fn is_uri_reserved(c: char) -> bool {
    matches!(c, ';' | '/' | '?' | ':' | '@' | '&' | '=' | '+' | '$' | ',' | '#')
}

// §19.2.6.1 Decode(string, preserveEscapeSet). %XX 시퀀스를 바이트로 모아 UTF-8 로 해석.
// preserve_reserved=true(decodeURI)면 예약 ASCII 문자는 %XX 로 남긴다. 잘못된 % 시퀀스나
// 유효하지 않은 UTF-8 은 URIError(Err). 예전엔 관대 통과 + 문자열 슬라이스로 멀티바이트에서
// 패닉했다.
pub(super) fn uri_decode(s: &str, preserve_reserved: bool) -> Result<String, ()> {
    let bytes = s.as_bytes();
    let hexval = |b: u8| -> Option<u8> { (b as char).to_digit(16).map(|d| d as u8) };
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'%' {
            // % 아닌 원본 바이트(멀티바이트 문자 포함)는 그대로 복사 — s 가 valid UTF-8 이라 안전.
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        // %XX — 두 자리는 반드시 hex.
        if i + 2 >= bytes.len() {
            return Err(());
        }
        let (Some(h), Some(l)) = (hexval(bytes[i + 1]), hexval(bytes[i + 2])) else {
            return Err(());
        };
        let b0 = h * 16 + l;
        let start = i;
        i += 3;
        if b0 & 0x80 == 0 {
            // ASCII: 예약 문자는 %XX 로 보존(decodeURI), 아니면 디코드.
            let c = b0 as char;
            if preserve_reserved && is_uri_reserved(c) {
                out.extend_from_slice(&bytes[start..i]);
            } else {
                out.push(b0);
            }
        } else {
            // 멀티바이트 UTF-8 선두. 선행 1비트 수 = 시퀀스 길이(2..4).
            let n = b0.leading_ones() as usize;
            if !(2..=4).contains(&n) {
                return Err(());
            }
            let mut octets = vec![b0];
            for _ in 1..n {
                if i + 2 >= bytes.len() || bytes[i] != b'%' {
                    return Err(());
                }
                let (Some(hh), Some(ll)) = (hexval(bytes[i + 1]), hexval(bytes[i + 2])) else {
                    return Err(());
                };
                let cb = hh * 16 + ll;
                if cb & 0xC0 != 0x80 {
                    return Err(()); // continuation 바이트는 10xxxxxx 여야 한다.
                }
                octets.push(cb);
                i += 3;
            }
            // UTF-8 유효성(overlong/서로게이트 거부는 std 가 처리) 확인 후 디코드.
            match std::str::from_utf8(&octets) {
                Ok(dec) => out.extend_from_slice(dec.as_bytes()),
                Err(_) => return Err(()),
            }
        }
    }
    String::from_utf8(out).map_err(|_| ())
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
    // InternalizeJSONProperty (§25.5.1.1): reviver 를 후위 순회로 적용한다. 자식을 먼저
    // 되살린 뒤(undefined 면 삭제) reviver(holder, [name, val, context]) 를 호출한다.
    // context 는 3번째 인자 (json-parse-with-source): 원시 리프면 { source: 원본텍스트 },
    // 객체/배열이거나 reviver 가 값을 바꿨으면 빈 객체.
    fn json_revive(
        &mut self,
        holder: &Value,
        name: &str,
        reviver: &Value,
        snap: Option<&JsonSrc>,
    ) -> Result<Value, String> {
        let val = self.member_get(holder, name)?;
        match &val {
            Value::Arr(a) => {
                let len = a.borrow().len();
                for i in 0..len {
                    let child = match snap {
                        Some(JsonSrc::Arr(v)) => v.get(i),
                        _ => None,
                    };
                    let nv = self.json_revive(&val, &i.to_string(), reviver, child)?;
                    // undefined 면 삭제(배열에선 hole) — 근사로 undefined 유지.
                    if let Some(slot) = a.borrow_mut().get_mut(i) {
                        *slot = nv;
                    }
                }
            }
            Value::Obj(m) => {
                for k in enumerable_keys(m) {
                    let child = match snap {
                        Some(JsonSrc::Obj(v)) => {
                            v.iter().rev().find(|(kk, _)| *kk == k).map(|(_, s)| s)
                        }
                        _ => None,
                    };
                    let nv = self.json_revive(&val, &k, reviver, child)?;
                    if matches!(nv, Value::Undefined) {
                        m.borrow_mut().remove(&k);
                    } else {
                        m.borrow_mut().insert(k, nv);
                    }
                }
            }
            _ => {}
        }
        // context 객체: 원시 리프이고 파싱된 값이 그대로면 source 를 채운다.
        let ctx = ObjMap::new();
        let ctx = Rc::new(RefCell::new(ctx));
        if !matches!(val, Value::Arr(_) | Value::Obj(_) | Value::Instance(_)) {
            if let Some(JsonSrc::Prim { raw, val: pv }) = snap {
                if same_value(&val, pv) {
                    ctx.borrow_mut().insert("source".to_string(), Value::Str(raw.clone()));
                }
            }
        }
        self.call_value(
            reviver.clone(),
            Some(holder.clone()),
            vec![Value::Str(name.to_string()), val, Value::Obj(ctx)],
        )
    }

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
                // §25.5.2: 순환 구조는 TypeError. throw_error 로 실제 TypeError 를 던진다
                // (예전엔 dispatch 가 문자열을 TypeError 로 재래핑했으나 이제 직접 전파).
                return Err(
                    self.throw_error("TypeError", "Converting circular structure to JSON")
                );
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
        // rawJSON 객체(ES2024 §25.5.1)는 저장된 원본 텍스트를 그대로 낸다.
        if let Value::Obj(m) = v {
            let raw = {
                let b = m.borrow();
                if b.contains_key("\u{0}isRawJSON") {
                    match b.get("rawJSON") {
                        Some(Value::Str(s)) => Some(s.clone()),
                        _ => None,
                    }
                } else {
                    None
                }
            };
            if let Some(raw) = raw {
                return Ok(Some(raw));
            }
        }
        // 원시 래퍼(new Number/String/Boolean)는 원시값으로 직렬화 (§25.5.2.2 step 4).
        let unwrapped;
        let v = match v {
            Value::Obj(m) if m.borrow().contains_key(WRAPPER_SLOT) => {
                unwrapped = wrapper_primitive(v).unwrap_or(Value::Null);
                &unwrapped
            }
            _ => v,
        };
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
                let len = a.borrow().len();
                let mut parts = Vec::with_capacity(len);
                for i in 0..len {
                    // Get(array, ToString(i)) — getter 호출/예외 전파(예전엔 저장값 그대로).
                    let item = self.member_get(v, &i.to_string())?;
                    let s = self
                        .json_ser(&item, &i.to_string(), v, fnrep, keys, indent, depth + 1, path)?
                        .unwrap_or_else(|| "null".to_string()); // 직렬화 불가 항목은 null
                    parts.push(s);
                }
                Some(wrap(parts, '[', ']'))
            }
            // Date 는 toJSON 규약대로 ISO 문자열 (내부 마커 노출 아님)
            Value::Obj(map) if json_is_date(map) => Some(
                json_date_iso(map).map(|s| json_quote_pub(&s)).unwrap_or_else(|| "null".to_string()),
            ),
            Value::Obj(map) => {
                // 키 순회: replacer 배열이 있으면 그 순서(§25.5.2.1), 없으면 열거 own 키.
                // 값은 Get(value, key) 로 읽어 getter 를 호출하고 예외를 전파한다 — 예전엔
                // enumerable_entries 로 저장값(Accessor)을 그대로 봐 getter 가 실행 안 됐다.
                let key_list: Vec<String> = match keys {
                    Some(ks) => ks.clone(),
                    None => enumerable_keys(map),
                };
                let mut parts = Vec::new();
                for k in &key_list {
                    let val = self.member_get(v, k)?;
                    if let Some(s) = self.json_ser(&val, k, v, fnrep, keys, indent, depth + 1, path)? {
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
                    let Some(mv) = m.get(k) else { continue };
                    if let Some(s) =
                        self.json_ser(&mv.clone(), k, v, fnrep, keys, indent, depth + 1, path)?
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
            // 문자열 패턴 (§22.1.3.19/.20): ToString(pat), 함수가 아니면 ToString(repl),
            // $ 치환은 GetSubstitution($$/$&/$`/$'). 문자 인덱스로 순회한다.
            let needle = self.to_string_value(pat)?;
            let repl_tmpl = if is_callable(repl) {
                None
            } else {
                Some(self.to_string_value(repl)?)
            };
            let hay: Vec<char> = s.chars().collect();
            let nchars: Vec<char> = needle.chars().collect();
            let nlen = nchars.len();
            let mut out = String::new();
            let mut i = 0usize;
            loop {
                let found = if nlen == 0 {
                    Some(i)
                } else if nlen > hay.len() || i > hay.len() - nlen {
                    None
                } else {
                    (i..=hay.len() - nlen).find(|&k| hay[k..k + nlen] == nchars[..])
                };
                match found {
                    Some(at) => {
                        out.extend(&hay[i..at]);
                        let mt = crate::js::regex::Match {
                            start: at,
                            end: at + nlen,
                            groups: vec![Some((at, at + nlen))],
                        };
                        if is_callable(repl) {
                            // position 은 UTF-16 코드유닛 인덱스 (§22.1.3.19 step 14.d).
                            let pos16 =
                                hay[..at].iter().collect::<String>().encode_utf16().count();
                            let r = self.call_value(
                                repl.clone(),
                                None,
                                vec![
                                    Value::Str(needle.clone()),
                                    Value::Num(pos16 as f64),
                                    Value::Str(s.to_string()),
                                ],
                            )?;
                            out.push_str(&self.to_string_value(&r)?);
                        } else {
                            out.push_str(&expand_replacement(
                                repl_tmpl.as_deref().unwrap_or(""),
                                &hay,
                                &mt,
                            ));
                        }
                        if nlen == 0 {
                            // 빈 패턴: 현재 문자를 흘리고 한 칸 전진.
                            if at < hay.len() {
                                out.push(hay[at]);
                            }
                            i = at + 1;
                        } else {
                            i = at + nlen;
                        }
                        if !all {
                            out.extend(hay.get(i.min(hay.len())..).unwrap_or(&[]));
                            break;
                        }
                        if i > hay.len() {
                            break;
                        }
                    }
                    None => {
                        out.extend(hay.get(i.min(hay.len())..).unwrap_or(&[]));
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
    // §21.4.2.1 Date(...args): 인자를 표준 순서로 강제변환하고(valueOf/@@toPrimitive 관찰,
    // 예외 전파) MakeDay/MakeTime/MakeDate/TimeClip 로 조립한다. NaN/Infinity/오버플로는
    // 유한성 검사로 NaN 이 되고, -0 은 TimeClip 이 +0 으로 만든다.
    fn make_date_from_args(&mut self, args: &[Value]) -> Result<Value, String> {
        let millis = match args.len() {
            0 => now_millis(),
            1 => match &args[0] {
                // [[DateValue]] 를 가진 Date → 그 시간값을 그대로(단, TimeClip).
                Value::Obj(m) if is_date_obj(m) => {
                    let t = match m.borrow().get("\u{0}time") {
                        Some(Value::Num(n)) => *n,
                        _ => f64::NAN,
                    };
                    time_clip(t)
                }
                other => {
                    // v = ToPrimitive(value) (힌트 default). String 이면 파싱, 아니면 ToNumber.
                    let prim = self.to_primitive_hint(other.clone(), "default")?;
                    match prim {
                        Value::Str(s) => parse_date_string(&s).unwrap_or(f64::NAN),
                        v => time_clip(self.to_number_value(&v)?),
                    }
                }
            },
            _ => {
                // (year, month[0기준], date, h, m, s, ms) — 제공된 인자만 순서대로 ToNumber.
                let num = |me: &mut Self, i: usize, dflt: f64| -> Result<f64, String> {
                    match args.get(i) {
                        Some(v) => me.to_number_value(v),
                        None => Ok(dflt),
                    }
                };
                let y = num(self, 0, f64::NAN)?;
                let mo = num(self, 1, 0.0)?;
                let d = num(self, 2, 1.0)?;
                let h = num(self, 3, 0.0)?;
                let mi = num(self, 4, 0.0)?;
                let s = num(self, 5, 0.0)?;
                let ms = num(self, 6, 0.0)?;
                build_date_full(y, mo, d, h, mi, s, ms)
            }
        };
        // 인스턴스를 실제 Date.prototype 에 링크한다 — getPrototypeOf/isPrototypeOf/
        // .constructor 가 하드코딩 특수처리 없이 프로토타입 체인으로 해석되게. 메서드
        // 디스패치는 is_date_obj 특수 arm 이 그대로 처리한다(중복이지만 무해).
        let v = make_date(millis);
        if let Value::Obj(m) = &v {
            m.borrow_mut().insert("__proto__".to_string(), self.date_proto.clone());
        }
        Ok(v)
    }




    // OrdinaryDefineOwnProperty (§10.1.6): 서술자를 검증하고 적용한다.
    // configurable:false 프로퍼티는 재정의를 거부하고(configurable/enumerable 변경 불가,
    // writable:false 로만 강등 가능, 값은 writable 일 때만), 없던 프로퍼티는 새로 만든다.
    // 예전엔 writable/configurable 을 통째로 무시하고 값만 덮었다 — 서술자가 이름만 있고
    // 강제되지 않는 편법이었다.
    pub(super) fn ordinary_define(
        &mut self,
        map: &RefCell<ObjMap>,
        key: &str,
        desc: &Value,
        extensible: bool,
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
        // §10.1.6.3 step 2.a: 존재하지 않는 프로퍼티인데 대상이 non-extensible 이면
        // 정의할 수 없다 → TypeError (defineProperty). 예전엔 이 검사가 없어 얼린/
        // preventExtensions 객체에도 새 프로퍼티가 조용히 추가됐다.
        if existing.is_none() && !extensible {
            return Err(self.throw_error(
                "TypeError",
                "Cannot define property: object is not extensible",
            ));
        }
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

    // generic 배열 메서드(§23.1.3)를 위한 array-like 읽기: ToObject 후
    // ToLength(Get(O,"length")) 만큼, 각 인덱스를 [[Get]] 으로(접근자/프로토타입
    // 체인 존중) 읽어 임시 Vec 로 재료화한다. 예전 array_like_to_vec 는 own 데이터
    // length·own 인덱스만 봐서 getter length / "Infinity" 문자열 / 상속 원소 /
    // 원시 래퍼(Boolean.prototype[1]) 수신자를 전부 놓쳤다.
    // ToObject (§7.1.18): 원시값(Bool/Num/Str)을 래퍼 객체로 박는다. 이미 객체면 그대로.
    // generic 배열/문자열 메서드의 콜백 3번째 인자(=ToObject(this))에 쓴다 — 예전엔
    // 원시값을 그대로 넘겨 `obj instanceof Boolean` 등이 어긋났다.
    pub(super) fn to_object_value(&self, v: Value) -> Value {
        let (proto, tag) = match &v {
            Value::Str(_) => (self.string_proto.clone(), "String"),
            Value::Num(_) => (self.number_proto.clone(), "Number"),
            Value::Bool(_) => (self.boolean_proto.clone(), "Boolean"),
            _ => return v, // 이미 객체(또는 null/undefined — 호출부에서 이미 처리)
        };
        let mut m = ObjMap::new();
        m.insert(WRAPPER_SLOT.to_string(), v.clone());
        m.insert("__proto__".to_string(), proto);
        m.insert("\u{0}class".to_string(), Value::Str(tag.to_string()));
        if let Value::Str(s) = &v {
            for (i, c) in s.chars().enumerate() {
                m.insert(i.to_string(), Value::Str(c.to_string()));
                m.insert(attr_marker(&i.to_string()), Value::Num(ATTR_ENUMERABLE as f64));
            }
            m.insert("length".to_string(), Value::Num(s.chars().count() as f64));
            set_prop_attrs(&mut m, "length", 0); // {w:f,e:f,c:f}
        }
        Value::Obj(Rc::new(RefCell::new(m)))
    }

    // indexOf/lastIndexOf/includes 를 length 가 배열 상한 초과인 array-like 에서 재료화 없이
    // 지연 검색한다({0:0,length:Infinity} 등). 존재하는 정수 own 키만 검사 → OOM/무한루프 없음.
    fn generic_array_search_huge(
        &mut self,
        recv: &Value,
        op: ArrOp,
        args: &[Value],
        len: f64,
    ) -> Result<Value, String> {
        let needle = args.first().cloned().unwrap_or(Value::Undefined);
        // fromIndex (ToIntegerOrInfinity). indexOf/includes 기본 0.
        let n = match args.get(1) {
            Some(v) if !matches!(v, Value::Undefined) => self.to_integer_or_infinity(v)?,
            _ => 0.0,
        };
        let start = if n < 0.0 { (len + n).max(0.0) } else { n };
        // 존재하는 정수 own 키(오름차순).
        let mut keys: Vec<usize> = match recv {
            Value::Obj(m) => m.borrow().keys().filter_map(|k| k.parse::<usize>().ok()).collect(),
            _ => Vec::new(),
        };
        keys.sort_unstable();
        keys.dedup();
        if matches!(op, ArrOp::Includes) {
            // huge sparse array-like 에는 구멍(=undefined)이 존재하므로 undefined 검색은 true.
            if start < len && same_value_zero(&needle, &Value::Undefined) {
                return Ok(Value::Bool(true));
            }
            for &k in &keys {
                if (k as f64) < start || (k as f64) >= len {
                    continue;
                }
                let v = self.member_get(recv, &k.to_string())?;
                if same_value_zero(&v, &needle) {
                    return Ok(Value::Bool(true));
                }
            }
            return Ok(Value::Bool(false));
        }
        // IndexOf
        for &k in &keys {
            if (k as f64) < start || (k as f64) >= len {
                continue;
            }
            if self.has_property(recv, &k.to_string()) {
                let v = self.member_get(recv, &k.to_string())?;
                if strict_eq(&v, &needle) {
                    return Ok(Value::Num(k as f64));
                }
            }
        }
        Ok(Value::Num(-1.0))
    }

    // Math 인자 강제변환: args[i] 를 ToNumber (없으면 NaN). valueOf/@@toPrimitive 관찰 + 예외 전파.
    fn math_arg(&mut self, args: &[Value], i: usize) -> Result<f64, String> {
        match args.get(i) {
            Some(v) => self.to_number_value(v),
            None => Ok(f64::NAN),
        }
    }

    // [[GetPrototypeOf]] (§20.1.2.12 등) — 모든 Value 종류의 프로토타입을 돌려준다.
    // 체인의 끝/프로토타입 없음은 Null. Object.getPrototypeOf 와 isPrototypeOf 가 공유한다.
    pub(super) fn proto_of(&mut self, v: &Value) -> Result<Value, String> {
        let obj_proto = self.member_get(&self.object_ns.clone(), "prototype")?;
        Ok(match v {
            Value::Obj(m) => {
                // Object.prototype 자신의 프로토타입은 null (체인의 끝).
                if let Value::Obj(op) = &obj_proto {
                    if Rc::ptr_eq(m, op) {
                        return Ok(Value::Null);
                    }
                }
                match m.borrow().get("__proto__") {
                    Some(p) => p.clone(),
                    None => obj_proto,
                }
            }
            Value::Arr(_) => self.member_get(&self.array_ns.clone(), "prototype")?,
            Value::Instance(inst) => {
                self.member_get(&Value::Class(inst.class.clone()), "prototype")?
            }
            // NativeError 생성자의 [[Prototype]] 은 Error 생성자 (§20.5.6.2).
            Value::Native(Native::ErrorCtor(n)) if *n != "Error" => {
                env_get(&self.global, "Error").unwrap_or_else(|| self.fn_proto.clone())
            }
            Value::Fn(f) => f
                .props
                .borrow()
                .get("__proto__")
                .cloned()
                .unwrap_or_else(|| self.fn_proto.clone()),
            Value::Native(_) | Value::Bound(_) => self.fn_proto.clone(),
            Value::Str(_) => self.string_proto.clone(),
            Value::Num(_) => self.number_proto.clone(),
            Value::Bool(_) => self.boolean_proto.clone(),
            Value::MapVal(_) => self.map_proto.clone(),
            Value::SetVal(_) => self.set_proto.clone(),
            Value::Gen(_) | Value::Class(_) | Value::Proxy(_) => obj_proto,
            _ => Value::Null,
        })
    }

    pub(super) fn generic_array_read(&mut self, recv: &Value) -> Result<Vec<Value>, String> {
        let len_val = self.member_get(recv, "length")?;
        // ToLength(? ToNumber(len)) — 객체 length 의 valueOf/toString 을 호출하고
        // Symbol/BigInt 는 TypeError. 예전엔 to_num 이라 valueOf 미호출·비강제였다
        // (Array.prototype.reduce.call({length:{valueOf(){…}}}) 등이 length 0 으로 오독).
        let len = to_length(self.to_number_value(&len_val)?);
        // 실제 배열 상한(2^32-1)을 넘는 length 는 재료화하지 않는다(수십억 할당 방지).
        // ToLength 는 2^53-1 까지 허용하지만, 그런 거대 array-like 는 지연순회가
        // 필요하고 실사용상 병적 케이스라 RangeError 로 막는다(프로세스 보호).
        if len > MAX_ARRAY_LEN {
            return Err(self.throw_error("RangeError", "Invalid array length"));
        }
        let n = len as usize;
        let mut out = Vec::with_capacity(n.min(4096));
        for i in 0..n {
            out.push(self.member_get(recv, &i.to_string())?);
        }
        Ok(out)
    }

    // generic_array_read 의 구멍 보존판: HasProperty(§7.3.11)가 false 인 인덱스는 구멍으로
    // 남긴다. filter/map/forEach/reduce 등이 구멍을 건너뛰어야(§23.1.3) 정확하다 —
    // 예전엔 dense Vec 라 obj={1:11,length:2} 의 인덱스 0 이 undefined 원소로 세어졌다.
    pub(super) fn generic_array_read_sparse(
        &mut self,
        recv: &Value,
    ) -> Result<std::rc::Rc<ArrayObj>, String> {
        let len_val = self.member_get(recv, "length")?;
        let len = to_length(self.to_number_value(&len_val)?);
        if len > MAX_ARRAY_LEN {
            return Err(self.throw_error("RangeError", "Invalid array length"));
        }
        let n = len as usize;
        let mut out = Vec::with_capacity(n.min(4096));
        let mut holes = std::collections::HashSet::new();
        for i in 0..n {
            let key = i.to_string();
            if self.has_property(recv, &key) {
                out.push(self.member_get(recv, &key)?);
            } else {
                out.push(Value::Undefined);
                holes.insert(i);
            }
        }
        Ok(ArrayObj::with_holes(out, holes))
    }

    // 배열 반복 메서드용 원소 해석 (§23.1.3: HasProperty + Get).
    // 반환 None = 인덱스가 존재하지 않음(진짜 구멍) → HasProperty 계열은 건너뛴다.
    // 구멍이라도 own-prop(defineProperty)/상속이면 Get 으로 값을 읽는다. 값이 접근자면
    // 호출한다. 덴스+비접근자(흔한 경우)는 snapshot 값을 그대로 써 빠르다.
    pub(super) fn arr_elem(
        &mut self,
        a: &std::rc::Rc<ArrayObj>,
        arr_val: &Value,
        snapshot: &[Value],
        i: usize,
        has_holes: bool,
    ) -> Result<Option<Value>, String> {
        if has_holes && a.is_hole(i) {
            let key = i.to_string();
            // own(defineProperty) 이거나 프로토타입 체인(Array.prototype 에 사용자가 얹은
            // 인덱스 포함)에 그 키가 있으면 HasProperty=true → [[Get]]. 예전엔 proto_method
            // 로 내장 메서드만 봐서 Array.prototype[1]=1 상속 원소를 놓쳤다.
            let inherited = {
                let proto = self.member_get(&self.array_ns.clone(), "prototype")?;
                self.has_property(&proto, &key)
            };
            if a.get_prop(&key).is_some() || inherited {
                Ok(Some(self.member_get(arr_val, &key)?))
            } else {
                Ok(None) // 진짜 구멍
            }
        } else {
            match snapshot.get(i) {
                // defineProperty 로 인덱스에 심긴 접근자는 호출해 값을 낸다.
                Some(Value::Accessor(_)) => Ok(Some(self.member_get(arr_val, &i.to_string())?)),
                Some(v) => Ok(Some(v.clone())),
                None => Ok(Some(Value::Undefined)),
            }
        }
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
            // Proxy.revocable(target, handler) (§28.2.1): { proxy, revoke } 를 만든다.
            // revoke() 는 proxy 를 취소해 이후 모든 내부 메서드가 TypeError 가 되게 한다.
            Native::ProxyRevocable => {
                let target = args.first().cloned().unwrap_or(Value::Undefined);
                let handler = args.get(1).cloned().unwrap_or(Value::Undefined);
                if !is_object(&target) || !is_object(&handler) {
                    return Err(self.throw_error(
                        "TypeError",
                        "Cannot create proxy with a non-object as target or handler",
                    ));
                }
                let proxy = Value::Proxy(Rc::new((target, handler)));
                // revoke: Bound(ProxyRevoke, this=proxy, []) — 호출 시 this 가 그 proxy.
                let revoke = Value::Bound(Rc::new((
                    Value::Native(Native::ProxyRevoke),
                    proxy.clone(),
                    vec![],
                )));
                let mut m = ObjMap::new();
                m.insert("proxy".to_string(), proxy);
                m.insert("revoke".to_string(), revoke);
                Ok(Value::Obj(Rc::new(RefCell::new(m))))
            }
            // revoke 함수 본체: bound this 인 proxy 의 포인터를 취소 집합에 넣는다.
            Native::ProxyRevoke => {
                if let Some(Value::Proxy(p)) = &recv {
                    self.revoked_proxies.insert(Rc::as_ptr(p) as *const () as usize);
                }
                Ok(Value::Undefined)
            }
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
                // §20.2.3.3: this(target)가 callable 이 아니면 TypeError.
                let target = recv.unwrap_or(Value::Undefined);
                if !is_callable(&target) {
                    return Err(self.throw_error(
                        "TypeError",
                        "Function.prototype.call called on non-callable",
                    ));
                }
                let mut it = args.into_iter();
                let this_arg = it.next().unwrap_or(Value::Undefined);
                self.call_value(target, Some(this_arg), it.collect())
            }
            // fn.apply(thisArg, argArray) — §20.2.3.1
            Native::FnApply => {
                let target = recv.unwrap_or(Value::Undefined);
                if !is_callable(&target) {
                    return Err(self.throw_error(
                        "TypeError",
                        "Function.prototype.apply called on non-callable",
                    ));
                }
                let mut it = args.into_iter();
                let this_arg = it.next().unwrap_or(Value::Undefined);
                // argArray: null/undefined → 빈 목록; 그 밖엔 CreateListFromArrayLike(§7.3.18 —
                // 객체가 아니면 TypeError, ToLength(length)+[[Get]], getter 예외 전파).
                let call_args = match it.next() {
                    None | Some(Value::Undefined) | Some(Value::Null) => Vec::new(),
                    Some(Value::Arr(a)) => a.borrow().clone(),
                    Some(v) if is_object(&v) => self.generic_array_read(&v)?,
                    Some(_) => {
                        return Err(self.throw_error(
                            "TypeError",
                            "CreateListFromArrayLike called on non-object",
                        ))
                    }
                };
                self.call_value(target, Some(this_arg), call_args)
            }
            // fn.bind(thisArg, ...partial) → 바운드 함수
            Native::FnBind => {
                // §20.2.3.2: Target 이 callable 이 아니면 TypeError.
                let target = recv.unwrap_or(Value::Undefined);
                if !is_callable(&target) {
                    return Err(self.throw_error(
                        "TypeError",
                        "Function.prototype.bind called on non-callable",
                    ));
                }
                let mut it = args.into_iter();
                let this_arg = it.next().unwrap_or(Value::Undefined);
                let partial: Vec<Value> = it.collect();
                Ok(Value::Bound(Rc::new((target, this_arg, partial))))
            }
            // Function.prototype[@@hasInstance](V) = OrdinaryHasInstance (§20.2.3.6).
            // instanceof 연산자의 기본 경로에 위임한다(그쪽이 FnHasInstance 를 우회하므로
            // 무한 재귀 없음). C 가 callable 이 아니면 false.
            Native::FnHasInstance => {
                let c = recv.unwrap_or(Value::Undefined);
                if !is_callable(&c) {
                    return Ok(Value::Bool(false));
                }
                let o = args.into_iter().next().unwrap_or(Value::Undefined);
                self.binary(BinOp::Instanceof, o, c)
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
                let target0 = args.first().cloned().unwrap_or(Value::Undefined);
                // ToObject: 문자열 원시값은 래퍼로 박싱해 인덱스 서술자를 노출(§20.1.2.8).
                let target = if matches!(target0, Value::Str(_)) {
                    self.to_object_value(target0)
                } else {
                    target0
                };
                let key = match args.get(1).cloned() {
                    Some(k) => self.to_property_key(k)?,
                    None => String::new(),
                };
                let mut d = ObjMap::new();
                let found = match &target {
                    // Proxy: [[GetOwnProperty]] → getOwnPropertyDescriptor 트랩(없으면 타깃).
                    // typed array 의 정수 인덱스 서술자가 이 경로로 나온다.
                    Value::Proxy(p) => {
                        self.proxy_revoked_guard(p)?;
                        let (t, h) = (p.0.clone(), p.1.clone());
                        let trap = self.member_get(&h, "getOwnPropertyDescriptor")?;
                        if is_callable(&trap) {
                            return self.call_value(trap, Some(h), vec![t, Value::Str(key)]);
                        }
                        return self.call_native(
                            Native::ObjectGetOwnPropertyDescriptor,
                            None,
                            vec![t, Value::Str(key)],
                        );
                    }
                    Value::Obj(m) => {
                        // 실제 저장된 속성 비트를 읽어 정확히 보고한다 (§10.1.5.1).
                        // 예전엔 writable 을 항상 true 로, configurable 도 무조건 true 로
                        // 거짓말했다.
                        let b = m.borrow();
                        match b.get(&key) {
                            // 내부 마커는 숨기지만 심볼 키("\0@@…")는 실제 프로퍼티다.
                            Some(_) if is_internal_key(&key) && !is_symbol_key(&key) => false,
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
                        // 구멍 인덱스는 own 프로퍼티가 없다 → 서술자 undefined.
                        let idx_val = key.parse::<usize>().ok().and_then(|i| {
                            let b = a.borrow();
                            if i < b.len() && !a.is_hole(i) {
                                Some(b[i].clone())
                            } else {
                                None
                            }
                        });
                        match idx_val {
                            Some(v) => {
                                // 배열 인덱스 프로퍼티는 { w:true, e:true, c:true } (§10.4.2).
                                d.insert("value".to_string(), v);
                                d.insert("writable".to_string(), Value::Bool(true));
                                d.insert("enumerable".to_string(), Value::Bool(true));
                                d.insert("configurable".to_string(), Value::Bool(true));
                                true
                            }
                            None => match a.get_prop(&key) {
                                Some(v) => {
                                    d.insert("value".to_string(), v);
                                    d.insert("writable".to_string(), Value::Bool(true));
                                    d.insert("enumerable".to_string(), Value::Bool(true));
                                    d.insert("configurable".to_string(), Value::Bool(true));
                                    true
                                }
                                None => false,
                            },
                        }
                    }
                    Value::Fn(f) => {
                        // name/length 가 delete 됐으면(툼스톤) 서술자 없음.
                        if matches!(key.as_str(), "name" | "length")
                            && f.props.borrow().contains_key(&format!("\u{0}fndel:{}", key))
                        {
                            return Ok(Value::Undefined);
                        }
                        // 함수의 name/length/prototype 은 own 프로퍼티다 (§10.2.4~10.2.9).
                        // props 에 실체화(materialize)/재정의됐으면 그 값·속성을 쓰고,
                        // 아니면 계산값 { w:false, e:false, c:true }.
                        let materialized = f.props.borrow().contains_key(&key);
                        let v = match key.as_str() {
                            "prototype" => Some(self.member_get(&target, "prototype")?),
                            "name" if !materialized => Some(Value::Str(f.name.borrow().clone())),
                            "length" if !materialized => {
                                Some(Value::Num(f.params.len() as f64))
                            }
                            _ => f.props.borrow().get(&key).cloned(),
                        };
                        // 계산 name/length(props 미실체화)는 고정 속성으로 보고.
                        if matches!(key.as_str(), "name" | "length") && !materialized {
                            if let Some(val) = v {
                                d.insert("value".to_string(), val);
                                d.insert("writable".to_string(), Value::Bool(false));
                                d.insert("enumerable".to_string(), Value::Bool(false));
                                d.insert("configurable".to_string(), Value::Bool(true));
                                return Ok(Value::Obj(Rc::new(RefCell::new(d))));
                            }
                        }
                        match v {
                            // 사용자 프로퍼티(prototype 이 아닌)는 props(ObjMap)의 실제
                            // 속성 비트로 정확히 보고한다 (§10.2.4). prototype 은 계산
                            // 프로퍼티라 아래 근사 경로(tail 이 비열거 처리)로 둔다.
                            Some(sv) if key != "prototype" => {
                                let attrs = prop_attrs(&f.props.borrow(), &key);
                                match &sv {
                                    Value::Accessor(acc) => {
                                        d.insert(
                                            "get".to_string(),
                                            acc.get.clone().unwrap_or(Value::Undefined),
                                        );
                                        d.insert(
                                            "set".to_string(),
                                            acc.set.clone().unwrap_or(Value::Undefined),
                                        );
                                    }
                                    _ => {
                                        d.insert("value".to_string(), sv.clone());
                                        d.insert(
                                            "writable".to_string(),
                                            Value::Bool(attrs & ATTR_WRITABLE != 0),
                                        );
                                    }
                                }
                                d.insert(
                                    "enumerable".to_string(),
                                    Value::Bool(attrs & ATTR_ENUMERABLE != 0),
                                );
                                d.insert(
                                    "configurable".to_string(),
                                    Value::Bool(attrs & ATTR_CONFIGURABLE != 0),
                                );
                                return Ok(Value::Obj(Rc::new(RefCell::new(d))));
                            }
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
                // Proxy: [[DefineOwnProperty]] → defineProperty 트랩(없으면 타깃). typed array
                // 의 정수 인덱스 exotic define 이 이 경로. 트랩이 falsish 면 TypeError.
                if let Value::Proxy(p) = &target {
                    self.proxy_revoked_guard(p)?;
                    let (t, h) = (p.0.clone(), p.1.clone());
                    let trap = self.member_get(&h, "defineProperty")?;
                    if is_callable(&trap) {
                        let ok = self.call_value(
                            trap,
                            Some(h),
                            vec![t, Value::Str(key), desc],
                        )?;
                        if !to_bool(&ok) {
                            return Err(self.throw_error(
                                "TypeError",
                                "'defineProperty' on proxy: trap returned falsish for property",
                            ));
                        }
                        return Ok(target);
                    }
                    return self.call_native(
                        Native::ObjectDefineProperty,
                        None,
                        vec![t, Value::Str(key), desc],
                    );
                }
                // Value::Obj 는 표준 OrdinaryDefineOwnProperty (§10.1.6) 로 처리한다.
                // 그 외 대상(Instance/Arr/Class)은 속성 강제 없이 값만 넣는 근사 유지.
                if let Value::Obj(map) = &target {
                    let ext = !self.is_nonextensible_val(&target);
                    self.ordinary_define(&**map, &key, &desc, ext)?;
                    return Ok(target);
                }
                // 함수도 ordinary object 다 (§10.2) — props(ObjMap)에 표준 속성 강제로
                // 정의한다. prototype 은 지연생성 근사 경로로 둔다.
                if let Value::Fn(func) = &target {
                    if key != "prototype" {
                        // name/length 는 계산 프로퍼티 { w:false, e:false, c:true }.
                        // 재정의 전에 현재 값을 props 에 실체화(materialize)해 ordinary_define
                        // 이 기존 프로퍼티로 보고 표준 검증을 하게 한다. 재정의 시 삭제
                        // 툼스톤은 해제한다.
                        if matches!(key.as_str(), "name" | "length") {
                            let tomb = format!("\u{0}fndel:{}", key);
                            let missing = !func.props.borrow().contains_key(&key);
                            if missing && !func.props.borrow().contains_key(&tomb) {
                                let cur = if key == "name" {
                                    Value::Str(func.name.borrow().clone())
                                } else {
                                    Value::Num(func.params.len() as f64)
                                };
                                let mut mm = func.props.borrow_mut();
                                mm.insert(key.clone(), cur);
                                set_prop_attrs(&mut mm, &key, ATTR_CONFIGURABLE);
                            }
                            func.props.borrow_mut().remove(&tomb);
                        }
                        let ext = !self.is_nonextensible_val(&target);
                        self.ordinary_define(&func.props, &key, &desc, ext)?;
                        return Ok(target);
                    }
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
                                let old_len = a.borrow().len();
                                {
                                    let mut items = a.borrow_mut();
                                    while items.len() <= i {
                                        items.push(Value::Undefined);
                                    }
                                    items[i] = val;
                                }
                                // 인덱스 i 를 정의하면 존재 — 건너뛴 자리(old_len..i)는 구멍.
                                if i > old_len {
                                    for h in old_len..i {
                                        a.mark_hole(h);
                                    }
                                }
                                a.fill_hole(i);
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
                // proto 를 __proto__ 로 링크. Object.create(null) 은 __proto__ 를 **명시적
                // Null** 로 저장한다(부재=기본 Object.prototype 과 구분) → getPrototypeOf 가 null,
                // 상속 메서드 없음.
                let mut map = ObjMap::new();
                match &proto {
                    Value::Obj(_) => {
                        map.insert("__proto__".to_string(), proto.clone());
                    }
                    Value::Null => {
                        map.insert("__proto__".to_string(), Value::Null);
                    }
                    _ => {}
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
                // §20.1.2.21: target 은 RequireObjectCoercible(undefined/null → TypeError),
                // proto 는 Object|Null 아니면 TypeError. [[SetPrototypeOf]] 가 false 면 TypeError.
                let target = args.first().cloned().unwrap_or(Value::Undefined);
                let proto = args.get(1).cloned().unwrap_or(Value::Undefined);
                if matches!(target, Value::Undefined | Value::Null) {
                    return Err(self.throw_error(
                        "TypeError",
                        "Object.setPrototypeOf called on null or undefined",
                    ));
                }
                if !is_object(&proto) && !matches!(proto, Value::Null) {
                    return Err(self.throw_error(
                        "TypeError",
                        "Object prototype may only be an Object or null",
                    ));
                }
                // 원시값 target 은 [[SetPrototypeOf]] 없이 그대로 반환(§ step 4).
                if matches!(target, Value::Obj(_) | Value::Fn(_)) {
                    if !self.ordinary_set_prototype_of(&target, proto)? {
                        return Err(self.throw_error(
                            "TypeError",
                            "Cannot set prototype of this object",
                        ));
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
                // §20.1.3.4: V 가 객체가 아니면 false. 아니면 V 의 프로토타입 체인에서 this 를 찾는다.
                // 인자는 Obj 뿐 아니라 함수/배열/인스턴스 등 모든 객체형이 될 수 있다
                // (Function.prototype.isPrototypeOf(Number) 등).
                let proto = match &recv {
                    Some(p) => p.clone(),
                    None => return Ok(Value::Bool(false)),
                };
                let arg = match args.first() {
                    Some(a) => a.clone(),
                    None => return Ok(Value::Bool(false)),
                };
                if !is_object(&arg) {
                    return Ok(Value::Bool(false));
                }
                let mut cur = arg;
                for _ in 0..1000 {
                    let p = self.proto_of(&cur)?;
                    if matches!(p, Value::Null) {
                        break;
                    }
                    if strict_eq(&p, &proto) {
                        return Ok(Value::Bool(true));
                    }
                    cur = p;
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
                // SetIntegrityLevel (§7.3.15): non-extensible 뿐 아니라 각 own 프로퍼티의
                // 속성도 조인다 — seal → [[Configurable]]=false; freeze → 추가로 데이터
                // 프로퍼티 [[Writable]]=false(접근자는 configurable 만). 예전엔 무결성
                // 비트만 남겨 gOPD 가 여전히 configurable:true 라 verifyProperty 가 깨졌다.
                if bit != super::INTEG_NONEXT {
                    if let Value::Obj(m) = &arg {
                        let keys: Vec<String> = {
                            let b = m.borrow();
                            b.keys().filter(|k| !is_internal_key(k)).cloned().collect()
                        };
                        for k in keys {
                            let (is_acc, mut a) = {
                                let b = m.borrow();
                                (matches!(b.get(&k), Some(Value::Accessor(_))), prop_attrs(&b, &k))
                            };
                            a &= !ATTR_CONFIGURABLE;
                            if bit == super::INTEG_FROZEN && !is_acc {
                                a &= !ATTR_WRITABLE;
                            }
                            set_prop_attrs(&mut m.borrow_mut(), &k, a);
                        }
                    }
                }
                self.set_integrity(&arg, bit);
                Ok(arg)
            }
            // isFrozen/isSealed/isExtensible.
            // 원시값(비객체)은 frozen·sealed=true, extensible=false (표준).
            // 예전엔 인스턴스/함수/Map 도 "비객체" 취급해 안 얼렸는데 true 를 반환했다(거짓말).
            Native::ObjectIsFrozen | Native::ObjectIsSealed | Native::ObjectIsExtensible => {
                let arg = args.first().cloned().unwrap_or(Value::Undefined);
                let r = if !is_object(&arg) {
                    // 비객체(원시값): isFrozen/isSealed=true, isExtensible=false.
                    !matches!(n, Native::ObjectIsExtensible)
                } else if super::integrity_ptr(&arg).is_some() {
                    let b = self.integrity_bits(&arg);
                    let frozen = b & super::INTEG_FROZEN != 0;
                    let sealed = frozen || b & super::INTEG_SEALED != 0;
                    let nonext = sealed || b & super::INTEG_NONEXT != 0;
                    match n {
                        Native::ObjectIsFrozen => frozen,
                        Native::ObjectIsSealed => sealed,
                        _ => !nonext,
                    }
                } else {
                    // 무결성 추적 대상이 아닌 객체(내장 함수 Native/Bound/DOM 등): 얼리거나
                    // 봉인된 적이 없다 → isExtensible=true, isFrozen/isSealed=false. 예전엔
                    // integrity_ptr 이 None 이라 원시값으로 오판해 내장 메서드가 non-extensible
                    // 로 보고됐다("Built-in objects must be extensible" 위반).
                    matches!(n, Native::ObjectIsExtensible)
                };
                Ok(Value::Bool(r))
            }
            // Object.getPrototypeOf (표준 §20.1.2.12). 예전엔 __proto__ 링크가 없으면
            // 무조건 null 을 돌려줬다 — 평범한 객체·배열·인스턴스가 전부 null 이었다.
            // regenerator/babel 런타임이 getProto(getProto(values([]))) 로 내장 프로토타입을
            // 캐내는데, null 이 나오면 그 위에 세우는 이터레이터 체인이 통째로 무너진다
            // (naver 가 여기서 죽고 메모리까지 터졌다).
            Native::ObjectGetPrototypeOf => {
                // 예전엔 __proto__ 링크가 없으면 무조건 null 이라 평범한 객체/배열/인스턴스가
                // 전부 null 이었다(regenerator 런타임이 여기서 무너졌다). proto_of 로 통일.
                let arg = args.first().cloned().unwrap_or(Value::Undefined);
                self.proto_of(&arg)
            }
            // Object.prototype.hasOwnProperty.call(obj, key) / obj.hasOwnProperty(key)
            Native::HasOwnProperty => {
                let key = match args.first().cloned() {
                    Some(k) => self.to_property_key(k)?,
                    None => String::new(),
                };
                let has = match &recv {
                    // __proto__ 는 own 프로퍼티 아님(상속 accessor). 심볼 키("\0@@…")는
                    // 내부 마커와 달리 실제 own 프로퍼티다 — 예외로 허용.
                    Some(Value::Obj(m)) => {
                        ((!is_internal_key(&key) || is_symbol_key(&key))
                            && m.borrow().contains_key(&key))
                            || self.global_has(m, &key)
                    }
                    // 인스턴스는 own 필드만 own 프로퍼티(메서드는 프로토타입 격)
                    Some(Value::Instance(i)) => i.fields.borrow().contains_key(&key),
                    Some(Value::Arr(a)) => {
                        // 구멍 인덱스는 own 프로퍼티가 아니다 (희소 배열).
                        key.parse::<usize>()
                            .map(|i| i < a.borrow().len() && !a.is_hole(i))
                            .unwrap_or(false)
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
                            || key == "prototype"
                            || (matches!(key.as_str(), "name" | "length")
                                && !f.props
                                    .borrow()
                                    .contains_key(&format!("\u{0}fndel:{}", key)))
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
            // Object.prototype.propertyIsEnumerable(P) (§20.1.3.4): own 프로퍼티이면서
            // enumerable 인가. 예전엔 hasOwnProperty 로 근사해 비열거 메서드도 true 였다.
            Native::PropertyIsEnumerable => {
                let key = match args.first().cloned() {
                    Some(k) => self.to_property_key(k)?,
                    None => String::new(),
                };
                let recvv = recv.unwrap_or(Value::Undefined);
                // 열거 가능한 own 프로퍼티 목록에 있으면 true (심볼 키는 별도지만 대부분
                // 문자열 키 검사). 내부/비열거 키는 own_enumerable_entries 가 이미 거른다.
                let enumerable =
                    own_enumerable_entries(&recvv).iter().any(|(k, _)| *k == key);
                Ok(Value::Bool(enumerable))
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
            // Error.isError(v) (ES2025 §20.5.2.1): v 가 [[ErrorData]] 를 가진 객체인가.
            Native::ErrorIsError => {
                let is = matches!(
                    args.first(),
                    Some(Value::Obj(m))
                        if matches!(m.borrow().get("\u{0}errdata"), Some(Value::Bool(true)))
                );
                Ok(Value::Bool(is))
            }
            // get Error.prototype.stack — 인스턴스의 내부 슬롯에서 캡처된 스택을 읽는다.
            // [[ErrorData]] 가 없으면(에러 아님) undefined.
            Native::ErrorStackGet => {
                let this = recv.unwrap_or(Value::Undefined);
                let s = match &this {
                    Value::Obj(m) => match m.borrow().get("\u{0}errstack") {
                        Some(Value::Str(s)) => Some(Value::Str(s.clone())),
                        _ => None,
                    },
                    _ => None,
                };
                Ok(s.unwrap_or(Value::Undefined))
            }
            // set Error.prototype.stack — CreateDataProperty(this, "stack", v): own 데이터로
            // accessor 를 가린다(이후 this.stack 은 이 own 값을 읽는다). §Error Stacks.
            Native::ErrorStackSet => {
                let this = recv.unwrap_or(Value::Undefined);
                let val = args.first().cloned().unwrap_or(Value::Undefined);
                if let Value::Obj(m) = &this {
                    m.borrow_mut().insert("stack".to_string(), val);
                }
                Ok(Value::Undefined)
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
            // get [Symbol.species] — 접근자는 this 를 그대로 돌려준다(종파생 생성자 = 자신).
            Native::SpeciesGet => Ok(recv.unwrap_or(Value::Undefined)),
            Native::FnToString => {
                // §20.2.3.5. [[SourceText]] 가 있으면 원본 소스, 없으면(내장/바운드/합성)
                // NativeFunction 문법: function <name>() { [native code] }.
                let f = recv.clone().unwrap_or(Value::Undefined);
                let s = match &f {
                    Value::Fn(fnc) => match &fnc.source {
                        Some(src) => src.to_string(),
                        None => {
                            // 소스 미보관(클래스 메서드/Function 생성자/합성). name/params
                            // 로 근사 재구성. 화살표는 prototype 없음 등 구분 불필요.
                            let name = fnc.name.borrow().clone();
                            let kw = if fnc.is_async { "async function" } else { "function" };
                            let star = if fnc.is_generator { "*" } else { "" };
                            format!(
                                "{}{} {}({}) {{ }}",
                                kw,
                                star,
                                name,
                                fnc.params.join(", ")
                            )
                        }
                    },
                    Value::Class(c) => match &c.source {
                        Some(src) => src.to_string(),
                        None => {
                            let name = c.name.borrow().clone();
                            if name.is_empty() {
                                "class { }".to_string()
                            } else {
                                format!("class {} {{ }}", name)
                            }
                        }
                    },
                    // 내장/바운드: NativeFunction 문법. 이름이 유효 식별자면 포함.
                    Value::Native(_) | Value::Bound(_) => {
                        let name = self.fn_name_of(&f);
                        let ident_ok = !name.is_empty()
                            && matches!(f, Value::Native(_))
                            && name
                                .chars()
                                .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
                            && !name.chars().next().unwrap().is_ascii_digit();
                        if ident_ok {
                            format!("function {}() {{ [native code] }}", name)
                        } else {
                            "function () { [native code] }".to_string()
                        }
                    }
                    _ => "function () { [native code] }".to_string(),
                };
                Ok(Value::Str(s))
            }
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
                // make_iter_from_vec 로 통일 — @@iterator(스스로 이터러블) +
                // __proto__=%IteratorPrototype%(Iterator 헬퍼 상속)를 함께 단다.
                Ok(self.make_iter_from_vec(items))
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
                    // async 제너레이터(§27.6): next/return/throw 는 Promise 를 돌려준다.
                    // 결과 {value,done} 는 이행 promise 로, 던짐은 거부 promise 로 감싼다.
                    // yield 된 value 가 thenable 이면 표준상 await 되므로 먼저 풀어준다.
                    if Self::gen_is_async(&gs) {
                        let p = self.new_promise();
                        match self.gen_resume(&gs, arg, mode) {
                            // yield 된 value 를 await 하다 거부되면 promise 를 거부해야 한다.
                            // 예전엔 `?` 로 Err 를 함수 밖으로 흘려 next() 가 동기적으로 throw 했다
                            // (거부 promise 를 못 돌려줌 → for await/.catch 가 깨짐).
                            Ok(res) => match self.await_iter_result_value(res) {
                                Ok(res) => self.resolve_promise(&p, res),
                                Err(e) => {
                                    let reason = self.thrown.take().unwrap_or(Value::Str(e));
                                    self.reject_promise(&p, reason);
                                }
                            },
                            Err(e) => {
                                let reason = self.thrown.take().unwrap_or(Value::Str(e));
                                self.reject_promise(&p, reason);
                            }
                        }
                        return Ok(p);
                    }
                    self.gen_resume(&gs, arg, mode)
                } else {
                    Ok(Value::Undefined)
                }
            }
            // Symbol(desc) — 고유 심볼 원시값 생성.
            Native::SymbolCtor => {
                // §20.4.1.1: description = (undefined ? undefined : ToString(desc)).
                // ToString 이라 valueOf/toString 을 호출하고 Symbol 인자는 TypeError.
                let desc = match args.first() {
                    Some(Value::Undefined) | None => None,
                    Some(v) => Some(self.to_string_value(v)?),
                };
                self.sym_counter += 1;
                Ok(Value::Symbol(Rc::new(super::SymbolData {
                    // desc 를 키에 담아 둔다 — getOwnPropertySymbols 가 심볼을 복원할 때
                    // 설명까지 되살릴 수 있어야 한다. 고유성은 카운터가 보장.
                    key: format!("\u{0}@@sym:{}:{}", self.sym_counter, desc.clone().unwrap_or_default()),
                    desc,
                })))
            }
            // Symbol.for(k) — 전역 레지스트리에서 공유 심볼.
            Native::SymbolFor => {
                // §20.4.2.2: stringKey = ToString(key) (valueOf/toString 호출, Symbol TypeError).
                let k = match args.first() {
                    Some(v) => self.to_string_value(v)?,
                    None => "undefined".to_string(),
                };
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
            // §20.4.2.7: 인자가 심볼이 아니면 TypeError.
            Native::SymbolKeyFor => match args.first() {
                Some(Value::Symbol(s)) => Ok(s
                    .key
                    .strip_prefix("\u{0}@@for:")
                    .map(|k| Value::Str(k.to_string()))
                    .unwrap_or(Value::Undefined)),
                _ => Err(self.throw_error("TypeError", "Symbol.keyFor requires a symbol argument")),
            },
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
            // Map()/Set() 를 new 없이 부르면 NewTarget 이 undefined → TypeError
            // (§24.1.1.1 / §24.2.1.1 step 1). 단 파생 클래스의 super() 호출은 NewTarget 이
            // 정의돼 있으므로(§10.2.2) 통과시켜야 한다 — 그땐 construct 처럼 초기화한다.
            Native::MapCtor => {
                if matches!(self.new_target, None | Some(Value::Undefined)) {
                    return Err(self.throw_error("TypeError", "Constructor Map requires 'new'"));
                }
                self.make_map(args)
            }
            Native::SetCtor => {
                if matches!(self.new_target, None | Some(Value::Undefined)) {
                    return Err(self.throw_error("TypeError", "Constructor Set requires 'new'"));
                }
                self.make_set(args)
            }
            Native::ErrorCtor(name) => {
                // Error('m') 은 new Error('m') 과 같다 (§20.5.1.1). message 는 인자가
                // 있을 때만 own 프로퍼티이고 비열거 — 객체 생성은 make_error 한 곳에서만.
                let msg = match args.first() {
                    None | Some(Value::Undefined) => None,
                    Some(v) => Some(to_display(v)),
                };
                let err = self.make_error(name, msg);
                self.install_error_cause(&err, &args, name)?;
                Ok(err)
            }
            Native::Map(op) => {
                // brand 체크: Map 이 아니면 TypeError (§24.1.3, RequireInternalSlot).
                // 예전엔 일반 Err(String)→Error 라 "TypeError 기대" 검사가 깨졌다.
                let Some(Value::MapVal(m)) = recv else {
                    return Err(self.throw_error(
                        "TypeError",
                        "Map.prototype method called on incompatible receiver",
                    ));
                };
                self.map_method(m, op, args)
            }
            Native::Set(op) => {
                let Some(Value::SetVal(s)) = recv else {
                    return Err(self.throw_error(
                        "TypeError",
                        "Set.prototype method called on incompatible receiver",
                    ));
                };
                self.set_method(s, op, args)
            }
            // get Map.prototype.size / Set.prototype.size — brand 체크 후 원소 수.
            Native::MapSize => match recv {
                Some(Value::MapVal(m)) => Ok(Value::Num(m.borrow().len() as f64)),
                _ => Err(self.throw_error(
                    "TypeError",
                    "get Map.prototype.size called on incompatible receiver",
                )),
            },
            Native::SetSize => match recv {
                Some(Value::SetVal(s)) => Ok(Value::Num(s.borrow().len() as f64)),
                _ => Err(self.throw_error(
                    "TypeError",
                    "get Set.prototype.size called on incompatible receiver",
                )),
            },
            // get Symbol.prototype.description — thisSymbolValue(§20.4.3.2). 심볼 원시값이나
            // 심볼 래퍼면 [[Description]](없으면 undefined), 아니면 TypeError.
            Native::SymbolDescGet => {
                let this = recv.unwrap_or(Value::Undefined);
                let sym = match &this {
                    Value::Symbol(s) => Some(s.clone()),
                    _ => match wrapper_primitive(&this) {
                        Some(Value::Symbol(s)) => Some(s),
                        _ => None,
                    },
                };
                match sym {
                    Some(s) => Ok(s.desc.clone().map(Value::Str).unwrap_or(Value::Undefined)),
                    None => Err(self.throw_error(
                        "TypeError",
                        "get Symbol.prototype.description called on incompatible receiver",
                    )),
                }
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
            // Date.UTC(year, month[0기준], day, h, m, s, ms) → 밀리초(UTC). §21.4.3.4
            // 인자를 순서대로 ToNumber(valueOf 관찰, 예외 전파), MakeFullYear/MakeDay/MakeTime.
            Native::DateUTC => {
                let num = |me: &mut Self, i: usize, dflt: f64| -> Result<f64, String> {
                    match args.get(i) {
                        Some(v) => me.to_number_value(v),
                        None => Ok(dflt),
                    }
                };
                // 인자가 하나도 없으면 NaN. year 는 항상 강제변환된다(§21.4.3.4 step 1).
                let y = num(self, 0, f64::NAN)?;
                let mo = num(self, 1, 0.0)?;
                let d = num(self, 2, 1.0)?;
                let h = num(self, 3, 0.0)?;
                let mi = num(self, 4, 0.0)?;
                let s = num(self, 5, 0.0)?;
                let ms = num(self, 6, 0.0)?;
                if args.is_empty() {
                    return Ok(Value::Num(f64::NAN));
                }
                Ok(Value::Num(build_date_full(y, mo, d, h, mi, s, ms)))
            }
            Native::DateCtor => {
                // §21.4.2.2: new 없이 Date(...) 를 부르면 인자를 무시하고 현재 시각의
                // toString 문자열을 낸다 (typeof 는 "string").
                if matches!(self.new_target, None | Some(Value::Undefined)) {
                    return Ok(Value::Str(date_tostring(now_millis())));
                }
                self.make_date_from_args(&args)
            }
            // date.getFullYear() 등 — recv 가 Date 객체
            Native::DateMethod(field) => {
                use DateField::*;
                // ── Date.prototype[Symbol.toPrimitive](hint) (§21.4.4.45) ──
                // 제네릭(임의 객체)이며 OrdinaryToPrimitive 를 쓴다.
                if matches!(field, ToPrimitive) {
                    let recvv = recv.clone().unwrap_or(Value::Undefined);
                    if !matches!(recvv, Value::Obj(_) | Value::Instance(_) | Value::Arr(_)) {
                        return Err(self.throw_error(
                            "TypeError",
                            "Date.prototype[Symbol.toPrimitive] called on non-object",
                        ));
                    }
                    let hint = args.first().cloned().unwrap_or(Value::Undefined);
                    let prefer_string = match &hint {
                        Value::Str(s) if s == "string" || s == "default" => true,
                        Value::Str(s) if s == "number" => false,
                        _ => {
                            return Err(self.throw_error(
                                "TypeError",
                                "invalid hint for Date.prototype[Symbol.toPrimitive]",
                            ))
                        }
                    };
                    // OrdinaryToPrimitive: @@toPrimitive 를 다시 타지 않고 toString/valueOf 직접.
                    let order: [&str; 2] = if prefer_string {
                        ["toString", "valueOf"]
                    } else {
                        ["valueOf", "toString"]
                    };
                    for name in order {
                        let f = self.member_get(&recvv, name)?;
                        if is_callable(&f) {
                            let res = self.call_value(f, Some(recvv.clone()), vec![])?;
                            if !matches!(res, Value::Obj(_) | Value::Instance(_) | Value::Arr(_)) {
                                return Ok(res);
                            }
                        }
                    }
                    return Err(
                        self.throw_error("TypeError", "Cannot convert Date to primitive value")
                    );
                }
                // ── toJSON(key) (§21.4.4.37) ── 제네릭:
                // 1. O = ToObject(this)  2. tv = ToPrimitive(O, number)
                // 3. tv 가 Number 이고 유한하지 않으면 null  4. Invoke(O,"toISOString") 반환.
                if matches!(field, ToJson) {
                    let recvv = recv.clone().unwrap_or(Value::Undefined);
                    if matches!(recvv, Value::Undefined | Value::Null) {
                        return Err(
                            self.throw_error("TypeError", "Date.prototype.toJSON called on null/undefined")
                        );
                    }
                    // ToObject: 원시값은 래퍼로 박싱(toISOString 은 O 에서 찾는다).
                    let o = self.to_object_value(recvv);
                    let tv = self.to_primitive_or_throw(o.clone(), false)?;
                    // null 은 tv 가 "Number 이면서 비유한"일 때만 (문자열/심볼 tv 는 통과).
                    if let Value::Num(n) = &tv {
                        if !n.is_finite() {
                            return Ok(Value::Null);
                        }
                    }
                    let f = self.member_get(&o, "toISOString")?;
                    if !is_callable(&f) {
                        return Err(self.throw_error("TypeError", "toISOString is not callable"));
                    }
                    return self.call_value(f, Some(o), vec![]);
                }
                // ── 그 외 메서드: this 가 [[DateValue]] 를 가진 Date 여야 한다 (§21.4.4) ──
                let millis = match &recv {
                    Some(Value::Obj(m)) if is_date_obj(m) => match m.borrow().get("\u{0}time") {
                        Some(Value::Num(n)) => *n,
                        _ => f64::NAN,
                    },
                    _ => {
                        return Err(
                            self.throw_error("TypeError", "this is not a Date object")
                        )
                    }
                };
                // UTC 변형 → 로컬 변형으로 정규화(이 엔진은 오프셋 0).
                let cfield = match field {
                    UtcFullYear => FullYear,
                    UtcMonth => Month,
                    UtcDate => Date,
                    UtcDay => Day,
                    UtcHours => Hours,
                    UtcMinutes => Minutes,
                    UtcSeconds => Seconds,
                    UtcMs => Ms,
                    SetUtcFullYear => SetFullYear,
                    SetUtcMonth => SetMonth,
                    SetUtcDate => SetDate,
                    SetUtcHours => SetHours,
                    SetUtcMinutes => SetMinutes,
                    SetUtcSeconds => SetSeconds,
                    SetUtcMs => SetMs,
                    other => other,
                };
                // ── setter 계열 ──
                // 표준상 참조하는 인자를 순서대로 ToNumber(valueOf 관찰) 한 뒤 조립한다.
                // 첫 인자는 항상, 나머지는 present(위치 전달)일 때만. NaN 처리는 §21.4.4.
                let param_count = match cfield {
                    SetTime => 1,
                    SetFullYear => 3,
                    SetMonth => 2,
                    SetDate => 1,
                    SetHours => 4,
                    SetMinutes => 3,
                    SetSeconds => 2,
                    SetMs => 1,
                    SetYear => 1,
                    _ => 0,
                };
                if param_count > 0 {
                    let argc = args.len();
                    let count = param_count.min(argc.max(1));
                    let mut nums: Vec<f64> = Vec::with_capacity(count);
                    for i in 0..count {
                        let v = args.get(i).cloned().unwrap_or(Value::Undefined);
                        // ToNumber: valueOf/@@toPrimitive 관찰 + Symbol/BigInt 은 TypeError.
                        nums.push(self.to_number_value(&v)?);
                    }
                    let a = |i: usize| -> Option<f64> {
                        if i == 0 {
                            nums.first().copied()
                        } else if i < argc {
                            nums.get(i).copied()
                        } else {
                            None
                        }
                    };
                    let t_nan = millis.is_nan();
                    // SetFullYear/SetYear 는 t 가 NaN 이면 시간 성분을 +0 기준으로 잡는다.
                    let base = if matches!(cfield, SetFullYear | SetYear) && t_nan {
                        0.0
                    } else {
                        millis
                    };
                    // 현재 필드를 f64 로 (month 은 0기준). MakeDay/MakeTime 로 조립해 NaN/
                    // Infinity/오버플로가 그대로 무효 날짜(NaN)로 전파되게 한다.
                    let (yi, mo1, di, hi, mii, si, msi, _) = date_parts(base);
                    let (cy, cmo0, cd, ch, cmi, cs, cms) = (
                        yi as f64,
                        mo1 as f64 - 1.0,
                        di as f64,
                        hi as f64,
                        mii as f64,
                        si as f64,
                        msi as f64,
                    );
                    // setter 는 MakeFullYear(1900+) 를 적용하지 않는다(연도를 그대로 쓴다).
                    let build = |yr: f64, mo0: f64, dd: f64, hh: f64, mm: f64, ss: f64, mss: f64| {
                        time_clip(make_date_ms(make_day(yr, mo0, dd), make_time(hh, mm, ss, mss)))
                    };
                    let new_millis = match cfield {
                        SetTime => time_clip(a(0).unwrap_or(f64::NAN)),
                        SetFullYear => build(
                            a(0).unwrap_or(f64::NAN),
                            a(1).unwrap_or(cmo0),
                            a(2).unwrap_or(cd),
                            ch,
                            cmi,
                            cs,
                            cms,
                        ),
                        SetYear => {
                            // Annex B §B.2.4.2: 0..99 → 1900+y. NaN → NaN.
                            let yv = a(0).unwrap_or(f64::NAN);
                            if yv.is_nan() {
                                f64::NAN
                            } else {
                                let yt = yv.trunc();
                                let full = if (0.0..=99.0).contains(&yt) { 1900.0 + yt } else { yt };
                                build(full, cmo0, cd, ch, cmi, cs, cms)
                            }
                        }
                        // 나머지 setter 는 t 가 NaN 이면(인자 강제 후) NaN 반환.
                        _ if t_nan => f64::NAN,
                        SetMonth => build(cy, a(0).unwrap_or(cmo0), a(1).unwrap_or(cd), ch, cmi, cs, cms),
                        SetDate => build(cy, cmo0, a(0).unwrap_or(cd), ch, cmi, cs, cms),
                        SetHours => build(
                            cy,
                            cmo0,
                            cd,
                            a(0).unwrap_or(ch),
                            a(1).unwrap_or(cmi),
                            a(2).unwrap_or(cs),
                            a(3).unwrap_or(cms),
                        ),
                        SetMinutes => build(
                            cy,
                            cmo0,
                            cd,
                            ch,
                            a(0).unwrap_or(cmi),
                            a(1).unwrap_or(cs),
                            a(2).unwrap_or(cms),
                        ),
                        SetSeconds => build(cy, cmo0, cd, ch, cmi, a(0).unwrap_or(cs), a(1).unwrap_or(cms)),
                        SetMs => build(cy, cmo0, cd, ch, cmi, cs, a(0).unwrap_or(cms)),
                        _ => f64::NAN,
                    };
                    // t(강제 전에 읽은 thisTimeValue)가 NaN 인 성분 세터는 NaN 을 반환하되
                    // [[DateValue]] 를 쓰지 않는다 — 인자 valueOf 안에서 setTime 등으로 바뀐
                    // 값이 그대로 남아야 한다(date-value-read-before-tonumber 테스트).
                    // SetTime/SetFullYear/SetYear 는 t 와 무관하게 항상 쓴다.
                    let skip_write = t_nan
                        && matches!(
                            cfield,
                            SetMonth
                                | SetDate
                                | SetHours
                                | SetMinutes
                                | SetSeconds
                                | SetMs
                        );
                    if !skip_write {
                        if let Some(Value::Obj(m)) = &recv {
                            m.borrow_mut()
                                .insert("\u{0}time".to_string(), Value::Num(new_millis));
                        }
                    }
                    return Ok(Value::Num(new_millis));
                }
                // ── getter / 문자열 / annexB getYear ──
                let (y, mo, d, h, mi, s, ms, wd) = date_parts(millis);
                let nan = millis.is_nan();
                let num = |v: f64| if nan { Value::Num(f64::NAN) } else { Value::Num(v) };
                Ok(match cfield {
                    Time => Value::Num(millis),
                    FullYear => num(y as f64),
                    Month => num((mo - 1) as f64), // JS 는 0 기준
                    Date => num(d as f64),
                    Day => num(wd as f64),
                    Hours => num(h as f64),
                    Minutes => num(mi as f64),
                    Seconds => num(s as f64),
                    Ms => num(ms as f64),
                    TimezoneOffset => num(0.0),
                    GetYear => num((y - 1900) as f64), // Annex B §B.2.4.1
                    ToIso => {
                        if !millis.is_finite() {
                            return Err(self.throw_error("RangeError", "Invalid time value"));
                        }
                        Value::Str(date_iso(millis))
                    }
                    ToStr => Value::Str(date_tostring(millis)),
                    ToDateStr => Value::Str(date_datestring(millis)),
                    ToTimeStr => Value::Str(date_timestring(millis)),
                    ToUtcStr => Value::Str(date_utcstring(millis)),
                    // Intl 미탑재 — impl-defined 로 toString 계열과 동일하게 낸다.
                    ToLocaleStr => Value::Str(date_tostring(millis)),
                    ToLocaleDateStr => Value::Str(date_datestring(millis)),
                    ToLocaleTimeStr => Value::Str(date_timestring(millis)),
                    _ => Value::Undefined,
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
            // §21.1.1.1 Number(value): ToNumeric — BigInt 는 수치로 변환, Symbol 은 TypeError,
            // 객체는 valueOf 관측. 예전엔 to_num 이라 valueOf 미호출·Symbol NaN 이었다.
            Native::NumberCtor => Ok(Value::Num(match args.first() {
                Some(Value::BigInt(b)) => b.to_f64(),
                Some(v) => self.to_number_value(v)?,
                None => 0.0,
            })),
            Native::BooleanCtor => Ok(Value::Bool(args.first().map(to_bool).unwrap_or(false))),
            Native::StrFromCharCode => {
                // §22.1.2.1: 각 인자 ToUint16 → UTF-16 코드 유닛(16비트 마스크). 로운
                // 서로게이트는 lossy(우리 문자열은 UTF-8). 예전엔 to_num→code point 라
                // fromCharCode(65601) 이 65 이 아니라 서로게이트가 됐다.
                let mut units: Vec<u16> = Vec::with_capacity(args.len());
                for a in &args {
                    let n = self.to_number_value(a)?;
                    units.push(to_uint16(n));
                }
                Ok(Value::Str(String::from_utf16_lossy(&units)))
            }
            Native::StrFromCodePoint => {
                // §22.1.2.2: 각 인자 ToNumber 후 정수 & [0, 0x10FFFF] 아니면 RangeError.
                let mut s = String::new();
                for a in &args {
                    let n = self.to_number_value(a)?;
                    if !n.is_finite() || n.trunc() != n || n < 0.0 || n > 0x10FFFF as f64 {
                        return Err(self.throw_error("RangeError", "Invalid code point"));
                    }
                    let cp = n as u32;
                    if cp <= 0xFFFF {
                        s.push_str(&String::from_utf16_lossy(&[cp as u16]));
                    } else {
                        s.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                    }
                }
                Ok(Value::Str(s))
            }
            // String.raw(template, ...subs) (§22.1.2.4): template.raw 의 각 세그먼트를
            // 치환값과 번갈아 잇는다. 태그된 템플릿의 원시 문자열용.
            Native::StrRaw => {
                // §22.1.2.4: cooked=ToObject(template); raw=ToObject(Get(cooked,"raw"));
                // literalSegments=ToLength(Get(raw,"length")); 각 세그먼트/치환 ToString(예외 전파).
                let template = args.first().cloned().unwrap_or(Value::Undefined);
                if matches!(template, Value::Undefined | Value::Null) {
                    return Err(self.throw_error(
                        "TypeError",
                        "Cannot convert undefined or null to object",
                    ));
                }
                let raw_val = self.member_get(&template, "raw")?;
                if matches!(raw_val, Value::Undefined | Value::Null) {
                    return Err(self.throw_error(
                        "TypeError",
                        "Cannot convert undefined or null to object",
                    ));
                }
                let raw = self.to_object_value(raw_val);
                let len_val = self.member_get(&raw, "length")?;
                let len = to_length(self.to_integer_or_infinity(&len_val)?) as usize;
                if len == 0 {
                    return Ok(Value::Str(String::new()));
                }
                let subs: &[Value] = if args.len() > 1 { &args[1..] } else { &[] };
                let mut out = String::new();
                for i in 0..len {
                    let seg = self.member_get(&raw, &i.to_string())?;
                    out.push_str(&self.to_string_value(&seg)?);
                    if i + 1 == len {
                        break;
                    }
                    if let Some(sub) = subs.get(i) {
                        out.push_str(&self.to_string_value(sub)?);
                    }
                }
                Ok(Value::Str(out))
            }
            Native::NumIsInteger => {
                let ok = matches!(args.first(), Some(Value::Num(n)) if n.fract() == 0.0 && n.is_finite());
                Ok(Value::Bool(ok))
            }
            // §21.1.2.5: 정수이면서 |n| ≤ 2^53-1 (안전 정수).
            Native::NumIsSafeInteger => {
                let ok = matches!(args.first(),
                    Some(Value::Num(n)) if n.is_finite() && n.fract() == 0.0 && n.abs() <= 9007199254740991.0);
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
                // §21.1.3.3: thisNumberValue, f = ToIntegerOrInfinity(digits) 로 0..100 검사,
                // NaN→"NaN", |x|>=1e21 는 ToString(x).
                // §21.1.3.x step 1: thisNumberValue — 숫자/Number 래퍼가 아니면 TypeError.
                let this = recv.clone().unwrap_or(Value::Undefined);
                let n = match self.this_prim_value(&this, PrimBrand::Number)? {
                    Value::Num(n) => n,
                    _ => f64::NAN,
                };
                // f 는 ToIntegerOrInfinity: NaN/문자열→0, Symbol/BigInt→TypeError, ±∞ 유지.
                // 예전엔 to_num 뒤 !is_finite 를 RangeError 로 처리해 toFixed(NaN)/toFixed("x")
                // 가 잘못 던졌다(표준은 0 으로 취급).
                let arg = args.first().cloned().unwrap_or(Value::Undefined);
                let f = self.to_integer_or_infinity(&arg)?;
                if f < 0.0 || f > 100.0 {
                    return Err(self.throw_error(
                        "RangeError",
                        "toFixed() digits argument must be between 0 and 100",
                    ));
                }
                if n.is_nan() {
                    return Ok(Value::Str("NaN".to_string()));
                }
                if !n.is_finite() {
                    return Ok(Value::Str(if n < 0.0 { "-Infinity" } else { "Infinity" }.to_string()));
                }
                if n.abs() >= 1e21 {
                    return Ok(Value::Str(num_to_str(n)));
                }
                // §21.1.3.3 step: 부호는 x<0 일 때만 붙는다. -0 은 x<0 이 거짓이라 부호 없음
                // ("0.00"). Rust 포매터는 -0.0 의 부호비트를 보존하므로 여기서 정규화한다.
                // (-0.001 등은 x<0 이 참이라 정상적으로 "-0.00" — 건드리지 않는다.)
                let n = if n == 0.0 { 0.0 } else { n };
                Ok(Value::Str(format!("{:.*}", f as usize, n)))
            }
            // Number.prototype.toExponential (§21.1.3.2)
            Native::NumToExponential => {
                // §21.1.3.x step 1: thisNumberValue — 숫자/Number 래퍼가 아니면 TypeError.
                let this = recv.clone().unwrap_or(Value::Undefined);
                let n = match self.this_prim_value(&this, PrimBrand::Number)? {
                    Value::Num(n) => n,
                    _ => f64::NAN,
                };
                let frac_undef = matches!(args.first(), None | Some(Value::Undefined));
                // §21.1.3.2 step 2: f = ToIntegerOrInfinity(fractionDigits) — undefined 여도
                // 수행(Symbol/BigInt→TypeError, NaN/문자열→0). undefined 면 f=0 이자 최소자릿수.
                // 예전엔 to_num 뒤 !is_finite 를 RangeError 로 처리해 toExponential(NaN) 가 잘못
                // 던지고 Symbol/BigInt 도 RangeError/성공으로 흘렀다.
                let arg = args.first().cloned().unwrap_or(Value::Undefined);
                let f = self.to_integer_or_infinity(&arg)?;
                if n.is_nan() {
                    return Ok(Value::Str("NaN".to_string()));
                }
                if !n.is_finite() {
                    return Ok(Value::Str(if n < 0.0 { "-Infinity" } else { "Infinity" }.to_string()));
                }
                // f=0(undefined 포함)은 통과, ±∞ 는 RangeError.
                if f < 0.0 || f > 100.0 {
                    return Err(self.throw_error(
                        "RangeError",
                        "toExponential() argument must be between 0 and 100",
                    ));
                }
                Ok(Value::Str(num_to_exponential(
                    n,
                    if frac_undef { None } else { Some(f as usize) },
                )))
            }
            // Number.prototype.toPrecision (§21.1.3.5)
            Native::NumToPrecision => {
                // §21.1.3.x step 1: thisNumberValue — 숫자/Number 래퍼가 아니면 TypeError.
                let this = recv.clone().unwrap_or(Value::Undefined);
                let n = match self.this_prim_value(&this, PrimBrand::Number)? {
                    Value::Num(n) => n,
                    _ => f64::NAN,
                };
                // step 2: precision 이 undefined 면 강제변환 전에 ToString.
                if matches!(args.first(), None | Some(Value::Undefined)) {
                    return Ok(Value::Str(num_to_str(n)));
                }
                // step 3: p = ToIntegerOrInfinity(precision) (Symbol/BigInt→TypeError, NaN→0).
                let p = self.to_integer_or_infinity(&args[0])?;
                if n.is_nan() {
                    return Ok(Value::Str("NaN".to_string()));
                }
                if !n.is_finite() {
                    return Ok(Value::Str(if n < 0.0 { "-Infinity" } else { "Infinity" }.to_string()));
                }
                if p < 1.0 || p > 100.0 {
                    return Err(self.throw_error(
                        "RangeError",
                        "toPrecision() argument must be between 1 and 100",
                    ));
                }
                Ok(Value::Str(num_to_precision(n, p as usize)))
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
                // §22.2.6.16: this 가 정규식이 아니면 TypeError, 인자는 ToString.
                let (src, flags) = match recv.as_ref().and_then(regex_src_flags) {
                    Some(sf) => sf,
                    None => {
                        return Err(self.throw_error(
                            "TypeError",
                            "Method RegExp.prototype.test called on incompatible receiver",
                        ))
                    }
                };
                let text =
                    self.to_string_value(&args.first().cloned().unwrap_or(Value::Undefined))?;
                let re = crate::js::regex::Regex::compile_pattern(&src, &flags)
                    .map_err(|e| format!("SyntaxError: Invalid regular expression: /{}/: {}", src, e))?;
                let chars: Vec<char> = text.chars().collect();
                Ok(Value::Bool(re.find(&chars, 0).is_some()))
            }
            // regex.exec(str) → [full, g1, ...] with .index, or null. global 이면 lastIndex 갱신.
            Native::RegexExec => {
                // §22.2.6.8: this 가 정규식이 아니면 TypeError, 인자는 ToString.
                let recv_obj = recv.clone();
                let (src, flags) = match recv.as_ref().and_then(regex_src_flags) {
                    Some(sf) => sf,
                    None => {
                        return Err(self.throw_error(
                            "TypeError",
                            "Method RegExp.prototype.exec called on incompatible receiver",
                        ))
                    }
                };
                let text =
                    self.to_string_value(&args.first().cloned().unwrap_or(Value::Undefined))?;
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
                    let (parent, child) = (parent, *child);
                    let reference = match args.get(1) {
                        Some(Value::Dom(r)) => Some(*r),
                        _ => None,
                    };
                    // §4.2.3: 순환/잘못된 reference 는 DOMException.
                    self.ensure_pre_insert_valid(parent, child, reference)?;
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
            // node.getRootNode() — 트리의 루트. 연결된 노드면 document, 아니면 최상위 조상.
            Native::DomGetRootNode => match recv {
                Some(Value::Dom(id)) => {
                    let (root, doc_root) = {
                        let dom = self.dom_arena()?;
                        // ancestors 는 [부모..루트](자기 제외). 조상 없으면 자기 자신이 루트.
                        let root = dom.ancestors(id).last().copied().unwrap_or(id);
                        (root, dom.root)
                    };
                    if root == doc_root {
                        Ok(env_get(&self.global, "document").unwrap_or(Value::Dom(root)))
                    } else {
                        Ok(Value::Dom(root))
                    }
                }
                _ => Ok(Value::Undefined),
            },
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
                    let (parent, child) = (parent, *child);
                    // §4.2.3: node 가 parent 의 조상(자기 포함)이면 순환 → HierarchyRequestError.
                    self.ensure_pre_insert_valid(parent, child, None)?;
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
                // 가변 인자(min/max/hypot)는 모든 인자를, 이항(pow/atan2)은 앞 둘을, 단항은 첫
                // 인자를 순서대로 ToNumber 한다 (valueOf/@@toPrimitive 관찰 + 예외 전파).
                // 단항 op 에 남는 인자는 강제변환하지 않는다(표준).
                if matches!(op, MathOp::Min | MathOp::Max | MathOp::Hypot) {
                    let mut ns = Vec::with_capacity(args.len());
                    for x in &args {
                        ns.push(self.to_number_value(x)?);
                    }
                    return Ok(Value::Num(match op {
                        MathOp::Min => ns.iter().fold(f64::INFINITY, |acc, &x| {
                            if acc.is_nan() || x.is_nan() { f64::NAN } else { acc.min(x) }
                        }),
                        MathOp::Max => ns.iter().fold(f64::NEG_INFINITY, |acc, &x| {
                            if acc.is_nan() || x.is_nan() { f64::NAN } else { acc.max(x) }
                        }),
                        _ => ns.iter().fold(0.0f64, |acc, &x| acc.hypot(x)),
                    }));
                }
                let a = self.math_arg(&args, 0)?;
                if matches!(op, MathOp::Pow | MathOp::Atan2) {
                    let b = self.math_arg(&args, 1)?;
                    return Ok(Value::Num(match op {
                        MathOp::Pow => math_pow(a, b),
                        _ => a.atan2(b),
                    }));
                }
                Ok(Value::Num(match op {
                    MathOp::Floor => a.floor(),
                    MathOp::Ceil => a.ceil(),
                    // JS Math.round: +∞ 방향 반올림. [-0.5,0) 과 -0 은 -0 을 유지한다.
                    MathOp::Round => {
                        if !a.is_finite() || a == 0.0 {
                            a
                        } else if a >= -0.5 && a < 0.5 {
                            if a.is_sign_negative() { -0.0 } else { 0.0 }
                        } else {
                            (a + 0.5).floor()
                        }
                    }
                    MathOp::Abs => a.abs(),
                    MathOp::Sqrt => a.sqrt(),
                    MathOp::Pow => unreachable!(),
                    MathOp::Min => unreachable!(),
                    MathOp::Max => unreachable!(),
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
                    // ES2015 (§21.3.2)
                    MathOp::Clz32 => (to_i32(args.first().unwrap_or(&Value::Undefined)) as u32)
                        .leading_zeros() as f64,
                    MathOp::Expm1 => a.exp_m1(),
                    MathOp::Log1p => a.ln_1p(),
                    MathOp::Sinh => a.sinh(),
                    MathOp::Cosh => a.cosh(),
                    MathOp::Tanh => a.tanh(),
                    MathOp::Asinh => a.asinh(),
                    MathOp::Acosh => a.acosh(),
                    MathOp::Atanh => a.atanh(),
                    MathOp::Fround => a as f32 as f64,
                    MathOp::Imul => {
                        let x = to_i32(args.first().unwrap_or(&Value::Undefined));
                        let y = to_i32(args.get(1).unwrap_or(&Value::Undefined));
                        x.wrapping_mul(y) as f64
                    }
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
                Ok(match op {
                    StrOp::Upper => Value::Str(s.to_uppercase()),
                    StrOp::Lower => Value::Str(s.to_lowercase()),
                    // Intl 없으면 로케일 독립(= toUpperCase/toLowerCase). §22.1.3.24/.25.
                    StrOp::LocaleUpper => Value::Str(s.to_uppercase()),
                    StrOp::LocaleLower => Value::Str(s.to_lowercase()),
                    StrOp::Trim => {
                        Value::Str(s.trim_matches(is_js_ws).to_string())
                    }
                    StrOp::CharAt => {
                        // UTF-16 코드 유닛 하나(범위 밖은 ""). pos=ToIntegerOrInfinity.
                        let arg = args.first().cloned().unwrap_or(Value::Undefined);
                        let i = self.to_integer_or_infinity(&arg)?;
                        let out = if i >= 0.0 && (i as usize) < units.len() {
                            String::from_utf16_lossy(&units[i as usize..i as usize + 1])
                        } else {
                            String::new()
                        };
                        Value::Str(out)
                    }
                    StrOp::IndexOf => {
                        // §22.1.3.9: searchString=ToString, fromIndex=ToIntegerOrInfinity.
                        let ndl_s = self.to_string_value(args.first().unwrap_or(&Value::Undefined))?;
                        let from = match args.get(1) {
                            None | Some(Value::Undefined) => 0.0,
                            Some(v) => self.to_integer_or_infinity(v)?,
                        };
                        let ndl: Vec<u16> = ndl_s.encode_utf16().collect();
                        let from = from.max(0.0).min(units.len() as f64) as usize;
                        Value::Num(utf16_index_of(&units, &ndl, from).map(|i| i as f64).unwrap_or(-1.0))
                    }
                    StrOp::LastIndexOf => {
                        let ndl_s = self.to_string_value(args.first().unwrap_or(&Value::Undefined))?;
                        let ndl: Vec<u16> = ndl_s.encode_utf16().collect();
                        Value::Num(utf16_last_index_of(&units, &ndl).map(|i| i as f64).unwrap_or(-1.0))
                    }
                    // includes/startsWith/endsWith 는 정규식 인자를 거부한다 (§22.1.3.7/.8/.23:
                    // IsRegExp(searchString) 이면 TypeError). 예전엔 정규식을 문자열화해 통과시켰다.
                    StrOp::Includes => {
                        // §22.1.3.8: IsRegExp → ToString(search) → ToIntegerOrInfinity(position).
                        let arg0 = args.first().cloned().unwrap_or(Value::Undefined);
                        if self.is_regexp_p(&arg0)? {
                            return Err(self.throw_error(
                                "TypeError",
                                "First argument to String.prototype.includes must not be a regular expression",
                            ));
                        }
                        let search = self.to_string_value(&arg0)?;
                        let pos = self.to_integer_or_infinity(
                            args.get(1).unwrap_or(&Value::Undefined))?;
                        let schars: Vec<char> = s.chars().collect();
                        let start = (pos.max(0.0).min(schars.len() as f64)) as usize;
                        let hay: String = schars[start..].iter().collect();
                        Value::Bool(hay.contains(&search))
                    }
                    StrOp::StartsWith => {
                        // §22.1.3.22: position 부터 시작하는지.
                        let arg0 = args.first().cloned().unwrap_or(Value::Undefined);
                        if self.is_regexp_p(&arg0)? {
                            return Err(self.throw_error(
                                "TypeError",
                                "First argument to String.prototype.startsWith must not be a regular expression",
                            ));
                        }
                        let search = self.to_string_value(&arg0)?;
                        let pos = self.to_integer_or_infinity(
                            args.get(1).unwrap_or(&Value::Undefined))?;
                        let schars: Vec<char> = s.chars().collect();
                        let start = (pos.max(0.0).min(schars.len() as f64)) as usize;
                        let tail: String = schars[start..].iter().collect();
                        Value::Bool(tail.starts_with(&search))
                    }
                    StrOp::EndsWith => {
                        // §22.1.3.7: endPosition(기본 길이)까지의 부분이 search 로 끝나는지.
                        let arg0 = args.first().cloned().unwrap_or(Value::Undefined);
                        if self.is_regexp_p(&arg0)? {
                            return Err(self.throw_error(
                                "TypeError",
                                "First argument to String.prototype.endsWith must not be a regular expression",
                            ));
                        }
                        let search = self.to_string_value(&arg0)?;
                        let schars: Vec<char> = s.chars().collect();
                        let len = schars.len();
                        let end_pos = match args.get(1) {
                            None | Some(Value::Undefined) => len as f64,
                            Some(v) => self.to_integer_or_infinity(v)?,
                        };
                        let end = (end_pos.max(0.0).min(len as f64)) as usize;
                        let head: String = schars[..end].iter().collect();
                        Value::Bool(head.ends_with(&search))
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
                        let (src, flags) = match args.first().and_then(regex_src_flags) {
                            Some(sf) => sf,
                            None => {
                                // RegExpCreate(§22.2.3.1): 인자를 정규식 '패턴'으로 컴파일한다
                                // (이스케이프 안 함). undefined → 빈 패턴("").
                                let pat = match args.first() {
                                    None | Some(Value::Undefined) => String::new(),
                                    Some(v) => self.to_string_value(v)?,
                                };
                                (pat, String::new())
                            }
                        };
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
                        let (src, flags) = match args.first().and_then(regex_src_flags) {
                            Some(sf) => sf,
                            None => {
                                // RegExpCreate(§22.2.3.1): 인자를 정규식 '패턴'으로 컴파일한다
                                // (이스케이프 안 함). undefined → 빈 패턴("").
                                let pat = match args.first() {
                                    None | Some(Value::Undefined) => String::new(),
                                    Some(v) => self.to_string_value(v)?,
                                };
                                (pat, String::new())
                            }
                        };
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
                        let (src, flags) = match args.first().and_then(regex_src_flags) {
                            Some(sf) => sf,
                            None => {
                                // RegExpCreate(§22.2.3.1): 인자를 정규식 '패턴'으로 컴파일한다
                                // (이스케이프 안 함). undefined → 빈 패턴("").
                                let pat = match args.first() {
                                    None | Some(Value::Undefined) => String::new(),
                                    Some(v) => self.to_string_value(v)?,
                                };
                                (pat, String::new())
                            }
                        };
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
                        // §22.1.3.20: start/end=ToIntegerOrInfinity(음수는 끝에서, ±∞ 처리).
                        let len = units.len() as f64;
                        let start_f = match args.first() {
                            Some(v) if !matches!(v, Value::Undefined) => {
                                self.to_integer_or_infinity(v)?
                            }
                            _ => 0.0,
                        };
                        let end_f = match args.get(1) {
                            Some(v) if !matches!(v, Value::Undefined) => {
                                self.to_integer_or_infinity(v)?
                            }
                            _ => len,
                        };
                        let rel = |v: f64| {
                            (if v < 0.0 { (len + v).max(0.0) } else { v.min(len) }) as usize
                        };
                        let start = rel(start_f);
                        let end = rel(end_f);
                        Value::Str(String::from_utf16_lossy(&units[start..end.max(start)]))
                    }
                    // substring (§22.1.3.24): ToIntegerOrInfinity → [0,len] 클램프, start>end 면 교환.
                    StrOp::Substring => {
                        let len = units.len() as f64;
                        let start_v = self
                            .to_integer_or_infinity(&args.first().cloned().unwrap_or(Value::Undefined))?;
                        let end_v = match args.get(1) {
                            None | Some(Value::Undefined) => len,
                            Some(v) => self.to_integer_or_infinity(v)?,
                        };
                        let clamp = |v: f64| v.max(0.0).min(len) as usize;
                        let s = clamp(start_v);
                        let e = clamp(end_v);
                        let (from, to) = if s <= e { (s, e) } else { (e, s) };
                        Value::Str(String::from_utf16_lossy(&units[from..to]))
                    }
                    // substr (Annex B §B.2.3.1): substr(start, length). 음수 start 는 len+start.
                    StrOp::Substr => {
                        let size = units.len() as isize;
                        let start_v = self
                            .to_integer_or_infinity(&args.first().cloned().unwrap_or(Value::Undefined))?;
                        let mut start = if start_v.is_infinite() {
                            if start_v < 0.0 { 0 } else { size }
                        } else {
                            start_v as isize
                        };
                        if start < 0 {
                            start = (size + start).max(0);
                        }
                        let length = match args.get(1) {
                            None | Some(Value::Undefined) => size,
                            Some(v) => {
                                let x = self.to_integer_or_infinity(v)?;
                                if x.is_infinite() {
                                    if x < 0.0 { 0 } else { size }
                                } else {
                                    x as isize
                                }
                            }
                        };
                        if start >= size || length <= 0 {
                            Value::Str(String::new())
                        } else {
                            let result_len = length.min(size - start);
                            Value::Str(String::from_utf16_lossy(
                                &units[start as usize..(start + result_len) as usize],
                            ))
                        }
                    }
                    StrOp::Split => {
                        // §22.1.3.21: lim=ToUint32(limit)(undefined→2^32-1)를 separator ToString
                        // '전에' 구한다. lim==0 → [], separator undefined → [S].
                        let sep_val = args.first().cloned().unwrap_or(Value::Undefined);
                        let lim = match args.get(1) {
                            None | Some(Value::Undefined) => u32::MAX as usize,
                            // ToUint32: ToInt32 와 비트패턴 동일(top-bit 해석만 다름).
                            Some(v) => self.to_int32(v)? as u32 as usize,
                        };
                        if lim == 0 {
                            Value::Arr(ArrayObj::new(Vec::new()))
                        } else if matches!(sep_val, Value::Undefined) {
                            Value::Arr(ArrayObj::new(vec![Value::Str(s.clone())]))
                        } else if let Some((src, flags)) = regex_src_flags(&sep_val) {
                            // 정규식 split (§22.2.6.14): 앵커(sticky) 매치로 분할, 캡처 그룹 포함,
                            // 빈 매치 처리, lim 적용.
                            let re = crate::js::regex::Regex::compile_pattern(&src, &flags)
                                .map_err(|e| format!("정규식: {}", e))?;
                            let size = chars.len();
                            let mut parts: Vec<Value> = Vec::new();
                            if size == 0 {
                                // 빈 문자열: 정규식이 빈 문자열에 매치하면 [], 아니면 [""].
                                if re.find(&chars, 0).filter(|mt| mt.start == 0).is_none() {
                                    parts.push(Value::Str(String::new()));
                                }
                            } else {
                                let mut p = 0usize; // 마지막 분할 끝
                                let mut q = 0usize; // 스캔 위치
                                while q < size {
                                    match re.find(&chars, q) {
                                        None => break,
                                        Some(mt) => {
                                            q = mt.start; // 그 앞엔 매치 없음 → 점프
                                            let e = mt.end.min(size);
                                            if e == p {
                                                q += 1; // 빈 매치가 분할 끝과 겹침 → 전진
                                            } else {
                                                parts.push(Value::Str(
                                                    chars[p..mt.start].iter().collect(),
                                                ));
                                                if parts.len() >= lim {
                                                    break;
                                                }
                                                let mut hit_lim = false;
                                                for g in mt.groups.iter().skip(1) {
                                                    parts.push(match g {
                                                        Some((a, b)) => Value::Str(
                                                            chars[*a..*b].iter().collect(),
                                                        ),
                                                        None => Value::Undefined,
                                                    });
                                                    if parts.len() >= lim {
                                                        hit_lim = true;
                                                        break;
                                                    }
                                                }
                                                if hit_lim {
                                                    break;
                                                }
                                                p = e;
                                                q = if e > mt.start { e } else { e + 1 };
                                            }
                                        }
                                    }
                                }
                                if parts.len() < lim {
                                    parts.push(Value::Str(chars[p..].iter().collect()));
                                }
                            }
                            parts.truncate(lim);
                            Value::Arr(ArrayObj::new(parts))
                        } else {
                            let sep = self.to_string_value(&sep_val)?;
                            let mut parts: Vec<Value> = if sep.is_empty() {
                                chars.iter().map(|c| Value::Str(c.to_string())).collect()
                            } else {
                                s.split(&sep).map(|p| Value::Str(p.to_string())).collect()
                            };
                            parts.truncate(lim);
                            Value::Arr(ArrayObj::new(parts))
                        }
                    }
                    StrOp::TrimStart => {
                        Value::Str(s.trim_start_matches(is_js_ws).to_string())
                    }
                    StrOp::TrimEnd => {
                        Value::Str(s.trim_end_matches(is_js_ws).to_string())
                    }
                    StrOp::Repeat => {
                        // §22.1.3.18: count=ToIntegerOrInfinity, 음수/∞ 는 RangeError.
                        let arg = args.first().cloned().unwrap_or(Value::Undefined);
                        let n = self.to_integer_or_infinity(&arg)?;
                        if n < 0.0 || n.is_infinite() {
                            return Err(self.throw_error("RangeError", "Invalid count value"));
                        }
                        Value::Str(s.repeat(n as usize))
                    }
                    StrOp::PadStart | StrOp::PadEnd => {
                        // targetLength=ToLength, fillString=ToString(undefined→" ").
                        let arg = args.first().cloned().unwrap_or(Value::Undefined);
                        let target = to_length(self.to_integer_or_infinity(&arg)?) as usize;
                        let pad = match args.get(1) {
                            Some(v) if !matches!(v, Value::Undefined) => self.to_string_value(v)?,
                            _ => " ".to_string(),
                        };
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
                        // i번째 UTF-16 코드 유닛(u16). pos=ToIntegerOrInfinity, 범위 밖 NaN.
                        let arg = args.first().cloned().unwrap_or(Value::Undefined);
                        let i = self.to_integer_or_infinity(&arg)?;
                        if i < 0.0 || !i.is_finite() {
                            Value::Num(f64::NAN)
                        } else {
                            match units.get(i as usize) {
                                Some(u) => Value::Num(*u as f64),
                                None => Value::Num(f64::NAN),
                            }
                        }
                    }
                    StrOp::CodePointAt => {
                        // i번째 UTF-16 위치의 코드 포인트(서로게이트쌍 결합). 범위 밖 undefined.
                        let arg = args.first().cloned().unwrap_or(Value::Undefined);
                        let i = self.to_integer_or_infinity(&arg)?;
                        if i < 0.0 || !i.is_finite() {
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
                        // §22.1.3.5: 각 인자를 ToString.
                        let mut out = s.clone();
                        for i in 0..args.len() {
                            let piece = self.to_string_value(&args[i])?;
                            out.push_str(&piece);
                        }
                        Value::Str(out)
                    }
                    StrOp::LocaleCompare => {
                        // 로케일 콜레이션 근사(코드포인트 순서). -1/0/1.
                        let other = self.to_string_value(args.first().unwrap_or(&Value::Undefined))?;
                        Value::Num(match s.as_str().cmp(other.as_str()) {
                            std::cmp::Ordering::Less => -1.0,
                            std::cmp::Ordering::Equal => 0.0,
                            std::cmp::Ordering::Greater => 1.0,
                        })
                    }
                    StrOp::At => {
                        // str.at(i): ToIntegerOrInfinity, 음수는 끝에서. 범위 밖 undefined.
                        let arg = args.first().cloned().unwrap_or(Value::Undefined);
                        let i = self.to_integer_or_infinity(&arg)?;
                        let len = units.len() as f64;
                        let idx = if i < 0.0 { len + i } else { i };
                        if idx >= 0.0 && idx < len {
                            let u = idx as usize;
                            Value::Str(String::from_utf16_lossy(&units[u..u + 1]))
                        } else {
                            Value::Undefined
                        }
                    }
                })
            }
            Native::Arr(op) => {
                // 배열이면 그대로. array-like(length 보유 객체)면 임시 배열로 옮겨 실행하고
                // 결과를 되쓴다 → 표준의 generic 배열 메서드(jQuery 가 이걸 의존).
                // 배열 메서드는 generic 하다 (§23.1.3): this 를 ToObject 로 강제한다.
                // null/undefined 는 TypeError (§7.1.18).
                if matches!(recv, None | Some(Value::Undefined) | Some(Value::Null)) {
                    return Err(self.throw_error(
                        "TypeError",
                        "Array.prototype method called on null or undefined",
                    ));
                }
                // indexOf/lastIndexOf/includes: length 가 배열 상한 초과인 array-like 객체는
                // 재료화하면 RangeError/OOM 이므로 존재 인덱스만 지연 검색한다.
                if matches!(op, ArrOp::IndexOf | ArrOp::Includes)
                    && matches!(recv, Some(Value::Obj(_)) | Some(Value::Instance(_)))
                {
                    let rv = recv.clone().unwrap();
                    let len_val = self.member_get(&rv, "length")?;
                    let len = to_length(self.to_number_value(&len_val)?);
                    if len > MAX_ARRAY_LEN {
                        return self.generic_array_search_huge(&rv, op, &args, len);
                    }
                }
                let (a, write_back) = match &recv {
                    // 얼린 배열은 제자리 변형을 무시한다(표준: 조용히 실패).
                    Some(Value::Arr(a))
                        if is_mutating_arr_op(op)
                            && self.is_frozen_val(&Value::Arr(a.clone())) =>
                    {
                        return Ok(Value::Arr(a.clone()))
                    }
                    Some(Value::Arr(a)) => (a.clone(), None),
                    // 문자열: 각 코드유닛을 원소로 (length 있는 array-like)
                    Some(Value::Str(s)) => {
                        let items: Vec<Value> =
                            s.chars().map(|c| Value::Str(c.to_string())).collect();
                        (ArrayObj::new(items), None)
                    }
                    // 그 외 객체/원시 래퍼: generic array-like 읽기(ToLength + [[Get]]).
                    // getter length / 문자열 length / 상속 원소 / Boolean·Number 래퍼
                    // 수신자를 표준대로 처리한다. 변형 연산은 원본 객체에 되쓴다.
                    _ => {
                        let rv = recv.clone().unwrap();
                        // 구멍 보존 재료화 — filter/map/forEach/reduce 가 HasProperty 로
                        // 존재하지 않는 인덱스를 건너뛰게 한다.
                        let arr = self.generic_array_read_sparse(&rv)?;
                        let wb = if is_mutating_arr_op(op) {
                            if let Value::Obj(o) = &rv {
                                Some(o.clone())
                            } else {
                                None
                            }
                        } else {
                            None
                        };
                        (arr, wb)
                    }
                };
                // 변형 연산은 items 를 재배치/축소하므로 구멍 집합과 어긋난다 →
                // 사전에 구멍을 실체화(undefined)해 동기화 문제를 없앤다. (sort/splice/
                // reverse/copyWithin/fill/shift/unshift/push/pop 등)
                if is_mutating_arr_op(op) && a.has_holes() {
                    a.materialize_holes();
                }
                // 콜백의 "배열" 인자(§23.1.3: 3번째 인자)는 **ToObject(원래 수신자)**다 —
                // 임시 복사 a 가 아니다. 원시 수신자는 래퍼로 박는다(obj instanceof Boolean).
                let cb_arr = self.to_object_value(recv.clone().unwrap_or(Value::Undefined));
                let out = match op {
                    ArrOp::Join => {
                        // §23.1.3.18: 구분자 undefined 는 ",". 원소 null/undefined 는 빈
                        // 문자열(예전엔 "undefined"/"null" 로 찍혔다).
                        let sep = match args.first() {
                            None | Some(Value::Undefined) => ",".to_string(),
                            Some(v) => to_display(v),
                        };
                        Value::Str(
                            a.borrow()
                                .iter()
                                .map(|v| match v {
                                    Value::Undefined | Value::Null => String::new(),
                                    other => to_display(other),
                                })
                                .collect::<Vec<_>>()
                                .join(&sep),
                        )
                    }
                    ArrOp::Pop => a.borrow_mut().pop().unwrap_or(Value::Undefined),
                    ArrOp::IndexOf => {
                        // §22.1.3.14: indexOf(searchElement, fromIndex). fromIndex 는
                        // ToIntegerOrInfinity, 음수면 len+n(하한 0)부터 앞으로 검색.
                        let needle = args.first().cloned().unwrap_or(Value::Undefined);
                        // fromIndex 는 ToIntegerOrInfinity(valueOf 관측, Symbol/BigInt→TypeError).
                        let n = match args.get(1) {
                            None | Some(Value::Undefined) => 0.0,
                            Some(v) => self.to_integer_or_infinity(v)?,
                        };
                        let snapshot: Vec<Value> = a.borrow().clone();
                        let len = snapshot.len() as f64;
                        let start = if n >= 0.0 { n } else { (len + n).max(0.0) };
                        let start = if start > len { len } else { start } as usize;
                        let has_holes = a.has_holes();
                        let arr_val = cb_arr.clone();
                        let mut found = -1.0;
                        for i in start..snapshot.len() {
                            // 구멍(HasProperty false)은 건너뛴다 (§23.1.3.14). own-prop/상속이면
                            // 그 값과 strict 비교.
                            let v = match self.arr_elem(&a, &arr_val, &snapshot, i, has_holes)? {
                                Some(v) => v,
                                None => continue,
                            };
                            if strict_eq(&v, &needle) {
                                found = i as f64;
                                break;
                            }
                        }
                        Value::Num(found)
                    }
                    ArrOp::Slice => {
                        // §23.1.3.25: start/end 는 배열 읽기 전에 ToIntegerOrInfinity 로 강제변환
                        // (객체 valueOf 관측, Symbol/BigInt→TypeError, ±∞ 처리). 예전엔 to_num.
                        let len0 = a.borrow().len() as f64;
                        let start_f = match args.first() {
                            Some(v) if !matches!(v, Value::Undefined) => {
                                self.to_integer_or_infinity(v)?
                            }
                            _ => 0.0,
                        };
                        let end_f = match args.get(1) {
                            Some(v) if !matches!(v, Value::Undefined) => {
                                self.to_integer_or_infinity(v)?
                            }
                            _ => len0,
                        };
                        let len = a.borrow().len() as f64; // valueOf 가 배열을 줄였을 수 있어 재확인
                        let rel = |v: f64| -> usize {
                            (if v < 0.0 { (len + v).max(0.0) } else { v.min(len) }) as usize
                        };
                        let start = rel(start_f);
                        let end = rel(end_f).max(start);
                        // §23.1.3.25: k 마다 HasProperty+Get(구멍은 결과에서도 구멍, 상속 원소는
                        // [[Get]] 로 읽음). arr_elem 이 구멍/상속/접근자를 해석한다.
                        let has_holes = a.has_holes();
                        let arr_val = cb_arr.clone();
                        let snapshot: Vec<Value> = a.borrow().clone();
                        let mut out = Vec::with_capacity(end - start);
                        let mut holes = std::collections::HashSet::new();
                        for k in start..end {
                            match self.arr_elem(&a, &arr_val, &snapshot, k, has_holes)? {
                                Some(v) => out.push(v),
                                None => {
                                    holes.insert(out.len());
                                    out.push(Value::Undefined);
                                }
                            }
                        }
                        Value::Arr(ArrayObj::with_holes(out, holes))
                    }
                    ArrOp::ForEach | ArrOp::Map | ArrOp::Filter | ArrOp::FlatMap => {
                        let f = args.first().cloned().unwrap_or(Value::Undefined);
                        // §23.1.3: 콜백은 순회 전에 IsCallable 검사 — 비호출이면 TypeError
                        // (빈/전부-구멍 배열이라 콜백이 안 불려도 던져야 한다).
                        if !is_callable(&f) {
                            return Err(self.throw_error("TypeError", "callback is not a function"));
                        }
                        // 표준: 콜백은 (값, 인덱스, **배열**) 로 부르고, 2번째 인자는 thisArg 다.
                        // 예전엔 (값, 인덱스) 만 넘겨서 a[i-1] 같은 관용 코드가 죽었다
                        // (IntersectionObserver 폴리필이 정확히 그 모양이다).
                        let this_arg = args.get(1).cloned();
                        let arr_val = cb_arr.clone();
                        let snapshot: Vec<Value> = a.borrow().clone();
                        let has_holes = a.has_holes();
                        let len = snapshot.len();
                        let mut out = Vec::new();
                        let mut out_holes = std::collections::HashSet::new();
                        for i in 0..len {
                            // §23.1.3: HasProperty 가 false(진짜 구멍)면 콜백 미호출. map 은
                            // 출력의 같은 자리에 구멍을 보존한다.
                            let item = match self.arr_elem(&a, &arr_val, &snapshot, i, has_holes)? {
                                Some(v) => v,
                                None => {
                                    if matches!(op, ArrOp::Map) {
                                        out_holes.insert(out.len());
                                        out.push(Value::Undefined);
                                    }
                                    continue;
                                }
                            };
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
                            _ => Value::Arr(if out_holes.is_empty() {
                                ArrayObj::new(out)
                            } else {
                                ArrayObj::with_holes(out, out_holes)
                            }),
                        }
                    }
                    ArrOp::Some | ArrOp::Every | ArrOp::Find | ArrOp::FindIndex => {
                        let f = args.first().cloned().unwrap_or(Value::Undefined);
                        if !is_callable(&f) {
                            return Err(self.throw_error("TypeError", "callback is not a function"));
                        }
                        let this_arg = args.get(1).cloned();
                        let arr_val = cb_arr.clone();
                        let snapshot: Vec<Value> = a.borrow().clone();
                        let has_holes = a.has_holes();
                        let len = snapshot.len();
                        let mut result = Value::Undefined;
                        let mut found = false;
                        for i in 0..len {
                            // some/every 는 HasProperty(진짜 구멍)면 콜백 미호출(§23.1.3).
                            // find/findIndex 는 Get 을 써 구멍도 undefined 로 방문한다.
                            let item = match self.arr_elem(&a, &arr_val, &snapshot, i, has_holes)? {
                                Some(v) => v,
                                None => {
                                    if matches!(op, ArrOp::Some | ArrOp::Every) {
                                        continue;
                                    }
                                    Value::Undefined // find/findIndex 는 방문
                                }
                            };
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
                        let f = args.first().cloned().unwrap_or(Value::Undefined);
                        if !is_callable(&f) {
                            return Err(self.throw_error("TypeError", "callback is not a function"));
                        }
                        let arr_val = cb_arr.clone(); // 콜백 4번째 인자 (표준)
                        let snapshot: Vec<Value> = a.borrow().clone();
                        // 구멍은 건너뛴다 (§23.1.3.24 HasProperty) — 초기값 선택과 순회 모두.
                        // 존재 원소를 (인덱스, 값)으로 — arr_elem 이 접근자/상속을 해석.
                        let has_holes = a.has_holes();
                        let mut present: Vec<(usize, Value)> = Vec::new();
                        for i in 0..snapshot.len() {
                            if let Some(v) = self.arr_elem(&a, &arr_val, &snapshot, i, has_holes)? {
                                present.push((i, v));
                            }
                        }
                        let mut pit = present.into_iter();
                        let mut acc = match args.get(1) {
                            Some(init) => init.clone(),
                            None => match pit.next() {
                                Some((_, v)) => v,
                                // 표준 §23.1.3.24: 초기값 없는 빈 배열 reduce 는 TypeError.
                                None => return Err(self.throw_error(
                                    "TypeError",
                                    "Reduce of empty array with no initial value",
                                )),
                            },
                        };
                        for (i, v) in pit {
                            acc = self.call_value(
                                f.clone(),
                                None,
                                vec![acc, v, Value::Num(i as f64), arr_val.clone()],
                            )?;
                        }
                        acc
                    }
                    ArrOp::ReduceRight => {
                        let f = args.first().cloned().unwrap_or(Value::Undefined);
                        if !is_callable(&f) {
                            return Err(self.throw_error("TypeError", "callback is not a function"));
                        }
                        let arr_val = cb_arr.clone();
                        let snapshot: Vec<Value> = a.borrow().clone();
                        let has_holes = a.has_holes();
                        // 존재 원소를 역순 (인덱스, 값)으로.
                        let mut present: Vec<(usize, Value)> = Vec::new();
                        for i in (0..snapshot.len()).rev() {
                            if let Some(v) = self.arr_elem(&a, &arr_val, &snapshot, i, has_holes)? {
                                present.push((i, v));
                            }
                        }
                        let mut pit = present.into_iter();
                        let mut acc = match args.get(1) {
                            Some(init) => init.clone(),
                            None => match pit.next() {
                                Some((_, v)) => v,
                                None => {
                                    return Err(self.throw_error(
                                        "TypeError",
                                        "Reduce of empty array with no initial value",
                                    ))
                                }
                            },
                        };
                        for (idx, v) in pit {
                            acc = self.call_value(
                                f.clone(),
                                None,
                                vec![acc, v, Value::Num(idx as f64), arr_val.clone()],
                            )?;
                        }
                        acc
                    }
                    ArrOp::FindLast | ArrOp::FindLastIndex => {
                        let f = args.first().cloned().unwrap_or(Value::Undefined);
                        // §23.1.3.13/.14: predicate 는 순회 전 IsCallable 검사(비호출 TypeError).
                        if !is_callable(&f) {
                            return Err(self.throw_error("TypeError", "predicate is not a function"));
                        }
                        let snapshot: Vec<Value> = a.borrow().clone();
                        let mut result = if matches!(op, ArrOp::FindLastIndex) {
                            Value::Num(-1.0)
                        } else {
                            Value::Undefined
                        };
                        for i in (0..snapshot.len()).rev() {
                            let r = self.call_value(
                                f.clone(),
                                args.get(1).cloned(), // thisArg
                                vec![snapshot[i].clone(), Value::Num(i as f64), cb_arr.clone()],
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
                        // §23.1.3.1: 수신자 + 각 인자를 순서대로, IsConcatSpreadable 이면 펼친다
                        // (@@isConcatSpreadable 우선, 없으면 IsArray). array-like 는 length+Get.
                        let mut all: Vec<Value> = Vec::with_capacity(1 + args.len());
                        all.push(Value::Arr(a.clone()));
                        all.extend(args.iter().cloned());
                        let mut out: Vec<Value> = Vec::new();
                        for item in all {
                            if self.is_concat_spreadable(&item)? {
                                match &item {
                                    Value::Arr(b) => out.extend(b.borrow().iter().cloned()),
                                    _ => {
                                        let lv = self.member_get(&item, "length")?;
                                        let len =
                                            to_length(self.to_number_value(&lv)?) as usize;
                                        for k in 0..len {
                                            let key = k.to_string();
                                            if self.has_property(&item, &key) {
                                                out.push(self.member_get(&item, &key)?);
                                            } else {
                                                out.push(Value::Undefined);
                                            }
                                        }
                                    }
                                }
                            } else {
                                out.push(item);
                            }
                        }
                        Value::Arr(ArrayObj::new(out))
                    }
                    ArrOp::Includes => {
                        // §22.1.3.13: includes(searchElement, fromIndex). SameValueZero
                        // (NaN 매칭), fromIndex 는 ToIntegerOrInfinity.
                        let needle = args.first().cloned().unwrap_or(Value::Undefined);
                        // fromIndex 는 ToIntegerOrInfinity(valueOf 관측, Symbol→TypeError).
                        let n = match args.get(1) {
                            None | Some(Value::Undefined) => 0.0,
                            Some(v) => self.to_integer_or_infinity(v)?,
                        };
                        let items = a.borrow();
                        let len = items.len() as f64;
                        let start = if n >= 0.0 { n } else { (len + n).max(0.0) };
                        let start = if start > len { len } else { start } as usize;
                        Value::Bool(
                            items[start.min(items.len())..]
                                .iter()
                                .any(|v| same_value_zero(v, &needle)),
                        )
                    }
                    ArrOp::Splice => {
                        // §23.1.3.29: start/deleteCount 는 ToIntegerOrInfinity(변형 전 강제변환).
                        // argCount==0 → 삭제 0, ==1 → len-start, else → clamp(ToInteger(dc)).
                        let len0 = a.borrow().len() as f64;
                        let start_f = match args.first() {
                            Some(v) => self.to_integer_or_infinity(v)?,
                            None => 0.0,
                        };
                        let start = if start_f < 0.0 {
                            (len0 + start_f).max(0.0)
                        } else {
                            start_f.min(len0)
                        };
                        let del_f = if args.is_empty() {
                            0.0
                        } else if args.len() == 1 {
                            len0 - start
                        } else {
                            self.to_integer_or_infinity(&args[1])?.max(0.0).min(len0 - start)
                        };
                        let mut arr = a.borrow_mut();
                        let start = (start as usize).min(arr.len());
                        let del = (del_f as usize).min(arr.len() - start);
                        let removed: Vec<Value> =
                            arr.splice(start..start + del, args.iter().skip(2).cloned()).collect();
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
                        // §23.1.3.30: comparefn 이 undefined 도 함수도 아니면 TypeError.
                        let cmp_arg = args.first().cloned().unwrap_or(Value::Undefined);
                        if !matches!(cmp_arg, Value::Undefined) && !is_callable(&cmp_arg) {
                            return Err(self.throw_error(
                                "TypeError",
                                "The comparison function must be either a function or undefined",
                            ));
                        }
                        let cmp = args.first().cloned().filter(|v| !matches!(v, Value::Undefined));
                        let mut items: Vec<Value> = a.borrow().clone();
                        let n = items.len();
                        for i in 1..n {
                            let mut j = i;
                            while j > 0 {
                                // CompareArrayElements (§23.1.3.30.2): undefined 는 항상
                                // 뒤로 가고 비교자에 넘기지 않는다. (홀은 dense 모델상 undefined.)
                                let (xu, yu) = (
                                    matches!(items[j - 1], Value::Undefined),
                                    matches!(items[j], Value::Undefined),
                                );
                                let ord = if xu || yu {
                                    if xu && yu {
                                        0.0
                                    } else if xu {
                                        1.0
                                    } else {
                                        -1.0
                                    }
                                } else {
                                    match &cmp {
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
                        // arr.at(i): ToIntegerOrInfinity, 음수는 끝에서. 범위 밖 undefined.
                        let arg = args.first().cloned().unwrap_or(Value::Undefined);
                        let i = self.to_integer_or_infinity(&arg)?;
                        let items = a.borrow();
                        let len = items.len() as f64;
                        let idx = if i < 0.0 { len + i } else { i };
                        if idx >= 0.0 && idx < len {
                            items.get(idx as usize).cloned().unwrap_or(Value::Undefined)
                        } else {
                            Value::Undefined
                        }
                    }
                    ArrOp::Fill => {
                        // arr.fill(value, start?, end?): start/end 는 ToIntegerOrInfinity(배열 읽기 전).
                        let val = args.first().cloned().unwrap_or(Value::Undefined);
                        let len0 = a.borrow().len() as f64;
                        let start_f = match args.get(1) {
                            Some(v) if !matches!(v, Value::Undefined) => {
                                self.to_integer_or_infinity(v)?
                            }
                            _ => 0.0,
                        };
                        let end_f = match args.get(2) {
                            Some(v) if !matches!(v, Value::Undefined) => {
                                self.to_integer_or_infinity(v)?
                            }
                            _ => len0,
                        };
                        let len = a.borrow().len() as f64;
                        let clampi = |v: f64| -> usize {
                            (if v < 0.0 { (len + v).max(0.0) } else { v.min(len) }) as usize
                        };
                        let start = clampi(start_f);
                        let end = clampi(end_f);
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
                        // index 는 ToIntegerOrInfinity (배열 복제 전).
                        let arg = args.first().cloned().unwrap_or(Value::Undefined);
                        let n = self.to_integer_or_infinity(&arg)?;
                        let items = a.borrow().clone();
                        let len = items.len() as f64;
                        let idx = if n < 0.0 { len + n } else { n };
                        if idx < 0.0 || idx >= len {
                            return Err(self.throw_error("RangeError", "Invalid index"));
                        }
                        let mut items = items;
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
                // §25.5.1: text = ToString(arg) 를 먼저(Symbol→TypeError, toString 예외 전파).
                // 그 다음 파싱 — 잘못된 JSON 은 SyntaxError.
                let text_arg = args.first().cloned().unwrap_or(Value::Undefined);
                let src = self.to_string_value(&text_arg)?;
                let (parsed, snap) = match json_parse_snap(&src) {
                    Ok(v) => v,
                    Err(msg) => return Err(self.throw_error("SyntaxError", msg)),
                };
                // reviver(2번째 인자): 결과를 후위 순회하며 변환한다 (§25.5.1.1 InternalizeJSONProperty).
                match args.get(1).cloned().filter(is_callable) {
                    Some(reviver) => {
                        let holder = Value::Obj(Rc::new(RefCell::new({
                            let mut m = ObjMap::new();
                            m.insert(String::new(), parsed);
                            m
                        })));
                        self.json_revive(&holder, "", &reviver, Some(&snap))
                    }
                    None => Ok(parsed),
                }
            }
            // 순환 구조면 TypeError 를 던진다(표준). 조용히 폭발/무한재귀하지 않는다.
            // replacer(배열/함수)와 space(들여쓰기)도 표준대로 처리한다 — 예전엔 둘 다
            // 조용히 무시해서 JSON.stringify(o, null, 2) 가 한 줄로 나왔다.
            // JSON.rawJSON(text) (ES2024 §25.5.1): 유효한 원시 JSON 텍스트를 담은 얼린
            // 객체를 만든다. JSON.stringify 가 이 텍스트를 그대로(따옴표 없이) 낸다.
            Native::JsonRawJson => {
                let text = match args.first().cloned() {
                    Some(v) => {
                        let p = self.to_primitive_or_throw(v, true)?;
                        to_display(&p)
                    }
                    None => "undefined".to_string(),
                };
                let is_ws = |c: char| matches!(c, '\t' | '\n' | '\r' | ' ');
                if text.is_empty()
                    || text.chars().next().map(is_ws).unwrap_or(false)
                    || text.chars().last().map(is_ws).unwrap_or(false)
                {
                    return Err(self.throw_error("SyntaxError", "JSON.rawJSON: invalid text"));
                }
                // 유효 JSON 검증(JSON.parse). 결과가 객체/배열이면 원시 아님 → SyntaxError.
                let parsed =
                    self.call_native(Native::JsonParse, None, vec![Value::Str(text.clone())])?;
                if matches!(parsed, Value::Obj(_) | Value::Arr(_)) {
                    return Err(
                        self.throw_error("SyntaxError", "JSON.rawJSON: value must be primitive")
                    );
                }
                let mut m = ObjMap::new();
                m.insert("rawJSON".to_string(), Value::Str(text));
                set_prop_attrs(&mut m, "rawJSON", ATTR_ENUMERABLE); // {w:f,e:t,c:f} (얼린 뒤)
                m.insert("\u{0}isRawJSON".to_string(), Value::Bool(true));
                m.insert("__proto__".to_string(), Value::Null);
                let obj = Value::Obj(Rc::new(RefCell::new(m)));
                self.set_integrity(&obj, super::INTEG_FROZEN);
                Ok(obj)
            }
            // JSON.isRawJSON(v) (ES2024): v 가 rawJSON 객체인가.
            Native::JsonIsRawJson => Ok(Value::Bool(matches!(
                args.first(),
                Some(Value::Obj(m)) if m.borrow().contains_key("\u{0}isRawJSON")
            ))),
            Native::JsonStringify => {
                let v = args.first().cloned().unwrap_or(Value::Undefined);
                let replacer = args.get(1).cloned().unwrap_or(Value::Undefined);
                // space(3번째 인자, §25.5.2.1): Number/String 래퍼 객체는 원시값으로 푼다.
                // Number → floor 만큼 공백(최대 10), String → 앞 10글자. 그 외는 들여쓰기 없음.
                let space = args.get(2).cloned().unwrap_or(Value::Undefined);
                let space = match &space {
                    Value::Obj(m) if m.borrow().contains_key(WRAPPER_SLOT) => {
                        match wrapper_primitive(&space) {
                            Some(Value::Num(n)) => Value::Num(n),
                            Some(Value::Str(s)) => Value::Str(s),
                            _ => Value::Undefined,
                        }
                    }
                    _ => space,
                };
                let indent = match &space {
                    Value::Num(n) if *n >= 1.0 => " ".repeat((n.floor() as usize).min(10)),
                    Value::Str(s) => s.chars().take(10).collect(),
                    _ => String::new(),
                };
                // replacer 배열 → PropertyList (§25.5.2.1 step 5): 문자열/수/String·Number 래퍼만
                // 채택(래퍼는 ToString), 중복 제거, 그 밖(undefined/불리언/일반객체 등)은 제외.
                let keys: Option<Vec<String>> = if let Value::Arr(a) = &replacer {
                    let items = a.borrow().clone();
                    let mut list: Vec<String> = Vec::new();
                    for el in &items {
                        let item: Option<String> = match el {
                            Value::Str(s) => Some(s.clone()),
                            Value::Num(n) => Some(num_to_str(*n)),
                            Value::Obj(m) if m.borrow().contains_key(WRAPPER_SLOT) => {
                                match wrapper_primitive(el) {
                                    Some(Value::Str(_)) | Some(Value::Num(_)) => {
                                        Some(self.to_string_value(el)?)
                                    }
                                    _ => None,
                                }
                            }
                            _ => None,
                        };
                        if let Some(it) = item {
                            if !list.contains(&it) {
                                list.push(it);
                            }
                        }
                    }
                    Some(list)
                } else {
                    None
                };
                let fnrep = if matches!(replacer, Value::Fn(_) | Value::Native(_) | Value::Bound(_)) {
                    Some(replacer.clone())
                } else {
                    None
                };
                let mut path = Vec::new();
                let holder = Value::Obj(Rc::new(RefCell::new(ObjMap::new())));
                // json_ser 의 Err 는 이미 throw 된 것(순환/BigInt 는 내부 throw_error, 사용자
                // getter/toJSON/replacer 는 전파) — 재래핑하면 원래 던진 값이 TypeError 로
                // 뭉개진다. 그대로 전파한다.
                let s = self.json_ser(&v, "", &holder, &fnrep, &keys, &indent, 0, &mut path)?;
                Ok(s.map(Value::Str).unwrap_or(Value::Undefined))
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
                // decodeURI 는 예약 문자를 %XX 로 보존, decodeURIComponent 는 전부 디코드.
                // 잘못된 % 시퀀스/비UTF-8 은 URIError.
                let s = args.first().map(to_display).unwrap_or_default();
                let preserve = matches!(n, Native::DecodeUri);
                match uri_decode(&s, preserve) {
                    Ok(r) => Ok(Value::Str(r)),
                    Err(()) => Err(self.throw_error("URIError", "URI malformed")),
                }
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
                // §28.1.6: target 이 객체가 아니면 TypeError. key 는 ToPropertyKey.
                let target = args.first().cloned().unwrap_or(Value::Undefined);
                if !is_object(&target) {
                    return Err(self.throw_error("TypeError", "Reflect.get called on non-object"));
                }
                let key = match args.get(1).cloned() {
                    Some(k) => self.to_property_key(k)?,
                    None => String::new(),
                };
                self.member_get(&target, &key)
            }
            Native::ReflectSet => {
                // §28.1.11: [[Set]](P, V, Receiver) 의 불리언. Receiver 기본값 = target.
                // 예전엔 target 에 무조건 insert(접근자/writable/receiver 무시)라 표준과 달랐다.
                let target = args.first().cloned().unwrap_or(Value::Undefined);
                if !is_object(&target) {
                    return Err(self.throw_error("TypeError", "Reflect.set called on non-object"));
                }
                let key = match args.get(1).cloned() {
                    Some(k) => self.to_property_key(k)?,
                    None => String::new(),
                };
                let val = args.get(2).cloned().unwrap_or(Value::Undefined);
                let receiver = args.get(3).cloned().unwrap_or_else(|| target.clone());
                Ok(Value::Bool(self.ordinary_set(&target, &key, val, &receiver)?))
            }
            Native::ReflectHas => {
                let target = args.first().cloned().unwrap_or(Value::Undefined);
                if !is_object(&target) {
                    return Err(self.throw_error("TypeError", "Reflect.has called on non-object"));
                }
                let key = match args.get(1).cloned() {
                    Some(k) => self.to_property_key(k)?,
                    None => String::new(),
                };
                let has = match args.first() {
                    Some(Value::Obj(m)) => {
                        // 심볼 own 키("\0@@…")도 HasProperty 대상 — 내부마커만 제외.
                        (!is_internal_key(&key) || is_symbol_key(&key))
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
                let target = args.first().cloned().unwrap_or(Value::Undefined);
                if !is_object(&target) {
                    return Err(
                        self.throw_error("TypeError", "Reflect.deleteProperty called on non-object")
                    );
                }
                let key = match args.get(1).cloned() {
                    Some(k) => self.to_property_key(k)?,
                    None => String::new(),
                };
                // §28.1.4 = target.[[Delete]](P): configurable:false 면 false(삭제 안 함).
                Ok(Value::Bool(self.delete_own(&target, &key)?))
            }
            Native::ReflectApply => {
                // Reflect.apply(target, thisArg, argumentsList) (§28.1.1).
                let f = args.first().cloned().unwrap_or(Value::Undefined);
                // step 1: target 이 callable 아니면 TypeError.
                if !is_callable(&f) {
                    return Err(self.throw_error("TypeError", "Reflect.apply target is not callable"));
                }
                let this = args.get(1).cloned();
                // step 2: args = ? CreateListFromArrayLike(argumentsList) — 비객체 TypeError,
                // length 게터/원소 게터 예외 전파.
                let list_val = args.get(2).cloned().unwrap_or(Value::Undefined);
                if !is_object(&list_val) {
                    return Err(self.throw_error(
                        "TypeError",
                        "CreateListFromArrayLike called on non-object",
                    ));
                }
                let arg_list = self.generic_array_read(&list_val)?;
                self.call_value(f, this, arg_list)
            }
            Native::ReflectConstruct => {
                // Reflect.construct(target, argumentsList[, newTarget]) (§28.1.2).
                let f = args.first().cloned().unwrap_or(Value::Undefined);
                // step 1: target 이 생성자가 아니면 TypeError.
                if !self.is_constructor(&f) {
                    return Err(self.throw_error(
                        "TypeError",
                        "Reflect.construct target is not a constructor",
                    ));
                }
                // step 3: newTarget(주어졌으면)도 생성자여야 한다(argumentsList 읽기 전에 검사).
                // isConstructor 하네스가 이 검사에 의존 — Reflect.construct(function(){}, [], method)
                // 가 던져야 isConstructor(method)===false 가 된다.
                if let Some(nt) = args.get(2) {
                    if !self.is_constructor(nt) {
                        return Err(self.throw_error(
                            "TypeError",
                            "Reflect.construct newTarget is not a constructor",
                        ));
                    }
                }
                // step 4: args = ? CreateListFromArrayLike(argumentsList).
                let list_val = args.get(1).cloned().unwrap_or(Value::Undefined);
                if !is_object(&list_val) {
                    return Err(self.throw_error(
                        "TypeError",
                        "CreateListFromArrayLike called on non-object",
                    ));
                }
                let arg_list = self.generic_array_read(&list_val)?;
                self.construct(f, arg_list)
            }
            // Reflect 나머지 (§28.1). 대상이 객체가 아니면 TypeError. 변형 계열은 불리언 반환.
            Native::ReflectSetPrototypeOf => {
                // §28.1.13: target 은 객체(TypeError), proto 는 Object|Null(TypeError).
                // [[SetPrototypeOf]] 의 불리언을 그대로 돌려준다(Object 처럼 throw 하지 않음).
                let target = args.first().cloned().unwrap_or(Value::Undefined);
                if !is_object(&target) {
                    return Err(self.throw_error("TypeError", "Reflect.setPrototypeOf called on non-object"));
                }
                let proto = args.get(1).cloned().unwrap_or(Value::Undefined);
                if !is_object(&proto) && !matches!(proto, Value::Null) {
                    return Err(self.throw_error(
                        "TypeError",
                        "Reflect.setPrototypeOf proto must be an Object or null",
                    ));
                }
                Ok(Value::Bool(self.ordinary_set_prototype_of(&target, proto)?))
            }
            Native::ReflectPreventExtensions => {
                if !is_object(args.first().unwrap_or(&Value::Undefined)) {
                    return Err(self.throw_error("TypeError", "Reflect.preventExtensions called on non-object"));
                }
                self.call_native(Native::ObjectPreventExt, None, args)?;
                Ok(Value::Bool(true))
            }
            Native::ReflectIsExtensible => {
                if !is_object(args.first().unwrap_or(&Value::Undefined)) {
                    return Err(self.throw_error("TypeError", "Reflect.isExtensible called on non-object"));
                }
                self.call_native(Native::ObjectIsExtensible, None, args)
            }
            // §28.1.6: Object.getPrototypeOf 와 달리 비객체 target 은 TypeError(원시 강제변환 안 함).
            Native::ReflectGetPrototypeOf => {
                if !is_object(args.first().unwrap_or(&Value::Undefined)) {
                    return Err(self.throw_error("TypeError", "Reflect.getPrototypeOf called on non-object"));
                }
                self.call_native(Native::ObjectGetPrototypeOf, None, args)
            }
            Native::ReflectOwnKeys => {
                if !is_object(args.first().unwrap_or(&Value::Undefined)) {
                    return Err(self.throw_error("TypeError", "Reflect.ownKeys called on non-object"));
                }
                // §28.1.10 = target.[[OwnPropertyKeys]](): 정수 인덱스 오름차순 + 문자열
                // 삽입순 + **심볼 삽입순**. getOwnPropertyNames(문자열)에 심볼 키를 이어붙인다.
                let names =
                    self.call_native(Native::ObjectGetOwnPropertyNames, None, args.clone())?;
                let syms =
                    self.call_native(Native::ObjectGetOwnPropertySymbols, None, args)?;
                let mut keys: Vec<Value> = match names {
                    Value::Arr(a) => a.borrow().clone(),
                    _ => Vec::new(),
                };
                if let Value::Arr(a) = syms {
                    keys.extend(a.borrow().iter().cloned());
                }
                Ok(Value::Arr(ArrayObj::new(keys)))
            }
            Native::ReflectGetOwnPropertyDescriptor => {
                if !is_object(args.first().unwrap_or(&Value::Undefined)) {
                    return Err(self.throw_error("TypeError", "Reflect.getOwnPropertyDescriptor called on non-object"));
                }
                self.call_native(Native::ObjectGetOwnPropertyDescriptor, None, args)
            }
            Native::ReflectDefineProperty => {
                if !is_object(args.first().unwrap_or(&Value::Undefined)) {
                    return Err(self.throw_error("TypeError", "Reflect.defineProperty called on non-object"));
                }
                // 성공하면 true. 재정의 불가 등으로 거부되면 false(throw 아님). 우리
                // ObjectDefineProperty 는 거부를 "Cannot redefine property" TypeError 로 내므로
                // 그것만 false 로 흡수하고, 서술자 강제변환 오류 등은 전파한다.
                match self.call_native(Native::ObjectDefineProperty, None, args) {
                    Ok(_) => Ok(Value::Bool(true)),
                    Err(e) => {
                        // 거부 신호(재정의 불가 / Proxy defineProperty 트랩 falsish)는 false 로
                        // 흡수. 서술자 강제변환 오류 등은 전파.
                        let redefine = matches!(&self.thrown, Some(Value::Obj(m))
                            if matches!(m.borrow().get("message"), Some(Value::Str(s))
                                if s.contains("redefine") || s.contains("falsish")));
                        if redefine {
                            self.thrown = None;
                            Ok(Value::Bool(false))
                        } else {
                            Err(e) // 서술자 강제변환 오류 등은 그대로 전파
                        }
                    }
                }
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
            // Object.groupBy(items, cb) (§20.1.2.13): 콜백 키(문자열)로 그룹화, null-proto 객체.
            Native::ObjectGroupBy => {
                let items = args.first().cloned().unwrap_or(Value::Undefined);
                let cb = args.get(1).cloned().unwrap_or(Value::Undefined);
                if !is_callable(&cb) {
                    return Err(self.throw_error("TypeError", "Object.groupBy callback is not callable"));
                }
                let vec = self.iterate_to_vec(&items)?;
                let mut map = ObjMap::new();
                for (i, item) in vec.into_iter().enumerate() {
                    let k = self.call_value(cb.clone(), None, vec![item.clone(), Value::Num(i as f64)])?;
                    let key = self.to_property_key(k)?;
                    let existing = match map.get(&key) {
                        Some(Value::Arr(a)) => Some(a.clone()),
                        _ => None,
                    };
                    match existing {
                        Some(a) => a.borrow_mut().push(item),
                        None => {
                            map.insert(key, Value::Arr(ArrayObj::new(vec![item])));
                        }
                    }
                }
                Ok(Value::Obj(Rc::new(RefCell::new(map))))
            }
            // Map.groupBy(items, cb) (§24.1.2.1): 키는 임의 값(SameValueZero), Map 반환.
            Native::MapGroupBy => {
                let items = args.first().cloned().unwrap_or(Value::Undefined);
                let cb = args.get(1).cloned().unwrap_or(Value::Undefined);
                if !is_callable(&cb) {
                    return Err(self.throw_error("TypeError", "Map.groupBy callback is not callable"));
                }
                let vec = self.iterate_to_vec(&items)?;
                let mut data: Vec<(Value, Value)> = Vec::new();
                for (i, item) in vec.into_iter().enumerate() {
                    let key = self.call_value(cb.clone(), None, vec![item.clone(), Value::Num(i as f64)])?;
                    let existing = data
                        .iter()
                        .find(|(k, _)| same_value_zero(k, &key))
                        .and_then(|(_, v)| if let Value::Arr(a) = v { Some(a.clone()) } else { None });
                    match existing {
                        Some(a) => a.borrow_mut().push(item),
                        None => data.push((key, Value::Arr(ArrayObj::new(vec![item])))),
                    }
                }
                Ok(Value::MapVal(Rc::new(RefCell::new(data))))
            }
            // Promise.withResolvers() (§27.2.4.8): {promise, resolve, reject}.
            // step 1: NewPromiseCapability(this) — this 가 생성자가 아니면 TypeError.
            Native::PromiseWithResolvers => {
                if !self.is_constructor(&recv.clone().unwrap_or(Value::Undefined)) {
                    return Err(self.throw_error(
                        "TypeError",
                        "Promise.withResolvers called on non-constructor",
                    ));
                }
                let p = self.new_promise();
                let resolve = Value::Bound(Rc::new((
                    Value::Native(Native::PromiseSettleResolve),
                    p.clone(),
                    vec![],
                )));
                let reject = Value::Bound(Rc::new((
                    Value::Native(Native::PromiseSettleReject),
                    p.clone(),
                    vec![],
                )));
                let mut m = ObjMap::new();
                m.insert("promise".to_string(), p);
                m.insert("resolve".to_string(), resolve);
                m.insert("reject".to_string(), reject);
                Ok(Value::Obj(Rc::new(RefCell::new(m))))
            }
            // ToObject: null/undefined 는 TypeError (§7.1.18).
            Native::ObjectKeys
            | Native::ObjectValues
            | Native::ObjectEntries
            | Native::ObjectGetOwnPropertyNames
                if matches!(args.first(), None | Some(Value::Undefined) | Some(Value::Null)) =>
            {
                Err(self.throw_error(
                    "TypeError",
                    "Cannot convert undefined or null to object",
                ))
            }
            // keys/values/entries/getOwnPropertyNames 는 문자열 원시값을 래퍼로 박싱해
            // 인덱스 프로퍼티를 노출한다 (§20.1.2: Object.keys('ab') → ['0','1']).
            n2 @ (Native::ObjectKeys
            | Native::ObjectValues
            | Native::ObjectEntries
            | Native::ObjectGetOwnPropertyNames)
                if matches!(args.first(), Some(Value::Str(_))) =>
            {
                let boxed = self.to_object_value(args[0].clone());
                let mut new_args = args.clone();
                new_args[0] = boxed;
                self.call_native(n2, None, new_args)
            }
            Native::ObjectKeys => match args.first() {
                // Proxy: ownKeys 트랩 (없으면 타깃 위임). Object.keys(proxy) 가 빈 배열을
                // 돌려주던 문제 — 반응성 프록시의 키 열거가 통째로 안 됐다.
                Some(Value::Proxy(p)) => {
                    self.proxy_revoked_guard(p)?;
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
                Some(v @ (Value::Arr(_) | Value::Instance(_) | Value::Class(_))) => {
                    // 배열은 구멍을 건너뛴 존재 인덱스 + own 열거 프로퍼티.
                    let keys: Vec<Value> = own_enumerable_entries(v)
                        .into_iter()
                        .map(|(k, _)| Value::Str(k))
                        .collect();
                    Ok(Value::Arr(ArrayObj::new(keys)))
                }
                // 함수도 ordinary object — 열거 가능한 own 프로퍼티(사용자가 얹은 것)를
                // 센다. name/length/prototype 은 비열거다(prototype 은 지연 생성돼 props 에
                // 들어갈 수 있으므로 명시 제외).
                Some(Value::Fn(f)) => {
                    let b = f.props.borrow();
                    let keys: Vec<Value> = b
                        .keys()
                        .filter(|k| {
                            !is_internal_key(k)
                                && !matches!(k.as_str(), "prototype" | "name" | "length")
                                && !b.contains_key(&nonenum_marker(k))
                        })
                        .map(|k| Value::Str(k.clone()))
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
                // §20.1.2.7: iterable 을 순회, 각 항목은 객체여야 하며 [0]/[1] 을 Get 해
                // CreateDataProperty(ToPropertyKey(k), v). 비이터러블/비객체 항목은 TypeError.
                let src = args.first().cloned().unwrap_or(Value::Undefined);
                let it = match self.try_get_iterator(&src)? {
                    Some(it) => it,
                    None => {
                        return Err(self.throw_error(
                            "TypeError",
                            "Object.fromEntries requires an iterable",
                        ))
                    }
                };
                let mut map = ObjMap::new();
                loop {
                    let (entry, done) = self.gen_iter_next(&it, Value::Undefined)?;
                    if done {
                        break;
                    }
                    if !is_object(&entry) {
                        let e = self.throw_error("TypeError", "iterator entry is not an object");
                        return Err(self.iterator_close_throw(&it, e));
                    }
                    let k = match self.member_get(&entry, "0") {
                        Ok(k) => k,
                        Err(e) => return Err(self.iterator_close_throw(&it, e)),
                    };
                    let v = match self.member_get(&entry, "1") {
                        Ok(v) => v,
                        Err(e) => return Err(self.iterator_close_throw(&it, e)),
                    };
                    let key = match self.to_property_key(k) {
                        Ok(key) => key,
                        Err(e) => return Err(self.iterator_close_throw(&it, e)),
                    };
                    map.insert(key, v);
                    self.tick()?;
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
                // §20.1.2.1: 각 소스의 열거 가능한 own 키(문자열+심볼)를 Get(getter 호출)해서
                // Set(Throw=true)로 대상에 복사. 실패(read-only/non-extensible/getter-only)면 TypeError.
                for src in args[1..].to_vec() {
                    if matches!(src, Value::Undefined | Value::Null) {
                        continue;
                    }
                    // 열거 가능한 own 키 — 심볼 키("\0@@…")도 포함(그 밖 내부 마커는 제외).
                    let keys: Vec<String> = match &src {
                        Value::Obj(m) => {
                            let b = m.borrow();
                            b.keys()
                                .filter(|k| {
                                    (!is_internal_key(k) || is_symbol_key(k))
                                        && !b.contains_key(&nonenum_marker(k))
                                })
                                .cloned()
                                .collect()
                        }
                        _ => own_enumerable_entries(&src)
                            .into_iter()
                            .map(|(k, _)| k)
                            .collect(),
                    };
                    for k in keys {
                        // Get: 접근자면 getter 호출(값을 복사, 서술자 아님).
                        let v = self.member_get(&src, &k)?;
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
                // §23.1.2.1: mapFn 이 undefined 아니면 반드시 callable(아니면 TypeError).
                let map_fn = match args.get(1) {
                    None | Some(Value::Undefined) => None,
                    Some(f) if is_callable(f) => Some(f.clone()),
                    Some(_) => {
                        return Err(self.throw_error(
                            "TypeError",
                            "Array.from: mapFn is not a function",
                        ))
                    }
                };
                // 이터러블(배열/문자열/Set/Map/제너레이터/반복자/사용자 [Symbol.iterator])이면
                // 프로토콜로, 아니면 array-like(ToLength(ToNumber(length)) + 인덱스).
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
                    // array-like: generic_array_read 로 length 강제변환(valueOf) + [[Get]].
                    Value::Obj(_) => self.generic_array_read(&src)?,
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
