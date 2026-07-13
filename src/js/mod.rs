// Kestrel JavaScript 엔진 (M4a): 렉서 → 파서 → 트리 워킹 인터프리터.
// 스펙: docs/superpowers/specs/2026-07-07-m4a-js-engine-design.md

pub mod ast;
pub mod bigint;
pub mod interp;
pub mod lexer;
pub mod parser;
pub mod regex;

// 인라인 <script> 를 문서 순서로 실행한다 (렌더 전, DOM 변형 가능).
// 실제 브라우저처럼 전역 환경은 페이지의 모든 스크립트가 공유한다.
// 에러는 해당 스크립트만 중단하고 보고 (관용 원칙). 외부 src 는 미지원.
// 반환된 Interp 는 페이지가 보관한다 — 등록된 이벤트 핸들러(클로저 포함)가 살아있음.
// 문서의 스크립트를 순서대로 실행한다.
// layout_ctx: 스크립트가 측정 API 를 읽을 때 강제 레이아웃에 쓸 입력(스타일시트/폰트/이미지).
// HTML 표준에서 파서가 삽입한 스크립트는 보류된 스타일시트를 기다린 뒤 실행된다 —
// 그래서 스크립트 시점에 CSS 가 이미 있어야 하고, 측정도 실제 값이 나와야 한다.
pub fn run_scripts(
    dom: &mut crate::dom::Dom,
    page_url: &str,
    layout_ctx: Option<crate::window::LayoutCtx>,
) -> interp::Interp {
    run_scripts_with_base(dom, page_url, page_url, layout_ctx)
}

// page_url: location.href 가 되는 문서 URL.
// base_url: 상대 URL 해석 기준 (<base href> 가 있으면 그것). 표준에서 이 둘은 다를 수 있다.
pub fn run_scripts_with_base(
    dom: &mut crate::dom::Dom,
    page_url: &str,
    base_url: &str,
    layout_ctx: Option<crate::window::LayoutCtx>,
) -> interp::Interp {
    let mut it = interp::Interp::new();
    it.install_location(page_url);
    it.set_base_url(base_url);
    it.layout_ctx = layout_ctx;
    // 외부 스크립트 src 도 문서의 기준 URL(<base href>)로 해석한다
    let base = crate::url::Url::parse(base_url).ok();
    let mut sources = Vec::new();
    collect_scripts(dom, dom.root, base.as_ref(), &mut sources);
    // 모듈 스크립트도 확인한다 — 클래식 스크립트가 없어도 모듈만 있는 페이지가 있다
    // (예전엔 여기서 일찍 반환해 모듈이 통째로 안 돌았다).
    let mut modules: Vec<(String, Option<String>)> = Vec::new();
    collect_modules(dom, dom.root, base.as_ref(), &mut modules);
    // 임포트 맵 (<script type="importmap">): 베어 명세자("react")를 URL 로 해석하는
    // **표준 메커니즘**이다 (HTML §4.12.5). 없으면 베어 명세자는 해석 불가로 실패한다.
    let mut import_map: Vec<(String, String)> = Vec::new();
    collect_import_map(dom, dom.root, base.as_ref(), &mut import_map);
    it.import_map = import_map;
    if sources.is_empty() && modules.is_empty() {
        return it;
    }
    // 폴리필 프렐류드(Symbol, Object.entries, 옵저버 스텁 등) — 표준 전역을 채운다.
    // webpack 등 번들 런타임은 사이트가 직접 싣고 있으므로 우리가 주입하지 않는다
    // (배열이 객체라 push 재정의가 되므로 사이트 자체 런타임이 그대로 동작).
    sources.insert(0, (None, JS_PRELUDE.to_string()));
    it.dom = Some(dom as *mut crate::dom::Dom); // 실행 동안만 유효
    for (node, src) in &sources {
        let code = src.strip_prefix(EXT_TAG).unwrap_or(src);
        // document.write 의 삽입 지점 = 지금 실행 중인 스크립트 자리 (파서 삽입 지점)
        it.current_script = *node;
        if let Err(e) = it.run(code) {
            println!("[js error] {}", e);
        }
        it.drain_microtasks();
        // document.write 로 새로 생긴 스크립트를 문서 순서대로 실행한다 (동기 의미론).
        run_written_scripts(&mut it, base.as_ref(), 0);
        for line in it.console.drain(..) {
            println!("[console] {}", line);
        }
    }
    it.current_script = None;
    // ── ES 모듈 (type=module) ──
    // 클래식 스크립트가 끝난 뒤 실행한다 (모듈은 defer 의미론).
    if !modules.is_empty() {
        for (url, inline) in &modules {
            load_module_graph(&mut it, url, inline.clone(), 0);
        }
        println!("[module] {}개 진입점, 그래프 {}개 모듈", modules.len(), it.module_sources.len());
        for (url, _) in &modules {
            if let Err(e) = it.run_module(url) {
                println!("[js error] 모듈 {} — {}", url, e);
            }
            it.drain_microtasks();
            for line in it.console.drain(..) {
                println!("[console] {}", line);
            }
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

// <script type="module"> 수집: (모듈 URL, 소스). 인라인 모듈은 합성 URL 을 준다.
// <script type="importmap">{"imports": {"react": "https://...", "lib/": "/lib/"}}</script>
// 접두 매핑(끝이 /)과 정확 매핑 둘 다 지원한다. 긴 키가 우선한다 (표준).
fn collect_import_map(
    dom: &crate::dom::Dom,
    id: crate::dom::NodeId,
    base: Option<&crate::url::Url>,
    out: &mut Vec<(String, String)>,
) {
    let node = dom.get(id);
    if let crate::dom::NodeType::Element(e) = &node.node_type {
        if e.tag_name == "script"
            && e.attributes.get("type").map(|s| s.trim().to_ascii_lowercase()).as_deref()
                == Some("importmap")
        {
            let text = dom.text_content(id);
            for (spec, target) in parse_import_map(&text) {
                // 상대 URL 은 문서 기준 절대화
                let abs = match base {
                    Some(b) => b.join(&target).map(|u| u.as_string()).unwrap_or(target.clone()),
                    None => target.clone(),
                };
                out.push((spec, abs));
            }
            // 긴 키가 먼저 매칭돼야 한다 (표준: 가장 긴 접두 우선)
            out.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
            return;
        }
    }
    for &c in &node.children {
        collect_import_map(dom, c, base, out);
    }
}

// {"imports": {"a": "b", ...}} 의 imports 객체만 읽는다 (scopes 는 미지원 — 흔치 않다).
fn parse_import_map(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Some(i) = text.find("\"imports\"") else { return out };
    let rest = &text[i..];
    let Some(open) = rest.find('{') else { return out };
    let mut depth = 0usize;
    let mut end = open;
    for (k, ch) in rest[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = open + k;
                    break;
                }
            }
            _ => {}
        }
    }
    let body = &rest[open + 1..end];
    // "키": "값" 쌍을 순서대로 뽑는다
    let mut chars = body.char_indices().peekable();
    let mut strs: Vec<String> = Vec::new();
    while let Some((k, ch)) = chars.next() {
        if ch != '"' {
            continue;
        }
        let mut s = String::new();
        let bytes: Vec<char> = body.chars().collect();
        let start_idx = body[..k].chars().count() + 1;
        let mut idx = start_idx;
        while idx < bytes.len() && bytes[idx] != '"' {
            s.push(bytes[idx]);
            idx += 1;
        }
        strs.push(s);
        // 소비한 만큼 건너뛰기
        let j = body
            .char_indices()
            .nth(idx + 1)
            .map(|(b, _)| b)
            .unwrap_or(body.len());
        while let Some(&(b, _)) = chars.peek() {
            if b < j {
                chars.next();
            } else {
                break;
            }
        }
    }
    for pair in strs.chunks(2) {
        if pair.len() == 2 {
            out.push((pair[0].clone(), pair[1].clone()));
        }
    }
    out
}

fn collect_modules(
    dom: &crate::dom::Dom,
    id: crate::dom::NodeId,
    base: Option<&crate::url::Url>,
    out: &mut Vec<(String, Option<String>)>,
) {
    let node = dom.get(id);
    if let crate::dom::NodeType::Element(e) = &node.node_type {
        if e.tag_name == "noscript" {
            return;
        }
        if e.tag_name == "script" {
            let ty = e
                .attributes
                .get("type")
                .map(|s| s.trim().to_ascii_lowercase())
                .unwrap_or_default();
            if ty == "module" {
                match e.attributes.get("src").map(|s| s.trim()).filter(|s| !s.is_empty()) {
                    Some(href) => {
                        if let Some(b) = base {
                            if let Some(u) = b.join(href) {
                                out.push((u.as_string(), None));
                            }
                        }
                    }
                    None => {
                        let text = dom.text_content(id);
                        if !text.trim().is_empty() {
                            // 인라인 모듈: 문서 URL 기준으로 상대 import 를 풀 수 있게
                            // 문서 URL + 프래그먼트를 합성 식별자로 쓴다.
                            let u = base
                                .map(|b| b.as_string())
                                .unwrap_or_else(|| "about:inline".to_string());
                            out.push((format!("{}#inline-module-{}", u, out.len()), Some(text)));
                        }
                    }
                }
            }
        }
    }
    for &c in &node.children {
        collect_modules(dom, c, base, out);
    }
}

