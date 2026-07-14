// HTML 파서. WHATWG HTML §13 트리 구성 알고리즘(삽입 모드 상태 기계 +
// active formatting elements + adoption agency + 테이블 정규화)을 구현한다.
// 이전의 단순 스택 파서와 달리, 잘못 중첩된 포맷 태그(<b><i></b></i>),
// 암묵적 <p>/<li>/<tbody> 종료·삽입, foster parenting 을 표준대로 처리한다.
//
// 공개 API 는 이전과 동일:
//   parse(String) -> Node          (문서면 문서 파싱, 단편이면 fragment 파싱)
//   parse_dom(String) -> Dom
//   parse_fragment(String) -> Vec<Node>   (innerHTML 용)


use crate::dom::{AttrMap, ElementData, Node, NodeType};

// ── 공개 진입점 ─────────────────────────────────────────────────────

pub fn parse(source: String) -> Node {
    if looks_like_document(&source) {
        parse_document(source)
    } else {
        let mut roots = parse_fragment(source);
        if roots.len() == 1 {
            roots.pop().unwrap()
        } else {
            elem_node("html".to_string(), AttrMap::new(), roots)
        }
    }
}

pub fn parse_dom(source: String) -> crate::dom::Dom {
    crate::dom::Dom::from_tree(parse(source))
}

// innerHTML 용: 다중 루트를 감싸지 않고 그대로 반환 (context = body)
pub fn parse_fragment(source: String) -> Vec<Node> {
    let mut b = Builder::new_fragment();
    b.run(source);
    let root = b.fragment_root;
    let kids = b.sink.nodes[root].children.clone();
    kids.iter().map(|&c| b.to_node(c)).collect()
}

fn parse_document(source: String) -> Node {
    let mut b = Builder::new_document();
    b.run(source);
    // 문서 노드(0)의 자식 중 첫 element = <html>
    let doc = 0usize;
    let html = b.sink.nodes[doc]
        .children
        .iter()
        .copied()
        .find(|&c| matches!(b.sink.nodes[c].node_type, NodeType::Element(_)));
    match html {
        Some(h) => b.to_node(h),
        None => elem_node("html".to_string(), AttrMap::new(), vec![]),
    }
}

// 문서 파싱 대상인지 휴리스틱 (실제로 fetch 된 페이지는 반드시 doctype/html 을 가진다).
fn looks_like_document(s: &str) -> bool {
    let head: String = s.chars().take(512).collect::<String>().to_ascii_lowercase();
    head.contains("<!doctype") || head.contains("<html")
}

fn elem_node(name: String, attrs: AttrMap, children: Vec<Node>) -> Node {
    Node { children, node_type: NodeType::Element(ElementData { tag_name: name, attributes: attrs }) }
}

// ── 토큰 ────────────────────────────────────────────────────────────

enum Token {
    Doctype,
    Start { name: String, attrs: AttrMap, self_closing: bool },
    End { name: String },
    Text(String),
    Eof,
}

// ── 토크나이저 ──────────────────────────────────────────────────────

struct Tokenizer {
    input: Vec<u8>,
    pos: usize,
}

impl Tokenizer {
    fn new(source: String) -> Tokenizer {
        Tokenizer { input: source.into_bytes(), pos: 0 }
    }
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
    fn starts_with_ci(&self, s: &[u8]) -> bool {
        let end = self.pos + s.len();
        end <= self.input.len() && self.input[self.pos..end].eq_ignore_ascii_case(s)
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

    fn next(&mut self) -> Token {
        if self.eof() {
            return Token::Eof;
        }
        if self.starts_with(b"<!--") {
            self.pos += 4;
            self.skip_past(b"-->");
            return self.next();
        }
        if self.starts_with_ci(b"<!doctype") {
            self.skip_past(b">");
            return Token::Doctype;
        }
        if self.starts_with(b"<!") || self.starts_with(b"<?") {
            self.skip_past(b">");
            return self.next();
        }
        if self.starts_with(b"</") {
            let name = self.read_close_tag();
            if name.is_empty() {
                return self.next();
            }
            return Token::End { name };
        }
        if self.peek() == b'<' && self.peek_at(1).map_or(false, |c| c.is_ascii_alphabetic()) {
            return self.read_start_tag();
        }
        // 텍스트
        let text = self.read_text();
        if text.is_empty() {
            return self.next();
        }
        Token::Text(text)
    }

    fn read_text(&mut self) -> String {
        let start = self.pos;
        // '<' 이 태그 시작이 아닐 수도 있으니(예: "a < b") 한 글자 전진 후 다음 '<' 까지.
        if !self.eof() && self.peek() == b'<' {
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

    fn read_start_tag(&mut self) -> Token {
        self.pos += 1; // '<'
        let name = self.read_tag_name();
        let mut attrs = AttrMap::new();
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
                        attrs.insert_if_absent(k, v); // 중복 속성은 첫 값 유지 (표준)
                    }
                }
            }
        }
        Token::Start { name, attrs, self_closing }
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

    // RAWTEXT/RCDATA: </name 까지 원문 반환 후 종료 태그 소비. decode=true 면 엔티티 해석.
    fn read_rawtext(&mut self, name: &str, decode: bool) -> String {
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
        // 종료 태그 소비
        while !self.eof() && self.peek() != b'>' {
            self.pos += 1;
        }
        if !self.eof() {
            self.pos += 1;
        }
        if decode {
            decode_entities(&raw)
        } else {
            raw
        }
    }
}

// ── 파서 내부 트리(sink) ────────────────────────────────────────────

struct SinkNode {
    node_type: NodeType,
    children: Vec<usize>,
    parent: Option<usize>,
}

struct Sink {
    nodes: Vec<SinkNode>,
}

impl Sink {
    fn new() -> Sink {
        // 노드 0 = 문서
        Sink {
            nodes: vec![SinkNode {
                node_type: NodeType::Element(ElementData {
                    tag_name: "#document".to_string(),
                    attributes: AttrMap::new(),
                }),
                children: vec![],
                parent: None,
            }],
        }
    }
    fn new_element(&mut self, name: &str, attrs: AttrMap) -> usize {
        let id = self.nodes.len();
        self.nodes.push(SinkNode {
            node_type: NodeType::Element(ElementData { tag_name: name.to_string(), attributes: attrs }),
            children: vec![],
            parent: None,
        });
        id
    }
    fn new_text(&mut self, s: String) -> usize {
        let id = self.nodes.len();
        self.nodes.push(SinkNode { node_type: NodeType::Text(s), children: vec![], parent: None });
        id
    }
    fn append(&mut self, parent: usize, child: usize) {
        self.nodes[child].parent = Some(parent);
        self.nodes[parent].children.push(child);
    }
    fn insert_before(&mut self, parent: usize, child: usize, reference: usize) {
        self.nodes[child].parent = Some(parent);
        let pos = self.nodes[parent].children.iter().position(|&c| c == reference);
        match pos {
            Some(i) => self.nodes[parent].children.insert(i, child),
            None => self.nodes[parent].children.push(child),
        }
    }
    fn name(&self, id: usize) -> &str {
        match &self.nodes[id].node_type {
            NodeType::Element(e) => &e.tag_name,
            NodeType::Text(_) => "#text",
        }
    }
    fn attrs(&self, id: usize) -> AttrMap {
        match &self.nodes[id].node_type {
            NodeType::Element(e) => e.attributes.clone(),
            NodeType::Text(_) => AttrMap::new(),
        }
    }
}

// ── 삽입 모드 & 상태 ────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Initial,
    BeforeHtml,
    BeforeHead,
    InHead,
    AfterHead,
    InBody,
    InTable,
    InTableText,
    InCaption,
    InColumnGroup,
    InTableBody,
    InRow,
    InCell,
    InSelect,
    InSelectInTable,
    AfterBody,
    AfterAfterBody,
}

#[derive(Clone, Copy, PartialEq)]
enum Ns {
    Html,
    Svg,
    Math,
}

struct OpenElem {
    id: usize,
    name: String,
    ns: Ns,
}

enum Afe {
    Marker,
    Element { id: usize, name: String, attrs: AttrMap },
}

struct Builder {
    sink: Sink,
    open: Vec<OpenElem>,
    active: Vec<Afe>,
    mode: Mode,
    original_mode: Mode,
    head: Option<usize>,
    form: Option<usize>,
    frameset_ok: bool,
    fragment: bool,
    fragment_root: usize,
    tok: Tokenizer,
    table_text: String,
    table_text_ws_only: bool,
    strip_leading_newline: bool,
    foster_next: bool,
}

impl Builder {
    fn new_document() -> Builder {
        Builder {
            sink: Sink::new(),
            open: Vec::new(),
            active: Vec::new(),
            mode: Mode::Initial,
            original_mode: Mode::Initial,
            head: None,
            form: None,
            frameset_ok: true,
            fragment: false,
            fragment_root: 0,
            tok: Tokenizer::new(String::new()),
            table_text: String::new(),
            table_text_ws_only: true,
            strip_leading_newline: false,
            foster_next: false,
        }
    }

