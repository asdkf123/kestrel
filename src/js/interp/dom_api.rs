// DOM 바인딩 메서드(dom_get/dom_set/query 등). interp/mod.rs 에서 분리.
use super::value::*;
use super::*;

impl Interp {
    pub(super) fn dom_arena(&mut self) -> Result<&mut crate::dom::Dom, String> {
        match self.dom {
            // 안전성: run_scripts/dispatch 가 실행 동안에만 유효한 포인터를 설정/해제한다.
            Some(p) => unsafe { Ok(&mut *p) },
            None => Err("document 를 사용할 수 없음".to_string()),
        }
    }

    // §4.2.3 pre-insertion validity 핵심 검사. 위반이면 DOMException 을 던진다.
    //  - node 가 parent 의 inclusive ancestor(자기 자신 포함)면 순환 → HierarchyRequestError.
    //  - reference(있으면)의 부모가 parent 가 아니면 → NotFoundError.
    // insertBefore/appendChild 가 공유. (문서 자식 제약 등 나머지는 후속.)
    pub(super) fn ensure_pre_insert_valid(
        &mut self,
        parent: crate::dom::NodeId,
        node: crate::dom::NodeId,
        reference: Option<crate::dom::NodeId>,
    ) -> Result<(), String> {
        let bad: Option<(&'static str, &'static str)> = {
            let dom = self.dom_arena()?;
            if node == parent || dom.ancestors(parent).contains(&node) {
                Some(("HierarchyRequestError", "The new child is an ancestor of the parent"))
            } else if let Some(r) = reference {
                if dom.get(r).parent != Some(parent) {
                    Some((
                        "NotFoundError",
                        "The node before which the new node is to be inserted is not a child of this node",
                    ))
                } else {
                    None
                }
            } else {
                None
            }
        };
        if let Some((name, msg)) = bad {
            return Err(self.throw_dom(name, msg));
        }
        Ok(())
    }

    pub(super) fn dom_get_element_by_id(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let id = args.first().map(to_display).unwrap_or_default();
        let dom = self.dom_arena()?;
        match dom.find_by_attr_id(&id) {
            Some(node_id) => Ok(Value::Dom(node_id)),
            None => Ok(Value::Null),
        }
    }

    // CSS 선택자로 문서/서브트리 검색 (문서 순서 DFS). 미지원 선택자는 관용:
    // querySelector → null, querySelectorAll → 빈 배열.
    pub(super) fn dom_query(
        &mut self,
        scope: Option<crate::dom::NodeId>,
        sel_src: &str,
        all: bool,
    ) -> Result<Value, String> {
        let selectors = crate::css::parse_selector_list(sel_src);
        let dom = self.dom_arena()?;
        let mut out: Vec<Value> = Vec::new();
        if let Some(selectors) = selectors {
            fn rec(
                dom: &crate::dom::Dom,
                id: crate::dom::NodeId,
                selectors: &[crate::css::Selector],
                out: &mut Vec<Value>,
                all: bool,
            ) -> bool {
                if crate::style::element_matches(dom, id, selectors) {
                    out.push(Value::Dom(id));
                    if !all {
                        return true; // 첫 매칭에서 중단
                    }
                }
                dom.get(id).children.iter().any(|&c| rec(dom, c, selectors, out, all))
            }
            match scope {
                // 요소 스코프: 자손만 (자신 제외)
                Some(el) => {
                    let children = dom.get(el).children.clone();
                    children.iter().any(|&c| rec(dom, c, &selectors, &mut out, all));
                }
                None => {
                    rec(dom, dom.root, &selectors, &mut out, all);
                }
            }
        }
        if all {
            Ok(Value::Arr(ArrayObj::new(out)))
        } else {
            Ok(out.into_iter().next().unwrap_or(Value::Null))
        }
    }

    // 요소의 inline style 속성 문자열을 읽는다
    pub(super) fn style_attr(&mut self, id: crate::dom::NodeId) -> String {
        if let Ok(dom) = self.dom_arena() {
            if let crate::dom::NodeType::Element(e) = &dom.get(id).node_type {
                return e.attributes.get("style").cloned().unwrap_or_default();
            }
        }
        String::new()
    }

    pub(super) fn set_style_attr(&mut self, id: crate::dom::NodeId, value: String) {
        if let Ok(dom) = self.dom_arena() {
            if value.is_empty() {
                dom.remove_attr(id, "style");
            } else {
                dom.set_attr(id, "style", value);
            }
        }
    }

    // style.prop 읽기 (prop 은 CSS 케밥 이름)
    // el.style.prop 읽기. CSSOM 은 **정규 형태로 직렬화한 값**을 준다 (§6.7):
    //   style="background-color: black"  →  el.style.backgroundColor === "rgb(0, 0, 0)"
    // 예전엔 style 속성의 원문을 그대로 돌려줬다. 값을 되읽어 비교하는 코드가
    // 조용히 틀렸다 (setProperty 로 쓴 값은 정규화하면서 읽기만 원문이라 앞뒤가
    // 맞지 않기까지 했다).
    pub(super) fn style_get(&mut self, id: crate::dom::NodeId, prop: &str) -> String {
        let attr = self.style_attr(id);
        let raw = style_pairs(&attr)
            .into_iter()
            .rev() // 뒤 선언 우선 (마지막 것이 유효)
            .find(|(k, _)| k == prop)
            .map(|(_, v)| v)
            .unwrap_or_default();
        if raw.is_empty() {
            return raw;
        }
        Self::serialize_decl(prop, &raw)
    }

    // 선언 하나를 CSSOM 정규 형태로 직렬화 (§6.7).
    //
    // 인라인 스타일은 **지정값**을 직렬화한다 (계산값이 아니다):
    //   style="background-color: black"  →  el.style.backgroundColor === "black"
    //   getComputedStyle(el).backgroundColor === "rgb(0, 0, 0)"   ← 여기서만 rgb()
    // 색 키워드는 키워드로 남고, 숫자 표기(#f00 / rgb(1,2,3))만 rgb()/rgba() 로 접힌다.
    // font-weight: bold 도 700 이 아니라 bold 그대로, content: "x" 도 따옴표를 유지한다.
    // (파싱된 값으로 전부 직렬화하면 이 모두가 계산값으로 접혀 버린다 — 실제로 그랬다.)
    pub(super) fn serialize_decl(prop: &str, raw: &str) -> String {
        let raw = raw.trim();
        let parsed = crate::css::parse_inline_style(&format!("{}: {}", prop, raw))
            .into_iter()
            .find(|d| d.name == prop)
            .map(|d| d.value);
        match parsed {
            // 색: 키워드(black/transparent/currentcolor)는 그대로(소문자),
            // 그 외 표기(#f00, rgb(1,2,3))는 rgb()/rgba() 로 정규화
            Some(v @ crate::css::Value::Color(_)) => {
                if raw.chars().all(|c| c.is_ascii_alphabetic() || c == '-') {
                    raw.to_ascii_lowercase()
                } else {
                    crate::style::computed_value_string(&v)
                }
            }
            Some(v @ crate::css::Value::Url(_)) => crate::style::computed_value_string(&v),
            _ => normalize_numbers(raw),
        }
    }


