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
pub fn run_scripts(dom: &mut crate::dom::Dom, page_url: &str) -> interp::Interp {
    let mut it = interp::Interp::new();
    it.install_location(page_url);
    let base = crate::url::Url::parse(page_url).ok();
    let mut sources = Vec::new();
    collect_scripts(dom, dom.root, base.as_ref(), &mut sources);
    if sources.is_empty() {
        return it;
    }
    it.dom = Some(dom as *mut crate::dom::Dom); // 실행 동안만 유효
    for src in &sources {
        let code = src.strip_prefix(EXT_TAG).unwrap_or(src);
        if let Err(e) = it.run(code) {
            println!("[js error] {}", e);
        }
        it.drain_microtasks(); // Promise .then 콜백 실행 (fetch 등)
        for line in it.console.drain(..) {
            println!("[console] {}", line);
        }
    }
    it.dom = None;
    it
}

// 상한: 외부 스크립트 네트워크 요청 폭주 방지 (실사이트는 수십 개까지 흔함).
const MAX_EXTERNAL_SCRIPTS: usize = 30;

fn collect_scripts(
    dom: &crate::dom::Dom,
    id: crate::dom::NodeId,
    base: Option<&crate::url::Url>,
    out: &mut Vec<String>,
) {
    let node = dom.get(id);
    if let crate::dom::NodeType::Element(e) = &node.node_type {
        if e.tag_name == "noscript" {
            return; // JS 실행 브라우저: noscript 내용 무시
        }
        if e.tag_name == "script" {
            // type 이 JS 일 때만 실행. application/json(데이터 임베드),
            // ld+json, text/template 등은 실행 대상이 아니다 (github 등).
            // module 은 import/export 미지원이라 스킵 (관용).
            let ty = e
                .attributes
                .get("type")
                .map(|s| s.trim().to_ascii_lowercase())
                .unwrap_or_default();
            let is_js =
                ty.is_empty() || ty == "text/javascript" || ty == "application/javascript";
            let src = e.attributes.get("src").map(|s| s.trim()).filter(|s| !s.is_empty());
            if is_js {
                match src {
                    // 외부 스크립트: 문서 순서대로 fetch 해 인라인처럼 실행.
                    // 클래식 스크립트 의미론(동기 실행). async/defer 구분은 생략.
                    Some(href) => {
                        if let Some(b) = base {
                            let n_ext = out.iter().filter(|s| s.starts_with(EXT_TAG)).count();
                            if n_ext < MAX_EXTERNAL_SCRIPTS {
                                if let Some(u) = b.join(href) {
                                    if let Ok(r) = crate::http::fetch(&u.as_string()) {
                                        let text =
                                            String::from_utf8_lossy(&r.body).to_string();
                                        if !text.trim().is_empty() {
                                            // 외부임을 표시(카운트용) — 앞의 마커는 실행 전 제거.
                                            out.push(format!("{}{}", EXT_TAG, text));
                                        }
                                    }
                                }
                            }
                        }
                    }
                    None => {
                        let text = dom.text_content(id);
                        if !text.trim().is_empty() {
                            out.push(text);
                        }
                    }
                }
            }
        }
    }
    for &c in &node.children {
        collect_scripts(dom, c, base, out);
    }
}

