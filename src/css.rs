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
        let b = parts.iter().map(|s| s.class.len()).sum();
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

pub fn parse(source: String) -> Stylesheet {
    let mut parser = Parser { pos: 0, input: source };
    Stylesheet { rules: parser.parse_rules() }
}

// button: inline-block 미지원 대체로 block (클릭 히트 영역을 갖게 함)
// 테이블 계열(td 등)도 block — 진짜 테이블 레이아웃 전까지 세로로라도 렌더되게
const UA_CSS: &str = "html, body, div, p, h1, h2, h3, h4, h5, h6, ul, ol, li, section, article, header, footer, nav, main, aside, blockquote, pre, table, thead, tbody, tfoot, tr, td, th, caption, center, form, fieldset, hr, figure, figcaption, address, dl, dt, dd, button, select, textarea, input { display: block; } head, script, style, title, meta, link, noscript, template { display: none; } img { display: block; } a { color: #0645ad; } ul, ol { padding-left: 24px; } li { padding-left: 18px; } td, th { padding: 4px 6px; } th { color: #202020; }";

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
                self.skip_at_rule();
                continue;
            }
            if let Some(rule) = self.parse_rule() {
                rules.push(rule);
            }
        }
        rules
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
                Some(c) if c == '.' || c == '#' || c == '*' || valid_identifier_char(c) => {
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
        let mut selector = SimpleSelector { tag_name: None, id: None, class: Vec::new() };
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
                c if valid_identifier_char(c) => {
                    selector.tag_name = Some(self.parse_identifier());
                }
                _ => break,
            }
        }
        selector
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

fn interpret_value(text: &str) -> Option<Value> {
    if text.is_empty() {
        return None;
    }
    let bytes = text.as_bytes();
    if bytes[0] == b'#' {
        return parse_hex_color(text).map(Value::Color);
    }
    let lower = text.to_ascii_lowercase();
    if lower.starts_with("rgb(") || lower.starts_with("rgba(") {
        return parse_rgb_func(&lower).map(Value::Color);
    }
    // url(...) — 따옴표 유무 모두. URL 은 대소문자 보존을 위해 원본에서 추출.
    if lower.starts_with("url(") && text.ends_with(')') {
        let inner = text[4..text.len() - 1].trim().trim_matches(|c| c == '"' || c == '\'');
        if inner.is_empty() {
            return None;
        }
        return Some(Value::Url(inner.to_string()));
    }
    let numeric_start = bytes[0].is_ascii_digit()
        || bytes[0] == b'.'
        || (bytes[0] == b'-' && bytes.len() > 1 && (bytes[1].is_ascii_digit() || bytes[1] == b'.'));
    if numeric_start {
        // 주의: "rem" 을 "em" 보다 먼저 검사
        for (suffix, unit) in
            [("px", Unit::Px), ("rem", Unit::Rem), ("em", Unit::Em), ("%", Unit::Percent)]
        {
            if let Some(num) = text.strip_suffix(suffix) {
                if let Ok(f) = num.trim().parse::<f32>() {
                    return Some(Value::Length(f, unit));
                }
                return None;
            }
        }
        // 단위 없는 0 은 유효한 길이 (예: margin: 0 auto)
        if let Ok(f) = text.parse::<f32>() {
            if f == 0.0 {
                return Some(Value::Length(0.0, Unit::Px));
            }
        }
        return None; // pt/vh/단위없는 0 아닌 수 등은 미지원
    }
    if text.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        if let Some(c) = named_color(&lower) {
            return Some(Value::Color(c));
        }
        return Some(Value::Keyword(text.to_string()));
    }
    None // calc()/다중값 등
}

// 선언 하나를 (경우에 따라 여러) longhand 선언으로 확장한다.
fn expand_declaration(name: &str, value_text: &str) -> Vec<Declaration> {
    match name {
        "margin" | "padding" => box_shorthand(name, "", value_text),
        "border-width" => box_shorthand("border", "-width", value_text),
        _ => match interpret_value(value_text) {
            Some(value) => vec![Declaration { name: name.to_string(), value }],
            None => Vec::new(),
        },
    }
}

