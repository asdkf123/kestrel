// JS AST. 파서가 만들고 인터프리터가 순회한다.

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Num(f64),
    Str(String),
    Bool(bool),
    Null,
    Undefined,
    Ident(String),
    Array(Vec<Expr>),
    // 프로퍼티: 정적 키(식별자/문자열/숫자) 또는 계산된 키 [expr]
    Object(Vec<(PropKey, Expr)>),
    // function 식과 화살표 함수 공용. 화살표의 식 본문은 Return 문 하나로 desugar.
    // is_arrow: 화살표는 this 를 렉시컬로 캡처 (호출 시 재바인딩 안 함)
    // is_generator: function* — 호출 시 본문을 즉시 실행해 yield 값을 모아 반복자 반환(eager)
    // name: 명명 함수식 이름 (재귀용 자기 참조). 익명/화살표는 None.
    Func { name: Option<String>, params: Vec<String>, body: Vec<Stmt>, is_arrow: bool, is_generator: bool, is_async: bool },
    // BigInt 리터럴 (소스 그대로 — 평가 시 정확히 파싱)
    BigInt(String),
    // yield [*] expr — 제너레이터 본문에서 값을 산출. star 면 iterable 을 위임 전개.
    Yield { star: bool, arg: Option<Box<Expr>> },
    // ...expr — 스프레드. 배열/호출 인자/객체 리터럴에서 전개.
    Spread(Box<Expr>),
    Unary { op: UnOp, expr: Box<Expr> },
    Update { op: UpdOp, prefix: bool, target: Box<Expr> },
    Binary { op: BinOp, left: Box<Expr>, right: Box<Expr> },
    Logical { op: LogOp, left: Box<Expr>, right: Box<Expr> },
    Ternary { cond: Box<Expr>, then: Box<Expr>, other: Box<Expr> },
    Assign { op: AssignOp, target: Box<Expr>, value: Box<Expr> },
    // 구조분해 할당: [a,b]=arr / ({x,y}=o) — 기존 바인딩에 대입
    AssignPattern { pattern: Pattern, value: Box<Expr> },
    Member { obj: Box<Expr>, prop: Box<Expr>, computed: bool },
    // 옵셔널 접근: obj?.prop / obj?.[expr] — obj 가 null/undefined 면 전체 undefined
    OptMember { obj: Box<Expr>, prop: Box<Expr>, computed: bool },
    Call { callee: Box<Expr>, args: Vec<Expr> },
    // 옵셔널 호출: fn?.(args) — fn 이 null/undefined 면 undefined
    OptCall { callee: Box<Expr>, args: Vec<Expr> },
    // nullish 병합: a ?? b — a 가 null/undefined 일 때만 b
    Nullish { left: Box<Expr>, right: Box<Expr> },
    // 템플릿 리터럴: 리터럴/보간 식 조각의 연결
    Template(Vec<TemplatePart>),
    // 태그드 템플릿: tag`a${x}b` → tag(strings, x) 이고 strings.raw 가 있어야 한다.
    // (styled-components / lit-html / graphql-tag 가 전부 strings.raw 를 읽는다)
    Tagged { tag: Box<Expr>, cooked: Vec<String>, raw: Vec<String>, values: Vec<Expr> },
    // 정규식 리터럴 — 매칭 엔진 없이 {source, flags} 객체로 평가 (관용)
    Regex { source: String, flags: String },
    // 콤마 연산자 (a, b, c) — 전부 평가, 마지막 값
    Sequence(Vec<Expr>),
    This,
    Super,
    New { callee: Box<Expr>, args: Vec<Expr> },
    // new.target 메타 프로퍼티 — new 로 호출됐으면 생성자, 아니면 undefined.
    NewTarget,
    Class(Box<ClassDef>),
    // await expr — 대상이 promise 면 이행될 때까지 마이크로태스크 드레인 후 값
    Await(Box<Expr>),
}