    // style.prop = value 쓰기 (빈 값이면 제거)
    pub(super) fn style_set(&mut self, id: crate::dom::NodeId, prop: &str, value: &str) {
        let attr = self.style_attr(id);
        let mut pairs = style_pairs(&attr);
        pairs.retain(|(k, _)| k != prop);
        if !value.trim().is_empty() {
            // 인라인 스타일은 **지정값**을 보관한다 (계산값으로 접지 않는다).
            // 예전엔 여기서 computed_value_string 으로 접어서 `el.style.color = "black"`
            // 이 rgb(0, 0, 0) 으로 저장됐다 — 지정값이 통째로 사라졌다.
            // 직렬화는 **읽을 때** serialize_decl 이 한 번만 한다.
            let text = value.trim().to_string();
            pairs.push((prop.to_string(), text));
        }
        let s = style_serialize(&pairs);
        self.set_style_attr(id, s);
    }

    // element.classList: class 속성을 공백 구분 토큰 목록으로
    // 토큰 검증 (§7.1): 빈 문자열은 SyntaxError, ASCII 공백이 들어 있으면
    // InvalidCharacterError. 예전엔 검증이 없어서 조용히 통과했고, 공백이 든 토큰이
    // class 속성에 들어가 **두 개의 클래스**가 돼 버렸다.
    // XML Name 문법 (Namespaces in XML §2 / DOM §Validate).
    fn is_name_start(c: char) -> bool {
        c == ':'
            || c == '_'
            || c.is_ascii_alphabetic()
            || matches!(c as u32,
                0xC0..=0xD6 | 0xD8..=0xF6 | 0xF8..=0x2FF | 0x370..=0x37D | 0x37F..=0x1FFF
                | 0x200C..=0x200D | 0x2070..=0x218F | 0x2C00..=0x2FEF | 0x3001..=0xD7FF
                | 0xF900..=0xFDCF | 0xFDF0..=0xFFFD | 0x10000..=0xEFFFF)
    }

    fn is_name_char(c: char) -> bool {
        Self::is_name_start(c)
            || c == '-'
            || c == '.'
            || c.is_ascii_digit()
            || c as u32 == 0xB7
            || matches!(c as u32, 0x0300..=0x036F | 0x203F..=0x2040)
    }

    fn is_valid_name(name: &str) -> bool {
        let mut it = name.chars();
        match it.next() {
            Some(c) if Self::is_name_start(c) => {}
            _ => return false,
        }
        it.all(Self::is_name_char)
    }

    // §4.4 "locate a namespace": 요소에서 조상으로 올라가며 네임스페이스를 찾는다.
    // prefix 가 비면 기본 네임스페이스(xmlns), 아니면 xmlns:prefix 선언을 본다.
    // 선언이 없으면 요소 자신의 네임스페이스(접두사가 일치할 때)를 쓴다.
    pub(super) fn locate_namespace(
        &mut self,
        id: crate::dom::NodeId,
        prefix: &str,
    ) -> Result<Option<String>, String> {
        let dom = self.dom_arena()?;
        let mut cur = Some(id);
        while let Some(nid) = cur {
            if let crate::dom::NodeType::Element(e) = &dom.get(nid).node_type {
                // 요소 자신의 네임스페이스: 접두사가 일치하면 그것이다
                let own_prefix = e.prefix().unwrap_or("");
                if own_prefix == prefix && e.namespace.is_some() {
                    return Ok(e.namespace.clone());
                }
                // xmlns / xmlns:prefix 선언
                let attr = if prefix.is_empty() {
                    "xmlns".to_string()
                } else {
                    format!("xmlns:{}", prefix)
                };
                if let Some(v) = e.attributes.get(&attr) {
                    return Ok(if v.is_empty() { None } else { Some(v.clone()) });
                }
                // HTML 네임스페이스 요소이고 기본 네임스페이스를 찾는 중이면 HTML ns
                if prefix.is_empty() && own_prefix.is_empty() && e.namespace.is_none() {
                    return Ok(Some(crate::dom::NS_HTML.to_string()));
                }
            }
            cur = dom.get(nid).parent;
        }
        Ok(None)
    }

    // §4.4 "locate a namespace prefix": 이 네임스페이스를 선언한 접두사를 찾는다.
    pub(super) fn locate_prefix(
        &mut self,
        id: crate::dom::NodeId,
        ns: &str,
    ) -> Result<Option<String>, String> {
        let dom = self.dom_arena()?;
        let mut cur = Some(id);
        while let Some(nid) = cur {
            if let crate::dom::NodeType::Element(e) = &dom.get(nid).node_type {
                if e.ns() == ns {
                    if let Some(p) = e.prefix() {
                        return Ok(Some(p.to_string()));
                    }
                }
                for (k, v) in e.attributes.iter() {
                    if let Some(p) = k.strip_prefix("xmlns:") {
                        if v == ns {
                            return Ok(Some(p.to_string()));
                        }
                    }
                }
            }
            cur = dom.get(nid).parent;
        }
        Ok(None)
    }

    // 속성 이름 정규화 (§4.9): HTML 네임스페이스 요소의 속성 이름은 **소문자**다.
    // 검증도 함께 — 유효한 이름이 아니면 InvalidCharacterError.
    // 예전엔 둘 다 없어서 setAttribute('FOO', v) 가 "FOO" 라는 속성을 만들었고,
    // getAttribute('foo') 는 그걸 못 찾았다 (조회는 소문자로 하니까).
    pub(super) fn attr_name(
        &mut self,
        id: crate::dom::NodeId,
        raw: &str,
    ) -> Result<String, String> {
        if !Self::is_valid_name(raw) {
            return Err(self.throw_dom("InvalidCharacterError", "유효하지 않은 속성 이름"));
        }
        let dom = self.dom_arena()?;
        let html_ns = matches!(&dom.get(id).node_type,
            crate::dom::NodeType::Element(e) if e.namespace.is_none());
        Ok(if html_ns { raw.to_ascii_lowercase() } else { raw.to_string() })
    }

    // createElement 의 이름 검증 (§4.5): 유효한 Name 이 아니면 InvalidCharacterError.
    // 예전엔 빈 문자열만 걸렀다 — createElement("<div>") 가 조용히 통과했다.
    pub(super) fn validate_element_name(&mut self, name: &str) -> Result<(), String> {
        if !Self::is_valid_name(name) {
            return Err(self.throw_dom("InvalidCharacterError", "유효하지 않은 요소 이름"));
        }
        Ok(())
    }

