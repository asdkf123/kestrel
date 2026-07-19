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
pub(super) fn symbol_from_key(key: &str) -> Value {
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
            // 구멍 인덱스는 열거 대상이 아니다 (희소 배열). defineProperty 로
            // non-enumerable 이 된 인덱스도 제외한다 (§10.4.2, index_attrs 빈 배열은 무영향).
            let b = a.borrow();
            let mut out: Vec<(String, Value)> = a
                .present_indices()
                .into_iter()
                .filter(|&i| !matches!(a.index_attr(i), Some(at) if at & ATTR_ENUMERABLE == 0))
                .map(|i| (i.to_string(), b[i].clone()))
                .collect();
            drop(b);
            // 비인덱스 프로퍼티도 non-enumerable(prop_attrs) 은 열거에서 제외한다.
            out.extend(
                a.own_props()
                    .into_iter()
                    .filter(|(k, _)| {
                        !matches!(a.prop_attr(k), Some(at) if at & ATTR_ENUMERABLE == 0)
                    }),
            );
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
// §19.2.4 parseFloat: 문자열 앞쪽의 StrDecimalLiteral 최장 프리픽스를 f64 로 파싱한다.
// 부호, "Infinity", 정수/소수부(선행·후행 소수점 허용), 지수(e/E±digits) 지원. 유효한
// 숫자 프리픽스가 없으면 NaN. Rust f64 파서가 "11." 같은 후행점을 거부하므로 직접 스캔한다.
fn parse_float_prefix(t: &str) -> f64 {
    let b = t.as_bytes();
    let mut i = 0;
    let neg = match b.first() {
        Some(b'-') => {
            i = 1;
            true
        }
        Some(b'+') => {
            i = 1;
            false
        }
        _ => false,
    };
    // Infinity
    if t[i..].starts_with("Infinity") {
        return if neg { f64::NEG_INFINITY } else { f64::INFINITY };
    }
    let num_start = i;
    let mut has_digit = false;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
        has_digit = true;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
            has_digit = true;
        }
    }
    if !has_digit {
        return f64::NAN;
    }
    // 지수: e/E [±] digits — 지수 자릿수가 없으면 e 전까지만 숫자로 본다.
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        if j < b.len() && b[j].is_ascii_digit() {
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            i = j;
        }
    }
    // Rust 파서가 후행점("11.")을 거부하므로 제거 후 파싱. 부호는 마지막에 적용.
    let mut chunk = t[num_start..i].to_string();
    if let Some(dot) = chunk.find('.') {
        // 소수점 뒤가 비었거나 바로 e 면 소수점을 지운다("11.e-1"→"11e-1", "11."→"11").
        let after = &chunk[dot + 1..];
        if after.is_empty() || after.starts_with(['e', 'E']) {
            chunk.remove(dot);
        }
    }
    if chunk.starts_with('.') {
        chunk.insert(0, '0'); // ".22e-1" → "0.22e-1"
    }
    match chunk.parse::<f64>() {
        Ok(n) => {
            if neg {
                -n
            } else {
                n
            }
        }
        Err(_) => f64::NAN,
    }
}

fn recv_prop_str(recv: &Option<Value>, key: &str) -> String {
    if let Some(Value::Obj(o)) = recv {
        if let Some(v) = o.borrow().get(key) {
            return to_display(v);
        }
    }
    String::new()
}

// 수신자에서 Set 의 내부 데이터([[SetData]])를 꺼낸다 — SetVal 이거나 extends Set
// 파생 인스턴스(내부 슬롯 COLLECTION_SLOT)면 Some, 아니면 None(brand check 실패).
fn recv_set_data(recv: &Option<Value>) -> Option<Rc<RefCell<Vec<Value>>>> {
    let from_slot = |m: &ObjMap| match m.get(COLLECTION_SLOT) {
        Some(Value::SetVal(s)) => Some(s.clone()),
        _ => None,
    };
    match recv {
        Some(Value::SetVal(s)) => Some(s.clone()),
        Some(Value::Instance(i)) => from_slot(&i.fields.borrow()),
        Some(Value::Obj(m)) => from_slot(&m.borrow()),
        _ => None,
    }
}

