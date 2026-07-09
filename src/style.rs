use std::collections::HashMap;

use crate::css::{Rule, Selector, SimpleSelector, Specificity, Stylesheet, Unit, Value};
use crate::dom::{Dom, ElementData, NodeData, NodeId, NodeType};

pub const DEFAULT_FONT_SIZE: f32 = 16.0;

pub type PropertyMap = HashMap<String, Value>;

pub struct StyledNode<'a> {
    pub node: &'a NodeData,
    // 아레나 NodeId — JS DOM 핸들과 같은 좌표계 (구조 변형에도 안정)
    pub id: NodeId,
    pub specified_values: PropertyMap,
    pub children: Vec<StyledNode<'a>>,
}

pub enum Display {
    Inline,
    Block,
    Flex,
    Grid,
    InlineBlock,
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
                "flex" => Display::Flex,
                "inline-flex" => Display::Flex,
                "grid" => Display::Grid,
                "inline-grid" => Display::Grid,
                "none" => Display::None,
                "inline" => Display::Inline,
                // inline-block: 자체 블록 박스를 갖되(내부 블록 자식 보존) 형제와
                // 가로로 흐른다. layout_children 이 shrink-to-fit 폭으로 좌→우 패킹 + 줄바꿈.
                "inline-block" => Display::InlineBlock,
                // grid/table/flow-root/list-item 등 미지원 값도 블록으로
                _ => Display::Block,
            },
            _ => Display::Inline,
        }
    }
}

// ancestors: 루트→부모 순. 자손 체인은 오른쪽(대상)부터 왼쪽으로,
// 조상은 가까운 쪽부터 위로 탐욕 매칭한다 (자손 결합자 표준 의미).
fn matches(elem: &ElementData, ancestors: &[&ElementData], selector: &Selector) -> bool {
    match selector {
        Selector::Simple(simple) => matches_simple_selector(elem, simple),
        Selector::Descendant(parts) => {
            let (subject, rest) = parts.split_last().unwrap();
            if !matches_simple_selector(elem, subject) {
                return false;
            }
            let mut i = rest.len();
            for anc in ancestors.iter().rev() {
                if i == 0 {
                    break;
                }
                if matches_simple_selector(anc, &rest[i - 1]) {
                    i -= 1;
                }
            }
            i == 0
        }
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
    // 속성 선택자: [name] 은 존재, [name=val] 은 값 일치
    for (name, val) in &selector.attrs {
        match elem.attributes.get(name) {
            Some(av) => {
                if let Some(v) = val {
                    if av != v {
                        return false;
                    }
                }
            }
            None => return false,
        }
    }
    true
}

type MatchedRule<'a> = (Specificity, &'a Rule);

fn match_rule<'a>(
    elem: &ElementData,
    ancestors: &[&ElementData],
    rule: &'a Rule,
) -> Option<MatchedRule<'a>> {
    rule.selectors
        .iter()
        .find(|selector| matches(elem, ancestors, selector))
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
                // 자손 체인은 대상(가장 오른쪽) 선택자 키로 버킷팅
                let s = selector.subject();
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

fn specified_values(elem: &ElementData, ancestors: &[&ElementData], index: &RuleIndex) -> PropertyMap {
    let mut values = HashMap::new();
    let mut rules: Vec<MatchedRule> = index
        .candidate_indices(elem)
        .into_iter()
        .filter_map(|i| match_rule(elem, ancestors, &index.rules[i]))
        .collect();
    // 오름차순 특이도, 안정 정렬 → 동일 특이도는 문서 순서 유지 (뒤 규칙이 이김)
    rules.sort_by(|&(a, _), &(b, _)| a.cmp(&b));
    for (_, rule) in rules {
        for declaration in &rule.declarations {
            values.insert(declaration.name.clone(), declaration.value.clone());
        }
    }
    // 인라인 style="..." 속성: 어떤 선택자보다 우선 (마지막에 얹어 이김)
    if let Some(style) = elem.attributes.get("style") {
        for declaration in crate::css::parse_inline_style(style) {
            values.insert(declaration.name, declaration.value);
        }
    }
    values
}

