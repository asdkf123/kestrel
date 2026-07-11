use std::collections::HashMap;

use crate::css::{
    Combinator, Rule, Selector, SimpleSelector, Specificity, Stylesheet, Unit, Value,
};
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

    pub fn is_bold(&self) -> bool {
        matches!(self.value("font-weight"), Some(Value::Keyword(k)) if k == "bold")
    }

    pub fn is_italic(&self) -> bool {
        matches!(self.value("font-style"), Some(Value::Keyword(k)) if k == "italic" || k == "oblique")
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

// 대상 요소의 형제 문맥: 구조적 의사 클래스(:nth-child 등)와 형제 결합자(+/~)용.
#[derive(Clone, Copy)]
pub struct SiblingCtx<'a> {
    pub index: usize, // 요소 형제 중 1-기반 위치
    pub total: usize, // 요소 형제 수
    pub prev: &'a [&'a ElementData], // 선행 요소 형제 (문서 순서)
    pub has_children: bool,          // :empty 판별용
}

impl Default for SiblingCtx<'_> {
    fn default() -> Self {
        SiblingCtx { index: 1, total: 1, prev: &[], has_children: false }
    }
}

// ancestors: 루트→부모 순. 결합자 체인을 대상부터 왼쪽으로 걸어 매칭.
fn matches(
    elem: &ElementData,
    ancestors: &[&ElementData],
    sib: &SiblingCtx,
    selector: &Selector,
) -> bool {
    match selector {
        Selector::Simple(simple) => matches_compound(elem, simple, Some(sib)),
        Selector::Complex(parts) => {
            let (_, subject) = parts.last().unwrap();
            if !matches_compound(elem, subject, Some(sib)) {
                return false;
            }
            // 현재 요소 기준 조상/선행형제를 유지하며 왼쪽으로 이동
            let mut cur_anc: &[&ElementData] = ancestors;
            let mut cur_prev: &[&ElementData] = sib.prev;
            for i in (1..parts.len()).rev() {
                let combinator = parts[i].0;
                let part = &parts[i - 1].1;
                match combinator {
                    Combinator::Child => {
                        let Some(parent) = cur_anc.last() else { return false };
                        if !matches_compound(parent, part, None) {
                            return false;
                        }
                        cur_anc = &cur_anc[..cur_anc.len() - 1];
                        cur_prev = &[]; // 부모의 선행형제는 미보유
                    }
                    Combinator::Descendant => {
                        let mut found = None;
                        for k in (0..cur_anc.len()).rev() {
                            if matches_compound(cur_anc[k], part, None) {
                                found = Some(k);
                                break;
                            }
                        }
                        let Some(k) = found else { return false };
                        cur_anc = &cur_anc[..k];
                        cur_prev = &[];
                    }
                    Combinator::NextSibling => {
                        let Some(prev) = cur_prev.last() else { return false };
                        if !matches_compound(prev, part, None) {
                            return false;
                        }
                        cur_prev = &cur_prev[..cur_prev.len() - 1];
                    }
                    Combinator::LaterSibling => {
                        let mut found = None;
                        for k in (0..cur_prev.len()).rev() {
                            if matches_compound(cur_prev[k], part, None) {
                                found = Some(k);
                                break;
                            }
                        }
                        let Some(k) = found else { return false };
                        cur_prev = &cur_prev[..k];
                    }
                }
            }
            true
        }
    }
}

