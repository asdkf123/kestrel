use std::collections::HashMap;

use crate::dom::{self, AttrMap, ElementData, Node, NodeType};

pub fn parse(source: String) -> Node {
    let mut roots = parse_roots(source);
    if roots.len() == 1 {
        roots.pop().unwrap()
    } else {
        elem_node("html".to_string(), HashMap::new(), roots)
    }
}

// 파스 후 바로 아레나로 (일반 경로)
pub fn parse_dom(source: String) -> crate::dom::Dom {
    crate::dom::Dom::from_tree(parse(source))
}

// innerHTML 용: 다중 루트를 html 로 감싸지 않고 그대로 반환
pub fn parse_fragment(source: String) -> Vec<Node> {
    parse_roots(source)
}

fn parse_roots(source: String) -> Vec<Node> {
    let mut t = Tokenizer { input: source.into_bytes(), pos: 0 };
    let mut b = Builder { stack: Vec::new(), roots: Vec::new() };

    while !t.eof() {
        if t.starts_with(b"<!--") {
            t.pos += 4;
            t.skip_past(b"-->");
        } else if t.starts_with(b"<!") {
            t.skip_past(b">");
        } else if t.starts_with(b"</") {
            let name = t.read_close_tag();
            b.close(&name);
        } else if t.peek() == b'<' && t.peek_at(1).map_or(false, |c| c.is_ascii_alphabetic()) {
            let (name, attrs, self_closing) = t.read_start_tag();
            if is_void(&name) || self_closing {
                b.add(elem_node(name, attrs, Vec::new()));
            } else if name == "script" || name == "style" {
                let raw = t.read_raw_until_close(&name);
                let children = if raw.is_empty() { Vec::new() } else { vec![dom::text(raw)] };
                b.add(elem_node(name, attrs, children));
            } else {
                b.open(ElementData { tag_name: name, attributes: attrs });
            }
        } else {
            let text = t.read_text();
            if !text.is_empty() {
                b.add(dom::text(text));
            }
        }
    }
    b.close_all();
    b.roots
}

fn elem_node(name: String, attrs: AttrMap, children: Vec<Node>) -> Node {
    Node { children, node_type: NodeType::Element(ElementData { tag_name: name, attributes: attrs }) }
}

fn is_void(name: &str) -> bool {
    matches!(
        name,
        "area" | "base" | "br" | "col" | "embed" | "hr" | "img" | "input" | "link" | "meta"
            | "param" | "source" | "track" | "wbr"
    )
}

struct Builder {
    stack: Vec<(ElementData, Vec<Node>)>,
    roots: Vec<Node>,
}

impl Builder {
    fn add(&mut self, node: Node) {
        if let Some((_, children)) = self.stack.last_mut() {
            children.push(node);
        } else {
            self.roots.push(node);
        }
    }
    fn open(&mut self, elem: ElementData) {
        self.stack.push((elem, Vec::new()));
    }
    fn close(&mut self, name: &str) {
        if let Some(pos) = self.stack.iter().rposition(|(e, _)| e.tag_name == name) {
            while self.stack.len() > pos {
                let (elem, children) = self.stack.pop().unwrap();
                let node = Node { children, node_type: NodeType::Element(elem) };
                self.add(node);
            }
        }
        // 매칭 없음: 스트레이 끝태그 무시
    }
    fn close_all(&mut self) {
        while let Some((elem, children)) = self.stack.pop() {
            let node = Node { children, node_type: NodeType::Element(elem) };
            self.add(node);
        }
    }
}

struct Tokenizer {
    input: Vec<u8>,
    pos: usize,
}

impl Tokenizer {
    fn eof(&self) -> bool {
        self.pos >= self.input.len()
    }
    fn peek(&self) -> u8 {
        self.input[self.pos]
    }
    fn peek_at(&self, o: usize) -> Option<u8> {
        self.input.get(self.pos + o).copied()
    }
    fn starts_with(&self, s: &[u8]) -> bool {
        self.input[self.pos..].starts_with(s)
    }
    fn skip_whitespace(&mut self) {
        while !self.eof() && self.peek().is_ascii_whitespace() {
            self.pos += 1;
        }
    }
    fn skip_past(&mut self, needle: &[u8]) {
        while !self.eof() && !self.input[self.pos..].starts_with(needle) {
            self.pos += 1;
        }
        if self.input[self.pos..].starts_with(needle) {
            self.pos += needle.len();
        }
    }