    // createElementNS 의 (네임스페이스, 정규화 이름) 검증 (§Validate and extract).
    pub(super) fn validate_qualified_name(
        &mut self,
        qname: &str,
        ns: Option<&str>,
    ) -> Result<(), String> {
        if !Self::is_valid_name(qname) {
            return Err(self.throw_dom("InvalidCharacterError", "유효하지 않은 이름"));
        }
        let parts: Vec<&str> = qname.split(':').collect();
        if parts.len() > 2 || parts.iter().any(|p| p.is_empty()) {
            return Err(self.throw_dom("InvalidCharacterError", "유효하지 않은 정규화 이름"));
        }
        let prefix = if parts.len() == 2 { Some(parts[0]) } else { None };
        if prefix.is_some() && ns.is_none() {
            return Err(self.throw_dom("NamespaceError", "접두사에 네임스페이스가 없다"));
        }
        if prefix == Some("xml") && ns != Some("http://www.w3.org/XML/1998/namespace") {
            return Err(self.throw_dom("NamespaceError", "xml 접두사의 네임스페이스가 다르다"));
        }
        let xmlns_ns = "http://www.w3.org/2000/xmlns/";
        let is_xmlns = qname == "xmlns" || prefix == Some("xmlns");
        if is_xmlns != (ns == Some(xmlns_ns)) {
            return Err(self.throw_dom("NamespaceError", "xmlns 네임스페이스가 맞지 않다"));
        }
        Ok(())
    }

    // 검증 순서가 중요하다 (표준): **모든** 토큰의 빈 문자열을 먼저 보고,
    // 그다음 **모든** 토큰의 공백을 본다. replace(" ", "") 는 InvalidCharacterError 가
    // 아니라 SyntaxError 다 (두 번째 인자가 빈 문자열이므로).
    pub(super) fn validate_tokens(&mut self, tokens: &[String]) -> Result<(), String> {
        if tokens.iter().any(|t| t.is_empty()) {
            return Err(self.throw_dom("SyntaxError", "빈 토큰"));
        }
        if tokens.iter().any(|t| t.contains([' ', '\t', '\n', '\x0C', '\r'])) {
            return Err(self.throw_dom("InvalidCharacterError", "토큰에 공백이 있다"));
        }
        Ok(())
    }

    // DOMTokenList 의 토큰 집합 (§7.1): ASCII 공백으로 자르고 **중복을 없앤 순서 집합**.
    // 예전엔 유니코드 공백으로 자르고 중복도 남겨서, class="a a b" 의 length 가 3 이었다.
    pub(super) fn class_tokens(&mut self, id: crate::dom::NodeId) -> Vec<String> {
        let raw = self.class_attr(id);
        let mut out: Vec<String> = Vec::new();
        for t in crate::dom::split_ascii_ws(&raw) {
            if !out.iter().any(|x| x == t) {
                out.push(t.to_string());
            }
        }
        out
    }

    // class 속성의 **원문** (§7.1 value 는 반영 속성이라 정규화하지 않는다)
    pub(super) fn class_attr(&mut self, id: crate::dom::NodeId) -> String {
        if let Ok(dom) = self.dom_arena() {
            if let crate::dom::NodeType::Element(e) = &dom.get(id).node_type {
                return e.attributes.get("class").cloned().unwrap_or_default();
            }
        }
        String::new()
    }

    // "update steps" (§7.1): 토큰 집합을 공백 하나로 이어 class 속성에 쓴다.
    // 단 속성이 원래 없고 집합도 비면 **속성을 만들지 않는다** (표준).
    pub(super) fn set_class_tokens(&mut self, id: crate::dom::NodeId, tokens: Vec<String>) {
        let had = {
            match self.dom_arena() {
                Ok(dom) => matches!(&dom.get(id).node_type,
                    crate::dom::NodeType::Element(e) if e.attributes.get("class").is_some()),
                Err(_) => false,
            }
        };
        if !had && tokens.is_empty() {
            return;
        }
        let joined = tokens.join(" ");
        if let Ok(dom) = self.dom_arena() {
            dom.set_attr(id, "class", joined);
        }
    }

    // 렌더된 텍스트 (innerText). display:none 은 건너뛰고, 블록 경계마다 줄을 나누고,
    // 공백은 접는다 (white-space: pre* 면 보존).
    fn inner_text(&mut self, id: crate::dom::NodeId) -> String {
        // 자기 자신이 렌더되지 않으면 textContent 를 돌려준다 (표준 §3.6.1 1단계).
        let hidden = self
            .computed_styles
            .get(&id)
            .and_then(|m| m.get("display"))
            .map(|d| d == "none")
            .unwrap_or(false);
        if hidden {
            if let Ok(dom) = self.dom_arena() {
                return dom.text_content(id);
            }
        }
        let mut lines: Vec<String> = vec![String::new()];
        self.render_text_into(id, &mut lines, true);
        let out: Vec<String> = lines.iter().map(|l| l.trim_end().to_string()).collect();
        // 앞뒤 빈 줄은 버린다 (표준: 시작/끝의 줄바꿈 제거)
        let start = out.iter().position(|l| !l.is_empty()).unwrap_or(out.len());
        let end = out.iter().rposition(|l| !l.is_empty()).map(|i| i + 1).unwrap_or(start);
        out[start..end].join("\n")
    }

    fn render_text_into(&mut self, id: crate::dom::NodeId, lines: &mut Vec<String>, root: bool) {
        let disp = self
            .computed_styles
            .get(&id)
            .and_then(|m| m.get("display"))
            .cloned()
            .unwrap_or_default();
        // 루트 자신이 렌더되지 않으면 textContent 를 돌려준다 (표준).
        if disp == "none" && !root {
            return;
        }
        let (kids, is_text, text, tag) = {
            let Ok(dom) = self.dom_arena() else { return };
            let node = dom.get(id);
            match &node.node_type {
                crate::dom::NodeType::Text(t) => (Vec::new(), true, t.clone(), String::new()),
                // 코멘트는 텍스트 콘텐츠에 기여하지 않는다 (§4.5 textContent)
                crate::dom::NodeType::Comment(_) => {
                    (Vec::new(), true, String::new(), String::new())
                }
                crate::dom::NodeType::Element(e) => {
                    (node.children.clone(), false, String::new(), e.tag_name.to_ascii_lowercase())
                }
            }
        };
        if is_text {
            let ws = self
                .computed_styles
                .get(&id)
                .and_then(|m| m.get("white-space"))
                .cloned()
                .unwrap_or_default();
            let keep = ws.starts_with("pre");
            if keep {
                let mut parts = text.split('\n');
                if let Some(first) = parts.next() {
                    lines.last_mut().unwrap().push_str(first);
                }
                for p in parts {
                    lines.push(p.to_string());
                }
            } else {
                // 공백 접기: 연속 공백/줄바꿈 → 공백 하나
                let mut collapsed = String::new();
                let mut sp = false;
                for c in text.chars() {
                    if c.is_whitespace() {
                        if !sp {
                            collapsed.push(' ');
                            sp = true;
                        }
                    } else {
                        collapsed.push(c);
                        sp = false;
                    }
                }
                let cur = lines.last_mut().unwrap();
                // 줄 맨 앞의 공백은 버린다 (블록 시작의 공백은 렌더되지 않는다)
                if cur.is_empty() {
                    cur.push_str(collapsed.trim_start());
                } else {
                    cur.push_str(&collapsed);
                }
            }
            return;
        }
        if tag == "br" {
            lines.push(String::new());
            return;
        }
        // 블록 레벨이면 앞뒤로 줄을 나눈다
        let block = matches!(
            disp.as_str(),
            "block" | "flex" | "grid" | "list-item" | "table" | "table-row" | "table-cell"
                | "table-row-group" | "table-header-group" | "table-footer-group" | "flow-root"
        );
        if block && !lines.last().map(|l| l.is_empty()).unwrap_or(true) {
            lines.push(String::new());
        }
        for c in kids {
            self.render_text_into(c, lines, false);
        }
        if block && !lines.last().map(|l| l.is_empty()).unwrap_or(true) {
            lines.push(String::new());
        }
    }

