// 지연(lazy) 제너레이터 — function* 의 중단/재개 실행.
//
// 인터프리터가 Rc 기반 재귀 트리워커라 네이티브 스택 스위칭(코루틴)을 쓸 수 없다.
// 대신 "제어흐름 위치를 저장/복원"하는 재개가능 인터프리터로 구현한다. 핵심 통찰:
// 지역변수는 이미 Rc<RefCell<Env>> 에 살아 있어 중단 사이에 자동 보존된다. 따라서
// 저장할 것은 오직 "어디까지 실행했는가"(제어흐름 경로)뿐이다.
//
// 지원 yield 위치(v1): 문장 위치(`yield e;`), 단순 대입 RHS(`x = yield e`),
// 선언 초기화(`let x = yield e`), `return yield e`, 그리고 이들이 놓인
// if/while/do-while/C-for/for-of/for-in/block/labeled/try/switch 안. yield* 위임 포함.
// 더 깊은 식 위치(`a + (yield b)`, `f(yield x)`)는 부작용 재실행 위험이 있어
// 조용히 오작동하는 대신 명확한 에러를 던진다(후속 과제).

use super::value::*;
use super::*;
use std::cell::RefCell;
use std::rc::Rc;

// 재개 지점을 가리키는 제어흐름 경로의 한 단계. 중단 시 잎→뿌리로 쌓았다가
// 뒤집어 뿌리→잎 경로로 저장하고, 재개 시 뿌리→잎으로 소비하며 재하강한다.
#[derive(Clone)]
pub(super) enum GStep {
    List(usize),                        // 문장 목록의 인덱스
    If { scope: EnvRef, branch: bool }, // if 가 만든 스코프 + 어느 분기
    While { scope: EnvRef },            // while/do-while 본문 스코프(현재 반복)
    For { cur: EnvRef, scope: EnvRef },  // C-for: 반복별 루프변수 env + 본문 스코프
    ForOf { iter: Value, scope: EnvRef }, // for-of: 지연 반복자 + 현재 본문 스코프
    ForIn { keys: Rc<Vec<String>>, idx: usize, scope: EnvRef }, // for-in: 키 목록/인덱스
    Block { scope: EnvRef },            // 블록 스코프
    Try { scope: EnvRef, part: u8 },    // 0=body / 1=catch / 2=finally
    Switch { scope: EnvRef, case: usize }, // switch 스코프 + 진입 케이스
    YieldStar { iter: Value },          // yield* 위임 중(내부 반복자 보존)
    Yield,                              // yield 잎(재개 목표)
}

// 제너레이터 인스턴스 상태. Value::Gen 이 이걸 Rc<RefCell> 로 들고 있다.
pub struct GenState {
    func: Rc<JsFn>,      // 본문/파라미터
    scope: EnvRef,       // 함수 스코프(파라미터·arguments·호이스트, 중단 사이 보존)
    started: bool,
    done: bool,
    resume: Vec<GStep>,  // 저장된 재개 경로(뿌리→잎). 비어 있으면 최상단에서 시작.
}

// next()/return()/throw() 가 재개 지점에 무엇을 주입할지.
pub(super) enum ResumeMode {
    Next,
    Return(Value),
    Throw(Value),
}

// 제너레이터 본문 실행의 제어흐름 결과.
enum GenFlow {
    Normal,
    Return(Value),
    Break(Option<String>),
    Continue(Option<String>),
    Yield(Value), // 중단됨(경로는 drive.saved 에 기록됨)
}

// yield 식 평가의 결과.
enum YieldOut {
    Suspend(Value), // yield 로 이 값을 산출하며 중단
    Resume(Value),  // 재개됨 — next(v) 로 넘어온 값(yield 식의 값)
    Return(Value),  // .return(v) 주입으로 조기 반환
}

// 한 번의 재개(next/return/throw) 동안 유지되는 구동 상태. self 에 두지 않고
// 지역으로 두어 제너레이터 재진입(중첩 gen.next)이 서로 간섭하지 않게 한다.
struct Drive {
    resume: Vec<GStep>, // 재하강할 경로(뿌리→잎)
    rpos: usize,        // 소비한 단계 수
    sent: Value,        // 재개 잎에 전달할 값
    saved: Vec<GStep>,  // 중단 시 쌓는 경로(잎→뿌리) — 끝에서 뒤집는다
    mode: ResumeMode,   // 재개 잎에서의 주입(Next/Return/Throw)
}

impl Drive {
    fn resuming(&self) -> bool {
        self.rpos < self.resume.len()
    }
    fn take_step(&mut self) -> Option<GStep> {
        if self.rpos < self.resume.len() {
            let s = self.resume[self.rpos].clone();
            self.rpos += 1;
            Some(s)
        } else {
            None
        }
    }
}

fn flow_to_genflow(f: Flow) -> GenFlow {
    match f {
        Flow::Normal(_) => GenFlow::Normal,
        Flow::Return(v) => GenFlow::Return(v),
        Flow::Break(l) => GenFlow::Break(l),
        Flow::Continue(l) => GenFlow::Continue(l),
    }
}