// 수신자에서 Map 의 내부 데이터([[MapData]])를 꺼낸다 (Set 과 동형).
fn recv_map_data(recv: &Option<Value>) -> Option<Rc<RefCell<Vec<(Value, Value)>>>> {
    let from_slot = |m: &ObjMap| match m.get(COLLECTION_SLOT) {
        Some(Value::MapVal(mm)) => Some(mm.clone()),
        _ => None,
    };
    match recv {
        Some(Value::MapVal(m)) => Some(m.clone()),
        Some(Value::Instance(i)) => from_slot(&i.fields.borrow()),
        Some(Value::Obj(m)) => from_slot(&m.borrow()),
        _ => None,
    }
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
        // §25.5.1.1: Type(val) 이 Object 면 IsArray 로 분기해 자식을 되살린다. 설정은
        // 추상연산(Get/CreateDataProperty/[[Delete]])으로 — Proxy 트랩·non-configurable 을
        // 존중한다. 예전엔 Value::Arr/Obj 만 매칭해 Proxy 를 놓치고 raw 슬롯을 썼다.
        if is_object(&val) {
            if self.is_array(&val)? {
                let len_v = self.member_get(&val, "length")?;
                let len = to_length(self.to_number_value(&len_v)?) as usize;
                for i in 0..len {
                    let child = match snap {
                        Some(JsonSrc::Arr(v)) => v.get(i),
                        _ => None,
                    };
                    let nv = self.json_revive(&val, &i.to_string(), reviver, child)?;
                    if matches!(nv, Value::Undefined) {
                        self.delete_own(&val, &i.to_string())?;
                    } else {
                        self.json_define_revived(&val, &i.to_string(), nv)?;
                    }
                }
            } else {
                // EnumerableOwnPropertyNames(val, key) — 평범 객체는 enumerable_keys,
                // 인스턴스는 own 필드, 그 외(Proxy 등)는 Object.keys 로(ownKeys/gOPD 트랩 경유).
                let keys: Vec<String> = match &val {
                    Value::Obj(m) => enumerable_keys(m),
                    Value::Instance(i) => i
                        .fields
                        .borrow()
                        .keys()
                        .filter(|k| !is_internal_key(k))
                        .cloned()
                        .collect(),
                    _ => {
                        let ks = self.call_native(Native::ObjectKeys, None, vec![val.clone()])?;
                        match &ks {
                            Value::Arr(a) => a
                                .borrow()
                                .iter()
                                .filter_map(|v| match v {
                                    Value::Str(s) => Some(s.clone()),
                                    _ => None,
                                })
                                .collect(),
                            _ => Vec::new(),
                        }
                    }
                };
                for k in keys {
                    let child = match snap {
                        Some(JsonSrc::Obj(v)) => {
                            v.iter().rev().find(|(kk, _)| *kk == k).map(|(_, s)| s)
                        }
                        _ => None,
                    };
                    let nv = self.json_revive(&val, &k, reviver, child)?;
                    if matches!(nv, Value::Undefined) {
                        self.delete_own(&val, &k)?;
                    } else {
                        self.json_define_revived(&val, &k, nv)?;
                    }
                }
            }
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

    // §25.5.1.1 의 CreateDataProperty(val, key, newElement): [[DefineOwnProperty]] 로
    // {value, w/e/c:true} 정의. non-configurable 등으로 실패하면 false(무시), Proxy
    // defineProperty 트랩이 던지면 전파한다 — Reflect.defineProperty 의미(OrThrow 아님).
    fn json_define_revived(&mut self, target: &Value, key: &str, v: Value) -> Result<(), String> {
        let mut desc = ObjMap::new();
        desc.insert("value".to_string(), v);
        desc.insert("writable".to_string(), Value::Bool(true));
        desc.insert("enumerable".to_string(), Value::Bool(true));
        desc.insert("configurable".to_string(), Value::Bool(true));
        let desc = Value::Obj(Rc::new(RefCell::new(desc)));
        self.call_native(
            Native::ReflectDefineProperty,
            None,
            vec![target.clone(), Value::Str(key.to_string()), desc],
        )?;
        Ok(())
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
        // 1) toJSON(key) 가 있으면 그 반환값을 직렬화한다 (Date 등). §25.5.2.2 step 2:
        // value 가 Object 또는 BigInt 면 Get(value,"toJSON") — BigInt.prototype.toJSON 도 본다.
        let mut v = v.clone();
        if matches!(
            v,
            Value::Obj(_) | Value::Instance(_) | Value::Arr(_) | Value::BigInt(_)
        ) {
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
        // 원시 래퍼(new Number/String/Boolean/BigInt)는 원시값으로 직렬화 (§25.5.2.2 step 4):
        // Number 는 ToNumber(오버라이드된 valueOf 관측), String 은 ToString(toString 관측),
        // Boolean/BigInt 는 내부 슬롯 그대로. 예전엔 전부 슬롯을 읽어 valueOf/toString
        // 오버라이드를 무시했다(new Number(42) 를 valueOf 로 2 로 바꿔도 42 로 직렬화).
        let unwrapped;
        let v = match v {
            Value::Obj(m) if m.borrow().contains_key(WRAPPER_SLOT) => {
                let prim = wrapper_primitive(v).unwrap_or(Value::Null);
                unwrapped = match prim {
                    Value::Num(_) => Value::Num(self.to_number_value(v)?),
                    Value::Str(_) => Value::Str(self.to_string_value(v)?),
                    // §25.5.2.2 step 4.d: [[BigIntData]] 슬롯을 가진 래퍼도 TypeError.
                    Value::BigInt(_) => {
                        return Err(self.throw_error(
                            "TypeError",
                            "Do not know how to serialize a BigInt",
                        ))
                    }
                    other => other, // Boolean: 슬롯 그대로(강제변환 없음)
                };
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
            | Value::Gen(_)
            | Value::Symbol(_)
            | Value::ComputedStyle(_) => None,
            // §25.5.2.2: Proxy 는 IsArray(타깃)에 따라 배열/객체로 직렬화하며, 길이·키·값을
            // 모두 트랩(get/ownKeys/gOPD)으로 읽는다. revoked proxy 는 그 트랩이 TypeError.
            Value::Proxy(_) => {
                if self.is_array(v)? {
                    let len_v = self.member_get(v, "length")?;
                    let len = to_length(self.to_number_value(&len_v)?);
                    let mut parts = Vec::with_capacity((len as usize).min(4096));
                    let mut i: u64 = 0;
                    while (i as f64) < len {
                        let item = self.member_get(v, &i.to_string())?;
                        let s = self
                            .json_ser(&item, &i.to_string(), v, fnrep, keys, indent, depth + 1, path)?
                            .unwrap_or_else(|| "null".to_string());
                        parts.push(s);
                        i += 1;
                    }
                    Some(wrap(parts, '[', ']'))
                } else {
                    let key_list: Vec<String> = match keys {
                        Some(ks) => ks.clone(),
                        None => self
                            .enumerable_own_live(v, true, false)?
                            .into_iter()
                            .filter_map(|k| match k {
                                Value::Str(s) => Some(s),
                                _ => None,
                            })
                            .collect(),
                    };
                    let mut parts = Vec::new();
                    for k in &key_list {
                        let val = self.member_get(v, k)?;
                        if let Some(s) =
                            self.json_ser(&val, k, v, fnrep, keys, indent, depth + 1, path)?
                        {
                            parts.push(format!("{}{}{}", json_quote_pub(k), colon, s));
                        }
                    }
                    Some(wrap(parts, '{', '}'))
                }
            }
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
        if regex_src_flags(pat).is_some() {
            // §22.1.3.17/.18: 정규식 검색값은 그 @@replace 로 위임한다 — custom exec/named
            // groups(functional replacer 의 마지막 groups 인자)/GetSubstitution/전역 모두
            // 표준 프로토콜로. replaceAll 은 비전역 정규식이면 TypeError.
            if all {
                let g = to_bool(&self.member_get(pat, "global")?);
                if !g {
                    return Err(self.throw_error(
                        "TypeError",
                        "replaceAll must be called with a global RegExp",
                    ));
                }
            }
            let r = self.call_native(
                Native::RegexSym(natives::StrOp::Replace),
                Some(pat.clone()),
                vec![Value::Str(s.to_string()), repl.clone()],
            )?;
            Ok(to_display(&r))
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
        } else if is_data_desc {
            // 데이터 서술자(value 또는 writable 포함). value 가 있으면 그 값을,
            // writable 만 있으면 기존 데이터 값을 보존한다. 기존이 접근자였다면
            // 접근자→데이터 전환이므로 value 는 undefined 로 기본값(§10.1.6.3).
            if has_value {
                new_value
            } else {
                match &existing {
                    Some(v) if !matches!(v, Value::Accessor(_)) => v.clone(),
                    _ => Value::Undefined,
                }
            }
        } else {
            // generic 서술자(value/get/set 없음): 기존 값을 그대로 보존한다 —
            // 접근자면 접근자, 데이터면 데이터. 예전엔 이 분기가 접근자를 undefined
            // 로 날려(가드 실패) enumerable 만 바꾼 재정의가 getter/setter 를 잃었다.
            match &existing {
                Some(v) => v.clone(),
                None => Value::Undefined,
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
    // §22.1.3.11/.12 String.prototype.{match,search}: regexp[sym] 로 위임한다.
    // regexp 가 null/undefined 아니면 GetMethod(regexp, sym) 를 호출(override 존중),
    // 없으면 RegExpCreate(regexp) 후 그 sym 으로 Invoke. sym 은 "\0@@match"/"\0@@search".
    fn str_regex_delegate(&mut self, s: &str, arg: Value, sym: &str) -> Result<Value, String> {
        // 최신 스펙(§22.1.3.11/.12): regexp 가 **Object** 일 때만 @@match/@@search 를
        // 접근한다 — 원시값(숫자/불리언 등)은 그 프로토타입의 심볼 메서드를 건드리지
        // 않고 곧장 RegExpCreate 로 간다.
        if is_object(&arg) {
            let matcher = self.member_get(&arg, sym)?;
            if !matches!(matcher, Value::Undefined | Value::Null) {
                if !is_callable(&matcher) {
                    return Err(self.throw_error("TypeError", "regexp method is not callable"));
                }
                return self.call_value(matcher, Some(arg), vec![Value::Str(s.to_string())]);
            }
        }
        // RegExpCreate(regexp): 정규식이면 source+flags 보존, undefined 는 빈 패턴,
        // 그 외(null 포함)는 ToString(arg)+무플래그 (null → "null").
        let (pat, flags) = match &arg {
            Value::Undefined => (String::new(), String::new()),
            v => match regex_src_flags(v) {
                Some((src, f)) => (src, f),
                None => (self.to_string_value(v)?, String::new()),
            },
        };
        let rx = make_regex_obj(&pat, &flags);
        let matcher = self.member_get(&rx, sym)?;
        self.call_value(matcher, Some(rx), vec![Value::Str(s.to_string())])
    }

    pub(super) fn to_object_value(&self, v: Value) -> Value {
        let (proto, tag) = match &v {
            Value::Str(_) => (self.string_proto.clone(), "String"),
            Value::Num(_) => (self.number_proto.clone(), "Number"),
            Value::Bool(_) => (self.boolean_proto.clone(), "Boolean"),
            Value::Symbol(_) => (self.symbol_proto.clone(), "Symbol"),
            Value::BigInt(_) => (self.bigint_proto.clone(), "BigInt"),
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

    // RegExpExec (§22.2.7.1): exec = Get(R,"exec"); IsCallable 이면 Call(exec, R, [S]) 후
    // 결과가 Object/null 아니면 TypeError. 아니면 내장 exec(정규식 필요).
    pub(super) fn regexp_exec(&mut self, r: &Value, s: &str) -> Result<Value, String> {
        let exec = self.member_get(r, "exec")?;
        if is_callable(&exec) {
            let result =
                self.call_value(exec, Some(r.clone()), vec![Value::Str(s.to_string())])?;
            if !is_object(&result) && !matches!(result, Value::Null) {
                return Err(self.throw_error(
                    "TypeError",
                    "RegExp exec method returned something other than an Object or null",
                ));
            }
            return Ok(result);
        }
        if regex_src_flags(r).is_none() {
            return Err(self.throw_error("TypeError", "RegExpExec called on non-RegExp"));
        }
        self.call_native(Native::RegexExec, Some(r.clone()), vec![Value::Str(s.to_string())])
    }

    // GetSubstitution (§22.1.3.19): $$ / $& / $` / $' / $n(1~2자리) / $<name> 치환.
    // captures 는 이미 ToString 된 그룹(없으면 None), named 는 groups 객체(없으면 Undefined).
    fn get_substitution(
        &mut self,
        matched: &str,
        str_chars: &[char],
        position: usize,
        captures: &[Option<String>],
        named: &Value,
        template: &str,
    ) -> Result<String, String> {
        let t: Vec<char> = template.chars().collect();
        let matched_len = matched.chars().count();
        let tail_start = (position + matched_len).min(str_chars.len());
        let mut out = String::new();
        let mut i = 0;
        while i < t.len() {
            if t[i] == '$' && i + 1 < t.len() {
                match t[i + 1] {
                    '$' => {
                        out.push('$');
                        i += 2;
                    }
                    '&' => {
                        out.push_str(matched);
                        i += 2;
                    }
                    '`' => {
                        out.extend(&str_chars[..position]);
                        i += 2;
                    }
                    '\'' => {
                        out.extend(&str_chars[tail_start..]);
                        i += 2;
                    }
                    '<' if !matches!(named, Value::Undefined) => {
                        if let Some(close) = t[i + 2..].iter().position(|&c| c == '>') {
                            let name: String = t[i + 2..i + 2 + close].iter().collect();
                            let v = self.member_get(named, &name)?;
                            if !matches!(v, Value::Undefined) {
                                out.push_str(&self.to_string_value(&v)?);
                            }
                            i = i + 2 + close + 1;
                        } else {
                            out.push('$');
                            i += 1;
                        }
                    }
                    d if d.is_ascii_digit() => {
                        // 2자리 그룹 우선, 유효하지 않으면 1자리 폴백.
                        let two = if i + 2 < t.len() && t[i + 2].is_ascii_digit() {
                            let n = (d as usize - '0' as usize) * 10
                                + (t[i + 2] as usize - '0' as usize);
                            if n >= 1 && n <= captures.len() { Some((n, 3usize)) } else { None }
                        } else {
                            None
                        };
                        let pick = two.or_else(|| {
                            let n = d as usize - '0' as usize;
                            if n >= 1 && n <= captures.len() { Some((n, 2usize)) } else { None }
                        });
                        match pick {
                            Some((n, adv)) => {
                                if let Some(Some(c)) = captures.get(n - 1) {
                                    out.push_str(c);
                                }
                                i += adv;
                            }
                            None => {
                                out.push('$');
                                i += 1;
                            }
                        }
                    }
                    _ => {
                        out.push('$');
                        i += 1;
                    }
                }
            } else {
                out.push(t[i]);
                i += 1;
            }
        }
        Ok(out)
    }

    // ArraySpeciesCreate 용 생성자 결정(§23.1.3.5). 반환 Some(C) 면 그 생성자로 결과를
    // 만들어야 하고, None 이면 기본 Array(빠른 경로). original 이 배열이 아니거나 species 가
    // 기본 Array/undefined 면 None. species 가 non-constructor 면 TypeError.
    pub(super) fn array_species_ctor(&mut self, original: &Value) -> Result<Option<Value>, String> {
        if !matches!(original, Value::Arr(_)) {
            return Ok(None);
        }
        let c = self.member_get(original, "constructor")?;
        let c = if is_object(&c) {
            let s = self.member_get(&c, "\u{0}@@species")?;
            if matches!(s, Value::Null) {
                Value::Undefined
            } else {
                s
            }
        } else {
            c
        };
        // 기본 Array 또는 undefined → 빠른 경로(일반 배열).
        if matches!(c, Value::Undefined | Value::Native(Native::ArrayCtor)) {
            return Ok(None);
        }
        if !self.is_constructor(&c) {
            return Err(self.throw_error("TypeError", "Array species is not a constructor"));
        }
        Ok(Some(c))
    }

    // CreateDataPropertyOrThrow(O, P, V) (§7.3.5): [[DefineOwnProperty]] 로 {value, writable:t,
    // enumerable:t, configurable:t} 데이터를 정의한다([[Set]]이 아님 — 기존 non-writable 이라도
    // configurable 이면 redefine 성공). 정의 실패(non-configurable/non-extensible)면 TypeError.
    fn create_data_property_or_throw(&mut self, target: &Value, key: usize, v: Value) -> Result<(), String> {
        self.create_data_property_str(target, &key.to_string(), v)
    }

    // CreateDataPropertyOrThrow(target, key, v): {value, w/e/c:true} 로 정의(실패 시 TypeError).
    // Proxy 의 defineProperty 트랩·non-configurable 불변식을 존중한다.
    fn create_data_property_str(&mut self, target: &Value, key: &str, v: Value) -> Result<(), String> {
        let mut desc = ObjMap::new();
        desc.insert("value".to_string(), v);
        desc.insert("writable".to_string(), Value::Bool(true));
        desc.insert("enumerable".to_string(), Value::Bool(true));
        desc.insert("configurable".to_string(), Value::Bool(true));
        let desc = Value::Obj(Rc::new(RefCell::new(desc)));
        self.call_native(
            Native::ObjectDefineProperty,
            None,
            vec![target.clone(), Value::Str(key.to_string()), desc],
        )?;
        Ok(())
    }

    // SpeciesConstructor(O, defaultConstructor) (§7.3.22): C=Get(O,"constructor"); undefined 면
    // default. 객체 아니면 TypeError. S=Get(C,@@species); undefined/null 이면 default.
    // IsConstructor(S) 면 S, 아니면 TypeError. 예외는 그대로 전파.
    pub(super) fn species_constructor(
        &mut self,
        o: &Value,
        default: Value,
    ) -> Result<Value, String> {
        let c = self.member_get(o, "constructor")?;
        if matches!(c, Value::Undefined) {
            return Ok(default);
        }
        if !is_object(&c) {
            return Err(self.throw_error("TypeError", "constructor property is not an object"));
        }
        let s = self.member_get(&c, "\u{0}@@species")?;
        if matches!(s, Value::Undefined | Value::Null) {
            return Ok(default);
        }
        if self.is_constructor(&s) {
            return Ok(s);
        }
        Err(self.throw_error("TypeError", "@@species is not a constructor"))
    }

    // Set(O, P, V, Throw=true) (§7.3.4): 실패하면 TypeError. 접근자 setter 예외는 전파.
    pub(super) fn set_throw(&mut self, o: &Value, key: &str, v: Value) -> Result<(), String> {
        if !self.ordinary_set(o, key, v, o)? {
            return Err(self.throw_error("TypeError", format!("Cannot assign to property '{}'", key)));
        }
        Ok(())
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
            // 서브클래스(extends Array) 인스턴스는 커스텀 __proto__(X.prototype)를 갖는다 —
            // 그게 [[Prototype]] 이다. 없으면 %Array.prototype%.
            Value::Arr(a) => match a.get_prop("__proto__") {
                Some(p) if is_object(&p) => p,
                _ => self.member_get(&self.array_ns.clone(), "prototype")?,
            },
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
            Value::Symbol(_) => self.symbol_proto.clone(),
            Value::BigInt(_) => self.bigint_proto.clone(),
            Value::MapVal(_) => self.map_proto.clone(),
            Value::SetVal(_) => self.set_proto.clone(),
            // 제너레이터의 [[Prototype]] 은 %IteratorPrototype%(프렐류드 __kIterProto)로
            // 이어져 `gen instanceof Iterator` 및 헬퍼 결과의 Iterator 판정이 성립한다
            // (§27.5 GeneratorPrototype → §27.1.2 %IteratorPrototype%). 근사이지만
            // Object.prototype 로 두는 것보다 정확하다.
            Value::Gen(_) => env_get(&self.global, "__kIterProto").unwrap_or(obj_proto),
            // 클래스도 함수 — [[Prototype]] 은 부모 생성자(파생 클래스)거나
            // %Function.prototype%(기저 클래스·extends null 은 constructorParent=FnProto).
            Value::Class(c) => {
                if let Some(p) = &c.parent {
                    Value::Class(p.clone())
                } else if let Some(pc) = &c.parent_ctor {
                    if matches!(pc, Value::Null) {
                        self.fn_proto.clone()
                    } else {
                        pc.clone()
                    }
                } else {
                    self.fn_proto.clone()
                }
            }
            // §10.5.1 [[GetPrototypeOf]]: getPrototypeOf 트랩(없으면 타깃에 위임).
            // 예전엔 Proxy 를 무조건 Object.prototype 으로 답해 트랩도, 타깃의
            // 실제 프로토타입도 무시했다(Object.getPrototypeOf/isPrototypeOf 가
            // 프록시에서 전부 틀린 답).
            Value::Proxy(p) => {
                self.proxy_revoked_guard(p)?;
                let (t, h) = (p.0.clone(), p.1.clone());
                let trap = self.member_get(&h, "getPrototypeOf")?;
                // GetMethod(§7.3.10): undefined/null 이면 트랩 없음(타깃 위임),
                // 존재하나 호출 불가면 TypeError.
                if matches!(trap, Value::Undefined | Value::Null) {
                    return self.proto_of(&t);
                }
                if !is_callable(&trap) {
                    return Err(self.throw_error(
                        "TypeError",
                        "'getPrototypeOf' trap is not callable",
                    ));
                }
                let handler_proto = self.call_value(trap, Some(h), vec![t.clone()])?;
                // 트랩 결과는 Object 또는 Null 이어야 한다.
                if !matches!(handler_proto, Value::Null) && !is_object(&handler_proto) {
                    return Err(self.throw_error(
                        "TypeError",
                        "'getPrototypeOf' on proxy must return an object or null",
                    ));
                }
                // target 이 non-extensible 이면 트랩 결과가 실제 프로토타입과
                // SameValue 여야 한다(트랩이 거짓 프로토타입을 보고 못 함).
                if self.is_nonextensible_val(&t) {
                    let target_proto = self.proto_of(&t)?;
                    if !same_value(&handler_proto, &target_proto) {
                        return Err(self.throw_error(
                            "TypeError",
                            "'getPrototypeOf' on proxy: inconsistent result for non-extensible target",
                        ));
                    }
                }
                handler_proto
            }
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

    // reduce/reduceRight 의 live 순회용: 인덱스 k 가 (own 이든 상속이든) present 면 [[Get]]
    // 값을 Some 으로, 진짜 없으면 None 을 준다. Arr 은 is_hole+접근자+Array.prototype 상속,
    // 그 외(generic array-like)는 HasProperty 로 판정한다(§7.3.12 HasProperty + §7.3.3 Get).
    // 스냅샷 대신 매 인덱스 live 로 읽어 콜백/게터 중 변형을 관측한다.
    pub(super) fn array_like_live_get(
        &mut self,
        o: &Value,
        k: usize,
    ) -> Result<Option<Value>, String> {
        match o {
            Value::Arr(a) => {
                let own_present = k < a.borrow().len() && !a.is_hole(k);
                if own_present {
                    return Ok(Some(self.member_get(o, &k.to_string())?));
                }
                // 구멍이거나 잘려나간 인덱스: own defineProperty 나 Array.prototype 상속 확인.
                let key = k.to_string();
                let inherited = {
                    let proto = self.member_get(&self.array_ns.clone(), "prototype")?;
                    self.has_property(&proto, &key)
                };
                if a.get_prop(&key).is_some() || inherited {
                    Ok(Some(self.member_get(o, &key)?))
                } else {
                    Ok(None)
                }
            }
            _ => {
                let key = k.to_string();
                if self.has_property(o, &key) {
                    Ok(Some(self.member_get(o, &key)?))
                } else {
                    Ok(None)
                }
            }
        }
    }

    // §23.1.3 반복 메서드(reduce/reduceRight/some/every/find/findIndex/forEach/map/
    // filter/flatMap)를 배열/generic array-like 위에서 매 인덱스 live 로 실행한다.
    // 스냅샷·재료화를 하지 않으므로 (1) 콜백·게터 중 배열 변형을 관측하고 (2) generic
    // array-like 재료화의 getter 이중호출을 없앤다. length 는 순회 시작에 한 번 읽는다.
    // some/every/forEach/map/filter/flatMap 는 HasProperty 로 구멍을 건너뛰고(map 은
    // 출력에 구멍 보존), find/findIndex 는 Get 으로 구멍도 방문한다(§23.1.3).
    pub(super) fn array_iter_live(
        &mut self,
        op: ArrOp,
        o: &Value,
        args: &[Value],
    ) -> Result<Value, String> {
        let len_f = match o {
            Value::Arr(a) => a.borrow().len() as f64,
            _ => {
                let lv = self.member_get(o, "length")?;
                to_length(self.to_number_value(&lv)?)
            }
        };
        let f = args.first().cloned().unwrap_or(Value::Undefined);
        if !is_callable(&f) {
            return Err(self.throw_error("TypeError", "callback is not a function"));
        }
        // findLast/findLastIndex(§23.1.3.11/.12): len-1 부터 뒤로 매 인덱스 [[Get]](구멍도
        // undefined 방문)+predicate, 첫 truthy 에서 값/인덱스 반환. 초거대 length 도 최고
        // 인덱스부터라 predicate 가 일찍 true 면 즉시 끝난다(maximum-index). live 순회라
        // 콜백 중 배열 변형을 관측한다.
        if matches!(op, ArrOp::FindLast | ArrOp::FindLastIndex) {
            let this_arg = args.get(1).cloned();
            let mut i: i64 = len_f as i64 - 1;
            while i >= 0 {
                let item = self.member_get(o, &(i as usize).to_string())?;
                let r = self.call_value(
                    f.clone(),
                    this_arg.clone(),
                    vec![item.clone(), Value::Num(i as f64), o.clone()],
                )?;
                if to_bool(&r) {
                    return Ok(if matches!(op, ArrOp::FindLastIndex) {
                        Value::Num(i as f64)
                    } else {
                        item
                    });
                }
                i -= 1;
            }
            return Ok(if matches!(op, ArrOp::FindLastIndex) {
                Value::Num(-1.0)
            } else {
                Value::Undefined
            });
        }
        // 초거대 length(> 2^32-1): 0..len 순회는 불가능하므로 존재하는 own 정수 인덱스만
        // 오름차순으로 방문한다(HasProperty 로 걸러지는 부재 인덱스는 어차피 콜백 미호출이라
        // 관측 동치). 결과 배열을 재료화해야 하는 map/filter/flatMap 과, 구멍도 Get 으로
        // 방문해야 하는 find/findIndex 는 초거대에서 실현 불가 → RangeError.
        if len_f > MAX_ARRAY_LEN {
            if matches!(
                op,
                ArrOp::Map | ArrOp::Filter | ArrOp::FlatMap | ArrOp::Find | ArrOp::FindIndex
            ) {
                return Err(self.throw_error("RangeError", "Invalid array length"));
            }
            let mut keys: Vec<usize> = match o {
                Value::Obj(m) => m
                    .borrow()
                    .keys()
                    .filter_map(|k| k.parse::<usize>().ok())
                    .filter(|&k| (k as f64) < len_f)
                    .collect(),
                _ => Vec::new(),
            };
            keys.sort_unstable();
            keys.dedup();
            if matches!(op, ArrOp::Reduce | ArrOp::ReduceRight) {
                let reverse = matches!(op, ArrOp::ReduceRight);
                if reverse {
                    keys.reverse();
                }
                let has_init = args.len() >= 2;
                let mut acc = if has_init { args[1].clone() } else { Value::Undefined };
                let mut have_acc = has_init;
                for &k in &keys {
                    if !self.has_property(o, &k.to_string()) {
                        continue; // 콜백이 지웠을 수 있음
                    }
                    let v = self.member_get(o, &k.to_string())?;
                    if have_acc {
                        acc = self.call_value(
                            f.clone(),
                            None,
                            vec![acc, v, Value::Num(k as f64), o.clone()],
                        )?;
                    } else {
                        acc = v;
                        have_acc = true;
                    }
                }
                if !have_acc {
                    return Err(self.throw_error(
                        "TypeError",
                        "Reduce of empty array with no initial value",
                    ));
                }
                return Ok(acc);
            }
            // some/every/forEach
            let this_arg = args.get(1).cloned();
            for &k in &keys {
                if !self.has_property(o, &k.to_string()) {
                    continue;
                }
                let v = self.member_get(o, &k.to_string())?;
                let r = self.call_value(
                    f.clone(),
                    this_arg.clone(),
                    vec![v, Value::Num(k as f64), o.clone()],
                )?;
                match op {
                    ArrOp::Some if to_bool(&r) => return Ok(Value::Bool(true)),
                    ArrOp::Every if !to_bool(&r) => return Ok(Value::Bool(false)),
                    _ => {}
                }
            }
            return Ok(match op {
                ArrOp::Some => Value::Bool(false),
                ArrOp::Every => Value::Bool(true),
                _ => Value::Undefined,
            });
        }
        let n = len_f as i64;

        // reduce/reduceRight: thisArg 없이 누산, args[1] 은 초기값.
        if matches!(op, ArrOp::Reduce | ArrOp::ReduceRight) {
            let reverse = matches!(op, ArrOp::ReduceRight);
            let has_init = args.len() >= 2;
            let mut acc = if has_init { args[1].clone() } else { Value::Undefined };
            let mut have_acc = has_init;
            let mut i: i64 = if reverse { n - 1 } else { 0 };
            while i >= 0 && i < n {
                if let Some(v) = self.array_like_live_get(o, i as usize)? {
                    if have_acc {
                        acc = self.call_value(
                            f.clone(),
                            None,
                            vec![acc, v, Value::Num(i as f64), o.clone()],
                        )?;
                    } else {
                        acc = v;
                        have_acc = true;
                    }
                }
                i += if reverse { -1 } else { 1 };
            }
            if !have_acc {
                return Err(self.throw_error(
                    "TypeError",
                    "Reduce of empty array with no initial value",
                ));
            }
            return Ok(acc);
        }

        let this_arg = args.get(1).cloned();
        let mut out: Vec<Value> = Vec::new();
        let mut out_holes: std::collections::HashSet<usize> = std::collections::HashSet::new();
        let mut i: i64 = 0;
        while i < n {
            let k = i as usize;
            // find/findIndex 는 Get(구멍도 undefined 로 방문), 그 외는 HasProperty 로 present.
            let (present, item) = if matches!(op, ArrOp::Find | ArrOp::FindIndex) {
                (true, self.member_get(o, &k.to_string())?)
            } else {
                match self.array_like_live_get(o, k)? {
                    Some(v) => (true, v),
                    None => (false, Value::Undefined),
                }
            };
            if !present {
                if matches!(op, ArrOp::Map) {
                    out_holes.insert(out.len());
                    out.push(Value::Undefined);
                }
                i += 1;
                continue;
            }
            let r = self.call_value(
                f.clone(),
                this_arg.clone(),
                vec![item.clone(), Value::Num(k as f64), o.clone()],
            )?;
            match op {
                ArrOp::Some if to_bool(&r) => return Ok(Value::Bool(true)),
                ArrOp::Every if !to_bool(&r) => return Ok(Value::Bool(false)),
                ArrOp::Find if to_bool(&r) => return Ok(item),
                ArrOp::FindIndex if to_bool(&r) => return Ok(Value::Num(k as f64)),
                ArrOp::Map => out.push(r),
                ArrOp::Filter => {
                    if to_bool(&r) {
                        out.push(item);
                    }
                }
                ArrOp::FlatMap => match r {
                    Value::Arr(inner) => out.extend(inner.borrow().iter().cloned()),
                    other => out.push(other),
                },
                _ => {}
            }
            i += 1;
        }

        match op {
            ArrOp::Some => Ok(Value::Bool(false)),
            ArrOp::Every => Ok(Value::Bool(true)),
            ArrOp::Find => Ok(Value::Undefined),
            ArrOp::FindIndex => Ok(Value::Num(-1.0)),
            ArrOp::ForEach => Ok(Value::Undefined),
            // map/filter/flatMap 결과는 ArraySpeciesCreate (§23.1.3).
            _ => match self.array_species_ctor(o)? {
                Some(ctor) => {
                    let a = self.construct(ctor, vec![Value::Num(out.len() as f64)])?;
                    for (k, v) in out.into_iter().enumerate() {
                        self.create_data_property_or_throw(&a, k, v)?;
                    }
                    Ok(a)
                }
                None => Ok(Value::Arr(if out_holes.is_empty() {
                    ArrayObj::new(out)
                } else {
                    ArrayObj::with_holes(out, out_holes)
                })),
            },
        }
    }

    // §22.1.3.14/.17 indexOf/lastIndexOf 를 배열/generic array-like 위에서 매 인덱스 live
    // 로 검색한다. len 은 시작에 한 번 읽고(len==0 이면 fromIndex 강제변환 이전에 -1),
    // 존재 인덱스만 [[Get]] 후 strict 비교(§ IsStrictlyEqual). array_like_live_get 이
    // 구멍/접근자/Array.prototype 상속을 해석해 콜백/게터 중 삭제·length 축소를 관측한다.
    pub(super) fn array_index_search_live(
        &mut self,
        reverse: bool,
        o: &Value,
        args: &[Value],
    ) -> Result<Value, String> {
        let needle = args.first().cloned().unwrap_or(Value::Undefined);
        let len_f = match o {
            Value::Arr(a) => a.borrow().len() as f64,
            _ => {
                let lv = self.member_get(o, "length")?;
                to_length(self.to_number_value(&lv)?)
            }
        };
        // §22.1.3.14 step 3 / .17 step 3: len==0 → -1 (fromIndex 강제변환보다 먼저).
        if len_f == 0.0 {
            return Ok(Value::Num(-1.0));
        }

        // fromIndex → 시작 인덱스.
        let k_start: f64 = if !reverse {
            let n = match args.get(1) {
                None | Some(Value::Undefined) => 0.0,
                Some(v) => self.to_integer_or_infinity(v)?,
            };
            if n == f64::INFINITY {
                return Ok(Value::Num(-1.0));
            }
            let n = if n == f64::NEG_INFINITY { 0.0 } else { n };
            if n >= 0.0 {
                n
            } else {
                (len_f + n).max(0.0)
            }
        } else {
            let n = match args.get(1) {
                None | Some(Value::Undefined) => len_f - 1.0,
                Some(v) => self.to_integer_or_infinity(v)?,
            };
            if n == f64::NEG_INFINITY {
                return Ok(Value::Num(-1.0));
            }
            if n >= 0.0 {
                n.min(len_f - 1.0)
            } else {
                len_f + n
            }
        };
        if reverse && k_start < 0.0 {
            return Ok(Value::Num(-1.0));
        }

        // 초거대 length: 존재 own 정수 인덱스만 (오름/내림차순).
        if len_f > MAX_ARRAY_LEN {
            let mut keys: Vec<usize> = match o {
                Value::Obj(m) => m
                    .borrow()
                    .keys()
                    .filter_map(|s| s.parse::<usize>().ok())
                    .filter(|&k| (k as f64) < len_f)
                    .collect(),
                _ => Vec::new(),
            };
            keys.sort_unstable();
            keys.dedup();
            if reverse {
                keys.reverse();
            }
            for &k in &keys {
                let kf = k as f64;
                if !reverse && kf < k_start {
                    continue;
                }
                if reverse && kf > k_start {
                    continue;
                }
                if self.has_property(o, &k.to_string()) {
                    let v = self.member_get(o, &k.to_string())?;
                    if strict_eq(&v, &needle) {
                        return Ok(Value::Num(kf));
                    }
                }
            }
            return Ok(Value::Num(-1.0));
        }

        if !reverse {
            let end = len_f as i64;
            let mut k = k_start as i64;
            while k < end {
                if let Some(v) = self.array_like_live_get(o, k as usize)? {
                    if strict_eq(&v, &needle) {
                        return Ok(Value::Num(k as f64));
                    }
                }
                k += 1;
            }
        } else {
            let mut k = k_start as i64;
            while k >= 0 {
                if let Some(v) = self.array_like_live_get(o, k as usize)? {
                    if strict_eq(&v, &needle) {
                        return Ok(Value::Num(k as f64));
                    }
                }
                k -= 1;
            }
        }
        Ok(Value::Num(-1.0))
    }

    // §23.1.3.13 includes 를 배열/generic array-like 위에서 매 인덱스 live 로 검색한다.
    // indexOf 와 달리 HasProperty 로 거르지 않고 모든 인덱스를 [[Get]](구멍은 undefined 로
    // 방문)하며 SameValueZero 비교(NaN 매칭). len==0 은 fromIndex 강제변환 이전에 false.
    pub(super) fn array_includes_live(
        &mut self,
        o: &Value,
        args: &[Value],
    ) -> Result<Value, String> {
        let needle = args.first().cloned().unwrap_or(Value::Undefined);
        let len_f = match o {
            Value::Arr(a) => a.borrow().len() as f64,
            _ => {
                let lv = self.member_get(o, "length")?;
                to_length(self.to_number_value(&lv)?)
            }
        };
        if len_f == 0.0 {
            return Ok(Value::Bool(false));
        }
        let n = match args.get(1) {
            None | Some(Value::Undefined) => 0.0,
            Some(v) => self.to_integer_or_infinity(v)?,
        };
        if n == f64::INFINITY {
            return Ok(Value::Bool(false));
        }
        let n = if n == f64::NEG_INFINITY { 0.0 } else { n };
        let start = if n >= 0.0 { n } else { (len_f + n).max(0.0) };

        // 초거대 length: 존재하지 않는 인덱스(=undefined)가 범위 안에 있으므로 undefined
        // 검색은 true. 그 외엔 존재 own 정수키만 훑는다.
        if len_f > MAX_ARRAY_LEN {
            if start < len_f && same_value_zero(&needle, &Value::Undefined) {
                return Ok(Value::Bool(true));
            }
            let mut keys: Vec<usize> = match o {
                Value::Obj(m) => m
                    .borrow()
                    .keys()
                    .filter_map(|s| s.parse::<usize>().ok())
                    .filter(|&k| (k as f64) >= start && (k as f64) < len_f)
                    .collect(),
                _ => Vec::new(),
            };
            keys.sort_unstable();
            keys.dedup();
            for &k in &keys {
                let v = self.member_get(o, &k.to_string())?;
                if same_value_zero(&v, &needle) {
                    return Ok(Value::Bool(true));
                }
            }
            return Ok(Value::Bool(false));
        }

        let end = len_f as i64;
        let mut k = start as i64;
        while k < end {
            let v = self.member_get(o, &k.to_string())?;
            if same_value_zero(&v, &needle) {
                return Ok(Value::Bool(true));
            }
            k += 1;
        }
        Ok(Value::Bool(false))
    }

    // §23.1.3.30 Array.prototype.sort 의 generic array-like(Obj) 경로. SortIndexedProperties
    // 로 존재 인덱스의 [[Get]] 값만 수집(구멍 스킵, 접근자·상속 원소 관측) → comparefn 정렬
    // (undefined 는 뒤로, comparefn 결과는 ToNumber) → 0..itemCount 는 [[Set]](Throw),
    // itemCount..len 은 [[Delete]](Throw)로 되쓰기. len 과 itemCount 는 시작에 고정.
    pub(super) fn array_sort_generic(
        &mut self,
        o: &Value,
        cmp_arg: &Value,
    ) -> Result<Value, String> {
        let lv = self.member_get(o, "length")?;
        let len = to_length(self.to_number_value(&lv)?);
        if len > MAX_ARRAY_LEN {
            return Err(self.throw_error("RangeError", "Invalid array length"));
        }
        let n = len as usize;
        let cmp = if matches!(cmp_arg, Value::Undefined) {
            None
        } else {
            Some(cmp_arg.clone())
        };
        // SortIndexedProperties: 존재하는(HasProperty) 인덱스의 [[Get]] 값만. 배열 구멍은
        // has_property 가 true 를 주므로(len 내면 참), array_like_live_get 으로 구멍/접근자/
        // 상속을 정확히 판정한다(None = 진짜 구멍 → 수집 제외).
        let mut items: Vec<Value> = Vec::new();
        for k in 0..n {
            if let Some(v) = self.array_like_live_get(o, k as usize)? {
                items.push(v);
            }
        }
        // 삽입정렬(안정, comparefn 오류 전파). undefined 는 comparefn 없이 항상 뒤로.
        let m = items.len();
        for i in 1..m {
            let mut j = i;
            while j > 0 {
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
                            let v = self.to_number_value(&r)?;
                            if v.is_nan() {
                                0.0
                            } else {
                                v
                            }
                        }
                        None => {
                            let x = self.to_string_value(&items[j - 1])?;
                            let y = self.to_string_value(&items[j])?;
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
        // 되쓰기: Set(Throw) 후 남은 자리 Delete(Throw). len/itemCount 는 고정.
        let item_count = items.len();
        for (j, v) in items.into_iter().enumerate() {
            if !self.ordinary_set(o, &j.to_string(), v, o)? {
                return Err(self.throw_error(
                    "TypeError",
                    "Cannot assign to read only property during sort",
                ));
            }
        }
        for j in item_count..n {
            if !self.delete_own(o, &j.to_string())? {
                return Err(
                    self.throw_error("TypeError", "Cannot delete property during sort")
                );
            }
        }
        Ok(o.clone())
    }

    // §7.3.25 EnumerableOwnProperties — Object.keys/values/entries 공용. [[OwnPropertyKeys]]
    // 로 문자열 own 키를 순서대로 얻고, 각 키마다 live 로 [[GetOwnProperty]](enumerable 검사)
    // 후 [[Get]]. 스냅샷이 아니라 매 키 live 라 getter 가 뒤 키를 지우거나 non-enumerable 로
    // 바꾸는 것을 관측하고, Proxy 는 ownKeys/gOPD/get 트랩을 순서대로 거친다.
    pub(super) fn enumerable_own_live(
        &mut self,
        obj: &Value,
        want_key: bool,
        want_val: bool,
    ) -> Result<Vec<Value>, String> {
        let names = self.call_native(
            Native::ObjectGetOwnPropertyNames,
            None,
            vec![obj.clone()],
        )?;
        let names: Vec<String> = match names {
            Value::Arr(a) => a
                .borrow()
                .iter()
                .filter_map(|v| match v {
                    Value::Str(s) => Some(s.clone()),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        let mut out = Vec::new();
        for k in names {
            let desc = self.call_native(
                Native::ObjectGetOwnPropertyDescriptor,
                None,
                vec![obj.clone(), Value::Str(k.clone())],
            )?;
            let enumerable = matches!(&desc, Value::Obj(m)
                if matches!(m.borrow().get("enumerable"), Some(v) if to_bool(v)));
            if !enumerable {
                continue;
            }
            if want_val {
                let v = self.member_get(obj, &k)?;
                if want_key {
                    out.push(Value::Arr(ArrayObj::new(vec![Value::Str(k), v])));
                } else {
                    out.push(v);
                }
            } else {
                out.push(Value::Str(k));
            }
        }
        Ok(out)
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
                    RefCell::new(ObjMap::new()),
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
                Ok(Value::Bound(Rc::new((target, this_arg, partial, RefCell::new(ObjMap::new())))))
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
                    // typed array 의 정수 인덱스 서술자가 이 경로로 나온다(§10.5.5).
                    Value::Proxy(p) => {
                        self.proxy_revoked_guard(p)?;
                        let (t, h) = (p.0.clone(), p.1.clone());
                        let trap = self.member_get(&h, "getOwnPropertyDescriptor")?;
                        // GetMethod: undefined/null → 타깃 위임, non-callable → TypeError.
                        if matches!(trap, Value::Undefined | Value::Null) {
                            return self.call_native(
                                Native::ObjectGetOwnPropertyDescriptor,
                                None,
                                vec![t, Value::Str(key)],
                            );
                        }
                        if !is_callable(&trap) {
                            return Err(self.throw_error(
                                "TypeError",
                                "'getOwnPropertyDescriptor' trap is not callable",
                            ));
                        }
                        let tkey = self.trap_key(&key);
                        let trap_result = self.call_value(
                            trap,
                            Some(h),
                            vec![t.clone(), tkey],
                        )?;
                        // 결과는 Object 또는 undefined 여야 한다(step 8).
                        if !is_object(&trap_result) && !matches!(trap_result, Value::Undefined) {
                            return Err(self.throw_error(
                                "TypeError",
                                "proxy 'getOwnPropertyDescriptor' must return an object or undefined",
                            ));
                        }
                        // targetDesc = target.[[GetOwnProperty]](key).
                        let target_desc = self.call_native(
                            Native::ObjectGetOwnPropertyDescriptor,
                            None,
                            vec![t.clone(), Value::Str(key.clone())],
                        )?;
                        let td_undefined = matches!(target_desc, Value::Undefined);
                        let td_conf = matches!(&target_desc, Value::Obj(m)
                            if matches!(m.borrow().get("configurable"), Some(v) if to_bool(v)));
                        let td_writable = matches!(&target_desc, Value::Obj(m)
                            if matches!(m.borrow().get("writable"), Some(v) if to_bool(v)));
                        if matches!(trap_result, Value::Undefined) {
                            // 트랩이 undefined 를 보고(프로퍼티 없음).
                            if td_undefined {
                                return Ok(Value::Undefined);
                            }
                            if !td_conf {
                                return Err(self.throw_error("TypeError", "proxy 'getOwnPropertyDescriptor': non-configurable property cannot be reported as non-existent"));
                            }
                            if !self.value_is_extensible(&t)? {
                                return Err(self.throw_error("TypeError", "proxy 'getOwnPropertyDescriptor': existing property of non-extensible target cannot be reported as non-existent"));
                            }
                            return Ok(Value::Undefined);
                        }
                        // 트랩 결과가 Object — 완결 서술자의 configurable/writable 판정
                        // (CompletePropertyDescriptor: 없으면 false).
                        let extensible = self.value_is_extensible(&t)?;
                        // IsCompatiblePropertyDescriptor 핵심 관측: 타깃에 없는 프로퍼티를
                        // non-extensible 타깃에 대해 보고하면 무효.
                        if td_undefined && !extensible {
                            return Err(self.throw_error("TypeError", "proxy 'getOwnPropertyDescriptor': cannot report a new property on a non-extensible target"));
                        }
                        let res_conf = self.has_property(&trap_result, "configurable")
                            && to_bool(&self.member_get(&trap_result, "configurable")?);
                        if !res_conf {
                            // non-configurable 로 보고: 타깃이 없거나 configurable 이면 무효.
                            if td_undefined || td_conf {
                                return Err(self.throw_error("TypeError", "proxy 'getOwnPropertyDescriptor': cannot report non-configurable for a configurable or absent target property"));
                            }
                            // non-writable 로 보고하는데 타깃은 writable 이면 무효.
                            let res_has_w = self.has_property(&trap_result, "writable");
                            let res_w = res_has_w
                                && to_bool(&self.member_get(&trap_result, "writable")?);
                            if res_has_w && !res_w && td_writable {
                                return Err(self.throw_error("TypeError", "proxy 'getOwnPropertyDescriptor': cannot report non-writable for a writable target property"));
                            }
                        }
                        return Ok(trap_result);
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
                            d.insert("writable".to_string(), Value::Bool(a.length_writable()));
                            // length 는 비열거·non-configurable 다 (§10.4.2).
                            d.insert("enumerable".to_string(), Value::Bool(false));
                            d.insert("configurable".to_string(), Value::Bool(false));
                            return Ok(Value::Obj(Rc::new(RefCell::new(d))));
                        }
                        // 구멍 인덱스는 own 프로퍼티가 없다 → 서술자 undefined.
                        let idx_val = key.parse::<usize>().ok().and_then(|i| {
                            // 매핑된 arguments[i] 는 현재 파라미터 값을 보고한다
                            // (§10.4.4.1 [[GetOwnProperty]]: desc.[[Value]] = Get(map, P)).
                            if let Some((name, penv)) = a.mapped_param(i) {
                                return Some(env_get(&penv, &name).unwrap_or(Value::Undefined));
                            }
                            let b = a.borrow();
                            if i < b.len() && !a.is_hole(i) {
                                Some(b[i].clone())
                            } else {
                                None
                            }
                        });
                        match idx_val {
                            Some(v) => {
                                // 배열 인덱스는 기본 { w:t, e:t, c:t } (§10.4.2), non-default
                                // 속성이 있으면 side-table 반영.
                                let at = key
                                    .parse::<usize>()
                                    .ok()
                                    .and_then(|i| a.index_attr(i))
                                    .unwrap_or(
                                        ATTR_WRITABLE | ATTR_ENUMERABLE | ATTR_CONFIGURABLE,
                                    );
                                // defineProperty 로 인덱스에 심긴 접근자는 accessor 서술자
                                // {get,set}로 낸다 — 예전엔 Accessor 값을 그대로 "value" 로
                                // 넣어 gOPD/[[Set]] 이 접근자를 데이터로 오인했다.
                                match v {
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
                                        d.insert("value".to_string(), v);
                                        d.insert(
                                            "writable".to_string(),
                                            Value::Bool(at & ATTR_WRITABLE != 0),
                                        );
                                    }
                                }
                                d.insert("enumerable".to_string(), Value::Bool(at & ATTR_ENUMERABLE != 0));
                                d.insert("configurable".to_string(), Value::Bool(at & ATTR_CONFIGURABLE != 0));
                                // early return — 아래 반환부(2193~)가 배열 enumerable/
                                // configurable 을 항상 true 로 덮어쓰므로 여기서 완성해 반환.
                                return Ok(Value::Obj(Rc::new(RefCell::new(d))));
                            }
                            None => match a.get_prop(&key) {
                                Some(v) => {
                                    // 비인덱스 배열 프로퍼티도 prop_attrs 로 속성을 추적한다
                                    // (defineProperty/freeze 로 non-default 준 경우). 기본 {w,e,c}=true.
                                    let at = a.prop_attr(&key).unwrap_or(
                                        ATTR_WRITABLE | ATTR_ENUMERABLE | ATTR_CONFIGURABLE,
                                    );
                                    match v {
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
                                            d.insert("value".to_string(), v);
                                            d.insert(
                                                "writable".to_string(),
                                                Value::Bool(at & ATTR_WRITABLE != 0),
                                            );
                                        }
                                    }
                                    d.insert(
                                        "enumerable".to_string(),
                                        Value::Bool(at & ATTR_ENUMERABLE != 0),
                                    );
                                    d.insert(
                                        "configurable".to_string(),
                                        Value::Bool(at & ATTR_CONFIGURABLE != 0),
                                    );
                                    return Ok(Value::Obj(Rc::new(RefCell::new(d))));
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
                            // prototype 은 생성자성 함수(비화살표·제너레이터·비async)만 own
                            // 으로 가진다. 화살표·async(비제너레이터)는 서술자 자체가 없다.
                            "prototype" => {
                                let has_proto = f.is_generator || (!f.is_arrow && !f.is_method && !f.is_async);
                                if has_proto || f.props.borrow().contains_key("prototype") {
                                    Some(self.member_get(&target, "prototype")?)
                                } else {
                                    None
                                }
                            }
                            "name" if !materialized => Some(Value::Str(f.name.borrow().clone())),
                            "length" if !materialized => {
                                Some(Value::Num(Self::fn_expected_args(f)))
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
                        let b = inst.fields.borrow();
                        match b.get(&fk) {
                            Some(v) if !is_private_name(&key) => {
                                // 실제 속성 비트로 정확히 보고한다 — 예전엔 writable 만 세우고
                                // 반환부가 enumerable/configurable 을 항상 true 로 덮어, class
                                // extends Error 의 message(non-enumerable)가 열거로 보고됐다.
                                let attrs = prop_attrs(&b, &key);
                                d.insert("value".to_string(), v.clone());
                                d.insert("writable".to_string(), Value::Bool(attrs & ATTR_WRITABLE != 0));
                                d.insert("enumerable".to_string(), Value::Bool(attrs & ATTR_ENUMERABLE != 0));
                                d.insert("configurable".to_string(), Value::Bool(attrs & ATTR_CONFIGURABLE != 0));
                                return Ok(Value::Obj(Rc::new(RefCell::new(d))));
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
                        } else if key == "name"
                            && !c.statics.borrow().contains_key("\u{0}clsdel:name")
                        {
                            d.insert("value".to_string(), Value::Str(c.name.borrow().clone()));
                            d.insert("writable".to_string(), Value::Bool(false));
                            true
                        } else if key == "length"
                            && !c.statics.borrow().contains_key("\u{0}clsdel:length")
                        {
                            // 클래스 생성자의 length 도 own { w:false, e:false, c:true } (§15.7).
                            let n = c.ctor.as_ref().map(|f| Self::fn_expected_args(f)).unwrap_or(0.0);
                            d.insert("value".to_string(), Value::Num(n));
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
                    // 바운드 함수의 사용자 프로퍼티(재정의된 name/length 포함)는 props 맵의
                    // 실제 값·속성으로 보고한다.
                    Value::Bound(b) if b.3.borrow().contains_key(&key) => {
                        let bm = b.3.borrow();
                        let attrs = prop_attrs(&bm, &key);
                        d.insert("value".to_string(), bm.get(&key).cloned().unwrap_or(Value::Undefined));
                        d.insert("writable".to_string(), Value::Bool(attrs & ATTR_WRITABLE != 0));
                        d.insert("enumerable".to_string(), Value::Bool(attrs & ATTR_ENUMERABLE != 0));
                        d.insert("configurable".to_string(), Value::Bool(attrs & ATTR_CONFIGURABLE != 0));
                        return Ok(Value::Obj(Rc::new(RefCell::new(d))));
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
                    // GetMethod: undefined/null → 위임, non-callable → TypeError.
                    if matches!(trap, Value::Undefined | Value::Null) {
                        return self.call_native(
                            Native::ObjectDefineProperty,
                            None,
                            vec![t, Value::Str(key), desc],
                        );
                    }
                    if !is_callable(&trap) {
                        return Err(self.throw_error(
                            "TypeError",
                            "'defineProperty' trap is not callable",
                        ));
                    }
                    let tkey = self.trap_key(&key);
                    let ok = self.call_value(
                        trap,
                        Some(h),
                        vec![t.clone(), tkey, desc.clone()],
                    )?;
                    if !to_bool(&ok) {
                        return Err(self.throw_error(
                            "TypeError",
                            "'defineProperty' on proxy: trap returned falsish for property",
                        ));
                    }
                    // §10.5.6 invariant: 트랩이 true 를 보고한 뒤 타깃과의 정합성 검증.
                    let target_desc = self.call_native(
                        Native::ObjectGetOwnPropertyDescriptor,
                        None,
                        vec![t.clone(), Value::Str(key.clone())],
                    )?;
                    let extensible = self.value_is_extensible(&t)?;
                    // Desc 필드(ToPropertyDescriptor 의미: HasProperty + Get).
                    let d_has_conf = self.has_property(&desc, "configurable");
                    let d_conf = d_has_conf && to_bool(&self.member_get(&desc, "configurable")?);
                    let setting_config_false = d_has_conf && !d_conf;
                    if matches!(target_desc, Value::Undefined) {
                        if !extensible {
                            return Err(self.throw_error("TypeError", "'defineProperty' on proxy: cannot add property to a non-extensible target"));
                        }
                        if setting_config_false {
                            return Err(self.throw_error("TypeError", "'defineProperty' on proxy: cannot define non-configurable property that is absent on target"));
                        }
                        return Ok(target);
                    }
                    // targetDesc 필드(gOPD 결과 Obj 직접 조회).
                    let (td_conf, td_enum, td_writable, td_is_accessor, td_value, td_get, td_set) =
                        if let Value::Obj(m) = &target_desc {
                            let b = m.borrow();
                            let is_acc = b.contains_key("get") || b.contains_key("set");
                            (
                                matches!(b.get("configurable"), Some(v) if to_bool(v)),
                                matches!(b.get("enumerable"), Some(v) if to_bool(v)),
                                matches!(b.get("writable"), Some(v) if to_bool(v)),
                                is_acc,
                                b.get("value").cloned().unwrap_or(Value::Undefined),
                                b.get("get").cloned().unwrap_or(Value::Undefined),
                                b.get("set").cloned().unwrap_or(Value::Undefined),
                            )
                        } else {
                            (true, false, false, false, Value::Undefined, Value::Undefined, Value::Undefined)
                        };
                    let d_has_enum = self.has_property(&desc, "enumerable");
                    let d_enum = d_has_enum && to_bool(&self.member_get(&desc, "enumerable")?);
                    let d_has_writable = self.has_property(&desc, "writable");
                    let d_writable = d_has_writable && to_bool(&self.member_get(&desc, "writable")?);
                    let d_has_value = self.has_property(&desc, "value");
                    let d_value = if d_has_value { self.member_get(&desc, "value")? } else { Value::Undefined };
                    let d_has_get = self.has_property(&desc, "get");
                    let d_get = if d_has_get { self.member_get(&desc, "get")? } else { Value::Undefined };
                    let d_has_set = self.has_property(&desc, "set");
                    let d_set = if d_has_set { self.member_get(&desc, "set")? } else { Value::Undefined };
                    // (15.b) configurable:false 를 보고하는데 타깃은 configurable → 무효.
                    if setting_config_false && td_conf {
                        return Err(self.throw_error("TypeError", "'defineProperty' on proxy: cannot report a configurable target property as non-configurable"));
                    }
                    // (15.a) IsCompatiblePropertyDescriptor — non-configurable 타깃만 실질 제약.
                    if !td_conf {
                        if d_has_conf && d_conf {
                            return Err(self.throw_error("TypeError", "'defineProperty' on proxy: cannot make a non-configurable target property configurable"));
                        }
                        if d_has_enum && d_enum != td_enum {
                            return Err(self.throw_error("TypeError", "'defineProperty' on proxy: cannot change enumerability of a non-configurable target property"));
                        }
                        if td_is_accessor {
                            // 접근자 → 데이터로 못 바꾸고, get/set 도 못 바꿈.
                            if d_has_value || d_has_writable {
                                return Err(self.throw_error("TypeError", "'defineProperty' on proxy: cannot convert a non-configurable accessor to a data property"));
                            }
                            if d_has_get && !same_value(&d_get, &td_get) {
                                return Err(self.throw_error("TypeError", "'defineProperty' on proxy: cannot change getter of a non-configurable property"));
                            }
                            if d_has_set && !same_value(&d_set, &td_set) {
                                return Err(self.throw_error("TypeError", "'defineProperty' on proxy: cannot change setter of a non-configurable property"));
                            }
                        } else {
                            // 데이터 → 접근자로 못 바꿈.
                            if d_has_get || d_has_set {
                                return Err(self.throw_error("TypeError", "'defineProperty' on proxy: cannot convert a non-configurable data property to an accessor"));
                            }
                            if !td_writable {
                                if d_has_writable && d_writable {
                                    return Err(self.throw_error("TypeError", "'defineProperty' on proxy: cannot make a non-writable non-configurable property writable"));
                                }
                                if d_has_value && !same_value(&d_value, &td_value) {
                                    return Err(self.throw_error("TypeError", "'defineProperty' on proxy: cannot change the value of a non-writable non-configurable property"));
                                }
                            }
                        }
                    }
                    // (15.c) 데이터+non-configurable+writable 인데 writable:false 보고 → 무효.
                    if !td_conf && !td_is_accessor && td_writable && d_has_writable && !d_writable {
                        return Err(self.throw_error("TypeError", "'defineProperty' on proxy: cannot report a writable non-configurable property as non-writable"));
                    }
                    return Ok(target);
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
                // 바운드 함수도 ordinary object — 프로퍼티는 props 맵(b.3)에 정의한다.
                // name/length 는 { w:false, e:false, c:true } 계산 프로퍼티라 재정의 전에
                // 현재 값을 실체화해 ordinary_define 이 표준 검증을 하게 한다(Fn 과 동형).
                if let Value::Bound(b) = &target {
                    if matches!(key.as_str(), "name" | "length") {
                        let tomb = format!("\u{0}fndel:{}", key);
                        let missing = !b.3.borrow().contains_key(&key);
                        if missing && !b.3.borrow().contains_key(&tomb) {
                            let cur = if key == "name" {
                                Value::Str(self.native_fn_name(&target))
                            } else {
                                Value::Num(self.native_fn_length(&target))
                            };
                            let mut mm = b.3.borrow_mut();
                            mm.insert(key.clone(), cur);
                            set_prop_attrs(&mut mm, &key, ATTR_CONFIGURABLE);
                        }
                        b.3.borrow_mut().remove(&tomb);
                    }
                    let ext = !self.is_nonextensible_val(&target);
                    self.ordinary_define(&b.3, &key, &desc, ext)?;
                    return Ok(target);
                }
                // 클래스 인스턴스도 ordinary object — 공개 필드는 fields(ObjMap)에 표준
                // 속성 강제로 정의한다(§10.1.6). private 이름은 프로퍼티가 아니라 제외.
                // 예전엔 근사라 속성이 강제 안 돼 verifyProperty(재정의/삭제)가 깨졌다.
                if let Value::Instance(inst) = &target {
                    if !is_private_name(&key) {
                        let ext = !self.is_nonextensible_val(&target);
                        self.ordinary_define(&inst.fields, &key, &desc, ext)?;
                        return Ok(target);
                    }
                }
                // ── 근사 경로 (표준 강제 없음) ── ToPropertyDescriptor (§10.2.4): 각 필드를
                // HasProperty + Get 으로 읽어 서술자 객체가 프로토타입에서 상속한 필드도
                // 반영한다(예전엔 own map 만 봐서 상속 value/get/set/enumerable 등을 놓쳤다).
                // Get 은 스펙 순서(enumerable→configurable→value→writable→get→set)로 한다.
                if !is_object(&desc) {
                    return Ok(target);
                }
                let has_enumerable = self.has_property(&desc, "enumerable");
                let has_configurable = self.has_property(&desc, "configurable");
                let has_value = self.has_property(&desc, "value");
                let has_writable = self.has_property(&desc, "writable");
                let has_get = self.has_property(&desc, "get");
                let has_set = self.has_property(&desc, "set");
                let enumerable =
                    has_enumerable && to_bool(&self.member_get(&desc, "enumerable")?);
                let configurable =
                    has_configurable && to_bool(&self.member_get(&desc, "configurable")?);
                let value_v = if has_value {
                    Some(self.member_get(&desc, "value")?)
                } else {
                    None
                };
                let writable = has_writable && to_bool(&self.member_get(&desc, "writable")?);
                let get_v = if has_get {
                    self.member_get(&desc, "get")?
                } else {
                    Value::Undefined
                };
                let set_v = if has_set {
                    self.member_get(&desc, "set")?
                } else {
                    Value::Undefined
                };
                // §10.2.4: get/set 은 callable 이거나 undefined. accessor+data 는 동시 지정 불가.
                if has_get && !matches!(get_v, Value::Undefined) && !is_callable(&get_v) {
                    return Err(self.throw_error("TypeError", "Getter must be a function"));
                }
                if has_set && !matches!(set_v, Value::Undefined) && !is_callable(&set_v) {
                    return Err(self.throw_error("TypeError", "Setter must be a function"));
                }
                if (has_get || has_set) && (has_value || has_writable) {
                    return Err(self.throw_error(
                        "TypeError",
                        "Invalid property descriptor. Cannot both specify accessors and a value or writable attribute",
                    ));
                }
                let entry = if has_get || has_set {
                    // get_v/set_v 는 아래 배열 인덱스 재정의 검증에서도 쓰므로 clone.
                    let g = if is_callable(&get_v) { Some(get_v.clone()) } else { None };
                    let st = if is_callable(&set_v) { Some(set_v.clone()) } else { None };
                    Some(Value::Accessor(Rc::new(super::AccessorPair { get: g, set: st })))
                } else {
                    value_v
                };
                // §10.1.6.3: value/get/set 없는 generic 서술자로 **새** 배열 프로퍼티를 정의
                // 하면 value=undefined 데이터로 생성한다(기존 프로퍼티는 속성만 병합 — entry
                // None 유지). 예전엔 새 인덱스/prop 이 아예 안 만들어졌다("0" doesn't exist).
                let entry = if entry.is_none() && key.as_str() != "length" {
                    if let Value::Arr(a) = &target {
                        let exists = if let Ok(i) = key.parse::<usize>() {
                            i < a.borrow().len() && !a.is_hole(i)
                        } else {
                            a.get_prop(&key).is_some()
                        };
                        if exists {
                            entry
                        } else {
                            Some(Value::Undefined)
                        }
                    } else {
                        entry
                    }
                } else {
                    entry
                };
                // 배열 length 는 exotic (§10.4.2.1): {enumerable:false, configurable:false}
                // 데이터 프로퍼티. accessor·configurable:true·enumerable:true 로 재정의하면
                // TypeError. (value 로 길이 변경 + writable:false 고정은 배열 표현 확장이
                // 필요해 별도 — 여기선 서술자 속성 위반만 막는다.)
                if let (Value::Arr(a), "length") = (&target, key.as_str()) {
                    if matches!(entry, Some(Value::Accessor(_))) || configurable || enumerable {
                        return Err(self.redefine_err());
                    }
                    // length 가 non-writable 이면(§10.4.2.4): writable:true 로 되돌리기
                    // (non-configurable 의 writable false→true 금지)나 다른 값으로의 변경은
                    // TypeError. 같은 값/미변경은 허용.
                    let mut length_set_ok = true;
                    if !a.length_writable() {
                        if has_writable && writable {
                            return Err(self.redefine_err());
                        }
                        if let Some(val) = &entry {
                            let newn = self.to_number_value(val)?;
                            let cur = a.borrow().len() as f64;
                            if newn != cur {
                                return Err(self.redefine_err());
                            }
                        }
                    } else if let Some(val) = &entry {
                        let a2 = a.clone();
                        length_set_ok = self.array_set_length(&a2, val.clone())?;
                    }
                    // writable:false 면 length 를 고정한다(§10.4.2.4: 축소 실패 여부와 무관).
                    if has_writable && !writable {
                        a.set_length_writable(false);
                    }
                    // 삭제 불가한 non-configurable 인덱스에 막혀 요청 길이로 못 줄이면 TypeError.
                    if !length_set_ok {
                        return Err(self.redefine_err());
                    }
                    return Ok(target);
                }
                // value/accessor 없는 desc 로 배열 인덱스 재정의(속성만): 값은 그대로 두고
                // attrs 만 병합 저장(§10.1.6). value 있는 경로는 아래 Arr index arm 에서.
                if entry.is_none() {
                    if let Value::Arr(a) = &target {
                        if let Ok(i) = key.parse::<usize>() {
                            if i < a.borrow().len() && !a.is_hole(i) {
                                let cur = a.index_attr(i).unwrap_or(
                                    ATTR_WRITABLE | ATTR_ENUMERABLE | ATTR_CONFIGURABLE,
                                );
                                // §10.1.6.3: non-configurable 인덱스는 configurable false→true,
                                // enumerable 변경, non-writable→writable 이 금지된다(TypeError).
                                if cur & ATTR_CONFIGURABLE == 0 {
                                    let cur_enum = cur & ATTR_ENUMERABLE != 0;
                                    let cur_wr = cur & ATTR_WRITABLE != 0;
                                    let is_acc = matches!(
                                        a.borrow().get(i),
                                        Some(Value::Accessor(_))
                                    );
                                    if (has_configurable && configurable)
                                        || (has_enumerable && enumerable != cur_enum)
                                        || (!is_acc && has_writable && writable && !cur_wr)
                                    {
                                        return Err(self.redefine_err());
                                    }
                                }
                                let wbit = if has_writable {
                                    if writable { ATTR_WRITABLE } else { 0 }
                                } else {
                                    cur & ATTR_WRITABLE
                                };
                                let ebit = if has_enumerable {
                                    if enumerable { ATTR_ENUMERABLE } else { 0 }
                                } else {
                                    cur & ATTR_ENUMERABLE
                                };
                                let cbit = if has_configurable {
                                    if configurable { ATTR_CONFIGURABLE } else { 0 }
                                } else {
                                    cur & ATTR_CONFIGURABLE
                                };
                                let attrs = wbit | ebit | cbit;
                                if attrs
                                    != ATTR_WRITABLE | ATTR_ENUMERABLE | ATTR_CONFIGURABLE
                                {
                                    a.set_index_attr(i, attrs);
                                } else {
                                    a.clear_index_attr(i);
                                }
                                // [[ParameterMap]] (§10.4.4.2): writable:false 로 만들면
                                // 매핑 해제(이후 파라미터 변경이 arguments[i] 에 반영 안 됨).
                                if has_writable && !writable {
                                    a.unmap_param(i);
                                }
                            }
                        } else if a.get_prop(&key).is_some() {
                            // 기존 비인덱스 프로퍼티: 속성만 병합(§10.1.6, prop_attrs).
                            let cur = a.prop_attr(&key).unwrap_or(
                                ATTR_WRITABLE | ATTR_ENUMERABLE | ATTR_CONFIGURABLE,
                            );
                            if cur & ATTR_CONFIGURABLE == 0 {
                                let cur_enum = cur & ATTR_ENUMERABLE != 0;
                                let cur_wr = cur & ATTR_WRITABLE != 0;
                                let is_acc =
                                    matches!(a.get_prop(&key), Some(Value::Accessor(_)));
                                if (has_configurable && configurable)
                                    || (has_enumerable && enumerable != cur_enum)
                                    || (!is_acc && has_writable && writable && !cur_wr)
                                {
                                    return Err(self.redefine_err());
                                }
                            }
                            let wbit = if has_writable {
                                if writable { ATTR_WRITABLE } else { 0 }
                            } else {
                                cur & ATTR_WRITABLE
                            };
                            let ebit = if has_enumerable {
                                if enumerable { ATTR_ENUMERABLE } else { 0 }
                            } else {
                                cur & ATTR_ENUMERABLE
                            };
                            let cbit = if has_configurable {
                                if configurable { ATTR_CONFIGURABLE } else { 0 }
                            } else {
                                cur & ATTR_CONFIGURABLE
                            };
                            let attrs = wbit | ebit | cbit;
                            if attrs != ATTR_WRITABLE | ATTR_ENUMERABLE | ATTR_CONFIGURABLE {
                                a.set_prop_attr(key.clone(), attrs);
                            } else {
                                a.clear_prop_attr(&key);
                            }
                        }
                    }
                }
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
                            // 유효 배열 인덱스는 0..2^32-1(canonical). 2^32-1 이상(예: "4294967295")은
                            // 배열 인덱스가 아니라 일반 프로퍼티다 — dense Vec 를 그만큼 키우면
                            // OOM 이므로 props 에 저장한다(§10.4.2.1, §6.1.7 array index 정의).
                            let as_index =
                                key.parse::<usize>().ok().filter(|&i| (i as u64) < 4294967295);
                            if let Some(i) = as_index {
                                let old_len = a.borrow().len();
                                // 값 설정 전에 "기존 데이터 프로퍼티였나"(§10.1.6 재정의 여부).
                                let existed = i < old_len && !a.is_hole(i);
                                // §10.4.2.1 3.c: 인덱스가 현재 length 이상인데 length 가
                                // non-writable 이면 배열을 늘릴 수 없다 → 거부(TypeError).
                                // (범위 내 인덱스/구멍 채우기는 length 를 안 늘리므로 허용.)
                                if i >= old_len && !a.length_writable() {
                                    return Err(self.throw_error(
                                        "TypeError",
                                        "Cannot define array index beyond a non-writable length",
                                    ));
                                }
                                // §10.1.6.3 ValidateAndApplyPropertyDescriptor: 기존
                                // non-configurable 인덱스는 configurable false→true, enumerable
                                // 변경, data↔accessor 전환이 금지되고, non-writable 데이터는
                                // writable false→true·value 변경이, 접근자는 get/set 변경이
                                // 금지된다(모두 TypeError). 예전엔 검증 없이 덮어썼다.
                                if existed {
                                    let cur_attrs = a.index_attr(i).unwrap_or(
                                        ATTR_WRITABLE | ATTR_ENUMERABLE | ATTR_CONFIGURABLE,
                                    );
                                    if cur_attrs & ATTR_CONFIGURABLE == 0 {
                                        let cur_enum = cur_attrs & ATTR_ENUMERABLE != 0;
                                        let cur_writable = cur_attrs & ATTR_WRITABLE != 0;
                                        let cur_item = a.borrow().get(i).cloned();
                                        let cur_is_acc =
                                            matches!(cur_item, Some(Value::Accessor(_)));
                                        let new_is_acc = matches!(&val, Value::Accessor(_));
                                        if (has_configurable && configurable)
                                            || (has_enumerable && enumerable != cur_enum)
                                            || (new_is_acc != cur_is_acc)
                                        {
                                            return Err(self.redefine_err());
                                        }
                                        if let (Some(Value::Accessor(cur_acc)), Value::Accessor(_)) =
                                            (&cur_item, &val)
                                        {
                                            let cur_get =
                                                cur_acc.get.clone().unwrap_or(Value::Undefined);
                                            let cur_set =
                                                cur_acc.set.clone().unwrap_or(Value::Undefined);
                                            if (has_get && !same_value(&get_v, &cur_get))
                                                || (has_set && !same_value(&set_v, &cur_set))
                                            {
                                                return Err(self.redefine_err());
                                            }
                                        } else if !cur_is_acc && !cur_writable {
                                            if has_writable && writable {
                                                return Err(self.redefine_err());
                                            }
                                            let cur_val =
                                                cur_item.unwrap_or(Value::Undefined);
                                            if has_value && !same_value(&val, &cur_val) {
                                                return Err(self.redefine_err());
                                            }
                                        }
                                    }
                                }
                                // [[ParameterMap]] (§10.4.4.2): 매핑된 인덱스 defineProperty.
                                // 접근자로 바뀌면 매핑 해제, 데이터면 value 로 파라미터 갱신하고
                                // writable:false 면 해제.
                                if let Some((name, penv)) = a.mapped_param(i) {
                                    if matches!(&val, Value::Accessor(_)) {
                                        a.unmap_param(i);
                                    } else {
                                        if has_value {
                                            env_set(&penv, &name, val.clone());
                                        }
                                        if has_writable && !writable {
                                            a.unmap_param(i);
                                        }
                                    }
                                }
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
                                // 인덱스별 속성 병합(§10.1.6): 지정된 필드(has_*)만 반영,
                                // 미지정은 재정의면 기존값, 새 정의면 false. default(7)면 제거.
                                let cur = if existed {
                                    a.index_attr(i).unwrap_or(
                                        ATTR_WRITABLE | ATTR_ENUMERABLE | ATTR_CONFIGURABLE,
                                    )
                                } else {
                                    0
                                };
                                let wbit = if has_writable {
                                    if writable { ATTR_WRITABLE } else { 0 }
                                } else {
                                    cur & ATTR_WRITABLE
                                };
                                let ebit = if has_enumerable {
                                    if enumerable { ATTR_ENUMERABLE } else { 0 }
                                } else {
                                    cur & ATTR_ENUMERABLE
                                };
                                let cbit = if has_configurable {
                                    if configurable { ATTR_CONFIGURABLE } else { 0 }
                                } else {
                                    cur & ATTR_CONFIGURABLE
                                };
                                let attrs = wbit | ebit | cbit;
                                if attrs
                                    != ATTR_WRITABLE | ATTR_ENUMERABLE | ATTR_CONFIGURABLE
                                {
                                    a.set_index_attr(i, attrs);
                                } else {
                                    a.clear_index_attr(i);
                                }
                            } else {
                                // 비인덱스 배열 프로퍼티: §10.1.6.3 검증 + prop_attrs 병합.
                                let cur_val = a.get_prop(&key);
                                let existed = cur_val.is_some();
                                if existed {
                                    let cur = a.prop_attr(&key).unwrap_or(
                                        ATTR_WRITABLE | ATTR_ENUMERABLE | ATTR_CONFIGURABLE,
                                    );
                                    if cur & ATTR_CONFIGURABLE == 0 {
                                        let cur_enum = cur & ATTR_ENUMERABLE != 0;
                                        let cur_wr = cur & ATTR_WRITABLE != 0;
                                        let cur_is_acc =
                                            matches!(cur_val, Some(Value::Accessor(_)));
                                        let new_is_acc = matches!(&val, Value::Accessor(_));
                                        if (has_configurable && configurable)
                                            || (has_enumerable && enumerable != cur_enum)
                                            || (new_is_acc != cur_is_acc)
                                        {
                                            return Err(self.redefine_err());
                                        }
                                        if let (Some(Value::Accessor(cur_acc)), Value::Accessor(_)) =
                                            (&cur_val, &val)
                                        {
                                            let cg =
                                                cur_acc.get.clone().unwrap_or(Value::Undefined);
                                            let cs =
                                                cur_acc.set.clone().unwrap_or(Value::Undefined);
                                            if (has_get && !same_value(&get_v, &cg))
                                                || (has_set && !same_value(&set_v, &cs))
                                            {
                                                return Err(self.redefine_err());
                                            }
                                        } else if !cur_is_acc && !cur_wr {
                                            if has_writable && writable {
                                                return Err(self.redefine_err());
                                            }
                                            if has_value
                                                && !same_value(
                                                    &val,
                                                    cur_val.as_ref().unwrap_or(&Value::Undefined),
                                                )
                                            {
                                                return Err(self.redefine_err());
                                            }
                                        }
                                    }
                                }
                                // 속성 병합: 미지정은 재정의면 기존, 새 정의면 false.
                                let cur = if existed {
                                    a.prop_attr(&key).unwrap_or(
                                        ATTR_WRITABLE | ATTR_ENUMERABLE | ATTR_CONFIGURABLE,
                                    )
                                } else {
                                    0
                                };
                                let wbit = if has_writable {
                                    if writable { ATTR_WRITABLE } else { 0 }
                                } else {
                                    cur & ATTR_WRITABLE
                                };
                                let ebit = if has_enumerable {
                                    if enumerable { ATTR_ENUMERABLE } else { 0 }
                                } else {
                                    cur & ATTR_ENUMERABLE
                                };
                                let cbit = if has_configurable {
                                    if configurable { ATTR_CONFIGURABLE } else { 0 }
                                } else {
                                    cur & ATTR_CONFIGURABLE
                                };
                                let attrs = wbit | ebit | cbit;
                                a.set_prop(key.clone(), val);
                                if attrs
                                    != (ATTR_WRITABLE | ATTR_ENUMERABLE | ATTR_CONFIGURABLE)
                                {
                                    a.set_prop_attr(key, attrs);
                                } else {
                                    a.clear_prop_attr(&key);
                                }
                            }
                        }
                        Value::Class(c) => {
                            c.statics.borrow_mut().insert(key, val);
                        }
                        // 내장(Object/Number/…)의 정적 프로퍼티 재정의는 native_props 오버라이드로
                        // 저장하고 삭제 툼스톤을 해제한다(member_get 이 이 값을 우선한다). 예전엔
                        // _ => {} 라 defineProperty(Number,"isNaN",{value}) 가 무시됐다.
                        Value::Native(n) => {
                            let ov = self.native_props.entry(*n).or_default();
                            ov.remove(&format!("\u{0}del:{}", key));
                            ov.insert(key, val);
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
                // 상속 메서드 없음. Proxy/Fn/Instance/Arr 등 모든 객체형 프로토타입도 링크한다
                // (예전엔 Obj/Null 만 저장해 Object.create(proxy) 의 프로토타입이 유실됐다).
                let mut map = ObjMap::new();
                if matches!(proto, Value::Null) {
                    map.insert("__proto__".to_string(), Value::Null);
                } else {
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
                // Proxy 도 객체이므로 [[SetPrototypeOf]](트랩)을 타야 한다 — 예전엔
                // Obj/Fn 만 게이트해 프록시의 setPrototypeOf 가 조용히 무시됐다.
                if matches!(target, Value::Obj(_) | Value::Fn(_) | Value::Proxy(_)) {
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
                // Proxy: ownKeys 트랩 결과 중 심볼 키만 (§10.5.11 + 필터).
                if let Some(Value::Proxy(p)) = args.first() {
                    let p = p.clone();
                    let syms: Vec<Value> = self
                        .proxy_own_keys(&p)?
                        .into_iter()
                        .filter(|k| matches!(k, Value::Symbol(_)))
                        .collect();
                    return Ok(Value::Arr(ArrayObj::new(syms)));
                }
                let keys: Vec<String> = match args.first() {
                    Some(Value::Obj(m)) => m
                        .borrow()
                        .keys()
                        .filter(|k| k.starts_with("\u{0}@@"))
                        .cloned()
                        .collect(),
                    // 클래스 인스턴스의 심볼 키 필드([sym]=v). 예전엔 Instance arm 이 없어
                    // getOwnPropertySymbols 가 빈 배열이었다.
                    Some(Value::Instance(i)) => i
                        .fields
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
            Native::ArrayCtor => {
                // §23.1.1.1: 단일 Number 인자는 유효한 uint32(0..2^32-1)여야 한다.
                // SameValueZero(len, ToUint32(len)) 이 false 면(정수 아님/음수/2^32 이상)
                // RangeError. 예전엔 조용히 1원소 배열([2^32])로 만들었다.
                if let [Value::Num(len)] = args.as_slice() {
                    if !(len.fract() == 0.0 && *len >= 0.0 && *len < 4294967296.0) {
                        return Err(self.throw_error("RangeError", "Invalid array length"));
                    }
                }
                Ok(self.coerce_object_call(&Value::Native(n), &args).unwrap_or(Value::Undefined))
            }
            Native::ObjectCtor => {
                Ok(self.coerce_object_call(&Value::Native(n), &args).unwrap_or(Value::Undefined))
            }
            // freeze/seal/preventExtensions — 모든 객체 종류(Obj/Arr/Fn/Instance/Class/Map/Set)에
            // 통일된 무결성 테이블로 상태를 남긴다. 대입 경로가 이 상태를 보고 변경을 막는다.
            Native::ObjectFreeze | Native::ObjectSeal | Native::ObjectPreventExt => {
                let arg = args.into_iter().next().unwrap_or(Value::Undefined);
                // Proxy 의 preventExtensions 는 트랩을 탄다(§10.5.4). freeze/seal 은
                // SetIntegrityLevel 이 ownKeys/defineProperty 트랩까지 조합하므로 여기선
                // preventExtensions 트랩만 우선 처리. 트랩이 false 를 보고하면
                // Object.preventExtensions 는 TypeError(§20.1.2.19).
                if let (Value::Proxy(_), Native::ObjectPreventExt) = (&arg, n) {
                    if !self.value_prevent_extensions(&arg)? {
                        return Err(self.throw_error(
                            "TypeError",
                            "Object.preventExtensions: proxy 'preventExtensions' trap returned falsish",
                        ));
                    }
                    return Ok(arg);
                }
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
                    // 배열: 각 존재 인덱스의 속성도 조인다(§7.3.15) — seal→configurable
                    // 제거, freeze→writable 도 제거. gOPD 가 index_attrs 를 반영한다.
                    if let Value::Arr(a) = &arg {
                        for i in a.present_indices() {
                            let cur = a.index_attr(i).unwrap_or(
                                ATTR_WRITABLE | ATTR_ENUMERABLE | ATTR_CONFIGURABLE,
                            );
                            let mut na = cur & !ATTR_CONFIGURABLE;
                            if bit == super::INTEG_FROZEN {
                                na &= !ATTR_WRITABLE;
                            }
                            a.set_index_attr(i, na);
                        }
                        // 배열의 비인덱스 문자열 프로퍼티(arr.foo, arguments.foo 등)도 조인다.
                        for (k, v) in a.own_props() {
                            let is_acc = matches!(v, Value::Accessor(_));
                            let cur = a.prop_attr(&k).unwrap_or(
                                ATTR_WRITABLE | ATTR_ENUMERABLE | ATTR_CONFIGURABLE,
                            );
                            let mut na = cur & !ATTR_CONFIGURABLE;
                            if bit == super::INTEG_FROZEN && !is_acc {
                                na &= !ATTR_WRITABLE;
                            }
                            a.set_prop_attr(k, na);
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
                // Proxy 의 isExtensible 은 트랩을 탄다(§10.5.3). isFrozen/isSealed 는
                // TestIntegrityLevel 이 [[IsExtensible]]+[[OwnPropertyKeys]] 를 조합하는데
                // 여기선 isExtensible 트랩만 우선 처리(freeze/seal 판정은 근사 유지).
                if let (Value::Proxy(p), Native::ObjectIsExtensible) = (&arg, n) {
                    let p = p.clone();
                    return Ok(Value::Bool(self.proxy_is_extensible(&p)?));
                }
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
                        // name/length 는 계산 own 프로퍼티지만 delete 됐으면(툼스톤) 없는 것.
                        let live_computed = matches!(key.as_str(), "name" | "length")
                            && !c.statics.borrow().contains_key(&format!("\u{0}clsdel:{}", key));
                        !is_private_name(&key)
                            && (c.statics.borrow().contains_key(&key)
                                || c.static_getters.contains_key(&key)
                                || c.static_setters.contains_key(&key)
                                || key == "prototype"
                                || live_computed)
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
                            // 삭제된(툼스톤) 정적 프로퍼티는 own 이 아니다.
                            if self.native_prop_deleted(v, &key) {
                                false
                            } else {
                                // 프렐류드가 native_props 에 얹은 정적 메서드도 own 이다.
                                (!is_internal_key(&key)
                                    && self
                                        .native_props
                                        .get(n)
                                        .map_or(false, |m| m.contains_key(&key)))
                                    || self
                                        .native_ctor_own_keys(n)
                                        .map(|ks| ks.iter().any(|k| *k == key))
                                        .unwrap_or(false)
                            }
                        } else if let Value::Bound(b) = v {
                            b.3.borrow().contains_key(&key)
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
                // §20.1.3.4: desc = O.[[GetOwnProperty]](P); undefined 면 false, 아니면
                // desc.[[Enumerable]]. gOPD 로 통일 — 예전엔 own_enumerable_entries(문자열
                // 키만)라 심볼 키 필드([sym]=v)가 gOPD(enumerable:true)와 어긋났다.
                let keyv = args.first().cloned().unwrap_or(Value::Undefined);
                let recvv = recv.unwrap_or(Value::Undefined);
                let desc = self.call_native(
                    Native::ObjectGetOwnPropertyDescriptor,
                    None,
                    vec![recvv, keyv],
                )?;
                let enumerable = matches!(&desc, Value::Obj(m)
                    if matches!(m.borrow().get("enumerable"), Some(v) if to_bool(v)));
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
                // §20.1.3.6: tag = Get(O, @@toStringTag) — 프로토타입 체인·getter·Proxy
                // 트랩까지 조회한다. 문자열이면 builtin tag 를 덮어쓴다. 예전엔 Obj own 맵
                // 직접조회라 상속 getter(typed array @@toStringTag)·Proxy 를 놓쳐 typed
                // array 가 "[object Object]" 였다. Undefined/Null 은 조회하지 않는다.
                let tag = match &recv {
                    Some(v) if !matches!(v, Value::Undefined | Value::Null) => {
                        match self.member_get(v, "\u{0}@@toStringTag")? {
                            Value::Str(t) => t,
                            _ => tag,
                        }
                    }
                    _ => tag,
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
                    // §20.2.3.5: [[SourceText]] 없는 호출 가능 객체(함수 Proxy 등)는
                    // NativeFunction 문법(step 4). 호출 불가면 TypeError(step 5).
                    _ if is_callable(&f) => "function () { [native code] }".to_string(),
                    _ => {
                        return Err(self.throw_error(
                            "TypeError",
                            "Function.prototype.toString requires that 'this' be a Function",
                        ))
                    }
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
                                    // yield 된 값의 Await 가 거부되면 그 예외는 yield 지점에서
                                    // 제너레이터 안으로 던져진 것과 같다 → 제너레이터 종료
                                    // (§27.6.3.8). 예전엔 promise 만 거부하고 상태를 안 닫아,
                                    // 다음 next() 가 그 다음 yield 로 진행했다(done:false 오답).
                                    Self::gen_mark_done(&gs);
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
                // brand 체크: Map(또는 extends Map 파생 인스턴스)이 아니면 TypeError
                // (§24.1.3, RequireInternalSlot). 예전엔 일반 Err→Error 라 검사가 깨졌다.
                let Some(m) = recv_map_data(&recv) else {
                    return Err(self.throw_error(
                        "TypeError",
                        "Map.prototype method called on incompatible receiver",
                    ));
                };
                self.map_method(m, op, args)
            }
            Native::Set(op) => {
                let Some(s) = recv_set_data(&recv) else {
                    return Err(self.throw_error(
                        "TypeError",
                        "Set.prototype method called on incompatible receiver",
                    ));
                };
                self.set_method(s, op, args)
            }
            // get Map.prototype.size / Set.prototype.size — brand 체크 후 원소 수.
            // extends Map/Set 파생 인스턴스도 내부 슬롯을 통해 지원.
            Native::MapSize => match recv_map_data(&recv) {
                Some(m) => Ok(Value::Num(m.borrow().len() as f64)),
                None => Err(self.throw_error(
                    "TypeError",
                    "get Map.prototype.size called on incompatible receiver",
                )),
            },
            Native::SetSize => match recv_set_data(&recv) {
                Some(s) => Ok(Value::Num(s.borrow().len() as f64)),
                None => Err(self.throw_error(
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
            // Symbol.prototype[Symbol.toPrimitive](hint) — hint 무시하고 thisSymbolValue.
            Native::SymbolToPrimitive => {
                let this = recv.unwrap_or(Value::Undefined);
                self.this_prim_value(&this, PrimBrand::Symbol)
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
                    // Symbol.prototype.toString (§20.4.3.3): "Symbol(desc)". 심볼은 암묵적
                    // 문자열 변환이 TypeError 지만 명시적 toString 은 허용된다.
                    PrimBrand::Symbol => Ok(Value::Str(to_display(&prim))),
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
                // RegExp.prototype 자신은 정규식이 아니지만 스펙상 기본값(source "(?:)",
                // 개별 플래그 undefined)을 낸다. 그 외 비정규식 객체/원시값은 TypeError.
                let is_proto = matches!(
                    (&this, &self.regexp_proto),
                    (Value::Obj(a), Value::Obj(b)) if std::rc::Rc::ptr_eq(a, b)
                );
                Ok(match kind {
                    RA::Source => {
                        // §22.2.6.13: this 가 정규식도 RegExp.prototype 도 아니면 TypeError.
                        if sf.is_none() && !is_proto {
                            return Err(self.throw_error(
                                "TypeError",
                                "RegExp.prototype.source getter called on non-RegExp",
                            ));
                        }
                        Value::Str(match &sf {
                            Some((s, _)) if !s.is_empty() => s.clone(),
                            _ => "(?:)".to_string(),
                        })
                    }
                    RA::Flags => {
                        // §22.2.6.4: this 가 객체가 아니면 TypeError. 각 플래그 프로퍼티를
                        // [[Get]]+ToBoolean 으로 읽어(예외 전파) d,g,i,m,s,u,v,y 순으로 조립한다.
                        // 제네릭이라 정규식 아닌 객체(플래그 프로퍼티만 가진)도 동작한다.
                        if !is_object(&this) {
                            return Err(self.throw_error(
                                "TypeError",
                                "RegExp.prototype.flags getter called on non-object",
                            ));
                        }
                        let mut out = String::new();
                        for (prop, ch) in [
                            ("hasIndices", 'd'),
                            ("global", 'g'),
                            ("ignoreCase", 'i'),
                            ("multiline", 'm'),
                            ("dotAll", 's'),
                            ("unicode", 'u'),
                            ("unicodeSets", 'v'),
                            ("sticky", 'y'),
                        ] {
                            if to_bool(&self.member_get(&this, prop)?) {
                                out.push(ch);
                            }
                        }
                        Value::Str(out)
                    }
                    // 개별 플래그(§22.2.6.x): 정규식이면 포함 여부(bool), RegExp.prototype 이면
                    // undefined, 그 외 비정규식은 TypeError.
                    _ => match &sf {
                        Some((_, f)) => {
                            let ch = RA::table()
                                .iter()
                                .find(|(_, k, _)| *k == kind)
                                .and_then(|(_, _, c)| *c)
                                .unwrap();
                            Value::Bool(f.contains(ch))
                        }
                        None if is_proto => Value::Undefined,
                        None => {
                            return Err(self.throw_error(
                                "TypeError",
                                "RegExp flag getter called on non-RegExp",
                            ))
                        }
                    },
                })
            }
            // RegExp.prototype[Symbol.match/replace/split/search/matchAll]: this=정규식,
            // args=[문자열, ...]. 기존 String 측 구현으로 위임한다(수신자/인자 교환).
            Native::RegexSym(op) => {
                let re = recv.unwrap_or(Value::Undefined);
                if let natives::StrOp::Search = op {
                    // §22.2.6.14 RegExp.prototype[@@search](string): brand + ToString +
                    // lastIndex 저장/복원 + RegExpExec + result.index.
                    if !is_object(&re) {
                        return Err(self.throw_error(
                            "TypeError",
                            "RegExp.prototype[Symbol.search] called on non-object",
                        ));
                    }
                    let s = self.to_string_value(&args.first().cloned().unwrap_or(Value::Undefined))?;
                    let prev = self.member_get(&re, "lastIndex")?;
                    if !same_value(&prev, &Value::Num(0.0)) {
                        self.set_throw(&re, "lastIndex", Value::Num(0.0))?;
                    }
                    let result = self.regexp_exec(&re, &s)?;
                    let cur = self.member_get(&re, "lastIndex")?;
                    if !same_value(&cur, &prev) {
                        self.set_throw(&re, "lastIndex", prev)?;
                    }
                    return if matches!(result, Value::Null) {
                        Ok(Value::Num(-1.0))
                    } else {
                        self.member_get(&result, "index")
                    };
                }
                if let natives::StrOp::Match = op {
                    // §22.2.6.8 RegExp.prototype[@@match](string).
                    if !is_object(&re) {
                        return Err(self.throw_error(
                            "TypeError",
                            "RegExp.prototype[Symbol.match] called on non-object",
                        ));
                    }
                    let s = self.to_string_value(&args.first().cloned().unwrap_or(Value::Undefined))?;
                    let flags_val = self.member_get(&re, "flags")?;
                    let flags = self.to_string_value(&flags_val)?;
                    if !flags.contains('g') {
                        // 비전역: 단일 RegExpExec 결과.
                        return self.regexp_exec(&re, &s);
                    }
                    // 전역: lastIndex=0 후 모든 매치의 match[0] 수집(빈 매치는 lastIndex 전진).
                    self.set_throw(&re, "lastIndex", Value::Num(0.0))?;
                    let mut out: Vec<Value> = Vec::new();
                    let slen = s.chars().count();
                    let cap = slen.saturating_mul(2) + 1000; // 무한루프/OOM 방지 안전 상한
                    for _ in 0..cap {
                        let result = self.regexp_exec(&re, &s)?;
                        if matches!(result, Value::Null) {
                            break;
                        }
                        let m0 = self.member_get(&result, "0")?;
                        let match_str = self.to_string_value(&m0)?;
                        let empty = match_str.is_empty();
                        out.push(Value::Str(match_str));
                        if empty {
                            // AdvanceStringIndex(근사): lastIndex + 1.
                            let li = to_num(&self.member_get(&re, "lastIndex")?);
                            self.set_throw(&re, "lastIndex", Value::Num(li + 1.0))?;
                        }
                    }
                    return if out.is_empty() {
                        Ok(Value::Null)
                    } else {
                        Ok(Value::Arr(ArrayObj::new(out)))
                    };
                }
                if let natives::StrOp::Replace = op {
                    // §22.2.6.11 RegExp.prototype[@@replace](string, replaceValue).
                    if !is_object(&re) {
                        return Err(self.throw_error(
                            "TypeError",
                            "RegExp.prototype[Symbol.replace] called on non-object",
                        ));
                    }
                    let s = self.to_string_value(&args.first().cloned().unwrap_or(Value::Undefined))?;
                    let repl_val = args.get(1).cloned().unwrap_or(Value::Undefined);
                    let functional = is_callable(&repl_val);
                    let repl_str = if functional {
                        None
                    } else {
                        Some(self.to_string_value(&repl_val)?)
                    };
                    let global = to_bool(&self.member_get(&re, "global")?);
                    if global {
                        self.set_throw(&re, "lastIndex", Value::Num(0.0))?;
                    }
                    let schars: Vec<char> = s.chars().collect();
                    let cap = schars.len().saturating_mul(2) + 1000;
                    // 1) 모든 매치 결과 수집.
                    let mut results: Vec<Value> = Vec::new();
                    for _ in 0..cap {
                        let result = self.regexp_exec(&re, &s)?;
                        if matches!(result, Value::Null) {
                            break;
                        }
                        let m0v = self.member_get(&result, "0")?;
                        let m0 = self.to_string_value(&m0v)?;
                        results.push(result);
                        if !global {
                            break;
                        }
                        if m0.is_empty() {
                            let li = to_num(&self.member_get(&re, "lastIndex")?);
                            self.set_throw(&re, "lastIndex", Value::Num(li + 1.0))?;
                        }
                    }
                    // 2) 결과별로 치환 문자열 조립.
                    let mut accumulated = String::new();
                    let mut next_pos = 0usize;
                    for result in &results {
                        let matched_v = self.member_get(result, "0")?;
                        let matched = self.to_string_value(&matched_v)?;
                        let len_val = self.member_get(result, "length")?;
                        let n_caps = (to_length(self.to_number_value(&len_val)?).max(1.0) - 1.0) as usize;
                        let idx_v = self.member_get(result, "index")?;
                        let idx = self.to_integer_or_infinity(&idx_v)?;
                        let position = (idx.max(0.0).min(schars.len() as f64)) as usize;
                        let mut captures: Vec<Option<String>> = Vec::with_capacity(n_caps);
                        for i in 1..=n_caps {
                            let cv = self.member_get(result, &i.to_string())?;
                            captures.push(if matches!(cv, Value::Undefined) {
                                None
                            } else {
                                Some(self.to_string_value(&cv)?)
                            });
                        }
                        let named = self.member_get(result, "groups")?;
                        let replacement = if functional {
                            let mut cargs = vec![Value::Str(matched.clone())];
                            for c in &captures {
                                cargs.push(c.clone().map(Value::Str).unwrap_or(Value::Undefined));
                            }
                            cargs.push(Value::Num(position as f64));
                            cargs.push(Value::Str(s.clone()));
                            if !matches!(named, Value::Undefined) {
                                cargs.push(named.clone());
                            }
                            let r = self.call_value(repl_val.clone(), None, cargs)?;
                            self.to_string_value(&r)?
                        } else {
                            self.get_substitution(
                                &matched,
                                &schars,
                                position,
                                &captures,
                                &named,
                                repl_str.as_deref().unwrap_or(""),
                            )?
                        };
                        if position >= next_pos {
                            accumulated.extend(&schars[next_pos..position]);
                            accumulated.push_str(&replacement);
                            next_pos = position + matched.chars().count();
                        }
                    }
                    accumulated.extend(&schars[next_pos.min(schars.len())..]);
                    return Ok(Value::Str(accumulated));
                }
                if let natives::StrOp::Split = op {
                    // §22.2.6.14 RegExp.prototype[@@split](string, limit).
                    if !is_object(&re) {
                        return Err(self.throw_error(
                            "TypeError",
                            "RegExp.prototype[Symbol.split] called on non-object",
                        ));
                    }
                    let s = self.to_string_value(&args.first().cloned().unwrap_or(Value::Undefined))?;
                    let flags_val = self.member_get(&re, "flags")?;
                    let flags = self.to_string_value(&flags_val)?;
                    let new_flags = if flags.contains('y') { flags } else { format!("{}y", flags) };
                    // splitter = Construct(SpeciesConstructor(rx,%RegExp%), [rx, newFlags]).
                    let ctor =
                        self.species_constructor(&re, Value::Native(Native::RegExpCtor))?;
                    let splitter = self.construct(ctor, vec![re.clone(), Value::Str(new_flags)])?;
                    let lim = match args.get(1) {
                        None | Some(Value::Undefined) => u32::MAX as usize,
                        Some(v) => self.to_int32(v)? as u32 as usize,
                    };
                    let schars: Vec<char> = s.chars().collect();
                    let size = schars.len();
                    let mut out: Vec<Value> = Vec::new();
                    if lim == 0 {
                        return Ok(Value::Arr(ArrayObj::new(out)));
                    }
                    if size == 0 {
                        let z = self.regexp_exec(&splitter, &s)?;
                        if matches!(z, Value::Null) {
                            out.push(Value::Str(s.clone()));
                        }
                        return Ok(Value::Arr(ArrayObj::new(out)));
                    }
                    let mut p = 0usize;
                    let mut q = 0usize;
                    let guard_cap = size.saturating_mul(2) + 1000;
                    let mut guard = 0;
                    while q < size {
                        guard += 1;
                        if guard > guard_cap {
                            break;
                        }
                        self.set_throw(&splitter, "lastIndex", Value::Num(q as f64))?;
                        let z = self.regexp_exec(&splitter, &s)?;
                        if matches!(z, Value::Null) {
                            q += 1;
                            continue;
                        }
                        let li_val = self.member_get(&splitter, "lastIndex")?;
                        let e = (to_length(to_num(&li_val)) as usize).min(size);
                        if e == p {
                            q += 1;
                            continue;
                        }
                        out.push(Value::Str(schars[p..q].iter().collect()));
                        if out.len() >= lim {
                            return Ok(Value::Arr(ArrayObj::new(out)));
                        }
                        let zlen_val = self.member_get(&z, "length")?;
                        let n_caps = (to_length(self.to_number_value(&zlen_val)?).max(1.0) - 1.0) as usize;
                        for i in 1..=n_caps {
                            let cv = self.member_get(&z, &i.to_string())?;
                            out.push(cv);
                            if out.len() >= lim {
                                return Ok(Value::Arr(ArrayObj::new(out)));
                            }
                        }
                        p = e;
                        q = p;
                    }
                    out.push(Value::Str(schars[p..].iter().collect()));
                    return Ok(Value::Arr(ArrayObj::new(out)));
                }
                if let natives::StrOp::MatchAll = op {
                    // §22.2.6.9 RegExp.prototype[@@matchAll](string). 지연 iterator 대신
                    // 매치를 모아 실제 이터레이터로 감싼다(전체순회/.next() 지원).
                    if !is_object(&re) {
                        return Err(self.throw_error(
                            "TypeError",
                            "RegExp.prototype[Symbol.matchAll] called on non-object",
                        ));
                    }
                    let s = self.to_string_value(&args.first().cloned().unwrap_or(Value::Undefined))?;
                    let flags_val = self.member_get(&re, "flags")?;
                    let flags = self.to_string_value(&flags_val)?;
                    let global = flags.contains('g');
                    // matcher = Construct(SpeciesConstructor(rx,%RegExp%), [rx, flags]).
                    let ctor =
                        self.species_constructor(&re, Value::Native(Native::RegExpCtor))?;
                    let matcher = self.construct(ctor, vec![re.clone(), Value::Str(flags)])?;
                    let li_val = self.member_get(&re, "lastIndex")?;
                    let li = to_length(to_num(&li_val));
                    self.set_throw(&matcher, "lastIndex", Value::Num(li))?;
                    let mut out: Vec<Value> = Vec::new();
                    let cap = s.chars().count().saturating_mul(2) + 1000;
                    for _ in 0..cap {
                        let result = self.regexp_exec(&matcher, &s)?;
                        if matches!(result, Value::Null) {
                            break;
                        }
                        out.push(result.clone());
                        if !global {
                            break;
                        }
                        let m0v = self.member_get(&result, "0")?;
                        let m0 = self.to_string_value(&m0v)?;
                        if m0.is_empty() {
                            let liv = self.member_get(&matcher, "lastIndex")?;
                            self.set_throw(&matcher, "lastIndex", Value::Num(to_num(&liv) + 1.0))?;
                        }
                    }
                    return Ok(self.make_iter_from_vec(out));
                }
                // 모든 @@메서드 처리됨 — 여기 도달하면 String 측 위임(안전망).
                let s = args.first().map(to_display).unwrap_or_default();
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
                // §22.2.7.2: sticky('y')/global 은 lastIndex 를 Get/Set 으로 다룬다 —
                // lastIndex=ToLength(Get(R,"lastIndex")), 갱신은 Set(Throw=true)로 setter 관측/
                // non-writable TypeError. 예전엔 맵 직접 접근이라 getter/setter/ToLength 를 우회했다.
                let sticky = flags.contains('y');
                let use_li = re.global || sticky;
                let r = recv_obj.clone().unwrap_or(Value::Undefined);
                let from = if use_li {
                    let liv = self.member_get(&r, "lastIndex")?;
                    to_length(self.to_number_value(&liv)?) as usize
                } else {
                    0
                };
                // lastIndex > length 면 매치 실패 → lastIndex 0, null.
                if use_li && from > chars.len() {
                    self.set_throw(&r, "lastIndex", Value::Num(0.0))?;
                    return Ok(Value::Null);
                }
                // sticky 는 from 에서 시작하는 매치만 (엔진에 y 미지원 → 여기서 앵커 검사).
                let mat = re.find(&chars, from).filter(|mt| !sticky || mt.start == from);
                match mat {
                    Some(mt) => {
                        if use_li {
                            self.set_throw(&r, "lastIndex", Value::Num(mt.end as f64))?;
                        }
                        Ok(self.regex_match_array(&chars, &mt, &re.group_names))
                    }
                    None => {
                        if use_li {
                            self.set_throw(&r, "lastIndex", Value::Num(0.0))?;
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
                        // §22.1.3.17: searchValue 가 Object 면 @@replace 로 위임(override 존중).
                        if is_object(&pat) {
                            let replacer = self.member_get(&pat, "\u{0}@@replace")?;
                            if !matches!(replacer, Value::Undefined | Value::Null) {
                                if !is_callable(&replacer) {
                                    return Err(self.throw_error(
                                        "TypeError",
                                        "Symbol.replace method is not callable",
                                    ));
                                }
                                return self.call_value(
                                    replacer,
                                    Some(pat),
                                    vec![Value::Str(s.clone()), repl],
                                );
                            }
                        }
                        Value::Str(self.str_replace(&s, &pat, &repl, false)?)
                    }
                    StrOp::ReplaceAll => {
                        let pat = args.first().cloned().unwrap_or(Value::Undefined);
                        let repl = args.get(1).cloned().unwrap_or(Value::Undefined);
                        // §22.1.3.18: searchValue 가 Object 면 IsRegExp 시 flags 'g' 검사 후
                        // @@replace 로 위임.
                        if is_object(&pat) {
                            if self.is_regexp_p(&pat)? {
                                let flags = self.member_get(&pat, "flags")?;
                                if matches!(flags, Value::Undefined | Value::Null) {
                                    return Err(self.throw_error(
                                        "TypeError",
                                        "RegExp flags is undefined or null",
                                    ));
                                }
                                let fs = self.to_string_value(&flags)?;
                                if !fs.contains('g') {
                                    return Err(self.throw_error(
                                        "TypeError",
                                        "replaceAll must be called with a global RegExp",
                                    ));
                                }
                            }
                            let replacer = self.member_get(&pat, "\u{0}@@replace")?;
                            if !matches!(replacer, Value::Undefined | Value::Null) {
                                if !is_callable(&replacer) {
                                    return Err(self.throw_error(
                                        "TypeError",
                                        "Symbol.replace method is not callable",
                                    ));
                                }
                                return self.call_value(
                                    replacer,
                                    Some(pat),
                                    vec![Value::Str(s.clone()), repl],
                                );
                            }
                        }
                        Value::Str(self.str_replace(&s, &pat, &repl, true)?)
                    }
                    StrOp::Search => {
                        // §22.1.3.12: regexp[@@search] 로 위임(GetMethod+Call, override 존중),
                        // 없으면 RegExpCreate(arg) 후 @@search.
                        let arg = args.first().cloned().unwrap_or(Value::Undefined);
                        return self.str_regex_delegate(&s, arg, "\u{0}@@search");
                    }
                    StrOp::Match => {
                        // §22.1.3.11: regexp[@@match] 로 위임(GetMethod+Call, override 존중),
                        // 없으면 RegExpCreate(arg) 후 @@match.
                        let arg = args.first().cloned().unwrap_or(Value::Undefined);
                        return self.str_regex_delegate(&s, arg, "\u{0}@@match");
                    }
                    StrOp::MatchAll => {
                        // §22.1.3.14 String.prototype.matchAll(regexp): regexp[@@matchAll] 로
                        // 위임한다(GetMethod+Call) — 사용자 override 를 존중하고, IsRegExp 면
                        // flags 를 Get 해 'g' 를 검사한다(하드코딩 RegexSym 직행이 아니라).
                        let arg = args.first().cloned().unwrap_or(Value::Undefined);
                        // 최신 스펙: regexp 가 **Object** 일 때만 @@matchAll·flags 를 접근한다
                        // (원시값은 심볼 메서드/flags 를 건드리지 않고 RegExpCreate 로).
                        if is_object(&arg) {
                            // 2.b IsRegExp 면 flags(Get, RequireObjectCoercible) 가 'g' 포함해야.
                            if self.is_regexp_p(&arg)? {
                                let flags = self.member_get(&arg, "flags")?;
                                if matches!(flags, Value::Undefined | Value::Null) {
                                    return Err(self.throw_error(
                                        "TypeError",
                                        "RegExp flags is undefined or null",
                                    ));
                                }
                                let flags_str = self.to_string_value(&flags)?;
                                if !flags_str.contains('g') {
                                    return Err(self.throw_error(
                                        "TypeError",
                                        "String.prototype.matchAll called with a non-global RegExp argument",
                                    ));
                                }
                            }
                            // 2.c-d GetMethod(regexp, @@matchAll): 있으면 호출해 그 결과 반환.
                            let matcher = self.member_get(&arg, "\u{0}@@matchAll")?;
                            if !matches!(matcher, Value::Undefined | Value::Null) {
                                if !is_callable(&matcher) {
                                    return Err(self.throw_error(
                                        "TypeError",
                                        "Symbol.matchAll method is not callable",
                                    ));
                                }
                                return self.call_value(matcher, Some(arg), vec![Value::Str(s.clone())]);
                            }
                        }
                        // 3-5: RegExpCreate(regexp,"g") 후 그 @@matchAll 로 Invoke.
                        // undefined 는 빈 패턴, null 포함 그 외는 ToString.
                        let pat = match &arg {
                            Value::Undefined => String::new(),
                            v => match regex_src_flags(v) {
                                Some((src, _)) => src,
                                None => self.to_string_value(v)?,
                            },
                        };
                        let rx = make_regex_obj(&pat, "g");
                        let matcher = self.member_get(&rx, "\u{0}@@matchAll")?;
                        return self.call_value(matcher, Some(rx), vec![Value::Str(s.clone())]);
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
                        // §22.1.3.20: separator 가 (정규식 아닌) Object 면 @@split 로 위임한다
                        // (GetMethod+Call, 사용자 override 존중, 인자는 [O, limit]). 정규식은
                        // 아래 기존 경로(RegexSym 동등)로 처리한다.
                        let sep_val = args.first().cloned().unwrap_or(Value::Undefined);
                        if is_object(&sep_val) && regex_src_flags(&sep_val).is_none() {
                            let splitter = self.member_get(&sep_val, "\u{0}@@split")?;
                            if !matches!(splitter, Value::Undefined | Value::Null) {
                                if !is_callable(&splitter) {
                                    return Err(self.throw_error(
                                        "TypeError",
                                        "Symbol.split method is not callable",
                                    ));
                                }
                                let limit = args.get(1).cloned().unwrap_or(Value::Undefined);
                                return self.call_value(
                                    splitter,
                                    Some(sep_val),
                                    vec![Value::Str(s.clone()), limit],
                                );
                            }
                        }
                        // §22.1.3.21: lim=ToUint32(limit)(undefined→2^32-1)를 separator ToString
                        // '전에' 구한다. lim==0 → [], separator undefined → [S].
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
                // §22.1.3.14 indexOf / §23.1.3.13 includes: 배열/generic array-like 수신자는
                // 재료화 이전에 매 인덱스 live([[Get]])로 검색한다 — 게터 중 삭제·length 축소·
                // 상속 인덱스·비캐시 값을 관측하고 huge length 도 지연 처리한다. indexOf 는
                // HasProperty 로 구멍 스킵+strict, includes 는 모든 인덱스 방문+SameValueZero.
                // (lastIndexOf 는 프렐류드 폴리필.) 문자열/원시 래퍼는 아래 스냅샷 arm.
                if matches!(
                    &recv,
                    Some(Value::Arr(_)) | Some(Value::Obj(_)) | Some(Value::Instance(_))
                ) {
                    if matches!(op, ArrOp::IndexOf) {
                        let o = recv.clone().unwrap();
                        return self.array_index_search_live(false, &o, &args);
                    }
                    if matches!(op, ArrOp::Includes) {
                        let o = recv.clone().unwrap();
                        return self.array_includes_live(&o, &args);
                    }
                }
                // §23.1.3.26 reverse: generic array-like(Obj)는 HasProperty/Get/Set/Delete
                // 스왑으로 구멍을 보존한다 — Vec 재료화는 구멍을 undefined 로 잃는다(A2/A3
                // 테스트). 밀집 배열/문자열은 아래 Vec 경로. 초거대 length 도 Vec 경로가
                // RangeError 로 방어.
                if matches!(op, ArrOp::Reverse) {
                    if let Some(Value::Obj(_)) = &recv {
                        let o = recv.clone().unwrap();
                        let len_v = self.member_get(&o, "length")?;
                        let len = to_length(self.to_number_value(&len_v)?);
                        if len <= MAX_ARRAY_LEN {
                            let mut lower = 0u64;
                            let middle = (len / 2.0) as u64;
                            let last = len as u64;
                            while lower < middle {
                                let upper = last - lower - 1;
                                let (lk, uk) = (lower.to_string(), upper.to_string());
                                let le = self.has_property(&o, &lk);
                                let lv = if le { self.member_get(&o, &lk)? } else { Value::Undefined };
                                let ue = self.has_property(&o, &uk);
                                let uv = if ue { self.member_get(&o, &uk)? } else { Value::Undefined };
                                // 존재 여부에 따라 Set/Delete (구멍은 Delete 로 보존).
                                match (le, ue) {
                                    (true, true) => {
                                        self.set_own_property(&o, lk, uv);
                                        self.set_own_property(&o, uk, lv);
                                    }
                                    (false, true) => {
                                        self.set_own_property(&o, lk, uv);
                                        self.delete_own(&o, &uk)?;
                                    }
                                    (true, false) => {
                                        self.delete_own(&o, &lk)?;
                                        self.set_own_property(&o, uk, lv);
                                    }
                                    (false, false) => {}
                                }
                                lower += 1;
                            }
                            return Ok(o);
                        }
                    }
                }
                // §23.1.3.30 sort: generic array-like(Obj)는 SortIndexedProperties(존재
                // 인덱스만 [[Get]] 로 수집)+정렬+[[Set]]/[[Delete]] 되쓰기로 접근자·상속
                // 원소·되쓰기 setter 를 정확히 관측한다. 밀집 배열/문자열은 아래 Vec 경로.
                if matches!(op, ArrOp::Sort) {
                    // 구멍이나 인덱스 접근자를 가진 배열도 정밀 경로로 — SortIndexedProperties
                    // 가 [[Get]]/[[Set]]/[[Delete]] 로 접근자·구멍을 정확히 처리한다. 순수
                    // 밀집 배열은 아래 빠른 Vec 정렬(무회귀).
                    let complex_arr = matches!(&recv, Some(Value::Arr(a))
                        if a.has_holes()
                            || a.borrow().iter().any(|v| matches!(v, Value::Accessor(_))));
                    if matches!(&recv, Some(Value::Obj(_))) || complex_arr {
                        let o = recv.clone().unwrap();
                        let cmp_arg = args.first().cloned().unwrap_or(Value::Undefined);
                        if !matches!(cmp_arg, Value::Undefined) && !is_callable(&cmp_arg) {
                            return Err(self.throw_error(
                                "TypeError",
                                "The comparison function must be either a function or undefined",
                            ));
                        }
                        return self.array_sort_generic(&o, &cmp_arg);
                    }
                }
                // §23.1.3 반복 메서드: 배열/generic array-like 수신자는 재료화 이전에
                // 매 인덱스 live 로 순회한다 — 콜백·게터 중 변형 관측 + 재료화 getter
                // 이중호출 제거. 문자열/원시 래퍼 수신자는 아래 기존 경로(불변이라 동일).
                if matches!(
                    op,
                    ArrOp::Reduce
                        | ArrOp::ReduceRight
                        | ArrOp::Some
                        | ArrOp::Every
                        | ArrOp::Find
                        | ArrOp::FindIndex
                        | ArrOp::FindLast
                        | ArrOp::FindLastIndex
                        | ArrOp::ForEach
                        | ArrOp::Map
                        | ArrOp::Filter
                        | ArrOp::FlatMap
                ) {
                    if matches!(
                        &recv,
                        Some(Value::Arr(_)) | Some(Value::Obj(_)) | Some(Value::Instance(_))
                    ) {
                        let o = recv.clone().unwrap();
                        return self.array_iter_live(op, &o, &args);
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
                    // §23.1.3.36 Array.prototype.toString: func=Get(this,"join"); callable 이면
                    // 그걸 호출(사용자 오버라이드 존중), 아니면 %Object.prototype.toString%.
                    // 예전엔 toString 을 Join 으로 하드코딩해 join 오버라이드/비호출을 무시했다.
                    ArrOp::ArrToString => {
                        let join = self.member_get(&cb_arr, "join")?;
                        if is_callable(&join) {
                            return self.call_value(join, Some(cb_arr), vec![]);
                        }
                        return self.call_native(Native::ObjToString, Some(cb_arr), vec![]);
                    }
                    ArrOp::Join => {
                        // §23.1.3.18: 구분자 undefined 는 ",", 아니면 ToString(구분자) —
                        // 원소도 ToString. 예전엔 lenient to_display 라 valueOf/toString·@@
                        // toPrimitive·예외를 무시했다. 구분자 강제변환이 먼저(스펙 순서).
                        let sep = match args.first() {
                            None | Some(Value::Undefined) => ",".to_string(),
                            Some(v) => self.to_string_value(v)?,
                        };
                        let items: Vec<Value> = a.borrow().clone();
                        let mut parts = Vec::with_capacity(items.len());
                        for v in items {
                            parts.push(match v {
                                Value::Undefined | Value::Null => String::new(),
                                other => self.to_string_value(&other)?,
                            });
                        }
                        Value::Str(parts.join(&sep))
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
                        // slice 결과도 ArraySpeciesCreate(§23.1.3.25). 기본 배열이면 빠른 경로.
                        match self.array_species_ctor(&cb_arr)? {
                            Some(ctor) => {
                                let sp = self.construct(ctor, vec![Value::Num(out.len() as f64)])?;
                                for (k, v) in out.into_iter().enumerate() {
                                    self.create_data_property_or_throw(&sp, k, v)?;
                                }
                                sp
                            }
                            None => Value::Arr(ArrayObj::with_holes(out, holes)),
                        }
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
                            // map/filter/flatMap 결과는 ArraySpeciesCreate 로 만든다(§23.1.3).
                            // 기본 배열이면 빠른 경로, 서브클래스/커스텀 @@species 면 그걸로.
                            _ => match self.array_species_ctor(&cb_arr)? {
                                Some(ctor) => {
                                    let a =
                                        self.construct(ctor, vec![Value::Num(out.len() as f64)])?;
                                    for (k, v) in out.into_iter().enumerate() {
                                        self.create_data_property_or_throw(&a, k, v)?;
                                    }
                                    a
                                }
                                None => Value::Arr(if out_holes.is_empty() {
                                    ArrayObj::new(out)
                                } else {
                                    ArrayObj::with_holes(out, out_holes)
                                }),
                            },
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
                        // 배열/generic array-like 수신자는 위 pre-dispatch 의 array_iter_live
                        // 로 처리된다. 여기 도달하는 건 문자열/원시 래퍼 수신자(불변).
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
                        // 배열/generic array-like 수신자는 위 pre-dispatch 의 array_iter_live
                        // 로 처리된다. 여기 도달하는 건 문자열/원시 래퍼 수신자(불변).
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
                        // §23.1.3.1: 스프레드 대상은 k 마다 HasProperty+Get 으로 읽는다 —
                        // 상속 인덱스(Array.prototype[k])도 읽고, 없는 자리는 결과에서도 구멍.
                        // 예전엔 raw items 를 그대로 복사해 상속/구멍 의미를 놓쳤다.
                        let mut out: Vec<Value> = Vec::new();
                        let mut out_holes = std::collections::HashSet::new();
                        for item in all {
                            if self.is_concat_spreadable(&item)? {
                                let len_f = match &item {
                                    Value::Arr(b) => b.borrow().len() as f64,
                                    _ => {
                                        let lv = self.member_get(&item, "length")?;
                                        to_length(self.to_number_value(&lv)?)
                                    }
                                };
                                // §23.1.3.1 step iii: n + len > 2^53-1 → TypeError. 초거대 length
                                // 를 그대로 펼치면 Vec 이 OOM(패닉)하므로 스펙대로 먼저 던진다.
                                if out.len() as f64 + len_f > 9007199254740991.0 {
                                    return Err(self.throw_error(
                                        "TypeError",
                                        "Array length exceeds the maximum safe integer",
                                    ));
                                }
                                let len = len_f as usize;
                                for k in 0..len {
                                    let key = k.to_string();
                                    if self.has_property(&item, &key) {
                                        out.push(self.member_get(&item, &key)?);
                                    } else {
                                        // 없는 인덱스(구멍) → 결과에서도 구멍(§CreateDataProperty 미호출).
                                        out_holes.insert(out.len());
                                        out.push(Value::Undefined);
                                    }
                                }
                            } else {
                                out.push(item);
                            }
                        }
                        // concat 결과도 ArraySpeciesCreate(§23.1.3.1). 기본 배열이면 빠른 경로.
                        match self.array_species_ctor(&cb_arr)? {
                            Some(ctor) => {
                                let sp = self.construct(ctor, vec![Value::Num(0.0)])?;
                                for (k, v) in out.into_iter().enumerate() {
                                    if !out_holes.contains(&k) {
                                        self.create_data_property_or_throw(&sp, k, v)?;
                                    }
                                }
                                sp
                            }
                            None => Value::Arr(ArrayObj::with_holes(out, out_holes)),
                        }
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
                        // §23.1.3.31 step 24: 끝에 Set(O,"length",…). 길이가 바뀌는데 배열
                        // length 가 non-writable 이면 그 Set 이 실패해 TypeError(밀집 배열).
                        let insert_count = args.len().saturating_sub(2) as f64;
                        if (len0 - del_f + insert_count) != len0
                            && matches!(&recv, Some(Value::Arr(_)))
                            && !a.length_writable()
                        {
                            return Err(self.redefine_err());
                        }
                        let removed: Vec<Value> = {
                            let mut arr = a.borrow_mut();
                            let start = (start as usize).min(arr.len());
                            let del = (del_f as usize).min(arr.len() - start);
                            arr.splice(start..start + del, args.iter().skip(2).cloned()).collect()
                        };
                        // splice 의 제거 배열도 ArraySpeciesCreate(§23.1.3.29). 기본은 빠른 경로.
                        match self.array_species_ctor(&cb_arr)? {
                            Some(ctor) => {
                                let sp =
                                    self.construct(ctor, vec![Value::Num(removed.len() as f64)])?;
                                for (k, v) in removed.into_iter().enumerate() {
                                    self.create_data_property_or_throw(&sp, k, v)?;
                                }
                                sp
                            }
                            None => Value::Arr(ArrayObj::new(removed)),
                        }
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
                        // §23.1.3.26 은 O(=ToObject(this))를 돌려준다 — 임시 복사 a 가 아니다.
                        // 밀집 배열이면 cb_arr 이 곧 그 배열, 원시 래퍼면 그 래퍼 객체.
                        cb_arr.clone()
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
                        // §23.1.3.11: depth = ToIntegerOrInfinity(depth)(기본 1, Symbol→TypeError,
                        // Infinity 면 완전 평탄화). 예전엔 lenient to_num 이라 Symbol depth 를 놓쳤다.
                        let depth_f = match args.first() {
                            None | Some(Value::Undefined) => 1.0,
                            Some(v) => self.to_integer_or_infinity(v)?,
                        };
                        let depth = if depth_f.is_nan() || depth_f <= 0.0 {
                            0
                        } else if depth_f > i32::MAX as f64 {
                            i32::MAX
                        } else {
                            depth_f as i32
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
                        // 결과도 ArraySpeciesCreate + CreateDataPropertyOrThrow(§23.1.3.11).
                        match self.array_species_ctor(&cb_arr)? {
                            Some(ctor) => {
                                let sp = self.construct(ctor, vec![Value::Num(0.0)])?;
                                for (k, v) in out.into_iter().enumerate() {
                                    self.create_data_property_or_throw(&sp, k, v)?;
                                }
                                sp
                            }
                            None => Value::Arr(ArrayObj::new(out)),
                        }
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
                        // §23.1.3.6 은 O(=ToObject(this))를 돌려준다 — 임시 복사 a 가 아니다.
                        // 밀집 배열이면 cb_arr 이 곧 그 배열, generic/원시 래퍼면 수신 객체.
                        cb_arr.clone()
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
                // §19.2.5: ToString(arg) 후 앞쪽 정수 프리픽스. radix 는 ToInt32(§7.1.6)로
                // 강제변환(Infinity→0, mod 2^32) — 예전엔 to_display/to_num 이라 valueOf/
                // toString 오버라이드와 큰 radix(mod)·Infinity 를 놓쳤다. 순서: 문자열 먼저.
                let arg = args.first().cloned().unwrap_or(Value::Undefined);
                let s = self.to_string_value(&arg)?;
                let radix_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
                let mut radix = self.to_int32(&radix_arg)? as i64;
                let t = s.trim_start();
                let (neg, mut body) = match t.strip_prefix('-') {
                    Some(rest) => (true, rest),
                    None => (false, t.strip_prefix('+').unwrap_or(t)),
                };
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
                // §19.2.4: ToString(arg) 후 앞쪽 StrDecimalLiteral 프리픽스를 파싱한다 —
                // 부호/Infinity/지수(e±d)/선행·후행 소수점 모두 지원. 예전엔 부호+digits+dot
                // 만 봐서 "Infinity"/"1e1"/".22e-1"/"11.e-1" 이 전부 틀렸다.
                let arg = args.first().cloned().unwrap_or(Value::Undefined);
                let s = self.to_string_value(&arg)?;
                Ok(Value::Num(parse_float_prefix(s.trim_start())))
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
                // §19.2.3 isNaN(number): ToNumber(number) 후 NaN 판정. ToNumber 는 객체의
                // valueOf/@@toPrimitive 를 호출하고 예외를 전파하며 Symbol/BigInt 는 TypeError.
                // 예전엔 lenient to_num 이라 예외를 삼키고 @@toPrimitive 를 무시했다.
                let v = args.first().cloned().unwrap_or(Value::Undefined);
                Ok(Value::Bool(self.to_number_value(&v)?.is_nan()))
            }
            // 전역 isFinite(number) (§19.2.5): ToNumber 후 유한 판정 — Number.isFinite 와 달리
            // 강제변환한다. 예전엔 Number.isFinite(NumIsFinite)와 같은 native 라 강제변환을
            // 안 해 isFinite("42")===false 였다.
            Native::GlobalIsFinite => {
                let v = args.first().cloned().unwrap_or(Value::Undefined);
                Ok(Value::Bool(self.to_number_value(&v)?.is_finite()))
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
                // Reflect.has(t, k) = k in t = t.[[HasProperty]](k). Proxy 는 has 트랩,
                // 그 외는 프로토타입 체인까지 — In 연산자 경로로 통일(예전엔 Proxy 를
                // false 로 떨궈 has 트랩을 아예 안 불렀다).
                self.binary(BinOp::In, Value::Str(key), target)
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
                // newTarget(3번째 인자, 기본=target)을 construct 에 넘긴다 —
                // construct 초입이 self.new_target 을 캡처해 인스턴스 프로토타입과
                // new.target·Proxy construct 트랩의 3번째 인자에 반영한다.
                let new_target = args.get(2).cloned().unwrap_or_else(|| f.clone());
                self.new_target = Some(new_target);
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
                let arg = args.first().cloned().unwrap_or(Value::Undefined);
                if !is_object(&arg) {
                    return Err(self.throw_error("TypeError", "Reflect.preventExtensions called on non-object"));
                }
                // [[PreventExtensions]] 의 불리언을 그대로 돌려준다(Object 와 달리 throw
                // 안 함). Proxy 면 트랩 결과가 그대로 전파된다.
                Ok(Value::Bool(self.value_prevent_extensions(&arg)?))
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
                // Proxy: [[OwnPropertyKeys]] 트랩(검증 포함).
                if let Some(Value::Proxy(p)) = args.first() {
                    let p = p.clone();
                    return Ok(Value::Arr(ArrayObj::new(self.proxy_own_keys(&p)?)));
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
                    RefCell::new(ObjMap::new()),
                )));
                let reject = Value::Bound(Rc::new((
                    Value::Native(Native::PromiseSettleReject),
                    p.clone(),
                    vec![],
                    RefCell::new(ObjMap::new()),
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
                // Proxy: [[OwnPropertyKeys]] 트랩(검증 포함) 후 EnumerableOwnPropertyNames —
                // 문자열 키 중 프록시의 gOPD 가 enumerable 인 것만(§20.1.2.17). 예전엔 트랩
                // 결과를 열거성 필터·심볼 제외 없이 그대로 돌려줬다.
                Some(Value::Proxy(p)) => {
                    let p = p.clone();
                    let all = self.proxy_own_keys(&p)?;
                    let mut out: Vec<Value> = Vec::new();
                    for k in all {
                        if let Value::Str(s) = &k {
                            let desc = self.call_native(
                                Native::ObjectGetOwnPropertyDescriptor,
                                None,
                                vec![Value::Proxy(p.clone()), Value::Str(s.clone())],
                            )?;
                            let enumerable = matches!(&desc, Value::Obj(m)
                                if matches!(m.borrow().get("enumerable"), Some(v) if to_bool(v)));
                            if enumerable {
                                out.push(k);
                            }
                        }
                    }
                    Ok(Value::Arr(ArrayObj::new(out)))
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
                                // 삭제된(툼스톤) 정적 프로퍼티는 own 키에서 제외한다.
                                out.extend(
                                    keys.into_iter()
                                        .filter(|k| !self.native_prop_deleted(v, k))
                                        .map(Value::Str),
                                );
                            }
                            // 사용자가 defineProperty 로 얹은 프로퍼티도 own 으로.
                            if let Some(m) = self.native_props.get(n) {
                                for k in m.keys() {
                                    if !is_internal_key(k)
                                        && !matches!(k.as_str(), "length" | "name")
                                    {
                                        out.push(Value::Str(k.clone()));
                                    }
                                }
                            }
                        }
                        out
                    }
                    // Proxy: ownKeys 트랩 결과 중 문자열 키만 (§10.5.11 + 필터).
                    Some(Value::Proxy(p)) => {
                        let p = p.clone();
                        self.proxy_own_keys(&p)?
                            .into_iter()
                            .filter(|k| matches!(k, Value::Str(_)))
                            .collect()
                    }
                    // 함수도 ordinary object — own 키(정수 인덱스 + length/name/prototype 계산
                    // 프로퍼티 + 사용자 props, §10.1.11 순서)를 가진다. 예전엔 arm 이 없어
                    // getOwnPropertyNames/Reflect.ownKeys(fn) 이 빈 배열이라 Object.assign 소스가
                    // 함수일 때 own prop 을 놓쳤다.
                    Some(Value::Fn(f)) => {
                        let props = f.props.borrow();
                        let deleted = |k: &str| props.contains_key(&format!("\u{0}fndel:{}", k));
                        let has_proto =
                            f.is_generator || (!f.is_arrow && !f.is_method && !f.is_async);
                        let mut ints: Vec<usize> = props
                            .keys()
                            .filter(|k| !is_internal_key(k))
                            .filter_map(|k| k.parse::<usize>().ok())
                            .collect();
                        ints.sort_unstable();
                        ints.dedup();
                        let mut seen: std::collections::HashSet<String> =
                            ints.iter().map(|i| i.to_string()).collect();
                        let mut out: Vec<Value> =
                            ints.iter().map(|i| Value::Str(i.to_string())).collect();
                        // 계산 프로퍼티 length/name/prototype (툼스톤/비생성자 제외), props 에
                        // 이미 있으면(defineProperty 실체화) 중복 방지.
                        for (k, ok) in [
                            ("length", !deleted("length")),
                            ("name", !deleted("name")),
                            ("prototype", has_proto && !deleted("prototype")),
                        ] {
                            if ok && seen.insert(k.to_string()) {
                                out.push(Value::Str(k.to_string()));
                            }
                        }
                        // 사용자 문자열 props (삽입순, 정수/내부 제외).
                        for k in props.keys() {
                            if is_internal_key(k) || k.parse::<usize>().is_ok() {
                                continue;
                            }
                            if seen.insert(k.clone()) {
                                out.push(Value::Str(k.clone()));
                            }
                        }
                        out
                    }
                    _ => Vec::new(),
                };
                Ok(Value::Arr(ArrayObj::new(names)))
            }
            Native::ObjectValues => {
                // §20.1.2.23: O = ToObject(arg)(null/undefined → TypeError), 그다음 live
                // EnumerableOwnProperties(value). getter·proxy 트랩·live enumerable 관측.
                let arg = args.first().cloned().unwrap_or(Value::Undefined);
                if matches!(arg, Value::Undefined | Value::Null) {
                    return Err(self.throw_error(
                        "TypeError",
                        "Cannot convert undefined or null to object",
                    ));
                }
                let obj = self.to_object_value(arg);
                let vals = self.enumerable_own_live(&obj, false, true)?;
                Ok(Value::Arr(ArrayObj::new(vals)))
            }
            Native::ObjectEntries => {
                // §20.1.2.6: O = ToObject(arg), 그다음 live EnumerableOwnProperties(key+value).
                let arg = args.first().cloned().unwrap_or(Value::Undefined);
                if matches!(arg, Value::Undefined | Value::Null) {
                    return Err(self.throw_error(
                        "TypeError",
                        "Cannot convert undefined or null to object",
                    ));
                }
                let obj = self.to_object_value(arg);
                let entries = self.enumerable_own_live(&obj, true, true)?;
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
                // §20.1.2.1 step 1: To = ToObject(target). 원시값 타깃은 래퍼 객체로 박싱한다 —
                // 예전엔 원시값에 직접 set 을 시도해 "read only" 로 죽었고, 인자가 하나면
                // typeof 결과가 "object" 가 아니라 원시 타입이었다.
                let target = if is_object(&target) {
                    target
                } else {
                    self.to_object_value(target)
                };
                // §20.1.2.1 step 3: 각 소스는 ToObject 후 [[OwnPropertyKeys]](정수·문자열·심볼
                // 순서) → 키마다 live [[GetOwnProperty]](enumerable) → [[Get]](getter 호출) →
                // Set(To, key, v, Throw=true). Proxy 는 ownKeys/gOPD/get 트랩을 순서대로 거치고
                // 그 예외(및 read-only/getter-only Set 실패)를 전파한다. 예전엔 맵을 직접 훑어
                // 심볼·문자열 순서가 뒤섞이고 프록시 트랩·live 서술자를 놓쳤다.
                for src in args[1..].to_vec() {
                    if matches!(src, Value::Undefined | Value::Null) {
                        continue;
                    }
                    let from = if is_object(&src) {
                        src
                    } else {
                        self.to_object_value(src)
                    };
                    let keys = self.call_native(Native::ReflectOwnKeys, None, vec![from.clone()])?;
                    let keys: Vec<Value> = match keys {
                        Value::Arr(a) => a.borrow().clone(),
                        _ => Vec::new(),
                    };
                    for k in keys {
                        let desc = self.call_native(
                            Native::ObjectGetOwnPropertyDescriptor,
                            None,
                            vec![from.clone(), k.clone()],
                        )?;
                        let enumerable = matches!(&desc, Value::Obj(m)
                            if matches!(m.borrow().get("enumerable"), Some(v) if to_bool(v)));
                        if !enumerable {
                            continue;
                        }
                        let key_str = self.to_property_key(k)?;
                        let v = self.member_get(&from, &key_str)?;
                        if !self.ordinary_set(&target, &key_str, v, &target)? {
                            return Err(self.throw_error(
                                "TypeError",
                                format!(
                                    "Cannot assign to read only property '{}' of object",
                                    key_str
                                ),
                            ));
                        }
                    }
                }
                Ok(target)
            }
            Native::ArrayIsArray => {
                // §23.1.2.2 = IsArray: Proxy-of-배열도 true, revoked proxy 는 TypeError.
                let v = args.first().cloned().unwrap_or(Value::Undefined);
                Ok(Value::Bool(self.is_array(&v)?))
            }
            Native::ArrayOf => {
                // §23.1.2.3: C = this. IsConstructor(C) 면 Construct(C, [len]) 로 만들고
                // 각 항목을 CreateDataPropertyOrThrow, 마지막에 Set(length). 아니면 일반 배열.
                // 예전엔 this 를 무시하고 항상 일반 배열이라 Array.of.call(Ctor,…)이 깨졌다.
                let this = recv.clone().unwrap_or(Value::Undefined);
                if self.is_constructor(&this) && !matches!(this, Value::Native(Native::ArrayCtor)) {
                    let len = args.len();
                    let a = self.construct(this, vec![Value::Num(len as f64)])?;
                    // §23.1.2.3 step 8.c: CreateDataPropertyOrThrow — [[Set]] 이 아니라
                    // [[DefineOwnProperty]]. 대상 프로토타입의 setter/인덱스 접근자를 타지
                    // 않고 own 데이터 프로퍼티를 만들며, 실패(거부)는 TypeError 로 전파한다.
                    for (k, item) in args.into_iter().enumerate() {
                        self.create_data_property_or_throw(&a, k, item)?;
                    }
                    self.ordinary_set(&a, "length", Value::Num(len as f64), &a)?;
                    Ok(a)
                } else {
                    Ok(Value::Arr(ArrayObj::new(args)))
                }
            }
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
                // mapFn 의 thisArg (3번째 인자). 예전엔 무시했다.
                let this_arg = args.get(2).cloned().unwrap_or(Value::Undefined);
                // null/undefined 는 GetMethod(items,@@iterator)에서 TypeError (§7.3.11).
                if matches!(src, Value::Undefined | Value::Null) {
                    return Err(self.throw_error(
                        "TypeError",
                        "Array.from: items is undefined or null",
                    ));
                }
                // 유한 fast path(배열/문자열/Set/Map/제너레이터/재료화 반복자)는 그대로 수집하고,
                // 일반 사용자 이터러블은 반복자를 직접 스텝하며 mapFn 을 즉시 적용한다 —
                // mapFn 이 abrupt 면 IteratorClose(return 호출) 후 전파(§23.1.2.1 step vii.2).
                // 예전엔 전량 수집 후 매핑이라, 무한 이터레이터 + throwing mapFn 이 스텝 상한까지
                // 돌았다(첫 항목에서 멈춰야 함).
                let mut iterable_path = true;
                let user_iter = match &src {
                    Value::Arr(_)
                    | Value::Str(_)
                    | Value::SetVal(_)
                    | Value::MapVal(_)
                    | Value::Gen(_) => None,
                    Value::Obj(o)
                        if o.borrow().contains_key("\u{0}items") || o.borrow().contains_key("next") =>
                    {
                        None
                    }
                    _ => self.try_get_iterator(&src)?,
                };
                let out: Vec<Value> = if let Some(it) = user_iter {
                    // 일반 사용자 이터러블: 스텝별 next → (선택) mapFn → 수집.
                    let mut r = Vec::new();
                    let mut k = 0usize;
                    loop {
                        let (val, done) = self.gen_iter_next(&it, Value::Undefined)?;
                        if done {
                            break;
                        }
                        let mapped = match &map_fn {
                            Some(f) => {
                                match self.call_value(
                                    f.clone(),
                                    Some(this_arg.clone()),
                                    vec![val, Value::Num(k as f64)],
                                ) {
                                    Ok(m) => m,
                                    // mapFn 예외 → IteratorClose 후 전파.
                                    Err(e) => return Err(self.iterator_close_throw(&it, e)),
                                }
                            }
                            None => val,
                        };
                        r.push(mapped);
                        k += 1;
                        self.tick()?;
                    }
                    r
                } else {
                    // 유한 fast path 또는 array-like: 수집 후 매핑.
                    let items = match &src {
                        Value::Obj(o)
                            if !(o.borrow().contains_key("\u{0}items")
                                || o.borrow().contains_key("next")) =>
                        {
                            // array-like: generic_array_read 로 length 강제변환(valueOf) + [[Get]].
                            iterable_path = false;
                            self.generic_array_read(&src)?
                        }
                        Value::Arr(_)
                        | Value::Str(_)
                        | Value::SetVal(_)
                        | Value::MapVal(_)
                        | Value::Gen(_)
                        | Value::Obj(_) => self.iterate_to_vec(&src)?,
                        _ => {
                            iterable_path = false;
                            Vec::new()
                        }
                    };
                    match &map_fn {
                        Some(f) => {
                            let mut r = Vec::with_capacity(items.len());
                            for (i, v) in items.into_iter().enumerate() {
                                r.push(self.call_value(
                                    f.clone(),
                                    Some(this_arg.clone()),
                                    vec![v, Value::Num(i as f64)],
                                )?);
                            }
                            r
                        }
                        None => items,
                    }
                };
                // §23.1.2.1: C = this. IsConstructor(C) 면 그걸로 만든다 — 이터러블 경로는
                // Construct(C)(길이 미지), array-like 는 Construct(C,[len]). 각 요소는
                // CreateDataProperty, 마지막에 Set(length). 예전엔 this 를 무시했다.
                let this = recv.clone().unwrap_or(Value::Undefined);
                if self.is_constructor(&this) && !matches!(this, Value::Native(Native::ArrayCtor)) {
                    let len = out.len();
                    let ctor_args = if iterable_path {
                        vec![]
                    } else {
                        vec![Value::Num(len as f64)]
                    };
                    let a = self.construct(this, ctor_args)?;
                    // §23.1.2.1 step: CreateDataPropertyOrThrow — [[Set]] 이 아니라
                    // [[DefineOwnProperty]]. 대상의 setter/non-writable 인덱스에 막히지 않고
                    // own 데이터 프로퍼티를 만들며, 거부는 TypeError 로 전파한다.
                    for (k, item) in out.into_iter().enumerate() {
                        self.create_data_property_or_throw(&a, k, item)?;
                    }
                    self.ordinary_set(&a, "length", Value::Num(len as f64), &a)?;
                    Ok(a)
                } else {
                    Ok(Value::Arr(ArrayObj::new(out)))
                }
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
            // executor 의 reject(e): this(=promise)를 거부 상태로.
            // reject_promise(→settle)로 정착해야 이미 등록된 대기 반응(.then 핸들러)이
            // 마이크로태스크로 스케줄된다. 예전엔 상태 맵만 직접 바꿔, 핸들러가 먼저
            // 붙은 뒤(pending 상태에서 .then) 늦게 reject 되면 그 핸들러가 영영 안 돌았다
            // — 조합자/비동기 거부 전파가 통째로 깨졌다. resolve 쪽과 대칭.
            Native::PromiseSettleReject => {
                let v = args.into_iter().next().unwrap_or(Value::Undefined);
                if let Some(p) = recv {
                    let pending = matches!(&p, Value::Obj(o)
                        if matches!(o.borrow().get("\u{0}state"), Some(Value::Str(s)) if s == "pending"));
                    if pending {
                        self.reject_promise(&p, v);
                    }
                }
                Ok(Value::Undefined)
            }
            Native::PromiseResolve => {
                let v = args.into_iter().next().unwrap_or(Value::Undefined);
                // §27.2.4.7: v 가 이미 프로미스이고 그 constructor 가 C(this)면 그대로
                // 반환(새 프로미스로 감싸지 않는다). Promise.resolve(p) === p (동일 생성자).
                // 예전엔 항상 새 프로미스로 감싸 조합자의 then 재정의/동일성 검사가 어긋났다.
                if is_promise(&v) {
                    let c = recv.unwrap_or_else(|| {
                        env_get(&self.global, "Promise").unwrap_or(Value::Undefined)
                    });
                    // C 가 %Promise% 네이티브면 네이티브 프로미스를 그대로 반환. 프로미스
                    // 객체에 constructor 슬롯이 없어 Get(v,"constructor")===C 대신 C 동일성으로
                    // 근사(흔한 Promise.resolve(promise) 경로). 그 외엔 constructor 비교.
                    let matches = matches!(&c, Value::Native(Native::PromiseCtor)) || {
                        let ctor = self.member_get(&v, "constructor").unwrap_or(Value::Undefined);
                        strict_eq(&ctor, &c)
                    };
                    if matches {
                        return Ok(v);
                    }
                }
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
