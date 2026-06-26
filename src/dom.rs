use std::collections::{HashMap, HashSet};

pub type AttrMap = HashMap<String, String>;

#[derive(Debug, PartialEq)]
pub struct Node {
    pub children: Vec<Node>,
    pub node_type: NodeType,
}

#[derive(Debug, PartialEq)]
pub enum NodeType {
    Text(String),
    Element(ElementData),
}

#[derive(Debug, PartialEq)]
pub struct ElementData {
    pub tag_name: String,
    pub attributes: AttrMap,
}

pub fn text(data: String) -> Node {
    Node { children: Vec::new(), node_type: NodeType::Text(data) }
}

#[allow(dead_code)]
pub fn elem(name: String, attrs: AttrMap, children: Vec<Node>) -> Node {
    Node {
        children,
        node_type: NodeType::Element(ElementData { tag_name: name, attributes: attrs }),
    }
}

impl ElementData {
    pub fn id(&self) -> Option<&String> {
        self.attributes.get("id")
    }

    pub fn classes(&self) -> HashSet<&str> {
        match self.attributes.get("class") {
            Some(classlist) => classlist.split(' ').collect(),
            None => HashSet::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_node_has_no_children() {
        let n = text("hello".to_string());
        assert_eq!(n.children.len(), 0);
        assert_eq!(n.node_type, NodeType::Text("hello".to_string()));
    }

    #[test]
    fn element_exposes_id_and_classes() {
        let mut attrs = AttrMap::new();
        attrs.insert("id".to_string(), "main".to_string());
        attrs.insert("class".to_string(), "a b".to_string());
        let n = elem("div".to_string(), attrs, Vec::new());

        if let NodeType::Element(ref e) = n.node_type {
            assert_eq!(e.id(), Some(&"main".to_string()));
            let classes = e.classes();
            assert!(classes.contains("a"));
            assert!(classes.contains("b"));
            assert_eq!(classes.len(), 2);
        } else {
            panic!("expected element");
        }
    }
}
