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
    Func { params: Vec<String>, body: Vec<Stmt>, is_arrow: bool, is_generator: bool, is_async: bool },
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
    // 정규식 리터럴 — 매칭 엔진 없이 {source, flags} 객체로 평가 (관용)
    Regex { source: String, flags: String },
    // 콤마 연산자 (a, b, c) — 전부 평가, 마지막 값
    Sequence(Vec<Expr>),
    This,
    Super,
    New { callee: Box<Expr>, args: Vec<Expr> },
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
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClassDef {
    pub name: Option<String>,
    pub parent: Option<Box<Expr>>,
    pub ctor: Option<(Vec<String>, Vec<Stmt>)>,
    // (이름, 파라미터, 몸통)
    pub methods: Vec<(String, Vec<String>, Vec<Stmt>)>,
    pub statics: Vec<(String, Vec<String>, Vec<Stmt>)>,
    // get 접근자: 프로퍼티 접근 시 호출돼 값을 산출 (this=인스턴스)
    pub getters: Vec<(String, Vec<String>, Vec<Stmt>)>,
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
    And,  // &&=
    Or,   // ||=
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DeclKind {
    Var,
    Let,
    Const,
}

// 선언 바인딩 대상: 단순 이름 또는 구조분해 패턴
#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    Name(String),
    // { key: sub = default, ... } — 중첩 패턴과 기본값 지원
    Object(Vec<(String, Pattern, Option<Expr>)>),
    // [ sub = default, , ... ] — None = 홀(건너뜀)
    Array(Vec<Option<(Pattern, Option<Expr>)>>),
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
    Break,
    Continue,
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
    ForOf { name: String, iter: Expr, body: Vec<Stmt> },
    ClassDecl(ClassDef),
}