    fn read_text(&mut self) -> String {
        let start = self.pos;
        if !self.eof() {
            self.pos += 1;
        }
        while !self.eof() && self.peek() != b'<' {
            self.pos += 1;
        }
        decode_entities(&String::from_utf8_lossy(&self.input[start..self.pos]))
    }

    fn read_close_tag(&mut self) -> String {
        self.pos += 2; // '</'
        let start = self.pos;
        while !self.eof() && self.peek() != b'>' && !self.peek().is_ascii_whitespace() {
            self.pos += 1;
        }
        let name = String::from_utf8_lossy(&self.input[start..self.pos]).to_ascii_lowercase();
        while !self.eof() && self.peek() != b'>' {
            self.pos += 1;
        }
        if !self.eof() {
            self.pos += 1; // '>'
        }
        name
    }

    fn read_tag_name(&mut self) -> String {
        let start = self.pos;
        while !self.eof() {
            let c = self.peek();
            if c.is_ascii_alphanumeric() || c == b'-' || c == b':' {
                self.pos += 1;
            } else {
                break;
            }
        }
        String::from_utf8_lossy(&self.input[start..self.pos]).to_ascii_lowercase()
    }

    fn read_start_tag(&mut self) -> (String, AttrMap, bool) {
        self.pos += 1; // '<'
        let name = self.read_tag_name();
        let mut attrs = HashMap::new();
        let mut self_closing = false;
        loop {
            self.skip_whitespace();
            if self.eof() {
                break;
            }
            match self.peek() {
                b'>' => {
                    self.pos += 1;
                    break;
                }
                b'/' => {
                    self_closing = true;
                    self.pos += 1;
                    if !self.eof() && self.peek() == b'>' {
                        self.pos += 1;
                    }
                    break;
                }
                _ => {
                    let (k, v) = self.read_attr();
                    if !k.is_empty() {
                        attrs.insert(k, v);
                    }
                }
            }
        }
        (name, attrs, self_closing)
    }

    fn read_attr(&mut self) -> (String, String) {
        let start = self.pos;
        while !self.eof() {
            let c = self.peek();
            if c == b'=' || c == b'>' || c == b'/' || c.is_ascii_whitespace() {
                break;
            }
            self.pos += 1;
        }
        let name = String::from_utf8_lossy(&self.input[start..self.pos]).to_ascii_lowercase();
        self.skip_whitespace();
        let mut value = String::new();
        if !self.eof() && self.peek() == b'=' {
            self.pos += 1;
            self.skip_whitespace();
            if !self.eof() && (self.peek() == b'"' || self.peek() == b'\'') {
                let quote = self.peek();
                self.pos += 1;
                let vs = self.pos;
                while !self.eof() && self.peek() != quote {
                    self.pos += 1;
                }
                value = String::from_utf8_lossy(&self.input[vs..self.pos]).to_string();
                if !self.eof() {
                    self.pos += 1;
                }
            } else {
                let vs = self.pos;
                while !self.eof() {
                    let c = self.peek();
                    if c == b'>' || c.is_ascii_whitespace() {
                        break;
                    }
                    self.pos += 1;
                }
                value = String::from_utf8_lossy(&self.input[vs..self.pos]).to_string();
            }
        }
        (name, decode_entities(&value))
    }

    fn read_raw_until_close(&mut self, name: &str) -> String {
        let close = format!("</{}", name).into_bytes();
        let start = self.pos;
        while !self.eof() {
            let end = self.pos + close.len();
            if end <= self.input.len() && self.input[self.pos..end].eq_ignore_ascii_case(&close) {
                break;
            }
            self.pos += 1;
        }
        let raw = String::from_utf8_lossy(&self.input[start..self.pos]).to_string();
        while !self.eof() && self.peek() != b'>' {
            self.pos += 1;
        }
        if !self.eof() {
            self.pos += 1;
        }
        raw
    }
}

fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp..];
        if let Some(semi) = after.find(';') {
            if semi <= 12 {
                if let Some(ch) = decode_one(&after[1..semi]) {
                    out.push(ch);
                    rest = &after[semi + 1..];
                    continue;
                }
            }
        }
        out.push('&');
        rest = &after[1..];
    }
    out.push_str(rest);
    out
}

fn decode_one(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some('\u{00A0}'),
        _ => {
            let num = entity.strip_prefix('#')?;
            let (radix, digits) = match num.strip_prefix('x').or_else(|| num.strip_prefix('X')) {
                Some(hex) => (16, hex),
                None => (10, num),
            };
            char::from_u32(u32::from_str_radix(digits, radix).ok()?)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::NodeType;

    fn tag_names(node: &Node, out: &mut Vec<String>) {
        if let NodeType::Element(e) = &node.node_type {
            out.push(e.tag_name.clone());
        }
        for c in &node.children {
            tag_names(c, out);
        }
    }

    fn all_text(node: &Node, out: &mut String) {
        if let NodeType::Text(t) = &node.node_type {
            out.push_str(t);
        }
        for c in &node.children {
            all_text(c, out);
        }
    }

    #[test]
    fn parses_single_element_with_text() {
        let node = parse("<p>hello</p>".to_string());
        match node.node_type {
            NodeType::Element(ref e) => assert_eq!(e.tag_name, "p"),
            _ => panic!("expected element"),
        }
        let mut s = String::new();
        all_text(&node, &mut s);
        assert_eq!(s, "hello");
    }

    #[test]
    fn parses_attributes() {
        let node = parse("<div id=\"main\" class=\"a b\"></div>".to_string());
        if let NodeType::Element(ref e) = node.node_type {
            assert_eq!(e.attributes.get("id").map(|s| s.as_str()), Some("main"));
            assert_eq!(e.attributes.get("class").map(|s| s.as_str()), Some("a b"));
        } else {
            panic!("expected element");
        }
    }

    #[test]
    fn wraps_multiple_roots_in_html() {
        let node = parse("<p></p><p></p>".to_string());
        if let NodeType::Element(ref e) = node.node_type {
            assert_eq!(e.tag_name, "html");
        } else {
            panic!("expected synthetic html root");
        }
    }

    #[test]
    fn skips_doctype_and_void_meta() {
        let n = parse(
            "<!doctype html><html><head><meta charset=\"utf-8\"></head><body><p>hi</p></body></html>"
                .to_string(),
        );
        let mut names = vec![];
        tag_names(&n, &mut names);
        assert!(names.contains(&"html".to_string()));
        assert!(names.contains(&"meta".to_string()));
        assert!(names.contains(&"p".to_string()));
    }

    #[test]
    fn auto_closes_unclosed_tags() {
        let n = parse("<div><p>a<p>b</div>".to_string());
        let mut names = vec![];
        tag_names(&n, &mut names);
        assert!(names.iter().filter(|s| *s == "p").count() >= 1);
        assert!(names.contains(&"div".to_string()));
    }

    #[test]
    fn raw_text_style_not_parsed_as_html() {
        let n = parse("<style>.a > .b { color: red; }</style>".to_string());
        let mut css = String::new();
        all_text(&n, &mut css);
        assert!(css.contains(".a > .b"), "style content preserved: {:?}", css);
    }

    #[test]
    fn decodes_entities() {
        let n = parse("<p>a &amp; b &#65;</p>".to_string());
        let mut s = String::new();
        all_text(&n, &mut s);
        assert!(s.contains("a & b A"), "got {:?}", s);
    }

    #[test]
    fn lowercases_tags_no_panic_on_messy() {
        let n = parse("<DIV><BR><img src=x><span>ok".to_string());
        let mut names = vec![];
        tag_names(&n, &mut names);
        assert!(names.contains(&"div".to_string()));
        assert!(names.contains(&"br".to_string()));
        assert!(names.contains(&"img".to_string()));
    }
}