// 이 문장이 (이 제너레이터 소관의) yield 를 직접 포함하는가. 중첩 함수/클래스
// 본문의 yield 는 그쪽 제너레이터 것이라 세지 않는다.
fn stmt_has_yield(s: &Stmt) -> bool {
    match s {
        Stmt::VarDecl { decls, .. } => decls.iter().any(|(_, init)| {
            init.as_ref().map_or(false, expr_has_yield)
        }),
        Stmt::FuncDecl { .. } | Stmt::ClassDecl(_) | Stmt::Break(_) | Stmt::Continue(_) => false,
        Stmt::If { cond, then, other } => {
            expr_has_yield(cond)
                || then.iter().any(stmt_has_yield)
                || other.as_ref().map_or(false, |o| o.iter().any(stmt_has_yield))
        }
        Stmt::While { cond, body } | Stmt::DoWhile { body, cond } => {
            expr_has_yield(cond) || body.iter().any(stmt_has_yield)
        }
        Stmt::For { init, cond, step, body } => {
            init.as_ref().map_or(false, |s| stmt_has_yield(s))
                || cond.as_ref().map_or(false, expr_has_yield)
                || step.as_ref().map_or(false, expr_has_yield)
                || body.iter().any(stmt_has_yield)
        }
        Stmt::Return(e) => e.as_ref().map_or(false, expr_has_yield),
        Stmt::Labeled(_, inner) => stmt_has_yield(inner),
        Stmt::Block(stmts) => stmts.iter().any(stmt_has_yield),
        Stmt::Expr(e) | Stmt::Throw(e) => expr_has_yield(e),
        Stmt::Try { body, catch, finally } => {
            body.iter().any(stmt_has_yield)
                || catch.as_ref().map_or(false, |(_, b)| b.iter().any(stmt_has_yield))
                || finally.as_ref().map_or(false, |b| b.iter().any(stmt_has_yield))
        }
        Stmt::Switch { disc, cases } => {
            expr_has_yield(disc)
                || cases.iter().any(|(t, b)| {
                    t.as_ref().map_or(false, expr_has_yield) || b.iter().any(stmt_has_yield)
                })
        }
        Stmt::ForIn { obj, body, .. } => expr_has_yield(obj) || body.iter().any(stmt_has_yield),
        Stmt::ForOf { iter, body, .. } => expr_has_yield(iter) || body.iter().any(stmt_has_yield),
    }
}

fn expr_has_yield(e: &Expr) -> bool {
    match e {
        Expr::Yield { .. } => true,
        // 중첩 함수/화살표/클래스 본문의 yield 는 이 제너레이터 것이 아니다.
        Expr::Func { .. } | Expr::Class(_) => false,
        Expr::Num(_) | Expr::Str(_) | Expr::Bool(_) | Expr::Null | Expr::Undefined
        | Expr::Ident(_) | Expr::This | Expr::Super | Expr::Regex { .. } => false,
        Expr::Array(items) => items.iter().any(expr_has_yield),
        Expr::Object(props) => props.iter().any(|(k, v)| {
            expr_has_yield(v)
                || matches!(k, PropKey::Computed(ke) if expr_has_yield(ke))
        }),
        Expr::Spread(x) | Expr::Unary { expr: x, .. } | Expr::Update { target: x, .. }
        | Expr::Await(x) => expr_has_yield(x),
        Expr::Binary { left, right, .. }
        | Expr::Logical { left, right, .. }
        | Expr::Nullish { left, right } => expr_has_yield(left) || expr_has_yield(right),
        Expr::Ternary { cond, then, other } => {
            expr_has_yield(cond) || expr_has_yield(then) || expr_has_yield(other)
        }
        Expr::Assign { target, value, .. } => expr_has_yield(target) || expr_has_yield(value),
        Expr::AssignPattern { value, .. } => expr_has_yield(value),
        Expr::Member { obj, prop, computed } | Expr::OptMember { obj, prop, computed } => {
            expr_has_yield(obj) || (*computed && expr_has_yield(prop))
        }
        Expr::Call { callee, args } | Expr::OptCall { callee, args } | Expr::New { callee, args } => {
            expr_has_yield(callee) || args.iter().any(expr_has_yield)
        }
        Expr::Template(parts) => parts.iter().any(|p| match p {
            TemplatePart::Expr(x) => expr_has_yield(x),
            TemplatePart::Lit(_) => false,
        }),
        Expr::Sequence(xs) => xs.iter().any(expr_has_yield),
    }
}

const UNSUPPORTED_YIELD: &str =
    "제너레이터: 이 yield 위치는 아직 지원되지 않음(식 내부 yield)";

impl Interp {
    // function* 호출 → 지연 제너레이터 객체 생성(본문은 아직 실행 안 함).
    pub(super) fn make_generator(&mut self, func: Rc<JsFn>, scope: EnvRef) -> Value {
        Value::Gen(Rc::new(RefCell::new(GenState {
            func,
            scope,
            started: false,
            done: false,
            resume: Vec::new(),
        })))
    }

    // { value, done } 결과 객체.
    fn iter_result(&self, value: Value, done: bool) -> Value {
        let mut m = ObjMap::new();
        m.insert("value".to_string(), value);
        m.insert("done".to_string(), Value::Bool(done));
        Value::Obj(Rc::new(RefCell::new(m)))
    }