    fn new_fragment() -> Builder {
        let mut b = Builder::new_document();
        b.fragment = true;
        // 합성 <html> 루트를 만들어 문서에 붙이고 open 에 push, context=body → InBody
        let html = b.sink.new_element("html", AttrMap::new());
        b.sink.append(0, html);
        b.open.push(OpenElem { id: html, name: "html".to_string(), ns: Ns::Html });
        b.fragment_root = html;
        b.mode = Mode::InBody;
        b
    }

    fn to_node(&self, id: usize) -> Node {
        let node_type = self.sink.nodes[id].node_type.clone();
        let children = self.sink.nodes[id].children.iter().map(|&c| self.to_node(c)).collect();
        Node { children, node_type }
    }

    fn run(&mut self, source: String) {
        self.tok = Tokenizer::new(source);
        loop {
            let t = self.tok.next();
            if let Token::Eof = t {
                break;
            }
            // 모드 전환 시 같은 토큰 재처리를 위해 루프
            let mut pending = Some(t);
            while let Some(tok) = pending.take() {
                pending = self.step(tok);
            }
        }
    }

    // ── 스택/스코프 헬퍼 ──

    fn cur_html(&self, name: &str) -> bool {
        self.open.last().map_or(false, |e| e.ns == Ns::Html && e.name == name)
    }
    fn open_contains(&self, id: usize) -> bool {
        self.open.iter().any(|e| e.id == id)
    }

    fn scope(&self, target: &str, stops: &[&str]) -> bool {
        for e in self.open.iter().rev() {
            if e.ns == Ns::Html && e.name == target {
                return true;
            }
            if e.ns == Ns::Html && stops.contains(&e.name.as_str()) {
                return false;
            }
        }
        false
    }
    fn in_scope(&self, target: &str) -> bool {
        self.scope(target, DEFAULT_SCOPE)
    }
    fn in_button_scope(&self, target: &str) -> bool {
        self.scope(target, BUTTON_SCOPE)
    }
    fn in_list_scope(&self, target: &str) -> bool {
        self.scope(target, LIST_SCOPE)
    }
    fn in_table_scope(&self, target: &str) -> bool {
        self.scope(target, &["html", "table", "template"])
    }
    fn heading_in_scope(&self) -> bool {
        for e in self.open.iter().rev() {
            if e.ns == Ns::Html && is_heading(&e.name) {
                return true;
            }
            if e.ns == Ns::Html && DEFAULT_SCOPE.contains(&e.name.as_str()) {
                return false;
            }
        }
        false
    }

    fn pop_to_name(&mut self, name: &str) {
        while let Some(e) = self.open.pop() {
            if e.ns == Ns::Html && e.name == name {
                break;
            }
        }
    }
    fn pop_to_heading(&mut self) {
        while let Some(e) = self.open.pop() {
            if e.ns == Ns::Html && is_heading(&e.name) {
                break;
            }
        }
    }

    fn gen_implied(&mut self, except: &str) {
        while let Some(e) = self.open.last() {
            if e.ns == Ns::Html && e.name != except && is_implied_end(&e.name) {
                self.open.pop();
            } else {
                break;
            }
        }
    }

    fn close_p(&mut self) {
        if self.in_button_scope("p") {
            self.gen_implied("p");
            self.pop_to_name("p");
        }
    }

    // ── 삽입 ──

    // 삽입 위치: (부모 id, 참조 노드 옵션[이 노드 앞]) — foster parenting 적용
    fn insertion_place(&self) -> (usize, Option<usize>) {
        let target = self.open.last().map(|e| e.id).unwrap_or(self.fragment_root);
        if self.foster_next && self.open.last().map_or(false, |e| is_table_ctx(&e.name)) {
            if let Some(ti) = self.open.iter().rposition(|e| e.ns == Ns::Html && e.name == "table") {
                let table = self.open[ti].id;
                if let Some(p) = self.sink.nodes[table].parent {
                    return (p, Some(table));
                } else if ti > 0 {
                    return (self.open[ti - 1].id, None);
                }
            }
            if let Some(first) = self.open.first() {
                return (first.id, None);
            }
        }
        (target, None)
    }

    fn insert_at(&mut self, child: usize) {
        let (parent, reference) = self.insertion_place();
        match reference {
            Some(r) => self.sink.insert_before(parent, child, r),
            None => self.sink.append(parent, child),
        }
    }

    fn insert_element(&mut self, name: &str, attrs: AttrMap, ns: Ns) -> usize {
        let id = self.sink.new_element(name, attrs);
        self.insert_at(id);
        self.open.push(OpenElem { id, name: name.to_string(), ns });
        id
    }

    // void/self-closing: 삽입하되 스택에 남기지 않음
    fn insert_void(&mut self, name: &str, attrs: AttrMap) -> usize {
        let id = self.sink.new_element(name, attrs);
        self.insert_at(id);
        id
    }

    fn insert_text(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        let (parent, reference) = self.insertion_place();
        let siblings = &self.sink.nodes[parent].children;
        let merge_target = match reference {
            Some(r) => siblings.iter().position(|&c| c == r).and_then(|i| {
                if i > 0 {
                    Some(siblings[i - 1])
                } else {
                    None
                }
            }),
            None => siblings.last().copied(),
        };
        if let Some(t) = merge_target {
            if let NodeType::Text(existing) = &mut self.sink.nodes[t].node_type {
                existing.push_str(s);
                return;
            }
        }
        let t = self.sink.new_text(s.to_string());
        match reference {
            Some(r) => self.sink.insert_before(parent, t, r),
            None => self.sink.append(parent, t),
        }
    }

    // RAWTEXT/RCDATA 요소를 삽입하고 내용 텍스트까지 읽어 붙인 뒤 스택에서 제거.
    fn insert_rawtext(&mut self, name: &str, attrs: AttrMap, decode: bool) {
        let id = self.insert_void(name, attrs);
        let mut txt = self.tok.read_rawtext(name, decode);
        if (name == "textarea" || name == "pre" || name == "listing") && txt.starts_with('\n') {
            txt.remove(0);
        }
        if !txt.is_empty() {
            let t = self.sink.new_text(txt);
            self.sink.append(id, t);
        }
    }

    // head 요소(base/link/meta/title/style/script/noscript 등) 삽입
    fn head_start(&mut self, name: &str, attrs: AttrMap, _self_closing: bool) {
        match name {
            "base" | "basefont" | "bgsound" | "link" | "meta" => {
                self.insert_void(name, attrs);
            }
            "title" => self.insert_rawtext(name, attrs, true),
            "noscript" | "noframes" | "style" | "script" | "noembed" | "iframe" | "xmp" => {
                self.insert_rawtext(name, attrs, false)
            }
            "template" => {
                self.insert_element(name, attrs, Ns::Html);
            }
            _ => {}
        }
    }

    // ── active formatting elements ──

    fn push_marker(&mut self) {
        self.active.push(Afe::Marker);
    }
    fn clear_active_to_marker(&mut self) {
        while let Some(e) = self.active.pop() {
            if let Afe::Marker = e {
                break;
            }
        }
    }
    fn afe_index_of(&self, id: usize) -> Option<usize> {
        self.active.iter().position(|e| matches!(e, Afe::Element { id: eid, .. } if *eid == id))
    }
    fn afe_find_after_marker(&self, name: &str) -> Option<usize> {
        for e in self.active.iter().rev() {
            match e {
                Afe::Marker => return None,
                Afe::Element { id, name: n, .. } => {
                    if n == name {
                        return Some(*id);
                    }
                }
            }
        }
        None
    }

    fn reconstruct_active(&mut self) {
        if self.active.is_empty() {
            return;
        }
        let last_ok = match self.active.last().unwrap() {
            Afe::Marker => true,
            Afe::Element { id, .. } => self.open_contains(*id),
        };
        if last_ok {
            return;
        }
        let mut i = self.active.len() - 1;
        loop {
            if i == 0 {
                break;
            }
            i -= 1;
            match &self.active[i] {
                Afe::Marker => {
                    i += 1;
                    break;
                }
                Afe::Element { id, .. } => {
                    if self.open_contains(*id) {
                        i += 1;
                        break;
                    }
                }
            }
        }
        while i < self.active.len() {
            let (name, attrs) = match &self.active[i] {
                Afe::Element { name, attrs, .. } => (name.clone(), attrs.clone()),
                Afe::Marker => {
                    i += 1;
                    continue;
                }
            };
            let newid = self.insert_element(&name, attrs.clone(), Ns::Html);
            self.active[i] = Afe::Element { id: newid, name, attrs };
            i += 1;
        }
    }