// compound(단순) 선택자 매칭 + 의사 클래스. sib=Some 이면 구조적 의사 클래스를
// 정확히 평가, None(비대상 부분)이면 근사(구조적→통과, 동적→비매칭).
fn matches_compound(
    elem: &ElementData,
    selector: &SimpleSelector,
    sib: Option<&SiblingCtx>,
) -> bool {
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
    for (name, op) in &selector.attrs {
        use crate::css::AttrOp;
        let Some(av) = elem.attributes.get(name) else { return false };
        let ok = match op {
            AttrOp::Exists => true,
            AttrOp::Equals(v) => av == v,
            AttrOp::Prefix(v) => !v.is_empty() && av.starts_with(v.as_str()),
            AttrOp::Suffix(v) => !v.is_empty() && av.ends_with(v.as_str()),
            AttrOp::Contains(v) => !v.is_empty() && av.contains(v.as_str()),
            AttrOp::Word(v) => !v.is_empty() && av.split_whitespace().any(|w| w == v),
            AttrOp::Dash(v) => av == v || av.starts_with(&format!("{}-", v)),
        };
        if !ok {
            return false;
        }
    }
    for p in &selector.pseudos {
        if !matches_pseudo(elem, p, sib) {
            return false;
        }
    }
    true
}

fn matches_pseudo(elem: &ElementData, p: &crate::css::Pseudo, sib: Option<&SiblingCtx>) -> bool {
    use crate::css::Pseudo;
    match p {
        Pseudo::Dynamic => false, // hover/focus/active/visited 등 정적 렌더에선 비매칭
        Pseudo::Not(inner) => !inner.iter().any(|s| matches_compound(elem, s, sib)),
        // 구조적: 대상(sib=Some)만 정확 평가, 비대상은 통과(근사)
        Pseudo::FirstChild => sib.map(|s| s.index == 1).unwrap_or(true),
        Pseudo::LastChild => sib.map(|s| s.index == s.total).unwrap_or(true),
        Pseudo::OnlyChild => sib.map(|s| s.total == 1).unwrap_or(true),
        Pseudo::Root => sib.map(|s| s.prev.is_empty() && s.total >= 1).unwrap_or(true) && elem.tag_name == "html",
        Pseudo::Empty => sib.map(|s| !s.has_children).unwrap_or(true),
        Pseudo::NthChild(a, b) => {
            let Some(s) = sib else { return true };
            let n = s.index as i32;
            // n = a*k + b, k>=0 인 정수 해 존재?
            if *a == 0 {
                n == *b
            } else {
                let diff = n - *b;
                diff % a == 0 && diff / a >= 0
            }
        }
    }
}

type MatchedRule<'a> = (Specificity, &'a Rule);

fn match_rule<'a>(
    elem: &ElementData,
    ancestors: &[&ElementData],
    sib: &SiblingCtx,
    rule: &'a Rule,
) -> Option<MatchedRule<'a>> {
    rule.selectors
        .iter()
        .find(|selector| matches(elem, ancestors, sib, selector))
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

fn specified_values(
    elem: &ElementData,
    ancestors: &[&ElementData],
    sib: &SiblingCtx,
    index: &RuleIndex,
) -> PropertyMap {
    let mut values = HashMap::new();
    let mut rules: Vec<MatchedRule> = index
        .candidate_indices(elem)
        .into_iter()
        .filter_map(|i| match_rule(elem, ancestors, sib, &index.rules[i]))
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
    // 형제 문맥 계산 (querySelector 는 :nth-child 등도 지원)
    let sib = sibling_ctx_for(dom, id);
    selectors.iter().any(|s| matches(elem, &ancestors, &sib, s))
}

// 아레나 요소의 형제 문맥(인덱스/총수/선행형제). element_matches 용.
fn sibling_ctx_for<'a>(dom: &'a Dom, id: NodeId) -> SiblingCtx<'a> {
    let has_children = !dom.get(id).children.is_empty();
    let Some(parent) = dom.get(id).parent else {
        return SiblingCtx { index: 1, total: 1, prev: &[], has_children };
    };
    let elem_sibs: Vec<NodeId> = dom
        .get(parent)
        .children
        .iter()
        .copied()
        .filter(|&c| matches!(dom.get(c).node_type, NodeType::Element(_)))
        .collect();
    let index = elem_sibs.iter().position(|&c| c == id).map(|i| i + 1).unwrap_or(1);
    // prev 는 문서 순서 선행 형제. 라이프타임상 leak 대신 빈 슬라이스(근사) — 대부분
    // querySelector 형제 결합자는 드묾. 인덱스/총수만 정확.
    SiblingCtx { index, total: elem_sibs.len().max(1), prev: &[], has_children }
}

