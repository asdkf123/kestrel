use std::collections::HashMap;

use crate::css::{Rule, Selector, SimpleSelector, Specificity, Stylesheet, Value};
use crate::dom::{ElementData, Node, NodeType};

pub type PropertyMap = HashMap<String, Value>;

pub struct StyledNode<'a> {
    pub node: &'a Node,
    pub specified_values: PropertyMap,
    pub children: Vec<StyledNode<'a>>,
}

pub enum Display {
    Inline,
    Block,
    None,
}

impl<'a> StyledNode<'a> {
    pub fn value(&self, name: &str) -> Option<Value> {
        self.specified_values.get(name).cloned()
    }

    pub fn lookup(&self, name: &str, fallback_name: &str, default: &Value) -> Value {
        self.value(name)
            .unwrap_or_else(|| self.value(fallback_name).unwrap_or_else(|| default.clone()))
    }

    pub fn display(&self) -> Display {
        match self.value("display") {
            Some(Value::Keyword(s)) => match &*s {
                "block" => Display::Block,
                "none" => Display::None,
                _ => Display::Inline,
            },
            _ => Display::Inline,
        }
    }
}

fn matches(elem: &ElementData, selector: &Selector) -> bool {
    match *selector {
        Selector::Simple(ref simple) => matches_simple_selector(elem, simple),
    }
}

fn matches_simple_selector(elem: &ElementData, selector: &SimpleSelector) -> bool {
    if selector.tag_name.iter().any(|name| elem.tag_name != *name) {
        return false;
    }
    if selector.id.iter().any(|id| elem.id() != Some(id)) {
        return false;
    }
    let elem_classes = elem.classes();
    if selector.class.iter().any(|class| !elem_classes.contains(&**class)) {
        return false;
    }
    true
}

type MatchedRule<'a> = (Specificity, &'a Rule);

fn match_rule<'a>(elem: &ElementData, rule: &'a Rule) -> Option<MatchedRule<'a>> {
    rule.selectors
        .iter()
        .find(|selector| matches(elem, selector))
        .map(|selector| (selector.specificity(), rule))
}

// 선택자의 오른쪽 키(id > class > tag > universal)로 규칙을 버킷팅한다.
// 요소마다 전체 규칙을 훑는 대신 해당 요소의 id/class/tag 버킷 후보만 확인 →
// O(요소×규칙) 에서 O(요소×후보) 로. build 는 스타일당 1회.
struct RuleIndex<'a> {
    rules: &'a [Rule],
    by_id: HashMap<String, Vec<usize>>,
    by_class: HashMap<String, Vec<usize>>,
    by_tag: HashMap<String, Vec<usize>>,
    universal: Vec<usize>,
}