    // offsetParent: 가장 가까운 위치 지정(static 아님) 조상. 없으면 body (표준 §CSSOM View).
    // position: fixed 인 요소와 body/html 자신은 null.
    fn offset_parent(&mut self, id: crate::dom::NodeId) -> Option<crate::dom::NodeId> {
        let pos = |me: &Self, n: crate::dom::NodeId| -> String {
            me.computed_styles
                .get(&n)
                .and_then(|m| m.get("position"))
                .cloned()
                .unwrap_or_else(|| "static".to_string())
        };
        if pos(self, id) == "fixed" {
            return None;
        }
        let dom = self.dom_arena().ok()?;
        // 조상 사슬 (아레나 borrow 를 먼저 끝낸다)
        let mut chain = Vec::new();
        let mut cur = dom.get(id).parent;
        while let Some(p) = cur {
            chain.push(p);
            cur = dom.get(p).parent;
        }
        let mut body = None;
        for p in &chain {
            if let crate::dom::NodeType::Element(e) = &self.dom_arena().ok()?.get(*p).node_type {
                if e.tag_name.eq_ignore_ascii_case("body") {
                    body = Some(*p);
                }
            }
        }
        for p in chain {
            if matches!(pos(self, p).as_str(), "relative" | "absolute" | "fixed" | "sticky") {
                return Some(p);
            }
        }
        body
    }