    // ── adoption agency 알고리즘 ──
    // 처리했으면 true, "그 외 종료 태그처럼" 처리해야 하면 false 반환.
    fn adoption_agency(&mut self, subject: &str) -> bool {
        if self.cur_html(subject) {
            let cur_id = self.open.last().unwrap().id;
            if self.afe_index_of(cur_id).is_none() {
                self.open.pop();
                return true;
            }
        }
        let mut outer = 0;
        while outer < 8 {
            outer += 1;
            let fe_id = match self.afe_find_after_marker(subject) {
                Some(id) => id,
                None => return false,
            };
            let fe_stack_idx = match self.open.iter().position(|e| e.id == fe_id) {
                Some(i) => i,
                None => {
                    if let Some(ai) = self.afe_index_of(fe_id) {
                        self.active.remove(ai);
                    }
                    return true;
                }
            };
            if !self.in_scope(subject) {
                return true;
            }
            let furthest = self.open[fe_stack_idx + 1..]
                .iter()
                .position(|e| e.ns == Ns::Html && is_special(&e.name))
                .map(|rel| fe_stack_idx + 1 + rel);

            let furthest = match furthest {
                None => {
                    while self.open.len() > fe_stack_idx {
                        self.open.pop();
                    }
                    if let Some(ai) = self.afe_index_of(fe_id) {
                        self.active.remove(ai);
                    }
                    return true;
                }
                Some(f) => f,
            };

            let common_ancestor = self.open[fe_stack_idx - 1].id;
            let mut bookmark = self.afe_index_of(fe_id).unwrap();

            let furthest_id = self.open[furthest].id;
            let mut last_id = furthest_id;
            let mut node_idx = furthest;
            let mut inner = 0;
            loop {
                inner += 1;
                node_idx -= 1;
                let node_id = self.open[node_idx].id;
                if node_id == fe_id {
                    break;
                }
                let node_afe = self.afe_index_of(node_id);
                if inner > 3 {
                    if let Some(ai) = node_afe {
                        self.active.remove(ai);
                        if ai < bookmark {
                            bookmark -= 1;
                        }
                    }
                    self.open.remove(node_idx);
                    continue;
                }
                let ai = match node_afe {
                    None => {
                        self.open.remove(node_idx);
                        continue;
                    }
                    Some(ai) => ai,
                };
                let name = self.sink.name(node_id).to_string();
                let attrs = self.sink.attrs(node_id);
                let new_id = self.sink.new_element(&name, attrs.clone());
                self.active[ai] = Afe::Element { id: new_id, name: name.clone(), attrs };
                self.open[node_idx].id = new_id;
                let node_id = new_id;
                if last_id == furthest_id {
                    bookmark = ai + 1;
                }
                self.sink_reparent(last_id, node_id);
                last_id = node_id;
            }

            self.foster_or_append(common_ancestor, last_id);

            let fe_name = self.sink.name(fe_id).to_string();
            let fe_attrs = self.sink.attrs(fe_id);
            let new_fe = self.sink.new_element(&fe_name, fe_attrs.clone());
            let kids: Vec<usize> = self.sink.nodes[furthest_id].children.clone();
            for k in kids {
                self.sink_reparent(k, new_fe);
            }
            self.sink.append(furthest_id, new_fe);

            if let Some(ai) = self.afe_index_of(fe_id) {
                self.active.remove(ai);
                if ai < bookmark {
                    bookmark -= 1;
                }
            }
            if bookmark > self.active.len() {
                bookmark = self.active.len();
            }
            self.active
                .insert(bookmark, Afe::Element { id: new_fe, name: fe_name.clone(), attrs: fe_attrs });

            if let Some(oi) = self.open.iter().position(|e| e.id == fe_id) {
                self.open.remove(oi);
            }
            if let Some(fi) = self.open.iter().position(|e| e.id == furthest_id) {
                self.open.insert(fi + 1, OpenElem { id: new_fe, name: fe_name, ns: Ns::Html });
            }
        }
        true
    }

    fn sink_reparent(&mut self, child: usize, new_parent: usize) {
        if let Some(p) = self.sink.nodes[child].parent {
            self.sink.nodes[p].children.retain(|&c| c != child);
        }
        self.sink.append(new_parent, child);
    }

    fn foster_or_append(&mut self, target: usize, child: usize) {
        let name = self.sink.name(target).to_string();
        if is_table_ctx(&name) {
            if let Some(ti) = self.open.iter().rposition(|e| e.ns == Ns::Html && e.name == "table") {
                let table = self.open[ti].id;
                if let Some(p) = self.sink.nodes[table].parent {
                    if let Some(cp) = self.sink.nodes[child].parent {
                        self.sink.nodes[cp].children.retain(|&c| c != child);
                    }
                    self.sink.insert_before(p, child, table);
                    return;
                }
            }
        }
        self.sink_reparent(child, target);
    }

    // ── 테이블 스택 정리 ──
    fn clear_to_table_ctx(&mut self) {
        while let Some(e) = self.open.last() {
            if e.ns == Ns::Html && matches!(e.name.as_str(), "table" | "template" | "html") {
                break;
            }
            self.open.pop();
        }
    }
    fn clear_to_table_body_ctx(&mut self) {
        while let Some(e) = self.open.last() {
            if e.ns == Ns::Html
                && matches!(e.name.as_str(), "tbody" | "tfoot" | "thead" | "template" | "html")
            {
                break;
            }
            self.open.pop();
        }
    }
    fn clear_to_table_row_ctx(&mut self) {
        while let Some(e) = self.open.last() {
            if e.ns == Ns::Html && matches!(e.name.as_str(), "tr" | "template" | "html") {
                break;
            }
            self.open.pop();
        }
    }

    fn reset_insertion_mode(&mut self) {
        for i in (0..self.open.len()).rev() {
            let last = i == 0;
            let name = self.open[i].name.clone();
            let ns = self.open[i].ns;
            if ns != Ns::Html {
                continue;
            }
            self.mode = match name.as_str() {
                "select" => Mode::InSelect,
                "td" | "th" => Mode::InCell,
                "tr" => Mode::InRow,
                "tbody" | "thead" | "tfoot" => Mode::InTableBody,
                "caption" => Mode::InCaption,
                "colgroup" => Mode::InColumnGroup,
                "table" => Mode::InTable,
                "head" => Mode::InHead,
                "body" => Mode::InBody,
                "html" => {
                    if self.head.is_none() {
                        Mode::BeforeHead
                    } else {
                        Mode::AfterHead
                    }
                }
                _ => {
                    if last {
                        Mode::InBody
                    } else {
                        continue;
                    }
                }
            };
            return;
        }
        self.mode = Mode::InBody;
    }

    // ── foreign content (SVG/MathML) 단순 소비 ──
    fn consume_foreign(&mut self, root: &str, root_attrs: AttrMap) {
        let ns = if root == "svg" { Ns::Svg } else { Ns::Math };
        let root_id = self.insert_element(root, root_attrs, ns);
        let base_depth = self.open.len(); // root 포함 깊이
        loop {
            let t = self.tok.next();
            match t {
                Token::Eof => break,
                Token::Text(s) => self.insert_text(&s),
                Token::Doctype => {}
                Token::Start { name, attrs, self_closing } => {
                    if matches!(name.as_str(), "style" | "script" | "title") {
                        self.insert_rawtext(&name, attrs, name == "title");
                        continue;
                    }
                    if self_closing {
                        self.insert_void(&name, attrs);
                    } else {
                        self.insert_element(&name, attrs, ns);
                    }
                }
                Token::End { name } => {
                    if let Some(idx) = self
                        .open
                        .iter()
                        .rposition(|e| e.ns == ns && e.name.eq_ignore_ascii_case(&name))
                    {
                        if idx < base_depth - 1 {
                            while self.open.len() >= base_depth {
                                self.open.pop();
                            }
                            break;
                        }
                        while self.open.len() > idx {
                            self.open.pop();
                        }
                        if self.open.len() < base_depth {
                            break;
                        }
                    }
                    if !self.open.iter().any(|e| e.id == root_id) {
                        break;
                    }
                }
            }
        }
        while self.open.len() >= base_depth {
            self.open.pop();
        }
    }

    // ── 메인 디스패치 (재처리 필요하면 Some(token) 반환) ──
    fn step(&mut self, t: Token) -> Option<Token> {
        match self.mode {
            Mode::Initial => self.m_initial(t),
            Mode::BeforeHtml => self.m_before_html(t),
            Mode::BeforeHead => self.m_before_head(t),
            Mode::InHead => self.m_in_head(t),
            Mode::AfterHead => self.m_after_head(t),
            Mode::InBody => self.m_in_body(t),
            Mode::InTable => self.m_in_table(t),
            Mode::InTableText => self.m_in_table_text(t),
            Mode::InCaption => self.m_in_caption(t),
            Mode::InColumnGroup => self.m_in_column_group(t),
            Mode::InTableBody => self.m_in_table_body(t),
            Mode::InRow => self.m_in_row(t),
            Mode::InCell => self.m_in_cell(t),
            Mode::InSelect => self.m_in_select(t),
            Mode::InSelectInTable => self.m_in_select_in_table(t),
            Mode::AfterBody => self.m_after_body(t),
            Mode::AfterAfterBody => self.m_after_after_body(t),
        }
    }

