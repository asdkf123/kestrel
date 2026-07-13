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
pub(super) fn to_i32(v: &Value) -> i32 {
    let n = to_num(v);
    if !n.is_finite() {
        return 0;
    }
    (n.trunc().rem_euclid(4294967296.0)) as u32 as i32
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
        Value::Str(s) => {
            let t = s.trim();
            if t.is_empty() {
                0.0
            } else {
                t.parse::<f64>().unwrap_or(f64::NAN)
            }
        }
        _ => f64::NAN,
    }
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
    matches!(v, Value::Fn(_) | Value::Native(_) | Value::Class(_) | Value::Bound(_))
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
                    let mut j = i + 1;
                    let mut num = String::new();
                    while j < t.len() && t[j].is_ascii_digit() && num.len() < 2 {
                        num.push(t[j]);
                        j += 1;
                    }
                    let gi: usize = num.parse().unwrap_or(0);
                    if gi >= 1 && gi < mt.groups.len() {
                        if let Some((a, b)) = mt.groups[gi] {
                            out.push_str(&chars[a..b].iter().collect::<String>());
                        }
                        i = j;
                    } else {
                        out.push('$');
                        i += 1;
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

// 문자열을 정규식 리터럴로 이스케이프 (str.match('a.b') 처럼 문자열 인자용)
pub(super) fn regex_escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if "\\^$.|?*+()[]{}".contains(c) {
            out.push('\\');
        }
        out.push(c);
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
                e.attributes
                    .get("class")
                    .map(|c| c.split_whitespace().any(|t| t == query))
                    .unwrap_or(false)
            } else {
                query == "*" || e.tag_name.eq_ignore_ascii_case(query)
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

// (year, month[1-12], day, h, m, s, ms) → epoch millis (UTC)
pub(super) fn date_to_millis(y: i64, mo: i64, d: i64, h: i64, mi: i64, s: i64, ms: i64) -> f64 {
    // days_from_civil
    let y = if mo <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as i64;
    let mp = if mo > 2 { mo - 3 } else { mo + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    (days * 86_400_000 + h * 3_600_000 + mi * 60_000 + s * 1000 + ms) as f64
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
    let mut dp = date.split('-');
    let y: i64 = dp.next()?.parse().ok()?;
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

fn two(n: u32) -> String {
    format!("{:02}", n)
}

// Date → ISO 8601 문자열
pub(super) fn date_iso(millis: f64) -> String {
    let (y, mo, d, h, mi, s, ms, _) = date_parts(millis);
    format!(
        "{:04}-{}-{}T{}:{}:{}.{:03}Z",
        y,
        two(mo),
        two(d),
        two(h),
        two(mi),
        two(s),
        ms
    )
}

// Date → 사람이 읽는 문자열 (간이 UTC)
pub(super) fn date_string(millis: f64) -> String {
    const DOW: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MON: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (y, mo, d, h, mi, s, _, wd) = date_parts(millis);
    format!(
        "{} {} {:02} {} {}:{}:{} GMT+0000",
        DOW[wd as usize % 7],
        MON[(mo as usize - 1) % 12],
        d,
        y,
        two(h),
        two(mi),
        two(s)
    )
}

// 정규식 리터럴/RegExp → {source, flags, __isRegex, global, lastIndex} 객체
pub(super) fn make_regex_obj(source: &str, flags: &str) -> Value {
    let mut map = ObjMap::new();
    map.insert("source".to_string(), Value::Str(source.to_string()));
    map.insert("flags".to_string(), Value::Str(flags.to_string()));
    map.insert("\u{0}isRegex".to_string(), Value::Bool(true));
    map.insert("global".to_string(), Value::Bool(flags.contains('g')));
    map.insert("ignoreCase".to_string(), Value::Bool(flags.contains('i')));
    map.insert("multiline".to_string(), Value::Bool(flags.contains('m')));
    map.insert("lastIndex".to_string(), Value::Num(0.0));
    Value::Obj(Rc::new(RefCell::new(map)))
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
            let s = match b.get("source") {
                Some(Value::Str(s)) => s.clone(),
                _ => return None,
            };
            let f = match b.get("flags") {
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
        Value::Obj(_) => "[object Object]".to_string(),
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
        // classList 를 문자열화하면 class 값 (DOMTokenList.toString)
        Value::ClassList(_) => "[object DOMTokenList]".to_string(),
        Value::Dom(_) => "[object Element]".to_string(),
        Value::Instance(i) => format!("[object {}]", i.class.name),
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
        (Value::ClassList(x), Value::ClassList(y)) => x == y,
        // 심볼 동일성은 고유 key 비교 (Symbol('x')!==Symbol('x'), Symbol.for 은 ===).
        (Value::Symbol(x), Value::Symbol(y)) => x.key == y.key,
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

pub(super) fn json_parse(src: &str) -> Result<Value, String> {
    let chars: Vec<char> = src.chars().collect();
    let mut pos = 0usize;
    let v = json_value(&chars, &mut pos)?;
    json_ws(&chars, &mut pos);
    if pos != chars.len() {
        return Err("JSON: 값 뒤에 잉여 문자".to_string());
    }
    Ok(v)
}

pub(super) fn json_ws(c: &[char], p: &mut usize) {
    while *p < c.len() && c[*p].is_whitespace() {
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

pub(super) fn json_value(c: &[char], p: &mut usize) -> Result<Value, String> {
    json_ws(c, p);
    match c.get(*p) {
        None => Err("JSON 이 갑자기 끝남".to_string()),
        Some('{') => {
            *p += 1;
            let mut map = ObjMap::new();
            json_ws(c, p);
            if c.get(*p) == Some(&'}') {
                *p += 1;
                return Ok(Value::Obj(Rc::new(RefCell::new(map))));
            }
            loop {
                json_ws(c, p);
                let key = json_string(c, p)?;
                json_ws(c, p);
                if c.get(*p) != Some(&':') {
                    return Err("JSON: ':' 필요".to_string());
                }
                *p += 1;
                map.insert(key, json_value(c, p)?);
                json_ws(c, p);
                match c.get(*p) {
                    Some(',') => *p += 1,
                    Some('}') => {
                        *p += 1;
                        return Ok(Value::Obj(Rc::new(RefCell::new(map))));
                    }
                    _ => return Err("JSON: ',' 나 '}' 필요".to_string()),
                }
            }
        }
        Some('[') => {
            *p += 1;
            let mut items = Vec::new();
            json_ws(c, p);
            if c.get(*p) == Some(&']') {
                *p += 1;
                return Ok(Value::Arr(ArrayObj::new(items)));
            }
            loop {
                items.push(json_value(c, p)?);
                json_ws(c, p);
                match c.get(*p) {
                    Some(',') => *p += 1,
                    Some(']') => {
                        *p += 1;
                        return Ok(Value::Arr(ArrayObj::new(items)));
                    }
                    _ => return Err("JSON: ',' 나 ']' 필요".to_string()),
                }
            }
        }
        Some('"') => Ok(Value::Str(json_string(c, p)?)),
        Some('t') if json_lit(c, p, "true") => Ok(Value::Bool(true)),
        Some('f') if json_lit(c, p, "false") => Ok(Value::Bool(false)),
        Some('n') if json_lit(c, p, "null") => Ok(Value::Null),
        Some(&ch) if ch == '-' || ch.is_ascii_digit() => {
            let start = *p;
            while *p < c.len()
                && matches!(c[*p], '-' | '+' | '.' | 'e' | 'E' | '0'..='9')
            {
                *p += 1;
            }
            let s: String = c[start..*p].iter().collect();
            s.parse::<f64>().map(Value::Num).map_err(|_| format!("JSON: 잘못된 수 {}", s))
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
                    Some(&other) => s.push(other), // \" \\ \/ 등
                    None => return Err("JSON: 문자열 끝의 역슬래시".to_string()),
                }
                *p += 1;
            }
            Some(&ch) => {
                s.push(ch);
                *p += 1;
            }
        }
    }
}

// 직렬화 불가(함수/undefined 등)는 Ok(None). 객체 키는 삽입 순서(ObjMap) 유지.
// 순환 구조는 Err → 호출측이 TypeError 로 던진다(표준).
pub(super) const JSON_CYCLE_MSG: &str = "순환 구조는 JSON 으로 직렬화할 수 없음";

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

pub(super) fn json_is_internal(k: &str) -> bool {
    is_internal_key(k)
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
