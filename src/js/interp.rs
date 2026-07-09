// 트리 워킹 인터프리터. Value/Env(렉시컬 체인)/제어 흐름.
// 무한 루프로 브라우저가 멈추지 않도록 실행 스텝 한도를 둔다.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use super::ast::*;
use super::parser::parse;

const STEP_LIMIT: u64 = 5_000_000;
// 이 접두사의 에러는 try/catch 로 잡을 수 없다 (무한 루프 가드가 무력화되지 않게)
const STEP_LIMIT_MSG: &str = "실행 한도 초과";

#[derive(Clone)]
pub enum Value {
    Undefined,
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Obj(Rc<RefCell<HashMap<String, Value>>>),
    Arr(Rc<RefCell<Vec<Value>>>),
    Fn(Rc<JsFn>),
    Native(Native),
    // DOM 요소 핸들: 아레나 NodeId (구조 변형에도 안정)
    Dom(crate::dom::NodeId),
}

pub struct JsFn {
    pub params: Vec<String>,
    pub body: Vec<Stmt>,
    pub env: EnvRef, // 클로저가 캡처한 렉시컬 환경
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Native {
    ConsoleLog,
    ArrayPush,
    GetElementById,
    AddEventListener,
    CreateElement,
    AppendChild,
    RemoveElement,
    SetAttribute,
    GetAttribute,
    QuerySelector,
    QuerySelectorAll,
    Math(MathOp),
    Str(StrOp),
    Arr(ArrOp),
    JsonParse,
    JsonStringify,
    ParseInt,
    ParseFloat,
    IsNaN,
    LsGetItem,
    LsSetItem,
    LsRemoveItem,
    LsClear,
    Alert,
    // 받고 아무것도 안 함 (window.addEventListener 등 — 창 이벤트는 아직 없음)
    Noop,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MathOp {
    Floor,
    Ceil,
    Round,
    Abs,
    Min,
    Max,
    Sqrt,
    Pow,
    Random,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StrOp {
    IndexOf,
    Slice,
    Split,
    Upper,
    Lower,
    Trim,
    Replace,
    CharAt,
    Includes,
    StartsWith,
    EndsWith,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ArrOp {
    Join,
    Pop,
    IndexOf,
    Slice,
    ForEach,
    Map,
    Filter,
}

impl std::fmt::Debug for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Value::Undefined => write!(f, "undefined"),
            Value::Null => write!(f, "null"),
            Value::Bool(b) => write!(f, "{}", b),
            Value::Num(n) => write!(f, "{}", n),
            Value::Str(s) => write!(f, "{:?}", s),
            Value::Obj(_) => write!(f, "[object]"),
            Value::Arr(_) => write!(f, "[array]"),
            Value::Fn(_) => write!(f, "[function]"),
            Value::Native(n) => write!(f, "[native {:?}]", n),
            Value::Dom(p) => write!(f, "[dom {:?}]", p),
        }
    }
}

// ── 환경 (스코프 체인) ──────────────────────────────────────────────

pub type EnvRef = Rc<RefCell<Env>>;

pub struct Env {
    vars: HashMap<String, Value>,
    parent: Option<EnvRef>,
}

impl Env {
    fn new(parent: Option<EnvRef>) -> EnvRef {
        Rc::new(RefCell::new(Env { vars: HashMap::new(), parent }))
    }
}

fn env_get(env: &EnvRef, name: &str) -> Option<Value> {
    if let Some(v) = env.borrow().vars.get(name) {
        return Some(v.clone());
    }
    let parent = env.borrow().parent.clone();
    parent.and_then(|p| env_get(&p, name))
}

// 체인에서 기존 바인딩을 갱신. 없으면 전역(최상위)에 새로 만든다 (sloppy 모드 유사).
fn env_set(env: &EnvRef, name: &str, value: Value) {
    {
        let mut e = env.borrow_mut();
        if e.vars.contains_key(name) {
            e.vars.insert(name.to_string(), value);
            return;
        }
    }
    let parent = env.borrow().parent.clone();
    match parent {
        Some(p) => env_set(&p, name, value),
        None => {
            env.borrow_mut().vars.insert(name.to_string(), value);
        }
    }
}

fn env_declare(env: &EnvRef, name: &str, value: Value) {
    env.borrow_mut().vars.insert(name.to_string(), value);
}

// ── 값 변환 ────────────────────────────────────────────────────────

pub fn num_to_str(n: f64) -> String {
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

pub fn to_bool(v: &Value) -> bool {
    match v {
        Value::Undefined | Value::Null => false,
        Value::Bool(b) => *b,
        Value::Num(n) => *n != 0.0 && !n.is_nan(),
        Value::Str(s) => !s.is_empty(),
        _ => true,
    }
}

// JS ToInt32: 2^32 모듈로 후 부호 있는 32비트로 (비트 연산 의미론)
fn to_i32(v: &Value) -> i32 {
    let n = to_num(v);
    if !n.is_finite() {
        return 0;
    }
    (n.trunc().rem_euclid(4294967296.0)) as u32 as i32
}

fn to_num(v: &Value) -> f64 {
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

pub fn to_display(v: &Value) -> String {
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
        Value::Fn(_) | Value::Native(_) => "function".to_string(),
        Value::Dom(_) => "[object Element]".to_string(),
    }
}

fn type_of(v: &Value) -> &'static str {
    match v {
        Value::Undefined => "undefined",
        Value::Null => "object", // JS 의 유명한 typeof null
        Value::Bool(_) => "boolean",
        Value::Num(_) => "number",
        Value::Str(_) => "string",
        Value::Fn(_) | Value::Native(_) => "function",
        _ => "object",
    }
}

fn strict_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Undefined, Value::Undefined) | (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Num(x), Value::Num(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Obj(x), Value::Obj(y)) => Rc::ptr_eq(x, y),
        (Value::Arr(x), Value::Arr(y)) => Rc::ptr_eq(x, y),
        (Value::Fn(x), Value::Fn(y)) => Rc::ptr_eq(x, y),
        (Value::Dom(x), Value::Dom(y)) => x == y,
        _ => false,
    }
}