// CSS 상속 속성 (자식이 명시 안 하면 부모 계산값을 물려받음). font-size 는
// 상대 단위 해석 때문에 아래에서 별도 처리하므로 여기 목록엔 없음.
const INHERITED: &[&str] = &[
    "color",
    "text-align",
    "font-family",
    "font-weight",
    "font-style",
    "font-variant",
    "line-height",
    "letter-spacing",
    "word-spacing",
    "white-space",
    "text-transform",
    "text-indent",
    "list-style-type",
    "list-style",
    "list-style-position",
    "visibility",
    "direction",
    "cursor",
];

// 뷰포트 단위(vw/vh/vmin/vmax) 해석용 뷰포트 크기(px).
#[derive(Clone, Copy)]
pub struct Viewport {
    pub w: f32,
    pub h: f32,
}
impl Default for Viewport {
    fn default() -> Self {
        Viewport { w: 800.0, h: 600.0 }
    }
}

// 뷰포트 단위 1 단위당 px (vw/vh/vmin/vmax). n 은 이미 나눠서 곱해 쓴다.
fn vp_unit_px(unit: Unit, vp: Viewport) -> f32 {
    match unit {
        Unit::Vw => vp.w,
        Unit::Vh => vp.h,
        Unit::Vmin => vp.w.min(vp.h),
        Unit::Vmax => vp.w.max(vp.h),
        _ => 0.0,
    }
}

pub fn style_tree<'a>(dom: &'a Dom, stylesheet: &'a Stylesheet) -> StyledNode<'a> {
    style_tree_vp(dom, stylesheet, Viewport::default())
}

pub fn style_tree_vp<'a>(
    dom: &'a Dom,
    stylesheet: &'a Stylesheet,
    vp: Viewport,
) -> StyledNode<'a> {
    let index = RuleIndex::build(stylesheet);
    let mut ancestors: Vec<&ElementData> = Vec::new();
    style_node(dom, dom.root, &index, &mut ancestors, None, &SiblingCtx::default(), vp)
}

