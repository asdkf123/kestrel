// Value 헬퍼: 표시/형변환/동등성/JSON. interp/mod.rs 에서 분리.
use super::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

pub(super) fn num_to_str(n: f64) -> String {
    if n.is_nan() {
        "NaN".to_string()
    } else if n.is_infinite() {
        if n > 0.0 { "Infinity".to_string() } else { "-Infinity".to_string() }
    } else if n.fract() == 0.0 && n.abs() < 9e15 {
        format!("{}", n as i64)
    } else {
        format!("{}", n)
    }
}

pub(super) fn to_bool(v: &Value) -> bool {
    match v {
        Value::Undefined | Value::Null => false,
        Value::Bool(b) => *b,
        Value::Num(n) => *n != 0.0 && !n.is_nan(),
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

// Obj 기반 Promise 판별 (__isPromise 마커)
pub(super) fn is_promise(v: &Value) -> bool {
    matches!(v, Value::Obj(o) if matches!(o.borrow().get("__isPromise"), Some(Value::Bool(true))))
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
    let mut m = HashMap::new();
    m.insert("__isDate".to_string(), Value::Bool(true));
    m.insert("__time".to_string(), Value::Num(millis));
    Value::Obj(Rc::new(RefCell::new(m)))
}

pub(super) fn is_date_obj(map: &Rc<RefCell<HashMap<String, Value>>>) -> bool {
    matches!(map.borrow().get("__isDate"), Some(Value::Bool(true)))
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
    let mut map = HashMap::new();
    map.insert("source".to_string(), Value::Str(source.to_string()));
    map.insert("flags".to_string(), Value::Str(flags.to_string()));
    map.insert("__isRegex".to_string(), Value::Bool(true));
    map.insert("global".to_string(), Value::Bool(flags.contains('g')));
    map.insert("ignoreCase".to_string(), Value::Bool(flags.contains('i')));
    map.insert("multiline".to_string(), Value::Bool(flags.contains('m')));
    map.insert("lastIndex".to_string(), Value::Num(0.0));
    Value::Obj(Rc::new(RefCell::new(map)))
}

// 객체가 정규식인지 (__isRegex == true)
pub(super) fn is_regex_obj(map: &Rc<RefCell<HashMap<String, Value>>>) -> bool {
    matches!(map.borrow().get("__isRegex"), Some(Value::Bool(true)))
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

pub(super) fn to_display(v: &Value) -> String {
    match v {
        Value::Undefined => "undefined".to_string(),
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Num(n) => num_to_str(*n),
        Value::Str(s) => s.clone(),
        Value::Obj(_) => "[object Object]".to_string(),
        Value::Arr(a) => {
            a.borrow().iter().map(to_display).collect::<Vec<_>>().join(",")
        }
        Value::Fn(_) | Value::Native(_) | Value::Class(_) | Value::Bound(_) => {
            "function".to_string()
        }
        Value::Getter(_) => "[getter]".to_string(),
        Value::MapVal(_) => "[object Map]".to_string(),
        Value::SetVal(_) => "[object Set]".to_string(),
        Value::Style(_) => "[object CSSStyleDeclaration]".to_string(),
        // classList 를 문자열화하면 class 값 (DOMTokenList.toString)
        Value::ClassList(_) => "[object DOMTokenList]".to_string(),
        Value::Dom(_) => "[object Element]".to_string(),
        Value::Instance(i) => format!("[object {}]", i.class.name),
        // Proxy 문자열화는 target 에 위임 (트랩 없는 근사)
        Value::Proxy(p) => to_display(&p.0),
    }
}

pub(super) fn type_of(v: &Value) -> &'static str {
    match v {
        Value::Undefined => "undefined",
        Value::Null => "object", // JS 의 유명한 typeof null
        Value::Bool(_) => "boolean",
        Value::Num(_) => "number",
        Value::Str(_) => "string",
        Value::Fn(_) | Value::Native(_) | Value::Class(_) | Value::Bound(_) => "function",
        _ => "object",
    }
}

pub(super) fn strict_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Undefined, Value::Undefined) | (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Num(x), Value::Num(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Obj(x), Value::Obj(y)) => Rc::ptr_eq(x, y),
        (Value::Arr(x), Value::Arr(y)) => Rc::ptr_eq(x, y),
        (Value::Fn(x), Value::Fn(y)) => Rc::ptr_eq(x, y),
        (Value::Dom(x), Value::Dom(y)) => x == y,
        (Value::Class(x), Value::Class(y)) => Rc::ptr_eq(x, y),
        (Value::Instance(x), Value::Instance(y)) => Rc::ptr_eq(x, y),
        (Value::MapVal(x), Value::MapVal(y)) => Rc::ptr_eq(x, y),
        (Value::SetVal(x), Value::SetVal(y)) => Rc::ptr_eq(x, y),
        (Value::Bound(x), Value::Bound(y)) => Rc::ptr_eq(x, y),
        (Value::Style(x), Value::Style(y)) => x == y,
        (Value::ClassList(x), Value::ClassList(y)) => x == y,
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
            let mut map = HashMap::new();
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

// 직렬화 불가(함수/undefined 등)는 None. 객체 키는 정렬 (HashMap 순서 비결정 대비).
pub(super) fn json_stringify(v: &Value) -> Option<String> {
    match v {
        Value::Undefined
        | Value::Fn(_)
        | Value::Native(_)
        | Value::Dom(_)
        | Value::Class(_)
        | Value::Bound(_)
        | Value::Getter(_)
        | Value::MapVal(_)
        | Value::SetVal(_)
        | Value::Style(_)
        | Value::ClassList(_)
        | Value::Proxy(_) => None,
        // 인스턴스는 필드를 일반 객체처럼 직렬화
        Value::Instance(inst) => {
            let m = inst.fields.borrow();
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .into_iter()
                .filter_map(|k| json_stringify(&m[k]).map(|v| format!("{}:{}", json_quote(k), v)))
                .collect();
            Some(format!("{{{}}}", parts.join(",")))
        }
        Value::Null => Some("null".to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Num(n) => {
            Some(if n.is_finite() { num_to_str(*n) } else { "null".to_string() })
        }
        Value::Str(s) => Some(json_quote(s)),
        Value::Arr(a) => {
            let items: Vec<String> = a
                .borrow()
                .iter()
                .map(|v| json_stringify(v).unwrap_or("null".to_string()))
                .collect();
            Some(format!("[{}]", items.join(",")))
        }
        Value::Obj(map) => {
            let m = map.borrow();
            // __proto__ 링크는 직렬화 대상 아님
            let mut keys: Vec<&String> = m.keys().filter(|k| *k != "__proto__").collect();
            keys.sort();
            let parts: Vec<String> = keys
                .into_iter()
                .filter_map(|k| json_stringify(&m[k]).map(|v| format!("{}:{}", json_quote(k), v)))
                .collect();
            Some(format!("{{{}}}", parts.join(",")))
        }
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