    // gen.next(v) / gen.return(v) / gen.throw(e) 진입점. { value, done } 를 돌려준다.
    pub(super) fn gen_resume(
        &mut self,
        gs: &Rc<RefCell<GenState>>,
        arg: Value,
        mode: ResumeMode,
    ) -> Result<Value, String> {
        // 이미 완료됐으면: next→{undefined,true}, return(v)→{v,true}, throw→재던짐.
        if gs.borrow().done {
            return match mode {
                ResumeMode::Return(v) => Ok(self.iter_result(v, true)),
                ResumeMode::Throw(e) => {
                    let msg = to_display(&e);
                    self.thrown = Some(e);
                    Err(msg)
                }
                ResumeMode::Next => Ok(self.iter_result(Value::Undefined, true)),
            };
        }
        let started = gs.borrow().started;
        // 시작 전 return/throw: 본문을 돌리지 않고 즉시 종료/던짐(spec).
        if !started {
            match mode {
                ResumeMode::Return(v) => {
                    gs.borrow_mut().done = true;
                    return Ok(self.iter_result(v, true));
                }
                ResumeMode::Throw(e) => {
                    gs.borrow_mut().done = true;
                    let msg = to_display(&e);
                    self.thrown = Some(e);
                    return Err(msg);
                }
                ResumeMode::Next => {}
            }
        }
        let (func, scope, resume) = {
            let mut b = gs.borrow_mut();
            b.started = true;
            (b.func.clone(), b.scope.clone(), std::mem::take(&mut b.resume))
        };
        let mut drive = Drive { resume, rpos: 0, sent: arg, saved: Vec::new(), mode };
        let flow = self.gen_list(&func.body, &scope, &mut drive);
        match flow {
            Ok(GenFlow::Yield(v)) => {
                drive.saved.reverse(); // 잎→뿌리로 쌓였으니 뒤집어 뿌리→잎
                gs.borrow_mut().resume = drive.saved;
                Ok(self.iter_result(v, false))
            }
            Ok(GenFlow::Return(v)) => {
                gs.borrow_mut().done = true;
                Ok(self.iter_result(v, true))
            }
            Ok(_) => {
                gs.borrow_mut().done = true;
                Ok(self.iter_result(Value::Undefined, true))
            }
            Err(e) => {
                gs.borrow_mut().done = true; // 예외로 종료
                Err(e)
            }
        }
    }

    // 문장 목록 실행(재개 가능). 재개 시 저장된 인덱스로 점프해 그 문장을 재개 실행하고,
    // 이후 문장은 정상(비재개)으로 이어 실행한다.
    fn gen_list(
        &mut self,
        stmts: &[Stmt],
        scope: &EnvRef,
        drive: &mut Drive,
    ) -> Result<GenFlow, String> {
        let start = if drive.resuming() {
            match drive.take_step() {
                Some(GStep::List(i)) => i,
                _ => return Err("제너레이터: 재개 경로 불일치(List)".to_string()),
            }
        } else {
            // 신규 진입: 함수 선언 호이스팅(이 목록 범위). 재개 시엔 이미 선언돼 있음.
            for s in stmts {
                if let Stmt::FuncDecl { name, params, body, is_generator, is_async } = s {
                    let f = Value::Fn(Rc::new(JsFn {
                        params: params.clone(),
                        body: body.clone(),
                        env: scope.clone(),
                        is_arrow: false,
                        is_generator: *is_generator,
                        is_async: *is_async,
                        this: None,
                        super_class: None,
                        props: RefCell::new(std::collections::HashMap::new()),
                    }));
                    env_declare(scope, name, f);
                }
            }
            0
        };
        let mut i = start;
        while i < stmts.len() {
            match self.gen_stmt(&stmts[i], scope, drive)? {
                GenFlow::Normal => i += 1,
                GenFlow::Yield(v) => {
                    drive.saved.push(GStep::List(i));
                    return Ok(GenFlow::Yield(v));
                }
                other => return Ok(other),
            }
        }
        Ok(GenFlow::Normal)
    }

    fn gen_stmt(
        &mut self,
        stmt: &Stmt,
        scope: &EnvRef,
        drive: &mut Drive,
    ) -> Result<GenFlow, String> {
        // yield 를 포함하지 않는 문장은 실제 평가기로 그대로 실행(의미론 동일 = 요행 없음).
        if !drive.resuming() && !stmt_has_yield(stmt) {
            self.tick()?;
            return Ok(flow_to_genflow(self.exec_stmt(stmt, scope)?));
        }
        self.tick()?;
        let my_label = self.pending_label.take();
        match stmt {
            // yield 를 담은 표현식 문: `yield e;` / `x = yield e;` / `yield* e;`
            Stmt::Expr(e) => self.gen_expr_stmt(e, scope, drive),
            // return [yield e]
            Stmt::Return(arg) => match arg {
                Some(Expr::Yield { star, arg: ye }) => {
                    match self.do_yield(*star, ye, scope, drive)? {
                        YieldOut::Suspend(v) => Ok(GenFlow::Yield(v)),
                        YieldOut::Resume(sent) => Ok(GenFlow::Return(sent)),
                        YieldOut::Return(v) => Ok(GenFlow::Return(v)),
                    }
                }
                _ => Err(UNSUPPORTED_YIELD.to_string()),
            },
            // let/var/const x = yield e; (단일 선언자, 단순 이름만)
            Stmt::VarDecl { kind, decls } => self.gen_var_decl(*kind, decls, scope, drive),
            Stmt::If { cond, then, other } => self.gen_if(cond, then, other, scope, drive),
            Stmt::While { cond, body } => {
                self.gen_while(cond, body, false, scope, drive, &my_label)
            }
            Stmt::DoWhile { body, cond } => {
                self.gen_while(cond, body, true, scope, drive, &my_label)
            }
            Stmt::For { init, cond, step, body } => {
                self.gen_for(init, cond, step, body, scope, drive, &my_label)
            }
            Stmt::ForOf { name, iter, body } => {
                self.gen_for_of(name, iter, body, scope, drive, &my_label)
            }
            Stmt::ForIn { name, obj, body } => {
                self.gen_for_in(name, obj, body, scope, drive, &my_label)
            }
            Stmt::Block(stmts) => self.gen_block(stmts, scope, drive),
            Stmt::Labeled(label, inner) => {
                self.pending_label = Some(label.clone());
                let r = self.gen_stmt(inner, scope, drive)?;
                self.pending_label = None;
                Ok(match r {
                    GenFlow::Break(Some(l)) if l == *label => GenFlow::Normal,
                    GenFlow::Continue(Some(l)) if l == *label => GenFlow::Normal,
                    other => other,
                })
            }
            Stmt::Try { body, catch, finally } => {
                self.gen_try(body, catch, finally, scope, drive)
            }
            Stmt::Switch { disc, cases } => {
                self.gen_switch(disc, cases, scope, drive, &my_label)
            }
            Stmt::Throw(e) => {
                // throw 인자에 yield 가 있는 경우만 여기 옴 — 미지원.
                let _ = e;
                Err(UNSUPPORTED_YIELD.to_string())
            }
            _ => Err(UNSUPPORTED_YIELD.to_string()),
        }
    }