    fn m_initial(&mut self, t: Token) -> Option<Token> {
        match t {
            Token::Doctype => {
                self.mode = Mode::BeforeHtml;
                None
            }
            Token::Text(s) if is_ws(&s) => None,
            other => {
                self.mode = Mode::BeforeHtml;
                Some(other)
            }
        }
    }

    fn m_before_html(&mut self, t: Token) -> Option<Token> {
        match t {
            Token::Doctype => None,
            Token::Text(s) if is_ws(&s) => None,
            Token::Start { name, attrs, .. } if name == "html" => {
                let id = self.sink.new_element("html", attrs);
                self.sink.append(0, id);
                self.open.push(OpenElem { id, name: "html".to_string(), ns: Ns::Html });
                self.mode = Mode::BeforeHead;
                None
            }
            Token::End { ref name } if matches!(name.as_str(), "head" | "body" | "html" | "br") => {
                self.default_html_root();
                self.mode = Mode::BeforeHead;
                Some(t)
            }
            Token::End { .. } => None,
            other => {
                self.default_html_root();
                self.mode = Mode::BeforeHead;
                Some(other)
            }
        }
    }

    fn default_html_root(&mut self) {
        let id = self.sink.new_element("html", AttrMap::new());
        self.sink.append(0, id);
        self.open.push(OpenElem { id, name: "html".to_string(), ns: Ns::Html });
    }

    fn m_before_head(&mut self, t: Token) -> Option<Token> {
        match t {
            Token::Doctype => None,
            Token::Text(s) if is_ws(&s) => None,
            Token::Start { name, attrs, .. } if name == "html" => {
                self.merge_html_attrs(attrs);
                None
            }
            Token::Start { name, attrs, .. } if name == "head" => {
                let id = self.insert_element("head", attrs, Ns::Html);
                self.head = Some(id);
                self.mode = Mode::InHead;
                None
            }
            Token::End { ref name } if matches!(name.as_str(), "head" | "body" | "html" | "br") => {
                let id = self.insert_element("head", AttrMap::new(), Ns::Html);
                self.head = Some(id);
                self.mode = Mode::InHead;
                Some(t)
            }
            Token::End { .. } => None,
            other => {
                let id = self.insert_element("head", AttrMap::new(), Ns::Html);
                self.head = Some(id);
                self.mode = Mode::InHead;
                Some(other)
            }
        }
    }

    fn m_in_head(&mut self, t: Token) -> Option<Token> {
        match t {
            Token::Doctype => None,
            Token::Text(s) => {
                if is_ws(&s) {
                    self.insert_text(&s);
                    None
                } else {
                    self.pop_head_to_after();
                    Some(Token::Text(s))
                }
            }
            Token::Start { name, attrs, .. } if name == "html" => {
                self.merge_html_attrs(attrs);
                None
            }
            Token::Start { name, attrs, self_closing }
                if matches!(
                    name.as_str(),
                    "base" | "basefont" | "bgsound" | "link" | "meta" | "title" | "noscript"
                        | "noframes" | "style" | "script" | "template" | "noembed"
                ) =>
            {
                self.head_start(&name, attrs, self_closing);
                None
            }
            Token::Start { name, .. } if name == "head" => None,
            Token::End { name } if name == "head" => {
                self.pop_to_name("head");
                self.mode = Mode::AfterHead;
                None
            }
            Token::End { name } if name == "template" => {
                self.pop_to_name("template");
                None
            }
            Token::End { ref name } if matches!(name.as_str(), "body" | "html" | "br") => {
                self.pop_head_to_after();
                Some(t)
            }
            Token::End { .. } => None,
            other => {
                self.pop_head_to_after();
                Some(other)
            }
        }
    }

    fn pop_head_to_after(&mut self) {
        if self.cur_html("head") {
            self.open.pop();
        }
        self.mode = Mode::AfterHead;
    }

    fn m_after_head(&mut self, t: Token) -> Option<Token> {
        match t {
            Token::Doctype => None,
            Token::Text(s) if is_ws(&s) => {
                self.insert_text(&s);
                None
            }
            Token::Start { name, attrs, .. } if name == "html" => {
                self.merge_html_attrs(attrs);
                None
            }
            Token::Start { name, attrs, .. } if name == "body" => {
                self.insert_element("body", attrs, Ns::Html);
                self.frameset_ok = false;
                self.mode = Mode::InBody;
                None
            }
            Token::Start { name, attrs, .. } if name == "frameset" => {
                self.insert_element("body", attrs, Ns::Html);
                self.mode = Mode::InBody;
                None
            }
            Token::Start { name, attrs, self_closing }
                if matches!(
                    name.as_str(),
                    "base" | "basefont" | "bgsound" | "link" | "meta" | "title" | "noframes"
                        | "style" | "script" | "template" | "noscript" | "noembed"
                ) =>
            {
                self.head_start(&name, attrs, self_closing);
                None
            }
            Token::End { ref name } if matches!(name.as_str(), "body" | "html" | "br") => {
                self.insert_element("body", AttrMap::new(), Ns::Html);
                self.mode = Mode::InBody;
                Some(t)
            }
            Token::End { .. } => None,
            other => {
                self.insert_element("body", AttrMap::new(), Ns::Html);
                self.mode = Mode::InBody;
                Some(other)
            }
        }
    }

    fn merge_html_attrs(&mut self, attrs: AttrMap) {
        let html_id = self.open.first().map(|e| e.id);
        if let Some(id) = html_id {
            if let NodeType::Element(e) = &mut self.sink.nodes[id].node_type {
                for (k, v) in attrs {
                    e.attributes.insert_if_absent(k, v);
                }
            }
        }
    }

    // ── in body (핵심) ──
    fn m_in_body(&mut self, t: Token) -> Option<Token> {
        match t {
            Token::Doctype => None,
            Token::Text(s) => {
                self.reconstruct_active();
                if !is_ws(&s) {
                    self.frameset_ok = false;
                }
                let s = if self.strip_leading_newline {
                    self.strip_leading_newline = false;
                    s.strip_prefix('\n').map(|r| r.to_string()).unwrap_or(s)
                } else {
                    s
                };
                self.insert_text(&s);
                None
            }
            Token::Start { name, attrs, self_closing } => self.in_body_start(name, attrs, self_closing),
            Token::End { name } => self.in_body_end(name),
            Token::Eof => None,
        }
    }

