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
    body: Rc<Vec<Stmt>>, // 디슈가된 본문(모든 yield 가 문장 위치로 정규화됨)
    scope: EnvRef,       // 함수 스코프(파라미터·arguments·호이스트, 중단 사이 보존)
    started: bool,
    done: bool,
    resume: Vec<GStep>,  // 저장된 재개 경로(뿌리→잎). 비어 있으면 최상단에서 시작.
    // async 제너레이터(async function*): next/return/throw 가 Promise 를 돌려준다(§27.6).
    is_async: bool,
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
        Stmt::With { obj, body } => expr_has_yield(obj) || stmt_has_yield(body),
        // 모듈 선언은 제너레이터 본문에 올 수 없다 (최상위 전용)
        Stmt::Import { .. }
        | Stmt::ExportNamed { .. }
        | Stmt::ExportAll { .. }
        | Stmt::ExportDefault(_)
        | Stmt::ExportDecl(_) => false,
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
        Expr::Num(_) | Expr::BigInt(_) | Expr::Str(_) | Expr::Bool(_) | Expr::Null | Expr::Undefined
        | Expr::Hole
        | Expr::Ident(_) | Expr::This | Expr::Super | Expr::Regex { .. }
        | Expr::NewTarget => false,
        Expr::Array(items) => items.iter().any(expr_has_yield),
        Expr::Tagged { tag, values, .. } => {
            expr_has_yield(tag) || values.iter().any(expr_has_yield)
        }
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
    // 본문을 디슈가해 식 내부 yield(`a+(yield b)`, `f(yield x)`)를 문장 위치로 정규화한다.
    pub(super) fn make_generator(&mut self, func: Rc<JsFn>, scope: EnvRef, skip_prologue: usize) -> Value {
        let mut ctr = 0usize;
        // 파라미터 프롤로그(구조분해/기본값)는 호출 시 이미 실행됐으므로 지연 본문에서 건너뛴다.
        let src = func.body.get(skip_prologue..).unwrap_or(&[]);
        let body = Rc::new(desugar_stmts(src, &mut ctr));
        Value::Gen(Rc::new(RefCell::new(GenState {
            body,
            scope,
            started: false,
            done: false,
            resume: Vec::new(),
            is_async: func.is_async,
        })))
    }

    pub(super) fn gen_is_async(gs: &Rc<RefCell<GenState>>) -> bool {
        gs.borrow().is_async
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
        let (body, scope, resume) = {
            let mut b = gs.borrow_mut();
            b.started = true;
            (b.body.clone(), b.scope.clone(), std::mem::take(&mut b.resume))
        };
        let mut drive = Drive { resume, rpos: 0, sent: arg, saved: Vec::new(), mode };
        let flow = self.gen_list(&body, &scope, &mut drive);
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
                if let Stmt::FuncDecl { name, params, body, is_generator, is_async, source, prologue_len } = s {
                    let f = Value::Fn(Rc::new(JsFn {
                        priv_id: std::cell::Cell::new(0),
                        name: RefCell::new(name.clone()),
                        params: params.clone(),
                        body: body.clone(),
                        param_prologue_len: *prologue_len,
                        env: scope.clone(),
                        is_arrow: false,
                        is_generator: *is_generator,
                        is_async: *is_async,
                        this: None,
                        super_class: None,
                        props: RefCell::new(super::objects::ObjMap::new()),
                        source: source.clone(),
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
            Stmt::ForOf { name, iter, body, .. } => {
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
                Value::Obj(m) => super::value::enumerable_keys(m),
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
        catch: &Option<(Option<crate::js::ast::Pattern>, Vec<Stmt>)>,
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
                        let caught = match self.thrown.take() {
                            Some(v) => v,
                            None => self.error_from_msg(e),
                        };
                        let cscope = Env::new(Some(scope.clone()));
                        if let Some(p) = param {
                            self.bind_pattern(p, caught, &cscope, false)?;
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

    // GetIterator 추상연산. 반복자 프로토콜로 반복자를 얻는다: 이미 반복자(next 보유)면
    // 그대로, `[Symbol.iterator]`(@@iterator) 메서드가 있으면 호출해 얻는다(사용자 정의
    // 이터러블/인스턴스 포함). 반복 불가면 None.
    // for await 용: @@asyncIterator 를 **먼저** 찾는다 (없으면 동기 이터레이터로 폴백 — 표준).
    // 이걸 안 하면 진짜 비동기 이터러블(스트림 리더 등)에서 "반복 가능하지 않음" 이 된다.
    pub(super) fn try_get_async_iterator(&mut self, v: &Value) -> Result<Option<Value>, String> {
        let itf = self.member_get(v, "\u{0}@@asyncIterator")?;
        if is_callable(&itf) {
            return Ok(Some(self.call_value(itf, Some(v.clone()), vec![])?));
        }
        self.try_get_iterator(v)
    }

    pub(super) fn try_get_iterator(&mut self, v: &Value) -> Result<Option<Value>, String> {
        // 이미 반복자 객체(next 보유, 재료화 배열은 제외)
        if let Value::Obj(o) = v {
            let b = o.borrow();
            if b.contains_key("next") && !b.contains_key("\u{0}items") {
                return Ok(Some(v.clone()));
            }
        }
        if matches!(v, Value::Gen(_)) {
            return Ok(Some(v.clone()));
        }
        // @@iterator 메서드 호출 (배열/문자열/Set/Map/사용자 이터러블 공통)
        let itf = self.member_get(v, "\u{0}@@iterator")?;
        if is_callable(&itf) {
            return Ok(Some(self.call_value(itf, Some(v.clone()), vec![])?));
        }
        Ok(None)
    }

    // 값에서 반복자를 얻는다. 프로토콜로 안 되면(비이터러블) 재료화 시도(빈 배열 안전).
    pub(super) fn gen_get_iterator(&mut self, v: Value) -> Result<Value, String> {
        match self.try_get_iterator(&v)? {
            Some(it) => Ok(it),
            None => {
                let items = self.iterate_to_vec(&v)?;
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
        self.gen_iter_next_maybe_async(iter, sent, false)
    }

    // r#await=true 면 next() 가 돌려준 promise 를 이행값으로 푼다 (비동기 이터레이터).
    pub(super) fn gen_iter_next_maybe_async(
        &mut self,
        iter: &Value,
        sent: Value,
        await_result: bool,
    ) -> Result<(Value, bool), String> {
        let next = self.member_get(iter, "next")?;
        let r = self.call_value(next, Some(iter.clone()), vec![sent])?;
        let r = if await_result { self.await_value(r)? } else { r };
        match &r {
            Value::Obj(_) => {
                // §7.4.8 IteratorComplete: done = ToBoolean(? Get(result, "done")).
                // §7.4.9 IteratorValue: value = ? Get(result, "value"). done/value 는 접근자일
                // 수 있으므로 raw get 이 아니라 member_get 으로 호출한다 — 예전엔 raw 라 done
                // 접근자가 무시돼 무한 루프(OOM)나 value getter 예외 누락이 났다. value 는 done
                // 이어도 읽는다(yield* 의 완료값 = 위임한 반복자의 return 값에 필요).
                let done = to_bool(&self.member_get(&r, "done")?);
                let value = self.member_get(&r, "value")?;
                Ok((value, done))
            }
            _ => Ok((Value::Undefined, true)),
        }
    }
}

// ── 디슈가(desugar) 패스 ───────────────────────────────────────────────
//
// 제너레이터 본문을 실행 전에 변환해, 식 내부 어디에 있든 yield 를 문장 위치로
// 끌어올린다(임시변수 도입). 예: `return a + (yield b)` →
//   `let _l = a; let _t = yield b; return _l + _t;`
// 평가 순서와 단락평가(&&/||/?:/??)를 정확히 보존한다. 변환 후엔 모든 yield 가
// `yield e;` / `x = yield e` / `let x = yield e` 형태라 재개가능 인터프리터가
// 기존 평가기 그대로로 처리한다(연산 의미론은 손대지 않음 = 요행 없음).

// 부작용 없는 리터럴만 임시변수 없이 재사용 가능. 식별자도 중단 사이 재대입될 수
// 있어 반드시 캡처한다.
fn is_literal(e: &Expr) -> bool {
    matches!(
        e,
        Expr::Num(_) | Expr::Str(_) | Expr::Bool(_) | Expr::Null | Expr::Undefined
    )
}

fn fresh(ctr: &mut usize) -> String {
    *ctr += 1;
    format!("\u{0}g{}", ctr) // NUL 접두사 — 소스 식별자와 절대 충돌하지 않음
}

fn let_stmt(name: String, init: Expr) -> Stmt {
    Stmt::VarDecl { kind: DeclKind::Let, decls: vec![(Pattern::Name(name), Some(init))] }
}

fn let_uninit(name: String) -> Stmt {
    Stmt::VarDecl { kind: DeclKind::Let, decls: vec![(Pattern::Name(name), None)] }
}

fn assign_stmt(name: String, val: Expr) -> Stmt {
    Stmt::Expr(Expr::Assign {
        op: AssignOp::Set,
        target: Box::new(Expr::Ident(name)),
        value: Box::new(val),
    })
}

fn not_expr(e: Expr) -> Expr {
    Expr::Unary { op: UnOp::Not, expr: Box::new(e) }
}

fn eq_null(e: Expr) -> Expr {
    // `e == null` — null 과 undefined 모두 참(느슨한 동등).
    Expr::Binary { op: BinOp::EqEq, left: Box::new(e), right: Box::new(Expr::Null) }
}

// 식을 평가해 임시변수에 담고 그 식별자를 돌려준다(리터럴은 그대로).
fn capture(e: Expr, out: &mut Vec<Stmt>, ctr: &mut usize) -> Expr {
    if is_literal(&e) {
        return e;
    }
    let t = fresh(ctr);
    out.push(let_stmt(t.clone(), e));
    Expr::Ident(t)
}

fn compound_binop(op: AssignOp) -> BinOp {
    match op {
        AssignOp::Add => BinOp::Add,
        AssignOp::Sub => BinOp::Sub,
        AssignOp::Mul => BinOp::Mul,
        AssignOp::Div => BinOp::Div,
        AssignOp::Mod => BinOp::Mod,
        AssignOp::Pow => BinOp::Pow,
        AssignOp::BitAnd => BinOp::BitAnd,
        AssignOp::BitOr => BinOp::BitOr,
        AssignOp::BitXor => BinOp::BitXor,
        AssignOp::Shl => BinOp::Shl,
        AssignOp::Shr => BinOp::Shr,
        AssignOp::UShr => BinOp::UShr,
        _ => BinOp::Add, // Set/And/Or/Nullish 은 별도 처리
    }
}

// 인자 목록을 순서대로 임시변수화(스프레드 유지). yield 인자 앞의 인자도 중단 전에
// 캡처돼 순서가 보존된다.
fn flatten_args(args: &[Expr], out: &mut Vec<Stmt>, ctr: &mut usize) -> Vec<Expr> {
    args.iter()
        .map(|a| match a {
            Expr::Spread(inner) => {
                let f = flatten(inner, out, ctr);
                Expr::Spread(Box::new(capture(f, out, ctr)))
            }
            _ => {
                let f = flatten(a, out, ctr);
                capture(f, out, ctr)
            }
        })
        .collect()
}

// 식을 평탄화한다. prelude 문을 out 에 덧붙이고, yield 없는 등가 식을 돌려준다.
fn flatten(e: &Expr, out: &mut Vec<Stmt>, ctr: &mut usize) -> Expr {
    if !expr_has_yield(e) {
        return e.clone();
    }
    match e {
        Expr::Yield { star, arg } => {
            let a = arg.as_ref().map(|a| Box::new(flatten(a, out, ctr)));
            let t = fresh(ctr);
            out.push(let_stmt(t.clone(), Expr::Yield { star: *star, arg: a }));
            Expr::Ident(t)
        }
        Expr::Binary { op, left, right } => {
            let l = flatten(left, out, ctr);
            if expr_has_yield(right) {
                let lt = capture(l, out, ctr); // 오른쪽 yield 전에 왼쪽 값 확정
                let r = flatten(right, out, ctr);
                Expr::Binary { op: *op, left: Box::new(lt), right: Box::new(r) }
            } else {
                Expr::Binary { op: *op, left: Box::new(l), right: right.clone() }
            }
        }
        Expr::Logical { op, left, right } => {
            let l = flatten(left, out, ctr);
            if expr_has_yield(right) {
                // 단락평가 보존: 오른쪽(yield 포함)은 조건 만족 시에만 실행.
                let res = fresh(ctr);
                out.push(let_stmt(res.clone(), l));
                let mut inner = Vec::new();
                let r = flatten(right, &mut inner, ctr);
                inner.push(assign_stmt(res.clone(), r));
                let cond = match op {
                    LogOp::And => Expr::Ident(res.clone()),
                    LogOp::Or => not_expr(Expr::Ident(res.clone())),
                };
                out.push(Stmt::If { cond, then: inner, other: None });
                Expr::Ident(res)
            } else {
                Expr::Logical { op: *op, left: Box::new(l), right: right.clone() }
            }
        }
        Expr::Nullish { left, right } => {
            let l = flatten(left, out, ctr);
            if expr_has_yield(right) {
                let res = fresh(ctr);
                out.push(let_stmt(res.clone(), l));
                let mut inner = Vec::new();
                let r = flatten(right, &mut inner, ctr);
                inner.push(assign_stmt(res.clone(), r));
                out.push(Stmt::If { cond: eq_null(Expr::Ident(res.clone())), then: inner, other: None });
                Expr::Ident(res)
            } else {
                Expr::Nullish { left: Box::new(l), right: right.clone() }
            }
        }
        Expr::Ternary { cond, then, other } => {
            let c = flatten(cond, out, ctr);
            if expr_has_yield(then) || expr_has_yield(other) {
                let res = fresh(ctr);
                out.push(let_uninit(res.clone()));
                let mut tb = Vec::new();
                let t = flatten(then, &mut tb, ctr);
                tb.push(assign_stmt(res.clone(), t));
                let mut ob = Vec::new();
                let o = flatten(other, &mut ob, ctr);
                ob.push(assign_stmt(res.clone(), o));
                out.push(Stmt::If { cond: c, then: tb, other: Some(ob) });
                Expr::Ident(res)
            } else {
                Expr::Ternary {
                    cond: Box::new(c),
                    then: then.clone(),
                    other: other.clone(),
                }
            }
        }
        Expr::Call { callee, args } => flatten_call(callee, args, false, out, ctr),
        Expr::New { callee, args } => flatten_call(callee, args, true, out, ctr),
        Expr::OptCall { callee, args } => {
            let c = flatten(callee, out, ctr);
            if args.iter().any(expr_has_yield) {
                // fn?.(yield x): fn 이 null/undefined 면 인자 평가 없이 undefined.
                let ct = capture(c, out, ctr);
                let res = fresh(ctr);
                out.push(let_uninit(res.clone()));
                let mut elseb = Vec::new();
                let a = flatten_args(args, &mut elseb, ctr);
                elseb.push(assign_stmt(
                    res.clone(),
                    Expr::Call { callee: Box::new(ct.clone()), args: a },
                ));
                out.push(Stmt::If {
                    cond: eq_null(ct),
                    then: vec![assign_stmt(res.clone(), Expr::Undefined)],
                    other: Some(elseb),
                });
                Expr::Ident(res)
            } else {
                Expr::OptCall { callee: Box::new(c), args: args.clone() }
            }
        }
        Expr::Member { obj, prop, computed } => {
            let o = flatten(obj, out, ctr);
            if *computed && expr_has_yield(prop) {
                let ot = capture(o, out, ctr);
                let p = flatten(prop, out, ctr);
                Expr::Member { obj: Box::new(ot), prop: Box::new(p), computed: true }
            } else {
                Expr::Member { obj: Box::new(o), prop: prop.clone(), computed: *computed }
            }
        }
        Expr::OptMember { obj, prop, computed } => {
            let o = flatten(obj, out, ctr);
            if *computed && expr_has_yield(prop) {
                let ot = capture(o, out, ctr);
                let res = fresh(ctr);
                out.push(let_uninit(res.clone()));
                let mut elseb = Vec::new();
                let p = flatten(prop, &mut elseb, ctr);
                elseb.push(assign_stmt(
                    res.clone(),
                    Expr::Member { obj: Box::new(ot.clone()), prop: Box::new(p), computed: true },
                ));
                out.push(Stmt::If {
                    cond: eq_null(ot),
                    then: vec![assign_stmt(res.clone(), Expr::Undefined)],
                    other: Some(elseb),
                });
                Expr::Ident(res)
            } else {
                Expr::OptMember { obj: Box::new(o), prop: prop.clone(), computed: *computed }
            }
        }
        Expr::Array(items) => Expr::Array(
            items
                .iter()
                .map(|it| match it {
                    Expr::Spread(inner) => {
                        Expr::Spread(Box::new(capture(flatten(inner, out, ctr), out, ctr)))
                    }
                    _ => capture(flatten(it, out, ctr), out, ctr),
                })
                .collect(),
        ),
        Expr::Object(props) => Expr::Object(
            props
                .iter()
                .map(|(k, v)| {
                    let k2 = match k {
                        PropKey::Computed(ke) => {
                            PropKey::Computed(Box::new(capture(flatten(ke, out, ctr), out, ctr)))
                        }
                        other => other.clone(),
                    };
                    let v2 = capture(flatten(v, out, ctr), out, ctr);
                    (k2, v2)
                })
                .collect(),
        ),
        Expr::Assign { op, target, value } => flatten_assign(*op, target, value, out, ctr),
        Expr::Sequence(xs) => {
            let n = xs.len();
            let mut result = Expr::Undefined;
            for (i, x) in xs.iter().enumerate() {
                let fx = flatten(x, out, ctr);
                if i + 1 < n {
                    out.push(Stmt::Expr(fx)); // 부작용만, 값 버림
                } else {
                    result = fx;
                }
            }
            result
        }
        Expr::Unary { op, expr } => {
            Expr::Unary { op: *op, expr: Box::new(flatten(expr, out, ctr)) }
        }
        Expr::Update { op, prefix, target } => match &**target {
            Expr::Member { obj, prop, computed }
                if expr_has_yield(obj) || (*computed && expr_has_yield(prop)) =>
            {
                let o = capture(flatten(obj, out, ctr), out, ctr);
                let p = if *computed {
                    capture(flatten(prop, out, ctr), out, ctr)
                } else {
                    (**prop).clone()
                };
                Expr::Update {
                    op: *op,
                    prefix: *prefix,
                    target: Box::new(Expr::Member {
                        obj: Box::new(o),
                        prop: Box::new(p),
                        computed: *computed,
                    }),
                }
            }
            _ => Expr::Update { op: *op, prefix: *prefix, target: target.clone() },
        },
        Expr::Spread(inner) => Expr::Spread(Box::new(flatten(inner, out, ctr))),
        // 태그드 템플릿 안의 보간식도 yield 를 품을 수 있다 — 평가 순서를 지켜 끌어올린다
        Expr::Tagged { tag, cooked, raw, values } => Expr::Tagged {
            tag: Box::new(capture(flatten(tag, out, ctr), out, ctr)),
            cooked: cooked.clone(),
            raw: raw.clone(),
            values: values
                .iter()
                .map(|v| capture(flatten(v, out, ctr), out, ctr))
                .collect(),
        },
        Expr::Template(parts) => Expr::Template(
            parts
                .iter()
                .map(|p| match p {
                    TemplatePart::Expr(x) => {
                        TemplatePart::Expr(Box::new(capture(flatten(x, out, ctr), out, ctr)))
                    }
                    TemplatePart::Lit(s) => TemplatePart::Lit(s.clone()),
                })
                .collect(),
        ),
        Expr::Await(x) => Expr::Await(Box::new(flatten(x, out, ctr))),
        Expr::AssignPattern { pattern, value } => Expr::AssignPattern {
            pattern: pattern.clone(),
            value: Box::new(flatten(value, out, ctr)),
        },
        // 나머지(함수/클래스 등)는 자기 소관 yield 를 포함하지 않으므로 그대로.
        _ => e.clone(),
    }
}

// 호출/생성자 평탄화. 메서드 호출은 this 를 보존하려 `_f.call(_o, ...)` 로 바꾼다.
fn flatten_call(
    callee: &Expr,
    args: &[Expr],
    is_new: bool,
    out: &mut Vec<Stmt>,
    ctr: &mut usize,
) -> Expr {
    if !is_new {
        if let Expr::Member { obj, prop, computed } = callee {
            // 메서드 호출: 객체·메서드를 인자 평가 전에 캡처하고 this 를 넘긴다.
            let o = flatten(obj, out, ctr);
            let ot = capture(o, out, ctr);
            let prop2 = if *computed {
                capture(flatten(prop, out, ctr), out, ctr)
            } else {
                (**prop).clone()
            };
            let method = Expr::Member {
                obj: Box::new(ot.clone()),
                prop: Box::new(prop2),
                computed: *computed,
            };
            let ft = capture(method, out, ctr);
            let arg_temps = flatten_args(args, out, ctr);
            let mut call_args = vec![ot];
            call_args.extend(arg_temps);
            return Expr::Call {
                callee: Box::new(Expr::Member {
                    obj: Box::new(ft),
                    prop: Box::new(Expr::Str("call".to_string())),
                    computed: false,
                }),
                args: call_args,
            };
        }
    }
    let c = flatten(callee, out, ctr);
    let ct = capture(c, out, ctr);
    let arg_temps = flatten_args(args, out, ctr);
    if is_new {
        Expr::New { callee: Box::new(ct), args: arg_temps }
    } else {
        Expr::Call { callee: Box::new(ct), args: arg_temps }
    }
}

fn flatten_assign(
    op: AssignOp,
    target: &Expr,
    value: &Expr,
    out: &mut Vec<Stmt>,
    ctr: &mut usize,
) -> Expr {
    // 논리 복합대입(&&= ||= ??=)은 단락평가.
    let logical = matches!(op, AssignOp::And | AssignOp::Or | AssignOp::Nullish);
    match target {
        Expr::Ident(name) => {
            if logical {
                let cond = match op {
                    AssignOp::And => Expr::Ident(name.clone()),
                    AssignOp::Or => not_expr(Expr::Ident(name.clone())),
                    _ => eq_null(Expr::Ident(name.clone())),
                };
                let mut inner = Vec::new();
                let v = flatten(value, &mut inner, ctr);
                inner.push(assign_stmt(name.clone(), v));
                out.push(Stmt::If { cond, then: inner, other: None });
                Expr::Ident(name.clone())
            } else if matches!(op, AssignOp::Set) {
                let v = flatten(value, out, ctr);
                Expr::Assign {
                    op,
                    target: Box::new(Expr::Ident(name.clone())),
                    value: Box::new(v),
                }
            } else {
                // 복합대입: 옛 값을 yield 전에 확정.
                let old = fresh(ctr);
                out.push(let_stmt(old.clone(), Expr::Ident(name.clone())));
                let v = flatten(value, out, ctr);
                let combined = Expr::Binary {
                    op: compound_binop(op),
                    left: Box::new(Expr::Ident(old)),
                    right: Box::new(v),
                };
                Expr::Assign {
                    op: AssignOp::Set,
                    target: Box::new(Expr::Ident(name.clone())),
                    value: Box::new(combined),
                }
            }
        }
        Expr::Member { obj, prop, computed } => {
            let ot = capture(flatten(obj, out, ctr), out, ctr);
            let prop2 = if *computed {
                capture(flatten(prop, out, ctr), out, ctr)
            } else {
                (**prop).clone()
            };
            let tgt = Expr::Member {
                obj: Box::new(ot),
                prop: Box::new(prop2),
                computed: *computed,
            };
            if matches!(op, AssignOp::Set) {
                let v = flatten(value, out, ctr);
                Expr::Assign { op, target: Box::new(tgt), value: Box::new(v) }
            } else if logical {
                let cond = match op {
                    AssignOp::And => tgt.clone(),
                    AssignOp::Or => not_expr(tgt.clone()),
                    _ => eq_null(tgt.clone()),
                };
                let mut inner = Vec::new();
                let v = flatten(value, &mut inner, ctr);
                inner.push(Stmt::Expr(Expr::Assign {
                    op: AssignOp::Set,
                    target: Box::new(tgt.clone()),
                    value: Box::new(v),
                }));
                out.push(Stmt::If { cond, then: inner, other: None });
                tgt
            } else {
                let old = fresh(ctr);
                out.push(let_stmt(old.clone(), tgt.clone()));
                let v = flatten(value, out, ctr);
                let combined = Expr::Binary {
                    op: compound_binop(op),
                    left: Box::new(Expr::Ident(old)),
                    right: Box::new(v),
                };
                Expr::Assign {
                    op: AssignOp::Set,
                    target: Box::new(tgt),
                    value: Box::new(combined),
                }
            }
        }
        // 그 외 대상(구조분해 등): 값만 평탄화(최선).
        _ => {
            let v = flatten(value, out, ctr);
            Expr::Assign { op, target: Box::new(target.clone()), value: Box::new(v) }
        }
    }
}

fn desugar_stmts(stmts: &[Stmt], ctr: &mut usize) -> Vec<Stmt> {
    let mut out = Vec::new();
    for s in stmts {
        out.extend(desugar_stmt(s, ctr));
    }
    out
}

fn desugar_stmt(s: &Stmt, ctr: &mut usize) -> Vec<Stmt> {
    if !stmt_has_yield(s) {
        return vec![s.clone()]; // yield 없으면 그대로
    }
    match s {
        Stmt::Expr(Expr::Yield { star, arg }) => {
            let mut out = Vec::new();
            let a = arg.as_ref().map(|a| Box::new(flatten(a, &mut out, ctr)));
            out.push(Stmt::Expr(Expr::Yield { star: *star, arg: a }));
            out
        }
        Stmt::Expr(Expr::Assign { op: AssignOp::Set, target, value })
            if matches!(&**target, Expr::Ident(_)) && matches!(&**value, Expr::Yield { .. }) =>
        {
            // x = yield e (정규 형태 유지)
            let (star, arg) = match &**value {
                Expr::Yield { star, arg } => (*star, arg),
                _ => unreachable!(),
            };
            let mut out = Vec::new();
            let a = arg.as_ref().map(|a| Box::new(flatten(a, &mut out, ctr)));
            out.push(Stmt::Expr(Expr::Assign {
                op: AssignOp::Set,
                target: target.clone(),
                value: Box::new(Expr::Yield { star, arg: a }),
            }));
            out
        }
        Stmt::Expr(e) => {
            let mut out = Vec::new();
            let e2 = flatten(e, &mut out, ctr);
            out.push(Stmt::Expr(e2));
            out
        }
        Stmt::Return(Some(Expr::Yield { star, arg })) => {
            let mut out = Vec::new();
            let a = arg.as_ref().map(|a| Box::new(flatten(a, &mut out, ctr)));
            out.push(Stmt::Return(Some(Expr::Yield { star: *star, arg: a })));
            out
        }
        Stmt::Return(Some(e)) => {
            let mut out = Vec::new();
            let e2 = flatten(e, &mut out, ctr);
            out.push(Stmt::Return(Some(e2)));
            out
        }
        Stmt::Throw(e) => {
            let mut out = Vec::new();
            let e2 = flatten(e, &mut out, ctr);
            out.push(Stmt::Throw(e2));
            out
        }
        Stmt::VarDecl { kind, decls } => {
            let mut out = Vec::new();
            for (pat, init) in decls {
                match (pat, init) {
                    (Pattern::Name(n), Some(Expr::Yield { star, arg })) => {
                        let a = arg.as_ref().map(|a| Box::new(flatten(a, &mut out, ctr)));
                        out.push(Stmt::VarDecl {
                            kind: *kind,
                            decls: vec![(
                                Pattern::Name(n.clone()),
                                Some(Expr::Yield { star: *star, arg: a }),
                            )],
                        });
                    }
                    (_, Some(e)) => {
                        let e2 = flatten(e, &mut out, ctr);
                        out.push(Stmt::VarDecl {
                            kind: *kind,
                            decls: vec![(pat.clone(), Some(e2))],
                        });
                    }
                    (_, None) => out.push(Stmt::VarDecl {
                        kind: *kind,
                        decls: vec![(pat.clone(), None)],
                    }),
                }
            }
            out
        }
        Stmt::If { cond, then, other } => {
            let mut out = Vec::new();
            let c = flatten(cond, &mut out, ctr);
            out.push(Stmt::If {
                cond: c,
                then: desugar_stmts(then, ctr),
                other: other.as_ref().map(|o| desugar_stmts(o, ctr)),
            });
            out
        }
        Stmt::While { cond, body } => {
            let body2 = desugar_stmts(body, ctr);
            if expr_has_yield(cond) {
                // while(cond) → while(true){ <pre>; if(!cond') break; body }
                let mut nb = Vec::new();
                let c = flatten(cond, &mut nb, ctr);
                nb.push(Stmt::If { cond: not_expr(c), then: vec![Stmt::Break(None)], other: None });
                nb.extend(body2);
                vec![Stmt::While { cond: Expr::Bool(true), body: nb }]
            } else {
                vec![Stmt::While { cond: cond.clone(), body: body2 }]
            }
        }
        Stmt::DoWhile { body, cond } => {
            let mut nb = desugar_stmts(body, ctr);
            if expr_has_yield(cond) {
                let c = flatten(cond, &mut nb, ctr);
                nb.push(Stmt::If { cond: not_expr(c), then: vec![Stmt::Break(None)], other: None });
                vec![Stmt::DoWhile { body: nb, cond: Expr::Bool(true) }]
            } else {
                vec![Stmt::DoWhile { body: nb, cond: cond.clone() }]
            }
        }
        Stmt::For { init, cond, step, body } => {
            let body2 = desugar_stmts(body, ctr);
            let cond_has = cond.as_ref().map_or(false, expr_has_yield);
            let step_has = step.as_ref().map_or(false, expr_has_yield);
            let init_has = init.as_ref().map_or(false, |s| stmt_has_yield(s));
            if !cond_has && !step_has && !init_has {
                // yield 는 본문에만 — for 유지(재개가능 인터프리터가 per-iteration 처리).
                vec![Stmt::For {
                    init: init.clone(),
                    cond: cond.clone(),
                    step: step.clone(),
                    body: body2,
                }]
            } else {
                // 드묾: cond/step/init 에 yield → while(true) 로 변환.
                // { init; let first=true; while(true){ if(first)first=false else step;
                //   <cond-pre>; if(!cond') break; body } }  (continue → step 재실행 보존)
                let mut outer = Vec::new();
                if let Some(init) = init {
                    outer.extend(desugar_stmt(init, ctr));
                }
                let first = fresh(ctr);
                outer.push(let_stmt(first.clone(), Expr::Bool(true)));
                let mut wb = Vec::new();
                let mut elseb = Vec::new();
                if let Some(step) = step {
                    let s2 = flatten(step, &mut elseb, ctr);
                    elseb.push(Stmt::Expr(s2));
                }
                wb.push(Stmt::If {
                    cond: Expr::Ident(first.clone()),
                    then: vec![assign_stmt(first.clone(), Expr::Bool(false))],
                    other: Some(elseb),
                });
                if let Some(cond) = cond {
                    let c = flatten(cond, &mut wb, ctr);
                    wb.push(Stmt::If { cond: not_expr(c), then: vec![Stmt::Break(None)], other: None });
                }
                wb.extend(body2);
                outer.push(Stmt::While { cond: Expr::Bool(true), body: wb });
                vec![Stmt::Block(outer)]
            }
        }
        Stmt::ForOf { name, iter, body, is_await } => {
            let mut out = Vec::new();
            let it = flatten(iter, &mut out, ctr);
            out.push(Stmt::ForOf {
                name: name.clone(),
                iter: it,
                body: desugar_stmts(body, ctr),
                is_await: *is_await,
            });
            out
        }
        Stmt::ForIn { name, obj, body } => {
            let mut out = Vec::new();
            let o = flatten(obj, &mut out, ctr);
            out.push(Stmt::ForIn {
                name: name.clone(),
                obj: o,
                body: desugar_stmts(body, ctr),
            });
            out
        }
        Stmt::Block(stmts) => vec![Stmt::Block(desugar_stmts(stmts, ctr))],
        Stmt::Labeled(l, inner) => {
            // 레이블은 대상 문(주로 루프)에 붙여야 한다. 디슈가가 prelude+문 을 낳으면
            // 마지막 문(루프)에 레이블을 단다.
            let mut d = desugar_stmt(inner, ctr);
            if let Some(last) = d.pop() {
                d.push(Stmt::Labeled(l.clone(), Box::new(last)));
            }
            d
        }
        Stmt::Try { body, catch, finally } => vec![Stmt::Try {
            body: desugar_stmts(body, ctr),
            catch: catch.as_ref().map(|(p, b)| (p.clone(), desugar_stmts(b, ctr))),
            finally: finally.as_ref().map(|b| desugar_stmts(b, ctr)),
        }],
        Stmt::Switch { disc, cases } => {
            let mut out = Vec::new();
            let d = flatten(disc, &mut out, ctr);
            out.push(Stmt::Switch {
                disc: d,
                cases: cases
                    .iter()
                    .map(|(t, b)| (t.clone(), desugar_stmts(b, ctr)))
                    .collect(),
            });
            out
        }
        _ => vec![s.clone()],
    }
}
