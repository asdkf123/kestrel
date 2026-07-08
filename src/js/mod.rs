// Kestrel JavaScript 엔진 (M4a): 렉서 → 파서 → 트리 워킹 인터프리터.
// 스펙: docs/superpowers/specs/2026-07-07-m4a-js-engine-design.md

pub mod ast;
pub mod interp;
pub mod lexer;
pub mod parser;

// 인라인 <script> 를 문서 순서로 실행한다 (렌더 전, DOM 변형 가능).
// 실제 브라우저처럼 전역 환경은 페이지의 모든 스크립트가 공유한다.
// 에러는 해당 스크립트만 중단하고 보고 (관용 원칙). 외부 src 는 미지원.
// 반환된 Interp 는 페이지가 보관한다 — 등록된 이벤트 핸들러(클로저 포함)가 살아있음.
pub fn run_scripts(dom: &mut crate::dom::Dom) -> interp::Interp {
    let mut it = interp::Interp::new();
    let mut sources = Vec::new();
    collect_scripts(dom, dom.root, &mut sources);
    if sources.is_empty() {
        return it;
    }
    it.dom = Some(dom as *mut crate::dom::Dom); // 실행 동안만 유효
    for src in &sources {
        if let Err(e) = it.run(src) {
            println!("[js error] {}", e);
        }
        for line in it.console.drain(..) {
            println!("[console] {}", line);
        }
    }
    it.dom = None;
    it
}

fn collect_scripts(dom: &crate::dom::Dom, id: crate::dom::NodeId, out: &mut Vec<String>) {
    let node = dom.get(id);
    if let crate::dom::NodeType::Element(e) = &node.node_type {
        if e.tag_name == "script" && e.attributes.get("src").map_or(true, |s| s.is_empty()) {
            let text = dom.text_content(id);
            if !text.trim().is_empty() {
                out.push(text);
            }
        }
    }
    for &c in &node.children {
        collect_scripts(dom, c, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::Dom;

    fn text_of_id(dom: &Dom, id: &str) -> Option<String> {
        dom.find_by_attr_id(id).map(|n| dom.text_content(n))
    }

    #[test]
    fn script_mutates_dom_text() {
        let mut dom = crate::html::parse_dom(
            "<p id=\"t\">old</p>\
             <script>document.getElementById('t').textContent = 'new ' + (1 + 2);</script>"
                .to_string(),
        );
        run_scripts(&mut dom);
        assert_eq!(text_of_id(&dom, "t").unwrap(), "new 3");
    }

    #[test]
    fn scripts_share_globals_in_document_order() {
        let mut dom = crate::html::parse_dom(
            "<p id=\"t\">old</p>\
             <script>var msg = 'from first';</script>\
             <script>document.getElementById('t').textContent = msg;</script>"
                .to_string(),
        );
        run_scripts(&mut dom);
        assert_eq!(text_of_id(&dom, "t").unwrap(), "from first");
    }

    #[test]
    fn script_error_does_not_stop_next_script() {
        let mut dom = crate::html::parse_dom(
            "<p id=\"t\">old</p>\
             <script>boom(</script>\
             <script>document.getElementById('t').textContent = 'survived';</script>"
                .to_string(),
        );
        run_scripts(&mut dom);
        assert_eq!(text_of_id(&dom, "t").unwrap(), "survived");
    }

    #[test]
    fn get_element_by_id_missing_returns_null() {
        let mut dom = crate::html::parse_dom(
            "<p id=\"t\">keep</p>\
             <script>var el = document.getElementById('nope'); \
             if (el === null) { document.getElementById('t').textContent = 'was null'; }</script>"
                .to_string(),
        );
        run_scripts(&mut dom);
        assert_eq!(text_of_id(&dom, "t").unwrap(), "was null");
    }

    #[test]
    fn create_element_and_append_child_build_structure() {
        let mut dom = crate::html::parse_dom(
            "<ul id=\"list\"></ul>\
             <script>var ul = document.getElementById('list'); \
             for (var i = 1; i <= 3; i++) { \
               var li = document.createElement('li'); \
               li.textContent = 'item ' + i; \
               ul.appendChild(li); \
             }</script>"
                .to_string(),
        );
        run_scripts(&mut dom);
        let ul = dom.find_by_attr_id("list").unwrap();
        assert_eq!(dom.get(ul).children.len(), 3);
        assert_eq!(dom.text_content(ul), "item 1item 2item 3");
    }

    #[test]
    fn remove_and_handle_stability_across_structure_changes() {
        // 아레나의 존재 이유: 앞 형제를 제거해도 기존 핸들이 같은 노드를 가리킨다
        let mut dom = crate::html::parse_dom(
            "<p id=\"a\">first</p><p id=\"b\">second</p>\
             <script>var b = document.getElementById('b'); \
             document.getElementById('a').remove(); \
             b.textContent = 'still me';</script>"
                .to_string(),
        );
        run_scripts(&mut dom);
        assert!(dom.find_by_attr_id("a").is_none(), "a 는 트리에서 제거됨");
        assert_eq!(text_of_id(&dom, "b").unwrap(), "still me");
    }

    #[test]
    fn set_get_attribute_roundtrip() {
        let mut dom = crate::html::parse_dom(
            "<div id=\"box\"></div><p id=\"out\">x</p>\
             <script>var el = document.getElementById('box'); \
             el.setAttribute('class', 'fancy'); \
             document.getElementById('out').textContent = el.getAttribute('class') + '/' \
               + (el.getAttribute('nope') === null ? 'null' : 'oops');</script>"
                .to_string(),
        );
        run_scripts(&mut dom);
        assert_eq!(text_of_id(&dom, "out").unwrap(), "fancy/null");
    }

    #[test]
    fn inner_html_replaces_children_with_parsed_fragment() {
        let mut dom = crate::html::parse_dom(
            "<div id=\"box\">old</div>\
             <script>document.getElementById('box').innerHTML = \
               '<p>one</p><p class=\"hi\">two <b>bold</b></p>';</script>"
                .to_string(),
        );
        run_scripts(&mut dom);
        let boxid = dom.find_by_attr_id("box").unwrap();
        assert_eq!(dom.get(boxid).children.len(), 2, "다중 루트 조각");
        assert_eq!(dom.text_content(boxid), "onetwo bold");
        // 파싱된 요소가 진짜 요소인지 (텍스트가 아니라)
        let second = dom.get(boxid).children[1];
        match &dom.get(second).node_type {
            crate::dom::NodeType::Element(e) => {
                assert_eq!(e.tag_name, "p");
                assert_eq!(e.attributes.get("class").map(|s| s.as_str()), Some("hi"));
            }
            other => panic!("expected element, got {:?}", other),
        }
    }

    #[test]
    fn text_content_reads_existing_text() {
        let mut dom = crate::html::parse_dom(
            "<p id=\"a\">left</p><p id=\"b\">right</p>\
             <script>var a = document.getElementById('a'); \
             a.textContent = a.textContent + '+' + document.getElementById('b').textContent;</script>"
                .to_string(),
        );
        run_scripts(&mut dom);
        assert_eq!(text_of_id(&dom, "a").unwrap(), "left+right");
    }
}
