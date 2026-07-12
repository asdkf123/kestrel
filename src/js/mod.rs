// Kestrel JavaScript 엔진 (M4a): 렉서 → 파서 → 트리 워킹 인터프리터.
// 스펙: docs/superpowers/specs/2026-07-07-m4a-js-engine-design.md

pub mod ast;
pub mod interp;
pub mod lexer;
pub mod parser;
pub mod regex;

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
    // 폴리필 프렐류드(Symbol, Object.entries, 옵저버 스텁 등) — 표준 전역을 채운다.
    // webpack 등 번들 런타임은 사이트가 직접 싣고 있으므로 우리가 주입하지 않는다
    // (배열이 객체라 push 재정의가 되므로 사이트 자체 런타임이 그대로 동작).
    sources.insert(0, JS_PRELUDE.to_string());
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
    // 모든 스크립트 실행 후 문서/윈도우 이벤트 발화: 프레임워크가 여기서
    // 콘텐츠를 구성한다(DOMContentLoaded → load 순). dom 포인터는 아직 유효.
    if std::env::var("KESTREL_JS_DEBUG").is_ok() {
        let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for (t, _) in &it.global_handlers {
            *counts.entry(t.as_str()).or_default() += 1;
        }
        eprintln!("[js debug] 전역 핸들러 {}개: {:?}", it.global_handlers.len(), counts);
        eprintln!("[js debug] 요소 핸들러 {}개, 타이머 {}개", it.handlers.len(), it.timers.len());
        if !it.lenient_hits.is_empty() {
            let mut hits: Vec<_> = it.lenient_hits.iter().collect();
            hits.sort_by(|a, b| b.1.cmp(a.1));
            eprintln!("[js debug] 관대 모드 히트 상위:");
            for (k, n) in hits.iter().take(25) {
                eprintln!("    {:>6}  {}", n, k);
            }
        }
    }
    it.set_ready_state("interactive");
    it.fire_global("DOMContentLoaded");
    it.set_ready_state("complete");
    it.fire_global("load");
    for line in it.console.drain(..) {
        println!("[console] {}", line);
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

// 폴리필 프렐류드: 프레임워크(React 등)가 기대하는 공통 전역/메서드를 채운다.
// 순수 JS 로 엔진의 기존 기능만 사용. 최상위 var 선언이라 진짜 전역이 된다.
const JS_PRELUDE: &str = r#"
var __kNoop = function(){};
var __kObs = function(){ return { observe: __kNoop, unobserve: __kNoop, disconnect: __kNoop, takeRecords: function(){ return []; } }; };
var Symbol = window.Symbol;
if (!Symbol) {
  var __symCount = 0;
  // 심볼은 고유 __key 문자열을 갖고, 계산된 멤버 접근에서 그 키로 매핑된다.
  // 잘 알려진 심볼(iterator 등)은 고정 키 → 배열/문자열 반복자와 연결.
  Symbol = function(d){ __symCount++; return { __isSymbol: true, description: d, __key: '@@s:' + (d === undefined ? '' : d) + ':' + __symCount }; };
  Symbol.iterator = { __isSymbol: true, description: 'Symbol.iterator', __key: '@@iterator' };
  Symbol.asyncIterator = { __isSymbol: true, description: 'Symbol.asyncIterator', __key: '@@asyncIterator' };
  Symbol.toStringTag = { __isSymbol: true, description: 'Symbol.toStringTag', __key: '@@toStringTag' };
  Symbol.hasInstance = { __isSymbol: true, description: 'Symbol.hasInstance', __key: '@@hasInstance' };
  Symbol.__reg = {};
  Symbol.for = function(k){ if (!Symbol.__reg[k]) Symbol.__reg[k] = { __isSymbol: true, description: k, __key: '@@for:' + k }; return Symbol.__reg[k]; };
  Symbol.keyFor = function(){ return undefined; };
  window.Symbol = Symbol;
}
if (!Object.entries) Object.entries = function(o){ var k = Object.keys(o || {}), r = []; for (var i = 0; i < k.length; i++) r.push([k[i], o[k[i]]]); return r; };
if (!Object.values) Object.values = function(o){ var k = Object.keys(o || {}), r = []; for (var i = 0; i < k.length; i++) r.push(o[k[i]]); return r; };
if (!Object.getOwnPropertyNames) Object.getOwnPropertyNames = function(o){ return Object.keys(o || {}); };
if (!Object.getOwnPropertySymbols) Object.getOwnPropertySymbols = function(){ return []; };
if (!Object.getOwnPropertyDescriptor) Object.getOwnPropertyDescriptor = function(o, k){ if (o && Object.prototype.hasOwnProperty.call(o, k)) return { value: o[k], writable: true, enumerable: true, configurable: true }; return undefined; };
if (!Object.setPrototypeOf) Object.setPrototypeOf = function(o){ return o; };
if (!Array.from) Array.from = function(x, fn){ var r = []; if (x === null || x === undefined) return r; var i = 0; if (typeof x.length === 'number') { for (i = 0; i < x.length; i++) r.push(fn ? fn(x[i], i) : x[i]); return r; } for (var v of x) { r.push(fn ? fn(v, i) : v); i++; } return r; };
if (!Object.fromEntries) Object.fromEntries = function(e){ var r = {}; for (var p of e) { r[p[0]] = p[1]; } return r; };
if (!Array.prototype.at) Array.prototype.at = function(i){ i = i < 0 ? this.length + i : i; return this[i]; };
if (!String.prototype.at) String.prototype.at = function(i){ i = i < 0 ? this.length + i : i; return this.charAt(i); };
if (!Array.of) Array.of = function(...a){ return a; };
if (!Array.prototype.entries) Array.prototype.entries = function(){ var r = []; for (var i = 0; i < this.length; i++) r.push([i, this[i]]); return r; };
if (!Array.prototype.fill) Array.prototype.fill = function(v, s, e){ s = s === undefined ? 0 : (s < 0 ? this.length + s : s); e = e === undefined ? this.length : (e < 0 ? this.length + e : e); for (var i = s; i < e; i++) this[i] = v; return this; };
if (!Array.prototype.flatMap) Array.prototype.flatMap = function(fn){ return this.map(fn).flat(); };
if (!Array.prototype.reduceRight) Array.prototype.reduceRight = function(fn){ var i = this.length - 1, acc; if (arguments.length > 1) { acc = arguments[1]; } else { acc = this[i--]; } for (; i >= 0; i--) acc = fn(acc, this[i], i, this); return acc; };
if (!Array.prototype.lastIndexOf) Array.prototype.lastIndexOf = function(x){ for (var i = this.length - 1; i >= 0; i--) if (this[i] === x) return i; return -1; };
if (!Array.prototype.toReversed) Array.prototype.toReversed = function(){ return this.slice().reverse(); };
if (!Array.prototype.toSorted) Array.prototype.toSorted = function(fn){ return this.slice().sort(fn); };
if (!Array.prototype.toLocaleString) Array.prototype.toLocaleString = function(){ return this.join(','); };
if (!Array.prototype.copyWithin) Array.prototype.copyWithin = function(t, s, e){ var len = this.length; t = t < 0 ? len + t : t; s = s === undefined ? 0 : (s < 0 ? len + s : s); e = e === undefined ? len : (e < 0 ? len + e : e); var tmp = this.slice(s, e); for (var i = 0; i < tmp.length && t + i < len; i++) this[t + i] = tmp[i]; return this; };
if (!String.prototype.localeCompare) String.prototype.localeCompare = function(o){ o = String(o); return this < o ? -1 : (this > o ? 1 : 0); };
if (!String.prototype.normalize) String.prototype.normalize = function(){ return String(this); };
if (!Object.hasOwn) Object.hasOwn = function(o, k){ return Object.prototype.hasOwnProperty.call(o, k); };
if (!Object.is) Object.is = function(a, b){ if (a === b) return a !== 0 || 1 / a === 1 / b; return a !== a && b !== b; };
if (!Object.getOwnPropertyDescriptors) Object.getOwnPropertyDescriptors = function(o){ var r = {}, k = Object.keys(o || {}); for (var i = 0; i < k.length; i++) r[k[i]] = Object.getOwnPropertyDescriptor(o, k[i]); return r; };
if (!Number.prototype.toExponential) Number.prototype.toExponential = function(){ return String(this); };
if (!Array.prototype.findLast) Array.prototype.findLast = function(fn){ for (var i = this.length - 1; i >= 0; i--) if (fn(this[i], i)) return this[i]; return undefined; };
if (!Array.prototype.findLastIndex) Array.prototype.findLastIndex = function(fn){ for (var i = this.length - 1; i >= 0; i--) if (fn(this[i], i)) return i; return -1; };
if (typeof console !== 'undefined') { var __klg = console.log; if (!console.warn) console.warn = __klg; if (!console.error) console.error = __klg; if (!console.info) console.info = __klg; if (!console.debug) console.debug = __klg; if (!console.trace) console.trace = __klg; if (!console.group) console.group = __kNoop; if (!console.groupEnd) console.groupEnd = __kNoop; if (!console.groupCollapsed) console.groupCollapsed = __kNoop; if (!console.table) console.table = __klg; if (!console.dir) console.dir = __klg; if (!console.assert) console.assert = __kNoop; if (!console.count) console.count = __kNoop; if (!console.time) console.time = __kNoop; if (!console.timeEnd) console.timeEnd = __kNoop; }
var MutationObserver = window.MutationObserver; if (!MutationObserver) { MutationObserver = __kObs; window.MutationObserver = MutationObserver; }
var IntersectionObserver = window.IntersectionObserver; if (!IntersectionObserver) { IntersectionObserver = __kObs; window.IntersectionObserver = IntersectionObserver; }
var ResizeObserver = window.ResizeObserver; if (!ResizeObserver) { ResizeObserver = __kObs; window.ResizeObserver = ResizeObserver; }
var PerformanceObserver = window.PerformanceObserver; if (!PerformanceObserver) { PerformanceObserver = __kObs; window.PerformanceObserver = PerformanceObserver; }
if (!window.matchMedia) window.matchMedia = function(q){ return { matches: false, media: q || '', onchange: null, addListener: __kNoop, removeListener: __kNoop, addEventListener: __kNoop, removeEventListener: __kNoop, dispatchEvent: function(){ return false; } }; };
var matchMedia = window.matchMedia;
if (!window.requestIdleCallback) window.requestIdleCallback = function(cb){ return setTimeout(function(){ cb({ didTimeout: false, timeRemaining: function(){ return 0; } }); }, 1); };
var requestIdleCallback = window.requestIdleCallback;
if (!window.cancelIdleCallback) window.cancelIdleCallback = function(id){ clearTimeout(id); };
if (!window.cancelAnimationFrame) window.cancelAnimationFrame = function(id){ clearTimeout(id); };
var cancelAnimationFrame = window.cancelAnimationFrame;
if (!window.getComputedStyle) window.getComputedStyle = function(){ return { getPropertyValue: function(){ return ''; } }; };
var getComputedStyle = window.getComputedStyle;
var Reflect = window.Reflect;
if (!Reflect) {
  Reflect = {};
  Reflect.get = function(t, k){ return t[k]; };
  Reflect.set = function(t, k, v){ t[k] = v; return true; };
  Reflect.has = function(t, k){ return k in t; };
  Reflect.deleteProperty = function(t, k){ delete t[k]; return true; };
  Reflect.ownKeys = function(t){ return Object.keys(t || {}); };
  Reflect.getPrototypeOf = function(){ return null; };
  Reflect.setPrototypeOf = function(){ return true; };
  Reflect.defineProperty = function(t, k, d){ Object.defineProperty(t, k, d); return true; };
  Reflect.getOwnPropertyDescriptor = function(t, k){ return Object.getOwnPropertyDescriptor(t, k); };
  Reflect.apply = function(fn, thisArg, args){ return fn.apply(thisArg, args); };
  Reflect.construct = function(fn, args){ return new (Function.prototype.bind.apply(fn, [null].concat(args || [])))(); };
  window.Reflect = Reflect;
}
"#;


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
    fn uri_encode_decode_globals() {
        // encodeURIComponent 는 예약문자/공백/비ASCII 를 %XX(UTF-8)로, 왕복 복원.
        let mut dom = crate::html::parse_dom(
            "<p id=\"t\">x</p>\
             <script>var e = encodeURIComponent('a b/c?d=\u{d55c}'); \
             document.getElementById('t').textContent = e + '|' + decodeURIComponent(e);</script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/");
        assert_eq!(
            text_of_id(&dom, "t").unwrap(),
            "a%20b%2Fc%3Fd%3D%ED%95%9C|a b/c?d=\u{d55c}"
        );
    }

    #[test]
    fn encode_uri_preserves_reserved() {
        // encodeURI 는 예약문자(/ ? : = &)를 보존, 공백만 %20.
        let mut dom = crate::html::parse_dom(
            "<p id=\"t\">x</p>\
             <script>document.getElementById('t').textContent = \
             encodeURI('http://x.com/a b?c=1&d=2');</script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/");
        assert_eq!(text_of_id(&dom, "t").unwrap(), "http://x.com/a%20b?c=1&d=2");
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
    fn extended_builtins_via_prelude() {
        // Array.of / String.prototype.at(원시) / Array.prototype.fill 폴리필
        let mut dom = crate::html::parse_dom(
            "<p id=\"out\">x</p><script>document.getElementById('out').textContent = \
             Array.of(1,2).join('') + '/' + 'ab'.at(-1) + '/' + [9,9].fill(5).join('');</script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/");
        assert_eq!(text_of_id(&dom, "out").unwrap(), "12/b/55");
    }

    #[test]
    fn element_dataset_reads_data_attributes() {
        // element.dataset.userName → data-user-name (camelCase 변환)
        let mut dom = crate::html::parse_dom(
            "<div id=\"a\" data-count=\"5\" data-user-name=\"ada\"></div>\
             <p id=\"out\">x</p>\
             <script>var a=document.getElementById('a'); \
               document.getElementById('out').textContent = a.dataset.count + '/' + a.dataset.userName;</script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/");
        assert_eq!(text_of_id(&dom, "out").unwrap(), "5/ada");
    }

    #[test]
    fn promise_all_resolves_with_values() {
        let mut dom = crate::html::parse_dom(
            "<p id=\"out\">x</p>\
             <script>Promise.all([Promise.resolve(1),Promise.resolve(2),3]).then(function(a){ \
               document.getElementById('out').textContent = a.join('-'); });</script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/");
        assert_eq!(text_of_id(&dom, "out").unwrap(), "1-2-3");
    }

    #[test]
    fn async_function_returns_awaitable_promise() {
        // async 함수는 이행된 Promise 를 반환, await 로 언랩되고 .then 으로 체이닝됨
        let mut dom = crate::html::parse_dom(
            "<p id=\"out\">x</p>\
             <script>async function f(){ return await Promise.resolve(9); } \
               f().then(function(v){ document.getElementById('out').textContent = 'v' + v; });</script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/");
        assert_eq!(text_of_id(&dom, "out").unwrap(), "v9");
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
