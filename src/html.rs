use std::collections::HashMap;

use crate::dom::{self, AttrMap, Node};

struct Parser {
    pos: usize,
    input: String,
}

impl Parser {
    fn next_char(&self) -> char {
        self.input[self.pos..].chars().next().unwrap()
    }

    fn starts_with(&self, s: &str) -> bool {
        self.input[self.pos..].starts_with(s)
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
        while !self.eof() && test(self.next_char()) {
            result.push(self.consume_char());
        }
        result
    }

    fn consume_whitespace(&mut self) {
        self.consume_while(char::is_whitespace);
    }

    fn parse_tag_name(&mut self) -> String {
        self.consume_while(|c| c.is_ascii_alphanumeric())
    }

    fn parse_node(&mut self) -> Node {
        if self.starts_with("<") {
            self.parse_element()
        } else {
            self.parse_text()
        }
    }

    fn parse_text(&mut self) -> Node {
        dom::text(self.consume_while(|c| c != '<'))
    }

    fn parse_element(&mut self) -> Node {
        assert!(self.consume_char() == '<');
        let tag_name = self.parse_tag_name();
        let attrs = self.parse_attributes();
        assert!(self.consume_char() == '>');

        let children = self.parse_nodes();

        assert!(self.consume_char() == '<');
        assert!(self.consume_char() == '/');
        assert!(self.parse_tag_name() == tag_name);
        assert!(self.consume_char() == '>');

        dom::elem(tag_name, attrs, children)
    }

    fn parse_attr(&mut self) -> (String, String) {
        let name = self.parse_tag_name();
        assert!(self.consume_char() == '=');
        let value = self.parse_attr_value();
        (name, value)
    }

    fn parse_attr_value(&mut self) -> String {
        let open_quote = self.consume_char();
        assert!(open_quote == '"' || open_quote == '\'');
        let value = self.consume_while(|c| c != open_quote);
        assert!(self.consume_char() == open_quote);
        value
    }

    fn parse_attributes(&mut self) -> AttrMap {
        let mut attributes = HashMap::new();
        loop {
            self.consume_whitespace();
            if self.next_char() == '>' {
                break;
            }
            let (name, value) = self.parse_attr();
            attributes.insert(name, value);
        }
        attributes
    }

    fn parse_nodes(&mut self) -> Vec<Node> {
        let mut nodes = Vec::new();
        loop {
            self.consume_whitespace();
            if self.eof() || self.starts_with("</") {
                break;
            }
            nodes.push(self.parse_node());
        }
        nodes
    }
}

pub fn parse(source: String) -> Node {
    let mut nodes = Parser { pos: 0, input: source }.parse_nodes();
    if nodes.len() == 1 {
        nodes.swap_remove(0)
    } else {
        dom::elem("html".to_string(), HashMap::new(), nodes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::NodeType;

    #[test]
    fn parses_single_element_with_text() {
        let node = parse("<p>hello</p>".to_string());
        match node.node_type {
            NodeType::Element(ref e) => assert_eq!(e.tag_name, "p"),
            _ => panic!("expected element"),
        }
        assert_eq!(node.children.len(), 1);
        match node.children[0].node_type {
            NodeType::Text(ref t) => assert_eq!(t, "hello"),
            _ => panic!("expected text child"),
        }
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
        assert_eq!(node.children.len(), 2);
    }
}
