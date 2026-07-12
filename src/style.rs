use std::collections::HashMap;

use crate::css::{
    Combinator, PseudoElement, Rule, Selector, SimpleSelector, Specificity, Stylesheet, Unit, Value,
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
    pub type_index: usize, // 같은 타입 형제 중 1-기반 위치 (of-type 용)
    pub type_total: usize, // 같은 타입 형제 수
    pub prev: &'a [&'a ElementData], // 선행 요소 형제 (문서 순서)
    pub has_children: bool,          // :empty 판별용
}

impl Default for SiblingCtx<'_> {
    fn default() -> Self {
        SiblingCtx { index: 1, total: 1, type_index: 1, type_total: 1, prev: &[], has_children: false }
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

// an+b 매칭: 1-기반 위치 pos 에 대해 pos = a*k + b (k>=0) 정수해 존재?
fn nth_matches(a: i32, b: i32, pos: usize) -> bool {
    let n = pos as i32;
    if a == 0 {
        n == b
    } else {
        let diff = n - b;
        diff % a == 0 && diff / a >= 0
    }
}

fn matches_pseudo(elem: &ElementData, p: &crate::css::Pseudo, sib: Option<&SiblingCtx>) -> bool {
    use crate::css::Pseudo;
    match p {
        Pseudo::Dynamic => false, // hover/focus/active/visited 등 정적 렌더에선 비매칭
        Pseudo::Not(inner) => !inner.iter().any(|s| matches_compound(elem, s, sib)),
        Pseudo::Is(inner) => inner.iter().any(|s| matches_compound(elem, s, sib)),
        // 구조적: 대상(sib=Some)만 정확 평가, 비대상은 통과(근사)
        Pseudo::FirstChild => sib.map(|s| s.index == 1).unwrap_or(true),
        Pseudo::LastChild => sib.map(|s| s.index == s.total).unwrap_or(true),
        Pseudo::OnlyChild => sib.map(|s| s.total == 1).unwrap_or(true),
        Pseudo::Root => sib.map(|s| s.prev.is_empty() && s.total >= 1).unwrap_or(true) && elem.tag_name == "html",
        Pseudo::Empty => sib.map(|s| !s.has_children).unwrap_or(true),
        Pseudo::NthChild(a, b) => sib.map(|s| nth_matches(*a, *b, s.index)).unwrap_or(true),
        Pseudo::NthLastChild(a, b) => {
            sib.map(|s| nth_matches(*a, *b, s.total + 1 - s.index)).unwrap_or(true)
        }
        Pseudo::NthOfType(a, b) => sib.map(|s| nth_matches(*a, *b, s.type_index)).unwrap_or(true),
        Pseudo::NthLastOfType(a, b) => {
            sib.map(|s| nth_matches(*a, *b, s.type_total + 1 - s.type_index)).unwrap_or(true)
        }
        Pseudo::OnlyOfType => sib.map(|s| s.type_total == 1).unwrap_or(true),
        // 폼 상태: 요소 속성으로 정적 판별
        Pseudo::Checked => {
            let has = |a: &str| elem.attributes.get(a).is_some();
            has("checked") || has("selected")
        }
        Pseudo::Disabled => elem.attributes.get("disabled").is_some(),
        Pseudo::Enabled => is_form_element(elem) && elem.attributes.get("disabled").is_none(),
        Pseudo::Required => elem.attributes.get("required").is_some(),
        Pseudo::Optional => is_form_field(elem) && elem.attributes.get("required").is_none(),
        // :link — href 있는 하이퍼링크. 정적 렌더엔 방문 이력이 없어 모든 링크가 unvisited.
        Pseudo::Link => {
            matches!(elem.tag_name.as_str(), "a" | "area" | "link")
                && elem.attributes.contains_key("href")
        }
    }
}

// disabled/enabled 대상이 되는 폼 요소
fn is_form_element(elem: &ElementData) -> bool {
    matches!(
        elem.tag_name.as_str(),
        "input" | "button" | "select" | "textarea" | "option" | "optgroup" | "fieldset"
    )
}

// required/optional 대상이 되는 폼 필드
fn is_form_field(elem: &ElementData) -> bool {
    matches!(elem.tag_name.as_str(), "input" | "select" | "textarea")
}

type MatchedRule<'a> = (Specificity, &'a Rule);

// want_pseudo: None=요소 자신을 스타일하는 규칙만, Some(x)=해당 의사요소 규칙만.
fn match_rule<'a>(
    elem: &ElementData,
    ancestors: &[&ElementData],
    sib: &SiblingCtx,
    rule: &'a Rule,
    want_pseudo: Option<PseudoElement>,
) -> Option<MatchedRule<'a>> {
    rule.selectors
        .iter()
        .filter(|selector| selector.subject().pseudo_element == want_pseudo)
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

// 특정 의사요소(::before/::after)에 매칭되는 규칙들만 모아 계산값 맵을 만든다.
// 매칭 대상은 소유 요소 elem, want_pseudo 로 의사요소 규칙만 필터.
fn pseudo_specified_values(
    elem: &ElementData,
    ancestors: &[&ElementData],
    sib: &SiblingCtx,
    index: &RuleIndex,
    which: PseudoElement,
) -> PropertyMap {
    let mut values = HashMap::new();
    let mut rules: Vec<MatchedRule> = index
        .candidate_indices(elem)
        .into_iter()
        .filter_map(|i| match_rule(elem, ancestors, sib, &index.rules[i], Some(which)))
        .collect();
    rules.sort_by(|&(a, _), &(b, _)| a.cmp(&b));
    // 일반 선언 먼저, 그다음 important (important 가 특이도 무관하게 이긴다)
    for (_, rule) in &rules {
        for d in &rule.declarations {
            if !d.important {
                values.insert(d.name.clone(), d.value.clone());
            }
        }
    }
    for (_, rule) in &rules {
        for d in &rule.declarations {
            if d.important {
                values.insert(d.name.clone(), d.value.clone());
            }
        }
    }
    values
}

// 레거시 색 속성 값을 CSS 색 문자열로 정규화 (bgcolor="ff6600" → "#ff6600").
fn norm_color(v: &str) -> Option<String> {
    let v = v.trim();
    if v.is_empty() {
        return None;
    }
    if v.starts_with('#') {
        return Some(v.to_string());
    }
    let hexish = v.chars().all(|c| c.is_ascii_hexdigit());
    if hexish && matches!(v.len(), 3 | 4 | 6 | 8) {
        return Some(format!("#{}", v));
    }
    Some(v.to_ascii_lowercase()) // 이름 색 (red 등)은 그대로 색 파서에 위임
}