    pub(super) fn dom_get(&mut self, id: crate::dom::NodeId, key: &str) -> Result<Value, String> {
        // href/src 절대 URL 해석용 base (dom borrow 전에 복제).
        let base = self.base_url.clone();
        let self_shadow = self.shadow_hosts.contains(&id);
        // 레이아웃 측정 프로퍼티 (dom 아레나 borrow 전에 처리 — 이중 borrow 방지).
        // offset* 는 border box, client* 는 근사로 같은 박스 크기를 돌려준다.
        match key {
            "offsetWidth" | "clientWidth" | "scrollWidth" | "offsetHeight" | "clientHeight"
            | "scrollHeight" | "offsetLeft" | "clientLeft" | "offsetTop" | "clientTop"
            | "offsetParent" | "innerText" => {
                // 측정 전에 보류된 레이아웃을 흘린다 (CSSOM View: forced layout)
                // innerText 도 "렌더된 텍스트" 라 렌더 정보가 있어야 한다.
                self.ensure_layout();
            }
            _ => {}
        }
        // innerText: **렌더된** 텍스트 (HTML §3.6.1). textContent 와 다르다 —
        // display:none 인 가지, <script>/<style>/<template> 의 내용은 빠지고,
        // 블록 경계에서 줄바꿈이 들어가고, 공백은 접힌다.
        // (예전엔 textContent 별칭이라 스크립트 소스까지 그대로 돌려줬다.)
        if key == "innerText" {
            return Ok(Value::Str(self.inner_text(id)));
        }
        // CSSOM View §4 — 셋은 서로 다른 상자다:
        //   offset* = 테두리 박스, client* = 패딩 박스(테두리 제외), scroll* = 스크롤 오버플로.
        //   clientLeft/clientTop 은 **좌표가 아니라 테두리 두께**다.
        //   offsetLeft/offsetTop 은 offsetParent 의 패딩 모서리 기준 상대 좌표다.
        match key {
            "offsetWidth" => {
                let w = self.layout_rects.get(&id).map(|r| r.2).unwrap_or(0.0);
                return Ok(Value::Num(w as f64));
            }
            "offsetHeight" => {
                let h = self.layout_rects.get(&id).map(|r| r.3).unwrap_or(0.0);
                return Ok(Value::Num(h as f64));
            }
            "clientWidth" => {
                let m = self.layout_metrics.get(&id).copied().unwrap_or_default();
                return Ok(Value::Num(m.padding_w as f64));
            }
            "clientHeight" => {
                let m = self.layout_metrics.get(&id).copied().unwrap_or_default();
                return Ok(Value::Num(m.padding_h as f64));
            }
            "scrollWidth" => {
                let m = self.layout_metrics.get(&id).copied().unwrap_or_default();
                return Ok(Value::Num(m.scroll_w.round() as f64));
            }
            "scrollHeight" => {
                let m = self.layout_metrics.get(&id).copied().unwrap_or_default();
                return Ok(Value::Num(m.scroll_h.round() as f64));
            }
            "clientLeft" => {
                let m = self.layout_metrics.get(&id).copied().unwrap_or_default();
                return Ok(Value::Num(m.border.3 as f64));
            }
            "clientTop" => {
                let m = self.layout_metrics.get(&id).copied().unwrap_or_default();
                return Ok(Value::Num(m.border.0 as f64));
            }
            "offsetLeft" | "offsetTop" => {
                let (x, y, ..) = self.layout_rects.get(&id).copied().unwrap_or_default();
                // offsetParent 의 패딩 모서리를 원점으로
                let (ox, oy) = match self.offset_parent(id) {
                    Some(p) => {
                        let (px, py, ..) = self.layout_rects.get(&p).copied().unwrap_or_default();
                        let m = self.layout_metrics.get(&p).copied().unwrap_or_default();
                        (px + m.border.3, py + m.border.0)
                    }
                    None => (0.0, 0.0),
                };
                return Ok(Value::Num(if key == "offsetLeft" {
                    (x - ox) as f64
                } else {
                    (y - oy) as f64
                }));
            }
            // 가장 가까운 위치 지정 조상 (없으면 body). 툴팁/드롭다운 배치가 이걸로 좌표계를 잡는다.
            "offsetParent" => {
                return Ok(match self.offset_parent(id) {
                    Some(p) => Value::Dom(p),
                    None => Value::Null,
                });
            }
            // element.dataset — data-* 속성을 camelCase 키 객체로 (읽기 스냅샷)
            // dataset 은 **살아있는 뷰**다 (DOMStringMap): 읽기도 쓰기도 data-* 속성에 직결.
            // 예전엔 스냅샷 객체를 돌려줘서 el.dataset.x = '1' 이 조용히 사라졌다.
            "dataset" => return Ok(Value::Dataset(id)),
            _ => {}
        }
        let dom = self.dom_arena()?;
        let is_el = |d: &crate::dom::Dom, c: crate::dom::NodeId| {
            matches!(d.get(c).node_type, crate::dom::NodeType::Element(_))
        };
        match key {
            // <template>.content — 우리 파서는 템플릿 자식을 그대로 그 아래 둔다.
            // UA 스타일시트가 template 을 display:none 으로 감추므로 렌더되지 않는다.
            // 템플릿 자신을 돌려주면 content.querySelector/cloneNode/children 이 다 동작한다.
            "content"
                if matches!(&dom.get(id).node_type,
                    crate::dom::NodeType::Element(e) if e.tag_name == "template") =>
            {
                Ok(Value::Dom(id))
            }
            // DOMParser 가 돌려준 <html> 문서 노드에서의 body/head/documentElement
            "body" | "head" | "documentElement"
                if matches!(&dom.get(id).node_type,
                    crate::dom::NodeType::Element(e) if e.tag_name == "html") =>
            {
                if key == "documentElement" {
                    return Ok(Value::Dom(id));
                }
                let want = key;
                Ok(dom
                    .get(id)
                    .children
                    .iter()
                    .copied()
                    .find(|&c| matches!(&dom.get(c).node_type,
                        crate::dom::NodeType::Element(e) if e.tag_name == want))
                    .map(Value::Dom)
                    .unwrap_or(Value::Null))
            }
            // el.attributes — NamedNodeMap (§4.9.1). 진짜 Attr 노드들이다.
            // 예전엔 평범한 {name, value} 객체 배열이라, attr.value 를 바꿔도 요소에
            // 아무 반영이 없었고 attr.ownerElement 도 없었다.
            "attributes" => {
                let names: Vec<String> = match &dom.get(id).node_type {
                    crate::dom::NodeType::Element(e) => {
                        e.attributes.iter().map(|(k, _)| k.clone()).collect()
                    }
                    _ => Vec::new(),
                };
                let list: Vec<Value> =
                    names.iter().map(|n| Value::Attr(id, n.clone())).collect();
                // NamedNodeMap 은 인덱스와 이름 양쪽으로 접근한다 (attrs['class'])
                let arr = ArrayObj::new(list.clone());
                for (n, v) in names.iter().zip(list.iter()) {
                    arr.set_prop(n.clone(), v.clone());
                }
                arr.set_prop("getNamedItem".to_string(), Value::Native(Native::GetNamedItem));
                Ok(Value::Arr(arr))
            }
            // <style>/<link> 의 .sheet — 그 요소가 만든 CSSStyleSheet (§CSSOM 6.3)
            "sheet" => {
                self.sync_sheets();
                let owner = id;
                let idx = self
                    .sheets()
                    .and_then(|ss| ss.iter().position(|e| e.owner == Some(owner)));
                return Ok(idx.map(Value::Sheet).unwrap_or(Value::Null));
            }
            // 문서 트리에 붙어 있는가 (분리된 노드인지 판별 — 프레임워크가 흔히 본다)
            "isConnected" => {
                let root = dom.root;
                let connected = id == root || dom.ancestors(id).contains(&root);
                Ok(Value::Bool(connected))
            }
            // attachShadow 를 부른 요소면 자기 자신이 섀도 루트다 (문서화된 근사)
            "shadowRoot" => Ok(if self_shadow {
                Value::Dom(id)
            } else {
                Value::Null
            }),
            // <form>.elements — 폼 컨트롤 목록
            "elements"
                if matches!(&dom.get(id).node_type,
                    crate::dom::NodeType::Element(e) if e.tag_name == "form") =>
            {
                let mut out = Vec::new();
                collect_form_controls(dom, id, &mut out);
                Ok(Value::Arr(ArrayObj::new(out.into_iter().map(Value::Dom).collect())))
            }
            // element.style/classList → 속성에 대한 라이브 프록시
            "style" => Ok(Value::Style(id)),
            "classList" => Ok(Value::ClassList(id)),
            "textContent" => Ok(Value::Str(dom.text_content(id))),
            "innerHTML" => Ok(Value::Str(dom.inner_html(id))),
            "outerHTML" => Ok(Value::Str(dom.outer_html(id))),
            // value: <select> 는 선택된 option 의 값, <option> 은 value 속성 없으면 텍스트,
            // 그 외(input/textarea)는 value 속성. 예전엔 셋 다 value 속성만 봐서
            // select.value 가 늘 빈 문자열이었다(폼 로직이 통째로 어긋난다).
            "value" => match &dom.get(id).node_type {
                crate::dom::NodeType::Element(e) if e.tag_name == "select" => {
                    Ok(Value::Str(selected_option(dom, id).map(|o| option_value(dom, o)).unwrap_or_default()))
                }
                crate::dom::NodeType::Element(e) if e.tag_name == "option" => {
                    Ok(Value::Str(option_value(dom, id)))
                }
                crate::dom::NodeType::Element(e) if e.tag_name == "textarea" => Ok(Value::Str(
                    e.attributes.get("value").cloned().unwrap_or_else(|| dom.text_content(id)),
                )),
                crate::dom::NodeType::Element(e) => Ok(Value::Str(
                    e.attributes.get("value").cloned().unwrap_or_default(),
                )),
                _ => Ok(Value::Undefined),
            },
            // checked/selected/disabled 등 불리언 속성 반사. 예전엔 undefined 였다 —
            // `if (cb.checked)` 가 항상 거짓이라 체크박스 로직이 죽는다.
            "checked" | "disabled" | "readOnly" | "required" | "multiple" | "hidden" => {
                let attr = match key {
                    "readOnly" => "readonly",
                    k => k,
                };
                Ok(match &dom.get(id).node_type {
                    crate::dom::NodeType::Element(e) => {
                        Value::Bool(e.attributes.contains_key(attr))
                    }
                    _ => Value::Undefined,
                })
            }
            "selected" => Ok(match &dom.get(id).node_type {
                crate::dom::NodeType::Element(e) => {
                    Value::Bool(e.attributes.contains_key("selected"))
                }
                _ => Value::Undefined,
            }),
            "selectedIndex" => Ok(match &dom.get(id).node_type {
                crate::dom::NodeType::Element(e) if e.tag_name == "select" => {
                    let opts = option_list(dom, id);
                    let sel = selected_option(dom, id);
                    Value::Num(
                        sel.and_then(|s| opts.iter().position(|&o| o == s))
                            .map(|i| i as f64)
                            .unwrap_or(-1.0),
                    )
                }
                _ => Value::Undefined,
            }),
            "options" => Ok(match &dom.get(id).node_type {
                crate::dom::NodeType::Element(e) if e.tag_name == "select" => {
                    Value::Arr(ArrayObj::new(
                        option_list(dom, id).into_iter().map(Value::Dom).collect(),
                    ))
                }
                _ => Value::Undefined,
            }),
            // 트리 순회 프로퍼티 (프레임워크/앱 코드가 광범위하게 사용)
            "children" => {
                let arr: Vec<Value> = dom
                    .get(id)
                    .children
                    .clone()
                    .into_iter()
                    .filter(|&c| is_el(dom, c))
                    .map(Value::Dom)
                    .collect();
                Ok(Value::Arr(ArrayObj::new(arr)))
            }
            "childNodes" => {
                let arr: Vec<Value> =
                    dom.get(id).children.iter().copied().map(Value::Dom).collect();
                Ok(Value::Arr(ArrayObj::new(arr)))
            }
            "childElementCount" => {
                let n = dom.get(id).children.iter().filter(|&&c| is_el(dom, c)).count();
                Ok(Value::Num(n as f64))
            }
            "firstElementChild" => Ok(dom
                .get(id)
                .children
                .iter()
                .copied()
                .find(|&c| is_el(dom, c))
                .map(Value::Dom)
                .unwrap_or(Value::Null)),
            "lastElementChild" => Ok(dom
                .get(id)
                .children
                .iter()
                .copied()
                .rev()
                .find(|&c| is_el(dom, c))
                .map(Value::Dom)
                .unwrap_or(Value::Null)),
            "firstChild" => {
                Ok(dom.get(id).children.first().copied().map(Value::Dom).unwrap_or(Value::Null))
            }
            "lastChild" => {
                Ok(dom.get(id).children.last().copied().map(Value::Dom).unwrap_or(Value::Null))
            }
            // parentNode 는 모든 부모, parentElement 는 부모가 **요소일 때만** (§4.4).
            // 예전엔 둘이 같았다.
            "parentNode" => Ok(dom.get(id).parent.map(Value::Dom).unwrap_or(Value::Null)),
            "parentElement" => Ok(dom
                .get(id)
                .parent
                .filter(|&p| is_el(dom, p))
                .map(Value::Dom)
                .unwrap_or(Value::Null)),
            // nextSibling/previousSibling — **모든** 노드 종류를 센다 (텍스트/코멘트 포함).
            // 예전엔 nextElementSibling 만 있어서 이 둘이 undefined 였다. DOM 순회의
            // 기본 연산이라, 이게 없으면 TreeWalker 도 하이라이터도 한 노드에서 멈춘다.
            "nextSibling" | "previousSibling" => {
                let next = key.starts_with("next");
                let result = dom.get(id).parent.and_then(|p| {
                    let sibs = &dom.get(p).children;
                    let idx = sibs.iter().position(|&c| c == id)?;
                    if next {
                        sibs.get(idx + 1).copied()
                    } else {
                        idx.checked_sub(1).and_then(|i| sibs.get(i).copied())
                    }
                });
                Ok(result.map(Value::Dom).unwrap_or(Value::Null))
            }
            // 요소가 속한 문서. jQuery 의 setDocument 가 `node.ownerDocument || node` 로
            // 문서를 정하는데, 없으면 요소 자신을 document 로 삼아 document.createElement
            // 가 undefined 가 되며 jQuery 전체가 죽었다.
            "ownerDocument" => {
                Ok(env_get(&self.global, "document").unwrap_or(Value::Null))
            }
            // 문서 순서 비교 (jQuery 의 sortOrder). 4=뒤따름, 2=앞섬, 0=동일.
            "compareDocumentPosition" => Ok(Value::Native(Native::CompareDocPosition)),
            // getRootNode() — 노드가 속한 트리의 루트 (§4.4). 섀도우 DOM 없으므로 연결된
            // 노드는 document, 분리된 서브트리는 최상위 조상.
            "getRootNode" => Ok(Value::Native(Native::DomGetRootNode)),
            "nextElementSibling" | "previousElementSibling" => {
                let next = key.starts_with("next");
                let result = dom.get(id).parent.and_then(|p| {
                    let sibs = dom.get(p).children.clone();
                    let idx = sibs.iter().position(|&c| c == id)?;
                    let order: Vec<usize> = if next {
                        (idx + 1..sibs.len()).collect()
                    } else {
                        (0..idx).rev().collect()
                    };
                    order.into_iter().map(|i| sibs[i]).find(|&c| is_el(dom, c))
                });
                Ok(result.map(Value::Dom).unwrap_or(Value::Null))
            }
            // tagName 은 요소만. nodeName 은 모든 노드에 있다 (§4.4).
            // tagName: HTML 네임스페이스에서만 대문자로 (§4.9). SVG 의 clipPath 를
            // 대문자로 만들면 다른 이름이 된다.
            "tagName" => match &dom.get(id).node_type {
                crate::dom::NodeType::Element(e) => Ok(Value::Str(
                    if e.namespace.is_none() {
                        e.tag_name.to_ascii_uppercase()
                    } else {
                        e.tag_name.clone()
                    },
                )),
                _ => Ok(Value::Undefined),
            },
            // 네임스페이스 관련 (DOM §4.9). 예전엔 아예 없어서 undefined 였다.
            "localName" => Ok(match &dom.get(id).node_type {
                crate::dom::NodeType::Element(e) => Value::Str(e.local_name().to_string()),
                _ => Value::Undefined,
            }),
            "namespaceURI" => Ok(match &dom.get(id).node_type {
                crate::dom::NodeType::Element(e) => Value::Str(e.ns().to_string()),
                _ => Value::Null,
            }),
            "prefix" => Ok(match &dom.get(id).node_type {
                crate::dom::NodeType::Element(e) => {
                    e.prefix().map(|p| Value::Str(p.to_string())).unwrap_or(Value::Null)
                }
                _ => Value::Null,
            }),
            "nodeName" => Ok(Value::Str(match &dom.get(id).node_type {
                crate::dom::NodeType::Element(e) => {
                    if e.namespace.is_none() {
                        e.tag_name.to_ascii_uppercase()
                    } else {
                        e.tag_name.clone()
                    }
                }
                crate::dom::NodeType::Text(_) => "#text".to_string(),
                crate::dom::NodeType::Comment(_) => "#comment".to_string(),
            })),
            // nodeValue/data: 텍스트·코멘트의 문자 데이터 (§4.9 CharacterData).
            // 예전엔 아예 없어서 textNode.data 가 undefined 였다.
            "nodeValue" => Ok(match &dom.get(id).node_type {
                crate::dom::NodeType::Text(t) => Value::Str(t.clone()),
                crate::dom::NodeType::Comment(c) => Value::Str(c.clone()),
                crate::dom::NodeType::Element(_) => Value::Null, // 요소는 null (표준)
            }),
            "data" => match &dom.get(id).node_type {
                crate::dom::NodeType::Text(t) => Ok(Value::Str(t.clone())),
                crate::dom::NodeType::Comment(c) => Ok(Value::Str(c.clone())),
                crate::dom::NodeType::Element(_) => Ok(Value::Undefined),
            },
            "length" => match &dom.get(id).node_type {
                crate::dom::NodeType::Text(t) => {
                    Ok(Value::Num(t.encode_utf16().count() as f64))
                }
                crate::dom::NodeType::Comment(c) => {
                    Ok(Value::Num(c.encode_utf16().count() as f64))
                }
                crate::dom::NodeType::Element(_) => Ok(Value::Undefined),
            },
            // nodeType: ELEMENT_NODE(1) / TEXT_NODE(3).
            // jQuery·프레임워크가 노드 종류 판별에 광범위하게 쓴다.
            "nodeType" => Ok(Value::Num(match &dom.get(id).node_type {
                crate::dom::NodeType::Element(_) => 1.0,
                crate::dom::NodeType::Text(_) => 3.0,
                crate::dom::NodeType::Comment(_) => 8.0,
            })),
            "id" => match &dom.get(id).node_type {
                crate::dom::NodeType::Element(e) => {
                    Ok(Value::Str(e.attributes.get("id").cloned().unwrap_or_default()))
                }
                _ => Ok(Value::Undefined),
            },
            "className" => match &dom.get(id).node_type {
                crate::dom::NodeType::Element(e) => {
                    Ok(Value::Str(e.attributes.get("class").cloned().unwrap_or_default()))
                }
                _ => Ok(Value::Undefined),
            },
            // URL 반사 프로퍼티: 절대 URL 로 해석 (getAttribute 는 원문 반환).
            // <a>/<area>/<link> 의 URL 분해 속성 (HTML 표준 HTMLHyperlinkElementUtils).
            // 없으면 a.pathname 같은 흔한 코드가 undefined 를 읽고 죽는다 (naver).
            "protocol" | "hostname" | "host" | "port" | "pathname" | "search" | "hash"
            | "origin" => {
                let raw = match &dom.get(id).node_type {
                    crate::dom::NodeType::Element(e) => {
                        e.attributes.get("href").cloned().unwrap_or_default()
                    }
                    _ => String::new(),
                };
                if raw.is_empty() {
                    return Ok(Value::Str(String::new()));
                }
                let abs = match &base {
                    Some(b) => crate::url::Url::parse(b)
                        .ok()
                        .and_then(|u| u.join(&raw))
                        .map(|u| u.as_string())
                        .unwrap_or(raw.clone()),
                    None => raw.clone(),
                };
                let Ok(u) = crate::url::Url::parse(&abs) else {
                    return Ok(Value::Str(String::new()));
                };
                let path_no_hash = u.path.split('#').next().unwrap_or("").to_string();
                let (pathname, search) = match path_no_hash.split_once('?') {
                    Some((p, q)) => (p.to_string(), format!("?{}", q)),
                    None => (path_no_hash.clone(), String::new()),
                };
                // 프래그먼트는 Url 파서가 떼어내므로 **속성 원문**에서 뽑는다
                // (join 이 이미 버린 뒤라 절대 URL 에는 남아 있지 않다).
                let hash = match raw.split_once('#') {
                    Some((_, h)) if !h.is_empty() => format!("#{}", h),
                    _ => String::new(),
                };
                // host 는 포트를 포함한다 (기본 포트면 생략) — hostname 은 포트 없이.
                let default_port = matches!(
                    (u.scheme.as_str(), u.port),
                    ("http", 80) | ("https", 443)
                );
                let host = if default_port {
                    u.host.clone()
                } else {
                    format!("{}:{}", u.host, u.port)
                };
                let port = if default_port { String::new() } else { u.port.to_string() };
                Ok(Value::Str(match key {
                    "protocol" => format!("{}:", u.scheme),
                    "hostname" => u.host.clone(),
                    "host" => host.clone(),
                    "port" => port,
                    "pathname" => {
                        if pathname.is_empty() {
                            "/".to_string()
                        } else {
                            pathname
                        }
                    }
                    "search" => search,
                    "hash" => hash,
                    _ => format!("{}://{}", u.scheme, host),
                }))
            }
            "href" | "src" | "action" => match &dom.get(id).node_type {
                crate::dom::NodeType::Element(e) => {
                    let raw = e.attributes.get(key).cloned().unwrap_or_default();
                    let abs = match &base {
                        Some(b) if !raw.is_empty() => crate::url::Url::parse(b)
                            .ok()
                            .and_then(|u| u.join(&raw))
                            .map(|u| u.as_string())
                            .unwrap_or(raw),
                        _ => raw,
                    };
                    Ok(Value::Str(abs))
                }
                _ => Ok(Value::Undefined),
            },
            // 여기까지 안 잡혔으면 IDL 반영 표를 본다 (HTML §2.6).
            // 표에도 없으면 undefined (표준의 "그런 IDL 속성 없음").
            _ => Ok(self.reflect_get(id, key)?.unwrap_or(Value::Undefined)),
        }
    }

