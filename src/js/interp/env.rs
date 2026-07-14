// 렉시컬 환경(스코프 체인)과 호이스팅. §9.1 Environment Records.
use super::*;

// ── 환경 (스코프 체인) ──────────────────────────────────────────────

pub type EnvRef = Rc<RefCell<Env>>;

pub struct Env {
    pub(crate) vars: HashMap<String, Value>,
    // const 로 선언된 이름 (재대입 시 TypeError). 바인딩과 같은 스코프에 표시.
    pub(crate) consts: std::collections::HashSet<String>,
    parent: Option<EnvRef>,
}

impl Env {
    pub(crate) fn new(parent: Option<EnvRef>) -> EnvRef {
        Rc::new(RefCell::new(Env {
            vars: HashMap::new(),
            consts: std::collections::HashSet::new(),
            parent,
        }))
    }
}

// name 바인딩이 있는 스코프에서 그것이 const 인가 (체인 탐색, env_get 과 동일 해석).
pub(crate) fn env_is_const(env: &EnvRef, name: &str) -> bool {
    let (has, is_const, parent) = {
        let e = env.borrow();
        (e.vars.contains_key(name), e.consts.contains(name), e.parent.clone())
    };
    if has {
        return is_const;
    }
    parent.map_or(false, |p| env_is_const(&p, name))
}

// getComputedStyle 프로퍼티명: 카멜케이스 → CSS 대시. backgroundColor → background-color,
// cssFloat → float, webkitTransform → -webkit-transform. 이미 대시면 그대로.
pub(crate) fn camel_to_dashed(s: &str) -> String {
    if s == "cssFloat" || s == "styleFloat" {
        return "float".to_string();
    }
    if s.contains('-') || !s.chars().any(|c| c.is_ascii_uppercase()) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() {
            // 선두 대문자(webkit/moz/ms/o 벤더)는 앞에도 대시
            if i == 0 {
                out.push('-');
            } else {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

// 선언문이 도입하는 이름들 (export 대상 파악용)
pub(crate) fn declared_names(st: &Stmt) -> Vec<String> {
    match st {
        Stmt::FuncDecl { name, .. } => vec![name.clone()],
        Stmt::ClassDecl(c) => c.name.clone().into_iter().collect(),
        Stmt::VarDecl { decls, .. } => {
            let mut out = Vec::new();
            for (pat, _) in decls {
                pattern_names(pat, &mut out);
            }
            out
        }
        _ => Vec::new(),
    }
}

pub(crate) fn env_get(env: &EnvRef, name: &str) -> Option<Value> {
    if let Some(v) = env.borrow().vars.get(name) {
        return Some(v.clone());
    }
    let parent = env.borrow().parent.clone();
    parent.and_then(|p| env_get(&p, name))
}

// 체인에서 기존 바인딩을 갱신. 없으면 전역(최상위)에 새로 만든다 (sloppy 모드 유사).
pub(crate) fn env_set(env: &EnvRef, name: &str, value: Value) {
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

pub(crate) fn env_declare(env: &EnvRef, name: &str, value: Value) {
    env.borrow_mut().vars.insert(name.to_string(), value);
}

// 프로토타입 객체(Value::Obj)에서 키를 꺼낸다 (원시값이 자기 프로토타입 참조용).
pub(crate) fn proto_prop(proto: &Value, key: &str) -> Value {
    if let Value::Obj(m) = proto {
        return m.borrow().get(key).cloned().unwrap_or(Value::Undefined);
    }
    Value::Undefined
}

// var 하이스팅: 함수/전역 진입 시 몸통의 모든 var 이름을 undefined 로 미리 선언.
// 제어흐름 몸통(if/for/while/try/switch/block)은 파고들되, 중첩 함수 몸통은 제외
// (var 은 함수 스코프). 이미 있는 이름(파라미터 등)은 덮지 않는다.
pub(crate) fn hoist_vars(stmts: &[Stmt], scope: &EnvRef) {
    for s in stmts {
        hoist_stmt(s, scope);
    }
}


pub(crate) fn pattern_names(pat: &crate::js::ast::Pattern, out: &mut Vec<String>) {
    use crate::js::ast::Pattern;
    match pat {
        Pattern::Name(n) => out.push(n.clone()),
        Pattern::Member(_) => {} // 새 이름을 만들지 않는다 (기존 대상에 대입)
        Pattern::Object(props, rest) => {
            for (_, sub, _) in props {
                pattern_names(sub, out);
            }
            if let Some(r) = rest {
                pattern_names(r, out);
            }
        }
        Pattern::Array(elems, rest) => {
            for slot in elems.iter().flatten() {
                pattern_names(&slot.0, out);
            }
            if let Some(r) = rest {
                pattern_names(r, out);
            }
        }
    }
}

pub(crate) fn hoist_stmt(s: &Stmt, scope: &EnvRef) {
    match s {
        Stmt::VarDecl { kind: crate::js::ast::DeclKind::Var, decls } => {
            for (pat, _) in decls {
                let mut names = Vec::new();
                pattern_names(pat, &mut names);
                for n in names {
                    if !scope.borrow().vars.contains_key(&n) {
                        env_declare(scope, &n, Value::Undefined);
                    }
                }
            }
        }
        Stmt::If { then, other, .. } => {
            hoist_vars(then, scope);
            if let Some(o) = other {
                hoist_vars(o, scope);
            }
        }
        Stmt::While { body, .. }
        | Stmt::DoWhile { body, .. }
        | Stmt::Block(body)
        | Stmt::ForIn { body, .. }
        | Stmt::ForOf { body, .. } => hoist_vars(body, scope),
        Stmt::For { init, body, .. } => {
            if let Some(init) = init {
                hoist_stmt(init, scope);
            }
            hoist_vars(body, scope);
        }
        Stmt::Try { body, catch, finally } => {
            hoist_vars(body, scope);
            if let Some((_, cb)) = catch {
                hoist_vars(cb, scope);
            }
            if let Some(fb) = finally {
                hoist_vars(fb, scope);
            }
        }
        Stmt::Switch { cases, .. } => {
            for (_, body) in cases {
                hoist_vars(body, scope);
            }
        }
        _ => {} // FuncDecl/ClassDecl 몸통은 별도 스코프 → 하이스트 안 함
    }
}

// ── 값 변환 ────────────────────────────────────────────────────────