// 레거시 길이 속성: 순수 숫자는 px, "50%" 는 그대로.
fn norm_len(v: &str) -> String {
    let v = v.trim();
    if v.ends_with('%') || v.ends_with("px") {
        return v.to_string();
    }
    if v.parse::<f32>().is_ok() {
        return format!("{}px", v);
    }
    v.to_string()
}

fn attr_text_align(v: &str) -> Option<&'static str> {
    match v.trim().to_ascii_lowercase().as_str() {
        "left" => Some("left"),
        "right" => Some("right"),
        "center" | "middle" => Some("center"),
        "justify" => Some("justify"),
        _ => None,
    }
}

// <font size="1".."7"> → 픽셀 (상대 +/- 는 미지원)
fn attr_font_size(v: &str) -> Option<f32> {
    const SIZES: [f32; 7] = [10.0, 13.0, 16.0, 18.0, 24.0, 32.0, 48.0];
    let n: usize = v.trim().parse().ok()?;
    if (1..=7).contains(&n) {
        Some(SIZES[n - 1])
    } else {
        None
    }
}

// HTML 표현 속성(presentational hints, 표준 §15)을 CSS 선언 문자열로.
// 저작자/UA 규칙보다 낮은 기본 레이어로 얹혀 캐스케이드에서 덮일 수 있다.
fn presentational_css(elem: &ElementData) -> String {
    let a = &elem.attributes;
    let get = |k: &str| a.get(k).map(|s| s.trim()).filter(|s| !s.is_empty());
    let tag = elem.tag_name.as_str();
    let mut out: Vec<String> = Vec::new();

    if let Some(v) = get("bgcolor") {
        if let Some(c) = norm_color(v) {
            out.push(format!("background-color:{}", c));
        }
    }
    // dir 속성 → direction (전역 속성, 모든 요소). auto 는 콘텐츠 판별이라 생략.
    if let Some(v) = get("dir") {
        match v.to_ascii_lowercase().as_str() {
            "rtl" => out.push("direction:rtl".into()),
            "ltr" => out.push("direction:ltr".into()),
            _ => {}
        }
    }
    // 테이블 자체의 width/height 만 CSS 로. 셀/행/열(td/th/tr/col)의 width 는
    // 테이블 레이아웃 알고리즘이 속성을 직접 읽어 열 폭을 계산하므로 CSS 로 넣으면
    // 셀 박스가 열 폭을 기준으로 % 를 재해석해 어긋난다 (cell_width 참고).
    if tag == "table" {
        if let Some(v) = get("width") {
            out.push(format!("width:{}", norm_len(v)));
        }
        if let Some(v) = get("height") {
            out.push(format!("height:{}", norm_len(v)));
        }
    }
    match tag {
        "body" => {
            if let Some(v) = get("text") {
                if let Some(c) = norm_color(v) {
                    out.push(format!("color:{}", c));
                }
            }
            if let Some(v) = get("background") {
                out.push(format!("background-image:url({})", v));
            }
        }
        "font" | "basefont" => {
            if let Some(v) = get("color") {
                if let Some(c) = norm_color(v) {
                    out.push(format!("color:{}", c));
                }
            }
            if let Some(v) = get("face") {
                out.push(format!("font-family:{}", v));
            }
            if let Some(v) = get("size") {
                if let Some(px) = attr_font_size(v) {
                    out.push(format!("font-size:{}px", px));
                }
            }
        }
        "td" | "th" => {
            if let Some(v) = get("align") {
                if let Some(t) = attr_text_align(v) {
                    out.push(format!("text-align:{}", t));
                }
            }
            if let Some(v) = get("valign") {
                out.push(format!("vertical-align:{}", v.to_ascii_lowercase()));
            }
            if a.contains_key("nowrap") {
                out.push("white-space:nowrap".into());
            }
        }
        "div" | "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "caption" | "tr" | "thead"
        | "tbody" | "tfoot" | "col" | "colgroup" => {
            if let Some(v) = get("align") {
                if let Some(t) = attr_text_align(v) {
                    out.push(format!("text-align:{}", t));
                }
            }
        }
        "table" => {
            if let Some(v) = get("align") {
                match v.to_ascii_lowercase().as_str() {
                    "center" => out.push("margin-left:auto;margin-right:auto".into()),
                    "left" => out.push("float:left".into()),
                    "right" => out.push("float:right".into()),
                    _ => {}
                }
            }
            if let Some(v) = get("border") {
                out.push(format!("border:{} solid #808080", norm_len(v)));
            }
            if let Some(v) = get("cellspacing") {
                out.push(format!("border-spacing:{}", norm_len(v)));
            }
        }
        "img" => {
            if let Some(v) = get("width") {
                out.push(format!("width:{}", norm_len(v)));
            }
            if let Some(v) = get("height") {
                out.push(format!("height:{}", norm_len(v)));
            }
            if let Some(v) = get("align") {
                match v.to_ascii_lowercase().as_str() {
                    "left" => out.push("float:left".into()),
                    "right" => out.push("float:right".into()),
                    t @ ("top" | "middle" | "bottom") => {
                        out.push(format!("vertical-align:{}", t))
                    }
                    _ => {}
                }
            }
            if let Some(v) = get("border") {
                out.push(format!("border:{} solid #000000", norm_len(v)));
            }
            if let Some(v) = get("hspace") {
                let n = norm_len(v);
                out.push(format!("margin-left:{0};margin-right:{0}", n));
            }
            if let Some(v) = get("vspace") {
                let n = norm_len(v);
                out.push(format!("margin-top:{0};margin-bottom:{0}", n));
            }
        }
        "hr" => {
            if let Some(v) = get("width") {
                out.push(format!("width:{}", norm_len(v)));
            }
            if let Some(v) = get("size") {
                out.push(format!("height:{}", norm_len(v)));
            }
            if let Some(v) = get("color") {
                if let Some(c) = norm_color(v) {
                    out.push(format!("background-color:{0};color:{0}", c));
                }
            }
        }
        "ol" | "ul" => {
            if let Some(v) = get("type") {
                let t = match v {
                    "1" => "decimal",
                    "a" => "lower-alpha",
                    "A" => "upper-alpha",
                    "i" => "lower-roman",
                    "I" => "upper-roman",
                    "disc" | "circle" | "square" => v,
                    _ => "",
                };
                if !t.is_empty() {
                    out.push(format!("list-style-type:{}", t));
                }
            }
        }
        _ => {}
    }
    out.join(";")
}

