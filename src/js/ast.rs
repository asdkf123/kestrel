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
    Object(Vec<(String, Expr)>),
    // function 식과 화살표 함수 공용. 화살표의 식 본문은 Return 문 하나로 desugar.
    Func { params: Vec<String>, body: Vec<Stmt> },
    Unary { op: UnOp, expr: Box<Expr> },
    Update { op: UpdOp, prefix: bool, target: Box<Expr> },
    Binary { op: BinOp, left: Box<Expr>, right: Box<Expr> },
    Logical { op: LogOp, left: Box<Expr>, right: Box<Expr> },
    Ternary { cond: Box<Expr>, then: Box<Expr>, other: Box<Expr> },
    Assign { op: AssignOp, target: Box<Expr>, value: Box<Expr> },
    Member { obj: Box<Expr>, prop: Box<Expr>, computed: bool },
    Call { callee: Box<Expr>, args: Vec<Expr> },
    // 템플릿 리터럴: 리터럴/보간 식 조각의 연결
    Template(Vec<TemplatePart>),
    // 정규식 리터럴 — 매칭 엔진 없이 {source, flags} 객체로 평가 (관용)
    Regex { source: String, flags: String },
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
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LogOp {
    And,
    Or,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UnOp {
    Neg,
    Not,
    Typeof,
    BitNot,
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
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DeclKind {
    Var,
    Let,
    Const,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    VarDecl { kind: DeclKind, name: String, init: Option<Expr> },
    FuncDecl { name: String, params: Vec<String>, body: Vec<Stmt> },
    If { cond: Expr, then: Vec<Stmt>, other: Option<Vec<Stmt>> },
    While { cond: Expr, body: Vec<Stmt> },
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
}
