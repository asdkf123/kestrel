mod media;
mod shorthand;
mod values;

use media::media_matches;
use shorthand::expand_declaration;
use values::valid_identifier_char;

#[derive(Debug, PartialEq)]
pub struct Stylesheet {
    pub rules: Vec<Rule>,
}

#[derive(Debug, PartialEq)]
pub struct Rule {
    pub selectors: Vec<Selector>,
    pub declarations: Vec<Declaration>,
}

#[derive(Debug, PartialEq)]
pub enum Selector {
    Simple(SimpleSelector),
    // 결합자 체인: [(결합자, 단순), ...]. 첫 항목의 결합자는 무시(대상 기준).
    // 예: ".a > .b" → [(Descendant, .a), (Child, .b)]. 마지막이 대상.
    Complex(Vec<(Combinator, SimpleSelector)>),
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Combinator {
    Descendant,   // 공백
    Child,        // >
    NextSibling,  // +
    LaterSibling, // ~
}

// 의사 클래스. 구조적(위치)과 동적(상호작용) 구분.
#[derive(Debug, PartialEq, Clone)]
pub enum Pseudo {
    FirstChild,
    LastChild,
    OnlyChild,
    NthChild(i32, i32), // an+b
    Not(Vec<SimpleSelector>),
    Root,
    Empty,
    Dynamic, // hover/focus/active/visited 등 — 정적 렌더에선 비매칭
}

// 속성 선택자 연산자.
#[derive(Debug, PartialEq, Clone)]
pub enum AttrOp {
    Exists,           // [attr]
    Equals(String),   // [attr=v]
    Prefix(String),   // [attr^=v]
    Suffix(String),   // [attr$=v]
    Contains(String), // [attr*=v]
    Word(String),     // [attr~=v] (공백 구분 목록에 v)
    Dash(String),     // [attr|=v] (v 또는 v-...)
}

#[derive(Debug, PartialEq, Clone)]
pub struct SimpleSelector {
    pub tag_name: Option<String>,
    pub id: Option<String>,
    pub class: Vec<String>,
    // 속성 선택자: (이름, 연산자).
    pub attrs: Vec<(String, AttrOp)>,
    pub pseudos: Vec<Pseudo>,
}

#[derive(Debug, PartialEq)]
pub struct Declaration {
    pub name: String,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Keyword(String),
    Length(f32, Unit),
    Color(Color),
    Url(String),
    // var() 를 포함한 미해석 원문. 스타일 계산 시 커스텀 프로퍼티로 치환 후 재파싱.
    Var(String),
    // calc() 를 (percent 계수, px 계수) 선형식으로 축약. 레이아웃이 len_px 로 해석.
    Calc(f32, f32),
    // linear-gradient. 페인트가 축을 따라 색 보간.
    Gradient(Gradient),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Gradient {
    pub angle_deg: f32,          // CSS 각도 (0=위, 90=오른쪽, 180=아래). radial 이면 무시.
    pub radial: bool,            // true=radial(중심에서 방사), false=linear
    pub stops: Vec<(Color, f32)>, // (색, 위치 0-1)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Unit {
    Px,
    Em,      // 부모 font-size 배수
    Rem,     // 루트 font-size 배수
    Percent, // 문맥 의존 (현재 font-size 에서만 해석)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

pub type Specificity = (usize, usize, usize);

impl Selector {
    // 대상(가장 오른쪽) 단순 선택자
    pub fn subject(&self) -> &SimpleSelector {
        match self {
            Selector::Simple(s) => s,
            Selector::Complex(v) => &v.last().unwrap().1,
        }
    }

    fn each_simple(&self) -> Vec<&SimpleSelector> {
        match self {
            Selector::Simple(s) => vec![s],
            Selector::Complex(v) => v.iter().map(|(_, s)| s).collect(),
        }
    }

    pub fn specificity(&self) -> Specificity {
        let parts = self.each_simple();
        // 의사 클래스는 클래스와 동일 특이도(:not 은 인자 기준이나 근사)
        let a = parts.iter().map(|s| s.id.iter().count()).sum();
        let b = parts
            .iter()
            .map(|s| s.class.len() + s.attrs.len() + s.pseudos.len())
            .sum();
        let c = parts.iter().map(|s| s.tag_name.iter().count()).sum();
        (a, b, c)
    }
}

impl Value {
    pub fn to_px(&self) -> f32 {
        match *self {
            Value::Length(f, Unit::Px) => f,
            _ => 0.0,
        }
    }
}

// @media 없는 시트/테스트/UA 용 기본 파스. 데스크톱 폭(1024)으로 미디어 평가.
pub fn parse(source: String) -> Stylesheet {
    parse_viewport(source, 1024.0)
}

// 뷰포트 폭을 알고 파스 — @media (min/max-width) 를 이 폭에 대해 평가해
// 매칭되는 규칙만 포함한다. 페이지 스타일시트는 실제 뷰포트 폭으로 호출.
pub fn parse_viewport(source: String, viewport_width: f32) -> Stylesheet {
    let mut parser = Parser { pos: 0, input: source, viewport_width };
    Stylesheet { rules: parser.parse_rules() }
}

// 인라인 style="..." 속성값(선언 블록, 중괄호 없음)을 선언 목록으로 파싱.
// 캐스케이드에서 어떤 선택자보다 높은 우선순위 (스타일 적용 시 마지막에 얹음).
// nth 인자 파싱: "2n+1"/"odd"/"even"/"3"/"n"/"-n+3" → (a, b) 의 an+b.
fn parse_nth(s: &str) -> Option<(i32, i32)> {
    let s: String = s.trim().to_ascii_lowercase().split_whitespace().collect();
    match s.as_str() {
        "odd" => return Some((2, 1)),
        "even" => return Some((2, 0)),
        _ => {}
    }
    if let Some(np) = s.find('n') {
        let a = match &s[..np] {
            "" | "+" => 1,
            "-" => -1,
            a => a.parse().ok()?,
        };
        let b_str = s[np + 1..].trim_start_matches('+');
        let b = if b_str.is_empty() { 0 } else { b_str.parse().ok()? };
        Some((a, b))
    } else {
        Some((0, s.parse().ok()?))
    }
}

pub fn parse_inline_style(text: &str) -> Vec<Declaration> {
    let mut parser = Parser { pos: 0, input: text.to_string(), viewport_width: 0.0 };
    parser.parse_declarations()
}

// var() 참조를 커스텀 프로퍼티로 치환 → 재파싱해 확정 선언들을 낸다.
// custom: 요소의 계산된 커스텀 프로퍼티 맵(--name → 원문 값). 미해석이면 빈 Vec.
pub(crate) fn resolve_var(
    name: &str,
    raw: &str,
    custom: &std::collections::HashMap<String, String>,
) -> Vec<Declaration> {
    let substituted = substitute_var(raw, custom, 0);
    if substituted.contains("var(") {
        return Vec::new(); // 여전히 미해석(정의 안 됨 + fallback 없음) → 드롭
    }
    expand_declaration(name, substituted.trim())
}

// 문자열 안의 var(--name[, fallback]) 을 커스텀 프로퍼티 값으로 치환 (중첩 8단계까지).
fn substitute_var(raw: &str, custom: &std::collections::HashMap<String, String>, depth: u32) -> String {
    if depth > 8 || !raw.contains("var(") {
        return raw.to_string();
    }
    let mut out = String::new();
    let mut rest = raw;
    while let Some(pos) = rest.find("var(") {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 4..];
        // 괄호 짝 찾기
        let mut depth_p = 1i32;
        let mut end = after.len();
        for (i, c) in after.char_indices() {
            match c {
                '(' => depth_p += 1,
                ')' => {
                    depth_p -= 1;
                    if depth_p == 0 {
                        end = i;
                        break;
                    }
                }
                _ => {}
            }
        }
        let inner = &after[..end];
        // "--name" 또는 "--name, fallback"
        let (var_name, fallback) = match inner.find(',') {
            Some(ci) => (inner[..ci].trim(), Some(inner[ci + 1..].trim())),
            None => (inner.trim(), None),
        };
        let resolved = match custom.get(var_name) {
            Some(v) => substitute_var(v, custom, depth + 1),
            None => match fallback {
                Some(f) => substitute_var(f, custom, depth + 1),
                None => {
                    // 미해석 표시(재파싱에서 드롭되게 var( 유지)
                    out.push_str("var(");
                    out.push_str(inner);
                    out.push(')');
                    rest = &after[end + 1..];
                    continue;
                }
            },
        };
        out.push_str(&resolved);
        rest = &after[(end + 1).min(after.len())..];
    }
    out.push_str(rest);
    out
}

// UA 기본 스타일시트. HTML 표준 §15 Rendering 을 근거로 함
// (https://html.spec.whatwg.org/multipage/rendering.html). 표준은 폼 컨트롤을
// appearance:auto(네이티브 위젯)로 두지만, 우리는 appearance 미구현이라 기본
// 테두리/배경을 여기 CSS 로 넣는다 — 캐스케이드상 저작자 CSS 가 덮을 수 있어
// 하드코딩(무조건 그림)과 달리 구글 등의 커스텀 스타일이 이긴다.
// 테이블 계열은 진짜 테이블 레이아웃 전까지 block 으로 근사(레이아웃은 tr 태그로 분기).
const UA_CSS: &str = "html, body, div, p, h1, h2, h3, h4, h5, h6, ul, ol, li, section, article, header, footer, nav, main, aside, blockquote, pre, table, thead, tbody, tfoot, tr, td, th, caption, center, form, fieldset, hr, figure, figcaption, address, dl, dt, dd, select, textarea { display: block; } head, script, style, title, meta, link, noscript, template { display: none; } img { display: block; } a { color: #0645ad; } ul, ol { padding-left: 24px; } li { padding-left: 18px; } td, th { padding: 4px 6px; } th { color: #202020; } center { text-align: center; } table { text-align: left; } input, button { display: inline-block; } input, textarea, select { border: 1px solid #767676; background-color: #ffffff; padding: 2px 5px; } button, input[type=submit], input[type=reset], input[type=button] { border: 1px solid #767676; background-color: #e9e9ed; padding: 2px 8px; text-align: center; } b, strong, h1, h2, h3, h4, h5, h6, th { font-weight: bold; } i, em, cite, var, address { font-style: italic; }";

pub fn user_agent_stylesheet() -> Stylesheet {
    parse(UA_CSS.to_string())
}

// querySelector 용: 선택자 목록만 파싱. 빈 규칙 몸통을 붙여 기존 파서를 재사용.
// 미지원 선택자(:hover, > 등)면 None (관용).
pub fn parse_selector_list(text: &str) -> Option<Vec<Selector>> {
    let ss = parse(format!("{} {{}}", text));
    ss.rules.into_iter().next().map(|r| r.selectors)
}

struct Parser {
    pos: usize,
    input: String,
    viewport_width: f32,
}

impl Parser {
    fn parse_rules(&mut self) -> Vec<Rule> {
        let mut rules = Vec::new();
        loop {
            self.consume_whitespace();
            if self.eof() {
                break;
            }
            if self.peek() == Some('@') {
                self.consume_char(); // '@'
                let ident = self.parse_identifier().to_ascii_lowercase();
                if ident == "media" {
                    let media_rules = self.parse_media_block();
                    rules.extend(media_rules);
                } else {
                    self.skip_at_rule(); // 그 외 @rule 은 스킵 (';' or {block})
                }
                continue;
            }
            if let Some(rule) = self.parse_rule() {
                rules.push(rule);
            }
        }
        rules
    }