fn specified_values(
    elem: &ElementData,
    ancestors: &[&ElementData],
    sib: &SiblingCtx,
    index: &RuleIndex,
) -> PropertyMap {
    let mut values = HashMap::new();
    // 표현 속성(presentational hints)을 기본 레이어로 먼저 얹는다 (규칙이 덮을 수 있음)
    let hints = presentational_css(elem);
    if !hints.is_empty() {
        for declaration in crate::css::parse_inline_style(&hints) {
            values.insert(declaration.name, declaration.value);
        }
    }
    let mut rules: Vec<MatchedRule> = index
        .candidate_indices(elem)
        .into_iter()
        .filter_map(|i| match_rule(elem, ancestors, sib, &index.rules[i], None))
        .collect();
    // 오름차순 특이도, 안정 정렬 → 동일 특이도는 문서 순서 유지 (뒤 규칙이 이김)
    rules.sort_by(|&(a, _), &(b, _)| a.cmp(&b));
    // 캐스케이드 우선순위: 일반 저작자 → 일반 인라인 → important 저작자 → important 인라인.
    for (_, rule) in &rules {
        for d in &rule.declarations {
            if !d.important {
                values.insert(d.name.clone(), d.value.clone());
            }
        }
    }
    let inline = elem
        .attributes
        .get("style")
        .map(|style| crate::css::parse_inline_style(style))
        .unwrap_or_default();
    for d in &inline {
        if !d.important {
            values.insert(d.name.clone(), d.value.clone());
        }
    }
    for (_, rule) in &rules {
        for d in &rule.declarations {
            if d.important {
                values.insert(d.name.clone(), d.value.clone());
            }
        }
    }
    for d in inline {
        if d.important {
            values.insert(d.name, d.value);
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

// 아레나 요소의 형제 문맥(인덱스/총수/타입 인덱스/선행형제). element_matches 용.
fn sibling_ctx_for<'a>(dom: &'a Dom, id: NodeId) -> SiblingCtx<'a> {
    let has_children = !dom.get(id).children.is_empty();
    let Some(parent) = dom.get(id).parent else {
        return SiblingCtx::default();
    };
    let elem_sibs: Vec<NodeId> = dom
        .get(parent)
        .children
        .iter()
        .copied()
        .filter(|&c| matches!(dom.get(c).node_type, NodeType::Element(_)))
        .collect();
    let index = elem_sibs.iter().position(|&c| c == id).map(|i| i + 1).unwrap_or(1);
    // 같은 타입 형제 인덱스/총수
    let my_tag = match &dom.get(id).node_type {
        NodeType::Element(e) => e.tag_name.as_str(),
        _ => "",
    };
    let same_tag = |c: NodeId| matches!(&dom.get(c).node_type, NodeType::Element(e) if e.tag_name == my_tag);
    let type_total = elem_sibs.iter().filter(|&&c| same_tag(c)).count().max(1);
    let type_index = elem_sibs
        .iter()
        .take_while(|&&c| c != id)
        .filter(|&&c| same_tag(c))
        .count()
        + 1;
    // prev 는 문서 순서 선행 형제. 라이프타임상 leak 대신 빈 슬라이스(근사) — 대부분
    // querySelector 형제 결합자는 드묾. 인덱스/총수만 정확.
    SiblingCtx {
        index,
        total: elem_sibs.len().max(1),
        type_index,
        type_total,
        prev: &[],
        has_children,
    }
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
    "text-shadow-x",
    "text-shadow-y",
    "text-shadow-color",
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

// em/rem/vw 등 문맥 단위를 px 로 확정 (재귀: min/max/clamp 인자도). %는 레이아웃까지 보존.
// em 은 요소 font-size(fs), rem 은 루트 요소 font-size(root_fs) 기준.
fn resolve_units(v: &mut Value, fs: f32, root_fs: f32, vp: Viewport) {
    match v {
        Value::Length(n, unit) => match unit {
            Unit::Em => *v = Value::Length(*n * fs, Unit::Px),
            Unit::Rem => *v = Value::Length(*n * root_fs, Unit::Px),
            Unit::Vw | Unit::Vh | Unit::Vmin | Unit::Vmax => {
                *v = Value::Length(*n / 100.0 * vp_unit_px(*unit, vp), Unit::Px)
            }
            _ => {}
        },
        Value::MinMax(_, args) => {
            for a in args.iter_mut() {
                resolve_units(a, fs, root_fs, vp);
            }
        }
        _ => {}
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
    style_tree_full(dom, stylesheet, vp, &HashMap::new())
}

// pseudo: 합성 의사요소 노드 id → 그 노드의 명시값(생성 콘텐츠 규칙에서). 이 노드들은
// 일반 캐스케이드를 건너뛰고 이 맵의 값을 쓴다. generate_pseudo_elements 가 만든다.
pub fn style_tree_full<'a>(
    dom: &'a Dom,
    stylesheet: &'a Stylesheet,
    vp: Viewport,
    pseudo: &PseudoStyles,
) -> StyledNode<'a> {
    let index = RuleIndex::build(stylesheet);
    let mut ancestors: Vec<&ElementData> = Vec::new();
    style_node(dom, dom.root, &index, &mut ancestors, None, &SiblingCtx::default(), vp, pseudo, &stylesheet.keyframes, DEFAULT_FONT_SIZE)
}

pub type PseudoStyles = HashMap<NodeId, PropertyMap>;

// 생성 콘텐츠 여부: none/normal/이스케이프 키워드/미지원 함수(attr()/counter())/빈 문자열 제외.
fn is_generated_content(s: &str) -> bool {
    !s.is_empty()
        && !matches!(s, "none" | "normal" | "inherit" | "initial" | "unset")
        && !s.contains('(')
}

fn is_synthetic_pseudo(elem: &ElementData) -> bool {
    elem.tag_name.starts_with("::")
}

// ::before/::after 생성 콘텐츠를 위한 합성 노드를 DOM 에 주입하고, 그 노드의 명시값
// 맵을 돌려준다. 스타일/레이아웃 전에 한 번 호출한다(재빌드 때 재주입 안 하도록 결과 보관).
// 합성 노드는 소유 요소의 첫/마지막 자식으로 삽입되며 tag 는 "::before"/"::after".
pub fn generate_pseudo_elements(dom: &mut Dom, sheet: &Stylesheet) -> PseudoStyles {
    let index = RuleIndex::build(sheet);
    let mut plans: Vec<(NodeId, PseudoElement, PropertyMap)> = Vec::new();
    {
        let mut ancestors: Vec<&ElementData> = Vec::new();
        let mut counters: HashMap<String, i32> = HashMap::new();
        collect_pseudo_plans(dom, dom.root, &index, &mut ancestors, &SiblingCtx::default(), &mut plans, &mut counters);
    }
    let mut map = HashMap::new();
    for (owner, which, mut vals) in plans {
        let content = match vals.get("content") {
            Some(Value::Keyword(s)) if is_generated_content(s) => s.clone(),
            _ => continue,
        };
        vals.entry("display".to_string()).or_insert(Value::Keyword("inline".to_string()));
        let tag = match which {
            PseudoElement::Before => "::before",
            PseudoElement::After => "::after",
        };
        let el = dom.create_element(tag);
        let txt = dom.create_text(content);
        dom.append_child(el, txt);
        match which {
            PseudoElement::Before => {
                let first = dom.get(owner).children.first().copied();
                dom.insert_before(owner, el, first);
            }
            PseudoElement::After => dom.append_child(owner, el),
        }
        map.insert(el, vals);
    }
    map
}

// style_node 구조 순회를 미러링하며 각 요소의 ::before/::after 명시값을 수집.
#[allow(clippy::too_many_arguments)]
fn collect_pseudo_plans<'a>(
    dom: &'a Dom,
    id: NodeId,
    index: &RuleIndex<'a>,
    ancestors: &mut Vec<&'a ElementData>,
    sib: &SiblingCtx,
    out: &mut Vec<(NodeId, PseudoElement, PropertyMap)>,
    counters: &mut HashMap<String, i32>,
) {
    let node = dom.get(id);
    let NodeType::Element(ref elem) = node.node_type else {
        return;
    };
    // 요소 자신의 counter-reset/increment 를 문서 순서로 적용 (::before content 전)
    let evals = specified_values(elem, ancestors, sib, index);
    if let Some(Value::Keyword(r)) = evals.get("counter-reset") {
        apply_counter_op(r, counters, true);
    }
    if let Some(Value::Keyword(inc)) = evals.get("counter-increment") {
        apply_counter_op(inc, counters, false);
    }
    for which in [PseudoElement::Before, PseudoElement::After] {
        let mut vals = pseudo_specified_values(elem, ancestors, sib, index, which);
        // content 의 counter()/counters() 를 현재 값으로 치환, open-quote/close-quote 를 인용부호로
        if let Some(Value::Keyword(c)) = vals.get("content") {
            let resolved = if c.contains("counter(") || c.contains("counters(") {
                resolve_counters(c, counters)
            } else {
                c.clone()
            };
            let resolved = match resolved.as_str() {
                "open-quote" => "\u{201C}".to_string(),  // "
                "close-quote" => "\u{201D}".to_string(), // "
                "no-open-quote" | "no-close-quote" => String::new(),
                _ => resolved,
            };
            if resolved != *c {
                vals.insert("content".to_string(), Value::Keyword(resolved));
            }
        }
        if vals.contains_key("content") {
            out.push((id, which, vals));
        }
    }
    ancestors.push(elem);
    let elem_children: Vec<NodeId> = node
        .children
        .iter()
        .copied()
        .filter(|&c| matches!(dom.get(c).node_type, NodeType::Element(_)))
        .collect();
    let total = elem_children.len();
    let mut type_totals: HashMap<&str, usize> = HashMap::new();
    for &c in &elem_children {
        if let NodeType::Element(e) = &dom.get(c).node_type {
            *type_totals.entry(e.tag_name.as_str()).or_insert(0) += 1;
        }
    }
    let mut type_seen: HashMap<&str, usize> = HashMap::new();
    let mut prev_elems: Vec<&ElementData> = Vec::new();
    for &child in &node.children {
        if let NodeType::Element(ref ce) = dom.get(child).node_type {
            let idx = prev_elems.len() + 1;
            let tcount = type_seen.entry(ce.tag_name.as_str()).or_insert(0);
            *tcount += 1;
            let has_children = !dom.get(child).children.is_empty();
            let csib = SiblingCtx {
                index: idx,
                total,
                type_index: *tcount,
                type_total: *type_totals.get(ce.tag_name.as_str()).unwrap_or(&1),
                prev: &prev_elems,
                has_children,
            };
            collect_pseudo_plans(dom, child, index, ancestors, &csib, out, counters);
            prev_elems.push(ce);
        }
    }
    ancestors.pop();
}