    fn in_body_start(&mut self, name: String, attrs: AttrMap, self_closing: bool) -> Option<Token> {
        match name.as_str() {
            "html" => {
                self.merge_html_attrs(attrs);
            }
            "base" | "basefont" | "bgsound" | "link" | "meta" | "noframes" | "script" | "style"
            | "template" | "title" | "noscript" | "noembed" => {
                self.head_start(&name, attrs, self_closing);
            }
            "body" => {
                if let Some(bid) = self.open.iter().find(|e| e.name == "body").map(|e| e.id) {
                    if let NodeType::Element(e) = &mut self.sink.nodes[bid].node_type {
                        for (k, v) in attrs {
                            e.attributes.insert_if_absent(k, v);
                        }
                    }
                }
            }
            "frameset" => {}
            "address" | "article" | "aside" | "blockquote" | "center" | "details" | "dialog"
            | "dir" | "div" | "dl" | "fieldset" | "figcaption" | "figure" | "footer" | "header"
            | "hgroup" | "main" | "menu" | "nav" | "ol" | "p" | "section" | "summary" | "ul"
            | "search" => {
                self.close_p();
                self.insert_element(&name, attrs, Ns::Html);
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.close_p();
                if self.open.last().map_or(false, |e| e.ns == Ns::Html && is_heading(&e.name)) {
                    self.open.pop();
                }
                self.insert_element(&name, attrs, Ns::Html);
            }
            "pre" | "listing" => {
                self.close_p();
                self.insert_element(&name, attrs, Ns::Html);
                self.strip_leading_newline = true;
                self.frameset_ok = false;
            }
            "form" => {
                if self.form.is_none() {
                    self.close_p();
                    let id = self.insert_element(&name, attrs, Ns::Html);
                    self.form = Some(id);
                }
            }
            "li" => {
                self.frameset_ok = false;
                let mut i = self.open.len();
                while i > 0 {
                    i -= 1;
                    let e_name = self.open[i].name.clone();
                    let e_html = self.open[i].ns == Ns::Html;
                    if e_html && e_name == "li" {
                        self.gen_implied("li");
                        self.pop_to_name("li");
                        break;
                    }
                    if e_html
                        && is_special(&e_name)
                        && !matches!(e_name.as_str(), "address" | "div" | "p")
                    {
                        break;
                    }
                }
                self.close_p();
                self.insert_element(&name, attrs, Ns::Html);
            }
            "dd" | "dt" => {
                self.frameset_ok = false;
                let mut i = self.open.len();
                while i > 0 {
                    i -= 1;
                    let e_name = self.open[i].name.clone();
                    let e_html = self.open[i].ns == Ns::Html;
                    if e_html && (e_name == "dd" || e_name == "dt") {
                        self.gen_implied(&e_name);
                        self.pop_to_name(&e_name);
                        break;
                    }
                    if e_html
                        && is_special(&e_name)
                        && !matches!(e_name.as_str(), "address" | "div" | "p")
                    {
                        break;
                    }
                }
                self.close_p();
                self.insert_element(&name, attrs, Ns::Html);
            }
            "plaintext" => {
                self.close_p();
                self.insert_rawtext(&name, attrs, false);
            }
            "button" => {
                if self.in_scope("button") {
                    self.gen_implied("");
                    self.pop_to_name("button");
                }
                self.reconstruct_active();
                self.insert_element(&name, attrs, Ns::Html);
                self.frameset_ok = false;
            }
            "a" => {
                if let Some(existing) = self.afe_find_after_marker("a") {
                    self.adoption_agency("a");
                    if let Some(ai) = self.afe_index_of(existing) {
                        self.active.remove(ai);
                    }
                    if let Some(oi) = self.open.iter().position(|e| e.id == existing) {
                        self.open.remove(oi);
                    }
                }
                self.reconstruct_active();
                let id = self.insert_element(&name, attrs.clone(), Ns::Html);
                self.active.push(Afe::Element { id, name, attrs });
            }
            "b" | "big" | "code" | "em" | "font" | "i" | "s" | "small" | "strike" | "tt" | "u" => {
                self.reconstruct_active();
                let id = self.insert_element(&name, attrs.clone(), Ns::Html);
                self.active.push(Afe::Element { id, name, attrs });
            }
            "nobr" => {
                self.reconstruct_active();
                if self.in_scope("nobr") {
                    self.adoption_agency("nobr");
                    self.reconstruct_active();
                }
                let id = self.insert_element(&name, attrs.clone(), Ns::Html);
                self.active.push(Afe::Element { id, name, attrs });
            }
            "applet" | "marquee" | "object" => {
                self.reconstruct_active();
                self.insert_element(&name, attrs, Ns::Html);
                self.push_marker();
                self.frameset_ok = false;
            }
            "table" => {
                self.close_p();
                self.insert_element(&name, attrs, Ns::Html);
                self.frameset_ok = false;
                self.mode = Mode::InTable;
            }
            "area" | "br" | "embed" | "img" | "keygen" | "wbr" => {
                self.reconstruct_active();
                self.insert_void(&name, attrs);
                self.frameset_ok = false;
            }
            "image" => {
                return self.in_body_start("img".to_string(), attrs, self_closing);
            }
            "input" => {
                self.reconstruct_active();
                let hidden = attrs.get("type").map_or(false, |v| v.eq_ignore_ascii_case("hidden"));
                self.insert_void(&name, attrs);
                if !hidden {
                    self.frameset_ok = false;
                }
            }
            "param" | "source" | "track" => {
                self.insert_void(&name, attrs);
            }
            "hr" => {
                self.close_p();
                self.insert_void(&name, attrs);
                self.frameset_ok = false;
            }
            "textarea" => {
                self.insert_rawtext(&name, attrs, true);
                self.frameset_ok = false;
            }
            "xmp" => {
                self.close_p();
                self.reconstruct_active();
                self.frameset_ok = false;
                self.insert_rawtext(&name, attrs, false);
            }
            "iframe" => {
                self.frameset_ok = false;
                self.insert_rawtext(&name, attrs, false);
            }
            "select" => {
                self.reconstruct_active();
                let table_ctx = matches!(
                    self.mode,
                    Mode::InTable | Mode::InCaption | Mode::InTableBody | Mode::InRow | Mode::InCell
                );
                self.insert_element(&name, attrs, Ns::Html);
                self.frameset_ok = false;
                self.mode = if table_ctx { Mode::InSelectInTable } else { Mode::InSelect };
            }
            "optgroup" | "option" => {
                if self.cur_html("option") {
                    self.open.pop();
                }
                self.reconstruct_active();
                self.insert_element(&name, attrs, Ns::Html);
            }
            "rb" | "rp" | "rt" | "rtc" => {
                self.gen_implied("");
                self.insert_element(&name, attrs, Ns::Html);
            }
            "math" => {
                self.reconstruct_active();
                self.consume_foreign("math", attrs);
            }
            "svg" => {
                self.reconstruct_active();
                self.consume_foreign("svg", attrs);
            }
            "caption" | "col" | "colgroup" | "frame" | "head" | "tbody" | "td" | "tfoot" | "th"
            | "thead" | "tr" => {
                // in body 에서는 무시 (파스 에러)
            }
            _ => {
                self.reconstruct_active();
                self.insert_element(&name, attrs, Ns::Html);
            }
        }
        None
    }

    fn in_body_end(&mut self, name: String) -> Option<Token> {
        match name.as_str() {
            "template" => {
                self.pop_to_name("template");
            }
            "body" => {
                if self.in_scope("body") {
                    self.mode = Mode::AfterBody;
                }
            }
            "html" => {
                if self.in_scope("body") {
                    self.mode = Mode::AfterBody;
                    return Some(Token::End { name });
                }
            }
            "address" | "article" | "aside" | "blockquote" | "button" | "center" | "details"
            | "dialog" | "dir" | "div" | "dl" | "fieldset" | "figcaption" | "figure" | "footer"
            | "header" | "hgroup" | "listing" | "main" | "menu" | "nav" | "ol" | "pre"
            | "section" | "summary" | "ul" | "search" => {
                if self.in_scope(&name) {
                    self.gen_implied("");
                    self.pop_to_name(&name);
                }
            }
            "form" => {
                let node = self.form.take();
                if let Some(fid) = node {
                    if self.open.iter().any(|e| e.id == fid) && self.in_scope("form") {
                        self.gen_implied("");
                        if let Some(oi) = self.open.iter().position(|e| e.id == fid) {
                            self.open.remove(oi);
                        }
                    }
                }
            }
            "p" => {
                if !self.in_button_scope("p") {
                    self.insert_element("p", AttrMap::new(), Ns::Html);
                    self.open.pop();
                }
                self.close_p();
            }
            "li" => {
                if self.in_list_scope("li") {
                    self.gen_implied("li");
                    self.pop_to_name("li");
                }
            }
            "dd" | "dt" => {
                if self.in_scope(&name) {
                    self.gen_implied(&name);
                    self.pop_to_name(&name);
                }
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                if self.heading_in_scope() {
                    self.gen_implied("");
                    self.pop_to_heading();
                }
            }
            "a" | "b" | "big" | "code" | "em" | "font" | "i" | "nobr" | "s" | "small" | "strike"
            | "tt" | "u" => {
                if !self.adoption_agency(&name) {
                    self.any_other_end(&name);
                }
            }
            "applet" | "marquee" | "object" => {
                if self.in_scope(&name) {
                    self.gen_implied("");
                    self.pop_to_name(&name);
                    self.clear_active_to_marker();
                }
            }
            "br" => {
                self.reconstruct_active();
                self.insert_void("br", AttrMap::new());
                self.frameset_ok = false;
            }
            _ => self.any_other_end(&name),
        }
        None
    }

    fn any_other_end(&mut self, name: &str) {
        let mut i = self.open.len();
        while i > 0 {
            i -= 1;
            let e_html = self.open[i].ns == Ns::Html;
            let e_name = self.open[i].name.clone();
            if e_html && e_name == name {
                self.gen_implied(name);
                while self.open.len() > i {
                    self.open.pop();
                }
                break;
            }
            if e_html && is_special(&e_name) {
                break;
            }
        }
    }