    // '@media' 뒤: 조건 텍스트 → '{' → 내부 규칙들 → '}'. 조건이 뷰포트에 맞으면
    // 내부 규칙을 반환, 아니면 빈 목록. (내부 규칙은 항상 파싱해 위치를 넘겨야 함)
    fn parse_media_block(&mut self) -> Vec<Rule> {
        let query = self.consume_while(|c| c != '{' && c != ';' && c != '}');
        if self.peek() != Some('{') {
            if self.peek() == Some(';') {
                self.consume_char();
            }
            return Vec::new();
        }
        self.consume_char(); // '{'
        let mut inner = Vec::new();
        loop {
            self.consume_whitespace();
            match self.peek() {
                None => break,
                Some('}') => {
                    self.consume_char();
                    break;
                }
                Some('@') => self.skip_at_rule(), // 중첩 @rule 은 스킵
                _ => {
                    if let Some(r) = self.parse_rule() {
                        inner.push(r);
                    }
                }
            }
        }
        if media_matches(&query, self.viewport_width) {
            inner
        } else {
            Vec::new()
        }
    }

    fn parse_rule(&mut self) -> Option<Rule> {
        match self.parse_selectors() {
            Some(selectors) => {
                let declarations = self.parse_declarations();
                Some(Rule { selectors, declarations })
            }
            None => {
                self.skip_to_block_end();
                None
            }
        }
    }

