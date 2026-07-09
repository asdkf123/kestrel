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
    // 공백 결합자 체인: [조상, ..., 대상] (예: ".a .b" → [.a, .b])
    Descendant(Vec<SimpleSelector>),
}

#[derive(Debug, PartialEq)]
pub struct SimpleSelector {
    pub tag_name: Option<String>,
    pub id: Option<String>,
    pub class: Vec<String>,
    // 속성 선택자: (이름, 값). 값 None = [attr] 존재만, Some = [attr=val] 정확 일치.
    pub attrs: Vec<(String, Option<String>)>,
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
    fn parts(&self) -> &[SimpleSelector] {
        match self {
            Selector::Simple(s) => std::slice::from_ref(s),
            Selector::Descendant(v) => v,
        }
    }

    // 대상(가장 오른쪽) 단순 선택자
    pub fn subject(&self) -> &SimpleSelector {
        self.parts().last().unwrap()
    }

    pub fn specificity(&self) -> Specificity {
        let parts = self.parts();
        let a = parts.iter().map(|s| s.id.iter().count()).sum();
        // 속성 선택자는 클래스와 동일 특이도 (CSS 표준)
        let b = parts.iter().map(|s| s.class.len() + s.attrs.len()).sum();
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
pub fn parse_inline_style(text: &str) -> Vec<Declaration> {
    let mut parser = Parser { pos: 0, input: text.to_string(), viewport_width: 0.0 };
    parser.parse_declarations()
}

// UA 기본 스타일시트. HTML 표준 §15 Rendering 을 근거로 함
// (https://html.spec.whatwg.org/multipage/rendering.html). 표준은 폼 컨트롤을
// appearance:auto(네이티브 위젯)로 두지만, 우리는 appearance 미구현이라 기본
// 테두리/배경을 여기 CSS 로 넣는다 — 캐스케이드상 저작자 CSS 가 덮을 수 있어
// 하드코딩(무조건 그림)과 달리 구글 등의 커스텀 스타일이 이긴다.
// 테이블 계열은 진짜 테이블 레이아웃 전까지 block 으로 근사(레이아웃은 tr 태그로 분기).
const UA_CSS: &str = "html, body, div, p, h1, h2, h3, h4, h5, h6, ul, ol, li, section, article, header, footer, nav, main, aside, blockquote, pre, table, thead, tbody, tfoot, tr, td, th, caption, center, form, fieldset, hr, figure, figcaption, address, dl, dt, dd, select, textarea { display: block; } head, script, style, title, meta, link, noscript, template { display: none; } img { display: block; } a { color: #0645ad; } ul, ol { padding-left: 24px; } li { padding-left: 18px; } td, th { padding: 4px 6px; } th { color: #202020; } center { text-align: center; } input, button { display: inline-block; } input, textarea, select { border: 1px solid #767676; background-color: #ffffff; padding: 2px 5px; } button, input[type=submit], input[type=reset], input[type=button] { border: 1px solid #767676; background-color: #e9e9ed; padding: 2px 8px; text-align: center; } b, strong, h1, h2, h3, h4, h5, h6, th { font-weight: bold; } i, em, cite, var, address { font-style: italic; }";

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
                // '>'/'+'/'~'/의사클래스/속성/eof 등은 미지원 → 규칙 스킵
                _ => return None,
            }
        }
        selectors.sort_by(|a, b| b.specificity().cmp(&a.specificity()));
        Some(selectors)
    }

    // 공백으로 이어진 단순 선택자 체인 (자손 결합자). 종료 후 peek 은 ','/'{'/미지원 문자.
    fn parse_complex_selector(&mut self) -> Option<Selector> {
        let mut parts = vec![self.parse_simple_selector()];
        loop {
            self.consume_whitespace();
            match self.peek() {
                Some(c) if c == '.' || c == '#' || c == '*' || c == '[' || valid_identifier_char(c) => {
                    parts.push(self.parse_simple_selector());
                }
                _ => break,
            }
        }
        if parts.len() == 1 {
            Some(Selector::Simple(parts.pop().unwrap()))
        } else {
            Some(Selector::Descendant(parts))
        }
    }

    fn parse_simple_selector(&mut self) -> SimpleSelector {
        let mut selector =
            SimpleSelector { tag_name: None, id: None, class: Vec::new(), attrs: Vec::new() };
        while !self.eof() {
            match self.input[self.pos..].chars().next().unwrap() {
                '#' => {
                    self.consume_char();
                    selector.id = Some(self.parse_identifier());
                }
                '.' => {
                    self.consume_char();
                    selector.class.push(self.parse_identifier());
                }
                '*' => {
                    self.consume_char();
                }
                '[' => {
                    if let Some(attr) = self.parse_attr_selector() {
                        selector.attrs.push(attr);
                    }
                }
                c if valid_identifier_char(c) => {
                    selector.tag_name = Some(self.parse_identifier());
                }
                _ => break,
            }
        }
        selector
    }

    // [name] 또는 [name=value] / [name="value"]. =/따옴표 파싱, 그 외 연산자(~= 등)는
    // value 없는 존재 검사로 관용 처리. name 은 소문자화.
    fn parse_attr_selector(&mut self) -> Option<(String, Option<String>)> {
        self.consume_char(); // '['
        self.consume_whitespace();
        let name = self.parse_identifier().to_ascii_lowercase();
        self.consume_whitespace();
        let value = if self.peek() == Some('=') {
            self.consume_char();
            self.consume_whitespace();
            Some(self.parse_attr_value())
        } else {
            None
        };
        // ']' 까지 소비 (미지원 연산자 잔여 포함)
        self.consume_while(|c| c != ']');
        if self.peek() == Some(']') {
            self.consume_char();
        }
        if name.is_empty() {
            None
        } else {
            Some((name, value))
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
    fn skips_unsupported_selectors() {
        // '>' (자식 결합자) 는 아직 미지원 → 규칙 스킵
        let ss = parse(".a > .b { color: #ff0000; } div { width: 5px; }".to_string());
        assert_eq!(ss.rules.len(), 1);
        match &ss.rules[0].selectors[0] {
            Selector::Simple(s) => assert_eq!(s.tag_name.as_deref(), Some("div")),
            other => panic!("unexpected selector: {:?}", other),
        }
    }

    #[test]
    fn parses_descendant_selector_chain() {
        let ss = parse("div .note p { width: 5px; }".to_string());
        match &ss.rules[0].selectors[0] {
            Selector::Descendant(parts) => {
                assert_eq!(parts.len(), 3);
                assert_eq!(parts[0].tag_name.as_deref(), Some("div"));
                assert_eq!(parts[1].class, vec!["note".to_string()]);
                assert_eq!(parts[2].tag_name.as_deref(), Some("p"));
            }
            other => panic!("expected Descendant, got {:?}", other),
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
