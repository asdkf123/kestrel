// JS 파서: 토큰열 → AST. 식은 우선순위 등반, 문은 재귀 하강.
// 세미콜론은 있으면 소비, 없어도 관용 (단순화된 ASI).

use super::ast::*;
use super::lexer::{tokenize, Tok, TplPart};

// 템플릿 보간 ${...} 소스를 독립적으로 식 파싱
fn parse_expr_source(src: &str) -> Result<Expr, String> {
    let toks = tokenize(src)?;
    let mut p = Parser { toks, pos: 0 };
    let e = p.expr()?;
    if !p.eof() {
        return Err("템플릿 보간 식 뒤에 잉여 토큰".to_string());
    }
    Ok(e)
}

pub fn parse(src: &str) -> Result<Vec<Stmt>, String> {
    let toks = tokenize(src)?;
    let mut p = Parser { toks, pos: 0 };
    let mut stmts = Vec::new();
    while !p.eof() {
        stmts.push(p.stmt()?);
    }
    Ok(stmts)
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn eof(&self) -> bool {
        self.pos >= self.toks.len()
    }

    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn next(&mut self) -> Result<Tok, String> {
        let t = self.toks.get(self.pos).cloned().ok_or("소스가 갑자기 끝남")?;
        self.pos += 1;
        Ok(t)
    }

    fn eat(&mut self, t: &Tok) -> bool {
        if self.peek() == Some(t) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, t: &Tok) -> Result<(), String> {
        if self.eat(t) {
            Ok(())
        } else {
            Err(format!("{:?} 이 필요한데 {:?}{}", t, self.peek(), self.ctx()))
        }
    }

    // 에러 진단용: 현재 위치 주변 토큰 덤프 (필드 로그에서 원인 규명)
    fn ctx(&self) -> String {
        let lo = self.pos.saturating_sub(4);
        let hi = (self.pos + 3).min(self.toks.len());
        format!(" (토큰 {} 근처: {:?})", self.pos, &self.toks[lo..hi])
    }

    fn ident(&mut self) -> Result<String, String> {
        match self.next()? {
            Tok::Ident(s) => Ok(s),
            other => Err(format!("식별자가 필요한데 {:?}{}", other, self.ctx())),
        }
    }

    // break/continue 뒤의 레이블: 다음이 문 종결이면 레이블로 보고 소비 (무시)
    fn eat_label(&mut self) {
        if matches!(self.peek(), Some(Tok::Ident(_)))
            && matches!(
                self.toks.get(self.pos + 1),
                Some(Tok::Semi) | Some(Tok::RBrace) | Some(Tok::Case) | Some(Tok::Default) | None
            )
        {
            self.pos += 1;
        }
    }

    // ── 문 ──────────────────────────────────────────────────────────

    fn stmt(&mut self) -> Result<Stmt, String> {
        match self.peek() {
            Some(Tok::Var) | Some(Tok::Let) | Some(Tok::Const) => self.var_decl(),
            Some(Tok::Function) => self.func_decl(),
            Some(Tok::If) => self.if_stmt(),
            Some(Tok::While) => self.while_stmt(),
            Some(Tok::For) => self.for_stmt(),
            Some(Tok::Return) => {
                self.pos += 1;
                let value = if self.peek() == Some(&Tok::Semi)
                    || self.peek() == Some(&Tok::RBrace)
                    || self.eof()
                {
                    None
                } else {
                    Some(self.expr()?)
                };
                self.eat(&Tok::Semi);
                Ok(Stmt::Return(value))
            }
            Some(Tok::Break) => {
                self.pos += 1;
                self.eat_label();
                self.eat(&Tok::Semi);
                Ok(Stmt::Break)
            }
            Some(Tok::Continue) => {
                self.pos += 1;
                self.eat_label();
                self.eat(&Tok::Semi);
                Ok(Stmt::Continue)
            }
            // 레이블 문 (foo: stmt) — 레이블은 파싱만 하고 버림.
            // break/continue 의 레이블도 무시되므로 다중 중첩 탈출 의미는 다를 수 있음 (관용)
            Some(Tok::Ident(_)) if self.toks.get(self.pos + 1) == Some(&Tok::Colon) => {
                self.pos += 2;
                self.stmt()
            }
            Some(Tok::Throw) => {
                self.pos += 1;
                let e = self.expr()?;
                self.eat(&Tok::Semi);
                Ok(Stmt::Throw(e))
            }
            Some(Tok::Try) => self.try_stmt(),
            Some(Tok::Switch) => self.switch_stmt(),
            Some(Tok::LBrace) => Ok(Stmt::Block(self.block()?)),
            Some(Tok::Semi) => {
                self.pos += 1;
                Ok(Stmt::Block(Vec::new())) // 빈 문
            }
            _ => {
                let e = self.expr()?;
                self.eat(&Tok::Semi);
                Ok(Stmt::Expr(e))
            }
        }
    }

    fn block(&mut self) -> Result<Vec<Stmt>, String> {
        self.expect(&Tok::LBrace)?;
        let mut stmts = Vec::new();
        while self.peek() != Some(&Tok::RBrace) {
            if self.eof() {
                return Err("닫히지 않은 블록".to_string());
            }
            stmts.push(self.stmt()?);
        }
        self.pos += 1; // '}'
        Ok(stmts)
    }

    fn var_decl(&mut self) -> Result<Stmt, String> {
        let kind = match self.next()? {
            Tok::Var => DeclKind::Var,
            Tok::Let => DeclKind::Let,
            _ => DeclKind::Const,
        };
        // 다중 선언자: var a = 1, b, c = 3;  (초기화식은 콤마 연산자 미포함 → assignment)
        let mut decls = Vec::new();
        loop {
            let name = self.ident()?;
            let init = if self.eat(&Tok::Assign) { Some(self.assignment()?) } else { None };
            decls.push((name, init));
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.eat(&Tok::Semi);
        Ok(Stmt::VarDecl { kind, decls })
    }

    fn func_decl(&mut self) -> Result<Stmt, String> {
        self.expect(&Tok::Function)?;
        let name = self.ident()?;
        let params = self.param_list()?;
        let body = self.block()?;
        Ok(Stmt::FuncDecl { name, params, body })
    }

    fn param_list(&mut self) -> Result<Vec<String>, String> {
        self.expect(&Tok::LParen)?;
        let mut params = Vec::new();
        if self.eat(&Tok::RParen) {
            return Ok(params);
        }
        loop {
            params.push(self.ident()?);
            if self.eat(&Tok::Comma) {
                continue;
            }
            self.expect(&Tok::RParen)?;
            break;
        }
        Ok(params)
    }

    fn if_stmt(&mut self) -> Result<Stmt, String> {
        self.expect(&Tok::If)?;
        self.expect(&Tok::LParen)?;
        let cond = self.expr()?;
        self.expect(&Tok::RParen)?;
        let then = self.body_of_clause()?;
        let other = if self.eat(&Tok::Else) {
            Some(if self.peek() == Some(&Tok::If) {
                vec![self.if_stmt()?] // else if 체인
            } else {
                self.body_of_clause()?
            })
        } else {
            None
        };
        Ok(Stmt::If { cond, then, other })
    }

    fn while_stmt(&mut self) -> Result<Stmt, String> {
        self.expect(&Tok::While)?;
        self.expect(&Tok::LParen)?;
        let cond = self.expr()?;
        self.expect(&Tok::RParen)?;
        Ok(Stmt::While { cond, body: self.body_of_clause()? })
    }

    fn for_stmt(&mut self) -> Result<Stmt, String> {
        self.expect(&Tok::For)?;
        self.expect(&Tok::LParen)?;
        // for (k in obj) / for (var k in obj)
        let is_decl_in = matches!(self.peek(), Some(Tok::Var | Tok::Let | Tok::Const))
            && matches!(self.toks.get(self.pos + 1), Some(Tok::Ident(_)))
            && self.toks.get(self.pos + 2) == Some(&Tok::In);
        let is_bare_in = matches!(self.peek(), Some(Tok::Ident(_)))
            && self.toks.get(self.pos + 1) == Some(&Tok::In);
        if is_decl_in || is_bare_in {
            if is_decl_in {
                self.pos += 1; // var/let/const
            }
            let name = self.ident()?;
            self.expect(&Tok::In)?;
            let obj = self.expr()?;
            self.expect(&Tok::RParen)?;
            return Ok(Stmt::ForIn { name, obj, body: self.body_of_clause()? });
        }
        let init = if self.eat(&Tok::Semi) {
            None
        } else {
            // var_decl/식문 모두 자체적으로 ';' 소비
            Some(Box::new(match self.peek() {
                Some(Tok::Var) | Some(Tok::Let) | Some(Tok::Const) => self.var_decl()?,
                _ => {
                    let e = self.expr()?;
                    self.expect(&Tok::Semi)?;
                    Stmt::Expr(e)
                }
            }))
        };
        let cond = if self.peek() == Some(&Tok::Semi) { None } else { Some(self.expr()?) };
        self.expect(&Tok::Semi)?;
        let step = if self.peek() == Some(&Tok::RParen) { None } else { Some(self.expr()?) };
        self.expect(&Tok::RParen)?;
        Ok(Stmt::For { init, cond, step, body: self.body_of_clause()? })
    }

    fn try_stmt(&mut self) -> Result<Stmt, String> {
        self.expect(&Tok::Try)?;
        let body = self.block()?;
        let catch = if self.eat(&Tok::Catch) {
            let param = if self.eat(&Tok::LParen) {
                let p = self.ident()?;
                self.expect(&Tok::RParen)?;
                Some(p)
            } else {
                None // ES2019: catch { } 바인딩 생략
            };
            Some((param, self.block()?))
        } else {
            None
        };
        let finally = if self.eat(&Tok::Finally) { Some(self.block()?) } else { None };
        if catch.is_none() && finally.is_none() {
            return Err("try 에는 catch 나 finally 가 필요".to_string());
        }
        Ok(Stmt::Try { body, catch, finally })
    }

    fn switch_stmt(&mut self) -> Result<Stmt, String> {
        self.expect(&Tok::Switch)?;
        self.expect(&Tok::LParen)?;
        let disc = self.expr()?;
        self.expect(&Tok::RParen)?;
        self.expect(&Tok::LBrace)?;
        let mut cases: Vec<(Option<Expr>, Vec<Stmt>)> = Vec::new();
        loop {
            match self.peek() {
                Some(Tok::RBrace) => {
                    self.pos += 1;
                    break;
                }
                Some(Tok::Case) => {
                    self.pos += 1;
                    let test = self.expr()?;
                    self.expect(&Tok::Colon)?;
                    cases.push((Some(test), Vec::new()));
                }
                Some(Tok::Default) => {
                    self.pos += 1;
                    self.expect(&Tok::Colon)?;
                    cases.push((None, Vec::new()));
                }
                None => return Err("닫히지 않은 switch".to_string()),
                _ => {
                    let stmt = self.stmt()?;
                    match cases.last_mut() {
                        Some((_, stmts)) => stmts.push(stmt),
                        None => return Err("switch 안 문은 case 뒤에 와야 함".to_string()),
                    }
                }
            }
        }
        Ok(Stmt::Switch { disc, cases })
    }

    // if/while/for 의 본문: 블록 또는 단일 문
    fn body_of_clause(&mut self) -> Result<Vec<Stmt>, String> {
        if self.peek() == Some(&Tok::LBrace) {
            self.block()
        } else {
            Ok(vec![self.stmt()?])
        }
    }

    // ── 식 (우선순위 낮은 → 높은) ───────────────────────────────────

    // 콤마 연산자 (최저 우선순위): a = 1, b = 2 → 전부 평가, 마지막 값.
    // 인자 목록/배열/객체/삼항 가지는 assignment 를 직접 쓰므로 영향 없음 (JS 동일)
    fn expr(&mut self) -> Result<Expr, String> {
        let first = self.assignment()?;
        if self.peek() != Some(&Tok::Comma) {
            return Ok(first);
        }
        let mut items = vec![first];
        while self.eat(&Tok::Comma) {
            items.push(self.assignment()?);
        }
        Ok(Expr::Sequence(items))
    }

    fn assignment(&mut self) -> Result<Expr, String> {
        // 화살표 함수 먼저 시도 (백트래킹)
        if let Some(f) = self.try_arrow()? {
            return Ok(f);
        }
        let left = self.ternary()?;
        let op = match self.peek() {
            Some(Tok::Assign) => Some(AssignOp::Set),
            Some(Tok::PlusAssign) => Some(AssignOp::Add),
            Some(Tok::MinusAssign) => Some(AssignOp::Sub),
            Some(Tok::StarAssign) => Some(AssignOp::Mul),
            Some(Tok::SlashAssign) => Some(AssignOp::Div),
            _ => None,
        };
        if let Some(op) = op {
            self.pos += 1;
            if !matches!(left, Expr::Ident(_) | Expr::Member { .. }) {
                return Err("할당 대상이 아님".to_string());
            }
            let value = self.assignment()?; // 우결합
            return Ok(Expr::Assign { op, target: Box::new(left), value: Box::new(value) });
        }
        Ok(left)
    }

    // `x => ...` / `(a, b) => ...`. 화살표가 아니면 위치를 되돌리고 None.
    fn try_arrow(&mut self) -> Result<Option<Expr>, String> {
        let save = self.pos;
        let params = match self.peek() {
            Some(Tok::Ident(_)) => {
                let name = self.ident()?;
                if self.peek() == Some(&Tok::Arrow) {
                    vec![name]
                } else {
                    self.pos = save;
                    return Ok(None);
                }
            }
            Some(Tok::LParen) => match self.param_list() {
                Ok(ps) if self.peek() == Some(&Tok::Arrow) => ps,
                _ => {
                    self.pos = save;
                    return Ok(None);
                }
            },
            _ => return Ok(None),
        };
        self.expect(&Tok::Arrow)?;
        let body = if self.peek() == Some(&Tok::LBrace) {
            self.block()?
        } else {
            vec![Stmt::Return(Some(self.assignment()?))] // 식 본문 → return desugar
        };
        Ok(Some(Expr::Func { params, body }))
    }

    fn ternary(&mut self) -> Result<Expr, String> {
        let cond = self.logical_or()?;
        if self.eat(&Tok::Question) {
            let then = self.assignment()?;
            self.expect(&Tok::Colon)?;
            let other = self.assignment()?;
            return Ok(Expr::Ternary {
                cond: Box::new(cond),
                then: Box::new(then),
                other: Box::new(other),
            });
        }
        Ok(cond)
    }

    fn logical_or(&mut self) -> Result<Expr, String> {
        let mut left = self.logical_and()?;
        while self.eat(&Tok::OrOr) {
            let right = self.logical_and()?;
            left = Expr::Logical { op: LogOp::Or, left: Box::new(left), right: Box::new(right) };
        }
        Ok(left)
    }

    fn logical_and(&mut self) -> Result<Expr, String> {
        let mut left = self.bit_or()?;
        while self.eat(&Tok::AndAnd) {
            let right = self.bit_or()?;
            left = Expr::Logical { op: LogOp::And, left: Box::new(left), right: Box::new(right) };
        }
        Ok(left)
    }

    fn bit_or(&mut self) -> Result<Expr, String> {
        let mut left = self.bit_xor()?;
        while self.eat(&Tok::Pipe) {
            let right = self.bit_xor()?;
            left = Expr::Binary { op: BinOp::BitOr, left: Box::new(left), right: Box::new(right) };
        }
        Ok(left)
    }

    fn bit_xor(&mut self) -> Result<Expr, String> {
        let mut left = self.bit_and()?;
        while self.eat(&Tok::Caret) {
            let right = self.bit_and()?;
            left = Expr::Binary { op: BinOp::BitXor, left: Box::new(left), right: Box::new(right) };
        }
        Ok(left)
    }

    fn bit_and(&mut self) -> Result<Expr, String> {
        let mut left = self.equality()?;
        while self.eat(&Tok::Amp) {
            let right = self.equality()?;
            left = Expr::Binary { op: BinOp::BitAnd, left: Box::new(left), right: Box::new(right) };
        }
        Ok(left)
    }

    fn equality(&mut self) -> Result<Expr, String> {
        let mut left = self.relational()?;
        loop {
            let op = match self.peek() {
                Some(Tok::EqEq) => BinOp::EqEq,
                Some(Tok::EqEqEq) => BinOp::EqEqEq,
                Some(Tok::NotEq) => BinOp::NotEq,
                Some(Tok::NotEqEq) => BinOp::NotEqEq,
                _ => break,
            };
            self.pos += 1;
            let right = self.relational()?;
            left = Expr::Binary { op, left: Box::new(left), right: Box::new(right) };
        }
        Ok(left)
    }

    fn relational(&mut self) -> Result<Expr, String> {
        let mut left = self.shift()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Lt) => BinOp::Lt,
                Some(Tok::Gt) => BinOp::Gt,
                Some(Tok::Le) => BinOp::Le,
                Some(Tok::Ge) => BinOp::Ge,
                Some(Tok::Instanceof) => BinOp::Instanceof,
                Some(Tok::In) => BinOp::In,
                _ => break,
            };
            self.pos += 1;
            let right = self.shift()?;
            left = Expr::Binary { op, left: Box::new(left), right: Box::new(right) };
        }
        Ok(left)
    }

    fn shift(&mut self) -> Result<Expr, String> {
        let mut left = self.additive()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Shl) => BinOp::Shl,
                Some(Tok::Shr) => BinOp::Shr,
                Some(Tok::UShr) => BinOp::UShr,
                _ => break,
            };
            self.pos += 1;
            let right = self.additive()?;
            left = Expr::Binary { op, left: Box::new(left), right: Box::new(right) };
        }
        Ok(left)
    }

    fn additive(&mut self) -> Result<Expr, String> {
        let mut left = self.multiplicative()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Plus) => BinOp::Add,
                Some(Tok::Minus) => BinOp::Sub,
                _ => break,
            };
            self.pos += 1;
            let right = self.multiplicative()?;
            left = Expr::Binary { op, left: Box::new(left), right: Box::new(right) };
        }
        Ok(left)
    }

    fn multiplicative(&mut self) -> Result<Expr, String> {
        let mut left = self.unary()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Star) => BinOp::Mul,
                Some(Tok::Slash) => BinOp::Div,
                Some(Tok::Percent) => BinOp::Mod,
                _ => break,
            };
            self.pos += 1;
            let right = self.unary()?;
            left = Expr::Binary { op, left: Box::new(left), right: Box::new(right) };
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Expr, String> {
        let op = match self.peek() {
            Some(Tok::Minus) => Some(UnOp::Neg),
            Some(Tok::Not) => Some(UnOp::Not),
            Some(Tok::Typeof) => Some(UnOp::Typeof),
            Some(Tok::Tilde) => Some(UnOp::BitNot),
            _ => None,
        };
        if let Some(op) = op {
            self.pos += 1;
            return Ok(Expr::Unary { op, expr: Box::new(self.unary()?) });
        }
        // 전위 ++/--
        let upd = match self.peek() {
            Some(Tok::PlusPlus) => Some(UpdOp::Inc),
            Some(Tok::MinusMinus) => Some(UpdOp::Dec),
            _ => None,
        };
        if let Some(op) = upd {
            self.pos += 1;
            let target = self.unary()?;
            return Ok(Expr::Update { op, prefix: true, target: Box::new(target) });
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<Expr, String> {
        let mut e = self.primary()?;
        loop {
            match self.peek() {
                Some(Tok::Dot) => {
                    self.pos += 1;
                    let name = self.ident()?;
                    e = Expr::Member {
                        obj: Box::new(e),
                        prop: Box::new(Expr::Str(name)),
                        computed: false,
                    };
                }
                Some(Tok::LBracket) => {
                    self.pos += 1;
                    let idx = self.expr()?;
                    self.expect(&Tok::RBracket)?;
                    e = Expr::Member { obj: Box::new(e), prop: Box::new(idx), computed: true };
                }
                Some(Tok::LParen) => {
                    self.pos += 1;
                    let mut args = Vec::new();
                    if !self.eat(&Tok::RParen) {
                        loop {
                            args.push(self.assignment()?);
                            if self.eat(&Tok::Comma) {
                                continue;
                            }
                            self.expect(&Tok::RParen)?;
                            break;
                        }
                    }
                    e = Expr::Call { callee: Box::new(e), args };
                }
                Some(Tok::PlusPlus) => {
                    self.pos += 1;
                    e = Expr::Update { op: UpdOp::Inc, prefix: false, target: Box::new(e) };
                }
                Some(Tok::MinusMinus) => {
                    self.pos += 1;
                    e = Expr::Update { op: UpdOp::Dec, prefix: false, target: Box::new(e) };
                }
                _ => break,
            }
        }
        Ok(e)
    }

    fn primary(&mut self) -> Result<Expr, String> {
        match self.next()? {
            Tok::Num(n) => Ok(Expr::Num(n)),
            Tok::Str(s) => Ok(Expr::Str(s)),
            Tok::Regex(source, flags) => Ok(Expr::Regex { source, flags }),
            Tok::Template(parts) => {
                let mut out = Vec::new();
                for part in parts {
                    out.push(match part {
                        TplPart::Lit(s) => TemplatePart::Lit(s),
                        TplPart::Expr(src) => {
                            TemplatePart::Expr(Box::new(parse_expr_source(&src)?))
                        }
                    });
                }
                Ok(Expr::Template(out))
            }
            Tok::True => Ok(Expr::Bool(true)),
            Tok::False => Ok(Expr::Bool(false)),
            Tok::Null => Ok(Expr::Null),
            Tok::Undefined => Ok(Expr::Undefined),
            Tok::Ident(s) => Ok(Expr::Ident(s)),
            Tok::LParen => {
                let e = self.expr()?;
                self.expect(&Tok::RParen)?;
                Ok(e)
            }
            Tok::LBracket => {
                let mut items = Vec::new();
                if !self.eat(&Tok::RBracket) {
                    loop {
                        // 배열 구멍 [1,,2] → undefined
                        if self.peek() == Some(&Tok::Comma) {
                            items.push(Expr::Undefined);
                            self.pos += 1;
                            if self.eat(&Tok::RBracket) {
                                break;
                            }
                            continue;
                        }
                        items.push(self.assignment()?);
                        if self.eat(&Tok::Comma) {
                            if self.eat(&Tok::RBracket) {
                                break; // 트레일링 콤마
                            }
                            continue;
                        }
                        self.expect(&Tok::RBracket)?;
                        break;
                    }
                }
                Ok(Expr::Array(items))
            }
            Tok::LBrace => {
                let mut props = Vec::new();
                if !self.eat(&Tok::RBrace) {
                    loop {
                        let key = match self.next()? {
                            Tok::Ident(s) => s,
                            Tok::Str(s) => s,
                            Tok::Num(n) => n.to_string(),
                            other => {
                                return Err(format!("객체 키가 아님: {:?}{}", other, self.ctx()))
                            }
                        };
                        let value = if self.eat(&Tok::Colon) {
                            self.assignment()?
                        } else if self.peek() == Some(&Tok::LParen) {
                            // 메서드 단축 { foo(a) { ... } }
                            let params = self.param_list()?;
                            let body = self.block()?;
                            Expr::Func { params, body }
                        } else {
                            Expr::Ident(key.clone()) // 단축 프로퍼티 { a }
                        };
                        props.push((key, value));
                        if self.eat(&Tok::Comma) {
                            if self.eat(&Tok::RBrace) {
                                break;
                            }
                            continue;
                        }
                        self.expect(&Tok::RBrace)?;
                        break;
                    }
                }
                Ok(Expr::Object(props))
            }
            Tok::Function => {
                // 함수 식 (이름은 무시 가능)
                if matches!(self.peek(), Some(Tok::Ident(_))) {
                    self.pos += 1;
                }
                let params = self.param_list()?;
                let body = self.block()?;
                Ok(Expr::Func { params, body })
            }
            other => Err(format!("식이 필요한데 {:?}{}", other, self.ctx())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expr_of(src: &str) -> Expr {
        match parse(src).unwrap().into_iter().next().unwrap() {
            Stmt::Expr(e) => e,
            other => panic!("expected expr stmt, got {:?}", other),
        }
    }

    #[test]
    fn precedence_mul_over_add() {
        let e = expr_of("1 + 2 * 3");
        match e {
            Expr::Binary { op: BinOp::Add, right, .. } => {
                assert!(matches!(*right, Expr::Binary { op: BinOp::Mul, .. }));
            }
            other => panic!("{:?}", other),
        }
    }

    #[test]
    fn left_associative_subtraction() {
        // (2-1)-1
        let e = expr_of("2 - 1 - 1");
        match e {
            Expr::Binary { op: BinOp::Sub, left, .. } => {
                assert!(matches!(*left, Expr::Binary { op: BinOp::Sub, .. }));
            }
            other => panic!("{:?}", other),
        }
    }

    #[test]
    fn member_call_chain() {
        let e = expr_of("a.b(1)[2]");
        // ((a.b)(1))[2]
        match e {
            Expr::Member { obj, computed: true, .. } => {
                assert!(matches!(*obj, Expr::Call { .. }));
            }
            other => panic!("{:?}", other),
        }
    }

    #[test]
    fn assignment_is_right_associative() {
        let e = expr_of("a = b = 1");
        match e {
            Expr::Assign { value, .. } => assert!(matches!(*value, Expr::Assign { .. })),
            other => panic!("{:?}", other),
        }
    }

    #[test]
    fn arrow_functions() {
        let e = expr_of("x => x + 1");
        match e {
            Expr::Func { params, body } => {
                assert_eq!(params, vec!["x"]);
                assert!(matches!(body[0], Stmt::Return(Some(_))));
            }
            other => panic!("{:?}", other),
        }
        let e2 = expr_of("(a, b) => { return a + b; }");
        assert!(matches!(e2, Expr::Func { .. }));
        // 화살표 아님 → 괄호식으로 백트래킹
        let e3 = expr_of("(1 + 2) * 3");
        assert!(matches!(e3, Expr::Binary { op: BinOp::Mul, .. }));
    }

    #[test]
    fn object_and_array_literals() {
        // 문 위치의 '{' 는 블록이므로 괄호로 식 문맥을 강제
        let e = expr_of("({ a: 1, 'b': 2, c })");
        match e {
            Expr::Object(props) => {
                assert_eq!(props.len(), 3);
                assert_eq!(props[2].0, "c");
                assert!(matches!(props[2].1, Expr::Ident(_)));
            }
            other => panic!("{:?}", other),
        }
        assert!(matches!(expr_of("[1, 2, 3,]"), Expr::Array(v) if v.len() == 3));
    }

    #[test]
    fn statements_parse() {
        let stmts = parse(
            "var x = 1; let y; function f(a) { return a; } \
             if (x) { y = 2; } else if (y) { y = 3; } \
             while (x < 10) x++; \
             for (var i = 0; i < 3; i++) { continue; } \
             for (;;) { break; }",
        )
        .unwrap();
        assert_eq!(stmts.len(), 7);
        assert!(matches!(stmts[2], Stmt::FuncDecl { .. }));
        assert!(matches!(stmts[3], Stmt::If { other: Some(_), .. }));
        assert!(matches!(stmts[6], Stmt::For { init: None, cond: None, step: None, .. }));
    }

    #[test]
    fn ternary_and_logical() {
        let e = expr_of("a && b || c ? 1 : 2");
        assert!(matches!(e, Expr::Ternary { .. }));
    }

    #[test]
    fn parse_error_reports() {
        assert!(parse("var = 3;").is_err());
        assert!(parse("if (").is_err());
    }

    #[test]
    fn object_literal_as_statement_is_block() {
        // 문 위치의 '{' 는 블록 (JS 와 동일)
        let stmts = parse("{ var a = 1; }").unwrap();
        assert!(matches!(&stmts[0], Stmt::Block(inner) if inner.len() == 1));
    }
}