// 모듈 그래프를 내려받아 인터프리터의 소스 맵에 채운다 (의존 → 나중).
// 인터프리터는 네트워크를 모른다 — 여기서 다 받아 넣는다.
fn load_module_graph(it: &mut interp::Interp, url: &str, inline: Option<String>, depth: u32) {
    if depth > 16 || it.module_sources.contains_key(url) {
        return;
    }
    let src = match inline {
        Some(s) => s,
        None => match crate::http::fetch(url) {
            Ok(r) if (200..300).contains(&r.status) => {
                String::from_utf8_lossy(&r.body).into_owned()
            }
            Ok(r) => {
                println!("[module] HTTP {} — {}", r.status, url);
                return;
            }
            Err(e) => {
                println!("[module] 로드 실패 {:?} — {}", e, url);
                return;
            }
        },
    };
    it.module_sources.insert(url.to_string(), src.clone());
    // 의존 모듈(정적 import / re-export)을 찾아 재귀로 받는다
    let Ok(body) = crate::js::parser::parse(&src) else {
        println!("[module] 파싱 실패 — {}", url);
        return;
    };
    if std::env::var("KESTREL_MODULE_DEBUG").is_ok() {
        eprintln!("[module] {} — 최상위 문 {}개", url, body.len());
    }
    let mut deps: Vec<String> = Vec::new();
    for st in &body {
        match st {
            crate::js::ast::Stmt::Import { source, .. } => deps.push(source.clone()),
            crate::js::ast::Stmt::ExportAll { source } => deps.push(source.clone()),
            crate::js::ast::Stmt::ExportNamed { source: Some(s), .. } => deps.push(s.clone()),
            _ => {}
        }
    }
    // 동적 import('...') 의 문자열 리터럴도 미리 받아 둔다 (인터프리터는 네트워크를 모른다).
    // 표현식으로 계산되는 명세자는 미리 알 수 없다 — 그건 실행 시 명확한 이유로 거부된다.
    let bytes = src.as_bytes();
    let mut i = 0usize;
    while let Some(pos) = src[i..].find("import(") {
        let start = i + pos + "import(".len();
        i = start;
        let rest = &src[start..];
        let rest_t = rest.trim_start();
        let skipped = rest.len() - rest_t.len();
        let quote = rest_t.chars().next();
        if let Some(q @ ('"' | '\'')) = quote {
            if let Some(end) = rest_t[1..].find(q) {
                deps.push(rest_t[1..1 + end].to_string());
                i = start + skipped + 1 + end;
            }
        }
        let _ = bytes;
    }

    let base = crate::url::Url::parse(url).ok();
    for d in deps {
        // 베어 명세자('react')는 임포트 맵으로 해석한다 (HTML §4.12.5).
        // 맵에 없으면 조용히 틀린 URL 을 만들지 않고 정직하게 알린다.
        let mapped = it.map_specifier(&d);
        let d = match mapped {
            Some(m) => m,
            None => {
                if !d.starts_with('.') && !d.starts_with('/') && !d.starts_with("http") {
                    println!("[module] 베어 명세자를 임포트 맵에서 못 찾음: {}", d);
                    continue;
                }
                d
            }
        };
        if d.starts_with("http") {
            load_module_graph(it, &d, None, depth + 1);
            continue;
        }
        let Some(abs) = base.as_ref().and_then(|b| b.join(&d)) else { continue };
        load_module_graph(it, &abs.as_string(), None, depth + 1);
    }
}

// document.write 로 써진 <script> 를 순서대로 실행한다. 써진 스크립트가 또 write 하면
// 재귀하되 깊이를 제한한다 (실제 광고 스크립트가 2~3단 중첩을 한다).
fn run_written_scripts(
    it: &mut interp::Interp,
    base: Option<&crate::url::Url>,
    depth: u32,
) {
    if depth > 4 {
        return;
    }
    let queued: Vec<(Option<String>, String)> = std::mem::take(&mut it.written_scripts);
    for (src, inline) in queued {
        let code = match src {
            Some(href) => {
                let Some(u) = base.and_then(|b| b.join(&href)) else { continue };
                match crate::http::fetch(&u.as_string()) {
                    Ok(r) if (200..300).contains(&r.status) => {
                        String::from_utf8_lossy(&r.body).into_owned()
                    }
                    Ok(r) => {
                        println!("[script] HTTP {} — {} (document.write)", r.status, href);
                        continue;
                    }
                    Err(e) => {
                        println!("[script] 로드 실패 {:?} — {} (document.write)", e, href);
                        continue;
                    }
                }
            }
            None => inline,
        };
        if code.trim().is_empty() {
            continue;
        }
        if let Err(e) = it.run(&code) {
            println!("[js error] {}", e);
        }
        it.drain_microtasks();
        run_written_scripts(it, base, depth + 1);
    }
}