// parent: 부모 요소의 계산값(상속 원천). 루트는 None. sib: 형제 문맥. vp: 뷰포트 크기.
fn style_node<'a>(
    dom: &'a Dom,
    id: NodeId,
    index: &RuleIndex<'a>,
    ancestors: &mut Vec<&'a ElementData>,
    parent: Option<&PropertyMap>,
    sib: &SiblingCtx,
    vp: Viewport,
) -> StyledNode<'a> {
    let node = dom.get(id);
    match node.node_type {
        NodeType::Element(ref elem) => {
            let mut values = specified_values(elem, ancestors, sib, index);
            let parent_fs = parent
                .and_then(|p| p.get("font-size"))
                .and_then(|v| match v {
                    Value::Length(n, Unit::Px) => Some(*n),
                    _ => None,
                })
                .unwrap_or(DEFAULT_FONT_SIZE);
            // font-size: 상대 단위를 부모 기준으로 해석해 px 로 확정 (computed value)
            let fs = match values.get("font-size") {
                Some(Value::Length(n, Unit::Px)) => *n,
                Some(Value::Length(n, Unit::Em)) => n * parent_fs,
                Some(Value::Length(n, Unit::Rem)) => n * DEFAULT_FONT_SIZE,
                Some(Value::Length(n, Unit::Percent)) => n / 100.0 * parent_fs,
                Some(Value::Length(n, u @ (Unit::Vw | Unit::Vh | Unit::Vmin | Unit::Vmax))) => {
                    n / 100.0 * vp_unit_px(*u, vp)
                }
                _ => parent_fs, // 미지정/키워드 → 상속
            };
            values.insert("font-size".to_string(), Value::Length(fs, Unit::Px));
            // align 속성 / <center> 요소 → text-align (CSS 미지정 시). 표준의
            // presentational hint. 여기서 안 잡히면 아래 상속 루프가 부모값 물려줌.
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
                }
            }
            // 상속: 상속 속성이 명시 안 됐으면 부모 계산값 복사.
            // 커스텀 프로퍼티(--*)도 전부 상속 (테마 토큰이 하위로 흐름).
            if let Some(p) = parent {
                for &name in INHERITED {
                    if !values.contains_key(name) {
                        if let Some(v) = p.get(name) {
                            values.insert(name.to_string(), v.clone());
                        }
                    }
                }
                for (k, v) in p {
                    if k.starts_with("--") && !values.contains_key(k) {
                        values.insert(k.clone(), v.clone());
                    }
                }
            }
            // var() 해석: 계산된 커스텀 프로퍼티로 치환 후 재파싱.
            let custom: HashMap<String, String> = values
                .iter()
                .filter(|(k, _)| k.starts_with("--"))
                .filter_map(|(k, v)| match v {
                    Value::Keyword(s) => Some((k.clone(), s.clone())),
                    _ => None,
                })
                .collect();
            let var_props: Vec<(String, String)> = values
                .iter()
                .filter_map(|(k, v)| match v {
                    Value::Var(raw) => Some((k.clone(), raw.clone())),
                    _ => None,
                })
                .collect();
            for (name, raw) in var_props {
                values.remove(&name);
                for decl in crate::css::resolve_var(&name, &raw, &custom) {
                    values.insert(decl.name, decl.value);
                }
            }
            // font-size 외 속성의 em/rem 을 px 로 확정한다 (computed value).
            // em 은 요소 자신의 font-size(fs), rem 은 루트 기준(DEFAULT_FONT_SIZE).
            // 퍼센트는 레이아웃(calculate_width)이 컨테이닝 블록 폭 기준으로 해석하므로 보존.
            for (k, v) in values.iter_mut() {
                if k == "font-size" {
                    continue;
                }
                if let Value::Length(n, unit) = v {
                    match unit {
                        Unit::Em => *v = Value::Length(*n * fs, Unit::Px),
                        Unit::Rem => *v = Value::Length(*n * DEFAULT_FONT_SIZE, Unit::Px),
                        Unit::Vw | Unit::Vh | Unit::Vmin | Unit::Vmax => {
                            *v = Value::Length(*n / 100.0 * vp_unit_px(*unit, vp), Unit::Px)
                        }
                        _ => {}
                    }
                }
            }
            ancestors.push(elem);
            // 자식별 형제 문맥 계산: 요소 자식의 인덱스/총수/선행형제
            let elem_children: Vec<NodeId> = node
                .children
                .iter()
                .copied()
                .filter(|&c| matches!(dom.get(c).node_type, NodeType::Element(_)))
                .collect();
            let total = elem_children.len();
            let mut prev_elems: Vec<&ElementData> = Vec::new();
            let mut children = Vec::with_capacity(node.children.len());
            for &child in &node.children {
                if let NodeType::Element(ref ce) = dom.get(child).node_type {
                    let idx = prev_elems.len() + 1;
                    let has_children = !dom.get(child).children.is_empty();
                    let csib = SiblingCtx { index: idx, total, prev: &prev_elems, has_children };
                    children.push(style_node(dom, child, index, ancestors, Some(&values), &csib, vp));
                    prev_elems.push(ce);
                } else {
                    children.push(style_node(
                        dom,
                        child,
                        index,
                        ancestors,
                        Some(&values),
                        &SiblingCtx::default(),
                        vp,
                    ));
                }
            }
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
                    style_node(dom, child, index, ancestors, parent, &SiblingCtx::default(), vp)
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
    fn viewport_units_resolve_against_viewport() {
        let root = crate::html::parse_dom("<div></div>".to_string());
        let ss = crate::css::parse(
            "div { width: 50vw; height: 100vh; padding-left: 10vmin; }".to_string(),
        );
        // 뷰포트 1000 x 600
        let styled = style_tree_vp(&root, &ss, Viewport { w: 1000.0, h: 600.0 });
        assert_eq!(styled.value("width"), Some(Value::Length(500.0, Unit::Px)));
        assert_eq!(styled.value("height"), Some(Value::Length(600.0, Unit::Px)));
        // vmin = min(1000,600)=600 → 10vmin = 60px
        assert_eq!(styled.value("padding-left"), Some(Value::Length(60.0, Unit::Px)));
    }

    #[test]
    fn em_rem_resolved_to_px_percent_kept_for_layout() {
        let root = crate::html::parse_dom("<div></div>".to_string());
        let ss = crate::css::parse(
            "div { width: 50%; font-size: 20px; margin-top: 2em; padding-left: 1rem; }"
                .to_string(),
        );
        let styled = style_tree(&root, &ss);
        // 퍼센트 width 는 보존 → 레이아웃이 컨테이닝 블록 폭 기준으로 해석
        assert_eq!(styled.value("width"), Some(Value::Length(50.0, Unit::Percent)));
        // em 은 요소 자신의 font-size(20px) 기준 → 2em = 40px
        assert_eq!(styled.value("margin-top"), Some(Value::Length(40.0, Unit::Px)));
        // rem 은 루트(16px) 기준 → 1rem = 16px
        assert_eq!(styled.value("padding-left"), Some(Value::Length(16.0, Unit::Px)));
    }

    #[test]
    fn bold_italic_from_ua_and_inheritance() {
        // <b>/<i> 는 UA 로 볼드/이탤릭
        let root = crate::html::parse_dom("<p><b>x</b><i>y</i></p>".to_string());
        let ss = crate::css::user_agent_stylesheet();
        let p = style_tree(&root, &ss);
        assert!(p.children[0].is_bold(), "<b> 볼드");
        assert!(p.children[1].is_italic(), "<i> 이탤릭");

        // font-weight: 700(숫자) → bold 정규화 + 자식 상속
        let root2 = crate::html::parse_dom("<div><span>t</span></div>".to_string());
        let mut ss2 = crate::css::user_agent_stylesheet();
        ss2.rules.extend(crate::css::parse("div { font-weight: 700; }".to_string()).rules);
        let div = style_tree(&root2, &ss2);
        assert!(div.is_bold(), "font-weight:700 → bold");
        assert!(div.children[0].is_bold(), "span 이 볼드 상속");
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
    fn custom_property_and_var_resolve() {
        // 커스텀 프로퍼티는 상속되고 var() 로 해석된다 (테마 토큰)
        let root = crate::html::parse_dom("<div class=\"t\"><p class=\"b\"></p></div>".to_string());
        let ss = crate::css::parse(".t { --c: #ff0000; } .b { color: var(--c); }".to_string());
        let styled = style_tree(&root, &ss);
        let p = &styled.children[0];
        assert_eq!(
            p.value("color"),
            Some(Value::Color(crate::css::Color { r: 255, g: 0, b: 0, a: 255 })),
            "var(--c) 가 상속된 커스텀 프로퍼티로 해석"
        );
    }

    #[test]
    fn var_fallback_when_undefined() {
        let root = crate::html::parse_dom("<div class=\"b\"></div>".to_string());
        let ss = crate::css::parse(".b { width: var(--missing, 42px); }".to_string());
        let styled = style_tree(&root, &ss);
        assert_eq!(styled.value("width"), Some(Value::Length(42.0, Unit::Px)), "미정의 → fallback");
    }

    fn col(r: u8, g: u8, b: u8) -> Value {
        Value::Color(crate::css::Color { r, g, b, a: 255 })
    }

    #[test]
    fn pseudo_classes_and_child_combinator() {
        let root =
            crate::html::parse_dom("<ul><li>a</li><li>b</li><li>c</li></ul>".to_string());
        let ss = crate::css::parse(
            "ul > li:first-child { color: #ff0000; } \
             li:nth-child(2) { color: #00ff00; } \
             li:last-child { color: #0000ff; }"
                .to_string(),
        );
        let ul = style_tree(&root, &ss);
        assert_eq!(ul.children[0].value("color"), Some(col(255, 0, 0)), ":first-child");
        assert_eq!(ul.children[1].value("color"), Some(col(0, 255, 0)), ":nth-child(2)");
        assert_eq!(ul.children[2].value("color"), Some(col(0, 0, 255)), ":last-child");
    }

    #[test]
    fn nth_child_formula_and_not() {
        let root = crate::html::parse_dom(
            "<ul><li></li><li></li><li></li><li></li></ul>".to_string(),
        );
        // 2n(짝수) 빨강, :not(:first-child) 파랑(1번 제외 나머지가 파랑이지만 짝수는 빨강이 이김?
        // 특이도 동일 → 문서 순서 뒤가 이김: not 규칙이 뒤라 파랑. 분리 테스트로.
        let ss = crate::css::parse("li:nth-child(2n) { color: #ff0000; }".to_string());
        let ul = style_tree(&root, &ss);
        assert_eq!(ul.children[0].value("color"), None, "1번(홀수) 미매칭");
        assert_eq!(ul.children[1].value("color"), Some(col(255, 0, 0)), "2번(짝수)");
        assert_eq!(ul.children[3].value("color"), Some(col(255, 0, 0)), "4번(짝수)");
        // :not
        let ss2 = crate::css::parse("li:not(:first-child) { color: #00ff00; }".to_string());
        let ul2 = style_tree(&root, &ss2);
        assert_eq!(ul2.children[0].value("color"), None, ":not(:first-child) 는 1번 제외");
        assert_eq!(ul2.children[1].value("color"), Some(col(0, 255, 0)));
    }

    #[test]
    fn child_combinator_excludes_grandchildren() {
        let root = crate::html::parse_dom(
            "<div class=\"a\"><p>direct</p><span><p>deep</p></span></div>".to_string(),
        );
        let ss = crate::css::parse(".a > p { color: #ff0000; }".to_string());
        let div = style_tree(&root, &ss);
        assert_eq!(div.children[0].value("color"), Some(col(255, 0, 0)), "직속 p 매칭");
        // span 안의 p 는 자식 결합자로 비매칭
        let deep_p = &div.children[1].children[0];
        assert_eq!(deep_p.value("color"), None, "손자 p 는 > 로 비매칭");
    }

    #[test]
    fn adjacent_sibling_combinator() {
        let root = crate::html::parse_dom(
            "<div><h2></h2><p class=\"x\"></p><p class=\"y\"></p></div>".to_string(),
        );
        let ss = crate::css::parse("h2 + p { color: #ff0000; }".to_string());
        let div = style_tree(&root, &ss);
        // h2 바로 다음 p(.x) 만 매칭, 그다음 p(.y) 는 비매칭
        assert_eq!(div.children[1].value("color"), Some(col(255, 0, 0)), "h2 직후 p");
        assert_eq!(div.children[2].value("color"), None, "인접 아닌 p 비매칭");
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
    fn attribute_operator_selectors() {
        let root = crate::html::parse_dom(
            "<div><a href=\"https://x.com\"></a><a href=\"http://y.io\"></a><a href=\"https://z.org\"></a></div>"
                .to_string(),
        );
        let ss = crate::css::parse(
            "a[href^=\"https\"] { color: #ff0000; } \
             a[href$=\".org\"] { width: 5px; } \
             a[href*=\"y.io\"] { height: 3px; }"
                .to_string(),
        );
        let div = style_tree(&root, &ss);
        assert_eq!(div.children[0].value("color"), Some(col(255, 0, 0)), "^=https");
        assert_eq!(div.children[1].value("color"), None, "http 는 ^=https 아님");
        assert_eq!(div.children[1].value("height"), Some(Value::Length(3.0, Unit::Px)), "*=y.io");
        assert_eq!(div.children[2].value("width"), Some(Value::Length(5.0, Unit::Px)), "$=.org");
        assert_eq!(div.children[2].value("color"), Some(col(255, 0, 0)), "z 도 ^=https");
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
