// call_native: 모든 네이티브(내장) 메서드/함수 디스패치. interp/mod.rs 에서 분리.
use super::*;
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
            i.fields.borrow().iter().map(|(k, val)| (k.clone(), val.clone())).collect()
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
            c.statics.borrow().iter().map(|(k, val)| (k.clone(), val.clone())).collect()
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

fn uri_encode(s: &str, extra_safe: &str) -> String {
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
fn uri_decode(s: &str) -> String {
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

// (키,값) 쌍을 x-www-form-urlencoded 쿼리 문자열로 (각 성분 encodeURIComponent).
fn build_query(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", uri_encode(k, ""), uri_encode(v, "")))
        .collect::<Vec<_>>()
        .join("&")
}

// application/x-www-form-urlencoded 쿼리를 (키,값) 쌍으로. '+' → 공백, %XX 디코드.
fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|s| !s.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (uri_decode(&k.replace('+', " ")), uri_decode(&v.replace('+', " ")))
        })
        .collect()
}

// 컨텍스트의 __path 배열 조작 ([x0,y0,x1,y1,...] 평탄 저장).
fn set_path(ctx: &Rc<RefCell<ObjMap>>, pts: Vec<Value>) {
    ctx.borrow_mut().insert("\u{0}path".to_string(), Value::Arr(ArrayObj::new(pts)));
}
fn push_path(ctx: &Rc<RefCell<ObjMap>>, x: f32, y: f32) {
    if let Some(Value::Arr(a)) = ctx.borrow().get("\u{0}path") {
        a.borrow_mut().push(Value::Num(x as f64));
        a.borrow_mut().push(Value::Num(y as f64));
    }
}
fn get_path(ctx: &Rc<RefCell<ObjMap>>) -> Vec<(f32, f32)> {
    if let Some(Value::Arr(a)) = ctx.borrow().get("\u{0}path") {
        let flat = a.borrow();
        return flat
            .chunks(2)
            .filter(|c| c.len() == 2)
            .map(|c| (to_num(&c[0]) as f32, to_num(&c[1]) as f32))
            .collect();
    }
    Vec::new()
}