    fn parse_selectors(&mut self) -> Option<Vec<Selector>> {
        let mut selectors = Vec::new();
        loop {
            selectors.push(self.parse_complex_selector()?);
            match self.peek() {
                Some(',') => {
                    self.consume_char();
                    self.consume_whitespace();
                }
                Some('{') => {
                    self.consume_char();
                    break;
                }
                // 파싱 실패(미지원 구문 잔여) → 규칙 스킵
                _ => return None,
            }
        }
        selectors.sort_by(|a, b| b.specificity().cmp(&a.specificity()));
        Some(selectors)
    }

    // 결합자 체인: 단순 선택자를 공백/`>`/`+`/`~` 로 이음. 종료 후 peek 은 ','/'{'.
    fn parse_complex_selector(&mut self) -> Option<Selector> {
        let mut parts = vec![(Combinator::Descendant, self.parse_simple_selector()?)];
        loop {
            let had_ws = self.peek().map(|c| c.is_whitespace()).unwrap_or(false);
            self.consume_whitespace();
            let combinator = match self.peek() {
                Some('>') => {
                    self.consume_char();
                    self.consume_whitespace();
                    Combinator::Child
                }
                Some('+') => {
                    self.consume_char();
                    self.consume_whitespace();
                    Combinator::NextSibling
                }
                Some('~') => {
                    self.consume_char();
                    self.consume_whitespace();
                    Combinator::LaterSibling
                }
                Some(c)
                    if had_ws
                        && (c == '.'
                            || c == '#'
                            || c == '*'
                            || c == '['
                            || c == ':'
                            || valid_identifier_char(c)) =>
                {
                    Combinator::Descendant
                }
                _ => break,
            };
            parts.push((combinator, self.parse_simple_selector()?));
        }
        if parts.len() == 1 {
            Some(Selector::Simple(parts.pop().unwrap().1))
        } else {
            Some(Selector::Complex(parts))
        }
    }