    // ── 테이블 모드들 ──
    fn m_in_table(&mut self, t: Token) -> Option<Token> {
        match t {
            Token::Text(s) => {
                // 표 안의 텍스트는 "in table text" 로 모아서 처리하고, 끝나면 **원래
                // 삽입 모드**로 돌아간다 (HTML 표준 §13.2.6.4.9).
                // 예전엔 원래 모드를 InTable 로 못 박았다. 그래서 <tr> 안에서
                // `</td> <td>` 처럼 공백 하나만 있어도 행 밖으로 튕겨 나가고,
                // 다음 <td> 가 새 tbody/tr 을 만들었다 — 표가 세로로 쪼개진다.
                // (위키백과의 힌트 상자가 정확히 이렇게 무너졌다)
                self.original_mode = self.mode;
                self.mode = Mode::InTableText;
                self.table_text.clear();
                self.table_text_ws_only = true;
                Some(Token::Text(s))
            }
            Token::Doctype => None,
            Token::Start { name, attrs, self_closing } => match name.as_str() {
                "caption" => {
                    self.clear_to_table_ctx();
                    self.push_marker();
                    self.insert_element("caption", attrs, Ns::Html);
                    self.mode = Mode::InCaption;
                    None
                }
                "colgroup" => {
                    self.clear_to_table_ctx();
                    self.insert_element("colgroup", attrs, Ns::Html);
                    self.mode = Mode::InColumnGroup;
                    None
                }
                "col" => {
                    self.clear_to_table_ctx();
                    self.insert_element("colgroup", AttrMap::new(), Ns::Html);
                    self.mode = Mode::InColumnGroup;
                    Some(Token::Start { name, attrs, self_closing })
                }
                "tbody" | "tfoot" | "thead" => {
                    self.clear_to_table_ctx();
                    self.insert_element(&name, attrs, Ns::Html);
                    self.mode = Mode::InTableBody;
                    None
                }
                "td" | "th" | "tr" => {
                    self.clear_to_table_ctx();
                    self.insert_element("tbody", AttrMap::new(), Ns::Html);
                    self.mode = Mode::InTableBody;
                    Some(Token::Start { name, attrs, self_closing })
                }
                "table" => {
                    if self.in_table_scope("table") {
                        self.pop_to_name("table");
                        self.reset_insertion_mode();
                        return Some(Token::Start { name, attrs, self_closing });
                    }
                    None
                }
                "style" | "script" | "template" => {
                    self.head_start(&name, attrs, self_closing);
                    None
                }
                "input"
                    if attrs.get("type").map_or(false, |v| v.eq_ignore_ascii_case("hidden")) =>
                {
                    self.insert_void(&name, attrs);
                    None
                }
                _ => self.table_foster(Token::Start { name, attrs, self_closing }),
            },
            Token::End { name } => match name.as_str() {
                "table" => {
                    if self.in_table_scope("table") {
                        self.pop_to_name("table");
                        self.reset_insertion_mode();
                    }
                    None
                }
                "body" | "caption" | "col" | "colgroup" | "html" | "tbody" | "td" | "tfoot"
                | "th" | "thead" | "tr" => None,
                "template" => {
                    self.pop_to_name("template");
                    None
                }
                _ => self.table_foster(Token::End { name }),
            },
            Token::Eof => None,
        }
    }

    fn table_foster(&mut self, t: Token) -> Option<Token> {
        self.foster_next = true;
        let r = self.m_in_body(t);
        self.foster_next = false;
        r
    }

    fn m_in_table_text(&mut self, t: Token) -> Option<Token> {
        match t {
            Token::Text(s) => {
                if !is_ws(&s) {
                    self.table_text_ws_only = false;
                }
                self.table_text.push_str(&s);
                None
            }
            other => {
                let text = std::mem::take(&mut self.table_text);
                let ws_only = self.table_text_ws_only;
                self.mode = self.original_mode;
                if !ws_only {
                    self.foster_next = true;
                    self.reconstruct_active();
                    self.insert_text(&text);
                    self.foster_next = false;
                } else {
                    self.insert_text(&text);
                }
                Some(other)
            }
        }
    }

    fn m_in_caption(&mut self, t: Token) -> Option<Token> {
        match t {
            Token::End { ref name } if name == "caption" => {
                if self.in_table_scope("caption") {
                    self.gen_implied("");
                    self.pop_to_name("caption");
                    self.clear_active_to_marker();
                    self.mode = Mode::InTable;
                }
                None
            }
            Token::Start { ref name, .. }
                if matches!(
                    name.as_str(),
                    "caption" | "col" | "colgroup" | "tbody" | "td" | "tfoot" | "th" | "thead" | "tr"
                ) =>
            {
                if self.in_table_scope("caption") {
                    self.gen_implied("");
                    self.pop_to_name("caption");
                    self.clear_active_to_marker();
                    self.mode = Mode::InTable;
                    return Some(t);
                }
                None
            }
            Token::End { ref name } if name == "table" => {
                if self.in_table_scope("caption") {
                    self.gen_implied("");
                    self.pop_to_name("caption");
                    self.clear_active_to_marker();
                    self.mode = Mode::InTable;
                    return Some(t);
                }
                None
            }
            Token::End { ref name }
                if matches!(
                    name.as_str(),
                    "body" | "col" | "colgroup" | "html" | "tbody" | "td" | "tfoot" | "th" | "thead"
                        | "tr"
                ) =>
            {
                None
            }
            other => self.m_in_body(other),
        }
    }

    fn m_in_column_group(&mut self, t: Token) -> Option<Token> {
        match t {
            Token::Text(s) if is_ws(&s) => {
                self.insert_text(&s);
                None
            }
            Token::Start { name, attrs, .. } if name == "col" => {
                self.insert_void("col", attrs);
                None
            }
            Token::Start { name, attrs, .. } if name == "html" => {
                self.merge_html_attrs(attrs);
                None
            }
            Token::End { ref name } if name == "colgroup" => {
                if self.cur_html("colgroup") {
                    self.open.pop();
                    self.mode = Mode::InTable;
                }
                None
            }
            Token::End { ref name } if name == "col" => None,
            Token::End { ref name } if name == "template" => {
                self.pop_to_name("template");
                None
            }
            other => {
                if self.cur_html("colgroup") {
                    self.open.pop();
                    self.mode = Mode::InTable;
                    Some(other)
                } else {
                    None
                }
            }
        }
    }

    fn m_in_table_body(&mut self, t: Token) -> Option<Token> {
        match t {
            Token::Start { name, attrs, self_closing } => match name.as_str() {
                "tr" => {
                    self.clear_to_table_body_ctx();
                    self.insert_element("tr", attrs, Ns::Html);
                    self.mode = Mode::InRow;
                    None
                }
                "td" | "th" => {
                    self.clear_to_table_body_ctx();
                    self.insert_element("tr", AttrMap::new(), Ns::Html);
                    self.mode = Mode::InRow;
                    Some(Token::Start { name, attrs, self_closing })
                }
                "caption" | "col" | "colgroup" | "tbody" | "tfoot" | "thead" => {
                    if self.in_table_scope("tbody")
                        || self.in_table_scope("thead")
                        || self.in_table_scope("tfoot")
                    {
                        self.clear_to_table_body_ctx();
                        self.open.pop();
                        self.mode = Mode::InTable;
                        return Some(Token::Start { name, attrs, self_closing });
                    }
                    None
                }
                _ => self.m_in_table(Token::Start { name, attrs, self_closing }),
            },
            Token::End { name } => match name.as_str() {
                "tbody" | "tfoot" | "thead" => {
                    if self.in_table_scope(&name) {
                        self.clear_to_table_body_ctx();
                        self.open.pop();
                        self.mode = Mode::InTable;
                    }
                    None
                }
                "table" => {
                    if self.in_table_scope("tbody")
                        || self.in_table_scope("thead")
                        || self.in_table_scope("tfoot")
                    {
                        self.clear_to_table_body_ctx();
                        self.open.pop();
                        self.mode = Mode::InTable;
                        return Some(Token::End { name });
                    }
                    None
                }
                "body" | "caption" | "col" | "colgroup" | "html" | "td" | "th" | "tr" => None,
                _ => self.m_in_table(Token::End { name }),
            },
            other => self.m_in_table(other),
        }
    }

    fn m_in_row(&mut self, t: Token) -> Option<Token> {
        match t {
            Token::Start { name, attrs, self_closing } => match name.as_str() {
                "td" | "th" => {
                    self.clear_to_table_row_ctx();
                    self.insert_element(&name, attrs, Ns::Html);
                    self.mode = Mode::InCell;
                    self.push_marker();
                    None
                }
                "caption" | "col" | "colgroup" | "tbody" | "tfoot" | "thead" | "tr" => {
                    if self.in_table_scope("tr") {
                        self.clear_to_table_row_ctx();
                        self.open.pop();
                        self.mode = Mode::InTableBody;
                        return Some(Token::Start { name, attrs, self_closing });
                    }
                    None
                }
                _ => self.m_in_table(Token::Start { name, attrs, self_closing }),
            },
            Token::End { name } => match name.as_str() {
                "tr" => {
                    if self.in_table_scope("tr") {
                        self.clear_to_table_row_ctx();
                        self.open.pop();
                        self.mode = Mode::InTableBody;
                    }
                    None
                }
                "table" => {
                    if self.in_table_scope("tr") {
                        self.clear_to_table_row_ctx();
                        self.open.pop();
                        self.mode = Mode::InTableBody;
                        return Some(Token::End { name });
                    }
                    None
                }
                "tbody" | "tfoot" | "thead" => {
                    if self.in_table_scope(&name) {
                        if self.in_table_scope("tr") {
                            self.clear_to_table_row_ctx();
                            self.open.pop();
                            self.mode = Mode::InTableBody;
                        }
                        return Some(Token::End { name });
                    }
                    None
                }
                "body" | "caption" | "col" | "colgroup" | "html" | "td" | "th" => None,
                _ => self.m_in_table(Token::End { name }),
            },
            other => self.m_in_table(other),
        }
    }

