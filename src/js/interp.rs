// 트리 워킹 인터프리터. Value/Env(렉시컬 체인)/제어 흐름.
// 무한 루프로 브라우저가 멈추지 않도록 실행 스텝 한도를 둔다.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use super::ast::*;
use super::parser::parse;

const STEP_LIMIT: u64 = 5_000_000;

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
    // DOM 요소 핸들: 루트로부터의 자식 인덱스 경로 (M4a 스펙)
    Dom(Rc<Vec<usize>>),
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
    // JS4 에서 DOM 바인딩이 사용 (getElementById / textContent)
    pub dom: Option<*mut crate::dom::Node>,
}

impl Interp {
    pub fn new() -> Interp {
        let global = Env::new(None);
        // console.log
        let mut console = HashMap::new();
        console.insert("log".to_string(), Value::Native(Native::ConsoleLog));
        env_declare(&global, "console", Value::Obj(Rc::new(RefCell::new(console))));
        Interp { global, console: Vec::new(), steps: 0, dom: None }
    }

    pub fn run(&mut self, src: &str) -> Result<Value, String> {
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
            return Err("실행 한도 초과 (무한 루프?)".to_string());
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
            Expr::Unary { op, expr } => {
                let v = self.eval(expr, env)?;
                Ok(match op {
                    UnOp::Neg => Value::Num(-to_num(&v)),
                    UnOp::Not => Value::Bool(!to_bool(&v)),
                    UnOp::Typeof => Value::Str(type_of(&v).to_string()),
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
                if let Ok(i) = key.parse::<usize>() {
                    return Ok(a.borrow().get(i).cloned().unwrap_or(Value::Undefined));
                }
                Ok(Value::Undefined)
            }
            Value::Str(s) => {
                if key == "length" {
                    return Ok(Value::Num(s.chars().count() as f64));
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
            Value::Dom(path) => self.dom_get(path, key),
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
                    Value::Dom(path) => self.dom_set(&path, &key, value),
                    other => Err(format!("{} 에 할당할 수 없음", to_display(&other))),
                }
            }
            _ => Err("할당 대상이 아님".to_string()),
        }
    }

    // ── DOM 바인딩 (JS4 에서 dom 포인터가 설정됨; 미설정 시 에러) ──

    fn dom_root(&mut self) -> Result<&mut crate::dom::Node, String> {
        match self.dom {
            // 안전성: run_scripts 가 실행 동안에만 유효한 포인터를 설정/해제한다.
            Some(p) => unsafe { Ok(&mut *p) },
            None => Err("document 를 사용할 수 없음".to_string()),
        }
    }

    fn dom_get_element_by_id(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let id = args.first().map(to_display).unwrap_or_default();
        let root = self.dom_root()?;
        fn find(node: &crate::dom::Node, id: &str, path: &mut Vec<usize>) -> Option<Vec<usize>> {
            if let crate::dom::NodeType::Element(e) = &node.node_type {
                if e.attributes.get("id").map(|s| s.as_str()) == Some(id) {
                    return Some(path.clone());
                }
            }
            for (i, c) in node.children.iter().enumerate() {
                path.push(i);
                if let Some(found) = find(c, id, path) {
                    return Some(found);
                }
                path.pop();
            }
            None
        }
        match find(root, &id, &mut Vec::new()) {
            Some(path) => Ok(Value::Dom(Rc::new(path))),
            None => Ok(Value::Null),
        }
    }

    fn dom_node<'a>(
        root: &'a mut crate::dom::Node,
        path: &[usize],
    ) -> Result<&'a mut crate::dom::Node, String> {
        let mut cur = root;
        for &i in path {
            cur = cur.children.get_mut(i).ok_or("유효하지 않은 DOM 핸들")?;
        }
        Ok(cur)
    }

    fn dom_get(&mut self, path: &[usize], key: &str) -> Result<Value, String> {
        let root = self.dom_root()?;
        let node = Self::dom_node(root, path)?;
        match key {
            "textContent" | "innerText" => {
                fn collect(n: &crate::dom::Node, out: &mut String) {
                    if let crate::dom::NodeType::Text(t) = &n.node_type {
                        out.push_str(t);
                    }
                    for c in &n.children {
                        collect(c, out);
                    }
                }
                let mut s = String::new();
                collect(node, &mut s);
                Ok(Value::Str(s))
            }
            _ => Ok(Value::Undefined),
        }
    }

    fn dom_set(&mut self, path: &[usize], key: &str, value: Value) -> Result<(), String> {
        let text = to_display(&value);
        let root = self.dom_root()?;
        let node = Self::dom_node(root, path)?;
        match key {
            "textContent" | "innerText" => {
                node.children = vec![crate::dom::text(text)];
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
}