    // `yield e;` / `yield* e;` / `x = yield e;` (x 는 단순 식별자)
    fn gen_expr_stmt(
        &mut self,
        e: &Expr,
        scope: &EnvRef,
        drive: &mut Drive,
    ) -> Result<GenFlow, String> {
        match e {
            Expr::Yield { star, arg } => match self.do_yield(*star, arg, scope, drive)? {
                YieldOut::Suspend(v) => Ok(GenFlow::Yield(v)),
                YieldOut::Resume(_) => Ok(GenFlow::Normal),
                YieldOut::Return(v) => Ok(GenFlow::Return(v)),
            },
            Expr::Assign { op: AssignOp::Set, target, value }
                if matches!(&**value, Expr::Yield { .. })
                    && matches!(&**target, Expr::Ident(_)) =>
            {
                let name = match &**target {
                    Expr::Ident(n) => n.clone(),
                    _ => unreachable!(),
                };
                let (star, ye) = match &**value {
                    Expr::Yield { star, arg } => (*star, arg),
                    _ => unreachable!(),
                };
                match self.do_yield(star, ye, scope, drive)? {
                    YieldOut::Suspend(v) => Ok(GenFlow::Yield(v)),
                    YieldOut::Resume(sent) => {
                        env_set(scope, &name, sent);
                        Ok(GenFlow::Normal)
                    }
                    YieldOut::Return(v) => Ok(GenFlow::Return(v)),
                }
            }
            _ => Err(UNSUPPORTED_YIELD.to_string()),
        }
    }

    fn gen_var_decl(
        &mut self,
        kind: DeclKind,
        decls: &[(Pattern, Option<Expr>)],
        scope: &EnvRef,
        drive: &mut Drive,
    ) -> Result<GenFlow, String> {
        // 단일 선언자 `let x = yield e` (x 는 단순 이름)만 지원. 그 외는 미지원 에러.
        if decls.len() != 1 {
            return Err(UNSUPPORTED_YIELD.to_string());
        }
        let (pat, init) = &decls[0];
        let name = match pat {
            Pattern::Name(n) => n.clone(),
            _ => return Err(UNSUPPORTED_YIELD.to_string()),
        };
        let (star, ye) = match init {
            Some(Expr::Yield { star, arg }) => (*star, arg),
            _ => return Err(UNSUPPORTED_YIELD.to_string()),
        };
        let is_var = matches!(kind, DeclKind::Var);
        let is_const = matches!(kind, DeclKind::Const);
        match self.do_yield(star, ye, scope, drive)? {
            YieldOut::Suspend(v) => Ok(GenFlow::Yield(v)),
            YieldOut::Resume(sent) => {
                self.bind_pattern(&Pattern::Name(name.clone()), sent, scope, is_var)?;
                if is_const {
                    scope.borrow_mut().consts.insert(name.clone());
                }
                Ok(GenFlow::Normal)
            }
            YieldOut::Return(v) => Ok(GenFlow::Return(v)),
        }
    }