    // 하나의 compound 선택자(태그/id/class/속성/의사클래스 조합). 없으면 None.
    fn parse_simple_selector(&mut self) -> Option<SimpleSelector> {
        let mut selector = SimpleSelector {
            tag_name: None,
            id: None,
            class: Vec::new(),
            attrs: Vec::new(),
            pseudos: Vec::new(),
        };
        let mut any = false;
        while !self.eof() {
            match self.input[self.pos..].chars().next().unwrap() {
                '#' => {
                    self.consume_char();
                    selector.id = Some(self.parse_identifier());
                    any = true;
                }
                '.' => {
                    self.consume_char();
                    selector.class.push(self.parse_identifier());
                    any = true;
                }
                '*' => {
                    self.consume_char();
                    any = true;
                }
                '[' => {
                    if let Some(attr) = self.parse_attr_selector() {
                        selector.attrs.push(attr);
                        any = true;
                    } else {
                        return None;
                    }
                }
                ':' => {
                    self.consume_char();
                    if self.peek() == Some(':') {
                        self.consume_char(); // ::pseudo-element → 의사요소, 매칭 대상 아님(Dynamic 근사)
                    }
                    match self.parse_pseudo() {
                        Some(p) => selector.pseudos.push(p),
                        None => return None,
                    }
                    any = true;
                }
                c if valid_identifier_char(c) => {
                    selector.tag_name = Some(self.parse_identifier());
                    any = true;
                }
                _ => break,
            }
        }
        if any {
            Some(selector)
        } else {
            None
        }
    }

    // 의사 클래스 파싱. 함수형(nth-child(..)/not(..))과 키워드형.
    fn parse_pseudo(&mut self) -> Option<Pseudo> {
        let name = self.parse_identifier().to_ascii_lowercase();
        // 함수형: 괄호 안 인자
        if self.peek() == Some('(') {
            self.consume_char();
            let arg = self.consume_while(|c| c != ')');
            if self.peek() == Some(')') {
                self.consume_char();
            }
            return match name.as_str() {
                "nth-child" | "nth-of-type" => {
                    let (a, b) = parse_nth(arg.trim())?;
                    Some(Pseudo::NthChild(a, b))
                }
                "not" => {
                    // :not(단순) — 단일 compound 만 (근사)
                    let mut inner = Parser { pos: 0, input: arg, viewport_width: 0.0 };
                    let s = inner.parse_simple_selector()?;
                    Some(Pseudo::Not(vec![s]))
                }
                _ => Some(Pseudo::Dynamic), // is/where/lang 등 미지원 → 근사
            };
        }
        Some(match name.as_str() {
            "first-child" => Pseudo::FirstChild,
            "last-child" => Pseudo::LastChild,
            "only-child" => Pseudo::OnlyChild,
            "first-of-type" => Pseudo::FirstChild, // 타입 구분 근사
            "last-of-type" => Pseudo::LastChild,
            "root" => Pseudo::Root,
            "empty" => Pseudo::Empty,
            // 상호작용/링크 상태 → 정적 렌더에선 비매칭
            _ => Pseudo::Dynamic,
        })
    }