// CSS 박스 단축값(1~4개)을 top/right/bottom/left longhand 로 확장.
// prefix="margin", suffix=""  → margin-top ...
// prefix="border", suffix="-width" → border-top-width ...
fn box_shorthand(prefix: &str, suffix: &str, value_text: &str) -> Vec<Declaration> {
    let tokens: Vec<Value> = value_text.split_whitespace().filter_map(interpret_value).collect();
    let (top, right, bottom, left) = match tokens.len() {
        1 => (tokens[0].clone(), tokens[0].clone(), tokens[0].clone(), tokens[0].clone()),
        2 => (tokens[0].clone(), tokens[1].clone(), tokens[0].clone(), tokens[1].clone()),
        3 => (tokens[0].clone(), tokens[1].clone(), tokens[2].clone(), tokens[1].clone()),
        4 => (tokens[0].clone(), tokens[1].clone(), tokens[2].clone(), tokens[3].clone()),
        _ => return Vec::new(),
    };
    let mk = |side: &str, value: Value| Declaration {
        name: format!("{}-{}{}", prefix, side, suffix),
        value,
    };
    vec![mk("top", top), mk("right", right), mk("bottom", bottom), mk("left", left)]
}

fn parse_hex_color(text: &str) -> Option<Color> {
    let hex = &text[1..];
    match hex.len() {
        3 => {
            let r = u8::from_str_radix(&hex[0..1], 16).ok()?;
            let g = u8::from_str_radix(&hex[1..2], 16).ok()?;
            let b = u8::from_str_radix(&hex[2..3], 16).ok()?;
            // 0xN → 0xNN (N*17)
            Some(Color { r: r * 17, g: g * 17, b: b * 17, a: 255 })
        }
        6 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            Some(Color { r, g, b, a: 255 })
        }
        _ => None,
    }
}

fn parse_rgb_func(text: &str) -> Option<Color> {
    let open = text.find('(')?;
    let close = text.find(')')?;
    let inner = &text[open + 1..close];
    let parts: Vec<&str> = inner.split(',').map(|s| s.trim()).collect();
    if parts.len() != 3 && parts.len() != 4 {
        return None;
    }
    let chan = |s: &str| -> Option<u8> { Some(s.parse::<f32>().ok()?.clamp(0.0, 255.0) as u8) };
    let r = chan(parts[0])?;
    let g = chan(parts[1])?;
    let b = chan(parts[2])?;
    let a = if parts.len() == 4 {
        (parts[3].parse::<f32>().ok()?.clamp(0.0, 1.0) * 255.0).round() as u8
    } else {
        255
    };
    Some(Color { r, g, b, a })
}

fn named_color(name: &str) -> Option<Color> {
    let rgb = match name {
        "black" => (0, 0, 0),
        "silver" => (192, 192, 192),
        "gray" | "grey" => (128, 128, 128),
        "white" => (255, 255, 255),
        "maroon" => (128, 0, 0),
        "red" => (255, 0, 0),
        "purple" => (128, 0, 128),
        "fuchsia" | "magenta" => (255, 0, 255),
        "green" => (0, 128, 0),
        "lime" => (0, 255, 0),
        "olive" => (128, 128, 0),
        "yellow" => (255, 255, 0),
        "navy" => (0, 0, 128),
        "blue" => (0, 0, 255),
        "teal" => (0, 128, 128),
        "aqua" | "cyan" => (0, 255, 255),
        "orange" => (255, 165, 0),
        "pink" => (255, 192, 203),
        "gold" => (255, 215, 0),
        "brown" => (165, 42, 42),
        "darkgray" | "darkgrey" => (169, 169, 169),
        "lightgray" | "lightgrey" => (211, 211, 211),
        "dimgray" | "dimgrey" => (105, 105, 105),
        "whitesmoke" => (245, 245, 245),
        "transparent" => return Some(Color { r: 0, g: 0, b: 0, a: 0 }),
        _ => return None,
    };
    Some(Color { r: rgb.0, g: rgb.1, b: rgb.2, a: 255 })
}

fn valid_identifier_char(c: char) -> bool {
    matches!(c, 'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_')
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
    fn skips_at_rules() {
        let ss = parse("@media screen { p { color: #ff0000; } } div { width: 5px; }".to_string());
        assert_eq!(ss.rules.len(), 1);
        assert_eq!(ss.rules[0].declarations[0].name, "width");
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