    fn m_in_cell(&mut self, t: Token) -> Option<Token> {
        match t {
            Token::End { name } if name == "td" || name == "th" => {
                if self.in_table_scope(&name) {
                    self.gen_implied("");
                    self.pop_to_name(&name);
                    self.clear_active_to_marker();
                    self.mode = Mode::InRow;
                }
                None
            }
            Token::Start { ref name, .. }
                if matches!(
                    name.as_str(),
                    "caption" | "col" | "colgroup" | "tbody" | "td" | "tfoot" | "th" | "thead" | "tr"
                ) =>
            {
                if self.in_table_scope("td") || self.in_table_scope("th") {
                    self.close_cell();
                    return Some(t);
                }
                None
            }
            Token::End { ref name }
                if matches!(name.as_str(), "table" | "tbody" | "tfoot" | "thead" | "tr") =>
            {
                if self.in_table_scope(name) {
                    self.close_cell();
                    return Some(t);
                }
                None
            }
            Token::End { ref name }
                if matches!(name.as_str(), "body" | "caption" | "col" | "colgroup" | "html") =>
            {
                None
            }
            other => self.m_in_body(other),
        }
    }

    fn close_cell(&mut self) {
        let which = if self.in_table_scope("td") { "td" } else { "th" };
        self.gen_implied("");
        self.pop_to_name(which);
        self.clear_active_to_marker();
        self.mode = Mode::InRow;
    }

    fn m_in_select(&mut self, t: Token) -> Option<Token> {
        match t {
            Token::Text(s) => {
                self.insert_text(&s);
                None
            }
            Token::Start { name, attrs, .. } => match name.as_str() {
                "html" => {
                    self.merge_html_attrs(attrs);
                    None
                }
                "option" => {
                    if self.cur_html("option") {
                        self.open.pop();
                    }
                    self.insert_element("option", attrs, Ns::Html);
                    None
                }
                "optgroup" => {
                    if self.cur_html("option") {
                        self.open.pop();
                    }
                    if self.cur_html("optgroup") {
                        self.open.pop();
                    }
                    self.insert_element("optgroup", attrs, Ns::Html);
                    None
                }
                "select" => {
                    if self.in_table_scope("select") {
                        self.pop_to_name("select");
                        self.reset_insertion_mode();
                    }
                    None
                }
                "input" | "keygen" | "textarea" => {
                    if self.in_table_scope("select") {
                        self.pop_to_name("select");
                        self.reset_insertion_mode();
                        return Some(Token::Start { name, attrs, self_closing: false });
                    }
                    None
                }
                "script" | "template" => {
                    self.head_start(&name, attrs, false);
                    None
                }
                _ => None,
            },
            Token::End { name } => match name.as_str() {
                "optgroup" => {
                    if self.cur_html("option")
                        && self.open.len() >= 2
                        && self.open[self.open.len() - 2].name == "optgroup"
                    {
                        self.open.pop();
                    }
                    if self.cur_html("optgroup") {
                        self.open.pop();
                    }
                    None
                }
                "option" => {
                    if self.cur_html("option") {
                        self.open.pop();
                    }
                    None
                }
                "select" => {
                    if self.in_table_scope("select") {
                        self.pop_to_name("select");
                        self.reset_insertion_mode();
                    }
                    None
                }
                "template" => {
                    self.pop_to_name("template");
                    None
                }
                _ => None,
            },
            _ => None,
        }
    }

    fn m_in_select_in_table(&mut self, t: Token) -> Option<Token> {
        match &t {
            Token::Start { name, .. }
                if matches!(
                    name.as_str(),
                    "caption" | "table" | "tbody" | "tfoot" | "thead" | "tr" | "td" | "th"
                ) =>
            {
                self.pop_to_name("select");
                self.reset_insertion_mode();
                Some(t)
            }
            Token::End { name }
                if matches!(
                    name.as_str(),
                    "caption" | "table" | "tbody" | "tfoot" | "thead" | "tr" | "td" | "th"
                ) =>
            {
                if self.in_table_scope(name) {
                    self.pop_to_name("select");
                    self.reset_insertion_mode();
                    return Some(t);
                }
                None
            }
            _ => self.m_in_select(t),
        }
    }

    fn m_after_body(&mut self, t: Token) -> Option<Token> {
        match t {
            Token::Text(s) if is_ws(&s) => self.m_in_body(Token::Text(s)),
            Token::End { ref name } if name == "html" => {
                self.mode = Mode::AfterAfterBody;
                None
            }
            Token::Eof => None,
            other => {
                self.mode = Mode::InBody;
                Some(other)
            }
        }
    }

    fn m_after_after_body(&mut self, t: Token) -> Option<Token> {
        match t {
            Token::Text(s) if is_ws(&s) => self.m_in_body(Token::Text(s)),
            Token::Doctype => None,
            Token::Eof => None,
            other => {
                self.mode = Mode::InBody;
                Some(other)
            }
        }
    }
}

// ── 문자 분류 & 요소 집합 ────────────────────────────────────────────

fn is_ws(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0c'))
}

fn is_heading(name: &str) -> bool {
    matches!(name, "h1" | "h2" | "h3" | "h4" | "h5" | "h6")
}

fn is_implied_end(name: &str) -> bool {
    matches!(name, "dd" | "dt" | "li" | "optgroup" | "option" | "p" | "rb" | "rp" | "rt" | "rtc")
}

fn is_table_ctx(name: &str) -> bool {
    matches!(name, "table" | "tbody" | "tfoot" | "thead" | "tr")
}

fn is_special(name: &str) -> bool {
    matches!(
        name,
        "address" | "applet" | "area" | "article" | "aside" | "base" | "basefont" | "bgsound"
            | "blockquote" | "body" | "br" | "button" | "caption" | "center" | "col" | "colgroup"
            | "dd" | "details" | "dir" | "div" | "dl" | "dt" | "embed" | "fieldset" | "figcaption"
            | "figure" | "footer" | "form" | "frame" | "frameset" | "h1" | "h2" | "h3" | "h4"
            | "h5" | "h6" | "head" | "header" | "hgroup" | "hr" | "html" | "iframe" | "img"
            | "input" | "keygen" | "li" | "link" | "listing" | "main" | "marquee" | "menu"
            | "meta" | "nav" | "noembed" | "noframes" | "noscript" | "object" | "ol" | "p"
            | "param" | "plaintext" | "pre" | "script" | "search" | "section" | "select"
            | "source" | "style" | "summary" | "table" | "tbody" | "td" | "template" | "textarea"
            | "tfoot" | "th" | "thead" | "title" | "tr" | "track" | "ul" | "wbr" | "xmp"
    )
}

const DEFAULT_SCOPE: &[&str] =
    &["applet", "caption", "html", "table", "td", "th", "marquee", "object", "template"];
const BUTTON_SCOPE: &[&str] = &[
    "applet", "caption", "html", "table", "td", "th", "marquee", "object", "template", "button",
];
const LIST_SCOPE: &[&str] = &[
    "applet", "caption", "html", "table", "td", "th", "marquee", "object", "template", "ol", "ul",
];

// ── 엔티티 디코드 ───────────────────────────────────────────────────

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
                    out.push_str(&ch);
                    rest = &after[semi + 1..];
                    continue;
                }
            }
        }
        // 세미콜론 없는 레거시 명명 참조 (&copy, &reg, &amp 등 — HTML 표준의 no-semicolon 목록)
        if let Some((ch, len)) = decode_legacy(&after[1..]) {
            out.push_str(&ch);
            rest = &after[1 + len..];
            continue;
        }
        out.push('&');
        rest = &after[1..];
    }
    out.push_str(rest);
    out
}

// 세미콜론 없이도 해석되는 레거시 명명 참조 여부 (HTML 표준 no-semicolon 목록 중 실사용분).
// 이 이름들만 세미콜론 없이 해석한다 (mdash/trade/hellip 등은 세미콜론 필수).
fn is_legacy_entity(name: &str) -> bool {
    matches!(
        name,
        "amp" | "lt" | "gt" | "quot" | "nbsp" | "copy" | "reg" | "laquo" | "raquo" | "middot"
            | "deg" | "times" | "divide" | "para" | "sect" | "pound" | "cent" | "yen" | "shy"
    )
}