    pub(super) fn dom_set(&mut self, id: crate::dom::NodeId, key: &str, value: Value) -> Result<(), String> {
        // el.onclick = fn → 핸들러 등록
        if let Some(event) = key.strip_prefix("on") {
            if matches!(value, Value::Fn(_)) {
                self.handlers.push((id, event.to_string(), value, false, false)); // on* 속성은 버블 단계
            }
            return Ok(());
        }
        let text = to_display(&value);
        let dom = self.dom_arena()?;
        match key {
            "textContent" => {
                dom.set_text_content(id, text);
                Ok(())
            }
            // 문자 데이터 대입 (§4.9). 요소에 대한 nodeValue 대입은 무시 (표준).
            "nodeValue" | "data" => {
                dom.set_char_data(id, text);
                Ok(())
            }
            // innerText 대입: 줄바꿈은 <br> 가 된다 (표준). textContent 로 넣으면
            // 줄이 통째로 붙어 버린다.
            "innerText" => {
                if text.contains('\n') {
                    let html = text
                        .split('\n')
                        .map(|l| {
                            l.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
                        })
                        .collect::<Vec<_>>()
                        .join("<br>");
                    dom.clear_children(id);
                    for tree in crate::html::parse_fragment(html) {
                        let sub = dom.insert_tree(tree, Some(id));
                        dom.get_mut(id).children.push(sub);
                    }
                } else {
                    dom.set_text_content(id, text);
                }
                Ok(())
            }
            "innerHTML" => {
                // 조각 파싱 (관용 파서) → 자식 교체
                dom.clear_children(id);
                for tree in crate::html::parse_fragment(text) {
                    let sub = dom.insert_tree(tree, Some(id));
                    dom.get_mut(id).children.push(sub);
                }
                Ok(())
            }
            "value" => {
                // select.value = x → 그 값을 가진 option 을 선택 상태로 (표준)
                let is_select = matches!(&dom.get(id).node_type,
                    crate::dom::NodeType::Element(e) if e.tag_name == "select");
                if is_select {
                    for o in option_list(dom, id) {
                        if option_value(dom, o) == text {
                            dom.set_attr(o, "selected", String::new());
                        } else {
                            dom.remove_attr(o, "selected");
                        }
                    }
                    return Ok(());
                }
                dom.set_attr(id, "value", text);
                Ok(())
            }
            // 불리언 속성: true 면 속성 추가, false 면 제거 (표준 반사)
            "checked" | "disabled" | "readOnly" | "required" | "multiple" | "hidden"
            | "selected" => {
                let attr = match key {
                    "readOnly" => "readonly",
                    k => k,
                };
                if to_bool(&value) {
                    dom.set_attr(id, attr, String::new());
                } else {
                    dom.remove_attr(id, attr);
                }
                Ok(())
            }
            // className/id 는 대응 속성으로 (스타일 매칭이 읽음)
            "className" | "id" => {
                let attr = if key == "className" { "class" } else { "id" };
                dom.set_attr(id, attr, text);
                Ok(())
            }
            // IDL 반영 표 (HTML §2.6). 예전엔 표에 있는 속성도 조용히 무시했다 —
            // img.width = 100 이 아무 일도 안 했다.
            // classList / style 대입은 [PutForwards] 다 (표준):
            // el.classList = "a b" 는 class 속성을, el.style = "..." 는 style 속성을 쓴다.
            "classList" => {
                let dom = self.dom_arena()?;
                dom.set_attr(id, "class", text);
                Ok(())
            }
            _ => {
                if self.reflect_set(id, key, &value)? {
                    return Ok(());
                }
                // 이미 존재하는 IDL 속성(읽기 전용)에 대입하면 **아무 일도 없다** (표준의
                // sloppy 모드). expando 로 저장하면 진짜 프로퍼티를 가려 버린다 —
                // 실제로 el.classList = "x" 가 DOMTokenList 를 문자열로 덮어썼다.
                if !matches!(self.dom_get(id, key)?, Value::Undefined) {
                    return Ok(());
                }
                // 그 외에는 스크립트가 붙인 임의 프로퍼티(expando)로 보관한다.
                // 플랫폼 객체도 평범한 객체다 — el.foo = 1 이 실제로 저장돼야 한다.
                // 예전엔 조용히 버려서, 커스텀 엘리먼트의 this._v = ... 가 사라졌다.
                self.dom_props.insert((id, key.to_string()), value);
                Ok(())
            }
        }
    }
}

