// Kestrel JavaScript 엔진 (M4a): 렉서 → 파서 → 트리 워킹 인터프리터.
// 스펙: docs/superpowers/specs/2026-07-07-m4a-js-engine-design.md

pub mod ast;
pub mod interp;
pub mod lexer;
pub mod parser;

// 인라인 <script> 를 문서 순서로 실행한다 (렌더 전, DOM 변형 가능).
// 실제 브라우저처럼 전역 환경은 페이지의 모든 스크립트가 공유한다.
// 에러는 해당 스크립트만 중단하고 보고 (관용 원칙). 외부 src 는 M4a 미지원.
pub fn run_scripts(dom: &mut crate::dom::Node) {
    let mut sources = Vec::new();
    collect_scripts(dom, &mut sources);
    if sources.is_empty() {
        return;
    }
    let mut it = interp::Interp::new();
    it.dom = Some(dom as *mut crate::dom::Node); // 실행 동안만 유효
    for src in &sources {
        if let Err(e) = it.run(src) {
            println!("[js error] {}", e);
        }
        for line in it.console.drain(..) {
            println!("[console] {}", line);
        }
    }
    it.dom = None;
}

fn collect_scripts(node: &crate::dom::Node, out: &mut Vec<String>) {
    if let crate::dom::NodeType::Element(e) = &node.node_type {
        if e.tag_name == "script" && e.attributes.get("src").map_or(true, |s| s.is_empty()) {
            let mut text = String::new();
            for c in &node.children {
                if let crate::dom::NodeType::Text(t) = &c.node_type {
                    text.push_str(t);
                }
            }
            if !text.trim().is_empty() {
                out.push(text);
            }
        }
    }
    for c in &node.children {
        collect_scripts(c, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::{Node, NodeType};

    fn text_of_id<'a>(node: &'a Node, id: &str) -> Option<String> {
        if let NodeType::Element(e) = &node.node_type {
            if e.attributes.get("id").map(|s| s.as_str()) == Some(id) {
                let mut s = String::new();
                fn collect(n: &Node, out: &mut String) {
                    if let NodeType::Text(t) = &n.node_type {
                        out.push_str(t);
                    }
                    for c in &n.children {
                        collect(c, out);
                    }
                }
                collect(node, &mut s);
                return Some(s);
            }
        }
        node.children.iter().find_map(|c| text_of_id(c, id))
    }

    #[test]
    fn script_mutates_dom_text() {
        let mut dom = crate::html::parse(
            "<p id=\"t\">old</p>\
             <script>document.getElementById('t').textContent = 'new ' + (1 + 2);</script>"
                .to_string(),
        );
        run_scripts(&mut dom);
        assert_eq!(text_of_id(&dom, "t").unwrap(), "new 3");
    }

    #[test]
    fn scripts_share_globals_in_document_order() {
        let mut dom = crate::html::parse(
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
        let mut dom = crate::html::parse(
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
        let mut dom = crate::html::parse(
            "<p id=\"t\">keep</p>\
             <script>var el = document.getElementById('nope'); \
             if (el === null) { document.getElementById('t').textContent = 'was null'; }</script>"
                .to_string(),
        );
        run_scripts(&mut dom);
        assert_eq!(text_of_id(&dom, "t").unwrap(), "was null");
    }

    #[test]
    fn text_content_reads_existing_text() {
        let mut dom = crate::html::parse(
            "<p id=\"a\">left</p><p id=\"b\">right</p>\
             <script>var a = document.getElementById('a'); \
             a.textContent = a.textContent + '+' + document.getElementById('b').textContent;</script>"
                .to_string(),
        );
        run_scripts(&mut dom);
        assert_eq!(text_of_id(&dom, "a").unwrap(), "left+right");
    }
}
