// 렉시컬 환경(스코프 체인)과 호이스팅. §9.1 Environment Records.
use super::*;

// ── 환경 (스코프 체인) ──────────────────────────────────────────────

pub type EnvRef = Rc<RefCell<Env>>;

pub struct Env {
    pub(crate) vars: HashMap<String, Value>,
    // const 로 선언된 이름 (재대입 시 TypeError). 바인딩과 같은 스코프에 표시.
    pub(crate) consts: std::collections::HashSet<String>,
    // TDZ(§9.1.1.1): 하이스트됐지만 아직 초기화 안 된 let/const/class 이름.
    // 초기화 전 읽기/쓰기는 ReferenceError. 초기화(선언 실행) 시 제거된다.
    pub(crate) tdz: std::collections::HashSet<String>,
    // 객체 환경 레코드 (§9.1.1.2): with (obj) { ... } 의 obj.
    // 이 스코프에서 이름을 찾을 때 이 객체의 프로퍼티(프로토타입 체인 포함)를 본다.
    pub(crate) with_obj: Option<Value>,
    parent: Option<EnvRef>,
}

// 객체(프로토타입 체인 포함)에서 프로퍼티를 찾는다. with 의 이름 해석에 쓴다.
// 게터는 여기서 호출하지 않는다 (Accessor 는 그대로 돌려준다 — 호출부가 처리).
pub(crate) fn obj_lookup(v: &Value, name: &str) -> Option<Value> {
    match v {
        Value::Obj(m) => {
            let mut cur = Some(m.clone());
            let mut depth = 0;
            while let Some(o) = cur {
                if let Some(val) = o.borrow().get(name) {
                    return Some(val.clone());
                }
                depth += 1;
                if depth > 100 {
                    return None;
                }
                cur = match o.borrow().get("__proto__") {
                    Some(Value::Obj(p)) => Some(p.clone()),
                    _ => None,
                };
            }
            None
        }
        Value::Instance(i) => i.fields.borrow().get(name).cloned(),
        _ => None,
    }
}

impl Env {
    pub(crate) fn new(parent: Option<EnvRef>) -> EnvRef {
        Rc::new(RefCell::new(Env {
            vars: HashMap::new(),
            consts: std::collections::HashSet::new(),
            tdz: std::collections::HashSet::new(),
            with_obj: None,
            parent,
        }))
    }
}

// name 이 (그 이름을 가진 가장 안쪽 스코프에서) TDZ(미초기화 let/const/class)인가.
// vars 에 있으면 초기화된 것(false), tdz 에 있으면 미초기화(true), 둘 다 없으면
// 바깥으로. 안쪽 tdz 가 바깥 바인딩을 가린다(shadowing).
pub(crate) fn env_in_tdz(env: &EnvRef, name: &str) -> bool {
    let (has_var, in_tdz, parent) = {
        let e = env.borrow();
        (e.vars.contains_key(name), e.tdz.contains(name), e.parent.clone())
    };
    if has_var {
        return false;
    }
    if in_tdz {
        return true;
    }
    parent.map_or(false, |p| env_in_tdz(&p, name))
}

// 렉시컬 선언 이름을 TDZ 로 예약(하이스팅). 이미 초기화됐으면(vars) 건드리지 않는다.
pub(crate) fn env_declare_tdz(env: &EnvRef, name: &str) {
    let mut e = env.borrow_mut();
    if !e.vars.contains_key(name) {
        e.tdz.insert(name.to_string());
    }
}

// 블록/함수/전역 진입 시 최상위 let/const/class 를 TDZ 로 예약한다(§ Block Instantiation).
// 중첩 블록/함수 몸통은 각자의 스코프라 파고들지 않는다.
pub(crate) fn hoist_lexical(stmts: &[Stmt], env: &EnvRef) {
    use crate::js::ast::DeclKind;
    for s in stmts {
        match s {
            Stmt::VarDecl { kind: DeclKind::Let | DeclKind::Const, decls } => {
                for (pat, _) in decls {
                    let mut names = Vec::new();
                    pattern_names(pat, &mut names);
                    for n in &names {
                        env_declare_tdz(env, n);
                    }
                }
            }
            Stmt::ClassDecl(c) => {
                if let Some(n) = &c.name {
                    env_declare_tdz(env, n);
                }
            }
            _ => {}
        }
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
    // 객체 환경 레코드 (with): 이 스코프의 객체가 그 이름을 가지면 그 값이다.
    let with_obj = env.borrow().with_obj.clone();
    if let Some(o) = with_obj {
        if let Some(v) = obj_lookup(&o, name) {
            return Some(v);
        }
    }
    let parent = env.borrow().parent.clone();
    parent.and_then(|p| env_get(&p, name))
}

// with 스코프에서 이 이름이 객체 프로퍼티인가 (대입 대상 판별용).
pub(crate) fn env_with_owner(env: &EnvRef, name: &str) -> Option<Value> {
    if env.borrow().vars.contains_key(name) {
        return None; // 진짜 바인딩이 가린다
    }
    let with_obj = env.borrow().with_obj.clone();
    if let Some(o) = with_obj {
        if obj_lookup(&o, name).is_some() {
            return Some(o);
        }
    }
    let parent = env.borrow().parent.clone();
    parent.and_then(|p| env_with_owner(&p, name))
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
    let mut e = env.borrow_mut();
    e.tdz.remove(name); // 선언/초기화되면 TDZ 해제
    e.vars.insert(name.to_string(), value);
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
