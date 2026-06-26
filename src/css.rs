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
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Unit {
    Px,
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
    pub fn specificity(&self) -> Specificity {
        let Selector::Simple(ref simple) = *self;
        let a = simple.id.iter().count();
        let b = simple.class.len();
        let c = simple.tag_name.iter().count();
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

const UA_CSS: &str = "html, body, div, p, h1, h2, h3, h4, h5, h6, ul, ol, li, section, article, header, footer, nav, main, aside, blockquote, pre, table, tr, form, figure, figcaption, address, dl, dt, dd { display: block; }";

pub fn user_agent_stylesheet() -> Stylesheet {
    parse(UA_CSS.to_string())
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
            selectors.push(Selector::Simple(self.parse_simple_selector()));
            self.consume_whitespace();
            match self.peek() {
                Some(',') => {
                    self.consume_char();
                    self.consume_whitespace();
                }
                Some('{') => {
                    self.consume_char();
                    break;
                }
                // 자손 결합자/의사클래스/속성/eof 등은 미지원 → 규칙 스킵
                _ => return None,
            }
        }
        selectors.sort_by(|a, b| b.specificity().cmp(&a.specificity()));
        Some(selectors)
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
                    if let Some(decl) = self.parse_declaration() {
                        declarations.push(decl);
                    }
                }
            }
        }
        declarations
    }

    fn parse_declaration(&mut self) -> Option<Declaration> {
        let name = self.parse_identifier().trim().to_ascii_lowercase();
        self.consume_whitespace();
        if self.peek() != Some(':') {
            self.skip_to_decl_end();
            return None;
        }
        self.consume_char(); // ':'
        self.consume_whitespace();
        let value_text = self.consume_while(|c| c != ';' && c != '}');
        if self.peek() == Some(';') {
            self.consume_char();
        }
        if name.is_empty() {
            return None;
        }
        let value = interpret_value(value_text.trim())?;
        Some(Declaration { name, value })
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
        if text.len() == 7 {
            let r = u8::from_str_radix(&text[1..3], 16).ok()?;
            let g = u8::from_str_radix(&text[3..5], 16).ok()?;
            let b = u8::from_str_radix(&text[5..7], 16).ok()?;
            return Some(Value::Color(Color { r, g, b, a: 255 }));
        }
        return None;
    }
    let numeric_start = bytes[0].is_ascii_digit()
        || bytes[0] == b'.'
        || (bytes[0] == b'-' && bytes.len() > 1 && (bytes[1].is_ascii_digit() || bytes[1] == b'.'));
    if numeric_start {
        if let Some(num) = text.strip_suffix("px") {
            if let Ok(f) = num.trim().parse::<f32>() {
                return Some(Value::Length(f, Unit::Px));
            }
        }
        return None; // em/%/rem/단위없는수 등은 미지원
    }
    if text.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Some(Value::Keyword(text.to_string()));
    }
    None // rgb()/calc()/다중값 등
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
        let ss = parse(".a .b { color: #ff0000; } div { width: 5px; }".to_string());
        assert_eq!(ss.rules.len(), 1);
        match &ss.rules[0].selectors[0] {
            Selector::Simple(s) => assert_eq!(s.tag_name.as_deref(), Some("div")),
        }
    }

    #[test]
    fn skips_unsupported_values() {
        let ss = parse("p { color: rgb(1,2,3); width: 5px; }".to_string());
        assert_eq!(ss.rules[0].declarations.len(), 1);
        assert_eq!(ss.rules[0].declarations[0].name, "width");
    }

    #[test]
    fn ua_stylesheet_has_display_block_for_div() {
        let ss = user_agent_stylesheet();
        let matches_div = ss.rules.iter().any(|r| {
            r.selectors.iter().any(|s| match s {
                Selector::Simple(sel) => sel.tag_name.as_deref() == Some("div"),
            }) && r
                .declarations
                .iter()
                .any(|d| d.name == "display" && d.value == Value::Keyword("block".to_string()))
        });
        assert!(matches_div);
    }
}