    // yield / yield* 한 지점의 평가. 신규면 산출값과 함께 중단, 재개면 next(v) 값을 전달.
    fn do_yield(
        &mut self,
        star: bool,
        arg: &Option<Box<Expr>>,
        scope: &EnvRef,
        drive: &mut Drive,
    ) -> Result<YieldOut, String> {
        if drive.resuming() {
            let step = drive.take_step();
            // 이 재개의 주입(Next/Return/Throw)은 잎에서 한 번만 소비하고 이후엔 Next.
            let mode = std::mem::replace(&mut drive.mode, ResumeMode::Next);
            match step {
                Some(GStep::Yield) => {
                    let sent = std::mem::replace(&mut drive.sent, Value::Undefined);
                    match mode {
                        ResumeMode::Throw(e) => {
                            let msg = to_display(&e);
                            self.thrown = Some(e);
                            Err(msg)
                        }
                        ResumeMode::Return(v) => Ok(YieldOut::Return(v)),
                        ResumeMode::Next => Ok(YieldOut::Resume(sent)),
                    }
                }
                Some(GStep::YieldStar { iter }) => {
                    // 위임 재개: next(v) 를 내부 반복자로 전달. Return/Throw 주입은 근사 처리.
                    match mode {
                        ResumeMode::Return(v) => Ok(YieldOut::Return(v)),
                        ResumeMode::Throw(e) => {
                            let msg = to_display(&e);
                            self.thrown = Some(e);
                            Err(msg)
                        }
                        ResumeMode::Next => {
                            let sent = std::mem::replace(&mut drive.sent, Value::Undefined);
                            self.yield_star_step(iter, sent, drive)
                        }
                    }
                }
                _ => Err("제너레이터: 재개 경로 불일치(Yield)".to_string()),
            }
        } else if star {
            // 신규 yield* — 반복자를 얻어 첫 값을 뽑는다.
            let src = match arg {
                Some(e) => self.eval(e, scope)?,
                None => Value::Undefined,
            };
            let iter = self.gen_get_iterator(src)?;
            self.yield_star_step(iter, Value::Undefined, drive)
        } else {
            // 신규 yield — 산출값 평가 후 중단.
            let v = match arg {
                Some(e) => self.eval(e, scope)?,
                None => Value::Undefined,
            };
            drive.saved.push(GStep::Yield);
            Ok(YieldOut::Suspend(v))
        }
    }

    // yield* 한 스텝: 내부 반복자에 sent 를 넣어 next() 호출. done 이면 위임 종료(내부
    // 반환값이 yield* 의 값), 아니면 그 값을 산출하며 중단(다음 재개에 대비해 iter 보존).
    fn yield_star_step(
        &mut self,
        iter: Value,
        sent: Value,
        drive: &mut Drive,
    ) -> Result<YieldOut, String> {
        let (val, done) = self.gen_iter_next(&iter, sent)?;
        if done {
            Ok(YieldOut::Resume(val))
        } else {
            drive.saved.push(GStep::YieldStar { iter });
            Ok(YieldOut::Suspend(val))
        }
    }

    fn gen_if(
        &mut self,
        cond: &Expr,
        then: &[Stmt],
        other: &Option<Vec<Stmt>>,
        scope: &EnvRef,
        drive: &mut Drive,
    ) -> Result<GenFlow, String> {
        let (child, branch) = if drive.resuming() {
            match drive.take_step() {
                Some(GStep::If { scope, branch }) => (scope, branch),
                _ => return Err("제너레이터: 재개 경로 불일치(If)".to_string()),
            }
        } else {
            if expr_has_yield(cond) {
                return Err(UNSUPPORTED_YIELD.to_string());
            }
            let c = self.eval(cond, scope)?;
            (Env::new(Some(scope.clone())), to_bool(&c))
        };
        let empty: Vec<Stmt> = Vec::new();
        let block: &[Stmt] = if branch { then } else { other.as_deref().unwrap_or(&empty) };
        match self.gen_list(block, &child, drive)? {
            GenFlow::Yield(v) => {
                drive.saved.push(GStep::If { scope: child, branch });
                Ok(GenFlow::Yield(v))
            }
            other => Ok(other),
        }
    }

    fn gen_block(
        &mut self,
        stmts: &[Stmt],
        scope: &EnvRef,
        drive: &mut Drive,
    ) -> Result<GenFlow, String> {
        let child = if drive.resuming() {
            match drive.take_step() {
                Some(GStep::Block { scope }) => scope,
                _ => return Err("제너레이터: 재개 경로 불일치(Block)".to_string()),
            }
        } else {
            Env::new(Some(scope.clone()))
        };
        match self.gen_list(stmts, &child, drive)? {
            GenFlow::Yield(v) => {
                drive.saved.push(GStep::Block { scope: child });
                Ok(GenFlow::Yield(v))
            }
            other => Ok(other),
        }
    }

    // while / do-while (do_while=true 면 첫 반복 조건검사 생략).
    fn gen_while(
        &mut self,
        cond: &Expr,
        body: &[Stmt],
        do_while: bool,
        scope: &EnvRef,
        drive: &mut Drive,
        my_label: &Option<String>,
    ) -> Result<GenFlow, String> {
        if expr_has_yield(cond) {
            return Err(UNSUPPORTED_YIELD.to_string());
        }
        let mut resuming_body = drive.resuming();
        let mut first = true;
        loop {
            self.tick()?;
            let body_scope = if resuming_body {
                match drive.take_step() {
                    Some(GStep::While { scope }) => scope,
                    _ => return Err("제너레이터: 재개 경로 불일치(While)".to_string()),
                }
            } else {
                // 조건 검사(do-while 은 첫 반복 건너뜀).
                let check = !(do_while && first);
                if check && !to_bool(&self.eval(cond, scope)?) {
                    break;
                }
                Env::new(Some(scope.clone()))
            };
            first = false;
            let r = self.gen_list(body, &body_scope, drive)?;
            resuming_body = false;
            match r {
                GenFlow::Yield(v) => {
                    drive.saved.push(GStep::While { scope: body_scope });
                    return Ok(GenFlow::Yield(v));
                }
                GenFlow::Normal => {}
                GenFlow::Continue(l) if l.is_none() || l.as_ref() == my_label.as_ref() => {}
                GenFlow::Break(l) if l.is_none() || l.as_ref() == my_label.as_ref() => break,
                other => return Ok(other),
            }
            // do-while 은 본문 뒤 조건 재검사.
            if do_while && !to_bool(&self.eval(cond, scope)?) {
                break;
            }
        }
        Ok(GenFlow::Normal)
    }