// counter-reset/increment 값 적용. reset=true 면 지정값(기본 0)으로 설정, 아니면 증가(기본 1).
// 구문: "name [n] name2 [n2] ..." (플랫 근사 — 중첩 스코프 무시).
fn apply_counter_op(text: &str, counters: &mut HashMap<String, i32>, reset: bool) {
    let toks: Vec<&str> = text.split_whitespace().collect();
    let mut i = 0;
    while i < toks.len() {
        let name = toks[i].to_string();
        let num = toks.get(i + 1).and_then(|t| t.parse::<i32>().ok());
        let step = num.unwrap_or(if reset { 0 } else { 1 });
        if reset {
            counters.insert(name, step);
        } else {
            *counters.entry(name).or_insert(0) += step;
        }
        i += if num.is_some() { 2 } else { 1 };
    }
}

// content 의 counter(name[,style]) / counters(name, sep[,style]) 를 현재 카운터 값으로 치환.
fn resolve_counters(content: &str, counters: &HashMap<String, i32>) -> String {
    let mut out = String::new();
    let mut rest = content;
    loop {
        // 다음 counter( 또는 counters( 위치
        let pos = ["counters(", "counter("]
            .iter()
            .filter_map(|kw| rest.find(kw).map(|p| (p, *kw)))
            .min_by_key(|(p, _)| *p);
        let Some((p, kw)) = pos else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..p]);
        let after = &rest[p + kw.len()..];
        let Some(close) = after.find(')') else {
            out.push_str(&rest[p..]);
            break;
        };
        let args = &after[..close];
        // 첫 인자 = 카운터 이름
        let name = args.split(',').next().unwrap_or("").trim();
        let val = counters.get(name).copied().unwrap_or(0);
        out.push_str(&val.to_string());
        rest = &after[close + 1..];
    }
    out
}