fn loose_eq(a: &Value, b: &Value) -> bool {
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

fn json_parse(src: &str) -> Result<Value, String> {
    let chars: Vec<char> = src.chars().collect();
    let mut pos = 0usize;
    let v = json_value(&chars, &mut pos)?;
    json_ws(&chars, &mut pos);
    if pos != chars.len() {
        return Err("JSON: 값 뒤에 잉여 문자".to_string());
    }
    Ok(v)
}

fn json_ws(c: &[char], p: &mut usize) {
    while *p < c.len() && c[*p].is_whitespace() {
        *p += 1;
    }
}

fn json_lit(c: &[char], p: &mut usize, lit: &str) -> bool {
    if c[*p..].starts_with(&lit.chars().collect::<Vec<_>>()[..]) {
        *p += lit.chars().count();
        true
    } else {
        false
    }
}

fn json_value(c: &[char], p: &mut usize) -> Result<Value, String> {
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
                return Ok(Value::Arr(Rc::new(RefCell::new(items))));
            }
            loop {
                items.push(json_value(c, p)?);
                json_ws(c, p);
                match c.get(*p) {
                    Some(',') => *p += 1,
                    Some(']') => {
                        *p += 1;
                        return Ok(Value::Arr(Rc::new(RefCell::new(items))));
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

fn json_string(c: &[char], p: &mut usize) -> Result<String, String> {
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
fn json_stringify(v: &Value) -> Option<String> {
    match v {
        Value::Undefined | Value::Fn(_) | Value::Native(_) | Value::Dom(_) => None,
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

fn json_quote(s: &str) -> String {
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

// ── 제어 흐름 ──────────────────────────────────────────────────────

enum Flow {
    Normal(Value),
    Return(Value),
    Break,
    Continue,
}

// ── 인터프리터 ────────────────────────────────────────────────────

pub struct Interp {
    pub global: EnvRef,
    pub console: Vec<String>, // console.log 캡처 (호출측이 터미널에 출력)
    steps: u64,
    // DOM 바인딩이 사용 (실행 동안만 유효한 아레나 포인터)
    pub dom: Option<*mut crate::dom::Dom>,
    // 이벤트 핸들러 레지스트리: (요소 NodeId, 이벤트 타입, 핸들러 함수)
    pub handlers: Vec<(crate::dom::NodeId, String, Value)>,
    // Math.random 용 xorshift 상태
    rng: u64,
    // throw 된 값 (에러 채널은 String 이라 값은 사이드 채널로 전달)
    thrown: Option<Value>,
    // localStorage 스텁 저장소 (페이지 수명)
    storage: HashMap<String, String>,
}

impl Interp {
    pub fn new() -> Interp {
        let global = Env::new(None);
        // console.log
        let mut console = HashMap::new();
        console.insert("log".to_string(), Value::Native(Native::ConsoleLog));
        env_declare(&global, "console", Value::Obj(Rc::new(RefCell::new(console))));
        // document (dom 포인터 미설정 시 호출하면 런타임 에러)
        let mut document = HashMap::new();
        document.insert("getElementById".to_string(), Value::Native(Native::GetElementById));
        document.insert("createElement".to_string(), Value::Native(Native::CreateElement));
        document.insert("querySelector".to_string(), Value::Native(Native::QuerySelector));
        document.insert("querySelectorAll".to_string(), Value::Native(Native::QuerySelectorAll));
        // 문서 레벨 이벤트(DOMContentLoaded 등)는 아직 발화 안 함 — no-op 수용
        document.insert("addEventListener".to_string(), Value::Native(Native::Noop));
        document.insert("removeEventListener".to_string(), Value::Native(Native::Noop));
        env_declare(&global, "document", Value::Obj(Rc::new(RefCell::new(document))));
        // Math
        let mut math = HashMap::new();
        for (name, op) in [
            ("floor", MathOp::Floor),
            ("ceil", MathOp::Ceil),
            ("round", MathOp::Round),
            ("abs", MathOp::Abs),
            ("min", MathOp::Min),
            ("max", MathOp::Max),
            ("sqrt", MathOp::Sqrt),
            ("pow", MathOp::Pow),
            ("random", MathOp::Random),
        ] {
            math.insert(name.to_string(), Value::Native(Native::Math(op)));
        }
        math.insert("PI".to_string(), Value::Num(std::f64::consts::PI));
        math.insert("E".to_string(), Value::Num(std::f64::consts::E));
        env_declare(&global, "Math", Value::Obj(Rc::new(RefCell::new(math))));
        // JSON
        let mut json = HashMap::new();
        json.insert("parse".to_string(), Value::Native(Native::JsonParse));
        json.insert("stringify".to_string(), Value::Native(Native::JsonStringify));
        env_declare(&global, "JSON", Value::Obj(Rc::new(RefCell::new(json))));
        // 전역 함수
        env_declare(&global, "parseInt", Value::Native(Native::ParseInt));
        env_declare(&global, "parseFloat", Value::Native(Native::ParseFloat));
        env_declare(&global, "isNaN", Value::Native(Native::IsNaN));
        // localStorage: 페이지 수명 동안 실제로 동작하는 인메모리 스토리지
        let mut ls = HashMap::new();
        ls.insert("getItem".to_string(), Value::Native(Native::LsGetItem));
        ls.insert("setItem".to_string(), Value::Native(Native::LsSetItem));
        ls.insert("removeItem".to_string(), Value::Native(Native::LsRemoveItem));
        ls.insert("clear".to_string(), Value::Native(Native::LsClear));
        let ls = Value::Obj(Rc::new(RefCell::new(ls)));
        env_declare(&global, "localStorage", ls.clone());
        env_declare(&global, "sessionStorage", ls.clone());
        // navigator / alert
        let mut nav = HashMap::new();
        nav.insert("userAgent".to_string(), Value::Str("Kestrel/0.1".to_string()));
        let nav = Value::Obj(Rc::new(RefCell::new(nav)));
        env_declare(&global, "navigator", nav.clone());
        env_declare(&global, "alert", Value::Native(Native::Alert));
        // window: 전역 객체 스텁 — 프로퍼티 읽기/쓰기는 되지만 전역 변수와
        // 연동되진 않음 (window.x = 1 후 x 로 읽기 미지원). 존재 자체가
        // "window 미정의" 즉사를 막는다. 필드 테스트 최다 런타임 에러.
        let mut window = HashMap::new();
        window.insert("localStorage".to_string(), ls);
        window.insert("navigator".to_string(), nav);
        window.insert("addEventListener".to_string(), Value::Native(Native::Noop));
        window.insert("removeEventListener".to_string(), Value::Native(Native::Noop));
        env_declare(&global, "window", Value::Obj(Rc::new(RefCell::new(window))));
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64 | 1)
            .unwrap_or(0x9e3779b9);
        Interp {
            global,
            console: Vec::new(),
            steps: 0,
            dom: None,
            handlers: Vec::new(),
            rng: seed,
            thrown: None,
            storage: HashMap::new(),
        }
    }

    // location 전역 설치 (페이지 URL 기반). window.location 에도 공유.
    pub fn install_location(&mut self, url: &str) {
        let Ok(u) = crate::url::Url::parse(url) else { return };
        let mut loc = HashMap::new();
        loc.insert("href".to_string(), Value::Str(u.as_string()));
        loc.insert("protocol".to_string(), Value::Str(format!("{}:", u.scheme)));
        loc.insert("host".to_string(), Value::Str(u.host.clone()));
        loc.insert("hostname".to_string(), Value::Str(u.host.clone()));
        loc.insert("pathname".to_string(), Value::Str(u.path.clone()));
        let loc = Value::Obj(Rc::new(RefCell::new(loc)));
        env_declare(&self.global, "location", loc.clone());
        if let Some(Value::Obj(w)) = env_get(&self.global, "window") {
            w.borrow_mut().insert("location".to_string(), loc);
        }
    }

    // 이벤트 디스패치: 타깃과 그 조상 체인에 등록된 핸들러 실행 (버블링).
    // 하나라도 실행되면 true. 핸들러 에러는 [js error] 로 격리.
    pub fn fire_handlers(&mut self, target: crate::dom::NodeId, event: &str) -> bool {
        self.steps = 0; // 이벤트마다 실행 한도 리셋
        let mut chain = vec![target];
        if let Some(p) = self.dom {
            chain.extend(unsafe { (*p).ancestors(target) });
        }
        let to_run: Vec<Value> = self
            .handlers
            .iter()
            .filter(|(id, t, _)| t == event && chain.contains(id))
            .map(|(_, _, f)| f.clone())
            .collect();
        let fired = !to_run.is_empty();
        for f in to_run {
            if let Err(e) = self.call_value(f, None, Vec::new()) {
                println!("[js error] {}", e);
            }
        }
        fired
    }

    // onclick 속성 등 인라인 핸들러 소스 실행 (전역 환경에서)
    pub fn run_inline_handler(&mut self, src: &str) {
        self.steps = 0;
        if let Err(e) = self.run(src) {
            println!("[js error] {}", e);
        }
    }

    pub fn run(&mut self, src: &str) -> Result<Value, String> {
        self.steps = 0; // 실행 단위(스크립트/핸들러)마다 한도 리셋
        let program = parse(src)?;
        let env = self.global.clone();
        match self.exec_block(&program, &env)? {
            Flow::Normal(v) | Flow::Return(v) => Ok(v),
            _ => Ok(Value::Undefined),
        }
    }

    fn tick(&mut self) -> Result<(), String> {
        self.steps += 1;
        if self.steps > STEP_LIMIT {
            return Err(format!("{} (무한 루프?)", STEP_LIMIT_MSG));
        }
        Ok(())
    }

    // 함수 선언 호이스팅: 블록 실행 전에 FuncDecl 을 먼저 바인딩
    fn exec_block(&mut self, stmts: &[Stmt], env: &EnvRef) -> Result<Flow, String> {
        for s in stmts {
            if let Stmt::FuncDecl { name, params, body } = s {
                let f = Value::Fn(Rc::new(JsFn {
                    params: params.clone(),
                    body: body.clone(),
                    env: env.clone(),
                }));
                env_declare(env, name, f);
            }
        }
        let mut last = Value::Undefined;
        for s in stmts {
            match self.exec_stmt(s, env)? {
                Flow::Normal(v) => last = v,
                flow => return Ok(flow),
            }
        }
        Ok(Flow::Normal(last))
    }

    fn exec_stmt(&mut self, stmt: &Stmt, env: &EnvRef) -> Result<Flow, String> {
        self.tick()?;
        match stmt {
            Stmt::VarDecl { name, init, .. } => {
                let v = match init {
                    Some(e) => self.eval(e, env)?,
                    None => Value::Undefined,
                };
                env_declare(env, name, v);
                Ok(Flow::Normal(Value::Undefined))
            }
            Stmt::FuncDecl { .. } => Ok(Flow::Normal(Value::Undefined)), // 호이스팅됨
            Stmt::If { cond, then, other } => {
                let c = self.eval(cond, env)?;
                let scope = Env::new(Some(env.clone()));
                if to_bool(&c) {
                    self.exec_block(then, &scope)
                } else if let Some(other) = other {
                    self.exec_block(other, &scope)
                } else {
                    Ok(Flow::Normal(Value::Undefined))
                }
            }
            Stmt::While { cond, body } => {
                loop {
                    self.tick()?;
                    if !to_bool(&self.eval(cond, env)?) {
                        break;
                    }
                    let scope = Env::new(Some(env.clone()));
                    match self.exec_block(body, &scope)? {
                        Flow::Break => break,
                        Flow::Continue | Flow::Normal(_) => {}
                        ret => return Ok(ret),
                    }
                }
                Ok(Flow::Normal(Value::Undefined))
            }
            Stmt::For { init, cond, step, body } => {
                let outer = Env::new(Some(env.clone())); // for(let i...) 스코프
                if let Some(init) = init {
                    self.exec_stmt(init, &outer)?;
                }
                loop {
                    self.tick()?;
                    if let Some(cond) = cond {
                        if !to_bool(&self.eval(cond, &outer)?) {
                            break;
                        }
                    }
                    let scope = Env::new(Some(outer.clone()));
                    match self.exec_block(body, &scope)? {
                        Flow::Break => break,
                        Flow::Continue | Flow::Normal(_) => {}
                        ret => return Ok(ret),
                    }
                    if let Some(step) = step {
                        self.eval(step, &outer)?;
                    }
                }
                Ok(Flow::Normal(Value::Undefined))
            }
            Stmt::Return(e) => {
                let v = match e {
                    Some(e) => self.eval(e, env)?,
                    None => Value::Undefined,
                };
                Ok(Flow::Return(v))
            }
            Stmt::Break => Ok(Flow::Break),
            Stmt::Continue => Ok(Flow::Continue),
            Stmt::Block(stmts) => {
                let scope = Env::new(Some(env.clone()));
                self.exec_block(stmts, &scope)
            }
            Stmt::Expr(e) => Ok(Flow::Normal(self.eval(e, env)?)),
            Stmt::Throw(e) => {
                let v = self.eval(e, env)?;
                let msg = to_display(&v);
                self.thrown = Some(v);
                Err(msg)
            }
            Stmt::Try { body, catch, finally } => {
                let scope = Env::new(Some(env.clone()));
                let mut result = self.exec_block(body, &scope);
                if let Err(e) = &result {
                    // 스텝 한도 초과는 잡을 수 없음 (가드 무력화 방지)
                    if !e.starts_with(STEP_LIMIT_MSG) {
                        if let Some((param, cbody)) = catch {
                            // throw 된 값이 있으면 그 값, 네이티브 에러면 메시지 문자열
                            let caught =
                                self.thrown.take().unwrap_or(Value::Str(e.clone()));
                            let cscope = Env::new(Some(env.clone()));
                            if let Some(p) = param {
                                env_declare(&cscope, p, caught);
                            }
                            result = self.exec_block(cbody, &cscope);
                        }
                    }
                }
                if let Some(fbody) = finally {
                    let fscope = Env::new(Some(env.clone()));
                    // finally 의 에러/제어 흐름이 우선
                    match self.exec_block(fbody, &fscope)? {
                        Flow::Normal(_) => {}
                        flow => return Ok(flow),
                    }
                }
                result
            }
            Stmt::Switch { disc, cases } => {
                let d = self.eval(disc, env)?;
                let scope = Env::new(Some(env.clone()));
                let mut start = None;
                for (i, (test, _)) in cases.iter().enumerate() {
                    if let Some(t) = test {
                        let tv = self.eval(t, &scope)?;
                        if strict_eq(&d, &tv) {
                            start = Some(i);
                            break;
                        }
                    }
                }
                if start.is_none() {
                    start = cases.iter().position(|(t, _)| t.is_none()); // default
                }
                if let Some(s) = start {
                    for (_, stmts) in &cases[s..] {
                        // 폴스루: break 가 나올 때까지 다음 케이스도 실행
                        match self.exec_block(stmts, &scope)? {
                            Flow::Break => return Ok(Flow::Normal(Value::Undefined)),
                            Flow::Normal(_) => {}
                            other => return Ok(other),
                        }
                    }
                }
                Ok(Flow::Normal(Value::Undefined))
            }
        }
    }

    fn eval(&mut self, expr: &Expr, env: &EnvRef) -> Result<Value, String> {
        self.tick()?;
        match expr {
            Expr::Num(n) => Ok(Value::Num(*n)),
            Expr::Str(s) => Ok(Value::Str(s.clone())),
            Expr::Bool(b) => Ok(Value::Bool(*b)),
            Expr::Null => Ok(Value::Null),
            Expr::Undefined => Ok(Value::Undefined),
            Expr::Ident(name) => {
                env_get(env, name).ok_or_else(|| format!("{} 은(는) 정의되지 않음", name))
            }
            Expr::Array(items) => {
                let mut v = Vec::new();
                for item in items {
                    v.push(self.eval(item, env)?);
                }
                Ok(Value::Arr(Rc::new(RefCell::new(v))))
            }
            Expr::Object(props) => {
                let mut map = HashMap::new();
                for (k, e) in props {
                    map.insert(k.clone(), self.eval(e, env)?);
                }
                Ok(Value::Obj(Rc::new(RefCell::new(map))))
            }
            Expr::Func { params, body } => Ok(Value::Fn(Rc::new(JsFn {
                params: params.clone(),
                body: body.clone(),
                env: env.clone(),
            }))),
            Expr::Regex { source, flags } => {
                // 매칭 엔진 없음: {source, flags} 객체. test/exec 호출 시 런타임 에러
                // (해당 스크립트만 중단 — try/catch 로 생존 가능)
                let mut map = HashMap::new();
                map.insert("source".to_string(), Value::Str(source.clone()));
                map.insert("flags".to_string(), Value::Str(flags.clone()));
                Ok(Value::Obj(Rc::new(RefCell::new(map))))
            }
            Expr::Template(parts) => {
                let mut s = String::new();
                for part in parts {
                    match part {
                        TemplatePart::Lit(t) => s.push_str(t),
                        TemplatePart::Expr(e) => s.push_str(&to_display(&self.eval(e, env)?)),
                    }
                }
                Ok(Value::Str(s))
            }
            Expr::Unary { op, expr } => {
                let v = self.eval(expr, env)?;
                Ok(match op {
                    UnOp::Neg => Value::Num(-to_num(&v)),
                    UnOp::Not => Value::Bool(!to_bool(&v)),
                    UnOp::Typeof => Value::Str(type_of(&v).to_string()),
                    UnOp::BitNot => Value::Num(!to_i32(&v) as f64),
                })
            }
            Expr::Update { op, prefix, target } => {
                let old = to_num(&self.eval(target, env)?);
                let new = match op {
                    UpdOp::Inc => old + 1.0,
                    UpdOp::Dec => old - 1.0,
                };
                self.assign_to(target, Value::Num(new), env)?;
                Ok(Value::Num(if *prefix { new } else { old }))
            }
            Expr::Binary { op, left, right } => {
                let l = self.eval(left, env)?;
                let r = self.eval(right, env)?;
                self.binary(*op, l, r)
            }
            Expr::Logical { op, left, right } => {
                let l = self.eval(left, env)?;
                match op {
                    LogOp::And => {
                        if to_bool(&l) {
                            self.eval(right, env)
                        } else {
                            Ok(l)
                        }
                    }
                    LogOp::Or => {
                        if to_bool(&l) {
                            Ok(l)
                        } else {
                            self.eval(right, env)
                        }
                    }
                }
            }
            Expr::Ternary { cond, then, other } => {
                if to_bool(&self.eval(cond, env)?) {
                    self.eval(then, env)
                } else {
                    self.eval(other, env)
                }
            }
            Expr::Assign { op, target, value } => {
                let rhs = self.eval(value, env)?;
                let new = match op {
                    AssignOp::Set => rhs,
                    compound => {
                        let old = self.eval(target, env)?;
                        let bin = match compound {
                            AssignOp::Add => BinOp::Add,
                            AssignOp::Sub => BinOp::Sub,
                            AssignOp::Mul => BinOp::Mul,
                            _ => BinOp::Div,
                        };
                        self.binary(bin, old, rhs)?
                    }
                };
                self.assign_to(target, new.clone(), env)?;
                Ok(new)
            }
            Expr::Member { obj, prop, computed } => {
                let recv = self.eval(obj, env)?;
                let key = self.member_key(prop, *computed, env)?;
                self.member_get(&recv, &key)
            }
            Expr::Call { callee, args } => {
                let mut arg_vals = Vec::new();
                // 인자 평가는 수신자 결정 후가 아닌 JS 순서(콜리 먼저)지만
                // 우리 서브셋에선 차이가 관찰되지 않아 단순화한다.
                if let Expr::Member { obj, prop, computed } = &**callee {
                    let recv = self.eval(obj, env)?;
                    let key = self.member_key(prop, *computed, env)?;
                    let f = self.member_get(&recv, &key)?;
                    for a in args {
                        arg_vals.push(self.eval(a, env)?);
                    }
                    self.call_value(f, Some(recv), arg_vals)
                } else {
                    let f = self.eval(callee, env)?;
                    for a in args {
                        arg_vals.push(self.eval(a, env)?);
                    }
                    self.call_value(f, None, arg_vals)
                }
            }
        }
    }

    fn member_key(&mut self, prop: &Expr, computed: bool, env: &EnvRef) -> Result<String, String> {
        if computed {
            Ok(to_display(&self.eval(prop, env)?))
        } else if let Expr::Str(s) = prop {
            Ok(s.clone())
        } else {
            Err("잘못된 멤버 접근".to_string())
        }
    }

    fn member_get(&mut self, recv: &Value, key: &str) -> Result<Value, String> {
        match recv {
            Value::Obj(map) => Ok(map.borrow().get(key).cloned().unwrap_or(Value::Undefined)),
            Value::Arr(a) => {
                if key == "length" {
                    return Ok(Value::Num(a.borrow().len() as f64));
                }
                if key == "push" {
                    return Ok(Value::Native(Native::ArrayPush));
                }
                let op = match key {
                    "join" => Some(ArrOp::Join),
                    "pop" => Some(ArrOp::Pop),
                    "indexOf" => Some(ArrOp::IndexOf),
                    "slice" => Some(ArrOp::Slice),
                    "forEach" => Some(ArrOp::ForEach),
                    "map" => Some(ArrOp::Map),
                    "filter" => Some(ArrOp::Filter),
                    _ => None,
                };
                if let Some(op) = op {
                    return Ok(Value::Native(Native::Arr(op)));
                }
                if let Ok(i) = key.parse::<usize>() {
                    return Ok(a.borrow().get(i).cloned().unwrap_or(Value::Undefined));
                }
                Ok(Value::Undefined)
            }
            Value::Str(s) => {
                if key == "length" {
                    return Ok(Value::Num(s.chars().count() as f64));
                }
                let op = match key {
                    "indexOf" => Some(StrOp::IndexOf),
                    "slice" | "substring" => Some(StrOp::Slice),
                    "split" => Some(StrOp::Split),
                    "toUpperCase" => Some(StrOp::Upper),
                    "toLowerCase" => Some(StrOp::Lower),
                    "trim" => Some(StrOp::Trim),
                    "replace" => Some(StrOp::Replace),
                    "charAt" => Some(StrOp::CharAt),
                    "includes" => Some(StrOp::Includes),
                    "startsWith" => Some(StrOp::StartsWith),
                    "endsWith" => Some(StrOp::EndsWith),
                    _ => None,
                };
                if let Some(op) = op {
                    return Ok(Value::Native(Native::Str(op)));
                }
                if let Ok(i) = key.parse::<usize>() {
                    return Ok(s
                        .chars()
                        .nth(i)
                        .map(|c| Value::Str(c.to_string()))
                        .unwrap_or(Value::Undefined));
                }
                Ok(Value::Undefined)
            }
            Value::Dom(id) => {
                let native = match key {
                    "addEventListener" => Some(Native::AddEventListener),
                    "appendChild" => Some(Native::AppendChild),
                    "remove" => Some(Native::RemoveElement),
                    "setAttribute" => Some(Native::SetAttribute),
                    "getAttribute" => Some(Native::GetAttribute),
                    "querySelector" => Some(Native::QuerySelector),
                    "querySelectorAll" => Some(Native::QuerySelectorAll),
                    _ => None,
                };
                if let Some(n) = native {
                    return Ok(Value::Native(n));
                }
                self.dom_get(*id, key)
            }
            Value::Undefined | Value::Null => {
                Err(format!("{} 의 '{}' 를 읽을 수 없음", to_display(recv), key))
            }
            _ => Ok(Value::Undefined),
        }
    }

    fn call_value(
        &mut self,
        f: Value,
        recv: Option<Value>,
        args: Vec<Value>,
    ) -> Result<Value, String> {
        match f {
            Value::Fn(func) => {
                let scope = Env::new(Some(func.env.clone()));
                for (i, p) in func.params.iter().enumerate() {
                    env_declare(&scope, p, args.get(i).cloned().unwrap_or(Value::Undefined));
                }
                match self.exec_block(&func.body, &scope)? {
                    Flow::Return(v) => Ok(v),
                    _ => Ok(Value::Undefined),
                }
            }
            Value::Native(n) => self.call_native(n, recv, args),
            other => Err(format!("{} 은(는) 함수가 아님", to_display(&other))),
        }
    }

    fn call_native(
        &mut self,
        n: Native,
        recv: Option<Value>,
        args: Vec<Value>,
    ) -> Result<Value, String> {
        match n {
            Native::ConsoleLog => {
                let line = args.iter().map(to_display).collect::<Vec<_>>().join(" ");
                self.console.push(line);
                Ok(Value::Undefined)
            }
            Native::ArrayPush => match recv {
                Some(Value::Arr(a)) => {
                    for v in args {
                        a.borrow_mut().push(v);
                    }
                    Ok(Value::Num(a.borrow().len() as f64))
                }
                _ => Err("push 는 배열 메서드".to_string()),
            },
            Native::GetElementById => self.dom_get_element_by_id(args),
            Native::AddEventListener => match recv {
                Some(Value::Dom(id)) => {
                    let event = args.first().map(to_display).unwrap_or_default();
                    if let Some(f @ Value::Fn(_)) = args.get(1) {
                        self.handlers.push((id, event, f.clone()));
                    }
                    Ok(Value::Undefined)
                }
                _ => Err("addEventListener 는 요소 메서드".to_string()),
            },
            Native::CreateElement => {
                let tag = args.first().map(to_display).unwrap_or_default();
                if tag.is_empty() {
                    return Err("createElement 에 태그 이름이 필요".to_string());
                }
                let dom = self.dom_arena()?;
                Ok(Value::Dom(dom.create_element(&tag)))
            }
            Native::AppendChild => match (recv, args.first()) {
                (Some(Value::Dom(parent)), Some(Value::Dom(child))) => {
                    let child = *child;
                    let dom = self.dom_arena()?;
                    dom.append_child(parent, child);
                    Ok(Value::Dom(child))
                }
                _ => Err("appendChild 는 요소 인자가 필요".to_string()),
            },
            Native::RemoveElement => match recv {
                Some(Value::Dom(id)) => {
                    let dom = self.dom_arena()?;
                    dom.detach(id);
                    Ok(Value::Undefined)
                }
                _ => Err("remove 는 요소 메서드".to_string()),
            },
            Native::SetAttribute => match recv {
                Some(Value::Dom(id)) => {
                    let name = args.first().map(to_display).unwrap_or_default();
                    let value = args.get(1).map(to_display).unwrap_or_default();
                    let dom = self.dom_arena()?;
                    if let crate::dom::NodeType::Element(e) = &mut dom.get_mut(id).node_type {
                        e.attributes.insert(name, value);
                    }
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
                    MathOp::Round => a.round(),
                    MathOp::Abs => a.abs(),
                    MathOp::Sqrt => a.sqrt(),
                    MathOp::Pow => a.powf(args.get(1).map(to_num).unwrap_or(f64::NAN)),
                    MathOp::Min => args.iter().map(to_num).fold(f64::INFINITY, f64::min),
                    MathOp::Max => args.iter().map(to_num).fold(f64::NEG_INFINITY, f64::max),
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
                let arg_str = |i: usize| args.get(i).map(to_display).unwrap_or_default();
                Ok(match op {
                    StrOp::Upper => Value::Str(s.to_uppercase()),
                    StrOp::Lower => Value::Str(s.to_lowercase()),
                    StrOp::Trim => Value::Str(s.trim().to_string()),
                    StrOp::CharAt => {
                        let i = args.first().map(to_num).unwrap_or(0.0) as isize;
                        Value::Str(
                            chars
                                .get(i.max(0) as usize)
                                .map(|c| c.to_string())
                                .unwrap_or_default(),
                        )
                    }
                    StrOp::IndexOf => {
                        // 문자(char) 인덱스 기준 (UTF-16 이 아님 — 단순화)
                        let needle = arg_str(0);
                        match s.find(&needle) {
                            Some(byte_i) => Value::Num(s[..byte_i].chars().count() as f64),
                            None => Value::Num(-1.0),
                        }
                    }
                    StrOp::Includes => Value::Bool(s.contains(&arg_str(0))),
                    StrOp::StartsWith => Value::Bool(s.starts_with(&arg_str(0))),
                    StrOp::EndsWith => Value::Bool(s.ends_with(&arg_str(0))),
                    StrOp::Replace => {
                        Value::Str(s.replacen(&arg_str(0), &arg_str(1), 1)) // 첫 1회 (JS 동일)
                    }
                    StrOp::Slice => {
                        let len = chars.len() as isize;
                        let clampi = |v: f64| -> usize {
                            let i = v as isize;
                            (if i < 0 { len + i } else { i }).clamp(0, len) as usize
                        };
                        let start = clampi(args.first().map(to_num).unwrap_or(0.0));
                        let end = clampi(args.get(1).map(to_num).unwrap_or(len as f64));
                        Value::Str(chars[start..end.max(start)].iter().collect())
                    }
                    StrOp::Split => {
                        let sep = arg_str(0);
                        let parts: Vec<Value> = if args.is_empty() {
                            vec![Value::Str(s.clone())]
                        } else if sep.is_empty() {
                            chars.iter().map(|c| Value::Str(c.to_string())).collect()
                        } else {
                            s.split(&sep).map(|p| Value::Str(p.to_string())).collect()
                        };
                        Value::Arr(Rc::new(RefCell::new(parts)))
                    }
                })
            }
            Native::Arr(op) => {
                let Some(Value::Arr(a)) = recv else {
                    return Err("배열 메서드".to_string());
                };
                Ok(match op {
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
                        Value::Arr(Rc::new(RefCell::new(items[start..end.max(start)].to_vec())))
                    }
                    ArrOp::ForEach | ArrOp::Map | ArrOp::Filter => {
                        let f = args.first().cloned().ok_or("콜백이 필요")?;
                        let snapshot: Vec<Value> = a.borrow().clone();
                        let mut out = Vec::new();
                        for (i, item) in snapshot.into_iter().enumerate() {
                            let r = self.call_value(
                                f.clone(),
                                None,
                                vec![item.clone(), Value::Num(i as f64)],
                            )?;
                            match op {
                                ArrOp::Map => out.push(r),
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
                            _ => Value::Arr(Rc::new(RefCell::new(out))),
                        }
                    }
                })
            }
            Native::JsonParse => {
                let src = args.first().map(to_display).unwrap_or_default();
                json_parse(&src)
            }
            Native::JsonStringify => {
                Ok(json_stringify(args.first().unwrap_or(&Value::Undefined))
                    .map(Value::Str)
                    .unwrap_or(Value::Undefined))
            }
            Native::ParseInt => {
                let s = args.first().map(to_display).unwrap_or_default();
                let t = s.trim();
                let (neg, t) = match t.strip_prefix('-') {
                    Some(rest) => (true, rest),
                    None => (false, t.strip_prefix('+').unwrap_or(t)),
                };
                let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
                Ok(match digits.parse::<f64>() {
                    Ok(n) => Value::Num(if neg { -n } else { n }),
                    Err(_) => Value::Num(f64::NAN),
                })
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
            Native::IsNaN => {
                Ok(Value::Bool(args.first().map(to_num).unwrap_or(f64::NAN).is_nan()))
            }
            Native::LsGetItem => {
                let k = args.first().map(to_display).unwrap_or_default();
                Ok(self.storage.get(&k).map(|v| Value::Str(v.clone())).unwrap_or(Value::Null))
            }
            Native::LsSetItem => {
                let k = args.first().map(to_display).unwrap_or_default();
                let v = args.get(1).map(to_display).unwrap_or_default();
                self.storage.insert(k, v);
                Ok(Value::Undefined)
            }
            Native::LsRemoveItem => {
                let k = args.first().map(to_display).unwrap_or_default();
                self.storage.remove(&k);
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
                _ => Err("getAttribute 는 요소 메서드".to_string()),
            },
        }
    }

    fn binary(&mut self, op: BinOp, l: Value, r: Value) -> Result<Value, String> {
        Ok(match op {
            BinOp::Add => match (&l, &r) {
                (Value::Str(_), _) | (_, Value::Str(_)) => {
                    Value::Str(format!("{}{}", to_display(&l), to_display(&r)))
                }
                _ => Value::Num(to_num(&l) + to_num(&r)),
            },
            BinOp::Sub => Value::Num(to_num(&l) - to_num(&r)),
            BinOp::Mul => Value::Num(to_num(&l) * to_num(&r)),
            BinOp::Div => Value::Num(to_num(&l) / to_num(&r)),
            BinOp::Mod => Value::Num(to_num(&l) % to_num(&r)),
            BinOp::BitAnd => Value::Num((to_i32(&l) & to_i32(&r)) as f64),
            BinOp::BitOr => Value::Num((to_i32(&l) | to_i32(&r)) as f64),
            BinOp::BitXor => Value::Num((to_i32(&l) ^ to_i32(&r)) as f64),
            BinOp::Shl => Value::Num((to_i32(&l) << (to_i32(&r) & 31)) as f64),
            BinOp::Shr => Value::Num((to_i32(&l) >> (to_i32(&r) & 31)) as f64),
            BinOp::UShr => Value::Num(((to_i32(&l) as u32) >> (to_i32(&r) & 31)) as f64),
            BinOp::EqEq => Value::Bool(loose_eq(&l, &r)),
            BinOp::NotEq => Value::Bool(!loose_eq(&l, &r)),
            BinOp::EqEqEq => Value::Bool(strict_eq(&l, &r)),
            BinOp::NotEqEq => Value::Bool(!strict_eq(&l, &r)),
            BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                // 둘 다 문자열이면 사전순, 아니면 숫자 비교 (JS 유사)
                let b = if let (Value::Str(a), Value::Str(c)) = (&l, &r) {
                    match op {
                        BinOp::Lt => a < c,
                        BinOp::Gt => a > c,
                        BinOp::Le => a <= c,
                        _ => a >= c,
                    }
                } else {
                    let (x, y) = (to_num(&l), to_num(&r));
                    match op {
                        BinOp::Lt => x < y,
                        BinOp::Gt => x > y,
                        BinOp::Le => x <= y,
                        _ => x >= y,
                    }
                };
                Value::Bool(b)
            }
        })
    }

    fn assign_to(&mut self, target: &Expr, value: Value, env: &EnvRef) -> Result<(), String> {
        match target {
            Expr::Ident(name) => {
                env_set(env, name, value);
                Ok(())
            }
            Expr::Member { obj, prop, computed } => {
                let recv = self.eval(obj, env)?;
                let key = self.member_key(prop, *computed, env)?;
                match recv {
                    Value::Obj(map) => {
                        map.borrow_mut().insert(key, value);
                        Ok(())
                    }
                    Value::Arr(a) => {
                        if let Ok(i) = key.parse::<usize>() {
                            let mut arr = a.borrow_mut();
                            if i >= arr.len() {
                                arr.resize(i + 1, Value::Undefined);
                            }
                            arr[i] = value;
                            Ok(())
                        } else {
                            Ok(()) // 배열 비인덱스 프로퍼티는 무시 (단순화)
                        }
                    }
                    Value::Dom(id) => self.dom_set(id, &key, value),
                    other => Err(format!("{} 에 할당할 수 없음", to_display(&other))),
                }
            }
            _ => Err("할당 대상이 아님".to_string()),
        }
    }

    // ── DOM 바인딩 (아레나; dom 포인터는 실행 동안만 유효, 미설정 시 에러) ──

    fn dom_arena(&mut self) -> Result<&mut crate::dom::Dom, String> {
        match self.dom {
            // 안전성: run_scripts/dispatch 가 실행 동안에만 유효한 포인터를 설정/해제한다.
            Some(p) => unsafe { Ok(&mut *p) },
            None => Err("document 를 사용할 수 없음".to_string()),
        }
    }

    fn dom_get_element_by_id(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let id = args.first().map(to_display).unwrap_or_default();
        let dom = self.dom_arena()?;
        match dom.find_by_attr_id(&id) {
            Some(node_id) => Ok(Value::Dom(node_id)),
            None => Ok(Value::Null),
        }
    }

    // CSS 선택자로 문서/서브트리 검색 (문서 순서 DFS). 미지원 선택자는 관용:
    // querySelector → null, querySelectorAll → 빈 배열.
    fn dom_query(
        &mut self,
        scope: Option<crate::dom::NodeId>,
        sel_src: &str,
        all: bool,
    ) -> Result<Value, String> {
        let selectors = crate::css::parse_selector_list(sel_src);
        let dom = self.dom_arena()?;
        let mut out: Vec<Value> = Vec::new();
        if let Some(selectors) = selectors {
            fn rec(
                dom: &crate::dom::Dom,
                id: crate::dom::NodeId,
                selectors: &[crate::css::Selector],
                out: &mut Vec<Value>,
                all: bool,
            ) -> bool {
                if crate::style::element_matches(dom, id, selectors) {
                    out.push(Value::Dom(id));
                    if !all {
                        return true; // 첫 매칭에서 중단
                    }
                }
                dom.get(id).children.iter().any(|&c| rec(dom, c, selectors, out, all))
            }
            match scope {
                // 요소 스코프: 자손만 (자신 제외)
                Some(el) => {
                    let children = dom.get(el).children.clone();
                    children.iter().any(|&c| rec(dom, c, &selectors, &mut out, all));
                }
                None => {
                    rec(dom, dom.root, &selectors, &mut out, all);
                }
            }
        }
        if all {
            Ok(Value::Arr(Rc::new(RefCell::new(out))))
        } else {
            Ok(out.into_iter().next().unwrap_or(Value::Null))
        }
    }

    fn dom_get(&mut self, id: crate::dom::NodeId, key: &str) -> Result<Value, String> {
        let dom = self.dom_arena()?;
        match key {
            "textContent" | "innerText" => Ok(Value::Str(dom.text_content(id))),
            _ => Ok(Value::Undefined),
        }
    }

    fn dom_set(&mut self, id: crate::dom::NodeId, key: &str, value: Value) -> Result<(), String> {
        // el.onclick = fn → 핸들러 등록
        if let Some(event) = key.strip_prefix("on") {
            if matches!(value, Value::Fn(_)) {
                self.handlers.push((id, event.to_string(), value));
            }
            return Ok(());
        }
        let text = to_display(&value);
        let dom = self.dom_arena()?;
        match key {
            "textContent" | "innerText" => {
                dom.set_text_content(id, text);
                Ok(())
            }
            "innerHTML" => {
                // 조각 파싱 (관용 파서) → 자식 교체
                dom.clear_children(id);
                for tree in crate::html::parse_fragment(text) {
                    let sub = dom.insert_tree(tree, Some(id));
                    dom.get_mut(id).children.push(sub);
                }
                Ok(())
            }
            _ => Ok(()), // 미지원 프로퍼티는 조용히 무시 (관용)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &str) -> Value {
        Interp::new().run(src).unwrap()
    }

    fn run_num(src: &str) -> f64 {
        match run(src) {
            Value::Num(n) => n,
            other => panic!("expected number, got {:?}", other),
        }
    }

    fn run_str(src: &str) -> String {
        match run(src) {
            Value::Str(s) => s,
            other => panic!("expected string, got {:?}", other),
        }
    }

    fn run_bool(src: &str) -> bool {
        match run(src) {
            Value::Bool(b) => b,
            other => panic!("expected bool, got {:?}", other),
        }
    }

    #[test]
    fn arithmetic_and_precedence() {
        assert_eq!(run_num("1 + 2 * 3"), 7.0);
        assert_eq!(run_num("(1 + 2) * 3"), 9.0);
        assert_eq!(run_num("7 % 3"), 1.0);
        assert_eq!(run_num("-3 + 1"), -2.0);
    }

    #[test]
    fn string_concat_and_coercion() {
        assert_eq!(run_str("'a' + 'b'"), "ab");
        assert_eq!(run_str("'x=' + (1 + 2)"), "x=3");
        assert_eq!(run_str("1 + '2'"), "12"); // JS 의 그 동작
        assert_eq!(run_num("'3' * '4'"), 12.0);
    }

    #[test]
    fn variables_and_compound_assign() {
        assert_eq!(run_num("var x = 1; x += 3; x *= 2; x"), 8.0);
        assert_eq!(run_num("let a = 5; a - 2"), 3.0);
    }

    #[test]
    fn control_flow() {
        assert_eq!(run_num("var s = 0; for (var i = 1; i <= 10; i++) s += i; s"), 55.0);
        assert_eq!(run_num("var n = 0; while (n < 5) { n++; } n"), 5.0);
        assert_eq!(
            run_num("var s = 0; for (var i = 0; i < 10; i++) { if (i % 2) continue; if (i > 6) break; s += i; } s"),
            12.0 // 0+2+4+6
        );
        assert_eq!(run_str("if (false) 'a'; else 'b'"), "b");
    }

    #[test]
    fn functions_closures_recursion() {
        assert_eq!(run_num("function add(a, b) { return a + b; } add(2, 3)"), 5.0);
        // 클로저 카운터
        assert_eq!(
            run_num(
                "function counter() { var n = 0; return function() { n++; return n; }; } \
                 var c = counter(); c(); c(); c()"
            ),
            3.0
        );
        // 재귀 (선언 전 호출 = 호이스팅)
        assert_eq!(run_num("fib(10); function fib(n) { return n < 2 ? n : fib(n-1) + fib(n-2); } fib(10)"), 55.0);
        // 화살표 + 고차 함수
        assert_eq!(run_num("var twice = f => x => f(f(x)); twice(n => n + 3)(1)"), 7.0);
    }

    #[test]
    fn arrays_and_objects() {
        assert_eq!(run_num("var a = [1, 2, 3]; a[0] + a[2]"), 4.0);
        assert_eq!(run_num("var a = []; a.push(7); a.push(8, 9); a.length"), 3.0);
        assert_eq!(run_num("var a = [1]; a[3] = 9; a.length"), 4.0);
        assert_eq!(run_num("var o = { x: 1, y: { z: 2 } }; o.x + o.y.z"), 3.0);
        assert_eq!(run_num("var o = {}; o.k = 5; o['k'] + 1"), 6.0);
        assert_eq!(run_str("var k = 'name'; var o = {}; o[k] = 'kestrel'; o.name"), "kestrel");
    }

    #[test]
    fn equality_semantics() {
        assert!(run_bool("1 == '1'"));
        assert!(!run_bool("1 === '1'"));
        assert!(run_bool("null == undefined"));
        assert!(!run_bool("null === undefined"));
        assert!(run_bool("'b' > 'a'"));
        assert!(run_bool("typeof null === 'object'"));
        assert!(run_bool("typeof (x => x) === 'function'"));
    }

    #[test]
    fn logical_short_circuit() {
        // 우변이 평가되면 에러가 났을 것 (미정의 함수 호출)
        assert_eq!(run_num("false && boom() ? 1 : 2"), 2.0);
        assert_eq!(run_num("true || boom() ? 1 : 2"), 1.0);
        assert_eq!(run_str("'' || 'fallback'"), "fallback");
    }

    #[test]
    fn update_operators() {
        assert_eq!(run_num("var i = 5; i++"), 5.0);
        assert_eq!(run_num("var i = 5; ++i"), 6.0);
        assert_eq!(run_num("var i = 5; i--; i"), 4.0);
    }

    #[test]
    fn console_log_captures() {
        let mut it = Interp::new();
        it.run("console.log('hello', 1 + 1, [1,2], { a: 1 })").unwrap();
        assert_eq!(it.console, vec!["hello 2 1,2 [object Object]"]);
    }

    #[test]
    fn block_scoping_let() {
        assert_eq!(run_num("let x = 1; { let x = 2; } x"), 1.0);
    }

    #[test]
    fn runtime_errors() {
        assert!(Interp::new().run("undefinedVar + 1").is_err());
        assert!(Interp::new().run("null.foo").is_err());
        assert!(Interp::new().run("var x = 3; x()").is_err());
    }

    #[test]
    fn infinite_loop_is_bounded() {
        assert!(Interp::new().run("while (true) {}").is_err());
    }

    #[test]
    fn math_builtins() {
        assert_eq!(run_num("Math.floor(3.7)"), 3.0);
        assert_eq!(run_num("Math.ceil(3.1)"), 4.0);
        assert_eq!(run_num("Math.round(2.5)"), 3.0);
        assert_eq!(run_num("Math.abs(-5)"), 5.0);
        assert_eq!(run_num("Math.min(3, 1, 2)"), 1.0);
        assert_eq!(run_num("Math.max(3, 1, 2)"), 3.0);
        assert_eq!(run_num("Math.sqrt(16)"), 4.0);
        assert_eq!(run_num("Math.pow(2, 10)"), 1024.0);
        assert!(run_bool("Math.PI > 3.14 && Math.PI < 3.15"));
        assert!(run_bool("var r = Math.random(); r >= 0 && r < 1"));
        assert!(run_bool("Math.random() !== Math.random()"));
    }

    #[test]
    fn string_methods() {
        assert_eq!(run_num("'hello world'.indexOf('world')"), 6.0);
        assert_eq!(run_num("'abc'.indexOf('z')"), -1.0);
        assert_eq!(run_str("'hello'.slice(1, 3)"), "el");
        assert_eq!(run_str("'hello'.slice(-3)"), "llo");
        assert_eq!(run_str("'a,b,c'.split(',').join('|')"), "a|b|c");
        assert_eq!(run_num("'abc'.split('').length"), 3.0);
        assert_eq!(run_str("'  x  '.trim()"), "x");
        assert_eq!(run_str("'AbC'.toUpperCase()"), "ABC");
        assert_eq!(run_str("'AbC'.toLowerCase()"), "abc");
        assert_eq!(run_str("'aaa'.replace('a', 'b')"), "baa");
        assert_eq!(run_str("'hey'.charAt(1)"), "e");
        assert!(run_bool("'hello'.includes('ell')"));
        assert!(run_bool("'hello'.startsWith('he') && 'hello'.endsWith('lo')"));
        // 한글도 문자 단위로
        assert_eq!(run_str("'황조롱이'.slice(0, 2)"), "황조");
    }

    #[test]
    fn array_methods() {
        assert_eq!(run_str("[1,2,3].join('-')"), "1-2-3");
        assert_eq!(run_num("var a = [1,2,3]; a.pop(); a.length"), 2.0);
        assert_eq!(run_num("[5,6,7].indexOf(6)"), 1.0);
        assert_eq!(run_num("[1,2,3,4].slice(1, 3).length"), 2.0);
        assert_eq!(run_num("var s = 0; [1,2,3].forEach(function(x) { s += x; }); s"), 6.0);
        assert_eq!(run_str("[1,2,3].map(x => x * 10).join(',')"), "10,20,30");
        assert_eq!(run_str("[1,2,3,4,5].filter(x => x % 2).join(',')"), "1,3,5");
        assert_eq!(
            run_num("[1,2,3].map((x, i) => x + i).indexOf(5)"),
            2.0,
            "콜백 두 번째 인자 = 인덱스"
        );
    }

    #[test]
    fn json_roundtrip() {
        assert_eq!(run_num("JSON.parse('42')"), 42.0);
        assert_eq!(run_str("JSON.parse('\"hi\\\\n\"')"), "hi\n");
        assert_eq!(run_num("JSON.parse('[1, 2, 3]')[1]"), 2.0);
        assert_eq!(run_num("JSON.parse('{\"a\": {\"b\": 7}}').a.b"), 7.0);
        assert!(run_bool("JSON.parse('true') === true && JSON.parse('null') === null"));
        assert_eq!(run_str("JSON.stringify({ b: 2, a: 'x' })"), "{\"a\":\"x\",\"b\":2}");
        assert_eq!(run_str("JSON.stringify([1, 'two', null, true])"), "[1,\"two\",null,true]");
        // 라운드트립
        assert_eq!(
            run_str("JSON.stringify(JSON.parse('{\"k\":[1,2,{\"n\":null}]}'))"),
            "{\"k\":[1,2,{\"n\":null}]}"
        );
        // 파싱 실패는 스크립트 에러
        assert!(Interp::new().run("JSON.parse('{oops')").is_err());
    }

    #[test]
    fn bitwise_operators() {
        assert_eq!(run_num("5 ^ 3"), 6.0);
        assert_eq!(run_num("5 & 3"), 1.0);
        assert_eq!(run_num("5 | 2"), 7.0);
        assert_eq!(run_num("~5"), -6.0);
        assert_eq!(run_num("1 << 8"), 256.0);
        assert_eq!(run_num("-8 >> 1"), -4.0);
        assert_eq!(run_num("-1 >>> 28"), 15.0);
        assert_eq!(run_num("4294967296 | 0"), 0.0, "ToInt32 랩어라운드");
        assert_eq!(run_num("3.9 | 0"), 3.0, "| 0 절삭 관용구");
        // 우선순위: & > ^ > | , 시프트 > 비교
        assert_eq!(run_num("1 | 2 & 3"), 3.0);
        assert!(run_bool("1 << 2 > 3"));
        assert!(run_bool("(5 & 3) === 1 && true"));
    }

    #[test]
    fn template_literals() {
        assert_eq!(run_str("var x = 3; `a ${x + 1} b`"), "a 4 b");
        assert_eq!(run_str("`no interp`"), "no interp");
        assert_eq!(run_str("``"), "");
        assert_eq!(run_str("`line1\nline2`"), "line1\nline2", "리터럴 줄바꿈 허용");
        assert_eq!(run_str("`\\`tick\\` ${'and'} \\${notinterp}`"), "`tick` and ${notinterp}");
        // 보간 안에 중괄호 포함 문자열
        assert_eq!(run_str("`v=${ '{'.length }`"), "v=1");
        // 중첩 식
        assert_eq!(run_str("var f = n => n * 2; `r=${f(3) + 1}`"), "r=7");
    }

    #[test]
    fn try_catch_finally_throw() {
        assert_eq!(run_str("try { throw 'boom'; } catch (e) { 'caught ' + e }"), "caught boom");
        // throw 된 값 그대로 바인딩 (객체)
        assert_eq!(
            run_num("try { throw { code: 42 }; } catch (e) { e.code }"),
            42.0
        );
        // 네이티브 런타임 에러도 잡힘
        assert_eq!(run_str("try { undefinedVar + 1; } catch (e) { 'survived' }"), "survived");
        // finally 는 항상 실행
        assert_eq!(
            run_str("var log = ''; try { log += 'a'; throw 1; } catch (e) { log += 'b'; } finally { log += 'c'; } log"),
            "abc"
        );
        // catch 없는 try/finally: 에러 전파 + finally 실행
        assert!(Interp::new()
            .run("var x = 0; try { throw 'up'; } finally { x = 1; }")
            .is_err());
        // 바인딩 생략 catch (ES2019)
        assert_eq!(run_num("try { throw 9; } catch { 7 }"), 7.0);
        // 함수 경계 넘는 전파
        assert_eq!(
            run_str("function f() { throw 'deep'; } try { f(); } catch (e) { e }"),
            "deep"
        );
        // 스텝 한도는 잡을 수 없음
        assert!(Interp::new().run("try { while (true) {} } catch (e) { 'nope' }").is_err());
    }

    #[test]
    fn switch_statement() {
        let src = "function grade(n) { \
             switch (n) { \
               case 1: return 'one'; \
               case 2: \
               case 3: return 'few'; \
               default: return 'many'; \
             } \
           }";
        assert_eq!(run_str(&format!("{} grade(1)", src)), "one");
        assert_eq!(run_str(&format!("{} grade(2)", src)), "few", "폴스루");
        assert_eq!(run_str(&format!("{} grade(3)", src)), "few");
        assert_eq!(run_str(&format!("{} grade(99)", src)), "many");
        // break 로 탈출, 문자열 판별, 스위치 뒤 계속 실행
        assert_eq!(
            run_num("var r = 0; switch ('b') { case 'a': r = 1; break; case 'b': r = 2; break; case 'c': r = 3; } r"),
            2.0
        );
        // 엄격 비교 (1 !== '1')
        assert_eq!(
            run_num("var r = 0; switch ('1') { case 1: r = 10; break; default: r = 20; } r"),
            20.0
        );
    }

    #[test]
    fn object_method_shorthand() {
        assert_eq!(run_num("var o = { double(n) { return n * 2; } }; o.double(4)"), 8.0);
        assert_eq!(
            run_str("var api = { name: 'k', hello() { return 'hi'; }, }; api.hello() + api.name"),
            "hik"
        );
    }

    #[test]
    fn regex_literal_tolerated_and_division_intact() {
        // 정규식 리터럴이 렉서를 죽이지 않고 {source, flags} 객체가 됨
        assert_eq!(run_str("var re = /a[/]b+/gi; re.source"), "a[/]b+");
        assert_eq!(run_str("var re = /x/; re.flags !== undefined ? 'obj' : 'no'"), "obj");
        // 나눗셈은 그대로
        assert_eq!(run_num("10 / 2"), 5.0);
        assert_eq!(run_num("var a = 8; a / 2 / 2"), 2.0);
        assert_eq!(run_num("(4 + 4) / 2"), 4.0);
        assert_eq!(run_num("var x = 9; x /= 3; x"), 3.0);
        // return 뒤는 정규식 문맥
        assert_eq!(run_str("function f() { return /ok/.source; } f()"), "ok");
    }

    #[test]
    fn labeled_statements_and_labeled_break() {
        // 레이블은 파싱만 하고 무시 (break label = 일반 break)
        assert_eq!(
            run_num("var n = 0; outer: for (var i = 0; i < 3; i++) { n++; break outer; } n"),
            1.0
        );
        assert_eq!(
            run_num("var s = 0; loop: while (s < 5) { s++; continue loop; } s"),
            5.0
        );
    }

    #[test]
    fn array_holes() {
        assert_eq!(run_num("[1,,2].length"), 3.0);
        assert!(run_bool("[1,,2][1] === undefined"));
        assert_eq!(run_num("[,,].length"), 2.0);
    }

    #[test]
    fn hash_identifiers_tolerated() {
        // 클래스 미지원이지만 #priv 가 렉서를 죽이진 않음
        assert!(super::super::lexer::tokenize("obj.#priv").is_ok());
    }

    #[test]
    fn storage_and_misc_stubs() {
        // localStorage 는 실제로 동작 (페이지 수명)
        assert_eq!(
            run_str("localStorage.setItem('k', 'v1'); localStorage.getItem('k')"),
            "v1"
        );
        assert!(run_bool("localStorage.getItem('none') === null"));
        assert!(run_bool(
            "localStorage.setItem('x', 1); localStorage.removeItem('x'); localStorage.getItem('x') === null"
        ));
        // window 를 통해서도 같은 스토리지
        assert_eq!(
            run_str("window.localStorage.setItem('w', 'ok'); localStorage.getItem('w')"),
            "ok"
        );
        assert!(run_bool("typeof navigator.userAgent === 'string'"));
        // alert 는 콘솔로
        let mut it = Interp::new();
        it.run("alert('hi', 2)").unwrap();
        assert_eq!(it.console, vec!["[alert] hi 2"]);
        // window.addEventListener 는 no-op (죽지 않음)
        assert!(Interp::new().run("window.addEventListener('load', x => x)").is_ok());
    }

    #[test]
    fn location_reflects_page_url() {
        let mut it = Interp::new();
        it.install_location("https://example.com/a/b?q=1");
        let v = it.run("location.hostname + location.pathname").unwrap();
        match v {
            Value::Str(s) => assert_eq!(s, "example.com/a/b?q=1"),
            other => panic!("{:?}", other),
        }
        let w = it.run("window.location.href").unwrap();
        assert!(matches!(w, Value::Str(s) if s.starts_with("https://example.com")));
    }

    #[test]
    fn global_number_functions() {
        assert_eq!(run_num("parseInt('42px')"), 42.0);
        assert_eq!(run_num("parseInt('-7')"), -7.0);
        assert!(run_bool("isNaN(parseInt('abc'))"));
        assert_eq!(run_num("parseFloat('3.14 rad')"), 3.14);
        assert!(run_bool("isNaN('x' * 2)"));
        assert!(run_bool("!isNaN(5)"));
    }
}