    fn gen_for(
        &mut self,
        init: &Option<Box<Stmt>>,
        cond: &Option<Expr>,
        step: &Option<Expr>,
        body: &[Stmt],
        scope: &EnvRef,
        drive: &mut Drive,
        my_label: &Option<String>,
    ) -> Result<GenFlow, String> {
        if cond.as_ref().map_or(false, expr_has_yield)
            || step.as_ref().map_or(false, expr_has_yield)
            || init.as_ref().map_or(false, |s| stmt_has_yield(s))
        {
            return Err(UNSUPPORTED_YIELD.to_string());
        }
        // 루프변수 이름(let/const per-iteration 바인딩) 수집.
        let mut loop_vars: Vec<String> = Vec::new();
        if let Some(s) = init {
            if let Stmt::VarDecl { kind: DeclKind::Let | DeclKind::Const, decls } = &**s {
                for (pat, _) in decls {
                    pattern_names(pat, &mut loop_vars);
                }
            }
        }
        let make_iter = |src: &EnvRef, base: &EnvRef| -> EnvRef {
            if loop_vars.is_empty() {
                return src.clone();
            }
            let e = Env::new(Some(base.clone()));
            for name in &loop_vars {
                env_declare(&e, name, env_get(src, name).unwrap_or(Value::Undefined));
            }
            e
        };

        let mut resuming_body = drive.resuming();
        let mut cur: EnvRef;
        if resuming_body {
            // 재개: 저장된 cur/본문 스코프 복원, 본문 이어서 실행 후 정상 루프 계속.
            let (rc, body_scope) = match drive.take_step() {
                Some(GStep::For { cur, scope }) => (cur, scope),
                _ => return Err("제너레이터: 재개 경로 불일치(For)".to_string()),
            };
            cur = rc;
            let r = self.gen_list(body, &body_scope, drive)?;
            resuming_body = false;
            match r {
                GenFlow::Yield(v) => {
                    drive.saved.push(GStep::For { cur, scope: body_scope });
                    return Ok(GenFlow::Yield(v));
                }
                GenFlow::Normal => {}
                GenFlow::Continue(l) if l.is_none() || l.as_ref() == my_label.as_ref() => {}
                GenFlow::Break(l) if l.is_none() || l.as_ref() == my_label.as_ref() => {
                    return Ok(GenFlow::Normal)
                }
                other => return Ok(other),
            }
            // 다음 반복 준비: 값 복사 후 step.
            let next = make_iter(&cur, scope);
            if let Some(step) = step {
                self.eval(step, &next)?;
            }
            cur = next;
        } else {
            let head = Env::new(Some(scope.clone()));
            if let Some(init) = init {
                self.exec_stmt(init, &head)?;
            }
            cur = make_iter(&head, scope);
        }
        let _ = resuming_body;
        loop {
            self.tick()?;
            if let Some(cond) = cond {
                if !to_bool(&self.eval(cond, &cur)?) {
                    break;
                }
            }
            let body_scope = Env::new(Some(cur.clone()));
            let r = self.gen_list(body, &body_scope, drive)?;
            match r {
                GenFlow::Yield(v) => {
                    drive.saved.push(GStep::For { cur: cur.clone(), scope: body_scope });
                    return Ok(GenFlow::Yield(v));
                }
                GenFlow::Normal => {}
                GenFlow::Continue(l) if l.is_none() || l.as_ref() == my_label.as_ref() => {}
                GenFlow::Break(l) if l.is_none() || l.as_ref() == my_label.as_ref() => break,
                other => return Ok(other),
            }
            let next = make_iter(&cur, scope);
            if let Some(step) = step {
                self.eval(step, &next)?;
            }
            cur = next;
        }
        Ok(GenFlow::Normal)
    }

    fn gen_for_of(
        &mut self,
        name: &str,
        iter: &Expr,
        body: &[Stmt],
        scope: &EnvRef,
        drive: &mut Drive,
        my_label: &Option<String>,
    ) -> Result<GenFlow, String> {
        if expr_has_yield(iter) {
            return Err(UNSUPPORTED_YIELD.to_string());
        }
        let mut resuming_body = drive.resuming();
        let iter_obj: Value;
        if resuming_body {
            let (it, body_scope) = match drive.take_step() {
                Some(GStep::ForOf { iter, scope }) => (iter, scope),
                _ => return Err("제너레이터: 재개 경로 불일치(ForOf)".to_string()),
            };
            iter_obj = it;
            let r = self.gen_list(body, &body_scope, drive)?;
            resuming_body = false;
            match r {
                GenFlow::Yield(v) => {
                    drive.saved.push(GStep::ForOf { iter: iter_obj, scope: body_scope });
                    return Ok(GenFlow::Yield(v));
                }
                GenFlow::Normal => {}
                GenFlow::Continue(l) if l.is_none() || l.as_ref() == my_label.as_ref() => {}
                GenFlow::Break(l) if l.is_none() || l.as_ref() == my_label.as_ref() => {
                    return Ok(GenFlow::Normal)
                }
                other => return Ok(other),
            }
        } else {
            let src = self.eval(iter, scope)?;
            iter_obj = self.gen_get_iterator(src)?;
        }
        let _ = resuming_body;
        loop {
            self.tick()?;
            let (val, done) = self.gen_iter_next(&iter_obj, Value::Undefined)?;
            if done {
                break;
            }
            let body_scope = Env::new(Some(scope.clone()));
            env_declare(&body_scope, name, val);
            let r = self.gen_list(body, &body_scope, drive)?;
            match r {
                GenFlow::Yield(v) => {
                    drive.saved.push(GStep::ForOf { iter: iter_obj.clone(), scope: body_scope });
                    return Ok(GenFlow::Yield(v));
                }
                GenFlow::Normal => {}
                GenFlow::Continue(l) if l.is_none() || l.as_ref() == my_label.as_ref() => {}
                GenFlow::Break(l) if l.is_none() || l.as_ref() == my_label.as_ref() => break,
                other => return Ok(other),
            }
        }
        Ok(GenFlow::Normal)
    }