    // [name] 또는 [name=value] / [name="value"]. =/따옴표 파싱, 그 외 연산자(~= 등)는
    // value 없는 존재 검사로 관용 처리. name 은 소문자화.
    fn parse_attr_selector(&mut self) -> Option<(String, AttrOp)> {
        self.consume_char(); // '['
        self.consume_whitespace();
        let name = self.parse_identifier().to_ascii_lowercase();
        self.consume_whitespace();
        // 연산자: = ^= $= *= ~= |=
        let op_char = self.peek();
        let op = if op_char == Some('=') {
            self.consume_char();
            Some(' ')
        } else if matches!(op_char, Some('^' | '$' | '*' | '~' | '|'))
            && self.input[self.pos..].chars().nth(1) == Some('=')
        {
            let c = op_char.unwrap();
            self.consume_char();
            self.consume_char(); // '='
            Some(c)
        } else {
            None
        };
        let attr_op = match op {
            None => AttrOp::Exists,
            Some(c) => {
                self.consume_whitespace();
                let v = self.parse_attr_value();
                match c {
                    '^' => AttrOp::Prefix(v),
                    '$' => AttrOp::Suffix(v),
                    '*' => AttrOp::Contains(v),
                    '~' => AttrOp::Word(v),
                    '|' => AttrOp::Dash(v),
                    _ => AttrOp::Equals(v),
                }
            }
        };
        // ']' 까지 소비 (i 플래그 등 잔여 포함)
        self.consume_while(|c| c != ']');
        if self.peek() == Some(']') {
            self.consume_char();
        }
        if name.is_empty() {
            None
        } else {
            Some((name, attr_op))
        }
    }

    // 속성값: 따옴표 있으면 그 안, 없으면 다음 공백/']' 까지.
    fn parse_attr_value(&mut self) -> String {
        match self.peek() {
            Some(q @ ('"' | '\'')) => {
                self.consume_char();
                let v = self.consume_while(|c| c != q);
                if self.peek() == Some(q) {
                    self.consume_char();
                }
                v
            }
            _ => self.consume_while(|c| c != ']' && !c.is_whitespace()),
        }
    }

    fn parse_declarations(&mut self) -> Vec<Declaration> {
        // '{' 는 parse_selectors 에서 이미 소비됨
        let mut declarations = Vec::new();
        loop {
            self.consume_whitespace();
            match self.peek() {
                None => break,
                Some('}') => {
                    self.consume_char();
                    break;
                }
                _ => {
                    declarations.extend(self.parse_declaration());
                }
            }
        }
        declarations
    }

    fn parse_declaration(&mut self) -> Vec<Declaration> {
        let name = self.parse_identifier().trim().to_ascii_lowercase();
        self.consume_whitespace();
        if self.peek() != Some(':') {
            self.skip_to_decl_end();
            return Vec::new();
        }
        self.consume_char(); // ':'
        self.consume_whitespace();
        let value_text = self.consume_while(|c| c != ';' && c != '}');
        if self.peek() == Some(';') {
            self.consume_char();
        }
        if name.is_empty() {
            return Vec::new();
        }
        expand_declaration(&name, value_text.trim())
    }

    fn skip_to_decl_end(&mut self) {
        self.consume_while(|c| c != ';' && c != '}');
        if self.peek() == Some(';') {
            self.consume_char();
        }
    }

    fn skip_at_rule(&mut self) {
        while !self.eof() {
            let c = self.consume_char();
            if c == ';' {
                return;
            }
            if c == '{' {
                self.skip_block();
                return;
            }
        }
    }

    fn skip_to_block_end(&mut self) {
        while !self.eof() {
            let c = self.consume_char();
            if c == '{' {
                self.skip_block();
                return;
            }
            if c == '}' {
                return;
            }
        }
    }