// <form> 안의 폼 컨트롤 (input/select/textarea/button)
pub(super) fn collect_form_controls(
    dom: &crate::dom::Dom,
    id: crate::dom::NodeId,
    out: &mut Vec<crate::dom::NodeId>,
) {
    for &c in &dom.get(id).children {
        if let crate::dom::NodeType::Element(e) = &dom.get(c).node_type {
            if matches!(e.tag_name.as_str(), "input" | "select" | "textarea" | "button") {
                out.push(c);
            }
        }
        collect_form_controls(dom, c, out);
    }
}

// <select> 의 option 목록 (optgroup 안쪽 포함)
pub(super) fn option_list(dom: &crate::dom::Dom, sel: crate::dom::NodeId) -> Vec<crate::dom::NodeId> {
    let mut out = Vec::new();
    fn walk(dom: &crate::dom::Dom, id: crate::dom::NodeId, out: &mut Vec<crate::dom::NodeId>) {
        for &c in &dom.get(id).children {
            if let crate::dom::NodeType::Element(e) = &dom.get(c).node_type {
                if e.tag_name == "option" {
                    out.push(c);
                } else {
                    walk(dom, c, out);
                }
            }
        }
    }
    walk(dom, sel, &mut out);
    out
}

// 선택된 option: selected 속성이 있는 첫 번째, 없으면 첫 option (HTML 표준의 기본 선택)
pub(super) fn selected_option(
    dom: &crate::dom::Dom,
    sel: crate::dom::NodeId,
) -> Option<crate::dom::NodeId> {
    let opts = option_list(dom, sel);
    opts.iter()
        .copied()
        .find(|&o| matches!(&dom.get(o).node_type,
            crate::dom::NodeType::Element(e) if e.attributes.contains_key("selected")))
        .or_else(|| opts.first().copied())
}