    fn gen_for_in(
        &mut self,
        name: &str,
        obj: &Expr,
        body: &[Stmt],
        scope: &EnvRef,
        drive: &mut Drive,
        my_label: &Option<String>,
    ) -> Result<GenFlow, String> {
        if expr_has_yield(obj) {
            return Err(UNSUPPORTED_YIELD.to_string());
        }
        let mut resuming_body = drive.resuming();
        let keys: Rc<Vec<String>>;
        let mut idx: usize;
        if resuming_body {
            let (k, i, body_scope) = match drive.take_step() {
                Some(GStep::ForIn { keys, idx, scope }) => (keys, idx, scope),
                _ => return Err("제너레이터: 재개 경로 불일치(ForIn)".to_string()),
            };
            keys = k;
            idx = i;
            let r = self.gen_list(body, &body_scope, drive)?;
            resuming_body = false;
            match r {
                GenFlow::Yield(v) => {
                    drive.saved.push(GStep::ForIn { keys, idx, scope: body_scope });
                    return Ok(GenFlow::Yield(v));
                }
                GenFlow::Normal => {}
                GenFlow::Continue(l) if l.is_none() || l.as_ref() == my_label.as_ref() => {}
                GenFlow::Break(l) if l.is_none() || l.as_ref() == my_label.as_ref() => {
                    return Ok(GenFlow::Normal)
                }
                other => return Ok(other),
            }
            idx += 1;
        } else {
            let target = self.eval(obj, scope)?;
            let ks: Vec<String> = match &target {
                Value::Obj(m) => m
                    .borrow()
                    .keys()
                    .filter(|k| !is_internal_key(k.as_str()))
                    .cloned()
                    .collect(),
                Value::Arr(a) => (0..a.borrow().len()).map(|i| i.to_string()).collect(),
                Value::Str(s) => (0..s.encode_utf16().count()).map(|i| i.to_string()).collect(),
                _ => Vec::new(),
            };
            keys = Rc::new(ks);
            idx = 0;
        }
        let _ = resuming_body;
        while idx < keys.len() {
            self.tick()?;
            let body_scope = Env::new(Some(scope.clone()));
            env_declare(&body_scope, name, Value::Str(keys[idx].clone()));
            let r = self.gen_list(body, &body_scope, drive)?;
            match r {
                GenFlow::Yield(v) => {
                    drive.saved.push(GStep::ForIn { keys: keys.clone(), idx, scope: body_scope });
                    return Ok(GenFlow::Yield(v));
                }
                GenFlow::Normal => {}
                GenFlow::Continue(l) if l.is_none() || l.as_ref() == my_label.as_ref() => {}
                GenFlow::Break(l) if l.is_none() || l.as_ref() == my_label.as_ref() => break,
                other => return Ok(other),
            }
            idx += 1;
        }
        Ok(GenFlow::Normal)
    }

