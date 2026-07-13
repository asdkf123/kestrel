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
    pub(super) fn style_get(&mut self, id: crate::dom::NodeId, prop: &str) -> String {
        let attr = self.style_attr(id);
        style_pairs(&attr)
            .into_iter()
            .rev() // 뒤 선언 우선 (마지막 것이 유효)
            .find(|(k, _)| k == prop)
            .map(|(_, v)| v)
            .unwrap_or_default()
    }

    // style.prop = value 쓰기 (빈 값이면 제거)
    pub(super) fn style_set(&mut self, id: crate::dom::NodeId, prop: &str, value: &str) {
        let attr = self.style_attr(id);
        let mut pairs = style_pairs(&attr);
        pairs.retain(|(k, _)| k != prop);
        if !value.trim().is_empty() {
            pairs.push((prop.to_string(), value.trim().to_string()));
        }
        let s = style_serialize(&pairs);
        self.set_style_attr(id, s);
    }

    // element.classList: class 속성을 공백 구분 토큰 목록으로
    pub(super) fn class_tokens(&mut self, id: crate::dom::NodeId) -> Vec<String> {
        if let Ok(dom) = self.dom_arena() {
            if let crate::dom::NodeType::Element(e) = &dom.get(id).node_type {
                if let Some(c) = e.attributes.get("class") {
                    return c.split_whitespace().map(|s| s.to_string()).collect();
                }
            }
        }
        Vec::new()
    }

    pub(super) fn set_class_tokens(&mut self, id: crate::dom::NodeId, tokens: Vec<String>) {
        let joined = tokens.join(" ");
        if let Ok(dom) = self.dom_arena() {
            if joined.is_empty() {
                dom.remove_attr(id, "class");
            } else {
                dom.set_attr(id, "class", joined);
            }
        }
    }

    pub(super) fn dom_get(&mut self, id: crate::dom::NodeId, key: &str) -> Result<Value, String> {
        // href/src 절대 URL 해석용 base (dom borrow 전에 복제).
        let base = self.base_url.clone();
        let self_shadow = self.shadow_hosts.contains(&id);
        // 레이아웃 측정 프로퍼티 (dom 아레나 borrow 전에 처리 — 이중 borrow 방지).
        // offset* 는 border box, client* 는 근사로 같은 박스 크기를 돌려준다.
        match key {
            "offsetWidth" | "clientWidth" | "scrollWidth" | "offsetHeight" | "clientHeight"
            | "scrollHeight" | "offsetLeft" | "clientLeft" | "offsetTop" | "clientTop" => {
                // 측정 전에 보류된 레이아웃을 흘린다 (CSSOM View: forced layout)
                self.ensure_layout();
            }
            _ => {}
        }
        match key {
            "offsetWidth" | "clientWidth" | "scrollWidth" => {
                let w = self.layout_rects.get(&id).map(|r| r.2).unwrap_or(0.0);
                return Ok(Value::Num(w as f64));
            }
            "offsetHeight" | "clientHeight" | "scrollHeight" => {
                let h = self.layout_rects.get(&id).map(|r| r.3).unwrap_or(0.0);
                return Ok(Value::Num(h as f64));
            }
            "offsetLeft" | "clientLeft" => {
                let x = self.layout_rects.get(&id).map(|r| r.0).unwrap_or(0.0);
                return Ok(Value::Num(x as f64));
            }
            "offsetTop" | "clientTop" => {
                let y = self.layout_rects.get(&id).map(|r| r.1).unwrap_or(0.0);
                return Ok(Value::Num(y as f64));
            }
            // element.dataset — data-* 속성을 camelCase 키 객체로 (읽기 스냅샷)
            "dataset" => {
                let dom = self.dom_arena()?;
                let mut map = ObjMap::new();
                if let crate::dom::NodeType::Element(e) = &dom.get(id).node_type {
                    for (k, v) in e.attributes.iter() {
                        if let Some(rest) = k.strip_prefix("data-") {
                            map.insert(kebab_to_camel(rest), Value::Str(v.clone()));
                        }
                    }
                }
                return Ok(Value::Obj(std::rc::Rc::new(std::cell::RefCell::new(map))));
            }
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
            // el.attributes — [{name, value}, …] (예전엔 undefined 라 읽는 순간 죽었다)
            "attributes" => {
                let list: Vec<Value> = match &dom.get(id).node_type {
                    crate::dom::NodeType::Element(e) => e
                        .attributes
                        .iter()
                        .map(|(k, v)| {
                            let mut m = ObjMap::new();
                            m.insert("name".to_string(), Value::Str(k.clone()));
                            m.insert("value".to_string(), Value::Str(v.clone()));
                            Value::Obj(std::rc::Rc::new(std::cell::RefCell::new(m)))
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                Ok(Value::Arr(ArrayObj::new(list)))
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
            "textContent" | "innerText" => Ok(Value::Str(dom.text_content(id))),
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
            "parentElement" | "parentNode" => {
                Ok(dom.get(id).parent.map(Value::Dom).unwrap_or(Value::Null))
            }
            // 요소가 속한 문서. jQuery 의 setDocument 가 `node.ownerDocument || node` 로
            // 문서를 정하는데, 없으면 요소 자신을 document 로 삼아 document.createElement
            // 가 undefined 가 되며 jQuery 전체가 죽었다.
            "ownerDocument" => {
                Ok(env_get(&self.global, "document").unwrap_or(Value::Null))
            }
            // 문서 순서 비교 (jQuery 의 sortOrder). 4=뒤따름, 2=앞섬, 0=동일.
            "compareDocumentPosition" => Ok(Value::Native(Native::CompareDocPosition)),
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
            "tagName" | "nodeName" => match &dom.get(id).node_type {
                crate::dom::NodeType::Element(e) => Ok(Value::Str(e.tag_name.to_ascii_uppercase())),
                _ => Ok(Value::Undefined),
            },
            // nodeType: ELEMENT_NODE(1) / TEXT_NODE(3).
            // jQuery·프레임워크가 노드 종류 판별에 광범위하게 쓴다.
            "nodeType" => Ok(Value::Num(match &dom.get(id).node_type {
                crate::dom::NodeType::Element(_) => 1.0,
                crate::dom::NodeType::Text(_) => 3.0,
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
            // 문자열 속성 반사 (원문 그대로).
            "alt" | "title" | "name" | "type" | "rel" | "target" | "placeholder" | "method"
            | "lang" | "dir" => match &dom.get(id).node_type {
                crate::dom::NodeType::Element(e) => {
                    Ok(Value::Str(e.attributes.get(key).cloned().unwrap_or_default()))
                }
                _ => Ok(Value::Undefined),
            },
            _ => Ok(Value::Undefined),
        }
    }

    pub(super) fn dom_set(&mut self, id: crate::dom::NodeId, key: &str, value: Value) -> Result<(), String> {
        // el.onclick = fn → 핸들러 등록
        if let Some(event) = key.strip_prefix("on") {
            if matches!(value, Value::Fn(_)) {
                self.handlers.push((id, event.to_string(), value));
            }
            return Ok(());
        }
        let text = to_display(&value);
        let dom = self.dom_arena()?;
        match key {
            "textContent" | "innerText" => {
                dom.set_text_content(id, text);
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
            _ => Ok(()), // 미지원 프로퍼티는 조용히 무시 (관용)
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
fn kebab_to_camel(s: &str) -> String {
    let mut out = String::new();
    let mut upper = false;
    for c in s.chars() {
        if c == '-' {
            upper = true;
        } else if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}