// 외부 스크립트 소스를 카운트하기 위한 내부 마커. 실행 직전 제거한다.
const EXT_TAG: &str = "\u{0}ext\u{0}";

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
        run_scripts(&mut dom, "https://localhost/");
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
        run_scripts(&mut dom, "https://localhost/");
        assert_eq!(text_of_id(&dom, "t").unwrap(), "from first");
    }

    #[test]
    fn non_js_script_types_are_not_executed() {
        let mut dom = crate::html::parse_dom(
            "<p id=\"t\">keep</p>\
             <script type=\"application/json\">{\"locale\": \"en\"}</script>\
             <script type=\"text/template\"><div>tpl</div></script>\
             <script type=\"module\">import x from 'y';</script>\
             <script type=\"text/javascript\">document.getElementById('t').textContent = 'ran';</script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/");
        assert_eq!(text_of_id(&dom, "t").unwrap(), "ran", "JS 타입만 실행, JSON/템플릿/모듈 스킵");
    }

    #[test]
    fn script_error_does_not_stop_next_script() {
        let mut dom = crate::html::parse_dom(
            "<p id=\"t\">old</p>\
             <script>boom(</script>\
             <script>document.getElementById('t').textContent = 'survived';</script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/");
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
        run_scripts(&mut dom, "https://localhost/");
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
        run_scripts(&mut dom, "https://localhost/");
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
        run_scripts(&mut dom, "https://localhost/");
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
        run_scripts(&mut dom, "https://localhost/");
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
        run_scripts(&mut dom, "https://localhost/");
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
    fn query_selector_finds_by_id_class_tag_and_descendant() {
        let mut dom = crate::html::parse_dom(
            "<div class=\"card\"><p class=\"note\">inside</p></div>\
             <p class=\"note\">outside</p><p id=\"out\">x</p>\
             <script>\
             var a = document.querySelector('#out'); \
             var b = document.querySelector('.card .note'); \
             var c = document.querySelector('p'); \
             a.textContent = b.textContent + '/' + c.textContent;</script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/");
        // '.card .note' 는 안쪽만, 'p' 는 문서 순서 첫 p
        assert_eq!(text_of_id(&dom, "out").unwrap(), "inside/inside");
    }

    #[test]
    fn query_selector_all_returns_array_of_handles() {
        let mut dom = crate::html::parse_dom(
            "<p class=\"i\">a</p><p class=\"i\">b</p><p class=\"i\">c</p><p id=\"out\">x</p>\
             <script>var list = document.querySelectorAll('.i'); \
             var s = list.length + ':'; \
             for (var k = 0; k < list.length; k++) { s += list[k].textContent; } \
             document.getElementById('out').textContent = s;</script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/");
        assert_eq!(text_of_id(&dom, "out").unwrap(), "3:abc");
    }

    #[test]
    fn scoped_query_selector_searches_descendants_only() {
        let mut dom = crate::html::parse_dom(
            "<div id=\"box\"><span class=\"t\">in</span></div><span class=\"t\">out</span>\
             <p id=\"res\">x</p>\
             <script>var box = document.getElementById('box'); \
             var hit = box.querySelector('.t'); \
             var miss = box.querySelector('#box'); \
             document.getElementById('res').textContent = \
               hit.textContent + '/' + (miss === null ? 'null' : 'self');</script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/");
        assert_eq!(text_of_id(&dom, "res").unwrap(), "in/null", "자신은 제외, 자손만");
    }

    #[test]
    fn unsupported_selector_is_tolerant() {
        let mut dom = crate::html::parse_dom(
            "<p id=\"out\">x</p>\
             <script>var a = document.querySelector('p:hover'); \
             var b = document.querySelectorAll('a > b'); \
             document.getElementById('out').textContent = \
               (a === null ? 'null' : 'oops') + '/' + b.length;</script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/");
        assert_eq!(text_of_id(&dom, "out").unwrap(), "null/0");
    }

    #[test]
    fn input_value_binding() {
        let mut dom = crate::html::parse_dom(
            "<input id=\"f\" value=\"initial\"><p id=\"out\">x</p>\
             <script>var f = document.getElementById('f'); \
             var was = f.value; f.value = 'changed'; \
             document.getElementById('out').textContent = was + '/' + f.value;</script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/");
        assert_eq!(text_of_id(&dom, "out").unwrap(), "initial/changed");
        // 속성에도 반영 (레이아웃/제출이 읽는 단일 진실)
        let f = dom.find_by_attr_id("f").unwrap();
        match &dom.get(f).node_type {
            crate::dom::NodeType::Element(e) => {
                assert_eq!(e.attributes.get("value").map(|s| s.as_str()), Some("changed"));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn promise_then_runs_via_microtask() {
        let mut dom = crate::html::parse_dom(
            "<p id=\"out\">x</p>\
             <script>Promise.resolve(5).then(function(v){ \
               document.getElementById('out').textContent = 'got ' + v; });</script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/");
        assert_eq!(text_of_id(&dom, "out").unwrap(), "got 5");
    }

    #[test]
    fn promise_then_chains_results() {
        let mut dom = crate::html::parse_dom(
            "<p id=\"out\">x</p>\
             <script>Promise.resolve(2)\
               .then(function(v){ return v * 10; })\
               .then(function(v){ document.getElementById('out').textContent = 'r' + v; });</script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/");
        assert_eq!(text_of_id(&dom, "out").unwrap(), "r20", "체인: 2→20");
    }

    #[test]
    fn promise_then_runs_after_sync_code() {
        // .then 콜백은 마이크로태스크라 동기 코드 뒤에 실행 (순서 보장)
        let mut dom = crate::html::parse_dom(
            "<p id=\"out\">x</p>\
             <script>var log=''; Promise.resolve().then(function(){ log+='B'; \
               document.getElementById('out').textContent = log; }); log+='A';</script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/");
        assert_eq!(text_of_id(&dom, "out").unwrap(), "AB", "동기 A 먼저, 마이크로태스크 B 나중");
    }

    #[test]
    fn async_await_unwraps_and_chains() {
        let mut dom = crate::html::parse_dom(
            "<p id=\"out\">x</p>\
             <script>\
             async function run() { \
               var a = await Promise.resolve(2); \
               var b = await Promise.resolve(a + 3); \
               document.getElementById('out').textContent = 'r' + b; \
             } \
             run();</script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/");
        assert_eq!(text_of_id(&dom, "out").unwrap(), "r5", "await 로 순차 언랩: 2→5");
    }

    #[test]
    fn async_arrow_parses_and_awaits() {
        let mut dom = crate::html::parse_dom(
            "<p id=\"out\">x</p>\
             <script>var f = async (n) => { \
               document.getElementById('out').textContent = 'got' + (await Promise.resolve(n)); \
             }; f(7);</script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/");
        assert_eq!(text_of_id(&dom, "out").unwrap(), "got7");
    }

    #[test]
    fn text_content_reads_existing_text() {
        let mut dom = crate::html::parse_dom(
            "<p id=\"a\">left</p><p id=\"b\">right</p>\
             <script>var a = document.getElementById('a'); \
             a.textContent = a.textContent + '+' + document.getElementById('b').textContent;</script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/");
        assert_eq!(text_of_id(&dom, "a").unwrap(), "left+right");
    }
}