// font 문자열에서 px 크기 추출 ("bold 16px sans-serif" → 16). 없으면 10.
fn font_px(font: &str) -> f32 {
    for tok in font.split_whitespace() {
        if let Some(n) = tok.strip_suffix("px") {
            if let Ok(v) = n.parse::<f32>() {
                return v;
            }
        }
    }
    10.0
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
    // 현재 문서의 (호스트, 경로) — 쿠키 범위 판정용
    fn page_host_path(&self) -> (String, String) {
        let raw = self
            .base_url
            .clone()
            .unwrap_or_else(|| "http://localhost/".to_string());
        match crate::url::Url::parse(&raw) {
            Ok(u) => {
                let path = u.path.split(['?', '#']).next().unwrap_or("/").to_string();
                (u.host, if path.is_empty() { "/".to_string() } else { path })
            }
            Err(_) => ("localhost".to_string(), "/".to_string()),
        }
    }
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
            return Err("BigInt 는 JSON 으로 직렬화할 수 없음".to_string());
        }
        Ok(match v {
            Value::Undefined
            | Value::Fn(_)
            | Value::Native(_)
            | Value::Dom(_)
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
                let mut ks: Vec<&String> = m.keys().collect();
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

    // canvas 2D 컨텍스트 메서드 처리. recv=컨텍스트 객체. ops 를 canvas_cmds 에 쌓는다.
    fn canvas_method(
        &mut self,
        method: CanvasMethod,
        recv: Option<Value>,
        args: Vec<Value>,
    ) -> Result<Value, String> {
        use CanvasMethod::*;
        // addColorStop 의 수신자는 **그라디언트 객체**다 (컨텍스트가 아니다) — 먼저 처리.
        if matches!(method, AddColorStop) {
            if let Some(Value::Obj(g)) = &recv {
                let pos = args.first().map(to_num).unwrap_or(0.0);
                let col = args.get(1).map(to_display).unwrap_or_default();
                let entry = Value::Arr(ArrayObj::new(vec![Value::Num(pos), Value::Str(col)]));
                let stops = match g.borrow().get("\u{0}stops") {
                    Some(Value::Arr(st)) => Some(st.clone()),
                    _ => None,
                };
                if let Some(st) = stops {
                    st.borrow_mut().push(entry);
                }
            }
            return Ok(Value::Undefined);
        }
        let Some(Value::Obj(ctx)) = recv else { return Ok(Value::Undefined) };
        let canvas_id = match ctx.borrow().get("\u{0}canvas") {
            Some(Value::Num(n)) => *n as crate::dom::NodeId,
            _ => return Ok(Value::Undefined),
        };
        let num = |i: usize| args.get(i).map(to_num).unwrap_or(0.0) as f32;
        let style = |key: &str| -> crate::css::Color {
            match ctx.borrow().get(key) {
                Some(Value::Str(s)) => {
                    crate::css::parse_color(s).unwrap_or(crate::css::Color { r: 0, g: 0, b: 0, a: 255 })
                }
                _ => crate::css::Color { r: 0, g: 0, b: 0, a: 255 },
            }
        };
        // 그림자 상태를 op 스트림에 흘려보낸다 (캔버스는 상태 기계다).
        // shadowColor/Blur/OffsetX/Y 는 프로퍼티로 **있기만 하고 아무도 안 읽었다** —
        // 그림자를 지정해도 아무 일도 안 일어났다.
        {
            let sc = match ctx.borrow().get("shadowColor") {
                Some(Value::Str(s)) => crate::css::parse_color(s)
                    .unwrap_or(crate::css::Color { r: 0, g: 0, b: 0, a: 0 }),
                _ => crate::css::Color { r: 0, g: 0, b: 0, a: 0 },
            };
            let n = |k: &str| match ctx.borrow().get(k) {
                Some(Value::Num(v)) => *v as f32,
                _ => 0.0,
            };
            let next = CanvasOp::SetShadow {
                color: sc,
                blur: n("shadowBlur"),
                dx: n("shadowOffsetX"),
                dy: n("shadowOffsetY"),
            };
            let ops = self.canvas_cmds.entry(canvas_id).or_default();
            // 마지막으로 흘려보낸 상태와 같으면 다시 넣지 않는다
            let same = ops.iter().rev().find_map(|o| match o {
                CanvasOp::SetShadow { color, blur, dx, dy } => {
                    Some((*color, *blur, *dx, *dy))
                }
                _ => None,
            });
            let cur = match &next {
                CanvasOp::SetShadow { color, blur, dx, dy } => (*color, *blur, *dx, *dy),
                _ => unreachable!(),
            };
            if same != Some(cur) && (cur.0.a > 0 || same.is_some()) {
                ops.push(next);
            }
        }
        // 현재 변환 행렬(CTM). 캔버스는 상태 기계다 — translate/rotate/scale 이
        // 이후 그리기에 실제로 적용돼야 한다. 예전엔 전부 조용한 no-op 이라
        // 그림이 엉뚱한 자리에 그려지거나 사라졌다 (아무 말도 없이).
        let get_ctm = |ctx: &Rc<RefCell<ObjMap>>| -> crate::layout::Mat {
            match ctx.borrow().get("\u{0}ctm") {
                Some(Value::Arr(a)) => {
                    let v = a.borrow();
                    let g = |i: usize| v.get(i).map(to_num).unwrap_or(0.0) as f32;
                    crate::layout::Mat { a: g(0), b: g(1), c: g(2), d: g(3), e: g(4), f: g(5) }
                }
                _ => crate::layout::Mat::IDENTITY,
            }
        };
        let set_ctm = |ctx: &Rc<RefCell<ObjMap>>, m: crate::layout::Mat| {
            let v = vec![
                Value::Num(m.a as f64),
                Value::Num(m.b as f64),
                Value::Num(m.c as f64),
                Value::Num(m.d as f64),
                Value::Num(m.e as f64),
                Value::Num(m.f as f64),
            ];
            ctx.borrow_mut().insert("\u{0}ctm".to_string(), Value::Arr(ArrayObj::new(v)));
        };
        let alpha = |ctx: &Rc<RefCell<ObjMap>>| -> f32 {
            match ctx.borrow().get("globalAlpha") {
                Some(Value::Num(n)) => (*n as f32).clamp(0.0, 1.0),
                _ => 1.0,
            }
        };
        // globalAlpha 는 색의 알파에 곱해진다 (표준)
        let with_alpha = |c: crate::css::Color, a: f32| crate::css::Color {
            r: c.r,
            g: c.g,
            b: c.b,
            a: ((c.a as f32) * a).round().clamp(0.0, 255.0) as u8,
        };
        let font_px_of = |ctx: &Rc<RefCell<ObjMap>>| -> f32 {
            match ctx.borrow().get("font") {
                Some(Value::Str(f)) => font_px(f),
                _ => 10.0,
            }
        };
        // 텍스트 폭 (실제 폰트 메트릭). 폰트가 없으면 근사.
        let text_width = |text: &str, px: f32, ctx_fonts: Option<&crate::font::FontStack>| -> f32 {
            match ctx_fonts {
                Some(fonts) => text
                    .chars()
                    .map(|ch| {
                        let (fi, gid) = fonts.glyph_for(ch);
                        let f = fonts.font(fi);
                        f.advance_width(gid) as f32 * (px / f.units_per_em() as f32)
                    })
                    .sum(),
                None => text.chars().count() as f32 * px * 0.5,
            }
        };
        let fonts_ptr: Option<&crate::font::FontStack> =
            self.layout_ctx.as_ref().map(|c| unsafe { &*c.fonts });

        let a = alpha(&ctx);
        let cur_m = get_ctm(&ctx);

        // fillStyle/strokeStyle 이 그라디언트·패턴 객체면 그걸 쓴다 (문자열이면 색).
        // 예전엔 createLinearGradient 가 no-op 이라 그라디언트 채우기가 통째로 사라졌다.
        let paint_source = |ctx: &Rc<RefCell<ObjMap>>, key: &str| -> Option<Value> {
            match ctx.borrow().get(key) {
                Some(v @ Value::Obj(o)) if o.borrow().contains_key("\u{0}grad")
                    || o.borrow().contains_key("\u{0}pattern") =>
                {
                    Some(v.clone())
                }
                _ => None,
            }
        };
        // 그라디언트 객체 → (kind, stops)
        let grad_of = |v: &Value| -> Option<(crate::paint::CanvasGrad, Vec<(crate::css::Color, f32)>)> {
            let Value::Obj(o) = v else { return None };
            let b = o.borrow();
            let Some(Value::Arr(p)) = b.get("\u{0}grad") else { return None };
            let pv = p.borrow();
            let g = |i: usize| pv.get(i).map(to_num).unwrap_or(0.0) as f32;
            let radial = pv.len() >= 6;
            let kind = if radial {
                crate::paint::CanvasGrad::Radial {
                    x0: g(0), y0: g(1), r0: g(2), x1: g(3), y1: g(4), r1: g(5),
                }
            } else {
                crate::paint::CanvasGrad::Linear { x0: g(0), y0: g(1), x1: g(2), y1: g(3) }
            };
            let mut stops: Vec<(crate::css::Color, f32)> = Vec::new();
            if let Some(Value::Arr(st)) = b.get("\u{0}stops") {
                for e in st.borrow().iter() {
                    if let Value::Arr(pair) = e {
                        let pv = pair.borrow();
                        let pos = pv.first().map(to_num).unwrap_or(0.0) as f32;
                        let col = pv
                            .get(1)
                            .map(to_display)
                            .and_then(|s| crate::css::parse_color(&s))
                            .unwrap_or(crate::css::Color { r: 0, g: 0, b: 0, a: 255 });
                        stops.push((col, pos));
                    }
                }
            }
            stops.sort_by(|x, y| x.1.partial_cmp(&y.1).unwrap_or(std::cmp::Ordering::Equal));
            Some((kind, stops))
        };
        let pattern_of = |v: &Value| -> Option<(usize, bool)> {
            let Value::Obj(o) = v else { return None };
            let b = o.borrow();
            let Some(Value::Arr(p)) = b.get("\u{0}pattern") else { return None };
            let pv = p.borrow();
            let idx = pv.first().map(to_num)? as usize;
            let repeat = pv.get(1).map(to_bool).unwrap_or(true);
            Some((idx, repeat))
        };
        // 다각형의 경계 상자
        let bbox = |pts: &[(f32, f32)]| -> crate::layout::Rect {
            let (mut x0, mut y0, mut x1, mut y1) =
                (f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
            for &(x, y) in pts {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
            crate::layout::Rect { x: x0, y: y0, width: (x1 - x0).max(0.0), height: (y1 - y0).max(0.0) }
        };

        match method {
            // ── 그라디언트 / 패턴 객체 ──
            CreateLinearGradient | CreateRadialGradient => {
                let n = if matches!(method, CreateRadialGradient) { 6 } else { 4 };
                let params: Vec<Value> = (0..n).map(|i| Value::Num(num(i) as f64)).collect();
                let mut g = ObjMap::new();
                g.insert("\u{0}grad".to_string(), Value::Arr(ArrayObj::new(params)));
                g.insert("\u{0}stops".to_string(), Value::Arr(ArrayObj::new(Vec::new())));
                g.insert("addColorStop".to_string(), Value::Native(Native::Canvas(AddColorStop)));
                return Ok(Value::Obj(Rc::new(RefCell::new(g))));
            }
            AddColorStop => {}
            CreatePattern => {
                let src = match args.first() {
                    Some(Value::Dom(id)) => {
                        let dom = self.dom_arena()?;
                        match &dom.get(*id).node_type {
                            crate::dom::NodeType::Element(e) => e.attributes.get("src").cloned(),
                            _ => None,
                        }
                    }
                    _ => None,
                };
                let idx = src.as_ref().and_then(|raw| {
                    let abs = self.absolute_url(raw);
                    self.layout_ctx.as_ref().and_then(|c| unsafe {
                        (*c.img_map).get(&abs).or_else(|| (*c.img_map).get(raw)).map(|(i, _, _)| *i)
                    })
                });
                let Some(idx) = idx else {
                    self.canvas_warn("createPattern 의 소스를 찾지 못했다 (<img> 요소만 지원)");
                    return Ok(Value::Null);
                };
                let rep = args.get(1).map(to_display).unwrap_or_else(|| "repeat".to_string());
                let mut p = ObjMap::new();
                p.insert(
                    "\u{0}pattern".to_string(),
                    Value::Arr(ArrayObj::new(vec![
                        Value::Num(idx as f64),
                        Value::Bool(rep != "no-repeat"),
                    ])),
                );
                return Ok(Value::Obj(Rc::new(RefCell::new(p))));
            }
            // ── ImageData (실제 픽셀) ──
            // 캔버스의 픽셀을 읽으려면 **진짜로 그려 봐야** 한다. 지금까지의 명령을
            // 오프스크린으로 래스터화해서 그 영역을 잘라 준다.
            GetImageData => {
                let (sx, sy, sw, sh) = (num(0), num(1), num(2), num(3));
                let (sw, sh) = (sw.max(0.0) as usize, sh.max(0.0) as usize);
                if sw == 0 || sh == 0 {
                    return Ok(Value::Null);
                }
                let Some(lc) = self.layout_ctx.as_ref() else {
                    self.canvas_warn("getImageData 는 렌더 컨텍스트가 필요하다");
                    return Ok(Value::Null);
                };
                let (fonts, images) = unsafe { (&*lc.fonts, &*lc.images) };
                let ops = self.canvas_cmds.get(&canvas_id).cloned().unwrap_or_default();
                // 캔버스 크기
                let (cw, ch) = {
                    let dom = self.dom_arena()?;
                    match &dom.get(canvas_id).node_type {
                        crate::dom::NodeType::Element(e) => (
                            e.attributes
                                .get("width")
                                .and_then(|v| v.parse::<usize>().ok())
                                .unwrap_or(300),
                            e.attributes
                                .get("height")
                                .and_then(|v| v.parse::<usize>().ok())
                                .unwrap_or(150),
                        ),
                        _ => (300, 150),
                    }
                };
                let items = crate::window::canvas_items_at_origin(&ops, fonts);
                let img = crate::paint::rasterize_items(&items, cw, ch, fonts, images);
                let mut data: Vec<Value> = Vec::with_capacity(sw * sh * 4);
                for y in 0..sh {
                    for x in 0..sw {
                        let (px0, py0) = (sx as usize + x, sy as usize + y);
                        if px0 < cw && py0 < ch {
                            let o = (py0 * cw + px0) * 4;
                            for k in 0..4 {
                                data.push(Value::Num(img.rgba[o + k] as f64));
                            }
                        } else {
                            for _ in 0..4 {
                                data.push(Value::Num(0.0));
                            }
                        }
                    }
                }
                let mut m = ObjMap::new();
                m.insert("width".to_string(), Value::Num(sw as f64));
                m.insert("height".to_string(), Value::Num(sh as f64));
                m.insert("data".to_string(), Value::Arr(ArrayObj::new(data)));
                return Ok(Value::Obj(Rc::new(RefCell::new(m))));
            }
            CreateImageData => {
                let (w0, h0) = (num(0).max(0.0) as usize, num(1).max(0.0) as usize);
                let data: Vec<Value> = vec![Value::Num(0.0); w0 * h0 * 4];
                let mut m = ObjMap::new();
                m.insert("width".to_string(), Value::Num(w0 as f64));
                m.insert("height".to_string(), Value::Num(h0 as f64));
                m.insert("data".to_string(), Value::Arr(ArrayObj::new(data)));
                return Ok(Value::Obj(Rc::new(RefCell::new(m))));
            }
            PutImageData => {
                let Some(Value::Obj(d)) = args.first() else { return Ok(Value::Undefined) };
                let b = d.borrow();
                let w0 = b.get("width").map(to_num).unwrap_or(0.0) as usize;
                let h0 = b.get("height").map(to_num).unwrap_or(0.0) as usize;
                let Some(Value::Arr(px)) = b.get("data") else { return Ok(Value::Undefined) };
                let vals = px.borrow();
                if w0 == 0 || h0 == 0 || vals.len() < w0 * h0 * 4 {
                    return Ok(Value::Undefined);
                }
                let rgba: Vec<u8> = vals
                    .iter()
                    .take(w0 * h0 * 4)
                    .map(|v| to_num(v).clamp(0.0, 255.0) as u8)
                    .collect();
                drop(vals);
                drop(b);
                let img = std::rc::Rc::new(crate::png::Image { width: w0, height: h0, rgba });
                self.canvas_cmds.entry(canvas_id).or_default().push(CanvasOp::PutImage {
                    x: num(1),
                    y: num(2),
                    img,
                });
            }
            // ── 클립 ──
            Clip => {
                let pts = get_path(&ctx);
                if pts.len() < 3 {
                    self.canvas_warn("clip() 에 경로가 없다");
                    return Ok(Value::Undefined);
                }
                // 클립도 그리기 상태다 — save/restore 로 복원돼야 한다 (표준).
                let flat: Vec<Value> = pts
                    .iter()
                    .flat_map(|&(x, y)| [Value::Num(x as f64), Value::Num(y as f64)])
                    .collect();
                ctx.borrow_mut()
                    .insert("\u{0}clip".to_string(), Value::Arr(ArrayObj::new(flat)));
                self.canvas_cmds
                    .entry(canvas_id)
                    .or_default()
                    .push(CanvasOp::Clip { pts: Some(pts) });
            }
            // ── 곡선 ──
            BezierCurveTo | QuadraticCurveTo => {
                let path = get_path(&ctx);
                let Some(&(px0, py0)) = path.last() else {
                    return Ok(Value::Undefined); // 시작점이 없으면 무시 (표준)
                };
                let seg = 20;
                for k in 1..=seg {
                    let t = k as f32 / seg as f32;
                    let (x, y) = if matches!(method, BezierCurveTo) {
                        let (c1x, c1y, c2x, c2y, ex, ey) =
                            (num(0), num(1), num(2), num(3), num(4), num(5));
                        let u = 1.0 - t;
                        (
                            u * u * u * px0 + 3.0 * u * u * t * c1x + 3.0 * u * t * t * c2x + t * t * t * ex,
                            u * u * u * py0 + 3.0 * u * u * t * c1y + 3.0 * u * t * t * c2y + t * t * t * ey,
                        )
                    } else {
                        let (cx, cy, ex, ey) = (num(0), num(1), num(2), num(3));
                        let u = 1.0 - t;
                        (
                            u * u * px0 + 2.0 * u * t * cx + t * t * ex,
                            u * u * py0 + 2.0 * u * t * cy + t * t * ey,
                        )
                    };
                    push_path(&ctx, x, y);
                }
            }
            // ── 변환 상태 ──
            Translate | Rotate | Scale | Transform | SetTransform | ResetTransform => {
                use crate::layout::Mat;
                let m = match method {
                    Translate => Mat { e: num(0), f: num(1), ..Mat::IDENTITY },
                    Rotate => {
                        let t = num(0);
                        Mat { a: t.cos(), b: t.sin(), c: -t.sin(), d: t.cos(), e: 0.0, f: 0.0 }
                    }
                    Scale => Mat { a: num(0), d: num(1), ..Mat::IDENTITY },
                    _ => Mat {
                        a: num(0),
                        b: num(1),
                        c: num(2),
                        d: num(3),
                        e: num(4),
                        f: num(5),
                    },
                };
                let new_m = match method {
                    // setTransform/resetTransform 은 CTM 을 **대체**한다
                    SetTransform => m,
                    ResetTransform => Mat::IDENTITY,
                    // 나머지는 현재 CTM 에 **누적**된다 (새 변환이 먼저 적용)
                    _ => m.then(&cur_m),
                };
                set_ctm(&ctx, new_m);
                self.canvas_cmds
                    .entry(canvas_id)
                    .or_default()
                    .push(CanvasOp::SetTransform { m: new_m });
            }
            Save => {
                // 상태 전체를 스택에 (CTM + 스타일)
                let snap = vec![
                    ctx.borrow().get("\u{0}ctm").cloned().unwrap_or(Value::Undefined),
                    ctx.borrow().get("fillStyle").cloned().unwrap_or(Value::Undefined),
                    ctx.borrow().get("strokeStyle").cloned().unwrap_or(Value::Undefined),
                    ctx.borrow().get("lineWidth").cloned().unwrap_or(Value::Undefined),
                    ctx.borrow().get("font").cloned().unwrap_or(Value::Undefined),
                    ctx.borrow().get("globalAlpha").cloned().unwrap_or(Value::Undefined),
                    ctx.borrow().get("\u{0}clip").cloned().unwrap_or(Value::Null),
                ];
                let stack = match ctx.borrow().get("\u{0}stack") {
                    Some(Value::Arr(st)) => Some(st.clone()),
                    _ => None,
                };
                match stack {
                    Some(st) => st.borrow_mut().push(Value::Arr(ArrayObj::new(snap))),
                    None => {
                        ctx.borrow_mut().insert(
                            "\u{0}stack".to_string(),
                            Value::Arr(ArrayObj::new(vec![Value::Arr(ArrayObj::new(snap))])),
                        );
                    }
                }
            }
            Restore => {
                let popped = match ctx.borrow().get("\u{0}stack") {
                    Some(Value::Arr(st)) => st.borrow_mut().pop(),
                    _ => None,
                };
                if let Some(Value::Arr(snap)) = popped {
                    let v = snap.borrow().clone();
                    let keys = [
                        "\u{0}ctm",
                        "fillStyle",
                        "strokeStyle",
                        "lineWidth",
                        "font",
                        "globalAlpha",
                    ];
                    for (k, val) in keys.iter().zip(v.iter().cloned()) {
                        if !matches!(val, Value::Undefined) {
                            ctx.borrow_mut().insert(k.to_string(), val);
                        }
                    }
                    // 클립 복원 (Null 이면 클립 해제). 예전엔 restore 가 클립을 되돌리지
                    // 않아서, 그 뒤 그리기가 전부 옛 클립에 갇혀 사라졌다.
                    let saved_clip = v.get(6).cloned().unwrap_or(Value::Null);
                    let pts = match &saved_clip {
                        Value::Arr(a) => {
                            let f = a.borrow();
                            let mut out = Vec::new();
                            let mut i = 0;
                            while i + 1 < f.len() {
                                out.push((to_num(&f[i]) as f32, to_num(&f[i + 1]) as f32));
                                i += 2;
                            }
                            Some(out)
                        }
                        _ => None,
                    };
                    ctx.borrow_mut().insert("\u{0}clip".to_string(), saved_clip);
                    let m = get_ctm(&ctx);
                    let ops = self.canvas_cmds.entry(canvas_id).or_default();
                    ops.push(CanvasOp::SetTransform { m });
                    ops.push(CanvasOp::Clip { pts });
                }
            }
            // ── 측정 ──
            MeasureText => {
                let text = args.first().map(to_display).unwrap_or_default();
                let px = font_px_of(&ctx);
                let w = text_width(&text, px, fonts_ptr);
                let mut m = ObjMap::new();
                m.insert("width".to_string(), Value::Num(w as f64));
                m.insert("actualBoundingBoxAscent".to_string(), Value::Num((px * 0.8) as f64));
                m.insert("actualBoundingBoxDescent".to_string(), Value::Num((px * 0.2) as f64));
                return Ok(Value::Obj(Rc::new(RefCell::new(m))));
            }
            // ── 이미지 ──
            DrawImage => {
                // drawImage(img, dx, dy [, dw, dh]) — <img> 요소만 지원 (캔버스 소스는 미지원).
                // 이미지 맵은 src(절대 URL) → (인덱스, 폭, 높이) 다.
                let src = match args.first() {
                    Some(Value::Dom(id)) => {
                        let dom = self.dom_arena()?;
                        match &dom.get(*id).node_type {
                            crate::dom::NodeType::Element(e) => e.attributes.get("src").cloned(),
                            _ => None,
                        }
                    }
                    _ => None,
                };
                let idx = src.as_ref().and_then(|raw| {
                    let abs = self.absolute_url(raw);
                    self.layout_ctx.as_ref().and_then(|c| unsafe {
                        (*c.img_map)
                            .get(&abs)
                            .or_else(|| (*c.img_map).get(raw))
                            .map(|(i, _, _)| *i)
                    })
                });
                let Some(idx) = idx else {
                    self.canvas_warn("drawImage 의 소스를 찾지 못했다 (<img> 요소만 지원)");
                    return Ok(Value::Undefined);
                };
                let (dx, dy) = (num(1), num(2));
                let (dw, dh) = if args.len() >= 5 {
                    (num(3), num(4))
                } else {
                    (0.0, 0.0) // 0 이면 호스트가 고유 크기로 그린다
                };
                self.canvas_cmds
                    .entry(canvas_id)
                    .or_default()
                    .push(CanvasOp::DrawImage { idx, x: dx, y: dy, w: dw, h: dh });
            }
            // ── 경로 ──
            Ellipse => {
                // ellipse(cx, cy, rx, ry, rot, start, end)
                let (cx, cy, rx, ry) = (num(0), num(1), num(2), num(3));
                let rot = num(4);
                let (s, e) = (num(5), num(6));
                for k in 0..=32 {
                    let t = s + (e - s) * k as f32 / 32.0;
                    let (px0, py0) = (rx * t.cos(), ry * t.sin());
                    let x = cx + px0 * rot.cos() - py0 * rot.sin();
                    let y = cy + px0 * rot.sin() + py0 * rot.cos();
                    push_path(&ctx, x, y);
                }
            }
            RoundRect => {
                let (x, y, w, h) = (num(0), num(1), num(2), num(3));
                let r = args.get(4).map(to_num).unwrap_or(0.0) as f32;
                let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
                let corner = |cx: f32, cy: f32, a0: f32, a1: f32, out: &mut Vec<(f32, f32)>| {
                    for k in 0..=6 {
                        let t = a0 + (a1 - a0) * k as f32 / 6.0;
                        out.push((cx + r * t.cos(), cy + r * t.sin()));
                    }
                };
                let mut pts = Vec::new();
                use std::f32::consts::PI;
                corner(x + w - r, y + r, -PI / 2.0, 0.0, &mut pts);
                corner(x + w - r, y + h - r, 0.0, PI / 2.0, &mut pts);
                corner(x + r, y + h - r, PI / 2.0, PI, &mut pts);
                corner(x + r, y + r, PI, 1.5 * PI, &mut pts);
                for (px0, py0) in pts {
                    push_path(&ctx, px0, py0);
                }
            }
            FillRect => {
                let rect = crate::layout::Rect {
                    x: num(0),
                    y: num(1),
                    width: num(2),
                    height: num(3),
                };
                let src = paint_source(&ctx, "fillStyle");
                let op = match src.as_ref().and_then(|v| grad_of(v)) {
                    Some((kind, stops)) => CanvasOp::FillGradient { rect, shape: None, kind, stops },
                    None => match src.as_ref().and_then(|v| pattern_of(v)) {
                        Some((idx, repeat)) => {
                            CanvasOp::FillPattern { rect, shape: None, idx, repeat }
                        }
                        None => CanvasOp::FillRect {
                            x: rect.x,
                            y: rect.y,
                            w: rect.width,
                            h: rect.height,
                            color: with_alpha(style("fillStyle"), a),
                        },
                    },
                };
                self.canvas_cmds.entry(canvas_id).or_default().push(op);
            }
            ClearRect => self
                .canvas_cmds
                .entry(canvas_id)
                .or_default()
                .push(CanvasOp::ClearRect { x: num(0), y: num(1), w: num(2), h: num(3) }),
            StrokeRect => {
                let lw = match ctx.borrow().get("lineWidth") {
                    Some(Value::Num(n)) => *n as f32,
                    _ => 1.0,
                };
                self.canvas_cmds.entry(canvas_id).or_default().push(CanvasOp::StrokeRect {
                    x: num(0),
                    y: num(1),
                    w: num(2),
                    h: num(3),
                    color: with_alpha(style("strokeStyle"), a),
                    lw,
                });
            }
            FillText => {
                let text = args.first().map(to_display).unwrap_or_default();
                let px = font_px_of(&ctx);
                // textAlign/textBaseline 을 실제로 반영한다 (표준). 예전엔 속성 자체가 없어
                // 가운데 정렬한 텍스트가 왼쪽으로 밀렸다.
                let w = text_width(&text, px, fonts_ptr);
                let align = match ctx.borrow().get("textAlign") {
                    Some(Value::Str(s)) => s.clone(),
                    _ => "start".to_string(),
                };
                let dx = match align.as_str() {
                    "center" => -w / 2.0,
                    "right" | "end" => -w,
                    _ => 0.0,
                };
                let baseline = match ctx.borrow().get("textBaseline") {
                    Some(Value::Str(s)) => s.clone(),
                    _ => "alphabetic".to_string(),
                };
                let dy = match baseline.as_str() {
                    "top" | "hanging" => px * 0.8,
                    "middle" => px * 0.3,
                    "bottom" | "ideographic" => -px * 0.2,
                    _ => 0.0,
                };
                self.canvas_cmds.entry(canvas_id).or_default().push(CanvasOp::FillText {
                    text,
                    x: num(0) + dx,
                    y: num(1) + dy,
                    color: with_alpha(style("fillStyle"), a),
                    px,
                });
            }
            // 경로: __path 에 점을 쌓았다가 fill/stroke 시 폴리곤으로.
            BeginPath => set_path(&ctx, Vec::new()),
            MoveTo | LineTo => push_path(&ctx, num(0), num(1)),
            Rect => {
                let (x, y, w, h) = (num(0), num(1), num(2), num(3));
                for (px0, py0) in [(x, y), (x + w, y), (x + w, y + h), (x, y + h)] {
                    push_path(&ctx, px0, py0);
                }
            }
            Arc => {
                let (cx, cy, r) = (num(0), num(1), num(2));
                let (s, e) = (num(3), num(4));
                let seg = 24;
                for k in 0..=seg {
                    let t = s + (e - s) * k as f32 / seg as f32;
                    push_path(&ctx, cx + r * t.cos(), cy + r * t.sin());
                }
            }
            ClosePath => {}
            Fill => {
                let pts = get_path(&ctx);
                if pts.len() >= 3 {
                    let src = paint_source(&ctx, "fillStyle");
                    let rect = bbox(&pts);
                    let op = match src.as_ref().and_then(|v| grad_of(v)) {
                        Some((kind, stops)) => {
                            CanvasOp::FillGradient { rect, shape: Some(pts), kind, stops }
                        }
                        None => match src.as_ref().and_then(|v| pattern_of(v)) {
                            Some((idx, repeat)) => {
                                CanvasOp::FillPattern { rect, shape: Some(pts), idx, repeat }
                            }
                            None => CanvasOp::FillPath {
                                pts,
                                color: with_alpha(style("fillStyle"), a),
                            },
                        },
                    };
                    self.canvas_cmds.entry(canvas_id).or_default().push(op);
                }
            }
            // 경로 스트로크: 각 선분을 두께만큼의 사각형(폴리곤)으로 그린다.
            // 예전엔 통째로 무시돼서 stroke() 한 그림이 아예 안 나왔다.
            Stroke => {
                let pts = get_path(&ctx);
                let lw = match ctx.borrow().get("lineWidth") {
                    Some(Value::Num(n)) => (*n as f32).max(1.0),
                    _ => 1.0,
                };
                // lineCap/lineJoin 은 프로퍼티로 **있기만 하고 아무도 안 읽었다**
                // (round 캡을 지정해도 butt 로 나왔다 — 속성은 있는데 아무 일도 안 하는 거짓말).
                let cap = match ctx.borrow().get("lineCap") {
                    Some(Value::Str(s)) => s.clone(),
                    _ => "butt".to_string(),
                };
                let join = match ctx.borrow().get("lineJoin") {
                    Some(Value::Str(s)) => s.clone(),
                    _ => "miter".to_string(),
                };
                let color = with_alpha(style("strokeStyle"), a);
                let ops = self.canvas_cmds.entry(canvas_id).or_default();
                let n = pts.len();
                for (i, w) in pts.windows(2).enumerate() {
                    let ((mut x0, mut y0), (mut x1, mut y1)) = (w[0], w[1]);
                    let (dx, dy) = (x1 - x0, y1 - y0);
                    let len = (dx * dx + dy * dy).sqrt();
                    if len < 0.01 {
                        continue;
                    }
                    let (ux, uy) = (dx / len, dy / len);
                    // square 캡: 끝을 반두께만큼 연장 (표준)
                    if cap == "square" {
                        if i == 0 {
                            x0 -= ux * lw / 2.0;
                            y0 -= uy * lw / 2.0;
                        }
                        if i + 2 == n {
                            x1 += ux * lw / 2.0;
                            y1 += uy * lw / 2.0;
                        }
                    }
                    let (nx, ny) = (-uy * lw / 2.0, ux * lw / 2.0);
                    ops.push(CanvasOp::FillPath {
                        pts: vec![
                            (x0 + nx, y0 + ny),
                            (x1 + nx, y1 + ny),
                            (x1 - nx, y1 - ny),
                            (x0 - nx, y0 - ny),
                        ],
                        color,
                    });
                    // round 캡/조인: 끝점/이음새에 반지름 lw/2 의 원을 얹는다
                    let circle = |cx: f32, cy: f32| -> Vec<(f32, f32)> {
                        (0..16)
                            .map(|k| {
                                let t = k as f32 / 16.0 * std::f32::consts::TAU;
                                (cx + t.cos() * lw / 2.0, cy + t.sin() * lw / 2.0)
                            })
                            .collect()
                    };
                    if cap == "round" {
                        if i == 0 {
                            ops.push(CanvasOp::FillPath { pts: circle(w[0].0, w[0].1), color });
                        }
                        if i + 2 == n {
                            ops.push(CanvasOp::FillPath { pts: circle(w[1].0, w[1].1), color });
                        }
                    }
                    // 이음새 (마지막 선분 제외). 아무것도 안 하면 바깥쪽에 V 자 홈이 남는다.
                    if i + 2 < n {
                        let (jx, jy) = w[1];
                        let (x2, y2) = pts[i + 2];
                        let (dx2, dy2) = (x2 - jx, y2 - jy);
                        let l2 = (dx2 * dx2 + dy2 * dy2).sqrt();
                        if l2 < 0.01 {
                            continue;
                        }
                        let (ux2, uy2) = (dx2 / l2, dy2 / l2);
                        match join.as_str() {
                            "round" => ops.push(CanvasOp::FillPath { pts: circle(jx, jy), color }),
                            _ => {
                                // 바깥쪽 코너 두 점 (양쪽 다 채워도 안쪽은 이미 덮여 있어 무해)
                                let (n1x, n1y) = (-uy * lw / 2.0, ux * lw / 2.0);
                                let (n2x, n2y) = (-uy2 * lw / 2.0, ux2 * lw / 2.0);
                                for sgn in [1.0f32, -1.0] {
                                    let a1 = (jx + n1x * sgn, jy + n1y * sgn);
                                    let a2 = (jx + n2x * sgn, jy + n2y * sgn);
                                    // 마이터 점 = 두 오프셋 선의 교점 (평행이면 없음)
                                    let cross = ux * uy2 - uy * ux2;
                                    let mut poly = vec![(jx, jy), a1, a2];
                                    if join == "miter" && cross.abs() > 1e-4 {
                                        // a1 + t·u = a2 + s·u2 를 풀어 교점
                                        let t = ((a2.0 - a1.0) * uy2 - (a2.1 - a1.1) * ux2) / cross;
                                        let m = (a1.0 + ux * t, a1.1 + uy * t);
                                        // 마이터 길이 한계 (기본 10) — 넘으면 bevel 로
                                        let ml = ((m.0 - jx).powi(2) + (m.1 - jy).powi(2)).sqrt()
                                            / (lw / 2.0);
                                        if ml <= 10.0 {
                                            poly = vec![(jx, jy), a1, m, a2];
                                        }
                                    }
                                    ops.push(CanvasOp::FillPath { pts: poly, color });
                                }
                            }
                        }
                    }
                }
            }
            Noop => {}
        }
        Ok(Value::Undefined)
    }

    // 캔버스 미지원 기능 경고 (같은 메시지는 한 번만)
    fn canvas_warn(&mut self, msg: &str) {
        if self.canvas_warned.insert(msg.to_string()) {
            println!("[canvas] {}", msg);
        }
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

    // 상대 URL 을 페이지 기준으로 해석
    fn resolve_url(&self, url: &str) -> String {
        if let Some(base) = &self.base_url {
            if let Ok(b) = crate::url::Url::parse(base) {
                if let Some(u) = b.join(url) {
                    return u.as_string();
                }
            }
        }
        url.to_string()
    }

    // new XMLHttpRequest() → 메서드/속성을 가진 객체
    pub(super) fn make_xhr(&self) -> Value {
        let mut m = ObjMap::new();
        m.insert("\u{0}isXhr".to_string(), Value::Bool(true));
        m.insert("readyState".to_string(), Value::Num(0.0));
        m.insert("status".to_string(), Value::Num(0.0));
        m.insert("statusText".to_string(), Value::Str(String::new()));
        m.insert("responseText".to_string(), Value::Str(String::new()));
        m.insert("response".to_string(), Value::Str(String::new()));
        m.insert("responseType".to_string(), Value::Str(String::new()));
        m.insert("open".to_string(), Value::Native(Native::XhrOpen));
        m.insert("send".to_string(), Value::Native(Native::XhrSend));
        m.insert("setRequestHeader".to_string(), Value::Native(Native::XhrSetHeader));
        m.insert("getResponseHeader".to_string(), Value::Native(Native::XhrGetHeader));
        m.insert("getAllResponseHeaders".to_string(), Value::Native(Native::XhrGetHeader));
        m.insert("abort".to_string(), Value::Native(Native::Noop));
        m.insert("addEventListener".to_string(), Value::Native(Native::AddEventListener));
        m.insert("removeEventListener".to_string(), Value::Native(Native::RemoveEventListener));
        m.insert("dispatchEvent".to_string(), Value::Native(Native::DispatchEvent));
        Value::Obj(Rc::new(RefCell::new(m)))
    }

    // 체크박스/라디오면 checked 를 뒤집는다. 뒤집었으면 true.
    fn pre_click_toggle(&mut self, id: crate::dom::NodeId) -> bool {
        let Ok(dom) = self.dom_arena() else { return false };
        let crate::dom::NodeType::Element(e) = &dom.get(id).node_type else { return false };
        if e.tag_name != "input" {
            return false;
        }
        let ty = e.attributes.get("type").map(|t| t.to_ascii_lowercase()).unwrap_or_default();
        if ty != "checkbox" && ty != "radio" {
            return false;
        }
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
            // 제출 버튼: 폼에 submit 이벤트를 쏜다 (취소되면 아무 일도 없다)
            "button" | "input" if ty == "submit" || (tag == "button" && ty.is_empty()) => {
                let form = self.owner_form(id);
                if let Some(f) = form {
                    let evt = self.make_event("submit", f);
                    self.dispatch_event_value(f, "submit", evt);
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

    // new WebSocket(url [, protocols]) — 진짜로 연결한다 (RFC 6455 핸드셰이크).
    // 실패하면 error/close 이벤트를 쏜다 (표준: 생성자는 throw 하지 않는다 —
    // 연결 실패는 비동기 이벤트다. 여기서 throw 하면 스크립트가 통째로 죽는다).
    fn make_websocket(&mut self, args: Vec<Value>) -> Value {
        let raw = args.first().map(to_display).unwrap_or_default();
        let url = self.absolute_url(&raw);
        // http(s) 기준 URL 로 절대화됐으면 ws(s) 로 되돌린다
        let url = if raw.starts_with("ws://") || raw.starts_with("wss://") {
            raw.clone()
        } else if let Some(rest) = url.strip_prefix("https://") {
            format!("wss://{}", rest)
        } else if let Some(rest) = url.strip_prefix("http://") {
            format!("ws://{}", rest)
        } else {
            url
        };

        let mut m = ObjMap::new();
        m.insert("url".to_string(), Value::Str(url.clone()));
        m.insert("readyState".to_string(), Value::Num(0.0)); // CONNECTING
        m.insert("bufferedAmount".to_string(), Value::Num(0.0));
        m.insert("protocol".to_string(), Value::Str(String::new()));
        m.insert("binaryType".to_string(), Value::Str("blob".to_string()));
        m.insert("send".to_string(), Value::Native(Native::WsSend));
        m.insert("close".to_string(), Value::Native(Native::WsClose));
        m.insert("addEventListener".to_string(), Value::Native(Native::AddEventListener));
        m.insert("removeEventListener".to_string(), Value::Native(Native::RemoveEventListener));
        m.insert("dispatchEvent".to_string(), Value::Native(Native::DispatchEvent));
        // 표준 상수
        m.insert("CONNECTING".to_string(), Value::Num(0.0));
        m.insert("OPEN".to_string(), Value::Num(1.0));
        m.insert("CLOSING".to_string(), Value::Num(2.0));
        m.insert("CLOSED".to_string(), Value::Num(3.0));
        let obj = Rc::new(RefCell::new(m));
        let val = Value::Obj(obj.clone());

        match crate::websocket::WebSocket::connect(&url) {
            Ok(ws) => {
                obj.borrow_mut().insert("readyState".to_string(), Value::Num(1.0));
                obj.borrow_mut()
                    .insert("protocol".to_string(), Value::Str(ws.protocol.clone()));
                obj.borrow_mut()
                    .insert("\u{0}sock".to_string(), Value::Num(self.sockets.len() as f64));
                self.sockets.push((ws, val.clone()));
                // open 이벤트는 마이크로태스크 뒤(핸들러 등록 후)에 와야 한다 —
                // 지금 부르면 onopen 을 붙이기도 전이라 아무도 못 듣는다.
                self.pending_ws_open.push(val.clone());
            }
            Err(e) => {
                self.console.push(format!("WebSocket 연결 실패: {}", e));
                obj.borrow_mut().insert("readyState".to_string(), Value::Num(3.0));
                self.pending_ws_error.push(val.clone());
            }
        }
        val
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

    fn ws_fire(&mut self, obj: &Rc<RefCell<ObjMap>>, event: &str, args: Vec<Value>) {
        let on = format!("on{}", event);
        let handler = obj.borrow().get(&on).cloned();
        if let Some(h) = handler {
            if is_callable(&h) {
                let _ = self.call_value(h, Some(Value::Obj(obj.clone())), args.clone());
            }
        }
        let listeners: Vec<Value> = match obj.borrow().get(&obj_listener_key(event)) {
            Some(Value::Arr(a)) => a.borrow().clone(),
            _ => Vec::new(),
        };
        for l in listeners {
            let _ = self.call_value(l, Some(Value::Obj(obj.clone())), args.clone());
        }
    }

    // XHR 발화: on<event> 프로퍼티 + addEventListener 로 등록된 리스너.
    // 예전엔 on<event> 만 불렀다 — addEventListener 로 붙인 load 핸들러가 영영 안 왔다.
    fn xhr_fire(&mut self, obj: &Rc<RefCell<ObjMap>>, event: &str) {
        let on = format!("on{}", event);
        let handler = obj.borrow().get(&on).cloned();
        if let Some(h) = handler {
            if is_callable(&h) {
                let _ = self.call_value(h, Some(Value::Obj(obj.clone())), Vec::new());
            }
        }
        let listeners: Vec<Value> = match obj.borrow().get(&obj_listener_key(event)) {
            Some(Value::Arr(a)) => a.borrow().clone(),
            _ => Vec::new(),
        };
        for l in listeners {
            let _ = self.call_value(l, Some(Value::Obj(obj.clone())), Vec::new());
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

    // 네이티브 호출의 유일한 관문. DOM 을 바꾼 호출이면 여기서 MutationObserver
    // 배달을 한 번 예약한다 (호출부마다 예약하면 반드시 빠뜨린다).
    // 바이트열 → 진짜 ArrayBuffer (프렐류드의 __kArrayBuffer 로 만들어 프로토타입까지 맞춘다).
    pub(super) fn make_array_buffer(&mut self, bytes: &[u8]) -> Result<Value, String> {
        let ctor = env_get(&self.global, "__kArrayBuffer")
            .ok_or("__kArrayBuffer 가 프렐류드에 없다")?;
        let buf = self.construct(ctor, vec![Value::Num(bytes.len() as f64)])?;
        if let Value::Obj(o) = &buf {
            let arr = o.borrow().get("_b").cloned();
            if let Some(Value::Arr(a)) = arr {
                let mut items = a.borrow_mut();
                for (i, b) in bytes.iter().enumerate() {
                    items[i] = Value::Num(*b as f64);
                }
            }
        }
        Ok(buf)
    }

    // wasm 안에서 memory.grow 가 일어나면 선형 메모리는 **새 배열**로 바뀐다.
    // JS 쪽 Memory 객체의 buffer 는 그대로 옛 배열을 가리키므로, 다시 묶지 않으면
    // 그 뒤로 wasm 이 쓴 값이 JS 에 아예 보이지 않는다 (조용히 틀린다).
    // JS 가 메모리를 볼 수 있는 경계 — 호출이 돌아올 때, 임포트로 JS 를 부르기 직전 — 마다 부른다.
    pub(super) fn sync_wasm_memories(&mut self) {
        for i in 0..self.wasm_memories.len() {
            let (mem, obj) = self.wasm_memories[i].clone();
            let cur = mem.borrow().clone();
            let Value::Obj(o) = &obj else { continue };
            let buf = o.borrow().get("buffer").cloned();
            if let Some(Value::Obj(b)) = &buf {
                let same = matches!(
                    b.borrow().get("_b"),
                    Some(Value::Arr(a)) if Rc::ptr_eq(a, &cur)
                );
                if same {
                    continue;
                }
                // 표준: 커진 메모리의 옛 ArrayBuffer 는 분리된다 (byteLength → 0).
                let mut bm = b.borrow_mut();
                bm.insert("_b".to_string(), Value::Arr(ArrayObj::new(Vec::new())));
                bm.insert("byteLength".to_string(), Value::Num(0.0));
            }
            // 새 배열을 감싼 ArrayBuffer 로 갈아끼운다 (배열은 공유 — 사본이 아니다)
            let len = cur.borrow().len();
            let Some(ctor) = env_get(&self.global, "__kArrayBuffer") else { continue };
            let Ok(nb) = self.construct(ctor, vec![Value::Num(0.0)]) else { continue };
            if let Value::Obj(n) = &nb {
                let mut nm = n.borrow_mut();
                nm.insert("_b".to_string(), Value::Arr(cur));
                nm.insert("byteLength".to_string(), Value::Num(len as f64));
            }
            if let Value::Obj(o) = &obj {
                o.borrow_mut().insert("buffer".to_string(), nb);
            }
        }
    }

    // 모듈 + 임포트 → 인스턴스, 그리고 exports 객체.
    // mem_idx >= 0 이면 JS 가 미리 만든 WebAssembly.Memory 를 모듈 자신의 메모리로 쓴다.
    fn wasm_instantiate(
        &mut self,
        mi: usize,
        imports: Value,
        mem_idx: f64,
    ) -> Result<Value, String> {
        use crate::wasm::{Extern, Export as WExport, ImportKind};
        let module = self
            .wasm_modules
            .get(mi)
            .cloned()
            .ok_or("wasm: 모듈 없음")?;

        // imports[모듈][이름] 조회
        let lookup = |me: &mut Self, m: &str, n: &str| -> Result<Value, String> {
            let ns = me.member_get(&imports, m)?;
            me.member_get(&ns, n)
        };

        let mut externs: Vec<Extern> = Vec::new();
        let mut import_fns: Vec<Value> = Vec::new();
        // 내보내진 memory 가 가리킬 JS Memory 객체 (임포트된 것이면 그것, 아니면 우리가 만든 것)
        let mut mem_obj = Value::Undefined;

        for imp in module.imports.clone() {
            let v = lookup(self, &imp.module, &imp.name).unwrap_or(Value::Undefined);
            match imp.kind {
                ImportKind::Func(_) => {
                    if matches!(v, Value::Undefined | Value::Null) {
                        return Err(format!(
                            "WebAssembly.LinkError: 임포트 {}.{} 가 없다",
                            imp.module, imp.name
                        ));
                    }
                    import_fns.push(v);
                    externs.push(Extern::Func);
                }
                ImportKind::Memory(min) => {
                    let idx = self.member_get(&v, "__mem")?;
                    let idx = match idx {
                        Value::Num(n) => n as usize,
                        _ => {
                            return Err(format!(
                                "WebAssembly.LinkError: 임포트 {}.{} 가 Memory 가 아니다",
                                imp.module, imp.name
                            ))
                        }
                    };
                    let (m, obj) = self
                        .wasm_memories
                        .get(idx)
                        .ok_or("wasm: 메모리 없음")?
                        .clone();
                    // 표준: 준 메모리가 모듈이 요구하는 최소 페이지보다 작으면 LinkError.
                    // 그냥 받으면 모듈이 없는 주소에 써서 조용히 죽는다.
                    let pages = m.borrow().borrow().len() / crate::wasm::PAGE;
                    if pages < min as usize {
                        return Err(format!(
                            "WebAssembly.LinkError: 임포트 {}.{} 메모리가 작다 ({} < {} 페이지)",
                            imp.module, imp.name, pages, min
                        ));
                    }
                    mem_obj = obj;
                    externs.push(Extern::Memory(m));
                }
                ImportKind::Global(t) => {
                    // 숫자로 오기도 하고 WebAssembly.Global 객체로 오기도 한다
                    let raw = match &v {
                        Value::Obj(_) => self.member_get(&v, "value")?,
                        other => other.clone(),
                    };
                    externs.push(Extern::Global(super::js_to_wasm_typed(&raw, t)));
                }
                ImportKind::Table => {
                    // 조용히 빈 테이블로 두면 call_indirect 가 엉뚱한 곳을 부른다.
                    return Err(
                        "WebAssembly.LinkError: 테이블 임포트는 아직 지원하지 않는다".to_string()
                    );
                }
            }
        }

        // 모듈 자신의 메모리 — JS 가 만든 버퍼를 그대로 쓴다 (살아있는 뷰)
        let own_mem = if mem_idx >= 0.0 {
            let (m, obj) = self
                .wasm_memories
                .get(mem_idx as usize)
                .ok_or("wasm: 메모리 없음")?
                .clone();
            mem_obj = obj;
            Some(m)
        } else {
            None
        };

        let inst = {
            let mut host = super::WasmHost {
                interp: self,
                imports: import_fns.clone(),
                module: module.clone(),
            };
            crate::wasm::instantiate(module.clone(), externs, own_mem, &mut host)?
        };
        let table_len = inst.table.borrow().len();
        self.sync_wasm_memories();
        self.wasm_instances
            .push(Rc::new(super::WasmInstance { inst, imports: import_fns }));
        let ii = (self.wasm_instances.len() - 1) as u32;

        // exports 객체
        let mut m = ObjMap::new();
        for (name, e) in &module.exports {
            let v = match e {
                WExport::Func(f) => Value::Native(Native::WasmCall(ii, *f)),
                WExport::Memory => mem_obj.clone(),
                // 내보내진 전역은 WebAssembly.Global 객체다 — .value 로 읽고 쓴다
                WExport::Global(g) => {
                    let mut go = ObjMap::new();
                    go.insert(
                        "value".to_string(),
                        Value::Accessor(Rc::new(AccessorPair {
                            get: Some(Value::Native(Native::WasmGlobalGet(ii, *g))),
                            set: Some(Value::Native(Native::WasmGlobalSet(ii, *g))),
                        })),
                    );
                    Value::Obj(Rc::new(RefCell::new(go)))
                }
                WExport::Table => {
                    let mut to = ObjMap::new();
                    to.insert("get".to_string(), Value::Native(Native::WasmTableGet(ii)));
                    to.insert("length".to_string(), Value::Num(table_len as f64));
                    Value::Obj(Rc::new(RefCell::new(to)))
                }
            };
            m.insert(name.clone(), v);
        }
        Ok(Value::Obj(Rc::new(RefCell::new(m))))
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
            // Object.getOwnPropertyDescriptor(o, k) — 접근자면 get/set, 값이면 value.
            // 예전엔 프렐류드 폴리필이 {value: o[k], enumerable: true} 를 만들었다:
            //   1) 게터 프로퍼티의 디스크립터에 get 이 없다 (게터를 **실행해** 값만 준다).
            //   2) enumerable 이 항상 true (비열거를 구분 못 한다).
            //   3) 배열 length / 함수 prototype 이 undefined.
            // 라이브러리가 d.get / d.enumerable 을 보고 분기하므로 조용히 틀린 길로 간다.
            Native::ObjectGetOwnPropertyDescriptor => {
                let target = args.first().cloned().unwrap_or(Value::Undefined);
                let key = args.get(1).map(to_display).unwrap_or_default();
                let mut d = ObjMap::new();
                let found = match &target {
                    Value::Obj(m) => {
                        let b = m.borrow();
                        match b.get(&key) {
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
                                d.insert("value".to_string(), v.clone());
                                d.insert("writable".to_string(), Value::Bool(true));
                                true
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
                        let v = if key == "prototype" {
                            Some(self.member_get(&target, "prototype")?)
                        } else {
                            f.props.borrow().get(&key).cloned()
                        };
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
                    Value::Instance(inst) => match inst.fields.borrow().get(&key) {
                        Some(v) => {
                            d.insert("value".to_string(), v.clone());
                            d.insert("writable".to_string(), Value::Bool(true));
                            true
                        }
                        None => false,
                    },
                    _ => false,
                };
                if !found {
                    return Ok(Value::Undefined);
                }
                let enumerable = match &target {
                    Value::Obj(m) => !m.borrow().contains_key(&nonenum_marker(&key)),
                    _ => true,
                };
                d.insert("enumerable".to_string(), Value::Bool(enumerable));
                d.insert("configurable".to_string(), Value::Bool(true));
                Ok(Value::Obj(Rc::new(RefCell::new(d))))
            }
            // Object.defineProperty(target, key, {get|value}) — 접근자/값 정의
            Native::ObjectDefineProperty => {
                let target = args.first().cloned().unwrap_or(Value::Undefined);
                let key = args.get(1).map(to_display).unwrap_or_default();
                let entry = if let Some(Value::Obj(d)) = args.get(2) {
                    let d = d.borrow();
                    let g = d.get("get").cloned().filter(is_callable);
                    let st = d.get("set").cloned().filter(is_callable);
                    if g.is_some() || st.is_some() {
                        // 접근자 프로퍼티 (get/set 둘 다, 또는 한쪽만)
                        Some(Value::Accessor(Rc::new(super::AccessorPair { get: g, set: st })))
                    } else {
                        d.get("value").cloned()
                    }
                } else {
                    None
                };
                // enumerable: false 면 표식을 남긴다 (기본값은 false — 표준).
                // 예전엔 이 플래그를 통째로 무시해서 숨겨야 할 프로퍼티가 Object.keys /
                // for-in / JSON 에 그대로 새어 나왔다.
                let enumerable = match args.get(2) {
                    Some(Value::Obj(dd)) => matches!(dd.borrow().get("enumerable"), Some(v) if to_bool(v)),
                    _ => false,
                };
                if let Some(val) = entry {
                    match &target {
                        Value::Obj(map) => {
                            let marker = nonenum_marker(&key);
                            map.borrow_mut().insert(key, val);
                            if enumerable {
                                map.borrow_mut().remove(&marker);
                            } else {
                                map.borrow_mut().insert(marker, Value::Bool(true));
                            }
                        }
                        // require.n 은 함수에 접근자를 정의 (getter.a = ...)
                        Value::Fn(func) => {
                            func.props.borrow_mut().insert(key, val);
                        }
                        _ => {}
                    }
                }
                Ok(target)
            }
            // Object.create(proto) — proto 의 얕은 복사 기반 새 객체 (관용)
            Native::ObjectCreate => {
                // proto 를 __proto__ 로 링크(스냅샷 복사 아님). Object.create(null) 은 링크 없음.
                let mut map = ObjMap::new();
                if let Some(p @ Value::Obj(_)) = args.first() {
                    map.insert("__proto__".to_string(), p.clone());
                }
                let obj = Value::Obj(Rc::new(RefCell::new(map)));
                // 2번째 인자(프로퍼티 서술자): {k: {value: v}} 를 얕게 반영(get/set 은 근사).
                if let Some(Value::Obj(descs)) = args.get(1) {
                    if let Value::Obj(m) = &obj {
                        for (k, d) in descs.borrow().iter() {
                            if let Value::Obj(dm) = d {
                                if let Some(v) = dm.borrow().get("value") {
                                    m.borrow_mut().insert(k.clone(), v.clone());
                                }
                            }
                        }
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
                let descs: Vec<(String, Value)> = match args.get(1) {
                    Some(Value::Obj(d)) => d
                        .borrow()
                        .iter()
                        .filter(|(k, _)| !is_internal_key(k))
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                    _ => Vec::new(),
                };
                for (k, desc) in descs {
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
                let key = args.first().map(to_display).unwrap_or_default();
                let has = match &recv {
                    // __proto__ 는 own 프로퍼티 아님(상속 accessor)
                    Some(Value::Obj(m)) => !is_internal_key(&key) && m.borrow().contains_key(&key),
                    // 인스턴스는 own 필드만 own 프로퍼티(메서드는 프로토타입 격)
                    Some(Value::Instance(i)) => i.fields.borrow().contains_key(&key),
                    Some(Value::Arr(a)) => {
                        key.parse::<usize>().map(|i| i < a.borrow().len()).unwrap_or(false)
                    }
                    _ => false,
                };
                Ok(Value::Bool(has))
            }
            // Object.prototype.toString.call(x) → "[object Array]" 등 (타입 판별 관용)
            Native::ObjToString => {
                let tag = match &recv {
                    Some(Value::Arr(_)) => "Array",
                    None | Some(Value::Undefined) => "Undefined",
                    Some(Value::Null) => "Null",
                    Some(Value::Str(_)) => "String",
                    Some(Value::Num(_)) => "Number",
                    Some(Value::Bool(_)) => "Boolean",
                    Some(Value::Fn(_))
                    | Some(Value::Native(_))
                    | Some(Value::Bound(_))
                    | Some(Value::Class(_)) => "Function",
                    Some(Value::MapVal(_)) => "Map",
                    Some(Value::SetVal(_)) => "Set",
                    _ => "Object",
                };
                Ok(Value::Str(format!("[object {}]", tag)))
            }
            Native::ReturnFalse => Ok(Value::Bool(false)),
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
                    return Err("queueMicrotask 인자는 함수여야 함".to_string());
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
                let name = args.first().map(to_display).unwrap_or_default();
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
                    m.insert("oldValue".to_string(), Value::Null);
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
                let mut map = ObjMap::new();
                map.insert("name".to_string(), Value::Str(name.to_string()));
                map.insert(
                    "message".to_string(),
                    Value::Str(args.first().map(to_display).unwrap_or_default()),
                );
                map.insert("stack".to_string(), Value::Str(String::new()));
                Ok(Value::Obj(Rc::new(RefCell::new(map))))
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
            Native::CreateElementNS => {
                let tag = args.get(1).map(to_display).unwrap_or_default();
                if tag.is_empty() {
                    return Err("createElementNS 에 태그 이름이 필요".to_string());
                }
                let dom = self.dom_arena()?;
                Ok(Value::Dom(dom.create_element(&tag)))
            }
            Native::CreateElement => {
                let tag = args.first().map(to_display).unwrap_or_default();
                if tag.is_empty() {
                    return Err("createElement 에 태그 이름이 필요".to_string());
                }
                let dom = self.dom_arena()?;
                Ok(Value::Dom(dom.create_element(&tag)))
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
            // classList.add(...names) / remove(...names) / toggle(name[,force]) / contains(name)
            Native::ClassAdd | Native::ClassRemove => {
                if let Some(Value::ClassList(id)) = recv {
                    let mut tokens = self.class_tokens(id);
                    for a in &args {
                        let name = to_display(a);
                        if name.is_empty() {
                            continue;
                        }
                        tokens.retain(|t| t != &name);
                        if matches!(n, Native::ClassAdd) {
                            tokens.push(name);
                        }
                    }
                    self.set_class_tokens(id, tokens);
                }
                Ok(Value::Undefined)
            }
            Native::ClassToggle => {
                if let Some(Value::ClassList(id)) = recv {
                    let name = args.first().map(to_display).unwrap_or_default();
                    let mut tokens = self.class_tokens(id);
                    let present = tokens.iter().any(|t| t == &name);
                    // 두 번째 인자(force)가 있으면 강제 설정
                    let want = match args.get(1) {
                        Some(v) => to_bool(v),
                        None => !present,
                    };
                    tokens.retain(|t| t != &name);
                    if want && !name.is_empty() {
                        tokens.push(name);
                    }
                    self.set_class_tokens(id, tokens);
                    return Ok(Value::Bool(want));
                }
                Ok(Value::Bool(false))
            }
            Native::ClassContains => {
                if let Some(Value::ClassList(id)) = recv {
                    let name = args.first().map(to_display).unwrap_or_default();
                    return Ok(Value::Bool(self.class_tokens(id).iter().any(|t| t == &name)));
                }
                Ok(Value::Bool(false))
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
                let v = recv.unwrap_or(Value::Undefined);
                // 숫자 + radix(2..36)면 진법 변환
                if let (Value::Num(n), Some(r)) = (&v, args.first().map(to_num)) {
                    let radix = r as u32;
                    if (2..=36).contains(&radix) && n.fract() == 0.0 && n.is_finite() {
                        let mut x = n.abs() as u64;
                        if x == 0 {
                            return Ok(Value::Str("0".to_string()));
                        }
                        let digits = b"0123456789abcdefghijklmnopqrstuvwxyz";
                        let mut buf = Vec::new();
                        while x > 0 {
                            buf.push(digits[(x % radix as u64) as usize]);
                            x /= radix as u64;
                        }
                        if *n < 0.0 {
                            buf.push(b'-');
                        }
                        buf.reverse();
                        return Ok(Value::Str(String::from_utf8_lossy(&buf).to_string()));
                    }
                }
                Ok(Value::Str(to_display(&v)))
            }
            Native::ValueOfSelf => Ok(recv.unwrap_or(Value::Undefined)),
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
                    Some(v) => (to_display(v), args.get(1).map(to_display).unwrap_or_default()),
                    None => (String::new(), String::new()),
                };
                Ok(make_regex_obj(&src, &flags))
            }
            // regex.test(str) → bool
            Native::RegexTest => {
                let (src, flags) = recv.as_ref().and_then(regex_src_flags).ok_or("test 대상이 정규식 아님")?;
                let text = args.first().map(to_display).unwrap_or_default();
                let re = crate::js::regex::Regex::compile_pattern(&src, &flags)
                    .map_err(|e| format!("정규식 컴파일 실패: {}", e))?;
                let chars: Vec<char> = text.chars().collect();
                Ok(Value::Bool(re.find(&chars, 0).is_some()))
            }
            // regex.exec(str) → [full, g1, ...] with .index, or null. global 이면 lastIndex 갱신.
            Native::RegexExec => {
                let recv_obj = recv.clone();
                let (src, flags) = recv.as_ref().and_then(regex_src_flags).ok_or("exec 대상이 정규식 아님")?;
                let text = args.first().map(to_display).unwrap_or_default();
                let re = crate::js::regex::Regex::compile_pattern(&src, &flags)
                    .map_err(|e| format!("정규식 컴파일 실패: {}", e))?;
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
            Native::EventCtor => {
                // new Event(type, opts) / new CustomEvent(type, {detail}) → 이벤트 객체
                let etype = args.first().map(to_display).unwrap_or_default();
                let mut m = ObjMap::new();
                m.insert("type".to_string(), Value::Str(etype));
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
                    let name = args.first().map(to_display).unwrap_or_default();
                    let dom = self.dom_arena()?;
                    dom.remove_attr(id, &name);
                }
                Ok(Value::Undefined)
            }
            Native::HasAttribute => {
                let has = if let Some(Value::Dom(id)) = recv {
                    let name = args.first().map(to_display).unwrap_or_default();
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
                    let name = args.first().map(to_display).unwrap_or_default();
                    let value = args.get(1).map(to_display).unwrap_or_default();
                    let dom = self.dom_arena()?;
                    dom.set_attr(id, &name, value);
                    Ok(Value::Undefined)
                }
                _ => Err("setAttribute 는 요소 메서드".to_string()),
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
                let Some(Value::Str(s)) = recv else {
                    return Err("문자열 메서드".to_string());
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
                    StrOp::Includes => Value::Bool(s.contains(&arg_str(0))),
                    StrOp::StartsWith => Value::Bool(s.starts_with(&arg_str(0))),
                    StrOp::EndsWith => Value::Bool(s.ends_with(&arg_str(0))),
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
                    _ => return Err("배열 메서드".to_string()),
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
                                None => return Err("빈 배열 reduce (초기값 없음)".to_string()),
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
                                    return Err("빈 배열 reduceRight (초기값 없음)".to_string());
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
                        // 기본 깊이 1: 한 단계 배열만 펼친다.
                        let mut out = Vec::new();
                        for v in a.borrow().iter() {
                            match v {
                                Value::Arr(inner) => out.extend(inner.borrow().iter().cloned()),
                                other => out.push(other.clone()),
                            }
                        }
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
                    Err(msg) => {
                        let mut e = ObjMap::new();
                        e.insert("name".to_string(), Value::Str("TypeError".to_string()));
                        e.insert("message".to_string(), Value::Str(msg.clone()));
                        self.thrown = Some(Value::Obj(Rc::new(RefCell::new(e))));
                        Err(msg)
                    }
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
                // Reflect.construct(fn, argsList)
                let f = args.first().cloned().unwrap_or(Value::Undefined);
                let arg_list = match args.get(1) {
                    Some(Value::Arr(a)) => a.borrow().clone(),
                    _ => Vec::new(),
                };
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
                Some(Value::Instance(i)) => {
                    let keys: Vec<Value> =
                        i.fields.borrow().keys().map(|k| Value::Str(k.clone())).collect();
                    Ok(Value::Arr(ArrayObj::new(keys)))
                }
                _ => Ok(Value::Arr(ArrayObj::new(Vec::new()))),
            },
            Native::ObjectValues => {
                let vals: Vec<Value> = match args.first() {
                    Some(Value::Obj(m)) => {
                        enumerable_entries(m).into_iter().map(|(_, v)| v).collect()
                    }
                    Some(Value::Arr(a)) => a.borrow().clone(),
                    Some(Value::Instance(i)) => i.fields.borrow().values().cloned().collect(),
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
                    Some(Value::Instance(inst)) => {
                        inst.fields.borrow().iter().map(|(k, v)| pair(k, v)).collect()
                    }
                    _ => Vec::new(),
                };
                Ok(Value::Arr(ArrayObj::new(entries)))
            }
            Native::ObjectFromEntries => {
                // [[k,v], ...] 또는 Map → 객체. 이터러블 순회.
                let mut map = ObjMap::new();
                if let Some(src) = args.first() {
                    for entry in self.iterate_to_vec(src) {
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
                    return Err("Object.assign 대상이 null/undefined".to_string());
                }
                for src in &args[1..] {
                    for (k, v) in own_enumerable_entries(src) {
                        self.set_own_property(&target, k, v);
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
                    | Value::Gen(_) => self.iterate_to_vec(&src),
                    Value::Obj(o)
                        if o.borrow().contains_key("\u{0}items") || o.borrow().contains_key("next") =>
                    {
                        self.iterate_to_vec(&src)
                    }
                    _ if self.try_get_iterator(&src)?.is_some() => self.iterate_to_vec(&src),
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
                    return Err("ArrayBuffer 가 너무 크다".to_string());
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
                    let name = args.first().map(to_display).unwrap_or_default();
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