// parent: 부모 요소의 계산값(상속 원천). 루트는 None. sib: 형제 문맥. vp: 뷰포트 크기.
// pseudo: 합성 의사요소 노드의 명시값 맵.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
fn style_node<'a>(
    dom: &'a Dom,
    id: NodeId,
    index: &RuleIndex<'a>,
    ancestors: &mut Vec<&'a ElementData>,
    parent: Option<&PropertyMap>,
    sib: &SiblingCtx,
    vp: Viewport,
    pseudo: &PseudoStyles,
    keyframes: &std::collections::HashMap<String, Vec<(String, Value)>>,
    root_fs: f32, // 루트 요소 계산 font-size (rem 해석 기준)
) -> StyledNode<'a> {
    let node = dom.get(id);
    match node.node_type {
        NodeType::Element(ref elem) => {
            // 합성 의사요소 노드: 캐스케이드 대신 사전 계산된 명시값 사용.
            let mut values = match pseudo.get(&id) {
                Some(v) => v.clone(),
                None => specified_values(elem, ancestors, sib, index),
            };
            // CSS 전역 키워드: inherit → 부모 계산값, unset → 제거(상속 속성이면 아래
            // 상속 루프가 부모값 복사, 아니면 기본값), initial → 제거(기본값 근사).
            {
                let wide: Vec<String> = values
                    .iter()
                    .filter(|(_, v)| {
                        matches!(v, Value::Keyword(k)
                            if k == "inherit" || k == "initial" || k == "unset")
                    })
                    .map(|(k, _)| k.clone())
                    .collect();
                for k in wide {
                    let kw = match values.get(&k) {
                        Some(Value::Keyword(s)) => s.clone(),
                        _ => continue,
                    };
                    match kw.as_str() {
                        "inherit" => match parent.and_then(|p| p.get(&k)) {
                            Some(v) => {
                                values.insert(k.clone(), v.clone());
                            }
                            None => {
                                values.remove(&k);
                            }
                        },
                        _ => {
                            values.remove(&k);
                        }
                    }
                }
            }
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
                Some(Value::Length(n, Unit::Rem)) => n * root_fs,
                Some(Value::Length(n, Unit::Percent)) => n / 100.0 * parent_fs,
                Some(Value::Length(n, u @ (Unit::Vw | Unit::Vh | Unit::Vmin | Unit::Vmax))) => {
                    n / 100.0 * vp_unit_px(*u, vp)
                }
                // font-size: clamp/min/max — 인자 단위를 부모 기준으로 확정 후 계산
                Some(Value::MinMax(kind, args)) => {
                    let kind = *kind;
                    let mut args = args.clone();
                    for a in args.iter_mut() {
                        resolve_units(a, parent_fs, root_fs, vp); // em 은 부모 font-size 기준
                    }
                    crate::css::eval_minmax(kind, &args, parent_fs)
                }
                _ => parent_fs, // 미지정/키워드 → 상속
            };
            values.insert("font-size".to_string(), Value::Length(fs, Unit::Px));
            // 루트 요소면 이 요소의 계산 font-size 가 자손의 rem 기준이 된다.
            let child_root_fs = if parent.is_none() { fs } else { root_fs };
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
                resolve_units(v, fs, root_fs, vp);
            }
            // currentColor 를 요소 계산 color 로 치환 (border-color/background-color 등).
            let cur_color = match values.get("color") {
                Some(Value::Color(c)) => Some(*c),
                _ => None,
            };
            if let Some(cc) = cur_color {
                for (k, v) in values.iter_mut() {
                    if k == "color" {
                        continue;
                    }
                    if matches!(v, Value::Keyword(s) if s.eq_ignore_ascii_case("currentcolor")) {
                        *v = Value::Color(cc);
                    }
                }
            }
            // animation: @keyframes 최종(100%/to) 프레임을 적용 (정적 렌더 = 애니메이션 종료 근사).
            // 진입 애니메이션의 opacity:0/off-screen 초기상태로 콘텐츠가 안 보이던 문제를 완화.
            if let Some(Value::Keyword(name)) = values.get("animation-name").cloned() {
                if let Some(frame) = keyframes.get(&name) {
                    for (k, v) in frame {
                        let mut vv = v.clone();
                        resolve_units(&mut vv, fs, root_fs, vp);
                        values.insert(k.clone(), vv);
                    }
                }
            }
            ancestors.push(elem);
            // 자식별 형제 문맥 계산: 요소 자식의 인덱스/총수/선행형제.
            // 합성 의사요소(::before/::after)는 구조적 선택자에서 제외(요소 트리에 없음).
            let elem_children: Vec<NodeId> = node
                .children
                .iter()
                .copied()
                .filter(|&c| match &dom.get(c).node_type {
                    NodeType::Element(e) => !is_synthetic_pseudo(e),
                    _ => false,
                })
                .collect();
            let total = elem_children.len();
            // 타입별 총수 (of-type 선택자용)
            let mut type_totals: HashMap<&str, usize> = HashMap::new();
            for &c in &elem_children {
                if let NodeType::Element(e) = &dom.get(c).node_type {
                    *type_totals.entry(e.tag_name.as_str()).or_insert(0) += 1;
                }
            }
            let mut type_seen: HashMap<&str, usize> = HashMap::new();
            let mut prev_elems: Vec<&ElementData> = Vec::new();
            let mut children = Vec::with_capacity(node.children.len());
            for &child in &node.children {
                if let NodeType::Element(ref ce) = dom.get(child).node_type {
                    if is_synthetic_pseudo(ce) {
                        // 구조적 문맥에 포함하지 않고 스타일만 (기본 형제 문맥)
                        children.push(style_node(
                            dom, child, index, ancestors, Some(&values),
                            &SiblingCtx::default(), vp, pseudo, keyframes, child_root_fs,
                        ));
                        continue;
                    }
                    let idx = prev_elems.len() + 1;
                    let tcount = type_seen.entry(ce.tag_name.as_str()).or_insert(0);
                    *tcount += 1;
                    let type_index = *tcount;
                    let type_total = *type_totals.get(ce.tag_name.as_str()).unwrap_or(&1);
                    let has_children = !dom.get(child).children.is_empty();
                    let csib = SiblingCtx {
                        index: idx,
                        total,
                        type_index,
                        type_total,
                        prev: &prev_elems,
                        has_children,
                    };
                    children.push(style_node(dom, child, index, ancestors, Some(&values), &csib, vp, pseudo, keyframes, child_root_fs));
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
                        pseudo,
                        keyframes,
                        child_root_fs,
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
                    style_node(dom, child, index, ancestors, parent, &SiblingCtx::default(), vp, pseudo, keyframes, root_fs)
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

    fn find_synthetic<'a>(n: &'a StyledNode<'a>) -> Option<&'a StyledNode<'a>> {
        if let NodeType::Element(e) = &n.node.node_type {
            if is_synthetic_pseudo(e) {
                return Some(n);
            }
        }
        n.children.iter().find_map(find_synthetic)
    }

    #[test]
    fn content_open_close_quote() {
        let mut dom = crate::html::parse_dom("<q>hi</q>".to_string());
        let ss = crate::css::parse(
            "q::before { content: open-quote; } q::after { content: close-quote; }".to_string(),
        );
        let map = generate_pseudo_elements(&mut dom, &ss);
        let texts: Vec<String> = map
            .keys()
            .filter_map(|&nid| {
                dom.get(nid).children.first().and_then(|&c| match &dom.get(c).node_type {
                    NodeType::Text(t) => Some(t.clone()),
                    _ => None,
                })
            })
            .collect();
        assert!(texts.contains(&"\u{201C}".to_string()), "여는 인용부호");
        assert!(texts.contains(&"\u{201D}".to_string()), "닫는 인용부호");
    }

    #[test]
    fn css_counters_number_content() {
        // section 마다 counter-increment, ::before content: counter(sec) → "1","2","3"
        let mut dom = crate::html::parse_dom(
            "<div><section>a</section><section>b</section><section>c</section></div>".to_string(),
        );
        let ss = crate::css::parse(
            "div { counter-reset: sec; } \
             section { counter-increment: sec; } \
             section::before { content: counter(sec); }"
                .to_string(),
        );
        let map = generate_pseudo_elements(&mut dom, &ss);
        // 3개의 ::before 생성, 텍스트가 1/2/3
        let mut texts: Vec<String> = map
            .keys()
            .filter_map(|&nid| {
                dom.get(nid).children.first().and_then(|&c| match &dom.get(c).node_type {
                    NodeType::Text(t) => Some(t.clone()),
                    _ => None,
                })
            })
            .collect();
        texts.sort();
        assert_eq!(texts, vec!["1", "2", "3"], "카운터 번호: {:?}", texts);
    }

    #[test]
    fn before_pseudo_generates_content_box() {
        let mut dom =
            crate::html::parse_dom("<div class=\"a\"><span>x</span></div>".to_string());
        let ss = crate::css::parse(
            ".a::before { content: \"\\2022\"; color: #ff0000; }".to_string(),
        );
        let map = generate_pseudo_elements(&mut dom, &ss);
        assert_eq!(map.len(), 1, "::before 노드 1개 생성");
        let styled = style_tree_full(&dom, &ss, Viewport::default(), &map);
        let synth = find_synthetic(&styled).expect("합성 ::before 노드");
        // 색은 규칙에서, 텍스트 자식은 디코드된 content
        assert_eq!(synth.value("color"), Some(Value::Color(crate::css::Color { r: 255, g: 0, b: 0, a: 255 })));
        match &synth.children[0].node.node_type {
            NodeType::Text(t) => assert_eq!(t, "\u{2022}"),
            other => panic!("텍스트 자식 기대, {:?}", other),
        }
        // ::before 는 소유 div 의 첫 자식 (span 앞)
        let div = &styled;
        assert!(matches!(&div.children[0].node.node_type, NodeType::Element(e) if is_synthetic_pseudo(e)));
    }

    #[test]
    fn animation_applies_final_keyframe() {
        // 진입 애니메이션: 초기 opacity:0 이지만 @keyframes 최종(to)에서 1 → 정적 렌더는 1
        let root = crate::html::parse_dom("<div class=\"a\"></div>".to_string());
        let ss = crate::css::parse(
            "@keyframes fadeIn { from { opacity: 0; } to { opacity: 1; } } \
             .a { opacity: 0; animation: fadeIn 1s ease forwards; }"
                .to_string(),
        );
        let styled = style_tree(&root, &ss);
        fn find_tag<'a>(n: &'a StyledNode<'a>, tag: &str) -> Option<&'a StyledNode<'a>> {
            if matches!(&n.node.node_type, NodeType::Element(e) if e.tag_name == tag) {
                return Some(n);
            }
            n.children.iter().find_map(|c| find_tag(c, tag))
        }
        let div = find_tag(&styled, "div").unwrap();
        // 최종 opacity 1 이 적용됨 (초기 0 을 덮음)
        assert_eq!(div.value("opacity"), Some(Value::Length(1.0, Unit::Px)));
    }

    #[test]
    fn details_collapses_content() {
        fn find_tag<'a>(n: &'a StyledNode<'a>, tag: &str) -> Option<&'a StyledNode<'a>> {
            if matches!(&n.node.node_type, NodeType::Element(e) if e.tag_name == tag) {
                return Some(n);
            }
            n.children.iter().find_map(|c| find_tag(c, tag))
        }
        // 닫힌 details: summary 는 보이고 p 는 display:none
        let closed = crate::html::parse_dom(
            "<details><summary>제목</summary><p>내용</p></details>".to_string(),
        );
        let ss = crate::css::user_agent_stylesheet();
        let sc = style_tree(&closed, &ss);
        assert_eq!(find_tag(&sc, "p").unwrap().value("display"), Some(Value::Keyword("none".to_string())), "닫힌 details 내용 숨김");
        assert_ne!(find_tag(&sc, "summary").unwrap().value("display"), Some(Value::Keyword("none".to_string())), "summary 는 보임");
        // 열린 details: p 도 보임
        let open = crate::html::parse_dom(
            "<details open><summary>제목</summary><p>내용</p></details>".to_string(),
        );
        let so = style_tree(&open, &ss);
        assert_ne!(find_tag(&so, "p").unwrap().value("display"), Some(Value::Keyword("none".to_string())), "열린 details 내용 표시");
    }

    #[test]
    fn is_where_selectors() {
        let root = crate::html::parse_dom(
            "<div><h2 class=\"t\">a</h2><p>b</p><span>c</span></div>".to_string(),
        );
        // :is(h2, p) → h2 와 p 만 매칭, span 은 아님
        let ss = crate::css::parse(
            ":is(h2, p) { color: #ff0000; } :where(span) { width: 3px; } \
             :not(h2, span) { height: 5px; }"
                .to_string(),
        );
        let styled = style_tree(&root, &ss);
        fn find_tag<'a>(n: &'a StyledNode<'a>, tag: &str) -> Option<&'a StyledNode<'a>> {
            if matches!(&n.node.node_type, NodeType::Element(e) if e.tag_name == tag) {
                return Some(n);
            }
            n.children.iter().find_map(|c| find_tag(c, tag))
        }
        let red = Value::Color(crate::css::Color { r: 255, g: 0, b: 0, a: 255 });
        assert_eq!(find_tag(&styled, "h2").unwrap().value("color"), Some(red.clone()), ":is h2");
        assert_eq!(find_tag(&styled, "p").unwrap().value("color"), Some(red), ":is p");
        assert_eq!(find_tag(&styled, "span").unwrap().value("color"), None, ":is 는 span 제외");
        // :where(span) → span width
        assert_eq!(find_tag(&styled, "span").unwrap().value("width"), Some(Value::Length(3.0, Unit::Px)));
        // :not(h2, span) → p 만 height (h2/span 제외)
        assert_eq!(find_tag(&styled, "p").unwrap().value("height"), Some(Value::Length(5.0, Unit::Px)));
        assert_eq!(find_tag(&styled, "h2").unwrap().value("height"), None, ":not 이 h2 제외");
    }

    #[test]
    fn nth_of_type_and_last_selectors() {
        // 혼합 타입: h2, p, p, span. nth-of-type/last-child/of-type 정확 판별
        let root = crate::html::parse_dom(
            "<div><h2>a</h2><p>b</p><p>c</p><span>d</span></div>".to_string(),
        );
        let ss = crate::css::parse(
            "p:first-of-type { color: #ff0000; } \
             p:last-of-type { color: #00ff00; } \
             :last-child { width: 7px; } \
             p:nth-last-of-type(1) { height: 3px; }"
                .to_string(),
        );
        let styled = style_tree(&root, &ss);
        fn find_tag<'a>(n: &'a StyledNode<'a>, tag: &str) -> Option<&'a StyledNode<'a>> {
            if matches!(&n.node.node_type, NodeType::Element(e) if e.tag_name == tag) {
                return Some(n);
            }
            n.children.iter().find_map(|c| find_tag(c, tag))
        }
        let div = find_tag(&styled, "div").unwrap();
        let kids = &div.children;
        // kids: h2, p(b), p(c), span
        let red = Value::Color(crate::css::Color { r: 255, g: 0, b: 0, a: 255 });
        let green = Value::Color(crate::css::Color { r: 0, g: 255, b: 0, a: 255 });
        assert_eq!(kids[1].value("color"), Some(red), "첫 p 가 first-of-type");
        assert_eq!(kids[2].value("color"), Some(green), "둘째 p 가 last-of-type");
        assert_eq!(kids[2].value("height"), Some(Value::Length(3.0, Unit::Px)), "p 중 마지막 = nth-last-of-type(1)");
        // last-child 는 span (전체 형제 중 마지막)
        assert_eq!(kids[3].value("width"), Some(Value::Length(7.0, Unit::Px)), "span 이 last-child");
        assert_ne!(kids[1].value("width"), Some(Value::Length(7.0, Unit::Px)), "첫 p 는 last-child 아님");
    }

    #[test]
    fn current_color_resolves_to_element_color() {
        let root = crate::html::parse_dom("<div></div>".to_string());
        let ss = crate::css::parse(
            "div { color: #ff0000; border-top-color: currentColor; background-color: currentcolor; }"
                .to_string(),
        );
        let styled = style_tree(&root, &ss);
        let red = Value::Color(crate::css::Color { r: 255, g: 0, b: 0, a: 255 });
        assert_eq!(styled.value("border-top-color"), Some(red.clone()), "currentColor → color");
        assert_eq!(styled.value("background-color"), Some(red), "대소문자 무시");
    }

    #[test]
    fn form_state_pseudo_classes() {
        let root = crate::html::parse_dom(
            "<form><input required checked><input disabled></form>".to_string(),
        );
        let ss = crate::css::parse(
            "input:checked { color: #ff0000; } input:disabled { color: #00ff00; } \
             input:required { width: 5px; }"
                .to_string(),
        );
        let styled = style_tree(&root, &ss);
        fn collect_inputs<'a>(n: &'a StyledNode<'a>, out: &mut Vec<&'a StyledNode<'a>>) {
            if matches!(&n.node.node_type, NodeType::Element(e) if e.tag_name == "input") {
                out.push(n);
            }
            for c in &n.children {
                collect_inputs(c, out);
            }
        }
        let mut inputs = Vec::new();
        collect_inputs(&styled, &mut inputs);
        assert_eq!(inputs.len(), 2, "input 2개");
        let first = inputs[0]; // required checked
        let second = inputs[1]; // disabled
        assert_eq!(first.value("color"), Some(Value::Color(crate::css::Color { r: 255, g: 0, b: 0, a: 255 })), ":checked");
        assert_eq!(first.value("width"), Some(Value::Length(5.0, Unit::Px)), ":required");
        assert_eq!(second.value("color"), Some(Value::Color(crate::css::Color { r: 0, g: 255, b: 0, a: 255 })), ":disabled");
        // 첫 입력은 disabled 아님 → :disabled 안 걸림
        assert_ne!(first.value("color"), Some(Value::Color(crate::css::Color { r: 0, g: 255, b: 0, a: 255 })));
    }

    #[test]
    fn no_pseudo_without_content() {
        let mut dom = crate::html::parse_dom("<div class=\"a\"></div>".to_string());
        // content 없는 ::before 규칙 → 생성 안 함
        let ss = crate::css::parse(".a::before { color: #ff0000; }".to_string());
        let map = generate_pseudo_elements(&mut dom, &ss);
        assert_eq!(map.len(), 0, "content 없으면 생성 안 함");
    }

    #[test]
    fn higher_specificity_wins() {
        let root = crate::html::parse_dom("<div id=\"a\" class=\"b\"></div>".to_string());
        let ss = crate::css::parse(".b { width: 10px; } #a { width: 99px; }".to_string());
        let styled = style_tree(&root, &ss);
        assert_eq!(styled.value("width"), Some(Value::Length(99.0, Unit::Px)));
    }

    #[test]
    fn important_beats_higher_specificity() {
        // 낮은 특이도 !important 가 높은 특이도 일반 선언을 이긴다 (캐스케이드 origin 우선)
        let root = crate::html::parse_dom("<div id=\"a\" class=\"b\"></div>".to_string());
        let ss = crate::css::parse(
            "#a { width: 99px; } .b { width: 10px !important; }".to_string(),
        );
        let styled = style_tree(&root, &ss);
        assert_eq!(styled.value("width"), Some(Value::Length(10.0, Unit::Px)));
    }

    #[test]
    fn important_beats_inline_style() {
        // important 저작자 선언이 일반 인라인 스타일을 이긴다
        let root = crate::html::parse_dom(
            "<div class=\"b\" style=\"width: 5px\"></div>".to_string(),
        );
        let ss = crate::css::parse(".b { width: 10px !important; }".to_string());
        let styled = style_tree(&root, &ss);
        assert_eq!(styled.value("width"), Some(Value::Length(10.0, Unit::Px)));
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
        // rem 은 루트 요소(여기선 트리 루트 div, font-size 20px) 기준 — 스펙대로
        assert_eq!(fs(2), Some(Value::Length(40.0, Unit::Px)), "2rem × 20px 루트");
        assert_eq!(fs(3), Some(Value::Length(20.0, Unit::Px)), "미지정 → 상속");
    }

    #[test]
    fn rem_follows_root_font_size() {
        // 루트 font-size:62.5% → 1rem=10px (흔한 트릭). 자손의 rem 은 상수 16 이 아니라
        // 루트 계산 font-size 기준. (이전엔 rem 이 항상 16 하드코딩이라 요행 통과)
        let root = crate::html::parse_dom("<div><p class=\"x\">a</p></div>".to_string());
        let ss = crate::css::parse(
            "div { font-size: 62.5%; } .x { width: 1.6rem; }".to_string(),
        );
        let styled = style_tree(&root, &ss);
        assert_eq!(styled.value("font-size"), Some(Value::Length(10.0, Unit::Px)), "루트 62.5%×16=10");
        assert_eq!(styled.children[0].value("width"), Some(Value::Length(16.0, Unit::Px)), "1.6rem×10");
    }

    #[test]
    fn unknown_display_falls_back_to_block() {
        // 미지원 display 값(table-cell 등)은 블록으로 폴백 — 자식 보존
        let root = crate::html::parse_dom("<div>content</div>".to_string());
        let ss = crate::css::parse("div { display: table-cell; }".to_string());
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
    fn css_wide_inherit_and_unset() {
        // background-color 는 비상속 속성 → inherit 키워드로 부모값을 가져와야
        let root = crate::html::parse_dom(
            "<div class=\"p\"><span class=\"c\">x</span></div>".to_string(),
        );
        let ss = crate::css::parse(
            ".p { background-color: #ff0000; } .c { background-color: inherit; }".to_string(),
        );
        let styled = style_tree(&root, &ss);
        assert!(
            styled.children[0].value("background-color").is_some(),
            "inherit → 부모 background-color 복사"
        );
        // 대조: inherit 없으면 비상속이라 자식엔 없음
        let ss2 = crate::css::parse(".p { background-color: #ff0000; }".to_string());
        assert!(
            style_tree(&root, &ss2).children[0].value("background-color").is_none(),
            "비상속 속성은 기본적으로 자식에 안 옴"
        );
        // color(상속 속성)에 unset → 부모값 상속 (초기값 아님 근사)
        let ss3 = crate::css::parse(".p { color: #00ff00; } .c { color: unset; }".to_string());
        assert!(
            style_tree(&root, &ss3).children[0].value("color").is_some(),
            "unset 상속 속성 → 부모 color 상속"
        );
    }

    #[test]
    fn link_pseudo_matches_href_anchors() {
        // a:link 는 href 있는 링크에 적용 (정적 렌더에선 모든 링크가 unvisited)
        let ss = crate::css::parse("a:link { color: #ff0000; }".to_string());
        let linked = crate::html::parse_dom("<a href=\"/x\">go</a>".to_string());
        assert!(style_tree(&linked, &ss).value("color").is_some(), "href 링크 → :link 매칭");
        // href 없는 <a> 는 :link 아님
        let bare = crate::html::parse_dom("<a>text</a>".to_string());
        assert!(style_tree(&bare, &ss).value("color").is_none(), "href 없으면 :link 아님");
    }

    #[test]
    fn presentational_hints_map_and_lose_to_author() {
        let ss = crate::css::parse(String::new());
        // <div align="center"> → text-align:center
        let root = crate::html::parse_dom("<div align=\"center\">x</div>".to_string());
        assert!(
            matches!(style_tree(&root, &ss).value("text-align"),
                Some(crate::css::Value::Keyword(k)) if k == "center"),
            "align 속성 → text-align"
        );
        // <font color="red"> → color 지정됨
        let rf = crate::html::parse_dom("<font color=\"red\">x</font>".to_string());
        assert!(style_tree(&rf, &ss).value("color").is_some(), "font color 매핑");
        // <table bgcolor="00ff00" width="300"> → background-color + width:300px
        let rt = crate::html::parse_dom("<table bgcolor=\"00ff00\" width=\"300\"></table>".to_string());
        let st = style_tree(&rt, &ss);
        assert!(st.value("background-color").is_some(), "bgcolor 매핑");
        assert!(
            matches!(st.value("width"), Some(crate::css::Value::Length(w, _)) if (w - 300.0).abs() < 0.1),
            "width=300 → 300px"
        );
        // 저작자 규칙이 표현 속성을 이긴다 (기본 레이어)
        let ss2 = crate::css::parse("div { text-align: right; }".to_string());
        assert!(
            matches!(style_tree(&root, &ss2).value("text-align"),
                Some(crate::css::Value::Keyword(k)) if k == "right"),
            "author 규칙이 힌트를 덮는다"
        );
    }

    #[test]
    fn font_size_clamp_resolves() {
        let root = crate::html::parse_dom("<div></div>".to_string());
        // clamp(1rem, 2vw, 2rem): 1rem=16, 2vw(vp 1000)=20, 2rem=32 → 20
        let ss = crate::css::parse("div { font-size: clamp(1rem, 2vw, 2rem); }".to_string());
        let styled = style_tree_vp(&root, &ss, Viewport { w: 1000.0, h: 600.0 });
        assert_eq!(styled.value("font-size"), Some(Value::Length(20.0, Unit::Px)));
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