// '&' 다음 문자열에서 세미콜론 없는 레거시 참조를 최장 일치로 해석.
// 반환: (해석 문자열, 소비한 이름 바이트 수). 뒤가 '=' 면 URL 쿼리로 보고 해석 안 함.
fn decode_legacy(body: &str) -> Option<(String, usize)> {
    let name_len = body.bytes().take_while(|b| b.is_ascii_alphabetic()).count();
    for len in (1..=name_len).rev() {
        if is_legacy_entity(&body[..len]) {
            if body[len..].starts_with('=') {
                return None;
            }
            return decode_one(&body[..len]).map(|s| (s, len));
        }
    }
    None
}

fn decode_one(entity: &str) -> Option<String> {
    let named = match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some('\u{00A0}'),
        "copy" => Some('\u{00A9}'),
        "reg" => Some('\u{00AE}'),
        "trade" => Some('\u{2122}'),
        "hellip" => Some('\u{2026}'),
        "mdash" => Some('\u{2014}'),
        "ndash" => Some('\u{2013}'),
        "lsquo" => Some('\u{2018}'),
        "rsquo" => Some('\u{2019}'),
        "ldquo" => Some('\u{201C}'),
        "rdquo" => Some('\u{201D}'),
        "laquo" => Some('\u{00AB}'),
        "raquo" => Some('\u{00BB}'),
        "middot" => Some('\u{00B7}'),
        "bull" => Some('\u{2022}'),
        "deg" => Some('\u{00B0}'),
        "times" => Some('\u{00D7}'),
        "divide" => Some('\u{00F7}'),
        "para" => Some('\u{00B6}'),
        "sect" => Some('\u{00A7}'),
        "euro" => Some('\u{20AC}'),
        "pound" => Some('\u{00A3}'),
        "cent" => Some('\u{00A2}'),
        "yen" => Some('\u{00A5}'),
        "shy" => Some('\u{00AD}'),
        "emsp" => Some('\u{2003}'),
        "ensp" => Some('\u{2002}'),
        "thinsp" => Some('\u{2009}'),
        "larr" => Some('\u{2190}'),
        "uarr" => Some('\u{2191}'),
        "rarr" => Some('\u{2192}'),
        "darr" => Some('\u{2193}'),
        "harr" => Some('\u{2194}'),
        "check" => Some('\u{2713}'),
        "star" => Some('\u{2605}'),
        _ => None,
    };
    if let Some(c) = named {
        return Some(c.to_string());
    }
    let num = entity.strip_prefix('#')?;
    let (radix, digits) = match num.strip_prefix('x').or_else(|| num.strip_prefix('X')) {
        Some(hex) => (16, hex),
        None => (10, num),
    };
    let code = u32::from_str_radix(digits, radix).ok()?;
    char::from_u32(code).map(|c| c.to_string())
}

#[cfg(test)]
mod tests {
    // (아래 표준 테스트들과 함께 실행)
    use super::*;

    #[test]
    fn whitespace_between_cells_keeps_one_row() {
        // 표 안 텍스트를 처리한 뒤엔 원래 삽입 모드로 돌아가야 한다(HTML 표준 §13.2.6.4.9).
        // 예전엔 InTable 로 못 박아서 `</td> <td>` 사이의 공백 하나에 행이 닫히고
        // 다음 셀이 새 tbody/tr 을 만들었다 — 표가 세로로 쪼개진다.
        let dom = parse_dom(
            "<table><tbody><tr><td>a</td> <td>b</td></tr></tbody></table>".to_string(),
        );
        let mut tbody = 0;
        let mut tr = 0;
        let mut td = 0;
        for i in 0..dom.node_count() {
            if let NodeType::Element(e) = &dom.get(i).node_type {
                match e.tag_name.as_str() {
                    "tbody" => tbody += 1,
                    "tr" => tr += 1,
                    "td" => td += 1,
                    _ => {}
                }
            }
        }
        assert_eq!((tbody, tr, td), (1, 1, 2), "한 행 두 칸이어야 한다");
    }

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

    fn find_href(node: &Node) -> Option<String> {
        if let NodeType::Element(e) = &node.node_type {
            if let Some(h) = e.attributes.get("href") {
                return Some(h.clone());
            }
        }
        node.children.iter().find_map(find_href)
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
    fn decodes_legacy_entities_without_semicolon() {
        // &copy, &reg 등 레거시 참조는 세미콜론 없이도 해석 (NPR 푸터 "&copy NPR")
        let n = parse("<p>&copy 2026 &reg &amp x</p>".to_string());
        let mut s = String::new();
        all_text(&n, &mut s);
        assert!(s.contains("\u{00A9} 2026 \u{00AE} & x"), "got {:?}", s);
        // 세미콜론 필수 엔티티는 세미콜론 없이 해석하지 않음
        let n2 = parse("<p>&mdash x</p>".to_string());
        let mut s2 = String::new();
        all_text(&n2, &mut s2);
        assert!(s2.contains("&mdash x"), "mdash 는 세미콜론 필수: {:?}", s2);
        // URL 쿼리 보호: &copy= 는 해석 안 함
        let n3 = parse("<a href=\"/x?a=1&copy=2\">l</a>".to_string());
        let href = find_href(&n3);
        assert_eq!(href.as_deref(), Some("/x?a=1&copy=2"), "쿼리스트링 &copy= 보존");
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

    // ── §13 트리 구성 규칙 ──

    #[test]
    fn implicit_p_close_makes_siblings() {
        let n = parse("<div><p>a<p>b</div>".to_string());
        let count = n
            .children
            .iter()
            .filter(|c| matches!(&c.node_type, NodeType::Element(e) if e.tag_name == "p"))
            .count();
        assert_eq!(count, 2, "형제 p 두 개");
    }

    #[test]
    fn table_implicit_tbody() {
        let n = parse("<table><tr><td>x</td></tr></table>".to_string());
        assert!(matches!(&n.node_type, NodeType::Element(e) if e.tag_name == "table"));
        let tbody = &n.children[0];
        assert!(
            matches!(&tbody.node_type, NodeType::Element(e) if e.tag_name == "tbody"),
            "암묵적 tbody"
        );
        let tr = &tbody.children[0];
        assert!(matches!(&tr.node_type, NodeType::Element(e) if e.tag_name == "tr"));
        let td = &tr.children[0];
        assert!(matches!(&td.node_type, NodeType::Element(e) if e.tag_name == "td"));
    }

    #[test]
    fn heading_auto_closes_previous() {
        let n = parse("<div><h1>a<h2>b</div>".to_string());
        let count = |name: &str| {
            n.children
                .iter()
                .filter(|c| matches!(&c.node_type, NodeType::Element(e) if e.tag_name == name))
                .count()
        };
        assert_eq!(count("h1"), 1, "h1 하나");
        assert_eq!(count("h2"), 1, "h2 하나 (형제)");
    }

    #[test]
    fn list_items_are_siblings() {
        let n = parse("<ul><li>a<li>b<li>c</ul>".to_string());
        let lis = n
            .children
            .iter()
            .filter(|c| matches!(&c.node_type, NodeType::Element(e) if e.tag_name == "li"))
            .count();
        assert_eq!(lis, 3, "li 세 개 형제");
    }

    #[test]
    fn adoption_agency_misnested_formatting() {
        let n = parse("<p><b>1<i>2</b>3</i></p>".to_string());
        let mut text = String::new();
        all_text(&n, &mut text);
        assert_eq!(text, "123", "텍스트 보존");
        let mut names = vec![];
        tag_names(&n, &mut names);
        assert!(names.iter().filter(|s| *s == "i").count() >= 2, "i 재생성: {:?}", names);
    }

    #[test]
    fn numeric_entities_decode_to_hangul() {
        let dom = super::parse_dom("<p>&#51060;&#48120;&#51648;</p>".to_string());
        assert_eq!(dom.text_content(dom.root), "이미지");
    }

    #[test]
    fn full_document_has_head_and_body() {
        let n = parse(
            "<!doctype html><html><head><title>t</title></head><body><p>x</p></body></html>"
                .to_string(),
        );
        assert!(matches!(&n.node_type, NodeType::Element(e) if e.tag_name == "html"));
        let mut names = vec![];
        tag_names(&n, &mut names);
        assert!(names.contains(&"head".to_string()));
        assert!(names.contains(&"body".to_string()));
        assert!(names.contains(&"title".to_string()));
    }

    #[test]
    fn stray_table_text_is_foster_parented() {
        let n = parse("<table>oops<tr><td>x</td></tr></table>".to_string());
        let mut text = String::new();
        all_text(&n, &mut text);
        assert!(text.contains("oops"), "foster 된 텍스트 보존: {:?}", text);
        assert!(text.contains("x"));
    }
}