    fn gen_try(
        &mut self,
        body: &[Stmt],
        catch: &Option<(Option<String>, Vec<Stmt>)>,
        finally: &Option<Vec<Stmt>>,
        scope: &EnvRef,
        drive: &mut Drive,
    ) -> Result<GenFlow, String> {
        // 재개 시 어느 부분(body/catch/finally)에 있었는지 복원.
        let (child, part) = if drive.resuming() {
            match drive.take_step() {
                Some(GStep::Try { scope, part }) => (scope, part),
                _ => return Err("제너레이터: 재개 경로 불일치(Try)".to_string()),
            }
        } else {
            (Env::new(Some(scope.clone())), 0u8)
        };

        // finally 부분에서 중단됐다가 재개하는 경우.
        if part == 2 {
            if let Some(fbody) = finally {
                return match self.gen_list(fbody, &child, drive)? {
                    GenFlow::Yield(v) => {
                        drive.saved.push(GStep::Try { scope: child, part: 2 });
                        Ok(GenFlow::Yield(v))
                    }
                    other => Ok(other),
                };
            }
            return Ok(GenFlow::Normal);
        }

        // body 또는 catch 실행 결과.
        let mut result: Result<GenFlow, String> = if part == 1 {
            // catch 에서 재개.
            let (_, cbody) = catch.as_ref().expect("catch");
            self.gen_list(cbody, &child, drive)
        } else {
            self.gen_list(body, &child, drive)
        };

        // body 가 중단됐으면 여기서 바로 반환(부분 저장).
        if let Ok(GenFlow::Yield(v)) = &result {
            let v = v.clone();
            let saved_part = if part == 1 { 1 } else { 0 };
            drive.saved.push(GStep::Try { scope: child, part: saved_part });
            return Ok(GenFlow::Yield(v));
        }

        // body 예외 → catch (part 0 에서 온 경우만; catch 자체 예외는 전파).
        if part == 0 {
            if let Err(e) = &result {
                if !e.starts_with(super::STEP_LIMIT_MSG) {
                    if let Some((param, cbody)) = catch {
                        let caught = self.thrown.take().unwrap_or(Value::Str(e.clone()));
                        let cscope = Env::new(Some(scope.clone()));
                        if let Some(p) = param {
                            env_declare(&cscope, p, caught);
                        }
                        result = self.gen_list(cbody, &cscope, drive);
                        if let Ok(GenFlow::Yield(v)) = &result {
                            let v = v.clone();
                            drive.saved.push(GStep::Try { scope: cscope, part: 1 });
                            return Ok(GenFlow::Yield(v));
                        }
                    }
                }
            }
        }

        // finally 는 항상 실행하며 그 제어흐름이 우선.
        if let Some(fbody) = finally {
            let fscope = Env::new(Some(scope.clone()));
            match self.gen_list(fbody, &fscope, drive)? {
                GenFlow::Normal => {}
                GenFlow::Yield(v) => {
                    drive.saved.push(GStep::Try { scope: fscope, part: 2 });
                    return Ok(GenFlow::Yield(v));
                }
                flow => return Ok(flow),
            }
        }
        result
    }

    fn gen_switch(
        &mut self,
        disc: &Expr,
        cases: &[(Option<Expr>, Vec<Stmt>)],
        scope: &EnvRef,
        drive: &mut Drive,
        my_label: &Option<String>,
    ) -> Result<GenFlow, String> {
        if expr_has_yield(disc) || cases.iter().any(|(t, _)| t.as_ref().map_or(false, expr_has_yield))
        {
            return Err(UNSUPPORTED_YIELD.to_string());
        }
        let (child, start) = if drive.resuming() {
            match drive.take_step() {
                Some(GStep::Switch { scope, case }) => (scope, case),
                _ => return Err("제너레이터: 재개 경로 불일치(Switch)".to_string()),
            }
        } else {
            let d = self.eval(disc, scope)?;
            let child = Env::new(Some(scope.clone()));
            let mut start = None;
            for (i, (test, _)) in cases.iter().enumerate() {
                if let Some(t) = test {
                    let tv = self.eval(t, &child)?;
                    if strict_eq(&d, &tv) {
                        start = Some(i);
                        break;
                    }
                }
            }
            if start.is_none() {
                start = cases.iter().position(|(t, _)| t.is_none());
            }
            match start {
                Some(s) => (child, s),
                None => return Ok(GenFlow::Normal),
            }
        };
        // start 케이스부터 폴스루. 각 케이스 본문을 재개 가능 목록으로 실행.
        for (ci, (_, stmts)) in cases.iter().enumerate().skip(start) {
            match self.gen_list(stmts, &child, drive)? {
                GenFlow::Normal => {}
                GenFlow::Yield(v) => {
                    drive.saved.push(GStep::Switch { scope: child, case: ci });
                    return Ok(GenFlow::Yield(v));
                }
                GenFlow::Break(l) if l.is_none() || l.as_ref() == my_label.as_ref() => {
                    return Ok(GenFlow::Normal)
                }
                other => return Ok(other),
            }
        }
        Ok(GenFlow::Normal)
    }

    // 값에서 반복자를 얻는다. 제너레이터/반복자 객체는 그대로(지연 유지), 그 외
    // (배열/문자열/Set/Map)는 유한하므로 재료화해 반복자로 감싼다.
    pub(super) fn gen_get_iterator(&mut self, v: Value) -> Result<Value, String> {
        match &v {
            Value::Gen(_) => Ok(v),
            Value::Obj(o) => {
                if o.borrow().contains_key("next") {
                    return Ok(v);
                }
                let it = self.member_get(&v, "@@iterator")?;
                if !matches!(it, Value::Undefined) {
                    return self.call_value(it, Some(v.clone()), vec![]);
                }
                let items = self.iterate_to_vec(&v);
                Ok(self.make_iter_from_vec(items))
            }
            _ => {
                let items = self.iterate_to_vec(&v);
                Ok(self.make_iter_from_vec(items))
            }
        }
    }

    // 반복자에 sent 를 넣어 next() 호출 → (value, done). 제너레이터면 지연 재개된다.
    pub(super) fn gen_iter_next(
        &mut self,
        iter: &Value,
        sent: Value,
    ) -> Result<(Value, bool), String> {
        let next = self.member_get(iter, "next")?;
        let r = self.call_value(next, Some(iter.clone()), vec![sent])?;
        match &r {
            Value::Obj(o) => {
                let b = o.borrow();
                Ok((
                    b.get("value").cloned().unwrap_or(Value::Undefined),
                    matches!(b.get("done"), Some(Value::Bool(true))),
                ))
            }
            _ => Ok((Value::Undefined, true)),
        }
    }
}
