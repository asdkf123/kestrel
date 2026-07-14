use std::collections::HashSet;

// 속성은 **순서 있는 목록**이다 (DOM §4.9: element's attribute list). HashMap 이면
// outerHTML/getAttributeNames 가 매번 다른 순서를 내놓는다 — 직렬화 결과가 원본과
// 달라지고(스냅샷/캐시 비교가 깨진다) 재현도 안 된다. 개수가 적으니 선형 탐색이면 충분하다.
#[derive(Debug, PartialEq, Clone, Default)]
pub struct AttrMap {
    items: Vec<(String, String)>,
}

impl AttrMap {
    pub fn new() -> AttrMap {
        AttrMap::default()
    }
    pub fn get(&self, k: &str) -> Option<&String> {
        self.items.iter().find(|(n, _)| n == k).map(|(_, v)| v)
    }
    // 기존 키면 **자리를 유지한 채** 값만 바꾼다 (표준: 순서는 처음 추가된 순서)
    pub fn insert(&mut self, k: String, v: String) -> Option<String> {
        if let Some(slot) = self.items.iter_mut().find(|(n, _)| *n == k) {
            return Some(std::mem::replace(&mut slot.1, v));
        }
        self.items.push((k, v));
        None
    }
    pub fn remove(&mut self, k: &str) -> Option<String> {
        let i = self.items.iter().position(|(n, _)| n == k)?;
        Some(self.items.remove(i).1)
    }
    // 없을 때만 넣는다 (HTML 파서: 중복 속성은 첫 값이 이긴다)
    pub fn insert_if_absent(&mut self, k: String, v: String) {
        if !self.contains_key(&k) {
            self.items.push((k, v));
        }
    }
    pub fn contains_key(&self, k: &str) -> bool {
        self.items.iter().any(|(n, _)| n == k)
    }
    pub fn iter(&self) -> impl Iterator<Item = (&String, &String)> {
        self.items.iter().map(|(k, v)| (k, v))
    }
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.items.iter().map(|(k, _)| k)
    }
    pub fn len(&self) -> usize {
        self.items.len()
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

impl FromIterator<(String, String)> for AttrMap {
    fn from_iter<I: IntoIterator<Item = (String, String)>>(it: I) -> AttrMap {
        let mut m = AttrMap::new();
        for (k, v) in it {
            m.insert(k, v);
        }
        m
    }
}

impl IntoIterator for AttrMap {
    type Item = (String, String);
    type IntoIter = std::vec::IntoIter<(String, String)>;
    fn into_iter(self) -> Self::IntoIter {
        self.items.into_iter()
    }
}

impl<'a> IntoIterator for &'a AttrMap {
    type Item = (&'a String, &'a String);
    type IntoIter = std::vec::IntoIter<(&'a String, &'a String)>;
    fn into_iter(self) -> Self::IntoIter {
        self.items.iter().map(|(k, v)| (k, v)).collect::<Vec<_>>().into_iter()
    }
}

#[derive(Debug, PartialEq)]
pub struct Node {
    pub children: Vec<Node>,
    pub node_type: NodeType,
}

#[derive(Debug, PartialEq, Clone)]
pub enum NodeType {
    Text(String),
    Element(ElementData),
    // 코멘트도 DOM 노드다 (§4.9 Comment). 예전엔 파서가 통째로 버려서 childNodes 가
    // 표준과 달랐고, document.createComment 도 없었다. 프레임워크가 코멘트를 앵커로
    // 쓴다 (Vue 의 v-if 자리표시자, React SSR 의 <!--$--> Suspense 경계).
    Comment(String),
}

#[derive(Debug, PartialEq, Clone)]
pub struct ElementData {
    pub tag_name: String,
    pub attributes: AttrMap,
    // 요소의 네임스페이스 (DOM §4.9). None = HTML 네임스페이스.
    // 예전엔 아예 없어서 createElementNS 가 네임스페이스를 **버렸다** —
    // SVG 의 <linearGradient> 를 만들어도 소문자 html 요소가 됐다.
    pub namespace: Option<String>,
}

pub const NS_HTML: &str = "http://www.w3.org/1999/xhtml";
pub const NS_SVG: &str = "http://www.w3.org/2000/svg";
pub const NS_MATHML: &str = "http://www.w3.org/1998/Math/MathML";

impl ElementData {
    // HTML 네임스페이스 요소 (기본). 테스트에서 노드를 손으로 만들 때 쓴다 —
    // 필드가 늘어도 테스트가 안 깨지도록.
    #[cfg(test)]
    pub fn html(tag: &str, attributes: AttrMap) -> ElementData {
        ElementData { tag_name: tag.to_string(), attributes, namespace: None }
    }

    pub fn ns(&self) -> &str {
        self.namespace.as_deref().unwrap_or(NS_HTML)
    }
    // 로컬 이름 (접두사를 뗀 부분)
    pub fn local_name(&self) -> &str {
        self.tag_name.rsplit(':').next().unwrap_or(&self.tag_name)
    }
    pub fn prefix(&self) -> Option<&str> {
        self.tag_name.split_once(':').map(|(p, _)| p)
    }
}

// text/elem: 테스트에서만 쓰이는 노드 생성자 (비-테스트 빌드에선 미사용).
#[allow(dead_code)]
pub fn text(data: String) -> Node {
    Node { children: Vec::new(), node_type: NodeType::Text(data) }
}

#[allow(dead_code)]
pub fn elem(name: String, attrs: AttrMap, children: Vec<Node>) -> Node {
    Node {
        children,
        node_type: NodeType::Element(ElementData { tag_name: name, attributes: attrs, namespace: None }),
    }
}

impl ElementData {
    pub fn id(&self) -> Option<&String> {
        self.attributes.get("id")
    }

    // <img> 가 실제로 가리키는 소스.
    //
    // 예전엔 srcset 의 **첫 후보**를 그냥 썼다. 첫 후보는 보통 가장 작은 이미지라
    // (예: 200w) 화면에서 흐리게 확대됐다. 표준은 sizes 와 디스크립터로 고른다
    // (HTML §4.8.4.4). 이제 그 알고리즘을 따른다: 뷰포트 폭 vw 로 sizes 를 풀어
    // 목표 폭을 구하고, 그 폭 이상인 후보 중 가장 작은 것을 쓴다.
    pub fn img_source(&self) -> Option<String> {
        self.img_source_vw(1000.0)
    }

    pub fn img_source_vw(&self, vw: f32) -> Option<String> {
        if let Some(set) = self.attributes.get("srcset") {
            let cands = parse_srcset(set);
            if !cands.is_empty() {
                let target = self
                    .attributes
                    .get("sizes")
                    .map(|s| resolve_sizes(s, vw))
                    .unwrap_or(vw); // sizes 가 없으면 100vw (표준 기본값)
                if let Some(u) = pick_candidate(&cands, target) {
                    return Some(u);
                }
            }
        }
        if let Some(src) = self.attributes.get("src") {
            if !src.trim().is_empty() {
                return Some(src.clone());
            }
        }
        None
    }

    pub fn classes(&self) -> HashSet<&str> {
        match self.attributes.get("class") {
            Some(classlist) => split_ascii_ws(classlist).collect(),
            None => HashSet::new(),
        }
    }
}

// DOM 표준의 "ASCII whitespace" 로 토큰화 (§Infra: TAB, LF, FF, CR, SPACE).
// 유니코드 공백(NBSP, EN QUAD, OGHAM SPACE MARK 등)은 **구분자가 아니다** —
// 클래스 이름의 일부다. 예전엔 class 를 ' ' 하나로만 자르거나(탭이 든 클래스가
// 한 덩어리가 됨) Rust 의 split_whitespace(유니코드 공백까지 자름)를 썼다.
// 둘 다 조용히 틀린 매칭을 만든다.
pub fn split_ascii_ws(s: &str) -> impl Iterator<Item = &str> {
    s.split(|c| matches!(c, '\t' | '\n' | '\x0C' | '\r' | ' ')).filter(|t| !t.is_empty())
}

// ── 아레나 DOM ──────────────────────────────────────────────────────
// NodeId 는 구조 변형(삽입/삭제)과 무관하게 안정 — JS 핸들/이벤트 레지스트리 키.
// detach 된 노드는 아레나에 남는다 (재사용 없음, 페이지 수명 동안 감수).

pub type NodeId = usize;

#[derive(Debug)]
pub struct NodeData {
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub node_type: NodeType,
}

// MutationObserver 로 배달할 변형 기록. 표준에서도 "mutation record 를 큐에 넣는" 일은
// DOM 연산의 일부다 — 그래서 JS 층이 아니라 여기(아레나)에서 기록한다.
#[derive(Debug, Clone)]
pub struct DomMut {
    pub target: NodeId,
    pub kind: &'static str, // childList / attributes / characterData
    pub attr: Option<String>,
    // 변경 전 값 (attributes/characterData). 예전엔 항상 null 이었다 —
    // attributeOldValue 를 요청한 옵저버가 조용히 아무것도 못 받았다.
    pub old_value: Option<String>,
    pub added: Vec<NodeId>,
    pub removed: Vec<NodeId>,
}

// srcset 후보: (URL, 디스크립터). w=폭(px), x=배율.
#[derive(Debug, PartialEq, Clone)]
pub enum Descriptor {
    Width(f32),
    Density(f32),
}

// srcset 파싱 (HTML §4.8.4.4.1). URL 토큰은 **공백까지** 읽는다 — 그래서 콤마가 든
// data: URI 도 안전하다 (예전 구현은 콤마로 잘라서 data URI 를 조각냈다).
pub fn parse_srcset(s: &str) -> Vec<(String, Descriptor)> {
    let b: Vec<char> = s.chars().collect();
    let mut i = 0usize;
    let mut out = Vec::new();
    while i < b.len() {
        // 공백/콤마 건너뛰기
        while i < b.len() && (b[i].is_whitespace() || b[i] == ',') {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        // URL: 공백이 나올 때까지
        let start = i;
        while i < b.len() && !b[i].is_whitespace() {
            i += 1;
        }
        let raw: String = b[start..i].iter().collect();
        let trimmed = raw.trim_end_matches(',');
        let had_comma = trimmed.len() != raw.len();
        if trimmed.is_empty() {
            continue;
        }
        let mut desc = Descriptor::Density(1.0);
        if !had_comma {
            // 디스크립터: 콤마 전까지
            while i < b.len() && b[i].is_whitespace() {
                i += 1;
            }
            let dstart = i;
            while i < b.len() && b[i] != ',' {
                i += 1;
            }
            let dtext: String = b[dstart..i].iter().collect();
            for tok in dtext.split_whitespace() {
                if let Some(n) = tok.strip_suffix('w').and_then(|n| n.parse::<f32>().ok()) {
                    desc = Descriptor::Width(n);
                } else if let Some(n) = tok.strip_suffix('x').and_then(|n| n.parse::<f32>().ok()) {
                    desc = Descriptor::Density(n);
                }
            }
        }
        out.push((trimmed.to_string(), desc));
    }
    out
}

// sizes 를 풀어 목표 폭(px)을 구한다: "(max-width: 600px) 100vw, 50vw"
pub fn resolve_sizes(sizes: &str, vw: f32) -> f32 {
    for part in sizes.split(',') {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        // 마지막 토큰이 길이, 앞은 미디어 조건
        let (cond, len) = match p.rfind(char::is_whitespace) {
            Some(k) if p.starts_with('(') => (p[..k].trim(), p[k..].trim()),
            _ => ("", p),
        };
        if !cond.is_empty() && !crate::css::media_matches(cond, vw) {
            continue;
        }
        if let Some(px) = parse_length_px(len, vw) {
            return px;
        }
    }
    vw
}

fn parse_length_px(s: &str, vw: f32) -> Option<f32> {
    let t = s.trim();
    if let Some(n) = t.strip_suffix("px").and_then(|n| n.trim().parse::<f32>().ok()) {
        return Some(n);
    }
    if let Some(n) = t.strip_suffix("vw").and_then(|n| n.trim().parse::<f32>().ok()) {
        return Some(n / 100.0 * vw);
    }
    if let Some(n) = t.strip_suffix("em").and_then(|n| n.trim().parse::<f32>().ok()) {
        return Some(n * 16.0);
    }
    if let Some(n) = t.strip_suffix("rem").and_then(|n| n.trim().parse::<f32>().ok()) {
        return Some(n * 16.0);
    }
    t.parse::<f32>().ok()
}

// 목표 폭에 맞는 후보 고르기 (DPR 1 가정): 목표 이상인 것 중 가장 작은 것,
// 없으면 가장 큰 것. x 디스크립터만 있으면 1x 에 가장 가까운 것.
pub fn pick_candidate(cands: &[(String, Descriptor)], target: f32) -> Option<String> {
    let has_w = cands.iter().any(|(_, d)| matches!(d, Descriptor::Width(_)));
    if has_w {
        let mut best: Option<(f32, &String)> = None;
        let mut largest: Option<(f32, &String)> = None;
        for (u, d) in cands {
            let Descriptor::Width(w) = d else { continue };
            if largest.map(|(lw, _)| *w > lw).unwrap_or(true) {
                largest = Some((*w, u));
            }
            if *w >= target && best.map(|(bw, _)| *w < bw).unwrap_or(true) {
                best = Some((*w, u));
            }
        }
        return best.or(largest).map(|(_, u)| u.clone());
    }
    // 밀도 디스크립터: 1x 이상 중 가장 작은 것
    let mut best: Option<(f32, &String)> = None;
    for (u, d) in cands {
        let Descriptor::Density(x) = d else { continue };
        let better = match best {
            None => true,
            Some((bx, _)) => {
                // 1 이상인 것 우선, 그중 작은 것
                if *x >= 1.0 && bx < 1.0 {
                    true
                } else if *x >= 1.0 && bx >= 1.0 {
                    *x < bx
                } else {
                    *x > bx && bx < 1.0
                }
            }
        };
        if better {
            best = Some((*x, u));
        }
    }
    best.map(|(_, u)| u.clone())
}

// 닫는 태그가 없는 요소 (HTML 표준의 void elements)
fn is_void_element(tag: &str) -> bool {
    matches!(
        tag,
        "area" | "base" | "br" | "col" | "embed" | "hr" | "img" | "input" | "link" | "meta"
            | "param" | "source" | "track" | "wbr"
    )
}

fn escape_text(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn escape_attr(s: &str) -> String {
    s.replace('&', "&amp;").replace('"', "&quot;")
}

#[derive(Debug)]
pub struct Dom {
    nodes: Vec<NodeData>,
    pub root: NodeId,
    // 아직 배달하지 않은 변형 기록. 파싱(from_tree/insert_tree)은 기록하지 않는다 —
    // 그 시점엔 옵저버가 존재할 수 없다.
    pub records: Vec<DomMut>,
    // 변형 카운터. 스타일/레이아웃 캐시가 자신이 본 버전과 비교해 재계산 여부를 정한다.
    // (JS 가 측정 API 를 읽을 때 강제 레이아웃을 흘려야 하는지 판정 — CSSOM View)
    version: u64,
}

impl Dom {
    // 아레나 노드 수 (테스트에서 전체 순회에 쓴다)
    #[cfg(test)]
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    fn touch(&mut self) {
        self.version += 1;
    }

    pub fn from_tree(tree: Node) -> Dom {
        let mut dom = Dom { nodes: Vec::new(), root: 0, version: 0, records: Vec::new() };
        let root = dom.insert_tree(tree, None);
        dom.root = root;
        dom
    }

    // 트리(파서 출력)를 아레나로 흡수. 새 서브트리의 루트 id 반환.
    pub fn insert_tree(&mut self, tree: Node, parent: Option<NodeId>) -> NodeId {
        self.touch();
        let id = self.nodes.len();
        self.nodes.push(NodeData { parent, children: Vec::new(), node_type: tree.node_type });
        for child in tree.children {
            let cid = self.insert_tree(child, Some(id));
            self.nodes[id].children.push(cid);
        }
        id
    }

    // 아레나에 든 노드 수 (테스트/진단용)

    pub fn get(&self, id: NodeId) -> &NodeData {
        &self.nodes[id]
    }

    // 노드를 가변으로 빌려주는 유일한 통로 — 여기서 버전을 올리면 속성/텍스트 변경이
    // 전부 잡힌다(호출부마다 표시하는 방식은 누락이 생긴다).
    pub fn get_mut(&mut self, id: NodeId) -> &mut NodeData {
        self.version += 1;
        &mut self.nodes[id]
    }

    pub fn create_element(&mut self, tag: &str) -> NodeId {
        self.touch();
        let id = self.nodes.len();
        self.nodes.push(NodeData {
            parent: None,
            children: Vec::new(),
            node_type: NodeType::Element(ElementData {
                tag_name: tag.to_ascii_lowercase(),
                attributes: AttrMap::new(),
                namespace: None, // HTML 네임스페이스
            }),
        });
        id
    }

    // createElementNS: 네임스페이스를 보존하고 **대소문자도 그대로** 둔다.
    // SVG 의 linearGradient / clipPath 는 대소문자가 의미를 갖는다 — 소문자로
    // 내리면 다른 요소가 된다.
    pub fn create_element_ns(&mut self, ns: Option<&str>, qname: &str) -> NodeId {
        self.touch();
        let id = self.nodes.len();
        let is_html = ns.is_none() || ns == Some(NS_HTML);
        self.nodes.push(NodeData {
            parent: None,
            children: Vec::new(),
            node_type: NodeType::Element(ElementData {
                // HTML 네임스페이스면 소문자로 정규화 (표준), 그 외는 원문 유지
                tag_name: if is_html { qname.to_ascii_lowercase() } else { qname.to_string() },
                attributes: AttrMap::new(),
                namespace: ns.filter(|n| *n != NS_HTML).map(|n| n.to_string()),
            }),
        });
        id
    }

    // node.cloneNode(deep): 노드(및 deep 이면 서브트리)를 복사해 분리된 새 노드로.
    // 새 루트의 NodeId 를 반환한다 (부모 없음).
    pub fn clone_node(&mut self, id: NodeId, deep: bool) -> NodeId {
        self.touch();
        let node_type = self.nodes[id].node_type.clone();
        let new_id = self.nodes.len();
        self.nodes.push(NodeData { parent: None, children: Vec::new(), node_type });
        if deep {
            let children = self.nodes[id].children.clone();
            for c in children {
                let cc = self.clone_node(c, true);
                self.nodes[cc].parent = Some(new_id);
                self.nodes[new_id].children.push(cc);
            }
        }
        new_id
    }

    pub fn create_comment(&mut self, data: String) -> NodeId {
        self.touch();
        let id = self.nodes.len();
        self.nodes.push(NodeData {
            parent: None,
            children: Vec::new(),
            node_type: NodeType::Comment(data),
        });
        id
    }

    pub fn create_text(&mut self, text: String) -> NodeId {
        self.touch();
        let id = self.nodes.len();
        self.nodes.push(NodeData {
            parent: None,
            children: Vec::new(),
            node_type: NodeType::Text(text),
        });
        id
    }

    // child 를 기존 부모에서 떼어 parent 의 마지막 자식으로. 자기 자신/순환은 무시.
    pub fn append_child(&mut self, parent: NodeId, child: NodeId) {
        self.touch();
        if parent == child || self.ancestors(parent).contains(&child) {
            return;
        }
        self.detach(child); // 기존 부모에서의 제거도 기록된다
        self.nodes[child].parent = Some(parent);
        self.nodes[parent].children.push(child);
        self.record(parent, "childList", None, vec![child], Vec::new());
    }

    pub fn detach(&mut self, id: NodeId) {
        self.touch();
        if let Some(p) = self.nodes[id].parent.take() {
            self.nodes[p].children.retain(|&c| c != id);
            self.record(p, "childList", None, Vec::new(), vec![id]);
        }
    }

    // parent.insertBefore(child, reference): reference 앞에 삽입.
    // reference 가 없거나 parent 의 자식이 아니면 끝에 추가(appendChild 동일).
    pub fn insert_before(&mut self, parent: NodeId, child: NodeId, reference: Option<NodeId>) {
        self.touch();
        if parent == child || self.ancestors(parent).contains(&child) {
            return;
        }
        self.detach(child);
        self.nodes[child].parent = Some(parent);
        let pos = reference.and_then(|r| self.nodes[parent].children.iter().position(|&c| c == r));
        match pos {
            Some(idx) => self.nodes[parent].children.insert(idx, child),
            None => self.nodes[parent].children.push(child),
        }
        self.record(parent, "childList", None, vec![child], Vec::new());
    }

    // 부모 → 루트 순 조상 체인
    pub fn ancestors(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut cur = self.nodes[id].parent;
        while let Some(p) = cur {
            out.push(p);
            cur = self.nodes[p].parent;
        }
        out
    }

    // 문서 순서(DFS)로 id 속성 검색
    pub fn find_by_attr_id(&self, want: &str) -> Option<NodeId> {
        fn rec(dom: &Dom, id: NodeId, want: &str) -> Option<NodeId> {
            if let NodeType::Element(e) = &dom.get(id).node_type {
                if e.attributes.get("id").map(|s| s.as_str()) == Some(want) {
                    return Some(id);
                }
            }
            dom.get(id).children.iter().find_map(|&c| rec(dom, c, want))
        }
        rec(self, self.root, want)
    }

    // HTML 요소 → IDL 인터페이스 이름 (HTML 표준의 "element interface" 매핑).
    // 이 표는 명세가 정한 것이지 추측이 아니다. 목록에 없는 HTML 태그는
    // HTMLElement, 알 수 없는 태그는 HTMLUnknownElement (§4.13).
    pub fn element_interface(tag: &str) -> &'static str {
        match tag {
            "a" => "HTMLAnchorElement",
            "area" => "HTMLAreaElement",
            "audio" => "HTMLAudioElement",
            "base" => "HTMLBaseElement",
            "blockquote" | "q" => "HTMLQuoteElement",
            "body" => "HTMLBodyElement",
            "br" => "HTMLBRElement",
            "button" => "HTMLButtonElement",
            "canvas" => "HTMLCanvasElement",
            "caption" => "HTMLTableCaptionElement",
            "col" | "colgroup" => "HTMLTableColElement",
            "data" => "HTMLDataElement",
            "datalist" => "HTMLDataListElement",
            "del" | "ins" => "HTMLModElement",
            "details" => "HTMLDetailsElement",
            "dialog" => "HTMLDialogElement",
            "div" => "HTMLDivElement",
            "dl" => "HTMLDListElement",
            "embed" => "HTMLEmbedElement",
            "fieldset" => "HTMLFieldSetElement",
            "form" => "HTMLFormElement",
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => "HTMLHeadingElement",
            "head" => "HTMLHeadElement",
            "hr" => "HTMLHRElement",
            "html" => "HTMLHtmlElement",
            "iframe" => "HTMLIFrameElement",
            "img" => "HTMLImageElement",
            "input" => "HTMLInputElement",
            "label" => "HTMLLabelElement",
            "legend" => "HTMLLegendElement",
            "li" => "HTMLLIElement",
            "link" => "HTMLLinkElement",
            "map" => "HTMLMapElement",
            "menu" => "HTMLMenuElement",
            "meta" => "HTMLMetaElement",
            "meter" => "HTMLMeterElement",
            "object" => "HTMLObjectElement",
            "ol" => "HTMLOListElement",
            "optgroup" => "HTMLOptGroupElement",
            "option" => "HTMLOptionElement",
            "output" => "HTMLOutputElement",
            "p" => "HTMLParagraphElement",
            "picture" => "HTMLPictureElement",
            "pre" => "HTMLPreElement",
            "progress" => "HTMLProgressElement",
            "script" => "HTMLScriptElement",
            "select" => "HTMLSelectElement",
            "slot" => "HTMLSlotElement",
            "source" => "HTMLSourceElement",
            "span" => "HTMLSpanElement",
            "style" => "HTMLStyleElement",
            "table" => "HTMLTableElement",
            "tbody" | "tfoot" | "thead" => "HTMLTableSectionElement",
            "td" | "th" => "HTMLTableCellElement",
            "template" => "HTMLTemplateElement",
            "textarea" => "HTMLTextAreaElement",
            "time" => "HTMLTimeElement",
            "title" => "HTMLTitleElement",
            "tr" => "HTMLTableRowElement",
            "track" => "HTMLTrackElement",
            "ul" => "HTMLUListElement",
            "video" => "HTMLVideoElement",
            // 알려진 HTML 태그이지만 전용 인터페이스가 없는 것들
            "abbr" | "address" | "article" | "aside" | "b" | "bdi" | "bdo" | "cite" | "code"
            | "dd" | "dfn" | "dt" | "em" | "figcaption" | "figure" | "footer" | "header"
            | "hgroup" | "i" | "kbd" | "main" | "mark" | "nav" | "noscript" | "rp" | "rt"
            | "ruby" | "s" | "samp" | "search" | "section" | "small" | "strong" | "sub"
            | "summary" | "sup" | "u" | "var" | "wbr" => "HTMLElement",
            // SVG
            "svg" => "SVGSVGElement",
            "path" | "rect" | "circle" | "ellipse" | "line" | "polygon" | "polyline" | "g"
            | "defs" | "use" | "text" | "tspan" | "clipPath" | "mask" | "pattern" => {
                "SVGElement"
            }
            // 그 외 (커스텀 요소 포함): 하이픈이 있으면 커스텀 요소라 HTMLElement,
            // 아니면 알 수 없는 요소.
            t if t.contains('-') => "HTMLElement",
            _ => "HTMLUnknownElement",
        }
    }

    // 술어를 만족하는 첫 요소 (문서 순서). named access 등에 쓴다.
    pub fn find<F: Fn(&ElementData) -> bool + Copy>(&self, pred: F) -> Option<NodeId> {
        fn rec<F: Fn(&ElementData) -> bool + Copy>(
            dom: &Dom,
            id: NodeId,
            pred: F,
        ) -> Option<NodeId> {
            if let NodeType::Element(e) = &dom.get(id).node_type {
                if pred(e) {
                    return Some(id);
                }
            }
            dom.get(id).children.iter().find_map(|&c| rec(dom, c, pred))
        }
        rec(self, self.root, pred)
    }

    // HTML 직렬화 (innerHTML / outerHTML).
    // 예전엔 innerHTML 을 쓰기만 되고 읽기가 없어서, el.innerHTML 을 읽는 코드가
    // undefined 를 받고 그 자리에서 죽었다 (아주 흔한 패턴이다).
    pub fn inner_html(&self, id: NodeId) -> String {
        let mut s = String::new();
        for &c in &self.get(id).children {
            self.serialize(c, &mut s);
        }
        s
    }

    pub fn outer_html(&self, id: NodeId) -> String {
        let mut s = String::new();
        self.serialize(id, &mut s);
        s
    }

    fn serialize(&self, id: NodeId, out: &mut String) {
        match &self.get(id).node_type {
            NodeType::Text(t) => out.push_str(&escape_text(t)),
            NodeType::Comment(c) => {
                out.push_str("<!--");
                out.push_str(c);
                out.push_str("-->");
            }
            NodeType::Element(e) => {
                out.push('<');
                out.push_str(&e.tag_name);
                for (k, v) in e.attributes.iter() {
                    out.push(' ');
                    out.push_str(k);
                    out.push_str("=\"");
                    out.push_str(&escape_attr(v));
                    out.push('"');
                }
                out.push('>');
                if is_void_element(&e.tag_name) {
                    return; // void 요소는 닫는 태그가 없다
                }
                for &c in &self.get(id).children {
                    self.serialize(c, out);
                }
                out.push_str("</");
                out.push_str(&e.tag_name);
                out.push('>');
            }
        }
    }

    pub fn text_content(&self, id: NodeId) -> String {
        fn rec(dom: &Dom, id: NodeId, out: &mut String) {
            if let NodeType::Text(t) = &dom.get(id).node_type {
                out.push_str(t);
            }
            for &c in &dom.get(id).children {
                rec(dom, c, out);
            }
        }
        let mut s = String::new();
        rec(self, id, &mut s);
        s
    }

    // 자식들을 텍스트 노드 하나로 교체
    // 텍스트/코멘트 노드의 문자 데이터 설정 (요소는 무시 — 표준의 nodeValue 규칙)
    pub fn set_char_data(&mut self, id: NodeId, data: String) {
        self.touch();
        match &mut self.nodes[id].node_type {
            NodeType::Text(t) => *t = data,
            NodeType::Comment(c) => *c = data,
            NodeType::Element(_) => {}
        }
    }

    pub fn set_text_content(&mut self, id: NodeId, text: String) {
        // 텍스트 노드 자체면 characterData, 요소면 자식 교체(childList)
        if let NodeType::Text(t) = &mut self.nodes[id].node_type {
            *t = text;
            self.touch();
            self.record(id, "characterData", None, Vec::new(), Vec::new());
            return;
        }
        self.clear_children(id);
        let t = self.create_text(text);
        self.nodes[t].parent = Some(id);
        self.nodes[id].children.push(t);
        self.record(id, "childList", None, vec![t], Vec::new());
    }

    pub fn clear_children(&mut self, id: NodeId) {
        self.touch();
        let old: Vec<NodeId> = std::mem::take(&mut self.nodes[id].children);
        for &c in &old {
            self.nodes[c].parent = None; // 고아로 방치 (아레나 재사용 없음)
        }
        if !old.is_empty() {
            self.record(id, "childList", None, Vec::new(), old);
        }
    }

    // 속성 쓰기의 유일한 통로. 여기로 모아야 attributes 기록에 누락이 없다.
    pub fn set_attr(&mut self, id: NodeId, name: &str, value: String) {
        self.touch();
        let old = if let NodeType::Element(e) = &mut self.nodes[id].node_type {
            let old = e.attributes.get(name).cloned();
            e.attributes.insert(name.to_string(), value);
            old
        } else {
            return;
        };
        self.record_attr(id, name.to_string(), old);
    }

    pub fn remove_attr(&mut self, id: NodeId, name: &str) {
        self.touch();
        let old = if let NodeType::Element(e) = &mut self.nodes[id].node_type {
            let old = e.attributes.get(name).cloned();
            e.attributes.remove(name);
            old
        } else {
            return;
        };
        self.record_attr(id, name.to_string(), old);
    }

    fn record(
        &mut self,
        target: NodeId,
        kind: &'static str,
        attr: Option<String>,
        added: Vec<NodeId>,
        removed: Vec<NodeId>,
    ) {
        self.records.push(DomMut { target, kind, attr, old_value: None, added, removed });
    }

    fn record_attr(&mut self, target: NodeId, name: String, old_value: Option<String>) {
        self.records.push(DomMut {
            target,
            kind: "attributes",
            attr: Some(name),
            old_value,
            added: Vec::new(),
            removed: Vec::new(),
        });
    }

    // 배달용으로 기록을 비우며 가져간다.
    pub fn take_records(&mut self) -> Vec<DomMut> {
        std::mem::take(&mut self.records)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srcset_selection_follows_standard() {
        // 예전엔 srcset 의 **첫 후보**를 그냥 썼다 — 보통 가장 작은 이미지라 흐리게 확대됐다.
        let c = parse_srcset("/a.png 200w, /b.png 800w, /c.png 1600w");
        assert_eq!(c.len(), 3);
        assert_eq!(c[1], ("/b.png".to_string(), Descriptor::Width(800.0)));
        // 목표 폭 이상 중 가장 작은 것
        assert_eq!(pick_candidate(&c, 700.0).as_deref(), Some("/b.png"));
        assert_eq!(pick_candidate(&c, 900.0).as_deref(), Some("/c.png"));
        // 목표보다 큰 후보가 없으면 가장 큰 것
        assert_eq!(pick_candidate(&c, 5000.0).as_deref(), Some("/c.png"));
        // 밀도 디스크립터: DPR 1 → 1x
        let d = parse_srcset("/a.png, /b.png 2x");
        assert_eq!(pick_candidate(&d, 100.0).as_deref(), Some("/a.png"));
        // URL 토큰은 공백까지 읽는다 → 콤마가 든 data: URI 도 안 깨진다
        let e = parse_srcset("data:image/png;base64,AAA=,BBB 1x, /x.png 2x");
        assert_eq!(e[0].0, "data:image/png;base64,AAA=,BBB");
        // sizes 해석
        assert_eq!(resolve_sizes("100vw", 1000.0), 1000.0);
        assert_eq!(resolve_sizes("(max-width: 600px) 100vw, 50vw", 1000.0), 500.0);
        assert_eq!(resolve_sizes("(max-width: 600px) 100vw, 50vw", 500.0), 500.0);
    }

    #[test]
    fn img_source_falls_back_to_srcset() {
        // 예전엔 src 만 봐서 srcset 만 있는 반응형 이미지가 아예 안 실렸다.
        let mut attrs = AttrMap::new();
        attrs.insert("srcset".to_string(), "a.png 1x, b.png 2x".to_string());
        let e = ElementData::html("img", attrs);
        assert_eq!(e.img_source().as_deref(), Some("a.png"), "DPR 1 → 1x 후보");

        // srcset 이 있으면 **srcset 이 이긴다** (src 는 srcset 을 모르는 브라우저용 폴백이다).
        // 예전엔 src 를 우선해서, 사이트가 폴백으로 둔 저해상도 이미지가 그대로 나왔다.
        let mut attrs = AttrMap::new();
        attrs.insert("src".to_string(), "c.png".to_string());
        attrs.insert("srcset".to_string(), "a.png 1x".to_string());
        let e = ElementData::html("img", attrs);
        assert_eq!(e.img_source().as_deref(), Some("a.png"));

        // srcset 이 비었으면 src
        let mut attrs = AttrMap::new();
        attrs.insert("src".to_string(), "c.png".to_string());
        attrs.insert("srcset".to_string(), "".to_string());
        let e = ElementData::html("img", attrs);
        assert_eq!(e.img_source().as_deref(), Some("c.png"));

        // 둘 다 없으면 None
        let e = ElementData::html("img", AttrMap::new());
        assert_eq!(e.img_source(), None);
    }

    #[test]
    fn text_node_has_no_children() {
        let n = text("hello".to_string());
        assert_eq!(n.children.len(), 0);
        assert_eq!(n.node_type, NodeType::Text("hello".to_string()));
    }

    // DOM 표준의 클래스 토큰화는 **ASCII 공백**만 구분자로 본다 (§Infra).
    // 유니코드 공백(NBSP, EN QUAD 등)은 클래스 이름의 일부다.
    // 예전엔 ' ' 하나로만 잘라서 탭이 든 class 가 한 덩어리가 됐다.
    #[test]
    fn class_tokenization_uses_ascii_whitespace_only() {
        let e = ElementData::html("div", {
            let mut a = AttrMap::new();
            a.insert("class".to_string(), "a\tb\nc  d".to_string());
            a
        });
        let cs = e.classes();
        assert_eq!(cs.len(), 4, "{:?}", cs);
        for want in ["a", "b", "c", "d"] {
            assert!(cs.contains(want), "{:?}", cs);
        }
        // NBSP 는 구분자가 아니다 — 이름의 일부
        let e2 = ElementData::html("div", {
            let mut a = AttrMap::new();
            a.insert("class".to_string(), "x\u{00A0}y".to_string());
            a
        });
        let cs2 = e2.classes();
        assert_eq!(cs2.len(), 1, "{:?}", cs2);
        assert!(cs2.contains("x\u{00A0}y"), "{:?}", cs2);
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

#[cfg(test)]
mod arena_tests {
    use super::*;

    fn tree() -> Node {
        // <div><p>a</p><p id="b">b</p></div>
        let mut attrs = AttrMap::new();
        attrs.insert("id".to_string(), "b".to_string());
        Node {
            node_type: NodeType::Element(ElementData::html("div", AttrMap::new())),
            children: vec![
                Node {
                    node_type: NodeType::Element(ElementData::html("p", AttrMap::new())),
                    children: vec![text("a".to_string())],
                },
                Node {
                    node_type: NodeType::Element(ElementData::html("p", attrs)),
                    children: vec![text("b".to_string())],
                },
            ],
        }
    }

    #[test]
    fn from_tree_preserves_structure_and_parents() {
        let dom = Dom::from_tree(tree());
        let root = dom.get(dom.root);
        assert_eq!(root.children.len(), 2);
        let p1 = root.children[0];
        assert_eq!(dom.get(p1).parent, Some(dom.root));
        assert_eq!(dom.text_content(dom.root), "ab");
        assert_eq!(dom.ancestors(dom.get(p1).children[0]), vec![p1, dom.root]);
    }

    #[test]
    fn find_by_attr_id_and_set_text() {
        let mut dom = Dom::from_tree(tree());
        let b = dom.find_by_attr_id("b").unwrap();
        assert_eq!(dom.text_content(b), "b");
        dom.set_text_content(b, "new".to_string());
        assert_eq!(dom.text_content(b), "new");
        assert_eq!(dom.text_content(dom.root), "anew");
        assert!(dom.find_by_attr_id("nope").is_none());
    }

    #[test]
    fn append_child_reparents_and_ignores_cycles() {
        let mut dom = Dom::from_tree(tree());
        let root = dom.root;
        let p1 = dom.get(root).children[0];
        let p2 = dom.get(root).children[1];
        // p1 을 p2 아래로 이동 (재부모화)
        dom.append_child(p2, p1);
        assert_eq!(dom.get(root).children, vec![p2]);
        assert_eq!(dom.get(p1).parent, Some(p2));
        assert_eq!(dom.text_content(p2), "ba");
        // 순환 무시: 조상을 자손 아래로 못 넣음
        dom.append_child(p1, root);
        assert_eq!(dom.get(root).parent, None);
        // NodeId 안정성: 구조가 바뀌어도 p1 핸들은 같은 노드
        assert_eq!(dom.text_content(p1), "a");
    }

    #[test]
    fn detach_and_create() {
        let mut dom = Dom::from_tree(tree());
        let root = dom.root;
        let p1 = dom.get(root).children[0];
        dom.detach(p1);
        assert_eq!(dom.get(root).children.len(), 1);
        assert_eq!(dom.text_content(root), "b");
        let li = dom.create_element("LI");
        if let NodeType::Element(e) = &dom.get(li).node_type {
            assert_eq!(e.tag_name, "li", "태그는 소문자 정규화");
        }
        let t = dom.create_text("x".to_string());
        dom.append_child(li, t);
        dom.append_child(root, li);
        assert_eq!(dom.text_content(root), "bx");
    }
}
