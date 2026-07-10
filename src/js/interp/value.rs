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
        Value::Dom(_) => "[object Element]".to_string(),
        Value::Instance(i) => format!("[object {}]", i.class.name),
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
        | Value::SetVal(_) => None,
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
            let mut keys: Vec<&String> = m.keys().collect();
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
