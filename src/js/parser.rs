// JS 파서: 토큰열 → AST. 식은 우선순위 등반, 문은 재귀 하강.
// 세미콜론은 있으면 소비, 없어도 관용 (단순화된 ASI).

use super::ast::*;
use super::lexer::{tokenize, Tok, TplPart};

// 계산된 메서드/프로퍼티 키 [expr] 를 정적으로 프로퍼티 키 문자열에 매핑한다.
// 잘 알려진 심볼(Symbol.iterator → "\u{0}@@iterator")과 문자열/숫자 리터럴을 처리한다.
// 런타임에만 알 수 있는 키(변수 등)는 None(호출측이 유일 플레이스홀더 사용).
fn computed_key_string(e: &Expr) -> Option<String> {
    match e {
        Expr::Str(s) => Some(s.clone()),
        Expr::Num(n) => Some(if n.fract() == 0.0 && n.is_finite() {
            format!("{}", *n as i64)
        } else {
            format!("{}", n)
        }),
        // Symbol.iterator 등 잘 알려진 심볼 → 엔진과 동일한 고정 키.
        Expr::Member { obj, prop, computed: false } => {
            if let (Expr::Ident(o), Expr::Str(p)) = (obj.as_ref(), prop.as_ref()) {
                if o == "Symbol" {
                    return Some(match p.as_str() {
                        "iterator" => "\u{0}@@iterator".to_string(),
                        "asyncIterator" => "\u{0}@@asyncIterator".to_string(),
                        "toStringTag" => "\u{0}@@toStringTag".to_string(),
                        "hasInstance" => "\u{0}@@hasInstance".to_string(),
                        "toPrimitive" => "\u{0}@@toPrimitive".to_string(),
                        other => format!("\u{0}@@{}", other),
                    });
                }
            }
            None
        }
        _ => None,
    }
}

// 예약어를 프로퍼티/메서드 이름으로 쓸 때 원래 문자열 (obj.for, Symbol.for 등)
fn keyword_word(t: &Tok) -> Option<String> {
    let s = match t {
        Tok::Var => "var",
        Tok::Let => "let",
        Tok::Const => "const",
        Tok::Function => "function",
        Tok::Return => "return",
        Tok::If => "if",
        Tok::Else => "else",
        Tok::While => "while",
        Tok::Do => "do",
        Tok::For => "for",
        Tok::Break => "break",
        Tok::Continue => "continue",
        Tok::True => "true",
        Tok::False => "false",
        Tok::Null => "null",
        Tok::Undefined => "undefined",
        Tok::Typeof => "typeof",
        Tok::Void => "void",
        Tok::Delete => "delete",
        Tok::Try => "try",
        Tok::Catch => "catch",
        Tok::Finally => "finally",
        Tok::Throw => "throw",
        Tok::Switch => "switch",
        Tok::With => "with",
        Tok::Case => "case",
        Tok::Default => "default",
        Tok::Instanceof => "instanceof",
        Tok::In => "in",
        Tok::Class => "class",
        Tok::New => "new",
        Tok::This => "this",
        Tok::Extends => "extends",
        Tok::Super => "super",
        Tok::Static => "static",
        _ => return None,
    };
    Some(s.to_string())
}

// 식(배열/객체 리터럴)을 구조분해 할당 패턴으로 변환 (cover grammar 정제).
fn expr_to_pattern(e: Expr) -> Option<Pattern> {
    match e {
        Expr::Ident(n) => Some(Pattern::Name(n)),
        // 멤버 표현식도 대입 대상이 된다 (표준): [o.p, arr[0]] = [1, 2]
        m @ Expr::Member { .. } => Some(Pattern::Member(Box::new(m))),
        Expr::Array(items) => {
            let mut out = Vec::new();
            let mut rest = None;
            for it in items {
                match it {
                    Expr::Undefined => out.push(None), // 홀 [a, , b]
                    Expr::Spread(inner) => {
                        rest = Some(Box::new(expr_to_pattern(*inner)?));
                    }
                    Expr::Assign { op: AssignOp::Set, target, value } => {
                        out.push(Some((expr_to_pattern(*target)?, Some(*value))));
                    }
                    other => out.push(Some((expr_to_pattern(other)?, None))),
                }
            }
            Some(Pattern::Array(out, rest))
        }
        Expr::Object(props) => {
            let mut out = Vec::new();
            let mut rest = None;
            for (key, val) in props {
                match key {
                    PropKey::Static(name) => match val {
                        Expr::Assign { op: AssignOp::Set, target, value } => {
                            out.push((
                                crate::js::ast::PatKey::Static(name),
                                expr_to_pattern(*target)?,
                                Some(*value),
                            ));
                        }
                        other => out.push((
                            crate::js::ast::PatKey::Static(name),
                            expr_to_pattern(other)?,
                            None,
                        )),
                    },
                    // ({ [ex]: t } = v) — 계산된 키 구조분해 대입
                    PropKey::Computed(e) => match val {
                        Expr::Assign { op: AssignOp::Set, target, value } => out.push((
                            crate::js::ast::PatKey::Computed(*e),
                            expr_to_pattern(*target)?,
                            Some(*value),
                        )),
                        other => out.push((
                            crate::js::ast::PatKey::Computed(*e),
                            expr_to_pattern(other)?,
                            None,
                        )),
                    },
                    PropKey::Spread => rest = Some(Box::new(expr_to_pattern(val)?)),
                    _ => return None, // computed/getter 키는 미지원
                }
            }
            Some(Pattern::Object(out, rest))
        }
        _ => None,
    }
}

// 템플릿 보간 ${...} 소스를 독립적으로 식 파싱
fn parse_expr_source(src: &str) -> Result<Expr, String> {
    let (toks, nl_before, spans) = tokenize(src)?;
    let src_chars: std::rc::Rc<[char]> = src.chars().collect::<Vec<_>>().into();
    let mut p = Parser { toks, nl_before, spans, src_chars, pos: 0, pending_async: false };
    let e = p.expr()?;
    if !p.eof() {
        return Err("템플릿 보간 식 뒤에 잉여 토큰".to_string());
    }
    Ok(e)
}

pub fn parse(src: &str) -> Result<Vec<Stmt>, String> {
    let (toks, nl_before, spans) = tokenize(src)?;
    let src_chars: std::rc::Rc<[char]> = src.chars().collect::<Vec<_>>().into();
    let mut p = Parser { toks, nl_before, spans, src_chars, pos: 0, pending_async: false };
    let mut stmts = Vec::new();
    while !p.eof() {
        stmts.push(p.stmt()?);
    }
    Ok(stmts)
}

struct Parser {
    toks: Vec<Tok>,
    nl_before: Vec<bool>, // 각 토큰 직전 개행 여부 (ASI 판정)
    spans: Vec<(u32, u32)>, // 각 토큰의 (시작,끝) char 인덱스 (소스 슬라이스용)
    src_chars: std::rc::Rc<[char]>, // 원본 소스(char 벡터) — 함수 소스 텍스트 추출용
    pos: usize,
    pending_async: bool,
}

impl Parser {
    fn eof(&self) -> bool {
        self.pos >= self.toks.len()
    }