impl<'a> RuleIndex<'a> {
    fn build(sheet: &'a Stylesheet) -> RuleIndex<'a> {
        let mut idx = RuleIndex {
            rules: &sheet.rules,
            by_id: HashMap::new(),
            by_class: HashMap::new(),
            by_tag: HashMap::new(),
            universal: Vec::new(),
        };
        for (i, rule) in sheet.rules.iter().enumerate() {
            for selector in &rule.selectors {
                let Selector::Simple(s) = selector;
                if let Some(id) = &s.id {
                    idx.by_id.entry(id.clone()).or_default().push(i);
                } else if let Some(class) = s.class.first() {
                    idx.by_class.entry(class.clone()).or_default().push(i);
                } else if let Some(tag) = &s.tag_name {
                    idx.by_tag.entry(tag.clone()).or_default().push(i);
                } else {
                    idx.universal.push(i);
                }
            }
        }
        idx
    }

    // 이 요소에 대해 검사할 후보 규칙 인덱스 (문서 순서, 중복 제거).
    fn candidate_indices(&self, elem: &ElementData) -> Vec<usize> {
        let mut out = Vec::new();
        if let Some(id) = elem.id() {
            if let Some(v) = self.by_id.get(id.as_str()) {
                out.extend_from_slice(v);
            }
        }
        for class in elem.classes() {
            if let Some(v) = self.by_class.get(class) {
                out.extend_from_slice(v);
            }
        }
        if let Some(v) = self.by_tag.get(elem.tag_name.as_str()) {
            out.extend_from_slice(v);
        }
        out.extend_from_slice(&self.universal);
        out.sort_unstable();
        out.dedup();
        out
    }
}

fn specified_values(elem: &ElementData, index: &RuleIndex) -> PropertyMap {
    let mut values = HashMap::new();
    let mut rules: Vec<MatchedRule> = index
        .candidate_indices(elem)
        .into_iter()
        .filter_map(|i| match_rule(elem, &index.rules[i]))
        .collect();
    // 오름차순 특이도, 안정 정렬 → 동일 특이도는 문서 순서 유지 (뒤 규칙이 이김)
    rules.sort_by(|&(a, _), &(b, _)| a.cmp(&b));
    for (_, rule) in rules {
        for declaration in &rule.declarations {
            values.insert(declaration.name.clone(), declaration.value.clone());
        }
    }
    values
}

pub fn style_tree<'a>(root: &'a Node, stylesheet: &'a Stylesheet) -> StyledNode<'a> {
    let index = RuleIndex::build(stylesheet);
    style_node(root, &index)
}

fn style_node<'a>(node: &'a Node, index: &RuleIndex<'a>) -> StyledNode<'a> {
    StyledNode {
        node,
        specified_values: match node.node_type {
            NodeType::Element(ref elem) => specified_values(elem, index),
            NodeType::Text(_) => HashMap::new(),
        },
        children: node.children.iter().map(|child| style_node(child, index)).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::{Unit, Value};

    #[test]
    fn matching_class_rule_is_applied() {
        let root = crate::html::parse("<div class=\"box\"></div>".to_string());
        let ss = crate::css::parse(".box { width: 50px; }".to_string());
        let styled = style_tree(&root, &ss);
        assert_eq!(styled.value("width"), Some(Value::Length(50.0, Unit::Px)));
    }

    #[test]
    fn higher_specificity_wins() {
        let root = crate::html::parse("<div id=\"a\" class=\"b\"></div>".to_string());
        let ss = crate::css::parse(".b { width: 10px; } #a { width: 99px; }".to_string());
        let styled = style_tree(&root, &ss);
        assert_eq!(styled.value("width"), Some(Value::Length(99.0, Unit::Px)));
    }

    #[test]
    fn universal_selector_applies_to_all() {
        let root = crate::html::parse("<div></div>".to_string());
        let ss = crate::css::parse("* { width: 7px; }".to_string());
        let styled = style_tree(&root, &ss);
        assert_eq!(styled.value("width"), Some(Value::Length(7.0, Unit::Px)));
    }

    #[test]
    fn later_rule_wins_at_equal_specificity() {
        let root = crate::html::parse("<p class=\"x\"></p>".to_string());
        let ss = crate::css::parse(".x { width: 1px; } .x { width: 2px; }".to_string());
        let styled = style_tree(&root, &ss);
        assert_eq!(styled.value("width"), Some(Value::Length(2.0, Unit::Px)));
    }

    #[test]
    fn compound_selector_needs_both_parts() {
        let root = crate::html::parse("<div><p></p><p class=\"note\"></p></div>".to_string());
        let ss = crate::css::parse("p.note { width: 5px; }".to_string());
        let styled = style_tree(&root, &ss);
        assert_eq!(styled.children[0].value("width"), None, "plain <p> must not match p.note");
        assert_eq!(
            styled.children[1].value("width"),
            Some(Value::Length(5.0, Unit::Px)),
            "<p class=note> must match p.note"
        );
    }

    #[test]
    fn target_wins_among_many_irrelevant_rules() {
        let root = crate::html::parse("<div class=\"target\"></div>".to_string());
        let mut css = String::new();
        for i in 0..200 {
            css.push_str(&format!(".n{} {{ width: {}px; }}", i, i));
        }
        css.push_str(".target { width: 999px; }");
        let ss = crate::css::parse(css);
        let styled = style_tree(&root, &ss);
        assert_eq!(styled.value("width"), Some(Value::Length(999.0, Unit::Px)));
    }
}
