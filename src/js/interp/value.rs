// Value 헬퍼: 표시/형변환/동등성/JSON. interp/mod.rs 에서 분리.
use super::*;
use std::cell::RefCell;
use std::rc::Rc;

// ECMAScript Number::toString (7.1.12.1). Rust 의 최단 유효숫자 표현({:e})을 분해해
// JS 의 소수/지수 표기 규칙(지수는 n>21 또는 n≤-6 에서, 형식 "de+X")을 적용한다.
pub(super) fn num_to_str(n: f64) -> String {
    if n.is_nan() {
        return "NaN".to_string();
    }
    if n == 0.0 {
        return "0".to_string(); // +0/-0 → "0"
    }
    if n.is_infinite() {
        return if n > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    let neg = n < 0.0;
    // 최단 유효숫자 + 십진 지수로 분해: "m e exp" (예: "1.2345e2", "1e21", "1e-7")
    let sci = format!("{:e}", n.abs());
    let (mant, exp_str) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exp: i32 = exp_str.parse().unwrap_or(0);
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32; // 유효숫자 개수
    let np = exp + 1; // 소수점 위치(첫 유효숫자의 자리 = 10^(np-1))
    let body = if np >= k && np <= 21 {
        // 정수: 숫자 + (np-k)개의 0
        format!("{}{}", digits, "0".repeat((np - k) as usize))
    } else if np > 0 && np <= 21 {
        // 소수점이 숫자들 사이
        format!("{}.{}", &digits[..np as usize], &digits[np as usize..])
    } else if np > -6 && np <= 0 {
        // 0.00…digits
        format!("0.{}{}", "0".repeat((-np) as usize), digits)
    } else {
        // 지수 표기: d[.rest]e{+|-}(np-1)
        let e = np - 1;
        let mantissa = if k == 1 {
            digits.clone()
        } else {
            format!("{}.{}", &digits[..1], &digits[1..])
        };
        let sign = if e >= 0 { "+" } else { "-" };
        format!("{}e{}{}", mantissa, sign, e.abs())
    };
    if neg {
        format!("-{}", body)
    } else {
        body
    }
}

// Number.prototype.toExponential (§21.1.3.2). digits=None 이면 최소 유효숫자.
// Rust 의 "d.ddde±X" 를 JS 의 "d.ddde+X"(부호 명시)로 변환한다.
pub(super) fn num_to_exponential(n: f64, digits: Option<usize>) -> String {
    if n == 0.0 {
        return match digits {
            Some(d) if d > 0 => format!("0.{}e+0", "0".repeat(d)),
            _ => "0e+0".to_string(),
        };
    }
    let neg = n < 0.0;
    let ax = n.abs();
    let s = match digits {
        Some(d) => format!("{:.*e}", d, ax),
        None => format!("{:e}", ax),
    };
    let (mant, exp) = s.split_once('e').unwrap_or((s.as_str(), "0"));
    let exp_i: i64 = exp.parse().unwrap_or(0);
    let sign = if exp_i >= 0 { "+" } else { "-" };
    format!("{}{}e{}{}", if neg { "-" } else { "" }, mant, sign, exp_i.abs())
}

// Number.prototype.toPrecision (§21.1.3.5). p 개의 유효숫자.
pub(super) fn num_to_precision(n: f64, p: usize) -> String {
    if n == 0.0 {
        return if p == 1 {
            "0".to_string()
        } else {
            format!("0.{}", "0".repeat(p - 1))
        };
    }
    let neg = n < 0.0;
    let ax = n.abs();
    // 지수 e 를 Rust 지수표기로 신뢰성 있게 구한다 (반올림 반영).
    let e: i64 = format!("{:.*e}", p - 1, ax)
        .split_once('e')
        .and_then(|(_, x)| x.parse().ok())
        .unwrap_or(0);
    let body = if e < -6 || e >= p as i64 {
        // 지수 표기, 유효숫자 p (소수부 p-1)
        let s = format!("{:.*e}", p - 1, ax);
        let (mant, exp) = s.split_once('e').unwrap_or((s.as_str(), "0"));
        let exp_i: i64 = exp.parse().unwrap_or(0);
        let sign = if exp_i >= 0 { "+" } else { "-" };
        format!("{}e{}{}", mant, sign, exp_i.abs())
    } else {
        // 고정 표기, 소수부 = p-1-e (음수면 0)
        let frac = (p as i64 - 1 - e).max(0) as usize;
        format!("{:.*}", frac, ax)
    };
    if neg {
        format!("-{}", body)
    } else {
        body
    }
}

pub(super) fn to_bool(v: &Value) -> bool {
    match v {
        Value::Undefined | Value::Null => false,
        Value::Bool(b) => *b,
        Value::Num(n) => *n != 0.0 && !n.is_nan(),
        Value::BigInt(b) => !b.is_zero(),
        Value::Str(s) => !s.is_empty(),
        _ => true,
    }
}

// JS ToInt32: 2^32 모듈로 후 부호 있는 32비트로 (비트 연산 의미론)
pub(super) fn to_i32_from_num(n: f64) -> i32 {
    if !n.is_finite() {
        return 0;
    }
    (n.trunc().rem_euclid(4294967296.0)) as u32 as i32
}
pub(super) fn to_i32(v: &Value) -> i32 {
    to_i32_from_num(to_num(v))
}

pub(super) fn to_num(v: &Value) -> f64 {
    match v {
        Value::Undefined => f64::NAN,
        Value::Null => 0.0,
        Value::Bool(b) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        Value::Num(n) => *n,
        Value::BigInt(b) => b.to_f64(),
        Value::Str(s) => str_to_num(s),
        // 원시 래퍼(new Number 등)는 내부 슬롯을 강제 변환
        Value::Obj(_) => wrapper_primitive(v).map(|p| to_num(&p)).unwrap_or(f64::NAN),
        _ => f64::NAN,
    }
}

// 표준 StringToNumber (§7.1.4.1). Rust 의 f64 파서와 달리:
//  - 0x/0b/0o 진법 접두를 받는다(부호 불가)
//  - "Infinity" 만 무한대 (Rust 는 "inf"/"nan" 도 받지만 JS 는 NaN)
//  - 빈/공백 문자열은 0
pub(super) fn str_to_num(s: &str) -> f64 {
    let t = s.trim();
    if t.is_empty() {
        return 0.0;
    }
    // 진법 접두 (부호 없이만)
    if let Some(rest) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return u128::from_str_radix(rest, 16).map(|v| v as f64).unwrap_or(f64::NAN);
    }
    if let Some(rest) = t.strip_prefix("0o").or_else(|| t.strip_prefix("0O")) {
        return u128::from_str_radix(rest, 8).map(|v| v as f64).unwrap_or(f64::NAN);
    }
    if let Some(rest) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        return u128::from_str_radix(rest, 2).map(|v| v as f64).unwrap_or(f64::NAN);
    }
    // Infinity (정확한 표기만)
    match t {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    // 십진수: 허용 문자만(Rust 의 "inf"/"nan"/"infinity" 오탐 차단)
    if !t.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-')) {
        return f64::NAN;
    }
    t.parse::<f64>().unwrap_or(f64::NAN)
}