fn collect_scripts(
    dom: &crate::dom::Dom,
    id: crate::dom::NodeId,
    base: Option<&crate::url::Url>,
    out: &mut Vec<(Option<crate::dom::NodeId>, String)>,
) {
    let node = dom.get(id);
    if let crate::dom::NodeType::Element(e) = &node.node_type {
        if e.tag_name == "noscript" {
            return; // JS 실행 브라우저: noscript 내용 무시
        }
        if e.tag_name == "script" {
            // type 이 JS 일 때만 실행. application/json(데이터 임베드),
            // ld+json, text/template 등은 실행 대상이 아니다 (github 등).
            // module 은 별도 파이프라인 (import/export 의미론이 다르다).
            let ty = e
                .attributes
                .get("type")
                .map(|s| s.trim().to_ascii_lowercase())
                .unwrap_or_default();
            let is_js =
                ty.is_empty() || ty == "text/javascript" || ty == "application/javascript";
            // nomodule: 모듈을 지원하는 브라우저는 **실행하지 않는다** (HTML 표준
            // §4.12.1 "prepare the script": nomodule 이 있고 모듈을 지원하면 중단).
            // 우리는 ESM 을 구현하므로 건너뛴다. 예전엔 레거시 폴리필 번들(core-js 등)을
            // 그대로 실행해서, 최신 엔진에 맞는 코드와 충돌하며 죽었다 (react.dev).
            if e.attributes.contains_key("nomodule") {
                return;
            }
            // type=module 은 아래 모듈 파이프라인(run_module)이 따로 실행한다.
            let src = e.attributes.get("src").map(|s| s.trim()).filter(|s| !s.is_empty());
            if is_js {
                match src {
                    // 외부 스크립트: 문서 순서대로 fetch 해 인라인처럼 실행.
                    // 클래식 스크립트 의미론(동기 실행). async/defer 구분은 생략.
                    Some(href) => {
                        if let Some(b) = base {
                            let n_ext = out.iter().filter(|(_, s)| s.starts_with(EXT_TAG)).count();
                            if n_ext >= MAX_EXTERNAL_SCRIPTS {
                                // 상한에 걸려 버리는 건 조용히 하면 안 된다 —
                                // "다 실행했다"고 착각하게 만든다.
                                println!("[script] 상한({}개) 초과로 건너뜀: {}", MAX_EXTERNAL_SCRIPTS, href);
                            } else if let Some(u) = b.join(href) {
                                match crate::http::fetch(&u.as_string()) {
                                    // 2xx 가 아니면 본문은 스크립트가 아니다.
                                    // 예전엔 상태를 안 보고 404/429 의 HTML 오류 페이지를
                                    // 그대로 자바스크립트로 실행했다.
                                    Ok(r) if !(200..300).contains(&r.status) => {
                                        println!("[script] HTTP {} — {}", r.status, href);
                                    }
                                    Ok(r) => {
                                        let text = String::from_utf8_lossy(&r.body).to_string();
                                        if !text.trim().is_empty() {
                                            // 외부임을 표시(카운트용) — 앞의 마커는 실행 전 제거.
                                            out.push((Some(id), format!("{}{}", EXT_TAG, text)));
                                        }
                                    }
                                    // 못 받으면 그 스크립트에 의존하는 코드가 줄줄이 죽는다.
                                    // 조용히 넘기면 렌더가 실행마다 달라진다 — 알린다.
                                    Err(e) => println!("[script] 로드 실패 {:?} — {}", e, href),
                                }
                            }
                        }
                    }
                    None => {
                        let text = dom.text_content(id);
                        if !text.trim().is_empty() {
                            out.push((Some(id), text));
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
pub(crate) const JS_PRELUDE: &str = r#"
var __kNoop = function(){};
var __kObs = function(){ return { observe: __kNoop, unobserve: __kNoop, disconnect: __kNoop, takeRecords: function(){ return []; } }; };
// Symbol 은 엔진이 진짜 원시값으로 제공(Value::Symbol). 폴리필 불필요.
if (!Object.entries) Object.entries = function(o){ var k = Object.keys(o || {}), r = []; for (var i = 0; i < k.length; i++) r.push([k[i], o[k[i]]]); return r; };
if (!Object.values) Object.values = function(o){ var k = Object.keys(o || {}), r = []; for (var i = 0; i < k.length; i++) r.push(o[k[i]]); return r; };
if (!Object.getOwnPropertyNames) Object.getOwnPropertyNames = function(o){ return Object.keys(o || {}); };
if (!Object.getOwnPropertyDescriptor) Object.getOwnPropertyDescriptor = function(o, k){ if (o && Object.prototype.hasOwnProperty.call(o, k)) return { value: o[k], writable: true, enumerable: true, configurable: true }; return undefined; };
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
// IntersectionObserver — 진짜 교차 판정. 예전엔 콜백이 영영 발화하지 않는 무동작 스텁이라,
// 교차 시점에 콘텐츠를 드러내는 사이트(reveal-on-scroll)가 화면 안 요소까지 opacity:0 인
// 채로 남았다. 레이아웃이 실제 사각형을 주므로 뷰포트와의 교차를 그대로 계산한다.
// 표준대로 observe() 직후 초기 관측을 비동기로 1회 전달한다.
function __kIntersectionObserver(cb, opts) {
  this._cb = cb; this._els = []; this._q = [];
  this.root = (opts && opts.root) || null;
  this.rootMargin = (opts && opts.rootMargin) || '0px';
  this.thresholds = (opts && opts.threshold != null) ? [].concat(opts.threshold) : [0];
}
__kIntersectionObserver.prototype.observe = function(el){
  if (!el || this._els.indexOf(el) >= 0) return;
  this._els.push(el);
  var self = this;
  setTimeout(function(){ self._deliver([el]); }, 0);
};
__kIntersectionObserver.prototype.unobserve = function(el){
  var i = this._els.indexOf(el); if (i >= 0) this._els.splice(i, 1);
};
__kIntersectionObserver.prototype.disconnect = function(){ this._els = []; };
__kIntersectionObserver.prototype.takeRecords = function(){ var r = this._q; this._q = []; return r; };
__kIntersectionObserver.prototype._deliver = function(els){
  var vw = window.innerWidth, vh = window.innerHeight, entries = [];
  for (var i = 0; i < els.length; i++) {
    var el = els[i];
    if (this._els.indexOf(el) < 0) continue;
    var r = el.getBoundingClientRect();
    var iw = Math.max(0, Math.min(r.right, vw) - Math.max(r.left, 0));
    var ih = Math.max(0, Math.min(r.bottom, vh) - Math.max(r.top, 0));
    var area = r.width * r.height;
    entries.push({
      target: el, boundingClientRect: r, intersectionRect: r,
      rootBounds: { x: 0, y: 0, top: 0, left: 0, right: vw, bottom: vh, width: vw, height: vh },
      intersectionRatio: area > 0 ? (iw * ih) / area : 0,
      isIntersecting: iw > 0 && ih > 0, time: 0
    });
  }
  if (entries.length && this._cb) this._cb(entries, this);
};

// ResizeObserver — 표준대로 observe() 직후 현재 크기로 초기 관측을 1회 전달한다.
// (정적 렌더에는 크기 변화가 없으니 그 뒤 발화는 없다 — 예전엔 초기 관측조차 없었다.)
function __kResizeObserver(cb) { this._cb = cb; this._els = []; }
__kResizeObserver.prototype.observe = function(el){
  if (!el || this._els.indexOf(el) >= 0) return;
  this._els.push(el);
  var self = this;
  setTimeout(function(){ self._deliver([el]); }, 0);
};
__kResizeObserver.prototype.unobserve = function(el){
  var i = this._els.indexOf(el); if (i >= 0) this._els.splice(i, 1);
};
__kResizeObserver.prototype.disconnect = function(){ this._els = []; };
__kResizeObserver.prototype._deliver = function(els){
  var entries = [];
  for (var i = 0; i < els.length; i++) {
    var el = els[i];
    if (this._els.indexOf(el) < 0) continue;
    var r = el.getBoundingClientRect();
    var box = { x: 0, y: 0, top: 0, left: 0, right: r.width, bottom: r.height, width: r.width, height: r.height };
    var size = [{ inlineSize: r.width, blockSize: r.height }];
    entries.push({ target: el, contentRect: box, borderBoxSize: size, contentBoxSize: size, devicePixelContentBoxSize: size });
  }
  if (entries.length && this._cb) this._cb(entries, this);
};

// MutationObserver — 진짜 변형 기록을 배달한다. 예전엔 무동작 스텁이라 콜백이 영영
// 오지 않았다("요소가 나타나면 처리" 패턴이 통째로 죽는다). 엔진(DOM 아레나)이 childList/
// attributes/characterData 기록을 쌓고, 첫 기록에서 마이크로태스크가 한 번 예약된다.
var __kMutObs = [];
function __kMutationObserver(cb) { this._cb = cb; this._regs = []; this._q = []; __kMutObs.push(this); }
__kMutationObserver.prototype.observe = function(target, opts) {
  if (!target) return;
  var o = opts || {};
  if (!o.childList && !o.attributes && !o.characterData) o.childList = true; // 기본
  this._regs.push({ t: target, o: o });
};
__kMutationObserver.prototype.disconnect = function(){ this._regs = []; this._q = []; };
__kMutationObserver.prototype.takeRecords = function(){ var r = this._q; this._q = []; return r; };
__kMutationObserver.prototype._match = function(rec) {
  for (var i = 0; i < this._regs.length; i++) {
    var reg = this._regs[i], o = reg.o;
    var same = reg.t === rec.target;
    var inSub = o.subtree && reg.t && reg.t.contains && reg.t.contains(rec.target);
    if (!same && !inSub) continue;
    if (rec.type === 'childList' && !o.childList) continue;
    if (rec.type === 'attributes') {
      if (!o.attributes && !o.attributeFilter) continue;
      if (o.attributeFilter && o.attributeFilter.indexOf(rec.attributeName) < 0) continue;
    }
    if (rec.type === 'characterData' && !o.characterData) continue;
    return true;
  }
  return false;
};
// 엔진이 마이크로태스크로 부른다.
function __kMutationNotify() {
  var recs = __kTakeMutations();
  if (!recs || !recs.length) return;
  for (var i = 0; i < __kMutObs.length; i++) {
    var ob = __kMutObs[i], out = [];
    for (var j = 0; j < recs.length; j++) if (ob._match(recs[j])) out.push(recs[j]);
    if (out.length && ob._cb) ob._cb(out, ob);
  }
}
var MutationObserver = window.MutationObserver; if (!MutationObserver) { MutationObserver = __kMutationObserver; window.MutationObserver = MutationObserver; }
var IntersectionObserver = window.IntersectionObserver; if (!IntersectionObserver) { IntersectionObserver = __kIntersectionObserver; window.IntersectionObserver = IntersectionObserver; }
var ResizeObserver = window.ResizeObserver; if (!ResizeObserver) { ResizeObserver = __kResizeObserver; window.ResizeObserver = ResizeObserver; }
var PerformanceObserver = window.PerformanceObserver; if (!PerformanceObserver) { PerformanceObserver = __kObs; window.PerformanceObserver = PerformanceObserver; }
// matchMedia 는 엔진이 CSS @media 와 같은 평가기로 제공(Native). 스텁 불필요.
window.matchMedia = matchMedia;

// getSelection: 정적 렌더에는 사용자 선택이 없다 → 항상 빈 Selection.
// 없으면 typeof 검사 후 .toString() 을 부르는 코드가 죽는다. 빈 선택은 거짓말이 아니다.
window.getSelection = function () {
  return {
    rangeCount: 0,
    isCollapsed: true,
    anchorNode: null,
    focusNode: null,
    type: "None",
    toString() { return ""; },
    removeAllRanges() {},
    addRange() {},
    getRangeAt() { throw new Error("IndexSizeError: 선택 범위 없음"); },
  };
};
document.getSelection = window.getSelection;
if (!window.requestIdleCallback) window.requestIdleCallback = function(cb){ return setTimeout(function(){ cb({ didTimeout: false, timeRemaining: function(){ return 0; } }); }, 1); };
var requestIdleCallback = window.requestIdleCallback;
if (!window.cancelIdleCallback) window.cancelIdleCallback = function(id){ clearTimeout(id); };
if (!window.cancelAnimationFrame) window.cancelAnimationFrame = function(id){ clearTimeout(id); };
var cancelAnimationFrame = window.cancelAnimationFrame;
// getComputedStyle 은 엔진이 실제 계산 스타일로 제공(Native). 스텁 불필요.

// ── 플랫폼 API: 없으면 스크립트가 통째로 죽는 것들 ──
// 이 전역들이 없으면 그걸 쓰는 첫 줄에서 TypeError 가 나고 번들 전체가 멈춘다.
// 스텁이 아니라 실제 동작으로 넣는다.

// performance.now() — 페이지 시작 이후 경과 ms. 아주 흔하게 쓰인다.
if (!window.performance) {
  var __kT0 = Date.now();
  var __kMarks = {};
  window.performance = {
    timeOrigin: __kT0,
    now: function(){ return Date.now() - __kT0; },
    mark: function(n){ __kMarks[n] = Date.now() - __kT0; },
    measure: function(){ },
    getEntriesByName: function(){ return []; },
    getEntriesByType: function(){ return []; },
    clearMarks: function(){ __kMarks = {}; },
    clearMeasures: __kNoop
  };
}
var performance = window.performance;

// btoa / atob — 진짜 base64 (RFC 4648).
var __kB64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
if (!window.btoa) window.btoa = function(s){
  s = String(s);
  var out = '', i = 0;
  while (i < s.length) {
    var c1 = s.charCodeAt(i++), c2 = s.charCodeAt(i++), c3 = s.charCodeAt(i++);
    if (c1 > 255 || (c2 === c2 && c2 > 255) || (c3 === c3 && c3 > 255)) {
      throw new Error('btoa: Latin-1 범위를 넘는 문자');
    }
    var e1 = c1 >> 2;
    var e2 = ((c1 & 3) << 4) | (c2 !== c2 ? 0 : c2 >> 4);
    var e3 = c2 !== c2 ? 64 : (((c2 & 15) << 2) | (c3 !== c3 ? 0 : c3 >> 6));
    var e4 = c3 !== c3 ? 64 : (c3 & 63);
    out += __kB64.charAt(e1) + __kB64.charAt(e2) +
           (e3 === 64 ? '=' : __kB64.charAt(e3)) + (e4 === 64 ? '=' : __kB64.charAt(e4));
  }
  return out;
};
if (!window.atob) window.atob = function(s){
  s = String(s).replace(/[\s=]/g, '');
  var out = '', acc = 0, bits = 0;
  for (var i = 0; i < s.length; i++) {
    var v = __kB64.indexOf(s.charAt(i));
    if (v < 0) throw new Error('atob: base64 가 아닌 문자');
    acc = (acc << 6) | v; bits += 6;
    if (bits >= 8) { bits -= 8; out += String.fromCharCode((acc >> bits) & 255); }
  }
  return out;
};
var btoa = window.btoa, atob = window.atob;

// Promise.any — 하나라도 이행되면 이행, 전부 거부면 AggregateError.
if (!Promise.any) Promise.any = function(list){
  var arr = Array.from(list);
  return new Promise(function(resolve, reject){
    var errs = [], left = arr.length;
    if (left === 0) { reject(new Error('AggregateError: 모든 프로미스가 거부됨')); return; }
    arr.forEach(function(p, i){
      Promise.resolve(p).then(resolve, function(e){
        errs[i] = e;
        if (--left === 0) {
          var ag = new Error('AggregateError: 모든 프로미스가 거부됨');
          ag.errors = errs;
          reject(ag);
        }
      });
    });
  });
};

// crypto — getRandomValues/randomUUID.
// 주의: 엔진의 Math.random(xorshift) 기반이라 암호학적으로 안전하지 않다.
// 렌더링 목적의 식별자 생성엔 충분하지만, 보안 용도로 신뢰하면 안 된다.
if (!window.crypto) {
  window.crypto = {
    getRandomValues: function(a){
      for (var i = 0; i < a.length; i++) a[i] = Math.floor(Math.random() * 256);
      return a;
    },
    randomUUID: function(){
      var h = '0123456789abcdef', s = '';
      for (var i = 0; i < 36; i++) {
        if (i === 8 || i === 13 || i === 18 || i === 23) { s += '-'; continue; }
        if (i === 14) { s += '4'; continue; }
        var r = Math.floor(Math.random() * 16);
        if (i === 19) r = (r & 3) | 8;
        s += h.charAt(r);
      }
      return s;
    }
  };
}
var crypto = window.crypto;

// URLSearchParams — 진짜 파싱/직렬화.
function __kURLSearchParams(init) {
  this._p = [];
  var self = this;
  if (typeof init === 'string') {
    var s = init.charAt(0) === '?' ? init.slice(1) : init;
    if (s) s.split('&').forEach(function(kv){
      if (!kv) return;
      var i = kv.indexOf('=');
      var k = i < 0 ? kv : kv.slice(0, i);
      var v = i < 0 ? '' : kv.slice(i + 1);
      self._p.push([decodeURIComponent(k.replace(/\+/g, ' ')),
                    decodeURIComponent(v.replace(/\+/g, ' '))]);
    });
  } else if (init && typeof init === 'object') {
    Object.keys(init).forEach(function(k){ self._p.push([k, String(init[k])]); });
  }
}
__kURLSearchParams.prototype.get = function(k){
  for (var i = 0; i < this._p.length; i++) if (this._p[i][0] === k) return this._p[i][1];
  return null;
};
__kURLSearchParams.prototype.getAll = function(k){
  return this._p.filter(function(e){ return e[0] === k; }).map(function(e){ return e[1]; });
};
__kURLSearchParams.prototype.has = function(k){ return this.get(k) !== null; };
__kURLSearchParams.prototype.set = function(k, v){
  var found = false, out = [];
  for (var i = 0; i < this._p.length; i++) {
    if (this._p[i][0] === k) { if (found) continue; out.push([k, String(v)]); found = true; }
    else out.push(this._p[i]);
  }
  if (!found) out.push([k, String(v)]);
  this._p = out;
};
__kURLSearchParams.prototype.append = function(k, v){ this._p.push([k, String(v)]); };
__kURLSearchParams.prototype['delete'] = function(k){
  this._p = this._p.filter(function(e){ return e[0] !== k; });
};
__kURLSearchParams.prototype.forEach = function(fn){
  this._p.forEach(function(e){ fn(e[1], e[0]); });
};
__kURLSearchParams.prototype.keys = function(){ return this._p.map(function(e){ return e[0]; }); };
__kURLSearchParams.prototype.values = function(){ return this._p.map(function(e){ return e[1]; }); };
__kURLSearchParams.prototype.entries = function(){ return this._p.map(function(e){ return [e[0], e[1]]; }); };
__kURLSearchParams.prototype.toString = function(){
  return this._p.map(function(e){
    return encodeURIComponent(e[0]) + '=' + encodeURIComponent(e[1]);
  }).join('&');
};
if (!window.URLSearchParams) window.URLSearchParams = __kURLSearchParams;
var URLSearchParams = window.URLSearchParams;

// AbortController / AbortSignal — 실제 상태 + 리스너 발화.
function __kAbortSignal() { this.aborted = false; this.reason = undefined; this._ls = []; this.onabort = null; }
__kAbortSignal.prototype.addEventListener = function(t, f){ if (t === 'abort') this._ls.push(f); };
__kAbortSignal.prototype.removeEventListener = function(t, f){
  if (t === 'abort') this._ls = this._ls.filter(function(x){ return x !== f; });
};
__kAbortSignal.prototype.throwIfAborted = function(){ if (this.aborted) throw this.reason; };
function __kAbortController() { this.signal = new __kAbortSignal(); }
__kAbortController.prototype.abort = function(reason){
  var s = this.signal;
  if (s.aborted) return;
  s.aborted = true;
  s.reason = reason === undefined ? new Error('AbortError') : reason;
  var e = { type: 'abort', target: s };
  if (typeof s.onabort === 'function') s.onabort(e);
  s._ls.slice().forEach(function(f){ f(e); });
};
if (!window.AbortController) { window.AbortController = __kAbortController; window.AbortSignal = __kAbortSignal; }
var AbortController = window.AbortController;

// ── 타입드 배열 ──
// 실제 바이트 버퍼(ArrayBuffer) 위의 뷰로 구현한다. 인덱스 읽기/쓰기는 Proxy 로 가로채
// 타입에 맞게 인코딩/디코딩한다 — 그래서 Uint8Array 에 256 을 넣으면 0 이 되고,
// Float32Array 는 32비트 정밀도로 실제로 반올림된다(IEEE754 인코딩을 거치므로).
// 숫자 배열로 흉내내면 이 규칙들이 전부 조용히 틀린다.
function __kArrayBuffer(len) {
  this.byteLength = len | 0;
  this._b = [];
  for (var i = 0; i < this.byteLength; i++) this._b.push(0);
}
__kArrayBuffer.prototype.slice = function(a, b){
  a = a || 0; b = (b === undefined) ? this.byteLength : b;
  var out = new __kArrayBuffer(Math.max(0, b - a));
  for (var i = 0; i < out.byteLength; i++) out._b[i] = this._b[a + i];
  return out;
};

// IEEE754 인코딩/디코딩 (Float32/Float64) — 실제 비트로 왕복한다.
function __kF2B(v, bytes, mBits, eBits) {
  var eMax = (1 << eBits) - 1, bias = eMax >> 1;
  var s = 0;
  if (v < 0 || (v === 0 && 1 / v < 0)) { s = 1; v = -v; }
  var e, m;
  if (v !== v) { e = eMax; m = 1; s = 0; }
  else if (v === Infinity) { e = eMax; m = 0; }
  else if (v === 0) { e = 0; m = 0; }
  else {
    e = Math.floor(Math.log(v) / Math.LN2);
    if (v * Math.pow(2, -e) < 1) e--;
    if (v * Math.pow(2, -e) >= 2) e++;
    if (e + bias >= 1) {
      m = Math.round((v * Math.pow(2, -e) - 1) * Math.pow(2, mBits));
      if (m >= Math.pow(2, mBits)) { m = 0; e++; }
      e = e + bias;
      if (e >= eMax) { e = eMax; m = 0; }
    } else {
      m = Math.round(v * Math.pow(2, bias - 1) * Math.pow(2, mBits));
      e = 0;
    }
  }
  // 비트를 바이트로 (리틀엔디언)
  var out = [];
  var bits = [];
  for (var i = 0; i < mBits; i++) { bits.push(m % 2); m = Math.floor(m / 2); }
  for (var i = 0; i < eBits; i++) { bits.push(e % 2); e = Math.floor(e / 2); }
  bits.push(s);
  for (var i = 0; i < bytes; i++) {
    var b = 0;
    for (var k = 0; k < 8; k++) b |= (bits[i * 8 + k] || 0) << k;
    out.push(b);
  }
  return out;
}
function __kB2F(bs, mBits, eBits) {
  var bits = [];
  for (var i = 0; i < bs.length; i++)
    for (var k = 0; k < 8; k++) bits.push((bs[i] >> k) & 1);
  var m = 0;
  for (var i = mBits - 1; i >= 0; i--) m = m * 2 + bits[i];
  var e = 0;
  for (var i = eBits - 1; i >= 0; i--) e = e * 2 + bits[mBits + i];
  var s = bits[mBits + eBits] ? -1 : 1;
  var eMax = (1 << eBits) - 1, bias = eMax >> 1;
  if (e === eMax) return m ? NaN : s * Infinity;
  if (e === 0) return s * m * Math.pow(2, 1 - bias - mBits);
  return s * (1 + m * Math.pow(2, -mBits)) * Math.pow(2, e - bias);
}

var __kTA = {
  Int8Array:    {size: 1, get: function(b,o){var v=b[o]; return v>127?v-256:v;},
                 set: function(b,o,v){ b[o]=((v|0)%256+256)%256; }},
  Uint8Array:   {size: 1, get: function(b,o){return b[o];},
                 set: function(b,o,v){ b[o]=((v|0)%256+256)%256; }},
  Uint8ClampedArray: {size: 1, get: function(b,o){return b[o];},
                 set: function(b,o,v){ v=Math.round(v); b[o]= v<0?0:(v>255?255:v); }},
  Int16Array:   {size: 2, get: function(b,o){var v=b[o]|(b[o+1]<<8); return v>32767?v-65536:v;},
                 set: function(b,o,v){ v=((v|0)%65536+65536)%65536; b[o]=v&255; b[o+1]=(v>>8)&255; }},
  Uint16Array:  {size: 2, get: function(b,o){return b[o]|(b[o+1]<<8);},
                 set: function(b,o,v){ v=((v|0)%65536+65536)%65536; b[o]=v&255; b[o+1]=(v>>8)&255; }},
  Int32Array:   {size: 4, get: function(b,o){return (b[o]|(b[o+1]<<8)|(b[o+2]<<16)|(b[o+3]<<24));},
                 set: function(b,o,v){ v=v|0; b[o]=v&255; b[o+1]=(v>>8)&255; b[o+2]=(v>>16)&255; b[o+3]=(v>>24)&255; }},
  Uint32Array:  {size: 4, get: function(b,o){var v=(b[o]|(b[o+1]<<8)|(b[o+2]<<16)|(b[o+3]<<24)); return v<0?v+4294967296:v;},
                 set: function(b,o,v){ v=v>>>0; b[o]=v&255; b[o+1]=(v>>>8)&255; b[o+2]=(v>>>16)&255; b[o+3]=(v>>>24)&255; }},
  Float32Array: {size: 4, get: function(b,o){return __kB2F([b[o],b[o+1],b[o+2],b[o+3]], 23, 8);},
                 set: function(b,o,v){ var e=__kF2B(+v,4,23,8); for(var i=0;i<4;i++) b[o+i]=e[i]; }},
  Float64Array: {size: 8, get: function(b,o){return __kB2F([b[o],b[o+1],b[o+2],b[o+3],b[o+4],b[o+5],b[o+6],b[o+7]], 52, 11);},
                 set: function(b,o,v){ var e=__kF2B(+v,8,52,11); for(var i=0;i<8;i++) b[o+i]=e[i]; }}
};

function __kMakeTypedArray(name) {
  var spec = __kTA[name];
  function Ctor(arg, byteOffset, length) {
    var buf, off = 0, len = 0;
    if (arg instanceof __kArrayBuffer) {
      buf = arg;
      off = byteOffset || 0;
      len = (length === undefined) ? Math.floor((buf.byteLength - off) / spec.size) : length;
    } else if (typeof arg === 'number') {
      len = arg | 0;
      buf = new __kArrayBuffer(len * spec.size);
    } else if (arg && typeof arg.length === 'number') {
      len = arg.length;
      buf = new __kArrayBuffer(len * spec.size);
    } else {
      len = 0;
      buf = new __kArrayBuffer(0);
    }
    var self = this;
    this.buffer = buf;
    this.byteOffset = off;
    this.length = len;
    this.byteLength = len * spec.size;
    this.BYTES_PER_ELEMENT = spec.size;
    this._spec = spec;
    var view = new Proxy(this, {
      get: function(t, k){
        var n = (typeof k === 'string' && k !== '' && String(+k) === k) ? +k : -1;
        if (n >= 0) {
          if (n >= t.length) return undefined;
          return spec.get(t.buffer._b, t.byteOffset + n * spec.size);
        }
        return t[k];
      },
      set: function(t, k, v){
        var n = (typeof k === 'string' && k !== '' && String(+k) === k) ? +k : -1;
        if (n >= 0) {
          if (n < t.length) spec.set(t.buffer._b, t.byteOffset + n * spec.size, v);
          return true;
        }
        t[k] = v;
        return true;
      }
    });
    if (arg && typeof arg !== 'number' && !(arg instanceof __kArrayBuffer) && typeof arg.length === 'number') {
      for (var i = 0; i < len; i++) spec.set(buf._b, off + i * spec.size, arg[i]);
    }
    return view;
  }
  Ctor.prototype.set = function(src, off){
    off = off || 0;
    for (var i = 0; i < src.length; i++) this[off + i] = src[i];
  };
  Ctor.prototype.fill = function(v, a, b){
    a = a || 0; b = (b === undefined) ? this.length : b;
    for (var i = a; i < b; i++) this[i] = v;
    return this;
  };
  Ctor.prototype.subarray = function(a, b){
    a = a || 0; b = (b === undefined) ? this.length : b;
    return new Ctor(this.buffer, this.byteOffset + a * spec.size, Math.max(0, b - a));
  };
  Ctor.prototype.slice = function(a, b){
    a = a || 0; b = (b === undefined) ? this.length : b;
    var out = new Ctor(Math.max(0, b - a));
    for (var i = 0; i < out.length; i++) out[i] = this[a + i];
    return out;
  };
  Ctor.prototype.forEach = function(fn){ for (var i = 0; i < this.length; i++) fn(this[i], i, this); };
  Ctor.prototype.map = function(fn){
    var out = new Ctor(this.length);
    for (var i = 0; i < this.length; i++) out[i] = fn(this[i], i, this);
    return out;
  };
  Ctor.prototype.indexOf = function(v){
    for (var i = 0; i < this.length; i++) if (this[i] === v) return i;
    return -1;
  };
  Ctor.prototype.includes = function(v){ return this.indexOf(v) >= 0; };
  Ctor.prototype.join = function(sep){
    var a = [];
    for (var i = 0; i < this.length; i++) a.push(this[i]);
    return a.join(sep === undefined ? ',' : sep);
  };
  Ctor.prototype.reduce = function(fn, init){
    var acc = init;
    for (var i = 0; i < this.length; i++) acc = fn(acc, this[i], i, this);
    return acc;
  };
  Ctor.prototype[Symbol.iterator] = function*(){
    for (var i = 0; i < this.length; i++) yield this[i];
  };
  Ctor.from = function(x, fn){
    var arr = Array.from(x, fn);
    return new Ctor(arr);
  };
  Ctor.of = function(){ return new Ctor(Array.prototype.slice.call(arguments)); };
  Ctor.BYTES_PER_ELEMENT = spec.size;
  return Ctor;
}
if (!window.ArrayBuffer) {
  window.ArrayBuffer = __kArrayBuffer;
  Object.keys(__kTA).forEach(function(n){ window[n] = __kMakeTypedArray(n); });
}
var ArrayBuffer = window.ArrayBuffer;
var Uint8Array = window.Uint8Array, Int8Array = window.Int8Array;
var Uint8ClampedArray = window.Uint8ClampedArray;
var Uint16Array = window.Uint16Array, Int16Array = window.Int16Array;
var Uint32Array = window.Uint32Array, Int32Array = window.Int32Array;
var Float32Array = window.Float32Array, Float64Array = window.Float64Array;

// TextEncoder / TextDecoder — 실제 UTF-8 인코딩/디코딩.
if (!window.TextEncoder) {
  window.TextEncoder = function(){};
  window.TextEncoder.prototype.encode = function(s){
    s = String(s === undefined ? '' : s);
    var out = [];
    for (var i = 0; i < s.length; i++) {
      var c = s.codePointAt(i);
      if (c > 0xffff) i++;
      if (c < 0x80) out.push(c);
      else if (c < 0x800) { out.push(0xc0 | (c >> 6), 0x80 | (c & 63)); }
      else if (c < 0x10000) { out.push(0xe0 | (c >> 12), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63)); }
      else { out.push(0xf0 | (c >> 18), 0x80 | ((c >> 12) & 63), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63)); }
    }
    return new Uint8Array(out);
  };
  window.TextDecoder = function(){};
  window.TextDecoder.prototype.decode = function(a){
    if (!a) return '';
    var out = '', i = 0;
    while (i < a.length) {
      var b = a[i++];
      var c;
      if (b < 0x80) c = b;
      else if (b < 0xe0) c = ((b & 31) << 6) | (a[i++] & 63);
      else if (b < 0xf0) c = ((b & 15) << 12) | ((a[i++] & 63) << 6) | (a[i++] & 63);
      else c = ((b & 7) << 18) | ((a[i++] & 63) << 12) | ((a[i++] & 63) << 6) | (a[i++] & 63);
      out += String.fromCodePoint(c);
    }
    return out;
  };
}
var TextEncoder = window.TextEncoder, TextDecoder = window.TextDecoder;

// ── 커스텀 엘리먼트 (Web Components) ──
// 없으면 customElements.define 한 줄에서 죽고, 컴포넌트로 만든 페이지는 통째로 빈다.
//
// 핵심: HTMLElement 를 "지금 업그레이드 중인 요소를 반환하는 함수"로 둔다.
// class MyEl extends HTMLElement { constructor(){ super(); … } } 에서 super() 의 반환
// 객체가 this 가 되므로(표준), this 는 진짜 DOM 노드다 — this.innerHTML 이 실제로 그린다.
// 흉내내는 게 아니라 실제 DOM 위에서 돈다.
var __kUpgrading = null;
function __kHTMLElement() { return __kUpgrading; }
__kHTMLElement.prototype = {};
if (!window.HTMLElement) window.HTMLElement = __kHTMLElement;
var HTMLElement = window.HTMLElement;
// 흔히 확장하는 다른 기반 클래스들도 같은 규약으로
if (!window.HTMLDivElement) window.HTMLDivElement = __kHTMLElement;
if (!window.HTMLSpanElement) window.HTMLSpanElement = __kHTMLElement;

var __kCE = {};      // 태그명 → 생성자
var __kCEDone = [];  // 이미 업그레이드한 요소들

function __kUpgrade(el, ctor) {
  if (!el || __kCEDone.indexOf(el) >= 0) return;
  __kCEDone.push(el);
  var prev = __kUpgrading;
  __kUpgrading = el;
  try { new ctor(); } catch (e) { console.error('커스텀 엘리먼트 생성자 오류: ' + e); }
  __kUpgrading = prev;
  var proto = ctor.prototype;
  if (proto && typeof proto.connectedCallback === 'function') {
    try { proto.connectedCallback.call(el); } catch (e) {
      console.error('connectedCallback 오류: ' + e);
    }
  }
  // 초기 속성에 대해 attributeChangedCallback (표준: 업그레이드 시 관측 속성 전달)
  var obs = ctor.observedAttributes;
  if (obs && proto && typeof proto.attributeChangedCallback === 'function') {
    for (var i = 0; i < obs.length; i++) {
      var v = el.getAttribute(obs[i]);
      if (v !== null) {
        try { proto.attributeChangedCallback.call(el, obs[i], null, v); } catch (e) {}
      }
    }
  }
}

function __kUpgradeAll(name) {
  var ctor = __kCE[name];
  if (!ctor) return;
  var list = document.querySelectorAll(name);
  for (var i = 0; i < list.length; i++) __kUpgrade(list[i], ctor);
}

if (!window.customElements) {
  window.customElements = {
    define: function(name, ctor){
      __kCE[name] = ctor;
      __kUpgradeAll(name);
    },
    get: function(name){ return __kCE[name]; },
    whenDefined: function(){ return Promise.resolve(); },
    upgrade: function(root){
      Object.keys(__kCE).forEach(function(n){ __kUpgradeAll(n); });
    }
  };
  // 나중에 DOM 에 추가되는 요소도 업그레이드한다. 진짜 MutationObserver 로 —
  // 폴링이나 훅 흉내가 아니라 표준 메커니즘 위에서 돈다.
  var __kCEObs = new MutationObserver(function(recs){
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (r.type === 'childList') {
        for (var j = 0; j < r.addedNodes.length; j++) {
          var n = r.addedNodes[j];
          if (!n || !n.tagName) continue;
          var t = n.tagName.toLowerCase();
          if (__kCE[t]) __kUpgrade(n, __kCE[t]);
        }
        // 새로 붙은 서브트리 안쪽도
        Object.keys(__kCE).forEach(function(nm){ __kUpgradeAll(nm); });
      } else if (r.type === 'attributes' && r.target && r.target.tagName) {
        var tt = r.target.tagName.toLowerCase();
        var c = __kCE[tt];
        if (c && c.observedAttributes && c.prototype &&
            typeof c.prototype.attributeChangedCallback === 'function' &&
            c.observedAttributes.indexOf(r.attributeName) >= 0) {
          try {
            c.prototype.attributeChangedCallback.call(
              r.target, r.attributeName, null, r.target.getAttribute(r.attributeName));
          } catch (e) {}
        }
      }
    }
  });
  // DOM 이 없는 실행 환경(엔진 단위 테스트 등)에서는 조용히 건너뛴다.
  try {
    if (document.body) {
      __kCEObs.observe(document.body, { childList: true, subtree: true, attributes: true });
    }
  } catch (e) {}
}
var customElements = window.customElements;

// FormData — 폼의 실제 컨트롤 값을 읽는다.
function __kFormData(form) {
  this._p = [];
  var self = this;
  if (form && form.elements) {
    var els = form.elements;
    for (var i = 0; i < els.length; i++) {
      var el = els[i];
      var name = el.getAttribute('name');
      if (!name) continue;
      var type = (el.getAttribute('type') || '').toLowerCase();
      if ((type === 'checkbox' || type === 'radio') && !el.checked) continue;
      if (el.tagName.toLowerCase() === 'button') continue;
      self._p.push([name, el.value === undefined ? '' : String(el.value)]);
    }
  }
}
__kFormData.prototype.get = function(k){
  for (var i = 0; i < this._p.length; i++) if (this._p[i][0] === k) return this._p[i][1];
  return null;
};
__kFormData.prototype.getAll = function(k){
  return this._p.filter(function(e){ return e[0] === k; }).map(function(e){ return e[1]; });
};
__kFormData.prototype.has = function(k){ return this.get(k) !== null; };
__kFormData.prototype.append = function(k, v){ this._p.push([k, String(v)]); };
__kFormData.prototype.set = function(k, v){
  this._p = this._p.filter(function(e){ return e[0] !== k; });
  this._p.push([k, String(v)]);
};
__kFormData.prototype['delete'] = function(k){
  this._p = this._p.filter(function(e){ return e[0] !== k; });
};
__kFormData.prototype.forEach = function(fn){ this._p.forEach(function(e){ fn(e[1], e[0]); }); };
__kFormData.prototype.entries = function(){ return this._p.map(function(e){ return [e[0], e[1]]; }); };
__kFormData.prototype.keys = function(){ return this._p.map(function(e){ return e[0]; }); };
__kFormData.prototype.toString = function(){
  return this._p.map(function(e){
    return encodeURIComponent(e[0]) + '=' + encodeURIComponent(e[1]);
  }).join('&');
};
if (!window.FormData) window.FormData = __kFormData;
var FormData = window.FormData;

// ── Intl ──
// 없으면 new Intl.NumberFormat(...) 한 줄에서 죽는다. 날짜/숫자를 로케일에 맞춰
// 찍는 코드는 아주 흔하다.
//
// 로케일 데이터를 다 담을 수는 없다. 담을 수 있는 것만 정확히 하고, 나머지는
// 표준의 기본 규칙(en-US)으로 떨어뜨린다 — 지어내지 않는다.
// 지원: 숫자 구분자(그룹/소수점), 최소/최대 소수 자릿수, 퍼센트/통화 표기,
//       날짜/시간 필드 조합, RelativeTimeFormat, PluralRules(en 규칙), Collator.
var __kSep = {
  // [그룹 구분자, 소수점]
  'en': [',', '.'], 'ko': [',', '.'], 'ja': [',', '.'], 'zh': [',', '.'],
  'he': [',', '.'], 'th': [',', '.'], 'en-IN': [',', '.'],
  'de': ['.', ','], 'es': ['.', ','], 'it': ['.', ','], 'pt': ['.', ','],
  'nl': ['.', ','], 'tr': ['.', ','], 'id': ['.', ','], 'da': ['.', ','],
  'fr': [' ', ','], 'ru': [' ', ','], 'pl': [' ', ','],
  'cs': [' ', ','], 'sv': [' ', ','], 'fi': [' ', ','],
  'nb': [' ', ','], 'uk': [' ', ',']
};
function __kLocSep(loc) {
  var l = String(loc || 'en');
  if (__kSep[l]) return __kSep[l];
  var base = l.split('-')[0];
  return __kSep[base] || __kSep['en'];
}
function __kCurrencySymbol(c) {
  var m = { USD: '$', EUR: '€', GBP: '£', JPY: '¥', KRW: '₩', CNY: 'CN¥' };
  return m[c] || (c ? c + ' ' : '');
}
function __kIntlNumberFormat(locales, opts) {
  if (!(this instanceof __kIntlNumberFormat)) return new __kIntlNumberFormat(locales, opts);
  var loc = Array.isArray(locales) ? locales[0] : locales;
  this._sep = __kLocSep(loc);
  this._o = opts || {};
  this._loc = loc || 'en';
}
__kIntlNumberFormat.prototype.format = function(n) {
  n = Number(n);
  if (n !== n) return 'NaN';
  if (n === Infinity) return '∞';
  if (n === -Infinity) return '-∞';
  var o = this._o;
  var style = o.style || 'decimal';
  var value = (style === 'percent') ? n * 100 : n;
  var minF = o.minimumFractionDigits;
  var maxF = o.maximumFractionDigits;
  if (minF === undefined) minF = (style === 'currency') ? 2 : 0;
  if (maxF === undefined) maxF = (style === 'currency') ? 2 : Math.max(minF, 3);
  if (maxF < minF) maxF = minF;
  var neg = value < 0;
  var abs = Math.abs(value);
  var fixed = abs.toFixed(maxF);
  // 최대 자릿수까지 반올림한 뒤, 최소 자릿수 이하의 불필요한 0 은 제거
  var parts = fixed.split('.');
  var ip = parts[0], fp = parts[1] || '';
  while (fp.length > minF && fp.charAt(fp.length - 1) === '0') fp = fp.slice(0, -1);
  // 그룹 구분자 (useGrouping: false 면 생략)
  var grouped = ip;
  if (o.useGrouping !== false && ip.length > 3) {
    grouped = '';
    var c = 0;
    for (var i = ip.length - 1; i >= 0; i--) {
      grouped = ip.charAt(i) + grouped;
      if (++c % 3 === 0 && i > 0) grouped = this._sep[0] + grouped;
    }
  }
  var out = grouped + (fp ? this._sep[1] + fp : '');
  if (neg) out = '-' + out;
  if (style === 'percent') out += '%';
  if (style === 'currency') out = __kCurrencySymbol(o.currency) + out;
  return out;
};
__kIntlNumberFormat.prototype.resolvedOptions = function(){
  return { locale: this._loc, style: this._o.style || 'decimal' };
};

var __kMonths = {
  'en': ['January','February','March','April','May','June','July','August','September','October','November','December'],
  'ko': ['1월','2월','3월','4월','5월','6월','7월','8월','9월','10월','11월','12월'],
  'ja': ['1月','2月','3月','4月','5月','6月','7月','8月','9月','10月','11月','12月']
};
var __kDays = {
  'en': ['Sunday','Monday','Tuesday','Wednesday','Thursday','Friday','Saturday'],
  'ko': ['일요일','월요일','화요일','수요일','목요일','금요일','토요일'],
  'ja': ['日曜日','月曜日','火曜日','水曜日','木曜日','金曜日','土曜日']
};
function __kIntlDateTimeFormat(locales, opts) {
  if (!(this instanceof __kIntlDateTimeFormat)) return new __kIntlDateTimeFormat(locales, opts);
  var loc = Array.isArray(locales) ? locales[0] : (locales || 'en');
  this._loc = loc;
  this._base = String(loc).split('-')[0];
  this._o = opts || {};
}
__kIntlDateTimeFormat.prototype.format = function(d) {
  d = (d === undefined) ? new Date() : (d instanceof Date ? d : new Date(d));
  var o = this._o;
  var months = __kMonths[this._base] || __kMonths['en'];
  var days = __kDays[this._base] || __kDays['en'];
  var pad = function(x){ return x < 10 ? '0' + x : String(x); };
  var y = d.getFullYear(), m = d.getMonth(), day = d.getDate();
  var out = [];
  var hasDate = o.year || o.month || o.day || o.weekday;
  var hasTime = o.hour || o.minute || o.second;
  if (!hasDate && !hasTime) { o = { year: 'numeric', month: 'numeric', day: 'numeric' }; hasDate = true; }
  if (o.weekday) out.push(days[d.getDay()]);
  if (hasDate) {
    var mo = (o.month === 'long') ? months[m]
           : (o.month === 'short') ? months[m].slice(0, 3)
           : (o.month === '2-digit') ? pad(m + 1)
           : String(m + 1);
    var dd = (o.day === '2-digit') ? pad(day) : String(day);
    var yy = (o.year === '2-digit') ? pad(y % 100) : String(y);
    if (this._base === 'ko' || this._base === 'ja' || this._base === 'zh') {
      out.push(yy + '. ' + mo + ' ' + dd + '.');
    } else if (o.month === 'long' || o.month === 'short') {
      out.push(mo + ' ' + dd + ', ' + yy);
    } else {
      out.push(mo + '/' + dd + '/' + yy);
    }
  }
  if (hasTime) {
    var h = d.getHours(), mi = d.getMinutes(), se = d.getSeconds();
    var t = (o.hour12 === false) ? (pad(h) + ':' + pad(mi)) :
            (((h % 12) || 12) + ':' + pad(mi));
    if (o.second) t += ':' + pad(se);
    if (o.hour12 !== false) t += (h < 12 ? ' AM' : ' PM');
    out.push(t);
  }
  return out.join(', ');
};
__kIntlDateTimeFormat.prototype.formatToParts = function(d){
  return [{ type: 'literal', value: this.format(d) }];
};
__kIntlDateTimeFormat.prototype.resolvedOptions = function(){
  return { locale: this._loc, timeZone: 'UTC', calendar: 'gregory' };
};

function __kIntlRelativeTimeFormat(locales, opts) {
  if (!(this instanceof __kIntlRelativeTimeFormat)) return new __kIntlRelativeTimeFormat(locales, opts);
  this._o = opts || {};
}
__kIntlRelativeTimeFormat.prototype.format = function(v, unit) {
  v = Number(v);
  var u = String(unit).replace(/s$/, '');
  var n = Math.abs(v);
  var plural = (n === 1) ? u : u + 's';
  if (v < 0) return n + ' ' + plural + ' ago';
  return 'in ' + n + ' ' + plural;
};

function __kIntlPluralRules(locales, opts) {
  if (!(this instanceof __kIntlPluralRules)) return new __kIntlPluralRules(locales, opts);
  this._type = (opts && opts.type) || 'cardinal';
}
__kIntlPluralRules.prototype.select = function(n) {
  n = Number(n);
  if (this._type === 'ordinal') {
    var r10 = n % 10, r100 = n % 100;
    if (r10 === 1 && r100 !== 11) return 'one';
    if (r10 === 2 && r100 !== 12) return 'two';
    if (r10 === 3 && r100 !== 13) return 'few';
    return 'other';
  }
  return n === 1 ? 'one' : 'other';
};

function __kIntlCollator(locales, opts) {
  if (!(this instanceof __kIntlCollator)) return new __kIntlCollator(locales, opts);
  this._num = !!(opts && opts.numeric);
  // 표준: collator.compare 는 바인딩된 함수다 — arr.sort(c.compare) 로 넘겨도 동작해야 한다.
  this.compare = this.compare.bind(this);
}
__kIntlCollator.prototype.compare = function(a, b) {
  a = String(a); b = String(b);
  if (this._num) {
    var na = parseFloat(a), nb = parseFloat(b);
    if (na === na && nb === nb && na !== nb) return na < nb ? -1 : 1;
  }
  return a < b ? -1 : (a > b ? 1 : 0);
};

if (!window.Intl) {
  window.Intl = {
    NumberFormat: __kIntlNumberFormat,
    DateTimeFormat: __kIntlDateTimeFormat,
    RelativeTimeFormat: __kIntlRelativeTimeFormat,
    PluralRules: __kIntlPluralRules,
    Collator: __kIntlCollator,
    getCanonicalLocales: function(l){ return [].concat(l || []); }
  };
}
var Intl = window.Intl;

// toLocaleString 계열도 Intl 로 (예전엔 그냥 String(this) 였다)
if (Number.prototype.toLocaleString) {
  Number.prototype.toLocaleString = function(loc, opts){
    return new Intl.NumberFormat(loc, opts).format(this);
  };
}
if (Date.prototype.toLocaleDateString) {
  Date.prototype.toLocaleDateString = function(loc, opts){
    return new Intl.DateTimeFormat(loc, opts || { year:'numeric', month:'numeric', day:'numeric' }).format(this);
  };
  Date.prototype.toLocaleString = function(loc, opts){
    return new Intl.DateTimeFormat(loc, opts || { year:'numeric', month:'numeric', day:'numeric', hour:'numeric', minute:'numeric' }).format(this);
  };
  Date.prototype.toLocaleTimeString = function(loc, opts){
    return new Intl.DateTimeFormat(loc, opts || { hour:'numeric', minute:'numeric', second:'numeric' }).format(this);
  };
}

var Reflect = window.Reflect;
if (!Reflect) {
  Reflect = {};
  Reflect.get = function(t, k){ return t[k]; };
  Reflect.set = function(t, k, v){ t[k] = v; return true; };
  Reflect.has = function(t, k){ return k in t; };
  Reflect.deleteProperty = function(t, k){ delete t[k]; return true; };
  Reflect.ownKeys = function(t){ return Object.keys(t || {}); };
  Reflect.getPrototypeOf = function(){ return null; };
  Reflect.setPrototypeOf = function(o, p){ Object.setPrototypeOf(o, p); return true; };
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
    fn event_capture_runs_before_target() {
        // DOM 이벤트는 캡처 → 타깃 → 버블 3단계다 (DOM 표준 §2.9). 예전엔 캡처 플래그를
        // 통째로 버려서 캡처 리스너가 타깃보다 **늦게** 불렸다 (이벤트 위임이 어긋난다).
        let mut dom = crate::html::parse_dom(
            "<div id=\"outer\"><button id=\"b\">x</button></div><p id=\"t\">?</p>\
             <script>\
             var s = '';\
             document.getElementById('outer').addEventListener('click', function () { s += 'C' }, true);\
             document.getElementById('b').addEventListener('click', function () { s += 'T' });\
             document.getElementById('outer').addEventListener('click', function () { s += 'B' });\
             document.getElementById('b').dispatchEvent(new Event('click', { bubbles: true }));\
             document.getElementById('t').textContent = s;\
             </script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/", None);
        assert_eq!(text_of_id(&dom, "t").unwrap(), "CTB", "캡처 → 타깃 → 버블 순서");
    }

    #[test]
    fn event_init_dict_becomes_properties() {
        // init 딕셔너리의 모든 멤버가 이벤트 프로퍼티가 된다 (§2.2). 예전엔 detail/bubbles
        // 만 베껴서 KeyboardEvent 의 key, MouseEvent 의 clientX 가 통째로 사라졌다.
        let mut dom = crate::html::parse_dom(
            "<p id=\"t\">?</p>\
             <script>\
             var got = '';\
             document.addEventListener('keydown', function (e) { got = e.key + ',' + e.ctrlKey });\
             document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', ctrlKey: true }));\
             document.getElementById('t').textContent = got;\
             </script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/", None);
        assert_eq!(text_of_id(&dom, "t").unwrap(), "Enter,true");
    }

    #[test]
    fn nomodule_script_is_not_executed() {
        // HTML 표준 §4.12.1: nomodule 이 붙은 스크립트는 **모듈을 지원하는 브라우저에서
        // 실행하지 않는다**. 우리는 ESM 을 구현하므로 건너뛴다. 예전엔 레거시 폴리필
        // 번들(core-js)을 그대로 실행해서 최신 코드와 충돌하며 죽었다 (react.dev).
        let mut dom = crate::html::parse_dom(
            "<p id=\"t\">ok</p>\
             <script nomodule>document.getElementById('t').textContent = '폴리필 실행됨';</script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/", None);
        assert_eq!(text_of_id(&dom, "t").unwrap(), "ok", "nomodule 스크립트는 실행되면 안 된다");
    }

    #[test]
    fn script_mutates_dom_text() {
        let mut dom = crate::html::parse_dom(
            "<p id=\"t\">old</p>\
             <script>document.getElementById('t').textContent = 'new ' + (1 + 2);</script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/", None);
        assert_eq!(text_of_id(&dom, "t").unwrap(), "new 3");
    }

    #[test]
    fn string_indexof_split_args_lastindexof() {
        // indexOf(needle, fromIndex), split(sep, limit), lastIndexOf — 이전엔 인자 무시/미구현.
        let mut dom = crate::html::parse_dom(
            "<p id=\"t\">x</p>\
             <script>document.getElementById('t').textContent = \
             ['abcabc'.indexOf('a',1), 'abcabc'.lastIndexOf('b'), 'a,b,c'.split(',',2).join('|')]\
             .join(';');</script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/", None);
        assert_eq!(text_of_id(&dom, "t").unwrap(), "3;4;a|b");
    }

    #[test]
    fn destructuring_assignment() {
        // [a,b]=[b,a] 스왑 + ({x,y}=o) 객체 — 이전엔 파스에러로 스크립트 전체 사망.
        let mut dom = crate::html::parse_dom(
            "<p id=\"t\">x</p>\
             <script>var a = 1, b = 2; [a, b] = [b, a]; \
             var x, y; ({x, y} = {x: 5, y: 6}); \
             document.getElementById('t').textContent = [a, b, x, y].join(',');</script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/", None);
        assert_eq!(text_of_id(&dom, "t").unwrap(), "2,1,5,6");
    }

    #[test]
    fn instanceof_builtins() {
        // 내장 생성자 instanceof (이전엔 하드코딩표라 Date/Map/Error/RegExp 다 false).
        let mut dom = crate::html::parse_dom(
            "<p id=\"t\">x</p>\
             <script>var c = [\
             [] instanceof Array, new Date() instanceof Date, new Map() instanceof Map, \
             /x/ instanceof RegExp, new TypeError('x') instanceof Error, \
             new TypeError('x') instanceof TypeError, new Error('x') instanceof TypeError, \
             (function(){}) instanceof Function, ({}) instanceof Array]; \
             document.getElementById('t').textContent = c.map(function(b){return b?'1':'0';}).join('');\
             </script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/", None);
        assert_eq!(text_of_id(&dom, "t").unwrap(), "111111010");
    }

    #[test]
    fn to_primitive_valueof_tostring() {
        // 객체 강제변환이 valueOf/toString 을 부른다 (이전엔 [object Object]/NaN).
        let mut dom = crate::html::parse_dom(
            "<p id=\"t\">x</p>\
             <script>var money = { valueOf: function(){ return 5; } }; \
             var d = { toString: function(){ return 'DAY'; } }; \
             document.getElementById('t').textContent = (money + 1) + '|' + (`${d}`) + '|' + [1,2,3];\
             </script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/", None);
        assert_eq!(text_of_id(&dom, "t").unwrap(), "6|DAY|1,2,3");
    }

    #[test]
    fn math_round_and_minmax_nan() {
        // Math.round 는 floor(x+0.5)(반올림 +∞ 방향), min/max 는 NaN 전파 — 스펙대로.
        let mut dom = crate::html::parse_dom(
            "<p id=\"t\">x</p>\
             <script>document.getElementById('t').textContent = \
             [Math.round(-2.5), Math.round(2.5), Math.min(1, NaN), Math.max(1, NaN)].join(',');\
             </script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/", None);
        assert_eq!(text_of_id(&dom, "t").unwrap(), "-2,3,NaN,NaN");
    }

    #[test]
    fn new_promise_executor_runs_and_then_fires() {
        // new Promise(executor) — executor 동기 실행 + resolve → then 마이크로태스크.
        // 이전엔 executor 가 아예 안 불리고 non-thenable 쓰레기 객체 반환.
        let mut dom = crate::html::parse_dom(
            "<p id=\"t\">x</p>\
             <script>new Promise(function(resolve){ resolve('ok'); })\
             .then(function(v){ document.getElementById('t').textContent = v + '!'; });</script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/", None);
        assert_eq!(text_of_id(&dom, "t").unwrap(), "ok!");
    }

    #[test]
    fn let_for_loop_per_iteration_binding() {
        // for(let i…) 클로저가 각 반복 값을 포착 → [0,1,2] (이전엔 공유 바인딩 [3,3,3]).
        // var 는 함수 스코프 단일 바인딩이라 [3,3,3] 유지.
        let mut dom = crate::html::parse_dom(
            "<p id=\"t\">x</p>\
             <script>var a = [], b = []; \
             for (let i = 0; i < 3; i++) a.push(function(){ return i; }); \
             for (var j = 0; j < 3; j++) b.push(function(){ return j; }); \
             document.getElementById('t').textContent = \
             [a[0](),a[1](),a[2]()].join('') + '|' + [b[0](),b[1](),b[2]()].join('');</script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/", None);
        assert_eq!(text_of_id(&dom, "t").unwrap(), "012|333");
    }

    #[test]
    fn string_escapes_unicode_hex_and_continuation() {
        // \uHHHH, \xHH, \u{...}, 줄 이음 — 이전엔 \u→"u0041" 로 문자열 손상.
        let mut dom = crate::html::parse_dom(
            "<p id=\"t\">x</p>\
             <script>var s = \"\\u0041\\x42\\u{43}\"; var lc = \"a\\\nb\"; \
             document.getElementById('t').textContent = s + '|' + lc;</script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/", None);
        assert_eq!(text_of_id(&dom, "t").unwrap(), "ABC|ab");
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
        run_scripts(&mut dom, "https://localhost/", None);
        assert_eq!(
            text_of_id(&dom, "t").unwrap(),
            "a%20b%2Fc%3Fd%3D%ED%95%9C|a b/c?d=\u{d55c}"
        );
    }

    #[test]
    fn anchor_href_reflects_absolute() {
        // element.href 는 절대 URL 반사, getAttribute('href') 는 원문.
        let mut dom = crate::html::parse_dom(
            "<a id=\"lnk\" href=\"/foo/bar?q=1\">L</a><p id=\"t\">x</p>\
             <script>var a = document.getElementById('lnk'); \
             document.getElementById('t').textContent = a.href + ' ' + a.getAttribute('href');\
             </script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/", None);
        assert_eq!(
            text_of_id(&dom, "t").unwrap(),
            "https://localhost/foo/bar?q=1 /foo/bar?q=1"
        );
    }

    #[test]
    fn parentnode_append_prepend() {
        // append/prepend (가변 인자, 문자열→텍스트 노드). 순서 확인.
        let mut dom = crate::html::parse_dom(
            "<div id=\"t\"><span>mid</span></div>\
             <script>var d = document.getElementById('t'); \
             d.append('Z'); d.prepend('A');</script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/", None);
        assert_eq!(text_of_id(&dom, "t").unwrap(), "AmidZ");
    }

    #[test]
    fn url_constructor_and_search_params() {
        // new URL(...) 핵심 프로퍼티 + searchParams.get (%20 디코드).
        let mut dom = crate::html::parse_dom(
            "<p id=\"t\">x</p>\
             <script>var u = new URL('https://ex.com:8080/a/b?x=1&y=two%20words#frag'); \
             document.getElementById('t').textContent = [u.protocol, u.hostname, u.port, \
             u.pathname, u.search, u.hash, u.searchParams.get('x'), u.searchParams.get('y')].join('|');\
             </script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/", None);
        assert_eq!(
            text_of_id(&dom, "t").unwrap(),
            "https:|ex.com|8080|/a/b|?x=1&y=two%20words|#frag|1|two words"
        );
    }

    #[test]
    fn url_search_params_mutation() {
        // searchParams.set/append/delete — 쿼리 조작.
        let mut dom = crate::html::parse_dom(
            "<p id=\"t\">x</p>\
             <script>var u = new URL('https://ex.com/p?a=1&b=2'); \
             u.searchParams.set('a','9'); u.searchParams.append('c','3'); u.searchParams.delete('b'); \
             document.getElementById('t').textContent = u.searchParams.toString();</script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/", None);
        assert_eq!(text_of_id(&dom, "t").unwrap(), "a=9&c=3");
    }

    #[test]
    fn url_resolves_against_base() {
        // new URL(relative, base) — 상대 경로를 base 로 해석.
        let mut dom = crate::html::parse_dom(
            "<p id=\"t\">x</p>\
             <script>var u = new URL('/foo?a=1', 'https://ex.com/bar'); \
             document.getElementById('t').textContent = u.href;</script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/", None);
        assert_eq!(text_of_id(&dom, "t").unwrap(), "https://ex.com/foo?a=1");
    }

    #[test]
    fn return_newline_expr_asi() {
        // `return` 뒤 개행이면 값을 안 받는다(ASI 제약 생성물) — `return; 42;` 로 파싱.
        // 키워드 휴리스틱으론 못 잡던 케이스(42 는 키워드 아님) — 개행 기반 표준 ASI.
        let mut dom = crate::html::parse_dom(
            "<p id=\"t\">x</p>\
             <script>function f(){ return\n 42; } \
             document.getElementById('t').textContent = String(f());</script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/", None);
        assert_eq!(text_of_id(&dom, "t").unwrap(), "undefined");
    }

    #[test]
    fn same_line_return_expr_still_works() {
        // 같은 줄 `return expr` 은 정상 (ASI 미적용).
        let mut dom = crate::html::parse_dom(
            "<p id=\"t\">x</p>\
             <script>function f(){ return 42; } \
             document.getElementById('t').textContent = String(f());</script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/", None);
        assert_eq!(text_of_id(&dom, "t").unwrap(), "42");
    }

    #[test]
    fn bare_return_then_declaration_asi() {
        // `if (!x) return` 뒤 개행 + const 선언 — ASI(값 없는 return). 렉서가 개행을
        // 안 남겨도 식을 시작 못 하는 키워드(const)로 판별. 조기 반환 흔한 패턴.
        let mut dom = crate::html::parse_dom(
            "<p id=\"t\">x</p>\
             <script>function g(x){ if(!x) return\nconst z = 7; return z; } \
             document.getElementById('t').textContent = String(g(true));</script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/", None);
        assert_eq!(text_of_id(&dom, "t").unwrap(), "7");
    }

    #[test]
    fn trailing_comma_in_call_args() {
        // f(2, 3,) — 함수 호출 인자의 트레일링 콤마 (ES2017+, 번들러 코드에 흔함)
        let mut dom = crate::html::parse_dom(
            "<p id=\"t\">x</p>\
             <script>function f(a, b) { return a + b; } \
             document.getElementById('t').textContent = String(f(2, 3,));</script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/", None);
        assert_eq!(text_of_id(&dom, "t").unwrap(), "5");
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
        run_scripts(&mut dom, "https://localhost/", None);
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
        run_scripts(&mut dom, "https://localhost/", None);
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
        run_scripts(&mut dom, "https://localhost/", None);
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
        run_scripts(&mut dom, "https://localhost/", None);
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
        run_scripts(&mut dom, "https://localhost/", None);
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
        run_scripts(&mut dom, "https://localhost/", None);
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
        run_scripts(&mut dom, "https://localhost/", None);
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
        run_scripts(&mut dom, "https://localhost/", None);
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
        run_scripts(&mut dom, "https://localhost/", None);
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
        run_scripts(&mut dom, "https://localhost/", None);
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
        run_scripts(&mut dom, "https://localhost/", None);
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
        run_scripts(&mut dom, "https://localhost/", None);
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
        run_scripts(&mut dom, "https://localhost/", None);
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
        run_scripts(&mut dom, "https://localhost/", None);
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
        run_scripts(&mut dom, "https://localhost/", None);
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
        run_scripts(&mut dom, "https://localhost/", None);
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
        run_scripts(&mut dom, "https://localhost/", None);
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
        run_scripts(&mut dom, "https://localhost/", None);
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
        run_scripts(&mut dom, "https://localhost/", None);
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
        run_scripts(&mut dom, "https://localhost/", None);
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
        run_scripts(&mut dom, "https://localhost/", None);
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
        run_scripts(&mut dom, "https://localhost/", None);
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
        run_scripts(&mut dom, "https://localhost/", None);
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
        run_scripts(&mut dom, "https://localhost/", None);
        assert_eq!(text_of_id(&dom, "a").unwrap(), "left+right");
    }
}