// querySelector 용: 아레나 요소가 선택자 목록 중 하나와 매칭되는가.
// 조상 체인은 아레나 parent 링크에서 구성 (루트→부모 순).
pub fn element_matches(dom: &Dom, id: NodeId, selectors: &[Selector]) -> bool {
    let NodeType::Element(elem) = &dom.get(id).node_type else {
        return false;
    };
    let ancestors: Vec<&ElementData> = dom
        .ancestors(id)
        .into_iter()
        .rev()
        .filter_map(|a| match &dom.get(a).node_type {
            NodeType::Element(e) => Some(e),
            _ => None,
        })
        .collect();
    selectors.iter().any(|s| matches(elem, &ancestors, s))
}

pub fn style_tree<'a>(dom: &'a Dom, stylesheet: &'a Stylesheet) -> StyledNode<'a> {
    let index = RuleIndex::build(stylesheet);
    let mut ancestors: Vec<&ElementData> = Vec::new();
    style_node(dom, dom.root, &index, &mut ancestors, None, DEFAULT_FONT_SIZE, None)
}

fn style_node<'a>(
    dom: &'a Dom,
    id: NodeId,
    index: &RuleIndex<'a>,
    ancestors: &mut Vec<&'a ElementData>,
    parent_color: Option<&Value>,
    parent_fs: f32,
    parent_align: Option<&Value>,
) -> StyledNode<'a> {
    let node = dom.get(id);
    match node.node_type {
        NodeType::Element(ref elem) => {
            let mut values = specified_values(elem, ancestors, index);
            // font-size: 상대 단위를 부모 기준으로 해석해 px 로 확정 (computed value)
            let fs = match values.get("font-size") {
                Some(Value::Length(n, Unit::Px)) => *n,
                Some(Value::Length(n, Unit::Em)) => n * parent_fs,
                Some(Value::Length(n, Unit::Rem)) => n * DEFAULT_FONT_SIZE,
                Some(Value::Length(n, Unit::Percent)) => n / 100.0 * parent_fs,
                _ => parent_fs, // 미지정/키워드 → 상속
            };
            values.insert("font-size".to_string(), Value::Length(fs, Unit::Px));
            // color: 명시 없으면 상속
            if !values.contains_key("color") {
                if let Some(c) = parent_color {
                    values.insert("color".to_string(), c.clone());
                }
            }
            // text-align: CSS > align 속성 > <center> 요소 > 상속 (text-align 은 상속 속성)
            if !values.contains_key("text-align") {
                let from_attr = elem.attributes.get("align").and_then(|a| match a.as_str() {
                    "center" | "right" | "left" => Some(a.clone()),
                    _ => None,
                });
                let resolved = if elem.tag_name == "center" {
                    Some("center".to_string())
                } else {
                    from_attr
                };
                if let Some(a) = resolved {
                    values.insert("text-align".to_string(), Value::Keyword(a));
                } else if let Some(a) = parent_align {
                    values.insert("text-align".to_string(), a.clone());
                }
            }
            // font-size 외 속성의 em/rem 은 아직 미해석 → 드롭 (미지원과 동일: auto 취급).
            // 퍼센트는 레이아웃(calculate_width)이 컨테이닝 블록 폭 기준으로 해석하므로 보존.
            values.retain(|k, v| {
                k == "font-size" || !matches!(v, Value::Length(_, Unit::Em | Unit::Rem))
            });
            let my_color = values.get("color").cloned();
            let my_align = values.get("text-align").cloned();
            ancestors.push(elem);
            let children = node
                .children
                .iter()
                .map(|&child| {
                    style_node(dom, child, index, ancestors, my_color.as_ref(), fs, my_align.as_ref())
                })
                .collect();
            ancestors.pop();
            StyledNode { node, id, specified_values: values, children }
        }
        NodeType::Text(_) => StyledNode {
            node,
            id,
            specified_values: HashMap::new(),
            children: node
                .children
                .iter()
                .map(|&child| {
                    style_node(dom, child, index, ancestors, parent_color, parent_fs, parent_align)
                })
                .collect(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::{Unit, Value};

    #[test]
    fn matching_class_rule_is_applied() {
        let root = crate::html::parse_dom("<div class=\"box\"></div>".to_string());
        let ss = crate::css::parse(".box { width: 50px; }".to_string());
        let styled = style_tree(&root, &ss);
        assert_eq!(styled.value("width"), Some(Value::Length(50.0, Unit::Px)));
    }

    #[test]
    fn higher_specificity_wins() {
        let root = crate::html::parse_dom("<div id=\"a\" class=\"b\"></div>".to_string());
        let ss = crate::css::parse(".b { width: 10px; } #a { width: 99px; }".to_string());
        let styled = style_tree(&root, &ss);
        assert_eq!(styled.value("width"), Some(Value::Length(99.0, Unit::Px)));
    }

    #[test]
    fn universal_selector_applies_to_all() {
        let root = crate::html::parse_dom("<div></div>".to_string());
        let ss = crate::css::parse("* { width: 7px; }".to_string());
        let styled = style_tree(&root, &ss);
        assert_eq!(styled.value("width"), Some(Value::Length(7.0, Unit::Px)));
    }

    #[test]
    fn later_rule_wins_at_equal_specificity() {
        let root = crate::html::parse_dom("<p class=\"x\"></p>".to_string());
        let ss = crate::css::parse(".x { width: 1px; } .x { width: 2px; }".to_string());
        let styled = style_tree(&root, &ss);
        assert_eq!(styled.value("width"), Some(Value::Length(2.0, Unit::Px)));
    }

    #[test]
    fn compound_selector_needs_both_parts() {
        let root = crate::html::parse_dom("<div><p></p><p class=\"note\"></p></div>".to_string());
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
    fn descendant_selector_matches_only_nested() {
        let root = crate::html::parse_dom(
            "<div><section class=\"a\"><p>in</p></section><p>out</p></div>".to_string(),
        );
        let ss = crate::css::parse(".a p { width: 9px; }".to_string());
        let styled = style_tree(&root, &ss);
        let section = &styled.children[0];
        let p_in = &section.children[0];
        let p_out = &styled.children[1];
        assert_eq!(p_in.value("width"), Some(Value::Length(9.0, Unit::Px)), ".a 안의 p 는 매칭");
        assert_eq!(p_out.value("width"), None, ".a 밖의 p 는 비매칭");
    }

    #[test]
    fn descendant_selector_skips_levels() {
        // 자손 결합자는 중간 단계를 건너뛴다 (자식 한정이 아님)
        let root = crate::html::parse_dom(
            "<div class=\"wrap\"><section><p>deep</p></section></div>".to_string(),
        );
        let ss = crate::css::parse(".wrap p { width: 7px; }".to_string());
        let styled = style_tree(&root, &ss);
        let p = &styled.children[0].children[0];
        assert_eq!(p.value("width"), Some(Value::Length(7.0, Unit::Px)));
    }

    #[test]
    fn descendant_out_of_order_does_not_match() {
        // "div .x" 인데 .x 가 div 의 조상인 경우 → 비매칭
        let root = crate::html::parse_dom("<span class=\"x\"><div><p>t</p></div></span>".to_string());
        let ss = crate::css::parse("div .x { width: 3px; }".to_string());
        let styled = style_tree(&root, &ss);
        assert_eq!(styled.value("width"), None);
    }

    #[test]
    fn descendant_specificity_beats_single_class() {
        let root = crate::html::parse_dom(
            "<div class=\"a\"><p class=\"b\">t</p></div>".to_string(),
        );
        let ss = crate::css::parse(".b { width: 1px; } .a .b { width: 2px; }".to_string());
        let styled = style_tree(&root, &ss);
        let p = &styled.children[0];
        assert_eq!(p.value("width"), Some(Value::Length(2.0, Unit::Px)), "(0,2,0) 이 (0,1,0) 을 이김");
    }

    #[test]
    fn color_inherits_to_descendants() {
        let root = crate::html::parse_dom("<div><p>t</p></div>".to_string());
        let ss = crate::css::parse("div { color: #ff0000; }".to_string());
        let styled = style_tree(&root, &ss);
        let p = &styled.children[0];
        assert_eq!(
            p.value("color"),
            Some(Value::Color(crate::css::Color { r: 255, g: 0, b: 0, a: 255 })),
            "color 는 부모에서 상속"
        );
    }

    #[test]
    fn own_color_overrides_inherited() {
        let root = crate::html::parse_dom("<div><p>t</p></div>".to_string());
        let ss = crate::css::parse("div { color: #ff0000; } p { color: #0000ff; }".to_string());
        let styled = style_tree(&root, &ss);
        let p = &styled.children[0];
        assert_eq!(
            p.value("color"),
            Some(Value::Color(crate::css::Color { r: 0, g: 0, b: 255, a: 255 }))
        );
    }

    #[test]
    fn font_size_relative_units_resolve() {
        let root = crate::html::parse_dom(
            "<div><p class=\"em\">a</p><p class=\"pc\">b</p><p class=\"rem\">c</p><p>d</p></div>"
                .to_string(),
        );
        let ss = crate::css::parse(
            "div { font-size: 20px; } .em { font-size: 1.5em; } .pc { font-size: 50%; } \
             .rem { font-size: 2rem; }"
                .to_string(),
        );
        let styled = style_tree(&root, &ss);
        // root = div, children = p 4개
        let fs = |i: usize| styled.children[i].value("font-size");
        assert_eq!(fs(0), Some(Value::Length(30.0, Unit::Px)), "1.5em × 20px");
        assert_eq!(fs(1), Some(Value::Length(10.0, Unit::Px)), "50% × 20px");
        assert_eq!(fs(2), Some(Value::Length(32.0, Unit::Px)), "2rem × 16px 루트");
        assert_eq!(fs(3), Some(Value::Length(20.0, Unit::Px)), "미지정 → 상속");
    }

    #[test]
    fn unknown_display_falls_back_to_block() {
        // 미지원 display 값(table-cell 등)은 블록으로 폴백 — 자식 보존
        let root = crate::html::parse_dom("<body><div>content</div></body>".to_string());
        let ss = crate::css::parse("body { display: table-cell; }".to_string());
        let styled = style_tree(&root, &ss);
        assert!(matches!(styled.display(), Display::Block));
        // inline-block 은 전용 Display (자체 박스 + 가로 흐름)
        let root2 = crate::html::parse_dom("<span></span>".to_string());
        let ss2 = crate::css::parse("span { display: inline-block; }".to_string());
        assert!(matches!(style_tree(&root2, &ss2).display(), Display::InlineBlock));
        // 순수 inline 은 그대로
        let root3 = crate::html::parse_dom("<span></span>".to_string());
        let ss3 = crate::css::parse("span { display: inline; }".to_string());
        assert!(matches!(style_tree(&root3, &ss3).display(), Display::Inline));
    }

    #[test]
    fn em_rem_dropped_but_percent_kept_for_layout() {
        let root = crate::html::parse_dom("<div></div>".to_string());
        let ss = crate::css::parse("div { width: 50%; margin-top: 2em; }".to_string());
        let styled = style_tree(&root, &ss);
        // 퍼센트 width 는 보존 → 레이아웃이 컨테이닝 블록 폭 기준으로 해석
        assert_eq!(styled.value("width"), Some(Value::Length(50.0, Unit::Percent)));
        // em/rem 은 여전히 미해석 → 드롭
        assert_eq!(styled.value("margin-top"), None);
    }

    #[test]
    fn inline_style_attribute_beats_selectors() {
        let root = crate::html::parse_dom(
            "<div id=\"a\" style=\"color: #00ff00; width: 5px\"></div>".to_string(),
        );
        // #id 선택자가 있어도 인라인 style 이 이겨야 함 (최고 우선순위)
        let ss = crate::css::parse("#a { color: #ff0000; width: 99px; }".to_string());
        let styled = style_tree(&root, &ss);
        assert_eq!(
            styled.value("color"),
            Some(Value::Color(crate::css::Color { r: 0, g: 255, b: 0, a: 255 }))
        );
        assert_eq!(styled.value("width"), Some(Value::Length(5.0, Unit::Px)));
    }

    #[test]
    fn attribute_selector_matches_by_value() {
        let root = crate::html::parse_dom(
            "<div><input type=\"submit\"><input type=\"text\"></div>".to_string(),
        );
        let ss = crate::css::parse("input[type=submit] { width: 5px; }".to_string());
        let styled = style_tree(&root, &ss);
        assert_eq!(
            styled.children[0].value("width"),
            Some(Value::Length(5.0, Unit::Px)),
            "input[type=submit] 매칭"
        );
        assert_eq!(styled.children[1].value("width"), None, "input[type=text] 비매칭");
    }

    #[test]
    fn attribute_presence_selector() {
        let root = crate::html::parse_dom(
            "<div><input disabled=\"\"><input></div>".to_string(),
        );
        let ss = crate::css::parse("input[disabled] { width: 3px; }".to_string());
        let styled = style_tree(&root, &ss);
        assert_eq!(styled.children[0].value("width"), Some(Value::Length(3.0, Unit::Px)));
        assert_eq!(styled.children[1].value("width"), None);
    }

    #[test]
    fn target_wins_among_many_irrelevant_rules() {
        let root = crate::html::parse_dom("<div class=\"target\"></div>".to_string());
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