    fn skip_block(&mut self) {
        // 여는 '{' 는 이미 소비됨
        let mut depth = 1;
        while !self.eof() && depth > 0 {
            match self.consume_char() {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
        }
    }

    fn parse_identifier(&mut self) -> String {
        self.consume_while(valid_identifier_char)
    }

    fn peek(&self) -> Option<char> {
        self.input[self.pos..].chars().next()
    }

    fn eof(&self) -> bool {
        self.pos >= self.input.len()
    }

    fn consume_char(&mut self) -> char {
        let mut iter = self.input[self.pos..].char_indices();
        let (_, cur_char) = iter.next().unwrap();
        let (next_pos, _) = iter.next().unwrap_or((1, ' '));
        self.pos += next_pos;
        cur_char
    }

    fn consume_while<F>(&mut self, test: F) -> String
    where
        F: Fn(char) -> bool,
    {
        let mut result = String::new();
        while !self.eof() && test(self.peek().unwrap()) {
            result.push(self.consume_char());
        }
        result
    }

    fn consume_whitespace(&mut self) {
        self.consume_while(char::is_whitespace);
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rule_with_length_and_color() {
        let ss = parse("div { width: 100px; background-color: #ff0000; }".to_string());
        assert_eq!(ss.rules.len(), 1);
        let rule = &ss.rules[0];
        assert_eq!(rule.declarations.len(), 2);
        assert_eq!(rule.declarations[0].name, "width");
        assert_eq!(rule.declarations[0].value, Value::Length(100.0, Unit::Px));
        assert_eq!(rule.declarations[1].value, Value::Color(Color { r: 255, g: 0, b: 0, a: 255 }));
    }

    #[test]
    fn parses_compound_selector() {
        let ss = parse("p.note { color: #112233; }".to_string());
        match &ss.rules[0].selectors[0] {
            Selector::Simple(s) => {
                assert_eq!(s.tag_name.as_deref(), Some("p"));
                assert_eq!(s.class, vec!["note".to_string()]);
                assert_eq!(s.id, None);
            }
            other => panic!("expected Simple, got {:?}", other),
        }
    }

    #[test]
    fn specificity_counts_id_class_tag() {
        let ss = parse("#x { color: #000000; }".to_string());
        assert_eq!(ss.rules[0].selectors[0].specificity(), (1, 0, 0));
    }

    #[test]
    fn skips_non_media_at_rules() {
        // @font-face 등은 여전히 스킵, 뒤 규칙은 파싱
        let ss = parse("@font-face { font-family: x; } div { width: 5px; }".to_string());
        assert_eq!(ss.rules.len(), 1);
        assert_eq!(ss.rules[0].declarations[0].name, "width");
    }

    #[test]
    fn media_min_width_included_when_viewport_wide_enough() {
        // 뷰포트 1000 → min-width:768 매칭 → 내부 규칙 포함
        let ss = parse_viewport(
            "@media (min-width: 768px) { p { color: #ff0000; } } div { width: 5px; }".to_string(),
            1000.0,
        );
        assert_eq!(ss.rules.len(), 2);
        assert!(ss.rules.iter().any(|r| r.declarations.iter().any(|d| d.name == "color")));
    }

    #[test]
    fn media_min_width_excluded_when_viewport_too_narrow() {
        // 뷰포트 600 → min-width:768 불일치 → 내부 규칙 드롭
        let ss = parse_viewport(
            "@media (min-width: 768px) { p { color: #ff0000; } } div { width: 5px; }".to_string(),
            600.0,
        );
        assert_eq!(ss.rules.len(), 1);
        assert_eq!(ss.rules[0].declarations[0].name, "width");
    }

    #[test]
    fn media_max_width_and_print() {
        // max-width:600 은 뷰포트 500 에서 매칭
        let ss = parse_viewport("@media (max-width: 600px) { p { width: 1px; } }".to_string(), 500.0);
        assert_eq!(ss.rules.len(), 1);
        // print 전용은 화면(어떤 폭이든)에서 제외
        let ss2 = parse_viewport("@media print { p { width: 1px; } }".to_string(), 1000.0);
        assert_eq!(ss2.rules.len(), 0);
    }

    #[test]
    fn parses_child_combinator() {
        // '>' 자식 결합자 지원 → [(Descendant, .a), (Child, .b)]
        let ss = parse(".a > .b { color: #ff0000; }".to_string());
        assert_eq!(ss.rules.len(), 1);
        match &ss.rules[0].selectors[0] {
            Selector::Complex(parts) => {
                assert_eq!(parts.len(), 2);
                assert_eq!(parts[1].0, Combinator::Child);
            }
            other => panic!("expected Complex, got {:?}", other),
        }
    }

    #[test]
    fn parses_descendant_selector_chain() {
        let ss = parse("div .note p { width: 5px; }".to_string());
        match &ss.rules[0].selectors[0] {
            Selector::Complex(parts) => {
                assert_eq!(parts.len(), 3);
                assert_eq!(parts[0].1.tag_name.as_deref(), Some("div"));
                assert_eq!(parts[1].0, Combinator::Descendant);
                assert_eq!(parts[1].1.class, vec!["note".to_string()]);
                assert_eq!(parts[2].1.tag_name.as_deref(), Some("p"));
            }
            other => panic!("expected Complex, got {:?}", other),
        }
    }

    #[test]
    fn descendant_specificity_sums_parts() {
        let ss = parse("#a .b p { width: 1px; }".to_string());
        assert_eq!(ss.rules[0].selectors[0].specificity(), (1, 1, 1));
    }

    #[test]
    fn parses_named_color() {
        let ss = parse("p { color: red; }".to_string());
        assert_eq!(
            ss.rules[0].declarations[0].value,
            Value::Color(Color { r: 255, g: 0, b: 0, a: 255 })
        );
    }

    #[test]
    fn parses_short_hex_color() {
        let ss = parse("p { color: #f80; }".to_string());
        // #f80 → #ff8800
        assert_eq!(
            ss.rules[0].declarations[0].value,
            Value::Color(Color { r: 255, g: 136, b: 0, a: 255 })
        );
    }

    #[test]
    fn parses_rgb_function() {
        let ss = parse("p { color: rgb(1, 2, 3); }".to_string());
        assert_eq!(
            ss.rules[0].declarations[0].value,
            Value::Color(Color { r: 1, g: 2, b: 3, a: 255 })
        );
    }

    #[test]
    fn parses_rgba_function_alpha() {
        let ss = parse("p { color: rgba(10, 20, 30, 0.5); }".to_string());
        assert_eq!(
            ss.rules[0].declarations[0].value,
            Value::Color(Color { r: 10, g: 20, b: 30, a: 128 })
        );
    }

    #[test]
    fn unknown_keyword_stays_keyword() {
        let ss = parse("p { display: flex; }".to_string());
        assert_eq!(
            ss.rules[0].declarations[0].value,
            Value::Keyword("flex".to_string())
        );
    }

    #[test]
    fn parses_relative_units() {
        let ss = parse("p { font-size: 1.5em; width: 50%; margin-top: 2rem; }".to_string());
        let d = &ss.rules[0].declarations;
        assert_eq!(d[0].value, Value::Length(1.5, Unit::Em));
        assert_eq!(d[1].value, Value::Length(50.0, Unit::Percent));
        assert_eq!(d[2].value, Value::Length(2.0, Unit::Rem));
    }

    #[test]
    fn parses_url_value() {
        let ss = parse("div { background-image: url(https://a.com/B.jpg); }".to_string());
        assert_eq!(
            ss.rules[0].declarations[0].value,
            Value::Url("https://a.com/B.jpg".to_string())
        );
        let ss = parse("div { background-image: url(\"img/x.png\"); }".to_string());
        assert_eq!(ss.rules[0].declarations[0].value, Value::Url("img/x.png".to_string()));
    }

    // 캐스케이드: 같은 이름이 여러 번이면 마지막 선언이 이긴다
    fn decl<'a>(ss: &'a Stylesheet, name: &str) -> Option<&'a Value> {
        ss.rules[0].declarations.iter().rev().find(|d| d.name == name).map(|d| &d.value)
    }

