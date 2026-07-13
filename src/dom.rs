use std::collections::{HashMap, HashSet};

pub type AttrMap = HashMap<String, String>;

#[derive(Debug, PartialEq)]
pub struct Node {
    pub children: Vec<Node>,
    pub node_type: NodeType,
}

#[derive(Debug, PartialEq, Clone)]
pub enum NodeType {
    Text(String),
    Element(ElementData),
}

#[derive(Debug, PartialEq, Clone)]
pub struct ElementData {
    pub tag_name: String,
    pub attributes: AttrMap,
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

#[derive(Debug)]
pub struct Dom {
    nodes: Vec<NodeData>,
    pub root: NodeId,
    // 변형 카운터. 스타일/레이아웃 캐시가 자신이 본 버전과 비교해 재계산 여부를 정한다.
    // (JS 가 측정 API 를 읽을 때 강제 레이아웃을 흘려야 하는지 판정 — CSSOM View)
    version: u64,
}

impl Dom {
    pub fn version(&self) -> u64 {
        self.version
    }

    fn touch(&mut self) {
        self.version += 1;
    }

    pub fn from_tree(tree: Node) -> Dom {
        let mut dom = Dom { nodes: Vec::new(), root: 0, version: 0 };
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
        self.detach(child);
        self.nodes[child].parent = Some(parent);
        self.nodes[parent].children.push(child);
    }

    pub fn detach(&mut self, id: NodeId) {
        self.touch();
        if let Some(p) = self.nodes[id].parent.take() {
            self.nodes[p].children.retain(|&c| c != id);
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
    pub fn set_text_content(&mut self, id: NodeId, text: String) {
        self.clear_children(id);
        let t = self.create_text(text);
        self.nodes[t].parent = Some(id);
        self.nodes[id].children.push(t);
    }

    pub fn clear_children(&mut self, id: NodeId) {
        let old: Vec<NodeId> = std::mem::take(&mut self.nodes[id].children);
        for c in old {
            self.nodes[c].parent = None; // 고아로 방치 (아레나 재사용 없음)
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

#[cfg(test)]
mod arena_tests {
    use super::*;

    fn tree() -> Node {
        // <div><p>a</p><p id="b">b</p></div>
        let mut attrs = AttrMap::new();
        attrs.insert("id".to_string(), "b".to_string());
        Node {
            node_type: NodeType::Element(ElementData {
                tag_name: "div".to_string(),
                attributes: AttrMap::new(),
            }),
            children: vec![
                Node {
                    node_type: NodeType::Element(ElementData {
                        tag_name: "p".to_string(),
                        attributes: AttrMap::new(),
                    }),
                    children: vec![text("a".to_string())],
                },
                Node {
                    node_type: NodeType::Element(ElementData {
                        tag_name: "p".to_string(),
                        attributes: attrs,
                    }),
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