    // 토큰 [start_tok, end_tok) 를 원본 소스에서 그대로 슬라이스 (함수 소스 텍스트).
    // 함수/화살표/메서드 파싱 전후의 self.pos 로 범위를 잡는다.
    fn src_between(&self, start_tok: usize, end_tok: usize) -> Option<std::rc::Rc<str>> {
        if end_tok == 0 || start_tok >= end_tok || end_tok > self.spans.len() {
            return None;
        }
        let s = self.spans[start_tok].0 as usize;
        let e = self.spans[end_tok - 1].1 as usize;
        if s > e || e > self.src_chars.len() {
            return None;
        }
        Some(self.src_chars[s..e].iter().collect::<String>().into())
    }

    // 현재 토큰 직전에 개행이 있었나 (ASI 판정용). EOF 도 종료로 본다.
    fn newline_here(&self) -> bool {
        self.pos >= self.toks.len() || self.nl_before.get(self.pos).copied().unwrap_or(false)
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

    // 스프레드 ... (Dot 3개) 를 확인하고 소비.
    fn eat_spread(&mut self) -> bool {
        if self.peek() == Some(&Tok::Dot)
            && self.toks.get(self.pos + 1) == Some(&Tok::Dot)
            && self.toks.get(self.pos + 2) == Some(&Tok::Dot)
        {
            self.pos += 3;
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

    // open 위치의 '[' 에 대응하는 ']' 인덱스 (중첩 고려). 계산 키 판별에 쓴다.
    fn matching_bracket(&self, open: usize) -> Option<usize> {
        if self.toks.get(open) != Some(&Tok::LBracket) {
            return None;
        }
        let mut depth = 0usize;
        for i in open..self.toks.len() {
            match self.toks.get(i) {
                Some(Tok::LBracket) => depth += 1,
                Some(Tok::RBracket) => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        None
    }

    // `async function(){}` / `async (a)=>b` / `async x=>b` 를 만나면 async 표시 후 소비.
    // assignment() 와 unary() 양쪽에서 필요하다 — `await async function(){}` 처럼
    // await 의 피연산자(unary)로 async 함수식이 오는 경우가 번들에 흔하다.
    fn eat_async_prefix(&mut self) {
        if !matches!(self.peek(), Some(Tok::Ident(n)) if n == "async") {
            return;
        }
        let n1 = self.toks.get(self.pos + 1);
        let n2 = self.toks.get(self.pos + 2);
        let is_async_fn = matches!(n1, Some(Tok::Function) | Some(Tok::LParen))
            || matches!((n1, n2), (Some(Tok::Ident(_)), Some(Tok::Arrow)));
        if is_async_fn {
            self.pos += 1;
            self.pending_async = true;
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

    // break/continue 뒤의 레이블: 식별자면 레이블 이름을 소비해 반환 (없으면 None).
    fn eat_label(&mut self) -> Option<String> {
        if let Some(Tok::Ident(name)) = self.peek() {
            let name = name.clone();
            self.pos += 1;
            return Some(name);
        }
        None
    }

    // ── 문 ──────────────────────────────────────────────────────────

    fn stmt(&mut self) -> Result<Stmt, String> {
        // ES 모듈: import 는 스킵(로딩 미지원), export 는 수식어 벗겨 선언 유지.
        if matches!(self.peek(), Some(Tok::Ident(n)) if n == "import")
            && !matches!(self.toks.get(self.pos + 1), Some(Tok::LParen) | Some(Tok::Dot))
        {
            return self.import_stmt();
        }
        if matches!(self.peek(), Some(Tok::Ident(n)) if n == "export") {
            return self.export_stmt();
        }
        // async function 선언: async 표시 후 함수 선언으로 (func_decl 이 is_async 로 반영)
        if matches!(self.peek(), Some(Tok::Ident(n)) if n == "async")
            && self.toks.get(self.pos + 1) == Some(&Tok::Function)
        {
            self.pos += 1;
            self.pending_async = true;
            return self.func_decl();
        }
        match self.peek() {
            Some(Tok::Var) | Some(Tok::Let) | Some(Tok::Const) => self.var_decl(),
            Some(Tok::Function) => self.func_decl(),
            Some(Tok::If) => self.if_stmt(),
            Some(Tok::While) => self.while_stmt(),
            Some(Tok::Do) => self.do_while_stmt(),
            Some(Tok::For) => self.for_stmt(),
            Some(Tok::Return) => {
                self.pos += 1;
                // 값 없는 return: ; } EOF, 또는 개행이 뒤따를 때(ASI 제약 생성물 — 표준).
                let value = if self.newline_here()
                    || matches!(self.peek(), Some(Tok::Semi) | Some(Tok::RBrace))
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
                // 개행이면 레이블 없음(ASI 제약 생성물)
                let label = if self.newline_here() { None } else { self.eat_label() };
                self.eat(&Tok::Semi);
                Ok(Stmt::Break(label))
            }
            Some(Tok::Continue) => {
                self.pos += 1;
                let label = if self.newline_here() { None } else { self.eat_label() };
                self.eat(&Tok::Semi);
                Ok(Stmt::Continue(label))
            }
            // 레이블 문 (foo: stmt) — break/continue 가 이 레이블을 지목할 수 있게 보존.
            Some(Tok::Ident(name)) if self.toks.get(self.pos + 1) == Some(&Tok::Colon) => {
                let name = name.clone();
                self.pos += 2; // Ident ':'
                let inner = self.stmt()?;
                Ok(Stmt::Labeled(name, Box::new(inner)))
            }
            Some(Tok::Throw) => {
                self.pos += 1;
                let e = self.expr()?;
                self.eat(&Tok::Semi);
                Ok(Stmt::Throw(e))
            }
            Some(Tok::Try) => self.try_stmt(),
            Some(Tok::Class) => {
                self.pos += 1; // 'class'
                Ok(Stmt::ClassDecl(self.class_def(true)?))
            }
            Some(Tok::Switch) => self.switch_stmt(),
            // with (obj) stmt (§14.11)
            Some(Tok::With) => {
                self.pos += 1;
                self.expect(&Tok::LParen)?;
                let obj = self.expr()?;
                self.expect(&Tok::RParen)?;
                let body = Box::new(self.stmt()?);
                Ok(Stmt::With { obj, body })
            }
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

    // import ... (from '...') — 모듈 로딩 미지원. 명세자 문자열까지 소비하고 no-op.
    // import 선언 (ES 모듈).
    //   import 'm';                      부수효과만
    //   import x from 'm';               기본
    //   import * as ns from 'm';         네임스페이스
    //   import { a, b as c } from 'm';   이름
    //   import x, { a } from 'm';        혼합
    fn import_stmt(&mut self) -> Result<Stmt, String> {
        self.pos += 1; // import
        let mut specs = Vec::new();
        // import 'm';  (부수효과 전용)
        if let Some(Tok::Str(src)) = self.peek().cloned() {
            self.pos += 1;
            self.eat(&Tok::Semi);
            return Ok(Stmt::Import { specs, source: src });
        }
        loop {
            match self.peek().cloned() {
                Some(Tok::Star) => {
                    self.pos += 1;
                    // as ns
                    if matches!(self.peek(), Some(Tok::Ident(w)) if w == "as") {
                        self.pos += 1;
                    }
                    let name = self.ident_name()?;
                    specs.push(ImportSpec::Namespace(name));
                }
                Some(Tok::LBrace) => {
                    self.pos += 1;
                    while !self.eof() && self.peek() != Some(&Tok::RBrace) {
                        let imported = self.ident_name()?;
                        let local = if matches!(self.peek(), Some(Tok::Ident(w)) if w == "as") {
                            self.pos += 1;
                            self.ident_name()?
                        } else {
                            imported.clone()
                        };
                        specs.push(ImportSpec::Named(imported, local));
                        if !self.eat(&Tok::Comma) {
                            break;
                        }
                    }
                    if !self.eat(&Tok::RBrace) {
                        return Err("import 목록이 닫히지 않음".to_string());
                    }
                }
                Some(Tok::Ident(_)) => {
                    let name = self.ident_name()?;
                    specs.push(ImportSpec::Default(name));
                }
                _ => break,
            }
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        // from 'm'
        if matches!(self.peek(), Some(Tok::Ident(w)) if w == "from") {
            self.pos += 1;
        }
        let source = match self.peek().cloned() {
            Some(Tok::Str(s)) => {
                self.pos += 1;
                s
            }
            _ => return Err("import 에 모듈 명세자가 없음".to_string()),
        };
        self.eat(&Tok::Semi);
        Ok(Stmt::Import { specs, source })
    }

    // 식별자(또는 예약어 형태의 이름) 하나
    fn ident_name(&mut self) -> Result<String, String> {
        match self.peek().cloned() {
            Some(Tok::Ident(w)) => {
                self.pos += 1;
                Ok(w)
            }
            Some(Tok::Default) => {
                self.pos += 1;
                Ok("default".to_string())
            }
            other => Err(format!("식별자를 기대: {:?}", other)),
        }
    }

    // export 선언 (ES 모듈).
    //   export default <식|함수|클래스>
    //   export const/let/var/function/class …
    //   export { a, b as c } [from 'm']
    //   export * from 'm'
    fn export_stmt(&mut self) -> Result<Stmt, String> {
        self.pos += 1; // export
        // export default …
        if matches!(self.peek(), Some(Tok::Default)) {
            self.pos += 1;
            let inner = match self.peek() {
                Some(Tok::Function) => self.func_decl()?,
                Some(Tok::Class) => {
                    self.pos += 1;
                    Stmt::ClassDecl(self.class_def(true)?)
                }
                _ => {
                    let e = self.assignment()?;
                    self.eat(&Tok::Semi);
                    Stmt::Expr(e)
                }
            };
            return Ok(Stmt::ExportDefault(Box::new(inner)));
        }
        // export * from 'm'  /  export * as ns from 'm'
        if matches!(self.peek(), Some(Tok::Star)) {
            self.pos += 1;
            // export * as ns from 'm' → 네임스페이스 재수출
            let ns = if matches!(self.peek(), Some(Tok::Ident(w)) if w == "as") {
                self.pos += 1;
                Some(self.ident_name()?)
            } else {
                None
            };
            if matches!(self.peek(), Some(Tok::Ident(w)) if w == "from") {
                self.pos += 1;
            }
            let source = match self.peek().cloned() {
                Some(Tok::Str(s)) => {
                    self.pos += 1;
                    s
                }
                _ => return Err("export * 에 모듈 명세자가 없음".to_string()),
            };
            self.eat(&Tok::Semi);
            return Ok(match ns {
                Some(n) => Stmt::ExportNamed {
                    specs: vec![("*".to_string(), n)],
                    source: Some(source),
                },
                None => Stmt::ExportAll { source },
            });
        }
        // export { a, b as c } [from 'm']
        if matches!(self.peek(), Some(Tok::LBrace)) {
            self.pos += 1;
            let mut specs = Vec::new();
            while !self.eof() && self.peek() != Some(&Tok::RBrace) {
                let local = self.ident_name()?;
                let exported = if matches!(self.peek(), Some(Tok::Ident(w)) if w == "as") {
                    self.pos += 1;
                    self.ident_name()?
                } else {
                    local.clone()
                };
                specs.push((local, exported));
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            if !self.eat(&Tok::RBrace) {
                return Err("export 목록이 닫히지 않음".to_string());
            }
            let source = if matches!(self.peek(), Some(Tok::Ident(w)) if w == "from") {
                self.pos += 1;
                match self.peek().cloned() {
                    Some(Tok::Str(s)) => {
                        self.pos += 1;
                        Some(s)
                    }
                    _ => return Err("export … from 에 모듈 명세자가 없음".to_string()),
                }
            } else {
                None
            };
            self.eat(&Tok::Semi);
            return Ok(Stmt::ExportNamed { specs, source });
        }
        // export const/let/var/function/class/async …
        let inner = self.stmt()?;
        Ok(Stmt::ExportDecl(Box::new(inner)))
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
            let pat = self.binding_pattern()?;
            let init = if self.eat(&Tok::Assign) { Some(self.assignment()?) } else { None };
            decls.push((pat, init));
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.eat(&Tok::Semi);
        Ok(Stmt::VarDecl { kind, decls })
    }

    // 바인딩 대상: 이름 / {a, b: c} / [a, , b]
    fn binding_pattern(&mut self) -> Result<Pattern, String> {
        match self.peek() {
            Some(Tok::LBrace) => {
                self.pos += 1;
                let mut props = Vec::new();
                let mut rest = None;
                while self.peek() != Some(&Tok::RBrace) {
                    // rest { a, ...others }
                    if self.eat_spread() {
                        rest = Some(Box::new(self.binding_pattern()?));
                        self.eat(&Tok::Comma);
                        break;
                    }
                    // 계산된 키: { [ex]: sub } (ES6),
                    // 문자열/숫자 키: { "a-b": x }, { 1: y } — 미니파이된 번들이 흔히 쓴다.
                    // 예전엔 식별자만 받아서 그 스크립트가 **파싱에서 통째로 죽었다**
                    // (lucide.dev 의 번들이 { "icon-node": a } 를 쓴다).
                    let key = if self.eat(&Tok::LBracket) {
                        let e = self.assignment()?;
                        self.expect(&Tok::RBracket)?;
                        crate::js::ast::PatKey::Computed(e)
                    } else {
                        match self.peek() {
                            Some(Tok::Str(_)) => {
                                let Some(Tok::Str(k)) = self.peek().cloned() else { unreachable!() };
                                self.pos += 1;
                                crate::js::ast::PatKey::Static(k)
                            }
                            Some(Tok::Num(_)) => {
                                let Some(Tok::Num(n)) = self.peek().cloned() else { unreachable!() };
                                self.pos += 1;
                                // 숫자 키는 문자열로 정규화된다 (표준: 프로퍼티 키는 문자열)
                                crate::js::ast::PatKey::Static(if n.fract() == 0.0 && n.abs() < 1e21 {
                                    format!("{}", n as i64)
                                } else {
                                    n.to_string()
                                })
                            }
                            _ => crate::js::ast::PatKey::Static(self.prop_name()?),
                        }
                    };
                    // { key: subpattern } (중첩 가능) 또는 { key }
                    let sub = if self.eat(&Tok::Colon) {
                        self.binding_pattern()?
                    } else {
                        match &key {
                            crate::js::ast::PatKey::Static(k) => Pattern::Name(k.clone()),
                            // { [ex] } 는 문법 오류 (반드시 : 대상이 있어야 한다)
                            _ => return Err("계산된 키에는 대상이 필요하다".to_string()),
                        }
                    };
                    // 기본값 { a = 1 } / { a: b = 1 }
                    let default =
                        if self.eat(&Tok::Assign) { Some(self.assignment()?) } else { None };
                    props.push((key, sub, default));
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                self.expect(&Tok::RBrace)?;
                Ok(Pattern::Object(props, rest))
            }
            Some(Tok::LBracket) => {
                self.pos += 1;
                let mut elems = Vec::new();
                let mut rest = None;
                while self.peek() != Some(&Tok::RBracket) {
                    if self.peek() == Some(&Tok::Comma) {
                        elems.push(None); // 구멍 [a, , b]
                        self.pos += 1;
                        continue;
                    }
                    // rest [a, ...others]
                    if self.eat_spread() {
                        rest = Some(Box::new(self.binding_pattern()?));
                        self.eat(&Tok::Comma);
                        break;
                    }
                    let sub = self.binding_pattern()?; // 중첩/이름
                    let default =
                        if self.eat(&Tok::Assign) { Some(self.assignment()?) } else { None };
                    elems.push(Some((sub, default)));
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                self.expect(&Tok::RBracket)?;
                Ok(Pattern::Array(elems, rest))
            }
            _ => Ok(Pattern::Name(self.ident()?)),
        }
    }

    fn func_decl(&mut self) -> Result<Stmt, String> {
        // 소스 시작: async 접두가 이미 소비됐으면 그 토큰부터(pending_async 참).
        let start = self.pos.saturating_sub(if self.pending_async { 1 } else { 0 });
        self.expect(&Tok::Function)?;
        let is_generator = self.eat(&Tok::Star); // function* 제너레이터
        let name = self.ident()?;
        let is_async = std::mem::take(&mut self.pending_async);
        let (params, mut body) = self.param_list()?;
        body.extend(self.block()?); // 프롤로그(기본값) 뒤에 실제 본문
        let source = self.src_between(start, self.pos);
        Ok(Stmt::FuncDecl { name, params, body, is_generator, is_async, source })
    }

    // 파라미터 목록 → (이름들, 본문 프롤로그).
    // 기본값 파라미터 name=expr 는 `if(name===undefined) name=expr;` 로 디슈가해
    // 프롤로그로 반환(호출자가 본문 앞에 붙임). 파라미터 타입 변경 없이 정확히 동작.
    // rest ...name 은 이름만 바인딩(간이). 구조분해 파라미터는 자리표시 이름으로 수용.
    fn param_list(&mut self) -> Result<(Vec<String>, Vec<Stmt>), String> {
        self.expect(&Tok::LParen)?;
        let mut params = Vec::new();
        let mut prologue = Vec::new();
        if self.eat(&Tok::RParen) {
            return Ok((params, prologue));
        }
        loop {
            // rest 파라미터 ...name (…는 Dot 3개로 렉싱됨)
            let is_rest = self.peek() == Some(&Tok::Dot)
                && self.toks.get(self.pos + 1) == Some(&Tok::Dot)
                && self.toks.get(self.pos + 2) == Some(&Tok::Dot);
            if is_rest {
                self.pos += 3;
            }
            // 구조분해 파라미터 { .. } / [ .. ] — 자리표시 이름으로 받고 프롤로그에서 분해
            let (name, pattern) = if matches!(self.peek(), Some(Tok::LBrace | Tok::LBracket)) {
                let pat = self.binding_pattern()?;
                (format!("__pat{}__", params.len()), Some(pat))
            } else {
                (self.ident()?, None)
            };
            if self.eat(&Tok::Assign) {
                let default = self.assignment()?;
                prologue.push(Stmt::If {
                    cond: Expr::Binary {
                        op: BinOp::EqEqEq,
                        left: Box::new(Expr::Ident(name.clone())),
                        right: Box::new(Expr::Undefined),
                    },
                    then: vec![Stmt::Expr(Expr::Assign {
                        op: AssignOp::Set,
                        target: Box::new(Expr::Ident(name.clone())),
                        value: Box::new(default),
                    })],
                    other: None,
                });
            }
            // 구조분해: let <pattern> = <자리표시 인자>;  (기본값 If 뒤에 실행)
            if let Some(pat) = pattern {
                prologue.push(Stmt::VarDecl {
                    kind: DeclKind::Let,
                    decls: vec![(pat, Some(Expr::Ident(name.clone())))],
                });
            }
            // rest 는 "...이름" 으로 저장 — 호출 시 남은 인자를 배열로 모은다.
            // (프롤로그는 위에서 깨끗한 이름을 쓰고, 저장 이름에만 표시를 붙인다)
            params.push(if is_rest { format!("...{}", name) } else { name });
            if self.eat(&Tok::Comma) {
                if self.eat(&Tok::RParen) {
                    break; // 트레일링 콤마
                }
                continue;
            }
            self.expect(&Tok::RParen)?;
            break;
        }
        Ok((params, prologue))
    }

    // 여는 괄호 종류에 맞춰 짝이 맞을 때까지 토큰을 소비 (구조분해 파라미터 스킵용)
    fn skip_balanced(&mut self) -> Result<(), String> {
        let open = self.next()?;
        let close = match open {
            Tok::LBrace => Tok::RBrace,
            Tok::LBracket => Tok::RBracket,
            Tok::LParen => Tok::RParen,
            other => return Err(format!("여는 괄호가 아님: {:?}{}", other, self.ctx())),
        };
        let mut depth = 1;
        while depth > 0 {
            match self.next()? {
                ref t if *t == open => depth += 1,
                ref t if *t == close => depth -= 1,
                _ => {}
            }
        }
        Ok(())
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

    fn do_while_stmt(&mut self) -> Result<Stmt, String> {
        self.expect(&Tok::Do)?;
        let body = self.body_of_clause()?;
        self.expect(&Tok::While)?;
        self.expect(&Tok::LParen)?;
        let cond = self.expr()?;
        self.expect(&Tok::RParen)?;
        self.eat(&Tok::Semi); // do-while 뒤 세미콜론 (있으면 소비)
        Ok(Stmt::DoWhile { body, cond })
    }

    fn for_stmt(&mut self) -> Result<Stmt, String> {
        self.expect(&Tok::For)?;
        // `for await (... of asyncIterable)` (ES2018). 없으면 이 스크립트가 통째로 죽는다
        // (파싱 실패는 파일 하나를 통째로 버린다 — 실제로 tailwindcss.com 이 그랬다).
        let is_await = if matches!(self.peek(), Some(Tok::Ident(s)) if s == "await") {
            self.pos += 1;
            true
        } else {
            false
        };
        self.expect(&Tok::LParen)?;
        // 구조분해 for-of/in: for ([var|let|const] {..}|[..] of|in ...) — 임시 변수로 디슈가
        let destr = {
            let save = self.pos;
            if matches!(self.peek(), Some(Tok::Var | Tok::Let | Tok::Const)) {
                self.pos += 1;
            }
            let mut kind = None;
            if matches!(self.peek(), Some(Tok::LBrace | Tok::LBracket)) && self.skip_balanced().is_ok()
            {
                if matches!(self.peek(), Some(Tok::Ident(s)) if s == "of") {
                    kind = Some(false); // of
                } else if self.peek() == Some(&Tok::In) {
                    kind = Some(true); // in
                }
            }
            self.pos = save;
            kind
        };
        if let Some(is_in) = destr {
            let tmp = format!("__forpat{}__", self.pos);
            if matches!(self.peek(), Some(Tok::Var | Tok::Let | Tok::Const)) {
                self.pos += 1;
            }
            let pat = self.binding_pattern()?;
            if is_in {
                self.expect(&Tok::In)?;
            } else {
                self.pos += 1; // "of"
            }
            let seq = self.expr()?;
            self.expect(&Tok::RParen)?;
            let mut body = self.body_of_clause()?;
            body.insert(
                0,
                Stmt::VarDecl {
                    kind: DeclKind::Let,
                    decls: vec![(pat, Some(Expr::Ident(tmp.clone())))],
                },
            );
            return Ok(if is_in {
                Stmt::ForIn { name: tmp, obj: seq, body }
            } else {
                Stmt::ForOf { name: tmp, iter: seq, body, is_await }
            });
        }
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
        // for (v of iter) — "of" 는 문맥 키워드(Ident)로 렉싱됨
        let is_of = |t: Option<&Tok>| matches!(t, Some(Tok::Ident(s)) if s == "of");
        let is_decl_of = matches!(self.peek(), Some(Tok::Var | Tok::Let | Tok::Const))
            && matches!(self.toks.get(self.pos + 1), Some(Tok::Ident(_)))
            && is_of(self.toks.get(self.pos + 2));
        let is_bare_of =
            matches!(self.peek(), Some(Tok::Ident(_))) && is_of(self.toks.get(self.pos + 1));
        if is_decl_of || is_bare_of {
            if is_decl_of {
                self.pos += 1; // var/let/const
            }
            let name = self.ident()?;
            self.pos += 1; // "of"
            let iter = self.expr()?;
            self.expect(&Tok::RParen)?;
            return Ok(Stmt::ForOf { name, iter, body: self.body_of_clause()?, is_await });
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
        // yield [*] [expr] — 제너레이터 본문. (yield 를 변수명으로 쓰는 경우는 드묾)
        if matches!(self.peek(), Some(Tok::Ident(n)) if n == "yield")
            && !matches!(self.toks.get(self.pos + 1), Some(Tok::Assign) | Some(Tok::Dot) | Some(Tok::Colon))
        {
            self.pos += 1; // yield
            let star = self.eat(&Tok::Star);
            // 인자 없는 yield (문 끝/닫는 괄호/콤마 앞)
            let arg = match self.peek() {
                None | Some(Tok::Semi) | Some(Tok::RBrace) | Some(Tok::RParen)
                | Some(Tok::RBracket) | Some(Tok::Comma) => None,
                _ => Some(Box::new(self.assignment()?)),
            };
            return Ok(Expr::Yield { star, arg });
        }
        // async 수식어(식 위치): async () => / async x => / async function(){} — 무시
        if matches!(self.peek(), Some(Tok::Ident(n)) if n == "async") {
            self.eat_async_prefix();
        }
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
            Some(Tok::PercentAssign) => Some(AssignOp::Mod),
            Some(Tok::AmpAssign) => Some(AssignOp::BitAnd),
            Some(Tok::PipeAssign) => Some(AssignOp::BitOr),
            Some(Tok::CaretAssign) => Some(AssignOp::BitXor),
            Some(Tok::ShlAssign) => Some(AssignOp::Shl),
            Some(Tok::ShrAssign) => Some(AssignOp::Shr),
            Some(Tok::UShrAssign) => Some(AssignOp::UShr),
            Some(Tok::StarStarAssign) => Some(AssignOp::Pow),
            Some(Tok::AndAndAssign) => Some(AssignOp::And),
            Some(Tok::OrOrAssign) => Some(AssignOp::Or),
            Some(Tok::QQAssign) => Some(AssignOp::Nullish),
            _ => None,
        };
        if let Some(op) = op {
            self.pos += 1;
            let value = self.assignment()?; // 우결합
            // 구조분해 할당: `=` 이고 LHS 가 배열/객체 리터럴이면 패턴으로 재해석
            if op == AssignOp::Set && matches!(left, Expr::Array(_) | Expr::Object(_)) {
                if let Some(pattern) = expr_to_pattern(left) {
                    return Ok(Expr::AssignPattern { pattern, value: Box::new(value) });
                }
                return Err("잘못된 구조분해 할당 대상".to_string());
            }
            if !matches!(left, Expr::Ident(_) | Expr::Member { .. }) {
                return Err("할당 대상이 아님".to_string());
            }
            return Ok(Expr::Assign { op, target: Box::new(left), value: Box::new(value) });
        }
        Ok(left)
    }

    // `x => ...` / `(a, b) => ...`. 화살표가 아니면 위치를 되돌리고 None.
    fn try_arrow(&mut self) -> Result<Option<Expr>, String> {
        let save = self.pos;
        let (params, prologue) = match self.peek() {
            Some(Tok::Ident(_)) => {
                let name = self.ident()?;
                if self.peek() == Some(&Tok::Arrow) {
                    (vec![name], Vec::new())
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
        let is_async = std::mem::take(&mut self.pending_async); // 본문 파싱 전에 캡처
        // 소스 시작: 화살표 params 시작(save). async 접두가 있었으면 그 앞 토큰부터.
        let start = if is_async { save.saturating_sub(1) } else { save };
        let mut body = prologue;
        if self.peek() == Some(&Tok::LBrace) {
            body.extend(self.block()?);
        } else {
            body.push(Stmt::Return(Some(self.assignment()?))); // 식 본문 → return desugar
        }
        let source = self.src_between(start, self.pos);
        Ok(Some(Expr::Func { name: None, params, body, is_arrow: true, is_generator: false, is_async, source }))
    }

    fn ternary(&mut self) -> Result<Expr, String> {
        let cond = self.nullish()?;
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

    fn nullish(&mut self) -> Result<Expr, String> {
        let mut left = self.logical_or()?;
        while self.eat(&Tok::QuestionQuestion) {
            let right = self.logical_or()?;
            left = Expr::Nullish { left: Box::new(left), right: Box::new(right) };
        }
        Ok(left)
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
        let mut left = self.exponent()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Star) => BinOp::Mul,
                Some(Tok::Slash) => BinOp::Div,
                Some(Tok::Percent) => BinOp::Mod,
                _ => break,
            };
            self.pos += 1;
            let right = self.exponent()?;
            left = Expr::Binary { op, left: Box::new(left), right: Box::new(right) };
        }
        Ok(left)
    }

    // 거듭제곱 ** — 곱셈보다 강하게 결합하고 우결합(2**3**2 = 2**9)
    fn exponent(&mut self) -> Result<Expr, String> {
        let base = self.unary()?;
        if self.peek() == Some(&Tok::StarStar) {
            self.pos += 1;
            let exp = self.exponent()?;
            return Ok(Expr::Binary {
                op: BinOp::Pow,
                left: Box::new(base),
                right: Box::new(exp),
            });
        }
        Ok(base)
    }

    fn unary(&mut self) -> Result<Expr, String> {
        // `await async function(){}` 처럼 피연산자로 async 함수식이 오는 경우를 위해
        // 여기서도 async 접두를 소비한다(assignment() 만으론 도달하지 않는다).
        self.eat_async_prefix();
        // await expr (async 함수 내). await 뒤가 식 시작일 때만 연산자로 취급
        // (그 외엔 'await' 를 일반 식별자로 — 관용).
        if matches!(self.peek(), Some(Tok::Ident(n)) if n == "await") {
            let follows = self.toks.get(self.pos + 1);
            let terminator = matches!(follows,
                None | Some(Tok::Semi) | Some(Tok::Comma) | Some(Tok::RParen)
                    | Some(Tok::RBrace) | Some(Tok::RBracket) | Some(Tok::Colon)
                    | Some(Tok::Assign) | Some(Tok::Arrow) | Some(Tok::Dot));
            if !terminator {
                self.pos += 1;
                return Ok(Expr::Await(Box::new(self.unary()?)));
            }
        }
        let op = match self.peek() {
            Some(Tok::Minus) => Some(UnOp::Neg),
            Some(Tok::Plus) => Some(UnOp::Pos),
            Some(Tok::Not) => Some(UnOp::Not),
            Some(Tok::Typeof) => Some(UnOp::Typeof),
            Some(Tok::Tilde) => Some(UnOp::BitNot),
            Some(Tok::Void) => Some(UnOp::Void),
            Some(Tok::Delete) => Some(UnOp::Delete),
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
                    let name = self.prop_name()?;
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
                    let args = self.arg_list()?;
                    e = Expr::Call { callee: Box::new(e), args };
                }
                // 태그드 템플릿: tag`a${x}b` → tag(strings, x). strings 에는 raw 도 실린다.
                Some(Tok::Template(parts)) => {
                    let parts = parts.clone();
                    self.pos += 1;
                    let mut cooked = Vec::new();
                    let mut raw = Vec::new();
                    let mut values = Vec::new();
                    for part in parts {
                        match part {
                            TplPart::Lit(c, r) => {
                                cooked.push(c);
                                raw.push(r);
                            }
                            TplPart::Expr(src) => {
                                // 보간 앞에 리터럴이 없으면 빈 문자열 자리를 채운다
                                // (표준: strings.length === values.length + 1)
                                if cooked.len() == values.len() {
                                    cooked.push(String::new());
                                    raw.push(String::new());
                                }
                                values.push(parse_expr_source(&src)?);
                            }
                        }
                    }
                    while cooked.len() < values.len() + 1 {
                        cooked.push(String::new());
                        raw.push(String::new());
                    }
                    e = Expr::Tagged { tag: Box::new(e), cooked, raw, values };
                }
                // 옵셔널 체이닝: ?.prop / ?.[expr] / ?.(args)
                Some(Tok::OptChain) => {
                    self.pos += 1;
                    match self.peek() {
                        Some(Tok::LParen) => {
                            let args = self.arg_list()?;
                            e = Expr::OptCall { callee: Box::new(e), args };
                        }
                        Some(Tok::LBracket) => {
                            self.pos += 1;
                            let idx = self.expr()?;
                            self.expect(&Tok::RBracket)?;
                            e = Expr::OptMember {
                                obj: Box::new(e),
                                prop: Box::new(idx),
                                computed: true,
                            };
                        }
                        _ => {
                            let name = self.prop_name()?;
                            e = Expr::OptMember {
                                obj: Box::new(e),
                                prop: Box::new(Expr::Str(name)),
                                computed: false,
                            };
                        }
                    }
                }
                // 후위 ++/-- 는 제약 생성물: 피연산자와 같은 줄이어야 한다(개행이면 후위 아님).
                Some(Tok::PlusPlus) if !self.newline_here() => {
                    self.pos += 1;
                    e = Expr::Update { op: UpdOp::Inc, prefix: false, target: Box::new(e) };
                }
                Some(Tok::MinusMinus) if !self.newline_here() => {
                    self.pos += 1;
                    e = Expr::Update { op: UpdOp::Dec, prefix: false, target: Box::new(e) };
                }
                _ => break,
            }
        }
        Ok(e)
    }

    // ( arg, arg, ... ) — 여는 괄호에서 시작해 닫는 괄호까지 소비. 트레일링 콤마 허용.
    fn arg_list(&mut self) -> Result<Vec<Expr>, String> {
        self.expect(&Tok::LParen)?;
        let mut args = Vec::new();
        loop {
            // 빈 목록 `()` 또는 트레일링 콤마 뒤 닫힘 `f(a,)`
            if self.eat(&Tok::RParen) {
                break;
            }
            if self.eat_spread() {
                args.push(Expr::Spread(Box::new(self.assignment()?)));
            } else {
                args.push(self.assignment()?);
            }
            if self.eat(&Tok::Comma) {
                continue;
            }
            self.expect(&Tok::RParen)?;
            break;
        }
        Ok(args)
    }

    // new 뒤 콜리: primary + 멤버 접근(.a, [b])만. 호출 괄호는 new 인자로.
    fn new_callee(&mut self) -> Result<Expr, String> {
        let mut e = self.primary()?;
        loop {
            match self.peek() {
                Some(Tok::Dot) => {
                    self.pos += 1;
                    let name = self.prop_name()?;
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
                _ => break,
            }
        }
        Ok(e)
    }

    // class 몸통 파싱. `class` 키워드는 호출자가 이미 소비함.
    // is_decl 이면 이름 필수 (문), 아니면 선택 (식).
    fn class_def(&mut self, is_decl: bool) -> Result<ClassDef, String> {
        // `class` 키워드는 호출자가 소비함 → 소스 시작은 그 앞 토큰.
        let start = self.pos.saturating_sub(1);
        let name = if matches!(self.peek(), Some(Tok::Ident(_))) {
            Some(self.ident()?)
        } else if is_decl {
            return Err("class 선언에 이름 필요".to_string());
        } else {
            None
        };
        // ClassHeritage 는 LeftHandSideExpression 이다 (표준 §15.7) — **호출식도 온다**.
        // 믹스인 패턴: `class X extends (0, ns.mixin)(Base) { }` (lit-element, MDN 등이 쓴다).
        // 예전엔 이름/멤버 경로만 받아서 파서가 죽었고, 모듈 전체가 실행되지 않았다.
        let parent = if self.eat(&Tok::Extends) {
            Some(Box::new(self.postfix()?))
        } else {
            None
        };
        self.expect(&Tok::LBrace)?;
        let mut ctor = None;
        let mut methods = Vec::new();
        let mut statics = Vec::new();
        let mut getters = Vec::new();
        let mut setters = Vec::new();
        let mut static_getters = Vec::new();
        let mut static_setters = Vec::new();
        let mut fields = Vec::new();
        let mut static_fields = Vec::new();
        while self.peek() != Some(&Tok::RBrace) {
            if self.eof() {
                return Err("닫히지 않은 class".to_string());
            }
            if self.eat(&Tok::Semi) {
                continue; // 멤버 사이 세미콜론 허용
            }
            let is_static = self.eat(&Tok::Static);
            // static 초기화 블록 (ES2022): `static { ... }`. 예전엔 여기서 파서가 죽어서
            // **스크립트 전체**가 실행되지 않았다. this 가 클래스인 즉시실행 화살표로
            // desugar 해서 static 필드와 같은 순서로 실행한다.
            if is_static && self.peek() == Some(&Tok::LBrace) {
                let body = self.block()?;
                let f = Expr::Func {
                    name: None,
                    params: Vec::new(),
                    body,
                    is_arrow: true, // this 를 렉시컬 캡처 → 정적 초기화 스코프의 this = 클래스
                    is_generator: false,
                    is_async: false,
                    source: None, // 합성(static 블록) — toString 대상 아님
                };
                static_fields.push((
                    format!("\u{0}staticblock:{}", static_fields.len()),
                    Some(Expr::Call { callee: Box::new(f), args: Vec::new() }),
                ));
                continue;
            }
            // 메서드 소스 시작(§20.2.3.5): static 소비 뒤(메서드 toString 은 static 제외),
            // async/*/get/set 접두 포함.
            let method_start = self.pos;
            // async 메서드: async 뒤가 메서드 이름/`*` 이면 비동기 표시.
            let is_async = matches!(self.peek(), Some(Tok::Ident(w)) if w == "async")
                && matches!(
                    self.toks.get(self.pos + 1),
                    Some(Tok::Ident(_)) | Some(Tok::Str(_)) | Some(Tok::Star)
                );
            if is_async {
                self.pos += 1;
            }
            // 제너레이터 메서드: *name(){}
            let is_generator = self.eat(&Tok::Star);
            // get/set 접근자: get 은 접근자로 분리, set 은 소비만(할당 시 미호출 근사).
            // 단 get/set 바로 뒤가 '(' 면 이름이 get/set 인 메서드다.
            let mut accessor = None; // Some("get") | Some("set")
            if !is_async && !is_generator {
                if let Some(Tok::Ident(w)) = self.peek() {
                    if (w == "get" || w == "set")
                        && !matches!(self.toks.get(self.pos + 1), Some(Tok::LParen) | Some(Tok::Assign) | Some(Tok::Semi))
                    {
                        accessor = Some(w.clone());
                        self.pos += 1;
                    }
                }
            }
            let mname = self.member_name()?;
            // 클래스 필드: 이름 뒤가 '(' 가 아니면 메서드가 아니라 필드 (x = 5; / x;)
            if self.peek() != Some(&Tok::LParen) {
                let init = if self.eat(&Tok::Assign) { Some(self.assignment()?) } else { None };
                self.eat(&Tok::Semi);
                if is_static {
                    static_fields.push((mname, init));
                } else {
                    fields.push((mname, init));
                }
                continue;
            }
            let (params, mut body) = self.param_list()?;
            body.extend(self.block()?);
            let msrc = self.src_between(method_start, self.pos);
            if !is_static && mname == "constructor" {
                ctor = Some((params, body));
            } else if accessor.as_deref() == Some("get") {
                if is_static {
                    static_getters.push((mname, params, body, msrc));
                } else {
                    getters.push((mname, params, body, msrc));
                }
            } else if accessor.as_deref() == Some("set") {
                // setter 는 실제로 등록한다. 예전엔 조용히 버려서 obj.x = v 가
                // 아무 일도 안 했다 (검증 로직/프록시 패턴이 통째로 무력화).
                if is_static {
                    static_setters.push((mname, params, body, msrc));
                } else {
                    setters.push((mname, params, body, msrc));
                }
            } else if is_static {
                statics.push((mname, params, body, is_generator, is_async, msrc));
            } else {
                methods.push((mname, params, body, is_generator, is_async, msrc));
            }
        }
        self.pos += 1; // '}'
        let source = self.src_between(start, self.pos);
        Ok(ClassDef {
            name,
            parent,
            ctor,
            methods,
            statics,
            getters,
            setters,
            static_getters,
            static_setters,
            fields,
            static_fields,
            source,
        })
    }

    // 메서드/프로퍼티 이름: 식별자 또는 문자열/키워드
    fn member_name(&mut self) -> Result<String, String> {
        // 계산된 메서드 키 [expr] — 잘 알려진 심볼/리터럴을 정적으로 키에 매핑.
        // 예: [Symbol.iterator]() {} → "\u{0}@@iterator". 사용자 정의 이터러블 클래스 지원.
        if self.eat(&Tok::LBracket) {
            let e = self.assignment()?;
            self.expect(&Tok::RBracket)?;
            return Ok(computed_key_string(&e).unwrap_or_else(|| format!("\u{0}@@computed:{}", self.pos)));
        }
        match self.next()? {
            Tok::Ident(s) => Ok(s),
            Tok::Str(s) => Ok(s),
            other => keyword_word(&other)
                .ok_or_else(|| format!("메서드 이름이 필요한데 {:?}{}", other, self.ctx())),
        }
    }

    // 프로퍼티 접근 이름: 식별자 또는 예약어 (Symbol.for, x.default 등)
    fn prop_name(&mut self) -> Result<String, String> {
        match self.next()? {
            Tok::Ident(s) => Ok(s),
            other => keyword_word(&other)
                .ok_or_else(|| format!("프로퍼티 이름이 필요한데 {:?}{}", other, self.ctx())),
        }
    }

    fn primary(&mut self) -> Result<Expr, String> {
        match self.next()? {
            Tok::Num(n) => Ok(Expr::Num(n)),
            Tok::BigInt(d) => Ok(Expr::BigInt(d)),
            Tok::Str(s) => Ok(Expr::Str(s)),
            Tok::Regex(source, flags) => Ok(Expr::Regex { source, flags }),
            Tok::Template(parts) => {
                let mut out = Vec::new();
                for part in parts {
                    out.push(match part {
                        TplPart::Lit(s, _) => TemplatePart::Lit(s),
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
                        if self.eat_spread() {
                            items.push(Expr::Spread(Box::new(self.assignment()?)));
                        } else {
                            items.push(self.assignment()?);
                        }
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
                        // 스프레드 { ...obj }
                        if self.eat_spread() {
                            let e = self.assignment()?;
                            props.push((PropKey::Spread, e));
                            if self.eat(&Tok::Comma) {
                                if self.eat(&Tok::RBrace) {
                                    break;
                                }
                                continue;
                            }
                            self.expect(&Tok::RBrace)?;
                            break;
                        }
                        // 계산된 키 { [expr]: v } 또는 계산 메서드 { [expr]() {} }
                        // (제너레이터 *[expr]()/async [expr]() 접두 포함). 키 식은 런타임 평가.
                        let comp_gen = self.peek() == Some(&Tok::Star)
                            && self.toks.get(self.pos + 1) == Some(&Tok::LBracket);
                        let comp_async = matches!(self.peek(), Some(Tok::Ident(w)) if w == "async")
                            && self.toks.get(self.pos + 1) == Some(&Tok::LBracket);
                        if comp_gen || comp_async || self.peek() == Some(&Tok::LBracket) {
                            let ms = self.pos;
                            let is_gen = self.eat(&Tok::Star);
                            let is_async = comp_async && {
                                self.pos += 1;
                                true
                            };
                            self.pos += 1; // '['
                            let key_expr = self.assignment()?;
                            self.expect(&Tok::RBracket)?;
                            let value = if self.eat(&Tok::Colon) {
                                self.assignment()?
                            } else {
                                // 계산 메서드 단축 { [k]() {} }
                                let (params, mut body) = self.param_list()?;
                                body.extend(self.block()?);
                                let source = self.src_between(ms, self.pos);
                                Expr::Func {
                                    name: None,
                                    params,
                                    body,
                                    is_arrow: false,
                                    is_generator: is_gen,
                                    is_async,
                                    source,
                                }
                            };
                            props.push((PropKey::Computed(Box::new(key_expr)), value));
                            if self.eat(&Tok::Comma) {
                                if self.eat(&Tok::RBrace) {
                                    break;
                                }
                                continue;
                            }
                            self.expect(&Tok::RBrace)?;
                            break;
                        }
                        // 접근자 { get x(){..} } / { set x(v){..} }.
                        // 이름은 식별자·문자열·숫자뿐 아니라 예약어({ get class(){} })도 되고,
                        // 계산 키({ get [expr](){} })도 된다. 셋 다 지원해야 번들이 파싱된다.
                        let is_acc =
                            matches!(self.peek(), Some(Tok::Ident(w)) if w == "get" || w == "set");
                        let acc_named = is_acc
                            && matches!(self.toks.get(self.pos + 1),
                                Some(Tok::Ident(_) | Tok::Str(_) | Tok::Num(_)))
                            && self.toks.get(self.pos + 2) == Some(&Tok::LParen);
                        // 예약어 이름: get class() / get default() 등
                        let acc_keyword = is_acc
                            && self
                                .toks
                                .get(self.pos + 1)
                                .map_or(false, |t| keyword_word(t).is_some())
                            && self.toks.get(self.pos + 2) == Some(&Tok::LParen);
                        // 계산 키: get [expr]() — 매칭 ']' 뒤가 '(' 여야 접근자다
                        let acc_computed = is_acc
                            && self.toks.get(self.pos + 1) == Some(&Tok::LBracket)
                            && self
                                .matching_bracket(self.pos + 1)
                                .map_or(false, |c| self.toks.get(c + 1) == Some(&Tok::LParen));
                        if acc_named || acc_keyword || acc_computed {
                            let ms = self.pos;
                            let is_get = matches!(self.peek(), Some(Tok::Ident(w)) if w == "get");
                            self.pos += 1; // get/set
                            // 계산 키는 런타임 평가를 위해 식 자체를 보존한다.
                            let computed_key = if acc_computed {
                                self.pos += 1; // '['
                                let e = self.assignment()?;
                                self.expect(&Tok::RBracket)?;
                                Some(e)
                            } else {
                                None
                            };
                            let name = match &computed_key {
                                Some(_) => String::new(),
                                None => self.member_name()?,
                            };
                            let (params, mut body) = self.param_list()?;
                            body.extend(self.block()?);
                            let source = self.src_between(ms, self.pos);
                            let f = Expr::Func {
                                name: None,
                                params,
                                body,
                                is_arrow: false,
                                is_generator: false,
                                is_async: false,
                                source,
                            };
                            // get/set 둘 다 보존한다. 예전엔 setter 를 버려서 대입이
                            // 조용히 setter 를 우회했다(부작용이 안 일어남).
                            match (is_get, computed_key) {
                                (true, Some(e)) => {
                                    props.push((PropKey::ComputedGetter(Box::new(e)), f))
                                }
                                (true, None) => props.push((PropKey::Getter(name), f)),
                                (false, Some(e)) => {
                                    props.push((PropKey::ComputedSetter(Box::new(e)), f))
                                }
                                (false, None) => props.push((PropKey::Setter(name), f)),
                            }
                            if self.eat(&Tok::Comma) {
                                if self.eat(&Tok::RBrace) {
                                    break;
                                }
                                continue;
                            }
                            self.expect(&Tok::RBrace)?;
                            break;
                        }
                        // 제너레이터/async 메서드 단축 { *gen(){}, async foo(){}, async *bar(){} }.
                        // async 가 프로퍼티명/메서드명({async:1}/{async(){}})인 경우는 제외.
                        let obj_gen = self.peek() == Some(&Tok::Star);
                        let obj_async = matches!(self.peek(), Some(Tok::Ident(w)) if w == "async")
                            && !matches!(
                                self.toks.get(self.pos + 1),
                                Some(Tok::Colon) | Some(Tok::Comma) | Some(Tok::RBrace)
                                    | Some(Tok::LParen) | Some(Tok::Assign)
                            );
                        if obj_gen || obj_async {
                            let ms = self.pos;
                            let is_async = obj_async && {
                                self.pos += 1;
                                true
                            };
                            let is_gen = self.eat(&Tok::Star); // async *name / *name
                            let key = self.member_name()?;
                            let (params, mut body) = self.param_list()?;
                            body.extend(self.block()?);
                            let source = self.src_between(ms, self.pos);
                            let f = Expr::Func {
                                name: None,
                                params,
                                body,
                                is_arrow: false,
                                is_generator: is_gen,
                                is_async,
                                source,
                            };
                            props.push((PropKey::Static(key), f));
                            if self.eat(&Tok::Comma) {
                                if self.eat(&Tok::RBrace) {
                                    break;
                                }
                                continue;
                            }
                            self.expect(&Tok::RBrace)?;
                            break;
                        }
                        let key_start = self.pos;
                        let key = match self.next()? {
                            Tok::Ident(s) => s,
                            Tok::Str(s) => s,
                            Tok::Num(n) => n.to_string(),
                            // 예약어를 키로: { return: 1, class: 2 } (미니파이 코드에 흔함)
                            ref other if keyword_word(other).is_some() => {
                                keyword_word(other).unwrap()
                            }
                            other => {
                                return Err(format!("객체 키가 아님: {:?}{}", other, self.ctx()))
                            }
                        };
                        let value = if self.eat(&Tok::Colon) {
                            self.assignment()?
                        } else if self.peek() == Some(&Tok::LParen) {
                            // 메서드 단축 { foo(a) { ... } }
                            let (params, mut body) = self.param_list()?;
                            body.extend(self.block()?);
                            let source = self.src_between(key_start, self.pos);
                            Expr::Func { name: None, params, body, is_arrow: false, is_generator: false, is_async: false, source }
                        } else if self.peek() == Some(&Tok::Assign) {
                            // CoverInitializedName: { a = 1 } — 객체 리터럴로는 문법 오류지만
                            // **구조분해 대입 대상**으로는 유효하다: ({a = 1} = o).
                            // 예전엔 파서가 여기서 죽어서 그 스크립트가 통째로 못 돌았다.
                            self.pos += 1;
                            let default = self.assignment()?;
                            Expr::Assign {
                                op: AssignOp::Set,
                                target: Box::new(Expr::Ident(key.clone())),
                                value: Box::new(default),
                            }
                        } else {
                            Expr::Ident(key.clone()) // 단축 프로퍼티 { a }
                        };
                        props.push((PropKey::Static(key), value));
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
                // 함수 식. function* 는 제너레이터. 이름 있으면 재귀용 자기 참조로 보존.
                // 소스 시작: 이미 소비된 `function`(pos-1), async 접두 있으면 그 앞.
                let start = self.pos.saturating_sub(1 + if self.pending_async { 1 } else { 0 });
                let is_async = std::mem::take(&mut self.pending_async);
                let is_generator = self.eat(&Tok::Star);
                let name = if let Some(Tok::Ident(n)) = self.peek() {
                    let n = n.clone();
                    self.pos += 1;
                    Some(n)
                } else {
                    None
                };
                let (params, mut body) = self.param_list()?;
                body.extend(self.block()?);
                let source = self.src_between(start, self.pos);
                Ok(Expr::Func { name, params, body, is_arrow: false, is_generator, is_async, source })
            }
            Tok::This => Ok(Expr::This),
            Tok::Super => Ok(Expr::Super),
            Tok::New => {
                // new.target 메타 프로퍼티
                if self.peek() == Some(&Tok::Dot) {
                    self.pos += 1; // '.'
                    match self.next()? {
                        Tok::Ident(w) if w == "target" => return Ok(Expr::NewTarget),
                        other => {
                            return Err(format!("new. 뒤엔 target 만: {:?}{}", other, self.ctx()))
                        }
                    }
                }
                // new Callee(args) — callee 는 멤버 접근까지 (호출은 별도)
                let callee = self.new_callee()?;
                let args = if self.peek() == Some(&Tok::LParen) {
                    self.arg_list()?
                } else {
                    Vec::new() // new Foo (괄호 생략)
                };
                Ok(Expr::New { callee: Box::new(callee), args })
            }
            Tok::Class => {
                let def = self.class_def(false)?;
                Ok(Expr::Class(Box::new(def)))
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
            Expr::Func { params, body, is_arrow, .. } => {
                assert_eq!(params, vec!["x"]);
                assert!(is_arrow);
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
                assert_eq!(props[2].0, PropKey::Static("c".to_string()));
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