    #[test]
    fn margin_shorthand_one_value_expands_to_four() {
        let ss = parse("div { margin: 10px; }".to_string());
        for side in ["margin-top", "margin-right", "margin-bottom", "margin-left"] {
            assert_eq!(decl(&ss, side), Some(&Value::Length(10.0, Unit::Px)), "{}", side);
        }
    }

    #[test]
    fn margin_shorthand_two_values() {
        let ss = parse("div { margin: 10px 20px; }".to_string());
        assert_eq!(decl(&ss, "margin-top"), Some(&Value::Length(10.0, Unit::Px)));
        assert_eq!(decl(&ss, "margin-bottom"), Some(&Value::Length(10.0, Unit::Px)));
        assert_eq!(decl(&ss, "margin-left"), Some(&Value::Length(20.0, Unit::Px)));
        assert_eq!(decl(&ss, "margin-right"), Some(&Value::Length(20.0, Unit::Px)));
    }

    #[test]
    fn margin_zero_auto_keeps_auto_sides() {
        let ss = parse("div { margin: 0 auto; }".to_string());
        assert_eq!(decl(&ss, "margin-top"), Some(&Value::Length(0.0, Unit::Px)));
        assert_eq!(decl(&ss, "margin-left"), Some(&Value::Keyword("auto".to_string())));
        assert_eq!(decl(&ss, "margin-right"), Some(&Value::Keyword("auto".to_string())));
    }

    #[test]
    fn padding_shorthand_four_values_clockwise() {
        let ss = parse("div { padding: 1px 2px 3px 4px; }".to_string());
        assert_eq!(decl(&ss, "padding-top"), Some(&Value::Length(1.0, Unit::Px)));
        assert_eq!(decl(&ss, "padding-right"), Some(&Value::Length(2.0, Unit::Px)));
        assert_eq!(decl(&ss, "padding-bottom"), Some(&Value::Length(3.0, Unit::Px)));
        assert_eq!(decl(&ss, "padding-left"), Some(&Value::Length(4.0, Unit::Px)));
    }