// 객체 리터럴 프로퍼티 키. 계산된 키 { [expr]: v } 는 런타임에 평가.
#[derive(Debug, Clone, PartialEq)]
pub enum PropKey {
    Static(String),
    Computed(Box<Expr>),
    Spread, // {...obj} — value 식의 프로퍼티를 병합
    Getter(String), // { get x() {..} } — 접근 시 호출되는 접근자
    Setter(String), // { set x(v) {..} } — 대입 시 호출되는 접근자
    // { get [expr]() {..} } / { set [expr](v) {..} } — 계산된 키의 접근자. 키는 런타임 평가.
    ComputedGetter(Box<Expr>),
    ComputedSetter(Box<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClassDef {
    pub name: Option<String>,
    pub parent: Option<Box<Expr>>,
    pub ctor: Option<(Vec<String>, Vec<Stmt>)>,
    // (이름, 파라미터, 몸통, is_generator, is_async)
    pub methods: Vec<(String, Vec<String>, Vec<Stmt>, bool, bool)>,
    pub statics: Vec<(String, Vec<String>, Vec<Stmt>, bool, bool)>,
    // get 접근자: 프로퍼티 접근 시 호출돼 값을 산출 (this=인스턴스)
    pub getters: Vec<(String, Vec<String>, Vec<Stmt>)>,
    // set 접근자: 대입 시 호출 (this=인스턴스). 예전엔 파싱만 하고 버렸다 —
    // 그러면 obj.x = v 가 조용히 아무 일도 안 한다.
    pub setters: Vec<(String, Vec<String>, Vec<Stmt>)>,
    // static get/set 접근자 (this=클래스). static get observedAttributes 가 대표.
    pub static_getters: Vec<(String, Vec<String>, Vec<Stmt>)>,
    pub static_setters: Vec<(String, Vec<String>, Vec<Stmt>)>,
    // 인스턴스 필드: (이름, 초기화식) — 생성 시 this 에 설정
    pub fields: Vec<(String, Option<Expr>)>,
    // static 필드: (이름, 초기화식) — 클래스에 설정
    pub static_fields: Vec<(String, Option<Expr>)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TemplatePart {
    Lit(String),
    Expr(Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    EqEq,
    EqEqEq,
    NotEq,
    NotEqEq,
    Lt,
    Gt,
    Le,
    Ge,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    UShr,
    Instanceof,
    In,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LogOp {
    And,
    Or,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UnOp {
    Neg,
    Pos,
    Not,
    Typeof,
    BitNot,
    Void,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UpdOp {
    Inc,
    Dec,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AssignOp {
    Set,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow, // **=
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    UShr, // >>>=
    And,     // &&=
    Or,      // ||=
    Nullish, // ??=
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DeclKind {
    Var,
    Let,
    Const,
}

// 선언 바인딩 대상: 단순 이름 또는 구조분해 패턴
#[derive(Debug, Clone, PartialEq)]
pub enum PatKey {
    Static(String),
    Computed(Expr),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    Name(String),
    // 구조분해 대상이 멤버 표현식일 수 있다: [o.p, o.q] = [1, 2] / ({x: o.a} = v)
    // 표준이 허용하는 형태다. 예전엔 "잘못된 구조분해 할당 대상" 으로 파싱이 죽어서,
    // 이 패턴을 쓰는 번들(예: vue 런타임)이 통째로 안 돌았다.
    Member(Box<Expr>),
    // { key: sub = default, ..., ...rest } — 중첩/기본값/rest 지원.
    // 키는 정적 이름이거나 **계산된 키**다: let { [ex]: v } = o (ES6).
    // 예전엔 정적 이름만 받아서, 계산된 키를 쓰는 번들이 파싱에서 통째로 죽었다.
    Object(Vec<(PatKey, Pattern, Option<Expr>)>, Option<String>),
    // [ sub = default, , ..., ...rest ] — None = 홀(건너뜀)
    Array(Vec<Option<(Pattern, Option<Expr>)>>, Option<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    // 다중 선언자 지원: var a = 1, b = 2; / 구조분해 const {a} = o
    VarDecl { kind: DeclKind, decls: Vec<(Pattern, Option<Expr>)> },
    FuncDecl { name: String, params: Vec<String>, body: Vec<Stmt>, is_generator: bool, is_async: bool },
    If { cond: Expr, then: Vec<Stmt>, other: Option<Vec<Stmt>> },
    While { cond: Expr, body: Vec<Stmt> },
    DoWhile { body: Vec<Stmt>, cond: Expr },
    For { init: Option<Box<Stmt>>, cond: Option<Expr>, step: Option<Expr>, body: Vec<Stmt> },
    Return(Option<Expr>),
    // break/continue 의 선택적 레이블 (break outer;). None 은 가장 안쪽 루프/스위치.
    Break(Option<String>),
    Continue(Option<String>),
    // 레이블 붙은 문 (outer: for(...)). break/continue 가 이 레이블을 지목할 수 있다.
    Labeled(String, Box<Stmt>),
    Block(Vec<Stmt>),
    Expr(Expr),
    Throw(Expr),
    // catch: (바인딩 이름 — ES2019 생략 가능, 몸통)
    Try {
        body: Vec<Stmt>,
        catch: Option<(Option<String>, Vec<Stmt>)>,
        finally: Option<Vec<Stmt>>,
    },
    // cases: (판별식 — None 은 default, 문 목록). 폴스루 의미론.
    Switch { disc: Expr, cases: Vec<(Option<Expr>, Vec<Stmt>)> },
    // for (k in obj) — 객체 키 / 배열 인덱스 순회
    ForIn { name: String, obj: Expr, body: Vec<Stmt> },
    // for (v of iterable) — 값 순회 (배열/문자열/Set/Map)
    // is_await: `for await (const x of asyncIterable)` — 각 값이 promise 면 언랩한다 (ES2018).
    ForOf { name: String, iter: Expr, body: Vec<Stmt>, is_await: bool },
    ClassDecl(ClassDef),

    // ── ES 모듈 (import/export) ──
    // 예전엔 파서가 import 를 통째로 버리고 export 는 수식어만 벗겼다.
    // 그러면 모듈의 의존성이 사라져서 실행하면 전부 undefined 다 —
    // "스크립트는 돌았는데 화면이 비었다"가 된다.
    Import { specs: Vec<ImportSpec>, source: String },
    // export { a, b as c } [from '...']
    ExportNamed { specs: Vec<(String, String)>, source: Option<String> },
    // export * from '...'
    ExportAll { source: String },
    // export default <식|함수|클래스>
    ExportDefault(Box<Stmt>),
    // export const/let/var/function/class …
    ExportDecl(Box<Stmt>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ImportSpec {
    Default(String),        // import x from 'm'
    Named(String, String),  // import { a as b } from 'm' → (a, b)
    Namespace(String),      // import * as ns from 'm'
}