// option 의 값: value 속성이 없으면 텍스트 내용 (HTML 표준)
pub(super) fn option_value(dom: &crate::dom::Dom, o: crate::dom::NodeId) -> String {
    match &dom.get(o).node_type {
        crate::dom::NodeType::Element(e) => {
            e.attributes.get("value").cloned().unwrap_or_else(|| dom.text_content(o).trim().to_string())
        }
        _ => String::new(),
    }
}

// data-foo-bar → fooBar (dataset 키 변환)

// CSS 값 원문 안의 숫자를 정규 형태로 (§6.7.2 "serialize a CSS component value"):
//   .5 → 0.5,  1.50 → 1.5,  +3 → 3
// 문자열 리터럴과 url(...) 안은 건드리지 않는다 (그 안의 숫자는 값이 아니다).
fn normalize_numbers(s: &str) -> String {
    let b: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        // 문자열 리터럴: 그대로 복사
        if c == '"' || c == '\'' {
            let quote = c;
            out.push(c);
            i += 1;
            while i < b.len() {
                out.push(b[i]);
                let esc = b[i] == '\\';
                i += 1;
                if !esc && b.get(i - 1) == Some(&quote) {
                    break;
                }
            }
            continue;
        }
        // url( ... ) 안은 그대로
        if b[i..].starts_with(&['u', 'r', 'l', '(']) || b[i..].starts_with(&['U', 'R', 'L', '(']) {
            while i < b.len() {
                out.push(b[i]);
                i += 1;
                if b.get(i - 1) == Some(&')') {
                    break;
                }
            }
            continue;
        }
        // 숫자 시작인가. 식별자 중간의 숫자(예: rgb1)는 건드리지 않는다.
        let prev_ident = i > 0 && (b[i - 1].is_alphanumeric() || b[i - 1] == '-' || b[i - 1] == '_');
        let starts_num = c.is_ascii_digit()
            || (c == '.' && b.get(i + 1).map_or(false, |d| d.is_ascii_digit()))
            || ((c == '+' || c == '-')
                && b.get(i + 1).map_or(false, |d| {
                    d.is_ascii_digit() || (*d == '.' && b.get(i + 2).map_or(false, |e| e.is_ascii_digit()))
                }));
        if !starts_num || prev_ident {
            out.push(c);
            i += 1;
            continue;
        }
        let start = i;
        if b[i] == '+' || b[i] == '-' {
            i += 1;
        }
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i < b.len() && b[i] == '.' {
            i += 1;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
        }
        // 지수 표기 (1e3)
        if i < b.len() && (b[i] == 'e' || b[i] == 'E') {
            let save = i;
            i += 1;
            if i < b.len() && (b[i] == '+' || b[i] == '-') {
                i += 1;
            }
            if i < b.len() && b[i].is_ascii_digit() {
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
            } else {
                i = save;
            }
        }
        let text: String = b[start..i].iter().collect();
        match text.parse::<f32>() {
            Ok(n) => out.push_str(&crate::style::num_css(n)),
            Err(_) => out.push_str(&text),
        }
    }
    out
}