// 진단용: 멤버 접근 대상 식을 짧은 소스 문자열로 (에러 메시지에 사용)
pub(super) fn obj_hint(e: &crate::js::ast::Expr) -> String {
    use crate::js::ast::Expr;
    match e {
        Expr::Ident(n) => n.clone(),
        Expr::Member { obj, prop, computed: false } => {
            if let Expr::Str(p) = &**prop {
                format!("{}.{}", obj_hint(obj), p)
            } else {
                format!("{}.?", obj_hint(obj))
            }
        }
        Expr::Member { obj, computed: true, .. } => format!("{}[..]", obj_hint(obj)),
        Expr::Call { callee, .. } => format!("{}()", obj_hint(callee)),
        Expr::This => "this".to_string(),
        // 진단은 이름이 전부다 — "?" 만 찍히면 어디를 볼지 알 수 없다.
        Expr::OptMember { obj, prop, computed: false } => match &**prop {
            Expr::Str(p) => format!("{}?.{}", obj_hint(obj), p),
            _ => format!("{}?.…", obj_hint(obj)),
        },
        Expr::OptMember { obj, .. } => format!("{}?.[..]", obj_hint(obj)),
        Expr::OptCall { callee, .. } => format!("{}?.()", obj_hint(callee)),
        Expr::New { callee, .. } => format!("new {}", obj_hint(callee)),
        Expr::Assign { target, .. } => format!("({}=…)", obj_hint(target)),
        Expr::Func { name: Some(n), .. } => format!("function {}", n),
        Expr::Func { .. } => "function".to_string(),
        Expr::Str(v) => format!("\"{}\"", v),
        Expr::Num(n) => n.to_string(),
        Expr::Ternary { .. } => "(삼항식)".to_string(),
        Expr::Logical { left, right, .. } => {
            format!("({} || {})", obj_hint(left), obj_hint(right))
        }
        Expr::Nullish { left, right } => format!("({} ?? {})", obj_hint(left), obj_hint(right)),
        _ => "?".to_string(),
    }
}

// DOM 트리에서 태그명으로 첫 요소 찾기 (document.body/head 등)
pub(super) fn find_tag(
    dom: &crate::dom::Dom,
    id: crate::dom::NodeId,
    tag: &str,
) -> Option<crate::dom::NodeId> {
    if let crate::dom::NodeType::Element(e) = &dom.get(id).node_type {
        if e.tag_name == tag {
            return Some(id);
        }
    }
    for &c in &dom.get(id).children {
        if let Some(r) = find_tag(dom, c, tag) {
            return Some(r);
        }
    }
    None
}

pub(super) fn is_callable(v: &Value) -> bool {
    match v {
        Value::Fn(_) | Value::Native(_) | Value::Class(_) | Value::Bound(_) => true,
        // Proxy 는 타깃이 callable 일 때만 [[Call]] 을 갖는다(§10.5.12). 함수 프록시가
        // callable 로 인식돼야 p() 호출·apply 트랩·Function.prototype.call 이 동작한다.
        Value::Proxy(p) => is_callable(&p.0),
        _ => false,
    }
}

// 객체 수신자(XHR 등)의 리스너 보관 키. NUL 접두라 JS 코드가 만들 수 없고
// Object.keys/JSON 에도 새지 않는다 (is_internal_key).
pub(super) fn obj_listener_key(event: &str) -> String {
    format!("\u{0}evt:{}", event)
}

// 객체인가 (원시값이 아닌가). 표준의 Type(x) == Object.
// 생성자의 반환값 판정과 ToPrimitive 가 쓴다. Proxy/Map/Set/Date/함수도 객체다 —
// 예전엔 Obj/Instance/Arr 만 객체로 봐서, 생성자가 Proxy 를 반환하면 조용히 버려졌다
// (타입드 배열처럼 Proxy 로 인덱스를 가로채는 구현이 통째로 무력화된다).
pub(super) fn is_object(v: &Value) -> bool {
    matches!(
        v,
        Value::Obj(_)
            | Value::Instance(_)
            | Value::Arr(_)
            | Value::Proxy(_)
            | Value::MapVal(_)
            | Value::SetVal(_)
            | Value::Fn(_)
            | Value::Class(_)
            | Value::Bound(_)
            | Value::Gen(_)
            // 내장 함수/생성자(Array/String/…)도 JS 에서는 객체다. 빠뜨리면
            // Object.defineProperty(Array, …) 같은 호출이 "non-object" 로 잘못 던진다.
            | Value::Native(_)
            // DOM 요소도 JS 에서는 객체다. 빠뜨리면 생성자가 요소를 반환할 때
            // 조용히 버려진다 — 커스텀 엘리먼트의 this 가 진짜 DOM 노드가 되지 못한다.
            | Value::Dom(_)
            | Value::ComputedStyle(_)
    )
}

// 두 값이 같은 함수인가 (참조 동일). removeEventListener 가 등록된 리스너를 찾을 때 쓴다.
// 표준도 참조 동일로 지운다 — bind() 로 새로 만든 함수는 안 지워지는 게 맞다.
pub(super) fn same_callable(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Fn(x), Value::Fn(y)) => Rc::ptr_eq(x, y),
        (Value::Native(x), Value::Native(y)) => x == y,
        (Value::Bound(x), Value::Bound(y)) => Rc::ptr_eq(x, y),
        (Value::Class(x), Value::Class(y)) => Rc::ptr_eq(x, y),
        // handleEvent 객체 리스너
        (Value::Obj(x), Value::Obj(y)) => Rc::ptr_eq(x, y),
        _ => false,
    }
}

// Obj 기반 Promise 판별 (__isPromise 마커)
pub(super) fn is_promise(v: &Value) -> bool {
    matches!(v, Value::Obj(o) if matches!(o.borrow().get("\u{0}isPromise"), Some(Value::Bool(true))))
}