    #[test]
    fn border_shorthand_expands_to_width_style_color() {
        let ss = parse("div { border: 1px solid #cccccc; }".to_string());
        for side in ["top", "right", "bottom", "left"] {
            assert_eq!(decl(&ss, &format!("border-{}-width", side)), Some(&Value::Length(1.0, Unit::Px)));
            assert_eq!(
                decl(&ss, &format!("border-{}-style", side)),
                Some(&Value::Keyword("solid".to_string()))
            );
            assert_eq!(
                decl(&ss, &format!("border-{}-color", side)),
                Some(&Value::Color(Color { r: 204, g: 204, b: 204, a: 255 }))
            );
        }
    }

    #[test]
    fn box_shadow_expands_to_longhands() {
        let ss = parse("div { box-shadow: 0 2px 8px rgba(0,0,0,0.15); }".to_string());
        assert_eq!(decl(&ss, "box-shadow-x"), Some(&Value::Length(0.0, Unit::Px)));
        assert_eq!(decl(&ss, "box-shadow-y"), Some(&Value::Length(2.0, Unit::Px)));
        assert_eq!(decl(&ss, "box-shadow-blur"), Some(&Value::Length(8.0, Unit::Px)));
        assert_eq!(
            decl(&ss, "box-shadow-color"),
            Some(&Value::Color(Color { r: 0, g: 0, b: 0, a: 38 }))
        );
    }

    #[test]
    fn box_shadow_inset_dropped() {
        let ss = parse("div { box-shadow: inset 0 2px 4px #000000; }".to_string());
        assert_eq!(decl(&ss, "box-shadow-x"), None, "inset 는 미지원 → 드롭");
    }

    #[test]
    fn border_radius_single_value_kept() {
        let ss = parse("div { border-radius: 12px; }".to_string());
        assert_eq!(decl(&ss, "border-radius"), Some(&Value::Length(12.0, Unit::Px)));
        // 다중값은 첫 토큰만 (균일 근사)
        let ss2 = parse("div { border-radius: 8px 4px 8px 4px; }".to_string());
        assert_eq!(decl(&ss2, "border-radius"), Some(&Value::Length(8.0, Unit::Px)));
    }

    #[test]
    fn border_side_and_color_shorthands() {
        // 변별 단축값 + border-color 4값
        let ss = parse(
            "div { border-left: 4px solid #f4b400; border-color: #111111 #222222 #333333 #444444; }"
                .to_string(),
        );
        assert_eq!(decl(&ss, "border-left-width"), Some(&Value::Length(4.0, Unit::Px)));
        assert_eq!(decl(&ss, "border-left-style"), Some(&Value::Keyword("solid".to_string())));
        // border-color 4값이 border-left-color 를 덮어씀 (문서 순서상 뒤)
        assert_eq!(decl(&ss, "border-top-color"), Some(&Value::Color(Color { r: 17, g: 17, b: 17, a: 255 })));
        assert_eq!(decl(&ss, "border-left-color"), Some(&Value::Color(Color { r: 68, g: 68, b: 68, a: 255 })));
    }

    #[test]
    fn longhand_after_shorthand_overrides() {
        let ss = parse("div { margin: 10px; margin-left: 5px; }".to_string());
        assert_eq!(decl(&ss, "margin-left"), Some(&Value::Length(5.0, Unit::Px)));
        assert_eq!(decl(&ss, "margin-top"), Some(&Value::Length(10.0, Unit::Px)));
    }

    #[test]
    fn ua_stylesheet_hides_script_and_style() {
        let ss = user_agent_stylesheet();
        for tag in ["script", "style", "head"] {
            let hidden = ss.rules.iter().any(|r| {
                r.selectors.iter().any(|s| s.subject().tag_name.as_deref() == Some(tag)) && r
                    .declarations
                    .iter()
                    .any(|d| d.name == "display" && d.value == Value::Keyword("none".to_string()))
            });
            assert!(hidden, "{} should be display:none in UA", tag);
        }
    }

    #[test]
    fn ua_stylesheet_has_display_block_for_div() {
        let ss = user_agent_stylesheet();
        let matches_div = ss.rules.iter().any(|r| {
            r.selectors.iter().any(|s| s.subject().tag_name.as_deref() == Some("div")) && r
                .declarations
                .iter()
                .any(|d| d.name == "display" && d.value == Value::Keyword("block".to_string()))
        });
        assert!(matches_div);
    }
}