// element.style.backgroundColor → "background-color" (선두 대문자는 벤더 프리픽스: -webkit-)
pub(super) fn camel_to_kebab(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for (i, c) in s.char_indices() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('-');
            } else {
                out.push('-'); // WebkitX → -webkit-x
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

// inline style 속성 문자열 "a: b; c: d" → 순서 보존 (키, 값) 쌍
pub(super) fn style_pairs(attr: &str) -> Vec<(String, String)> {
    attr.split(';')
        .filter_map(|decl| {
            let decl = decl.trim();
            if decl.is_empty() {
                return None;
            }
            let (k, v) = decl.split_once(':')?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

pub(super) fn style_serialize(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}: {}", k, v))
        .collect::<Vec<_>>()
        .join("; ")
}

// 치환 문자열의 $& $1 $$ $` $' 확장 (정규식 replace)
pub(super) fn expand_replacement(
    templ: &str,
    chars: &[char],
    mt: &crate::js::regex::Match,
) -> String {
    let full: String = chars[mt.start..mt.end].iter().collect();
    let t: Vec<char> = templ.chars().collect();
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
                    out.push_str(&full);
                    i += 2;
                }
                '`' => {
                    out.push_str(&chars[..mt.start].iter().collect::<String>());
                    i += 2;
                }
                '\'' => {
                    out.push_str(&chars[mt.end..].iter().collect::<String>());
                    i += 2;
                }
                d if d.is_ascii_digit() => {
                    // §22.1.3.19: 두 자리 그룹번호를 우선 시도하고, 유효하지 않으면 한 자리로
                    // 폴백한다. 예: 그룹이 1개뿐이면 $11 → $1 + 리터럴 "1", $10 → $1 + "0".
                    let two = if i + 2 < t.len() && t[i + 2].is_ascii_digit() {
                        let n = (d as usize - '0' as usize) * 10 + (t[i + 2] as usize - '0' as usize);
                        if n >= 1 && n < mt.groups.len() { Some((n, 3usize)) } else { None }
                    } else {
                        None
                    };
                    let pick = two.or_else(|| {
                        let n = d as usize - '0' as usize;
                        if n >= 1 && n < mt.groups.len() { Some((n, 2usize)) } else { None }
                    });
                    match pick {
                        Some((gi, adv)) => {
                            if let Some((a, b)) = mt.groups[gi] {
                                out.push_str(&chars[a..b].iter().collect::<String>());
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
    out
}

// getElementsByClassName/TagName: id 서브트리에서 매칭 요소 수집.
// skip_self=true 면 스코프 요소 자신은 제외(자손만).
pub(super) fn collect_elements(
    dom: &crate::dom::Dom,
    id: crate::dom::NodeId,
    skip_self: bool,
    query: &str,
    by_class: bool,
    out: &mut Vec<Value>,
) {
    if !skip_self {
        if let crate::dom::NodeType::Element(e) = &dom.get(id).node_type {
            let hit = if by_class {
                // 인자도 ASCII 공백으로 쪼갠다: 여러 클래스를 **모두** 가진 요소만 (표준).
                // 빈 인자는 아무것도 매치하지 않는다.
                let want: Vec<&str> = crate::dom::split_ascii_ws(query).collect();
                let have: std::collections::HashSet<&str> = e
                    .attributes
                    .get("class")
                    .map(|c| crate::dom::split_ascii_ws(c).collect())
                    .unwrap_or_default();
                !want.is_empty() && want.iter().all(|w| have.contains(w))
            } else {
                // getElementsByTagName (§4.5): HTML 문서에서
                //  - HTML 네임스페이스 요소는 **소문자화한** 이름과 비교한다
                //  - 그 외(SVG/MathML)는 **그대로** 비교한다 (대소문자 구분)
                // 예전엔 무조건 eq_ignore_ascii_case 라서 SVG 의 <ABC> 와 <abc> 를
                // 구분하지 못했다.
                if query == "*" {
                    true
                } else if e.namespace.is_none() {
                    e.tag_name == query.to_ascii_lowercase()
                } else {
                    e.tag_name == query
                }
            };
            if hit {
                out.push(Value::Dom(id));
            }
        }
    }
    for &c in &dom.get(id).children {
        collect_elements(dom, c, false, query, by_class, out);
    }
}

// epoch millis → (year, month[1-12], day, hours, min, sec, ms, weekday[0=일])
// UTC 기준 (타임존 미구현). Howard Hinnant 의 civil_from_days 알고리즘.
pub(super) fn date_parts(millis: f64) -> (i64, u32, u32, u32, u32, u32, u32, u32) {
    let ms_total = millis as i64;
    let days = ms_total.div_euclid(86_400_000);
    let ms_of_day = ms_total.rem_euclid(86_400_000);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as i64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = if m <= 2 { y + 1 } else { y };
    let secs = ms_of_day / 1000;
    let hours = (secs / 3600) as u32;
    let min = ((secs % 3600) / 60) as u32;
    let sec = (secs % 60) as u32;
    let ms = (ms_of_day % 1000) as u32;
    // 1970-01-01 = 목요일(4). weekday = (days + 4) mod 7
    let weekday = (days.rem_euclid(7) + 4).rem_euclid(7) as u32;
    (year, m, d, hours, min, sec, ms, weekday)
}

// days_from_civil: (year, month[1-12], day) → 1970-01-01 기준 일수. (Howard Hinnant)
pub(super) fn days_from_civil(y: i64, mo: i64, d: i64) -> i64 {
    let y = if mo <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if mo > 2 { mo - 3 } else { mo + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

// (year, month[1-12], day, h, m, s, ms) → epoch millis (UTC)
pub(super) fn date_to_millis(y: i64, mo: i64, d: i64, h: i64, mi: i64, s: i64, ms: i64) -> f64 {
    let days = days_from_civil(y, mo, d);
    (days * 86_400_000 + h * 3_600_000 + mi * 60_000 + s * 1000 + ms) as f64
}

// §21.4.1.11 MakeTime — 하나라도 유한하지 않으면 NaN, 나머진 trunc 후 합산.
pub(super) fn make_time(h: f64, m: f64, s: f64, ms: f64) -> f64 {
    if !(h.is_finite() && m.is_finite() && s.is_finite() && ms.is_finite()) {
        return f64::NAN;
    }
    h.trunc() * 3_600_000.0 + m.trunc() * 60_000.0 + s.trunc() * 1000.0 + ms.trunc()
}

// §21.4.1.12 MakeDay(year, month[0기준], date) → 1970-01-01 기준 일수(f64, NaN 가능).
pub(super) fn make_day(year: f64, month: f64, date: f64) -> f64 {
    if !(year.is_finite() && month.is_finite() && date.is_finite()) {
        return f64::NAN;
    }
    let y = year.trunc();
    let m = month.trunc();
    let dt = date.trunc();
    let ym = y + (m / 12.0).floor();
    // 표현 가능한 날짜 범위(±약 27만년) 밖이면 어차피 TimeClip 에서 NaN → 조기 NaN.
    if !ym.is_finite() || ym.abs() > 300_000.0 {
        return f64::NAN;
    }
    let mn = m.rem_euclid(12.0); // 0..11
    let days = days_from_civil(ym as i64, mn as i64 + 1, 1) as f64;
    days + (dt - 1.0)
}

// §21.4.1.13 MakeDate(day, time) = day*86400000 + time (비유한이면 NaN).
pub(super) fn make_date_ms(day: f64, time: f64) -> f64 {
    if !day.is_finite() || !time.is_finite() {
        return f64::NAN;
    }
    let tv = day * 86_400_000.0 + time;
    if tv.is_finite() {
        tv
    } else {
        f64::NAN
    }
}

// MakeFullYear: 구조분해 생성자/UTC 의 연도 0..99 → 1900+ 보정 (§21.4.2.1 / §21.4.3.4).
pub(super) fn make_full_year(y: f64) -> f64 {
    if y.is_nan() {
        return f64::NAN;
    }
    let yi = y.trunc();
    if (0.0..=99.0).contains(&yi) {
        1900.0 + yi
    } else {
        yi
    }
}

// (year, month0, date, h, m, s, ms) 성분(이미 ToNumber 된 f64) → TimeClip 된 시간값.
// MakeFullYear 적용판(생성자/Date.UTC).
pub(super) fn build_date_full(y: f64, mo: f64, d: f64, h: f64, mi: f64, s: f64, ms: f64) -> f64 {
    let day = make_day(make_full_year(y), mo, d);
    time_clip(make_date_ms(day, make_time(h, mi, s, ms)))
}

// 현재 epoch millis
pub(super) fn now_millis() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as f64)
        .unwrap_or(0.0)
}

// ISO 8601 근사 파싱: "2026-07-11" / "2026-07-11T10:30:00[.mmm][Z]"
pub(super) fn parse_date_string(s: &str) -> Option<f64> {
    let s = s.trim();
    let (date, time) = match s.split_once(['T', ' ']) {
        Some((d, t)) => (d, Some(t)),
        None => (s, None),
    };
    // 확장 연도(ISO 8601): 앞의 +/-YYYYYY. '-' 로 split 하기 전에 부호를 떼어낸다.
    let (ysign, dbody) = match date.strip_prefix('-') {
        Some(r) => (-1i64, r),
        None => (1i64, date.strip_prefix('+').unwrap_or(date)),
    };
    let mut dp = dbody.split('-');
    let y: i64 = ysign * dp.next()?.parse::<i64>().ok()?;
    let mo: i64 = dp.next().and_then(|x| x.parse().ok()).unwrap_or(1);
    let d: i64 = dp.next().and_then(|x| x.parse().ok()).unwrap_or(1);
    let (mut h, mut mi, mut sec, mut ms) = (0i64, 0i64, 0i64, 0i64);
    if let Some(t) = time {
        let t = t.trim_end_matches('Z');
        let (hms, frac) = match t.split_once('.') {
            Some((a, b)) => (a, Some(b)),
            None => (t, None),
        };
        let mut tp = hms.split(':');
        h = tp.next().and_then(|x| x.parse().ok()).unwrap_or(0);
        mi = tp.next().and_then(|x| x.parse().ok()).unwrap_or(0);
        sec = tp.next().and_then(|x| x.parse().ok()).unwrap_or(0);
        if let Some(f) = frac {
            let f: String = f.chars().take(3).collect();
            ms = format!("{:0<3}", f).parse().unwrap_or(0);
        }
    }
    Some(date_to_millis(y, mo, d, h, mi, sec, ms))
}

pub(super) fn make_date(millis: f64) -> Value {
    let mut m = ObjMap::new();
    m.insert("\u{0}isDate".to_string(), Value::Bool(true));
    m.insert("\u{0}time".to_string(), Value::Num(millis));
    Value::Obj(Rc::new(RefCell::new(m)))
}

pub(super) fn is_date_obj(map: &Rc<RefCell<ObjMap>>) -> bool {
    matches!(map.borrow().get("\u{0}isDate"), Some(Value::Bool(true)))
}

// 엔진 내부 마커 키(구현 세부). Date/Promise/정규식/반복자 등의 상태를 프로퍼티 맵에
// 담는데, 이 키들은 열거(Object.keys/for-in/JSON/스프레드)에 노출되면 안 된다.
// 사용자 데이터 키(__typename, __esModule 등)와 겹치지 않는 엔진 전용 이름만.
// 엔진 전용 키는 모두 NUL 접두("\0…")로 산다 — 심볼 키("\0@@…")와 내부 마커("\0state" 등).
// JS 소스가 만들 수 있는 문자열 키는 이 공간에 도달하지 못하므로,
//  - 사용자가 `obj.__isPromise = true` 로 promise 를 위장할 수 없고,
//  - 사용자의 정상 `__items`/`__value` 키가 열거에서 사라지지도 않는다.
// __proto__ 만 예외: JS 표준 이름이고 비열거 접근자라는 의미론이 우리 동작과 일치한다.
pub(super) fn is_internal_key(k: &str) -> bool {
    k.starts_with('\u{0}') || k == "__proto__"
}

// 심볼 키(well-known/등록 심볼 프로퍼티)는 "\0@@…" 로 산다. 내부 마커(\0time 등)와
// 달리 실제 own 프로퍼티다 — getOwnPropertySymbols / hasOwnProperty(sym) /
// getOwnPropertyDescriptor(obj, sym) / obj[sym] 에 노출돼야 한다. 단 문자열 키
// 열거(Object.keys/for-in/JSON/스프레드)에는 여전히 안 잡힌다(심볼이라서).
pub(super) fn is_symbol_key(k: &str) -> bool {
    k.starts_with("\u{0}@@")
}

// 비열거(enumerable: false) 프로퍼티 표식. 내부 키라서 스스로는 열거되지 않는다.
// 예전엔 defineProperty 의 enumerable 을 통째로 무시해서, 숨겨야 할 프로퍼티가
// Object.keys / for-in / JSON 에 그대로 새어 나왔다 (조용히 틀린 출력).
pub(super) fn nonenum_marker(k: &str) -> String {
    format!("\u{0}ne:{}", k)
}

// 내장 객체(Math/JSON/네임스페이스/프로토타입)의 현재 non-internal 프로퍼티를 전부
// non-enumerable 로 표식한다 (§17: 내장 프로퍼티는 열거되지 않는다). 생성 직후,
// 사용자 코드 실행 전에 부른다.
pub(super) fn mark_nonenum_all(m: &mut ObjMap) {
    let keys: Vec<String> =
        m.keys().filter(|k| !is_internal_key(k)).cloned().collect();
    for k in keys {
        m.insert(nonenum_marker(&k), Value::Bool(true));
    }
}

// 프로퍼티 속성 비트 (§6.1.7.1). 데이터 프로퍼티의 writable/enumerable/configurable.
// 기본값(전부 true)이면 마커를 두지 않는다 — 하위 호환이고 흔한 경우라 저장 비용 0.
pub(super) const ATTR_WRITABLE: u8 = 1;
pub(super) const ATTR_ENUMERABLE: u8 = 2;
pub(super) const ATTR_CONFIGURABLE: u8 = 4;
pub(super) const ATTR_DEFAULT: u8 = ATTR_WRITABLE | ATTR_ENUMERABLE | ATTR_CONFIGURABLE;

// 속성 비트를 저장하는 내부 키. Value::Num 으로 비트를 담는다.
pub(super) fn attr_marker(k: &str) -> String {
    format!("\u{0}attr:{}", k)
}

// 이 키의 속성 비트를 읽는다. 마커가 없으면 기본값(전부 true).
// nonenum_marker 도 함께 존중한다 (기존 코드가 심은 비열거 표식과 호환).
pub(super) fn prop_attrs(m: &ObjMap, k: &str) -> u8 {
    if let Some(Value::Num(n)) = m.get(&attr_marker(k)) {
        return *n as u8;
    }
    let mut a = ATTR_DEFAULT;
    if m.contains_key(&nonenum_marker(k)) {
        a &= !ATTR_ENUMERABLE;
    }
    a
}

// 속성 비트를 저장한다. 기본값이면 마커를 지운다 (군더더기 없음).
pub(super) fn set_prop_attrs(m: &mut ObjMap, k: &str, attrs: u8) {
    let am = attr_marker(k);
    let ne = nonenum_marker(k);
    if attrs == ATTR_DEFAULT {
        m.remove(&am);
        m.remove(&ne);
    } else {
        m.insert(am, Value::Num(attrs as f64));
        // 비열거면 기존 nonenum 마커와도 일관되게
        if attrs & ATTR_ENUMERABLE == 0 {
            m.insert(ne, Value::Bool(true));
        } else {
            m.remove(&ne);
        }
    }
}

pub(super) fn enumerable_keys(m: &Rc<RefCell<ObjMap>>) -> Vec<String> {
    let b = m.borrow();
    b.keys()
        .filter(|k| !is_internal_key(k) && !b.contains_key(&nonenum_marker(k)))
        .cloned()
        .collect()
}

pub(super) fn enumerable_entries(m: &Rc<RefCell<ObjMap>>) -> Vec<(String, Value)> {
    let b = m.borrow();
    b.iter()
        .filter(|(k, _)| !is_internal_key(k) && !b.contains_key(&nonenum_marker(k)))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

fn two(n: u32) -> String {
    format!("{:02}", n)
}

const DATE_DOW: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const DATE_MON: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

// toString 계열 연도 표기: 0..9999 는 4자리, 그 외는 부호 포함 그대로.
fn fmt_year(y: i64) -> String {
    // DateString/UTCString 의 연도는 최소 4자리(부호 포함): -1 → "-0001", 999 → "0999",
    // 12345 → "12345" (양수엔 + 안 붙임 — +는 ISO 확장연도 전용).
    if y < 0 {
        format!("-{:04}", -y)
    } else {
        format!("{:04}", y)
    }
}

// TimeClip (§21.4.1.31): ±8.64e15 초과/비유한 → NaN, 아니면 정수로 절단(-0 → +0).
pub(super) fn time_clip(t: f64) -> f64 {
    if !t.is_finite() || t.abs() > 8.64e15 {
        return f64::NAN;
    }
    let r = t.trunc();
    if r == 0.0 {
        0.0
    } else {
        r
    }
}

// Date → ISO 8601 문자열 (§21.4.4.36). 확장 연도(±YYYYYY) 표기 포함.
pub(super) fn date_iso(millis: f64) -> String {
    let (y, mo, d, h, mi, s, ms, _) = date_parts(millis);
    let year = if (0..=9999).contains(&y) {
        format!("{:04}", y)
    } else if y < 0 {
        format!("-{:06}", -y)
    } else {
        format!("+{:06}", y)
    };
    format!(
        "{}-{}-{}T{}:{}:{}.{:03}Z",
        year,
        two(mo),
        two(d),
        two(h),
        two(mi),
        two(s),
        ms
    )
}

// Date.prototype.toString (§21.4.4.41): "Thu Jan 01 1970 00:00:00 GMT+0000 (...)"
pub(super) fn date_tostring(millis: f64) -> String {
    if millis.is_nan() {
        return "Invalid Date".to_string();
    }
    format!("{} {}", date_datestring(millis), date_timestring_body(millis))
}

// Date.prototype.toDateString (§21.4.4.35): "Thu Jan 01 1970"
pub(super) fn date_datestring(millis: f64) -> String {
    if millis.is_nan() {
        return "Invalid Date".to_string();
    }
    let (y, mo, d, _, _, _, _, wd) = date_parts(millis);
    format!(
        "{} {} {:02} {}",
        DATE_DOW[wd as usize % 7],
        DATE_MON[(mo as usize - 1) % 12],
        d,
        fmt_year(y)
    )
}

// Date.prototype.toTimeString (§21.4.4.42): "00:00:00 GMT+0000 (...)"
pub(super) fn date_timestring(millis: f64) -> String {
    if millis.is_nan() {
        return "Invalid Date".to_string();
    }
    date_timestring_body(millis)
}

fn date_timestring_body(millis: f64) -> String {
    let (_, _, _, h, mi, s, _, _) = date_parts(millis);
    format!(
        "{}:{}:{} GMT+0000 (Coordinated Universal Time)",
        two(h),
        two(mi),
        two(s)
    )
}

// Date.prototype.toUTCString (§21.4.4.43): "Thu, 01 Jan 1970 00:00:00 GMT"
pub(super) fn date_utcstring(millis: f64) -> String {
    if millis.is_nan() {
        return "Invalid Date".to_string();
    }
    let (y, mo, d, h, mi, s, _, wd) = date_parts(millis);
    format!(
        "{}, {:02} {} {} {}:{}:{} GMT",
        DATE_DOW[wd as usize % 7],
        d,
        DATE_MON[(mo as usize - 1) % 12],
        fmt_year(y),
        two(h),
        two(mi),
        two(s)
    )
}

// 정규식 리터럴/RegExp → {source, flags, __isRegex, global, lastIndex} 객체
pub(super) fn make_regex_obj(source: &str, flags: &str) -> Value {
    let mut map = ObjMap::new();
    // source/flags 는 내부 매칭(regex_src_flags)이 읽는다. global/ignoreCase/multiline
    // 등 플래그 파생값은 own 데이터로 두지 않는다 — 표준상 RegExp.prototype 의
    // 접근자이며, 인스턴스 접근은 member_get 이 flags 에서 계산한다. lastIndex 는
    // 표준상 쓰기 가능한 데이터 프로퍼티다 (§22.2.6.12).
    // 원시 source/flags 는 내부 키에 둔다 — 공개 source/flags 접근은 member_get 이
    // 접근자로 계산한다(표준: RegExp.prototype 의 getter). 내부 키라 own 열거/조회에
    // 안 잡힌다.
    map.insert("\u{0}source".to_string(), Value::Str(source.to_string()));
    map.insert("\u{0}flags".to_string(), Value::Str(flags.to_string()));
    map.insert("\u{0}isRegex".to_string(), Value::Bool(true));
    map.insert("lastIndex".to_string(), Value::Num(0.0));
    Value::Obj(Rc::new(RefCell::new(map)))
}

// RegExp.escape(S) 의 이스케이프 (§22.2.5.2, ES2025). 결과 문자열은 정규식 안에서
// 원본 S 를 리터럴로 매칭한다.
pub(super) fn regexp_escape(s: &str) -> String {
    // 구문 문자(§22.2.1 SyntaxCharacter) + '/'
    const SYNTAX: &str = "^$\\.*+?()[]{}|/";
    // 그 외 이스케이프해야 하는 구두점 + 공백
    const OTHER_PUNCT: &str = ",-=<>#&!%:;@~'`\" ";
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        // 첫 코드포인트가 영숫자면 \xHH 로 (앞 토큰과 결합 방지)
        if i == 0 && (c.is_ascii_alphanumeric()) {
            out.push_str(&format!("\\x{:02x}", c as u32));
            continue;
        }
        if SYNTAX.contains(c) {
            out.push('\\');
            out.push(c);
        } else if let Some(esc) = match c {
            '\t' => Some("\\t"),
            '\n' => Some("\\n"),
            '\u{0b}' => Some("\\v"),
            '\u{0c}' => Some("\\f"),
            '\r' => Some("\\r"),
            _ => None,
        } {
            out.push_str(esc);
        } else if OTHER_PUNCT.contains(c) || c.is_whitespace() || is_line_terminator(c) {
            let n = c as u32;
            if n <= 0xFF {
                out.push_str(&format!("\\x{:02x}", n));
            } else if n <= 0xFFFF {
                out.push_str(&format!("\\u{:04x}", n));
            } else {
                // 아스트랄: UTF-16 서로게이트 쌍으로
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{:04x}", unit));
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn is_line_terminator(c: char) -> bool {
    matches!(c, '\u{0a}' | '\u{0d}' | '\u{2028}' | '\u{2029}')
}

// 정규식 플래그 검증 (§22.2.3.1). 유효 플래그는 d,g,i,m,s,u,v,y. 중복 금지,
// u 와 v 동시 금지. 위반 시 문제 플래그(또는 조합)를 Some 으로 돌린다.
pub(super) fn invalid_regex_flags(flags: &str) -> Option<String> {
    let mut seen = std::collections::HashSet::new();
    for c in flags.chars() {
        if !matches!(c, 'd' | 'g' | 'i' | 'm' | 's' | 'u' | 'v' | 'y') {
            return Some(c.to_string());
        }
        if !seen.insert(c) {
            return Some(c.to_string()); // 중복
        }
    }
    if seen.contains(&'u') && seen.contains(&'v') {
        return Some("uv".to_string()); // u 와 v 는 상호 배타
    }
    None
}

// 원시 래퍼 객체(new String/Number/Boolean)의 내부 [[PrimitiveValue]] 슬롯을 읽는다.
// 래퍼가 아니면 None. 강제 변환(to_num/to_display/valueOf)이 이걸 참조한다.
pub(super) const WRAPPER_SLOT: &str = "\u{0}primitive";
// class X extends Set/Map {} 인스턴스가 물려받는 내부 슬롯([[SetData]]/[[MapData]]).
// 파생 인스턴스는 Value::Instance 라 SetVal/MapVal 을 직접 담을 수 없어, super() 가
// 부모(내장 생성자)로 만든 exotic 컬렉션을 이 슬롯에 붙이고 메서드가 언랩한다.
pub(super) const COLLECTION_SLOT: &str = "\u{0}collectiondata";
pub(super) fn wrapper_primitive(v: &Value) -> Option<Value> {
    if let Value::Obj(m) = v {
        return m.borrow().get(WRAPPER_SLOT).cloned();
    }
    None
}

// 객체가 정규식인지 (__isRegex == true)
pub(super) fn is_regex_obj(map: &Rc<RefCell<ObjMap>>) -> bool {
    matches!(map.borrow().get("\u{0}isRegex"), Some(Value::Bool(true)))
}

// 정규식 객체에서 (source, flags) 추출
pub(super) fn regex_src_flags(v: &Value) -> Option<(String, String)> {
    if let Value::Obj(m) = v {
        if is_regex_obj(m) {
            let b = m.borrow();
            let s = match b.get("\u{0}source") {
                Some(Value::Str(s)) => s.clone(),
                _ => return None,
            };
            let f = match b.get("\u{0}flags") {
                Some(Value::Str(f)) => f.clone(),
                _ => String::new(),
            };
            return Some((s, f));
        }
    }
    None
}

// 값을 프로퍼티 키 문자열로. 심볼은 고유 key, 그 외는 ToString.
pub(super) fn key_of(v: &Value) -> String {
    match v {
        Value::Symbol(s) => s.key.clone(),
        _ => to_display(v),
    }
}

pub(super) fn to_display(v: &Value) -> String {
    match v {
        Value::Undefined => "undefined".to_string(),
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Num(n) => num_to_str(*n),
        // String(1n) === "1" (n 접미 없음)
        Value::BigInt(b) => b.to_string(),
        Value::Str(s) => s.clone(),
        // 원시 래퍼(new String/Number/Boolean)는 내부 슬롯 문자열화
        Value::Obj(_) => wrapper_primitive(v)
            .map(|p| to_display(&p))
            .unwrap_or_else(|| "[object Object]".to_string()),
        Value::Arr(a) => {
            a.borrow().iter().map(to_display).collect::<Vec<_>>().join(",")
        }
        Value::Fn(_) | Value::Native(_) | Value::Class(_) | Value::Bound(_) => {
            "function".to_string()
        }
        Value::Accessor(_) => "[accessor]".to_string(),
        Value::MapVal(_) => "[object Map]".to_string(),
        Value::SetVal(_) => "[object Set]".to_string(),
        Value::Style(_) => "[object CSSStyleDeclaration]".to_string(),
        Value::Attr(_, _) => "[object Attr]".to_string(),
        Value::Sheet(_) => "[object CSSStyleSheet]".to_string(),
        Value::CssRule(_, _) => "[object CSSStyleRule]".to_string(),
        Value::RuleStyle(_, _) => "[object CSSStyleDeclaration]".to_string(),
        Value::Dataset(_) => "[object DOMStringMap]".to_string(),
        // classList 를 문자열화하면 class 값 (DOMTokenList.toString)
        Value::ClassList(_) => "[object DOMTokenList]".to_string(),
        Value::Dom(_) => "[object Element]".to_string(),
        Value::Instance(i) => format!("[object {}]", i.class.name.borrow()),
        // Proxy 문자열화는 target 에 위임 (트랩 없는 근사)
        Value::Proxy(p) => to_display(&p.0),
        Value::Gen(_) => "[object Generator]".to_string(),
        // String(sym) 은 "Symbol(desc)". (`+ sym` 은 스펙상 throw 지만 관대 처리)
        Value::Symbol(s) => {
            format!("Symbol({})", s.desc.as_deref().unwrap_or(""))
        }
        Value::ComputedStyle(_) => "[object CSSStyleDeclaration]".to_string(),
    }
}

pub(super) fn type_of(v: &Value) -> &'static str {
    match v {
        Value::Undefined => "undefined",
        Value::Null => "object", // JS 의 유명한 typeof null
        Value::Bool(_) => "boolean",
        Value::Num(_) => "number",
        Value::BigInt(_) => "bigint",
        Value::Str(_) => "string",
        Value::Symbol(_) => "symbol",
        // Symbol 생성자만 함수, 다른 Native 도 함수.
        Value::Fn(_) | Value::Native(_) | Value::Class(_) | Value::Bound(_) => "function",
        // 프록시는 타깃이 callable 이면 typeof === "function"(§13.5.3).
        Value::Proxy(p) => {
            if is_callable(&p.0) {
                "function"
            } else {
                "object"
            }
        }
        _ => "object",
    }
}

// structuredClone: 깊은 복제(배열/객체/Map/Set/인스턴스 재귀, 원시값 그대로).
// 함수/클래스/DOM 은 복제 불가 → null(스펙은 throw, 관대 처리). __proto__ 링크는 미복제.
pub(super) fn deep_clone(v: &Value, depth: usize) -> Value {
    if depth > 500 {
        return Value::Null; // 순환/과도한 깊이 방어
    }
    match v {
        Value::Arr(a) => {
            Value::Arr(ArrayObj::new(a.borrow().iter().map(|x| deep_clone(x, depth + 1)).collect()))
        }
        Value::Obj(o) => {
            let mut m = ObjMap::new();
            for (k, val) in o.borrow().iter() {
                if k != "__proto__" {
                    m.insert(k.clone(), deep_clone(val, depth + 1));
                }
            }
            Value::Obj(Rc::new(RefCell::new(m)))
        }
        Value::MapVal(mp) => Value::MapVal(Rc::new(RefCell::new(
            mp.borrow()
                .iter()
                .map(|(k, val)| (deep_clone(k, depth + 1), deep_clone(val, depth + 1)))
                .collect(),
        ))),
        Value::SetVal(s) => Value::SetVal(Rc::new(RefCell::new(
            s.borrow().iter().map(|x| deep_clone(x, depth + 1)).collect(),
        ))),
        Value::Instance(i) => {
            // 인스턴스는 필드만 복제한 일반 객체로(프로토타입 미복제 — 근사).
            let mut m = ObjMap::new();
            for (k, val) in i.fields.borrow().iter() {
                if is_internal_key(k) || i.fields.borrow().contains_key(&nonenum_marker(k)) {
                    continue;
                }
                m.insert(k.clone(), deep_clone(val, depth + 1));
            }
            Value::Obj(Rc::new(RefCell::new(m)))
        }
        Value::Fn(_)
        | Value::Native(_)
        | Value::Class(_)
        | Value::Bound(_)
        | Value::Dom(_) => Value::Null,
        other => other.clone(),
    }
}

// UTF-16 코드 유닛열 hay 에서 ndl 을 from 부터 찾아 첫 유닛 인덱스 반환(String.indexOf).
pub(super) fn utf16_index_of(hay: &[u16], ndl: &[u16], from: usize) -> Option<usize> {
    if ndl.is_empty() {
        return Some(from.min(hay.len()));
    }
    if ndl.len() > hay.len() {
        return None;
    }
    (from.min(hay.len())..=hay.len() - ndl.len()).find(|&i| &hay[i..i + ndl.len()] == ndl)
}

// String.lastIndexOf: 뒤에서부터 마지막 일치의 첫 유닛 인덱스.
pub(super) fn utf16_last_index_of(hay: &[u16], ndl: &[u16]) -> Option<usize> {
    if ndl.is_empty() {
        return Some(hay.len());
    }
    if ndl.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - ndl.len()).rev().find(|&i| &hay[i..i + ndl.len()] == ndl)
}

// SameValueZero: strict_eq 와 같되 NaN 은 서로 같다(Map/Set 키 비교용). +0/-0 은 strict 와 동일.
pub(super) fn same_value_zero(a: &Value, b: &Value) -> bool {
    if let (Value::Num(x), Value::Num(y)) = (a, b) {
        if x.is_nan() && y.is_nan() {
            return true;
        }
    }
    strict_eq(a, b)
}

// SameValue (§7.2.11): NaN 은 자기 자신과 같고, +0 과 -0 은 **다르다**.
// defineProperty 의 값 비교(비writable 프로퍼티 재정의 판정)에 쓴다.
pub(super) fn same_value(a: &Value, b: &Value) -> bool {
    if let (Value::Num(x), Value::Num(y)) = (a, b) {
        if x.is_nan() && y.is_nan() {
            return true;
        }
        if *x == 0.0 && *y == 0.0 {
            return x.is_sign_negative() == y.is_sign_negative();
        }
    }
    strict_eq(a, b)
}

pub(super) fn strict_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Undefined, Value::Undefined) | (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Num(x), Value::Num(y)) => x == y,
        // 1n === 1 은 false (타입이 다르다). 1n === 1n 은 값 비교.
        (Value::BigInt(x), Value::BigInt(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Obj(x), Value::Obj(y)) => Rc::ptr_eq(x, y),
        (Value::Arr(x), Value::Arr(y)) => Rc::ptr_eq(x, y),
        (Value::Fn(x), Value::Fn(y)) => Rc::ptr_eq(x, y),
        // 같은 네이티브(내장) 함수는 동일 (Math.round === Math.round 등)
        (Value::Native(x), Value::Native(y)) => x == y,
        (Value::Dom(x), Value::Dom(y)) => x == y,
        (Value::Class(x), Value::Class(y)) => Rc::ptr_eq(x, y),
        (Value::Instance(x), Value::Instance(y)) => Rc::ptr_eq(x, y),
        (Value::MapVal(x), Value::MapVal(y)) => Rc::ptr_eq(x, y),
        (Value::SetVal(x), Value::SetVal(y)) => Rc::ptr_eq(x, y),
        (Value::Bound(x), Value::Bound(y)) => Rc::ptr_eq(x, y),
        (Value::Style(x), Value::Style(y)) => x == y,
        // CSSOM/Attr 은 (대상, 키) 로 식별되는 살아 있는 뷰다 — 같은 대상이면 같은 것.
        (Value::Attr(a, x), Value::Attr(b, y)) => a == b && x == y,
        (Value::Sheet(x), Value::Sheet(y)) => x == y,
        (Value::CssRule(a, x), Value::CssRule(b, y)) => a == b && x == y,
        (Value::RuleStyle(a, x), Value::RuleStyle(b, y)) => a == b && x == y,
        (Value::Dataset(x), Value::Dataset(y)) => x == y,
        (Value::ClassList(x), Value::ClassList(y)) => x == y,
        // 심볼 동일성은 고유 key 비교 (Symbol('x')!==Symbol('x'), Symbol.for 은 ===).
        (Value::Symbol(x), Value::Symbol(y)) => x.key == y.key,
        // 제너레이터·프록시·접근자도 신원(Rc 포인터)으로 비교한다. 빠뜨리면 항상 false 라
        // it[Symbol.iterator]() === it 같은 표준 불변식이 거짓이 된다 (라이브러리가
        // 이터레이터인지 판정하는 데 이 비교를 쓴다).
        (Value::Gen(x), Value::Gen(y)) => Rc::ptr_eq(x, y),
        (Value::Proxy(x), Value::Proxy(y)) => Rc::ptr_eq(x, y),
        (Value::Accessor(x), Value::Accessor(y)) => Rc::ptr_eq(x, y),
        (Value::ComputedStyle(x), Value::ComputedStyle(y)) => x == y,
        _ => false,
    }
}

pub(super) fn loose_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Undefined | Value::Null, Value::Undefined | Value::Null) => true,
        (Value::Num(_), Value::Num(_))
        | (Value::Str(_), Value::Str(_))
        | (Value::Bool(_), Value::Bool(_)) => strict_eq(a, b),
        (Value::Num(_) | Value::Str(_) | Value::Bool(_), Value::Num(_) | Value::Str(_) | Value::Bool(_)) => {
            to_num(a) == to_num(b)
        }
        _ => strict_eq(a, b),
    }
}

// ── JSON ──────────────────────────────────────────────────────────

// json-parse-with-source (ES2023): reviver 의 3번째 인자 context.source 를 채우려면
// 파싱 시 각 원시 리프의 원본 텍스트 스냅샷이 필요하다. 객체/배열은 자식만 담는다.
pub(super) enum JsonSrc {
    Prim { raw: String, val: Value },
    Arr(Vec<JsonSrc>),
    Obj(Vec<(String, JsonSrc)>),
}

pub(super) fn json_parse(src: &str) -> Result<Value, String> {
    json_parse_snap(src).map(|(v, _)| v)
}

// 값과 함께 소스 스냅샷을 돌려준다 (reviver 경로 전용).
pub(super) fn json_parse_snap(src: &str) -> Result<(Value, JsonSrc), String> {
    let chars: Vec<char> = src.chars().collect();
    let mut pos = 0usize;
    let (v, snap) = json_value(&chars, &mut pos)?;
    json_ws(&chars, &mut pos);
    if pos != chars.len() {
        return Err("JSON: 값 뒤에 잉여 문자".to_string());
    }
    Ok((v, snap))
}

pub(super) fn json_ws(c: &[char], p: &mut usize) {
    // JSON 공백은 U+0020/U+0009/U+000A/U+000D 뿐 (§25.5.1). Rust is_whitespace 는
    // U+00A0/U+1680 등 유니코드 공백까지 포함해 너무 관대했다.
    while *p < c.len() && matches!(c[*p], ' ' | '\t' | '\n' | '\r') {
        *p += 1;
    }
}

pub(super) fn json_lit(c: &[char], p: &mut usize, lit: &str) -> bool {
    if c[*p..].starts_with(&lit.chars().collect::<Vec<_>>()[..]) {
        *p += lit.chars().count();
        true
    } else {
        false
    }
}

pub(super) fn json_value(c: &[char], p: &mut usize) -> Result<(Value, JsonSrc), String> {
    json_ws(c, p);
    match c.get(*p) {
        None => Err("JSON 이 갑자기 끝남".to_string()),
        Some('{') => {
            *p += 1;
            let mut map = ObjMap::new();
            let mut snap: Vec<(String, JsonSrc)> = Vec::new();
            json_ws(c, p);
            if c.get(*p) == Some(&'}') {
                *p += 1;
                return Ok((Value::Obj(Rc::new(RefCell::new(map))), JsonSrc::Obj(snap)));
            }
            loop {
                json_ws(c, p);
                let key = json_string(c, p)?;
                json_ws(c, p);
                if c.get(*p) != Some(&':') {
                    return Err("JSON: ':' 필요".to_string());
                }
                *p += 1;
                let (v, s) = json_value(c, p)?;
                map.insert(key.clone(), v);
                snap.push((key, s));
                json_ws(c, p);
                match c.get(*p) {
                    Some(',') => *p += 1,
                    Some('}') => {
                        *p += 1;
                        return Ok((Value::Obj(Rc::new(RefCell::new(map))), JsonSrc::Obj(snap)));
                    }
                    _ => return Err("JSON: ',' 나 '}' 필요".to_string()),
                }
            }
        }
        Some('[') => {
            *p += 1;
            let mut items = Vec::new();
            let mut snap: Vec<JsonSrc> = Vec::new();
            json_ws(c, p);
            if c.get(*p) == Some(&']') {
                *p += 1;
                return Ok((Value::Arr(ArrayObj::new(items)), JsonSrc::Arr(snap)));
            }
            loop {
                let (v, s) = json_value(c, p)?;
                items.push(v);
                snap.push(s);
                json_ws(c, p);
                match c.get(*p) {
                    Some(',') => *p += 1,
                    Some(']') => {
                        *p += 1;
                        return Ok((Value::Arr(ArrayObj::new(items)), JsonSrc::Arr(snap)));
                    }
                    _ => return Err("JSON: ',' 나 ']' 필요".to_string()),
                }
            }
        }
        Some('"') => {
            let start = *p;
            let v = Value::Str(json_string(c, p)?);
            let raw: String = c[start..*p].iter().collect();
            Ok((v.clone(), JsonSrc::Prim { raw, val: v }))
        }
        Some('t') if json_lit(c, p, "true") => {
            Ok((Value::Bool(true), JsonSrc::Prim { raw: "true".into(), val: Value::Bool(true) }))
        }
        Some('f') if json_lit(c, p, "false") => {
            Ok((Value::Bool(false), JsonSrc::Prim { raw: "false".into(), val: Value::Bool(false) }))
        }
        Some('n') if json_lit(c, p, "null") => {
            Ok((Value::Null, JsonSrc::Prim { raw: "null".into(), val: Value::Null }))
        }
        Some(&ch) if ch == '-' || ch.is_ascii_digit() => {
            // 엄격한 JSON 수 문법 (§25.5.1): [-] int [frac] [exp]. leading zero(01)/
            // 소수점 뒤 숫자 없음(1.)/지수 숫자 없음(1e)/+부호 금지. 예전엔 관대 collect 였다.
            let start = *p;
            let is_digit = |x: Option<&char>| x.map_or(false, |d| d.is_ascii_digit());
            if c.get(*p) == Some(&'-') {
                *p += 1;
            }
            match c.get(*p) {
                Some('0') => *p += 1, // 0 단독(뒤에 숫자 오면 아래 caller 가 거부)
                Some(d) if d.is_ascii_digit() => {
                    while is_digit(c.get(*p)) {
                        *p += 1;
                    }
                }
                _ => return Err("JSON: 잘못된 수".to_string()),
            }
            if c.get(*p) == Some(&'.') {
                *p += 1;
                if !is_digit(c.get(*p)) {
                    return Err("JSON: 소수점 뒤 숫자 필요".to_string());
                }
                while is_digit(c.get(*p)) {
                    *p += 1;
                }
            }
            if matches!(c.get(*p), Some('e') | Some('E')) {
                *p += 1;
                if matches!(c.get(*p), Some('+') | Some('-')) {
                    *p += 1;
                }
                if !is_digit(c.get(*p)) {
                    return Err("JSON: 지수 숫자 필요".to_string());
                }
                while is_digit(c.get(*p)) {
                    *p += 1;
                }
            }
            let s: String = c[start..*p].iter().collect();
            let n = s.parse::<f64>().map_err(|_| format!("JSON: 잘못된 수 {}", s))?;
            Ok((Value::Num(n), JsonSrc::Prim { raw: s, val: Value::Num(n) }))
        }
        Some(other) => Err(format!("JSON: 예상 못한 문자 {:?}", other)),
    }
}

pub(super) fn json_string(c: &[char], p: &mut usize) -> Result<String, String> {
    if c.get(*p) != Some(&'"') {
        return Err("JSON: 문자열 필요".to_string());
    }
    *p += 1;
    let mut s = String::new();
    loop {
        match c.get(*p) {
            None => return Err("JSON: 닫히지 않은 문자열".to_string()),
            Some('"') => {
                *p += 1;
                return Ok(s);
            }
            Some('\\') => {
                *p += 1;
                match c.get(*p) {
                    Some('n') => s.push('\n'),
                    Some('t') => s.push('\t'),
                    Some('r') => s.push('\r'),
                    Some('b') => s.push('\u{8}'),
                    Some('f') => s.push('\u{c}'),
                    Some('u') => {
                        let hex: String = c[*p + 1..(*p + 5).min(c.len())].iter().collect();
                        let code = u32::from_str_radix(&hex, 16)
                            .map_err(|_| "JSON: 잘못된 \\u".to_string())?;
                        s.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                        *p += 4;
                    }
                    // 유효한 JSON 이스케이프는 " \ / b f n r t u 뿐. \x 등은 SyntaxError.
                    Some('"') => s.push('"'),
                    Some('\\') => s.push('\\'),
                    Some('/') => s.push('/'),
                    Some(_) => return Err("JSON: 잘못된 이스케이프".to_string()),
                    None => return Err("JSON: 문자열 끝의 역슬래시".to_string()),
                }
                *p += 1;
            }
            Some(&ch) => {
                // 이스케이프 안 된 제어문자(U+0000-U+001F)는 JSON 문자열에서 금지 (§25.5.1).
                if (ch as u32) < 0x20 {
                    return Err("JSON: 문자열의 제어문자".to_string());
                }
                s.push(ch);
                *p += 1;
            }
        }
    }
}


// 아래 헬퍼는 인터프리터를 아는 직렬화기(builtins::json_ser)가 쓴다.
// 예전 자유함수 경로(json_stringify/_d/_body)는 replacer·indent·toJSON 을 못 해서
// 통째로 교체했다 — 남겨두면 "두 개의 진실"이 생긴다.
pub(super) fn json_quote_pub(s: &str) -> String {
    json_quote(s)
}

pub(super) fn json_num(n: f64) -> String {
    if n.is_finite() {
        num_to_str(n)
    } else {
        "null".to_string()
    }
}


pub(super) fn json_is_date(map: &Rc<RefCell<ObjMap>>) -> bool {
    is_date_obj(map)
}

pub(super) fn json_date_iso(map: &Rc<RefCell<ObjMap>>) -> Option<String> {
    let millis = match map.borrow().get("\u{0}time") {
        Some(Value::Num(n)) => *n,
        _ => 0.0,
    };
    if millis.is_finite() {
        Some(date_iso(millis))
    } else {
        None
    }
}

pub(super) fn json_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
