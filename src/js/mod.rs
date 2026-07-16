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
    for (idx, (node, src)) in sources.iter().enumerate() {
        let code = src.strip_prefix(EXT_TAG).unwrap_or(src);
        // document.write 의 삽입 지점 = 지금 실행 중인 스크립트 자리 (파서 삽입 지점)
        it.current_script = *node;
        let t0 = std::time::Instant::now();
        if std::env::var("KESTREL_TIME").is_ok() {
            eprintln!("[time] script #{} 시작 ({}바이트)", idx, code.len());
        }
        if let Err(e) = it.run(code) {
            println!("[js error] {}", e);
        }
        if std::env::var("KESTREL_TIME").is_ok() {
            eprintln!(
                "[time] script #{} ({}바이트) {:.0}ms",
                idx,
                code.len(),
                t0.elapsed().as_secs_f64() * 1000.0
            );
        }
        it.drain_microtasks();
        // WebSocket: 열린 소켓의 open/message 를 배달한다 (핸들러 등록 후, 스크립트 사이).
        it.pump_websockets();
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
    it.pump_websockets();
    it.set_ready_state("complete");
    it.fire_global("load");
    // 마지막으로 한 번 더: load 핸들러가 방금 연 소켓의 첫 메시지도 받는다.
    it.pump_websockets();
    it.drain_microtasks();
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
// toLocaleLowerCase/toLocaleUpperCase (§22.1.3.24/.25): Intl 없으면 로케일 독립
// 대소문자 매핑(= toLowerCase/toUpperCase). RequireObjectCoercible(this) 후 ToString.
if (!String.prototype.toLocaleLowerCase) String.prototype.toLocaleLowerCase = function(){
  if (this === null || this === undefined) throw new TypeError('String.prototype.toLocaleLowerCase called on null or undefined');
  return String(this).toLowerCase();
};
if (!String.prototype.toLocaleUpperCase) String.prototype.toLocaleUpperCase = function(){
  if (this === null || this === undefined) throw new TypeError('String.prototype.toLocaleUpperCase called on null or undefined');
  return String(this).toUpperCase();
};
// 함수 name 은 표준상 메서드명 (§10.2.9). 프로퍼티 대입은 NamedEvaluation 이 안 되므로 명시.
Object.defineProperty(String.prototype.toLocaleLowerCase, 'name', { value: 'toLocaleLowerCase', configurable: true });
Object.defineProperty(String.prototype.toLocaleUpperCase, 'name', { value: 'toLocaleUpperCase', configurable: true });
if (!Object.hasOwn) Object.hasOwn = function(o, k){ return Object.prototype.hasOwnProperty.call(o, k); };
// Annex B 레거시 접근자 (§B.2.2): __defineGetter__/__defineSetter__/__lookupGetter__/
// __lookupSetter__. defineProperty/gOPD 위에 얹는다. 비열거·구성가능 데이터 프로퍼티.
if (!Object.prototype.__defineGetter__) {
  Object.defineProperty(Object.prototype, '__defineGetter__', { value: function(k, fn){
    if (typeof fn !== 'function') throw new TypeError('Object.prototype.__defineGetter__: Expecting function');
    Object.defineProperty(Object(this), k, { get: fn, enumerable: true, configurable: true });
  }, writable: true, enumerable: false, configurable: true });
  Object.defineProperty(Object.prototype, '__defineSetter__', { value: function(k, fn){
    if (typeof fn !== 'function') throw new TypeError('Object.prototype.__defineSetter__: Expecting function');
    Object.defineProperty(Object(this), k, { set: fn, enumerable: true, configurable: true });
  }, writable: true, enumerable: false, configurable: true });
  Object.defineProperty(Object.prototype, '__lookupGetter__', { value: function(k){
    var o = Object(this);
    while (o !== null && o !== undefined) {
      var d = Object.getOwnPropertyDescriptor(o, k);
      if (d !== undefined) return d.get;   // 접근자면 get, 데이터면 undefined
      o = Object.getPrototypeOf(o);
    }
    return undefined;
  }, writable: true, enumerable: false, configurable: true });
  Object.defineProperty(Object.prototype, '__lookupSetter__', { value: function(k){
    var o = Object(this);
    while (o !== null && o !== undefined) {
      var d = Object.getOwnPropertyDescriptor(o, k);
      if (d !== undefined) return d.set;
      o = Object.getPrototypeOf(o);
    }
    return undefined;
  }, writable: true, enumerable: false, configurable: true });
}
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
  // 기록은 **그 변경이 일어난 시점에 등록돼 있던** 옵저버에게만 간다 (표준).
  // 아직 배분 안 된 기록을 지금 등록된 옵저버들에게 먼저 넘긴 뒤에 등록한다 —
  // 안 그러면 새 옵저버가 자기가 생기기 전의 변경까지 받아 간다.
  __kDrainMutations();
  var o = opts || {};
  if (!o.childList && !o.attributes && !o.characterData) o.childList = true; // 기본
  this._regs.push({ t: target, o: o });
};
__kMutationObserver.prototype.disconnect = function(){ this._regs = []; this._q = []; };
// 엔진의 대기 기록을 각 옵저버의 큐로 옮긴다 (표준의 "queue a mutation record").
// 예전엔 이게 없어서 옵저버의 _q 가 **영영 비어 있었다** — takeRecords() 가 항상
// 빈 배열이었다 (그리고 콜백 배달은 큐를 우회해 직접 호출했다).
function __kDrainMutations() {
  var recs = __kTakeMutations();
  if (!recs || !recs.length) return;
  for (var i = 0; i < __kMutObs.length; i++) {
    var ob = __kMutObs[i];
    for (var j = 0; j < recs.length; j++) {
      if (ob._match(recs[j])) ob._q.push(recs[j]);
    }
  }
}
__kMutationObserver.prototype.takeRecords = function(){
  __kDrainMutations();
  var r = this._q; this._q = []; return r;
};
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
  __kDrainMutations();
  for (var i = 0; i < __kMutObs.length; i++) {
    var ob = __kMutObs[i];
    if (ob._q.length && ob._cb) {
      var out = ob._q;
      ob._q = [];
      ob._cb(out, ob);
    }
  }
}
var MutationObserver = window.MutationObserver; if (!MutationObserver) { MutationObserver = __kMutationObserver; window.MutationObserver = MutationObserver; }
var IntersectionObserver = window.IntersectionObserver; if (!IntersectionObserver) { IntersectionObserver = __kIntersectionObserver; window.IntersectionObserver = IntersectionObserver; }
var ResizeObserver = window.ResizeObserver; if (!ResizeObserver) { ResizeObserver = __kResizeObserver; window.ResizeObserver = ResizeObserver; }
var PerformanceObserver = window.PerformanceObserver; if (!PerformanceObserver) { PerformanceObserver = __kObs; window.PerformanceObserver = PerformanceObserver; }
// matchMedia 는 엔진이 CSS @media 와 같은 평가기로 제공(Native). 스텁 불필요.
window.matchMedia = matchMedia;

// Blob / File / FileReader / URL.createObjectURL (File API).
// 사이트가 기능 탐지로 typeof Blob 을 보고, 있으면 그 경로를 탄다 — 없으면 죽는다.
// 텍스트 조각만 다루는 최소 구현이다 (바이너리는 문자열로 보관).
if (typeof Blob === "undefined") {
  window.Blob = function (parts, opts) {
    var s = "";
    var list = parts || [];
    for (var i = 0; i < list.length; i++) {
      var p = list[i];
      s += (p && p.__text !== undefined) ? p.__text : String(p);
    }
    this.__text = s;
    this.size = s.length;
    this.type = (opts && opts.type) || "";
  };
  Blob.prototype.text = function () { var t = this.__text; return Promise.resolve(t); };
  Blob.prototype.slice = function (a, b, type) {
    return new Blob([this.__text.slice(a === undefined ? 0 : a, b)], { type: type || this.type });
  };
  Blob.prototype.arrayBuffer = function () {
    var t = this.__text;
    var buf = new ArrayBuffer(t.length);
    var view = new Uint8Array(buf);
    for (var i = 0; i < t.length; i++) view[i] = t.charCodeAt(i) & 0xff;
    return Promise.resolve(buf);
  };
  window.File = function (parts, name, opts) {
    Blob.call(this, parts, opts);
    this.name = name;
    this.lastModified = 0;
  };
  File.prototype = Object.create(Blob.prototype);
}
if (typeof FileReader === "undefined") {
  window.FileReader = function () { this.result = null; this.onload = null; this.onloadend = null; };
  FileReader.prototype.readAsText = function (blob) {
    this.result = blob && blob.__text !== undefined ? blob.__text : String(blob);
    var self = this;
    queueMicrotask(function () {
      var e = { target: self };
      if (self.onload) self.onload(e);
      if (self.onloadend) self.onloadend(e);
    });
  };
  FileReader.prototype.readAsDataURL = function (blob) {
    var t = blob && blob.__text !== undefined ? blob.__text : String(blob);
    this.result = "data:" + ((blob && blob.type) || "text/plain") + ";base64," + btoa(t);
    var self = this;
    queueMicrotask(function () {
      var e = { target: self };
      if (self.onload) self.onload(e);
      if (self.onloadend) self.onloadend(e);
    });
  };
}
// URL.createObjectURL: 실제 네트워크가 아니라 data: URL 로 만든다 (내용이 살아 있어야
// <img src=blobUrl> 이 뜬다). 못 만들면 조용히 빈 문자열을 주지 않고 정직하게 blob: 를 준다.
if (typeof URL !== "undefined" && !URL.createObjectURL) {
  URL.createObjectURL = function (blob) {
    try {
      var t = blob && blob.__text !== undefined ? blob.__text : "";
      return "data:" + ((blob && blob.type) || "application/octet-stream") + ";base64," + btoa(t);
    } catch (e) {
      return "blob:kestrel/unsupported";
    }
  };
  URL.revokeObjectURL = function () {};
}

// Storage 인터페이스 전역 (표준). 사이트가 typeof Storage !== 'undefined' 로 탐지한다.
if (typeof Storage === "undefined") {
  window.Storage = function () {};
}

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
    getEntries: function(){ return []; },
    clearMarks: function(){ __kMarks = {}; },
    clearMeasures: __kNoop
  };
  // performance.timing (레거시 Navigation Timing). 분석 스크립트가 아직 대량으로 읽는다.
  // 없으면 t.navigationStart 한 줄에서 죽는다 (bun.sh 가 그랬다).
  window.performance.timing = {
    navigationStart: __kT0, fetchStart: __kT0, domainLookupStart: __kT0,
    domainLookupEnd: __kT0, connectStart: __kT0, connectEnd: __kT0,
    requestStart: __kT0, responseStart: __kT0, responseEnd: __kT0,
    domLoading: __kT0, domInteractive: __kT0, domContentLoadedEventStart: __kT0,
    domContentLoadedEventEnd: __kT0, domComplete: __kT0,
    loadEventStart: __kT0, loadEventEnd: __kT0, unloadEventStart: 0, unloadEventEnd: 0,
    redirectStart: 0, redirectEnd: 0, secureConnectionStart: 0
  };
  window.performance.navigation = { type: 0, redirectCount: 0 };
}
var performance = window.performance;

// ── Iterator Helpers (ES2025) ──────────────────────────────────────────────
// arr.values().find(…) / .map(…).toArray() 처럼 이터레이터에 직접 거는 메서드들.
// 크롬이 이미 출시했고 사이트가 쓴다 (astro.build). 없으면 undefined 호출로 죽는다.
// 네이티브 이터레이터 객체가 이 객체를 __proto__ 로 단다.
var __kIterProto = {
  next: undefined, // 각 이터레이터 자신의 next 가 가린다
  map: function(fn){ var out = []; var i = 0, r;
    while (!(r = this.next()).done) out.push(fn(r.value, i++));
    return __kIterOf(out); },
  filter: function(fn){ var out = []; var i = 0, r;
    while (!(r = this.next()).done) { if (fn(r.value, i++)) out.push(r.value); }
    return __kIterOf(out); },
  find: function(fn){ var i = 0, r;
    while (!(r = this.next()).done) { if (fn(r.value, i++)) return r.value; }
    return undefined; },
  forEach: function(fn){ var i = 0, r;
    while (!(r = this.next()).done) fn(r.value, i++); },
  some: function(fn){ var i = 0, r;
    while (!(r = this.next()).done) { if (fn(r.value, i++)) return true; }
    return false; },
  every: function(fn){ var i = 0, r;
    while (!(r = this.next()).done) { if (!fn(r.value, i++)) return false; }
    return true; },
  reduce: function(fn, init){ var acc = init, first = arguments.length < 2, r;
    while (!(r = this.next()).done) {
      if (first) { acc = r.value; first = false; } else { acc = fn(acc, r.value); }
    }
    return acc; },
  take: function(n){ var out = [], r;
    while (out.length < n && !(r = this.next()).done) out.push(r.value);
    return __kIterOf(out); },
  drop: function(n){ var out = [], i = 0, r;
    while (!(r = this.next()).done) { if (i++ >= n) out.push(r.value); }
    return __kIterOf(out); },
  flatMap: function(fn){ var out = [], i = 0, r;
    while (!(r = this.next()).done) {
      var v = fn(r.value, i++);
      if (v && typeof v.length === 'number' && typeof v !== 'string') {
        for (var k = 0; k < v.length; k++) out.push(v[k]);
      } else { out.push(v); }
    }
    return __kIterOf(out); },
  toArray: function(){ var out = [], r;
    while (!(r = this.next()).done) out.push(r.value);
    return out; }
};
// 배열 → 이터레이터 (헬퍼들이 돌려주는 것도 이터레이터여야 체이닝된다)
function __kIterOf(arr) {
  var i = 0;
  var it = {
    next: function(){
      return i < arr.length ? { value: arr[i++], done: false } : { value: undefined, done: true };
    }
  };
  it.__proto__ = __kIterProto;
  it[Symbol.iterator] = function(){ return this; };
  return it;
}
window.__kIterProto = __kIterProto;

// ── DOM 인터페이스 생성자 ──────────────────────────────────────────────────
// `el instanceof HTMLAnchorElement`, `class X extends HTMLElement` 처럼 쓰인다.
// 없으면 "HTMLAnchorElement 은(는) 정의되지 않음" 으로 스크립트가 통째로 죽는다.
// instanceof 판정은 표준 메커니즘(Symbol.hasInstance)으로 한다 — 흉내가 아니다.
function __kIface(check) {
  var f = function(){ throw new TypeError('Illegal constructor'); };
  f[Symbol.hasInstance] = function(x){ return !!x && check(x); };
  return f;
}
var __kTagIface = function(tag) {
  return __kIface(function(x){
    return typeof x.tagName === 'string' && x.tagName.toUpperCase() === tag;
  });
};
// DOMException (WebIDL §3.14). DOM 이 표준적으로 던지는 오류 타입이다.
// 없으면 DOM 오류가 전부 평범한 Error 로 나가서, e.name 으로 분기하는 코드가
// 조용히 다른 길로 샌다. code 는 레거시 상수표 (신규 이름은 0).
var __kDOMCodes = {
  IndexSizeError: 1, HierarchyRequestError: 3, WrongDocumentError: 4,
  InvalidCharacterError: 5, NoModificationAllowedError: 7, NotFoundError: 8,
  NotSupportedError: 9, InUseAttributeError: 10, InvalidStateError: 11,
  SyntaxError: 12, InvalidModificationError: 13, NamespaceError: 14,
  InvalidAccessError: 15, TypeMismatchError: 17, SecurityError: 18,
  NetworkError: 19, AbortError: 20, URLMismatchError: 21,
  QuotaExceededError: 22, TimeoutError: 23, InvalidNodeTypeError: 24,
  DataCloneError: 25
};
class DOMException extends Error {
  constructor(message, name) {
    super(message === undefined ? '' : String(message));
    Object.defineProperty(this, 'name', {
      value: name === undefined ? 'Error' : String(name),
      writable: true, enumerable: false, configurable: true
    });
  }
  get code() { return __kDOMCodes[this.name] || 0; }
}
window.DOMException = DOMException;
// 레거시 코드 상수 (생성자와 prototype 양쪽 — WebIDL 이 요구한다)
for (var __dn in __kDOMCodes) {
  var __legacy = {
    IndexSizeError: 'INDEX_SIZE_ERR', HierarchyRequestError: 'HIERARCHY_REQUEST_ERR',
    WrongDocumentError: 'WRONG_DOCUMENT_ERR', InvalidCharacterError: 'INVALID_CHARACTER_ERR',
    NoModificationAllowedError: 'NO_MODIFICATION_ALLOWED_ERR', NotFoundError: 'NOT_FOUND_ERR',
    NotSupportedError: 'NOT_SUPPORTED_ERR', InUseAttributeError: 'INUSE_ATTRIBUTE_ERR',
    InvalidStateError: 'INVALID_STATE_ERR', SyntaxError: 'SYNTAX_ERR',
    InvalidModificationError: 'INVALID_MODIFICATION_ERR', NamespaceError: 'NAMESPACE_ERR',
    InvalidAccessError: 'INVALID_ACCESS_ERR', TypeMismatchError: 'TYPE_MISMATCH_ERR',
    SecurityError: 'SECURITY_ERR', NetworkError: 'NETWORK_ERR', AbortError: 'ABORT_ERR',
    URLMismatchError: 'URL_MISMATCH_ERR', QuotaExceededError: 'QUOTA_EXCEEDED_ERR',
    TimeoutError: 'TIMEOUT_ERR', InvalidNodeTypeError: 'INVALID_NODE_TYPE_ERR',
    DataCloneError: 'DATA_CLONE_ERR'
  }[__dn];
  if (__legacy) {
    DOMException[__legacy] = __kDOMCodes[__dn];
    DOMException.prototype[__legacy] = __kDOMCodes[__dn];
  }
}
// EventTarget 은 **진짜 생성 가능한 클래스**여야 한다 — 사이트가 `class X extends EventTarget`
// 으로 상속한다 (astro.build). 스텁으로 두면 "Illegal constructor" 로 죽는다.
if (!window.EventTarget) {
  window.EventTarget = function EventTarget() {
    Object.defineProperty(this, '__kEvts', { value: {}, enumerable: false, writable: true });
  };
  window.EventTarget.prototype.addEventListener = function(type, fn, opts) {
    if (typeof fn !== 'function') return;
    if (!this.__kEvts) this.__kEvts = {};
    var once = !!(opts && opts.once);
    (this.__kEvts[type] = this.__kEvts[type] || []).push({ fn: fn, once: once });
  };
  window.EventTarget.prototype.removeEventListener = function(type, fn) {
    var l = this.__kEvts && this.__kEvts[type];
    if (!l) return;
    this.__kEvts[type] = l.filter(function(e){ return e.fn !== fn; });
  };
  window.EventTarget.prototype.dispatchEvent = function(evt) {
    var type = evt && evt.type;
    var l = (this.__kEvts && this.__kEvts[type]) || [];
    if (evt && evt.target === undefined) evt.target = this;
    var keep = [];
    for (var i = 0; i < l.length; i++) {
      try { l[i].fn.call(this, evt); } catch (e) { console.error(e); }
      if (!l[i].once) keep.push(l[i]);
    }
    if (this.__kEvts) this.__kEvts[type] = keep;
    return !(evt && evt.defaultPrevented);
  };
  // DOM 노드도 EventTarget 이다 (instanceof 판정은 표준 메커니즘으로)
  window.EventTarget[Symbol.hasInstance] = function(x) {
    return !!x && typeof x.addEventListener === 'function';
  };
}
// 플랫폼 인터페이스 객체 (WebIDL). instanceof 는 **브랜드 상속 체인**으로 판별한다 —
// 오리 판별(프로퍼티가 있나?)이 아니다. 요소는 HTML 표준의 요소-인터페이스 매핑을 따른다:
// <div> → HTMLDivElement → HTMLElement → Element → Node → EventTarget.
function __kMakeIface(name) {
  var f = __kIface(function (x) {
    var chain = __kBrand(x);
    return !!chain && chain.indexOf(name) >= 0;
  });
  Object.defineProperty(f, 'name', { value: name, configurable: true });
  return f;
}
var __kIfaceNames = [
  'EventTarget2', 'Node2', 'Element', 'CharacterData', 'Text', 'Comment', 'Attr',
  'HTMLElement', 'HTMLUnknownElement', 'HTMLAnchorElement', 'HTMLAreaElement',
  'HTMLAudioElement', 'HTMLBaseElement', 'HTMLQuoteElement', 'HTMLBodyElement',
  'HTMLBRElement', 'HTMLButtonElement', 'HTMLCanvasElement', 'HTMLTableCaptionElement',
  'HTMLTableColElement', 'HTMLDataElement', 'HTMLDataListElement', 'HTMLModElement',
  'HTMLDetailsElement', 'HTMLDialogElement', 'HTMLDivElement', 'HTMLDListElement',
  'HTMLEmbedElement', 'HTMLFieldSetElement', 'HTMLFormElement', 'HTMLHeadingElement',
  'HTMLHeadElement', 'HTMLHRElement', 'HTMLHtmlElement', 'HTMLIFrameElement',
  'HTMLImageElement', 'HTMLInputElement', 'HTMLLabelElement', 'HTMLLegendElement',
  'HTMLLIElement', 'HTMLLinkElement', 'HTMLMapElement', 'HTMLMenuElement',
  'HTMLMetaElement', 'HTMLMeterElement', 'HTMLObjectElement', 'HTMLOListElement',
  'HTMLOptGroupElement', 'HTMLOptionElement', 'HTMLOutputElement',
  'HTMLParagraphElement', 'HTMLPictureElement', 'HTMLPreElement', 'HTMLProgressElement',
  'HTMLScriptElement', 'HTMLSelectElement', 'HTMLSlotElement', 'HTMLSourceElement',
  'HTMLSpanElement', 'HTMLStyleElement', 'HTMLTableElement', 'HTMLTableSectionElement',
  'HTMLTableCellElement', 'HTMLTemplateElement', 'HTMLTextAreaElement', 'HTMLTimeElement',
  'HTMLTitleElement', 'HTMLTableRowElement', 'HTMLTrackElement', 'HTMLUListElement',
  'HTMLVideoElement', 'SVGElement', 'SVGSVGElement',
  'CSSStyleSheet', 'StyleSheet', 'CSSStyleRule', 'CSSRule', 'CSSStyleDeclaration'
];
for (var __i = 0; __i < __kIfaceNames.length; __i++) {
  var __n = __kIfaceNames[__i];
  if (__n === 'EventTarget2' || __n === 'Node2') continue; // 아래에서 따로
  window[__n] = __kMakeIface(__n);
}
// Node 는 상수(ELEMENT_NODE 등)를 가진 네임스페이스라 이미 있다 — hasInstance 만 얹는다.
if (window.Node) {
  window.Node[Symbol.hasInstance] = function (x) {
    var chain = __kBrand(x);
    return !!chain && chain.indexOf('Node') >= 0;
  };
}
if (!window.Node) window.Node = __kIface(function(x){ return typeof x.nodeType === 'number'; });
if (!window.Element) window.Element = __kIface(function(x){ return typeof x.tagName === 'string'; });
if (!window.Document) window.Document = __kIface(function(x){ return x.nodeType === 9; });
if (!window.HTMLAnchorElement) window.HTMLAnchorElement = __kTagIface('A');
if (!window.HTMLImageElement) window.HTMLImageElement = __kTagIface('IMG');
if (!window.HTMLInputElement) window.HTMLInputElement = __kTagIface('INPUT');
if (!window.HTMLButtonElement) window.HTMLButtonElement = __kTagIface('BUTTON');
if (!window.HTMLFormElement) window.HTMLFormElement = __kTagIface('FORM');
if (!window.HTMLCanvasElement) window.HTMLCanvasElement = __kTagIface('CANVAS');
if (!window.HTMLScriptElement) window.HTMLScriptElement = __kTagIface('SCRIPT');
if (!window.HTMLSelectElement) window.HTMLSelectElement = __kTagIface('SELECT');
if (!window.HTMLTextAreaElement) window.HTMLTextAreaElement = __kTagIface('TEXTAREA');
if (!window.SVGElement) window.SVGElement = __kIface(function(x){
  return typeof x.tagName === 'string' && ['SVG','PATH','G','CIRCLE','RECT','LINE','POLYGON','POLYLINE','ELLIPSE','USE','TEXT'].indexOf(x.tagName.toUpperCase()) >= 0;
});
var EventTarget = window.EventTarget, Node = window.Node, Element = window.Element;
var HTMLAnchorElement = window.HTMLAnchorElement, HTMLImageElement = window.HTMLImageElement;
var HTMLInputElement = window.HTMLInputElement, HTMLButtonElement = window.HTMLButtonElement;
var SVGElement = window.SVGElement;

// new Image(w, h) — <img> 를 만드는 생성자 (HTML §4.8.4.1). 프리로드/스프라이트에 흔하다.
// DOM Range (§5). 경계점 두 개(노드+오프셋)로 문서의 한 구간을 가리킨다.
// 에디터, 텍스트 선택, 하이라이터, 클립보드가 전부 이걸 쓴다. 없으면 그런 코드가
// document.createRange 한 줄에서 죽는다.
function __kNodeLength(node) {
  var t = node.nodeType;
  if (t === 3 || t === 8) return node.data.length;   // Text / Comment
  return node.childNodes.length;
}
function __kNodeIndex(node) {
  var p = node.parentNode;
  if (!p) return 0;
  var kids = p.childNodes;
  for (var i = 0; i < kids.length; i++) if (kids[i] === node) return i;
  return 0;
}
function __kAncestors(node) {
  var out = [];
  for (var n = node; n; n = n.parentNode) out.push(n);
  return out;
}
// 경계점 (nodeA, offsetA) 가 (nodeB, offsetB) 보다 before(-1)/equal(0)/after(1) 인가 (§5.1)
function __kComparePoints(nodeA, offsetA, nodeB, offsetB) {
  if (nodeA === nodeB) {
    return offsetA < offsetB ? -1 : offsetA > offsetB ? 1 : 0;
  }
  // B 가 A 의 조상인가?
  var ancB = __kAncestors(nodeB);
  var ancA = __kAncestors(nodeA);
  if (ancA.indexOf(nodeB) >= 0) {
    // nodeB 의 자식들 중 nodeA 를 품은 것의 인덱스와 offsetB 비교
    var child = nodeA;
    while (child.parentNode !== nodeB) child = child.parentNode;
    return __kNodeIndex(child) < offsetB ? -1 : 1;
  }
  if (ancB.indexOf(nodeA) >= 0) {
    var child2 = nodeB;
    while (child2.parentNode !== nodeA) child2 = child2.parentNode;
    return __kNodeIndex(child2) < offsetA ? 1 : -1;
  }
  // 공통 조상을 찾아 형제 순서로 비교
  for (var i = 0; i < ancA.length; i++) {
    var j = ancB.indexOf(ancA[i]);
    if (j >= 0) {
      var ca = ancA[i - 1], cb = ancB[j - 1];
      if (!ca || !cb) return 0;
      return __kNodeIndex(ca) < __kNodeIndex(cb) ? -1 : 1;
    }
  }
  return 0; // 서로 다른 트리 — 순서 미정
}
function Range() {
  var d = typeof document !== 'undefined' ? document : null;
  var root = d && d.body ? d.body : null;
  this.startContainer = root;
  this.startOffset = 0;
  this.endContainer = root;
  this.endOffset = 0;
}
Object.defineProperty(Range.prototype, 'collapsed', {
  get: function () {
    return this.startContainer === this.endContainer && this.startOffset === this.endOffset;
  },
  configurable: true
});
Object.defineProperty(Range.prototype, 'commonAncestorContainer', {
  get: function () {
    var a = __kAncestors(this.startContainer);
    for (var n = this.endContainer; n; n = n.parentNode) {
      if (a.indexOf(n) >= 0) return n;
    }
    return null;
  },
  configurable: true
});
Range.prototype.__kSet = function (which, node, offset) {
  if (offset < 0 || offset > __kNodeLength(node)) {
    throw new DOMException('offset 이 범위를 벗어남', 'IndexSizeError');
  }
  if (which === 'start') {
    this.startContainer = node;
    this.startOffset = offset;
    // start 가 end 를 넘어서면 end 를 start 로 당긴다 (표준)
    if (__kComparePoints(node, offset, this.endContainer, this.endOffset) > 0) {
      this.endContainer = node;
      this.endOffset = offset;
    }
  } else {
    this.endContainer = node;
    this.endOffset = offset;
    if (__kComparePoints(this.startContainer, this.startOffset, node, offset) > 0) {
      this.startContainer = node;
      this.startOffset = offset;
    }
  }
};
Range.prototype.setStart = function (node, offset) { this.__kSet('start', node, offset); };
Range.prototype.setEnd = function (node, offset) { this.__kSet('end', node, offset); };
Range.prototype.setStartBefore = function (node) { this.setStart(node.parentNode, __kNodeIndex(node)); };
Range.prototype.setStartAfter = function (node) { this.setStart(node.parentNode, __kNodeIndex(node) + 1); };
Range.prototype.setEndBefore = function (node) { this.setEnd(node.parentNode, __kNodeIndex(node)); };
Range.prototype.setEndAfter = function (node) { this.setEnd(node.parentNode, __kNodeIndex(node) + 1); };
Range.prototype.collapse = function (toStart) {
  if (toStart) { this.endContainer = this.startContainer; this.endOffset = this.startOffset; }
  else { this.startContainer = this.endContainer; this.startOffset = this.endOffset; }
};
Range.prototype.selectNode = function (node) {
  var p = node.parentNode;
  if (!p) throw new DOMException('부모가 없는 노드', 'InvalidNodeTypeError');
  var i = __kNodeIndex(node);
  this.startContainer = p; this.startOffset = i;
  this.endContainer = p; this.endOffset = i + 1;
};
Range.prototype.selectNodeContents = function (node) {
  this.startContainer = node; this.startOffset = 0;
  this.endContainer = node; this.endOffset = __kNodeLength(node);
};
Range.prototype.cloneRange = function () {
  var r = new Range();
  r.startContainer = this.startContainer; r.startOffset = this.startOffset;
  r.endContainer = this.endContainer; r.endOffset = this.endOffset;
  return r;
};
Range.prototype.detach = function () {}; // 표준: 아무 일도 안 한다
Range.START_TO_START = 0; Range.START_TO_END = 1;
Range.END_TO_END = 2; Range.END_TO_START = 3;
Range.prototype.compareBoundaryPoints = function (how, other) {
  var mine, theirs;
  if (how === 0) { mine = [this.startContainer, this.startOffset]; theirs = [other.startContainer, other.startOffset]; }
  else if (how === 1) { mine = [this.endContainer, this.endOffset]; theirs = [other.startContainer, other.startOffset]; }
  else if (how === 2) { mine = [this.endContainer, this.endOffset]; theirs = [other.endContainer, other.endOffset]; }
  else if (how === 3) { mine = [this.startContainer, this.startOffset]; theirs = [other.endContainer, other.endOffset]; }
  else throw new DOMException('알 수 없는 비교 방식', 'NotSupportedError');
  return __kComparePoints(mine[0], mine[1], theirs[0], theirs[1]);
};
Range.prototype.isPointInRange = function (node, offset) {
  return __kComparePoints(node, offset, this.startContainer, this.startOffset) >= 0 &&
         __kComparePoints(node, offset, this.endContainer, this.endOffset) <= 0;
};
Range.prototype.comparePoint = function (node, offset) {
  if (__kComparePoints(node, offset, this.startContainer, this.startOffset) < 0) return -1;
  if (__kComparePoints(node, offset, this.endContainer, this.endOffset) > 0) return 1;
  return 0;
};
Range.prototype.intersectsNode = function (node) {
  var p = node.parentNode;
  if (!p) return false;
  var i = __kNodeIndex(node);
  return __kComparePoints(p, i, this.endContainer, this.endOffset) < 0 &&
         __kComparePoints(p, i + 1, this.startContainer, this.startOffset) > 0;
};
// §5.5 이 구간이 포함하는 노드들 (완전히 포함된 노드만)
Range.prototype.__kContained = function () {
  var out = [];
  var common = this.commonAncestorContainer;
  if (!common) return out;
  var self = this;
  var walk = function (node) {
    var kids = node.childNodes;
    for (var i = 0; i < kids.length; i++) {
      var c = kids[i];
      var p = c.parentNode;
      var idx = __kNodeIndex(c);
      var startsAfter = __kComparePoints(p, idx, self.startContainer, self.startOffset) >= 0;
      var endsBefore = __kComparePoints(p, idx + 1, self.endContainer, self.endOffset) <= 0;
      if (startsAfter && endsBefore) out.push(c);
      else walk(c);
    }
  };
  walk(common);
  return out;
};
Range.prototype.toString = function () {
  var s = this.startContainer, e = this.endContainer;
  if (s === e && s.nodeType === 3) {
    return s.data.slice(this.startOffset, this.endOffset);
  }
  var out = '';
  if (s.nodeType === 3) out += s.data.slice(this.startOffset);
  var contained = this.__kContained();
  for (var i = 0; i < contained.length; i++) {
    var c = contained[i];
    if (c.nodeType === 3) out += c.data;
    else if (c.nodeType === 1) out += c.textContent;
  }
  if (e.nodeType === 3) out += e.data.slice(0, this.endOffset);
  return out;
};
Range.prototype.deleteContents = function () {
  if (this.collapsed) return;
  var s = this.startContainer, e = this.endContainer;
  if (s === e && s.nodeType === 3) {
    s.data = s.data.slice(0, this.startOffset) + s.data.slice(this.endOffset);
    this.collapse(true);
    return;
  }
  var contained = this.__kContained();
  if (e.nodeType === 3) e.data = e.data.slice(this.endOffset);
  if (s.nodeType === 3) s.data = s.data.slice(0, this.startOffset);
  for (var i = 0; i < contained.length; i++) contained[i].remove();
  this.collapse(true);
};
Range.prototype.cloneContents = function () {
  var frag = document.createDocumentFragment();
  var s = this.startContainer, e = this.endContainer;
  if (s === e && s.nodeType === 3) {
    frag.appendChild(document.createTextNode(s.data.slice(this.startOffset, this.endOffset)));
    return frag;
  }
  if (s.nodeType === 3) frag.appendChild(document.createTextNode(s.data.slice(this.startOffset)));
  var contained = this.__kContained();
  for (var i = 0; i < contained.length; i++) frag.appendChild(contained[i].cloneNode(true));
  if (e.nodeType === 3) frag.appendChild(document.createTextNode(e.data.slice(0, this.endOffset)));
  return frag;
};
Range.prototype.extractContents = function () {
  var frag = this.cloneContents();
  this.deleteContents();
  return frag;
};
Range.prototype.insertNode = function (node) {
  var s = this.startContainer;
  if (s.nodeType === 3) {
    // 텍스트 중간이면 쪼갠 뒤 그 사이에 넣는다
    var after = document.createTextNode(s.data.slice(this.startOffset));
    s.data = s.data.slice(0, this.startOffset);
    var parent = s.parentNode;
    var kids = parent.childNodes;
    var idx = __kNodeIndex(s);
    var ref = kids[idx + 1] || null;
    parent.insertBefore(node, ref);
    parent.insertBefore(after, node.nextSibling);
  } else {
    var ref2 = s.childNodes[this.startOffset] || null;
    s.insertBefore(node, ref2);
  }
};
Range.prototype.surroundContents = function (newParent) {
  var frag = this.extractContents();
  this.insertNode(newParent);
  newParent.appendChild(frag);
  this.selectNode(newParent);
};
window.Range = Range;
if (typeof document !== 'undefined') {
  document.createRange = function () { return new Range(); };
}
// DOM Traversal (§6) — NodeFilter / TreeWalker / NodeIterator.
// 스펙 알고리즘을 DOM 원시연산 위에 그대로 옮긴 것이다 (근사 아님).
// 없으면 텍스트를 훑는 코드(하이라이터, 번역기, 접근성 도구)가 통째로 죽는다.
var NodeFilter = {
  FILTER_ACCEPT: 1, FILTER_REJECT: 2, FILTER_SKIP: 3,
  SHOW_ALL: 0xFFFFFFFF,
  SHOW_ELEMENT: 0x1, SHOW_ATTRIBUTE: 0x2, SHOW_TEXT: 0x4,
  SHOW_CDATA_SECTION: 0x8, SHOW_ENTITY_REFERENCE: 0x10, SHOW_ENTITY: 0x20,
  SHOW_PROCESSING_INSTRUCTION: 0x40, SHOW_COMMENT: 0x80, SHOW_DOCUMENT: 0x100,
  SHOW_DOCUMENT_TYPE: 0x200, SHOW_DOCUMENT_FRAGMENT: 0x400, SHOW_NOTATION: 0x800
};
window.NodeFilter = NodeFilter;

// §6.1 filter 실행: whatToShow 비트마스크 → 콜백(함수 또는 {acceptNode})
function __kFilterNode(walker, node) {
  var n = node.nodeType;
  if (!(walker.whatToShow & (1 << (n - 1)))) return NodeFilter.FILTER_SKIP;
  var f = walker.filter;
  if (f === null || f === undefined) return NodeFilter.FILTER_ACCEPT;
  var r = typeof f === 'function' ? f(node) : f.acceptNode(node);
  return r;
}

function TreeWalker(root, whatToShow, filter) {
  this.root = root;
  this.whatToShow = whatToShow === undefined ? NodeFilter.SHOW_ALL : (whatToShow >>> 0);
  this.filter = filter === undefined ? null : filter;
  this.currentNode = root;
}
// §6.2 traverseChildren
TreeWalker.prototype.__kChildren = function (first) {
  var node = first ? this.currentNode.firstChild : this.currentNode.lastChild;
  while (node) {
    var result = __kFilterNode(this, node);
    if (result === NodeFilter.FILTER_ACCEPT) { this.currentNode = node; return node; }
    if (result === NodeFilter.FILTER_SKIP) {
      var child = first ? node.firstChild : node.lastChild;
      if (child) { node = child; continue; }
    }
    // 형제로, 없으면 조상을 타고 올라가며 형제를 찾는다
    while (node) {
      var sibling = first ? node.nextSibling : node.previousSibling;
      if (sibling) { node = sibling; break; }
      var parent = node.parentNode;
      if (!parent || parent === this.root || parent === this.currentNode) return null;
      node = parent;
      if (node === this.root) return null;
    }
    if (!node) return null;
  }
  return null;
};
TreeWalker.prototype.firstChild = function () { return this.__kChildren(true); };
TreeWalker.prototype.lastChild = function () { return this.__kChildren(false); };
// §6.2 traverseSiblings
TreeWalker.prototype.__kSiblings = function (next) {
  var node = this.currentNode;
  if (node === this.root) return null;
  while (true) {
    var sibling = next ? node.nextSibling : node.previousSibling;
    while (sibling) {
      node = sibling;
      var result = __kFilterNode(this, node);
      if (result === NodeFilter.FILTER_ACCEPT) { this.currentNode = node; return node; }
      sibling = next ? node.firstChild : node.lastChild;
      if (result === NodeFilter.FILTER_REJECT || !sibling) {
        sibling = next ? node.nextSibling : node.previousSibling;
      }
    }
    node = node.parentNode;
    if (!node || node === this.root) return null;
    if (__kFilterNode(this, node) === NodeFilter.FILTER_ACCEPT) return null;
  }
};
TreeWalker.prototype.nextSibling = function () { return this.__kSiblings(true); };
TreeWalker.prototype.previousSibling = function () { return this.__kSiblings(false); };
TreeWalker.prototype.parentNode = function () {
  var node = this.currentNode;
  while (node && node !== this.root) {
    node = node.parentNode;
    if (node && __kFilterNode(this, node) === NodeFilter.FILTER_ACCEPT) {
      this.currentNode = node;
      return node;
    }
  }
  return null;
};
TreeWalker.prototype.nextNode = function () {
  var node = this.currentNode;
  var result = NodeFilter.FILTER_ACCEPT;
  while (true) {
    while (result !== NodeFilter.FILTER_REJECT && node.firstChild) {
      node = node.firstChild;
      result = __kFilterNode(this, node);
      if (result === NodeFilter.FILTER_ACCEPT) { this.currentNode = node; return node; }
    }
    var sibling = null;
    var temporary = node;
    while (temporary) {
      if (temporary === this.root) return null;
      sibling = temporary.nextSibling;
      if (sibling) break;
      temporary = temporary.parentNode;
    }
    if (!sibling) return null;
    node = sibling;
    result = __kFilterNode(this, node);
    if (result === NodeFilter.FILTER_ACCEPT) { this.currentNode = node; return node; }
  }
};
TreeWalker.prototype.previousNode = function () {
  var node = this.currentNode;
  while (node !== this.root) {
    var sibling = node.previousSibling;
    while (sibling) {
      node = sibling;
      var result = __kFilterNode(this, node);
      while (result !== NodeFilter.FILTER_REJECT && node.lastChild) {
        node = node.lastChild;
        result = __kFilterNode(this, node);
      }
      if (result === NodeFilter.FILTER_ACCEPT) { this.currentNode = node; return node; }
      sibling = node.previousSibling;
    }
    if (node === this.root || !node.parentNode) return null;
    node = node.parentNode;
    if (__kFilterNode(this, node) === NodeFilter.FILTER_ACCEPT) {
      this.currentNode = node;
      return node;
    }
  }
  return null;
};
window.TreeWalker = TreeWalker;

// §6.1 NodeIterator — 전위 순회 + 포인터가 참조 노드 '앞/뒤' 중 어디인지 기억
function NodeIterator(root, whatToShow, filter) {
  this.root = root;
  this.whatToShow = whatToShow === undefined ? NodeFilter.SHOW_ALL : (whatToShow >>> 0);
  this.filter = filter === undefined ? null : filter;
  this.referenceNode = root;
  this.pointerBeforeReferenceNode = true;
}
function __kNextInOrder(node, root) {
  if (node.firstChild) return node.firstChild;
  var n = node;
  while (n && n !== root) {
    if (n.nextSibling) return n.nextSibling;
    n = n.parentNode;
  }
  return null;
}
function __kPrevInOrder(node, root) {
  if (node === root) return null;
  var p = node.previousSibling;
  if (!p) return node.parentNode;
  while (p.lastChild) p = p.lastChild;
  return p;
}
NodeIterator.prototype.__kTraverse = function (forward) {
  var node = this.referenceNode;
  var before = this.pointerBeforeReferenceNode;
  while (true) {
    if (forward) {
      if (before) { before = false; }
      else {
        var nx = __kNextInOrder(node, this.root);
        if (!nx) return null;
        node = nx;
      }
    } else {
      if (!before) { before = true; }
      else {
        var pv = __kPrevInOrder(node, this.root);
        if (!pv) return null;
        node = pv;
      }
    }
    if (__kFilterNode(this, node) === NodeFilter.FILTER_ACCEPT) {
      this.referenceNode = node;
      this.pointerBeforeReferenceNode = before;
      return node;
    }
  }
};
NodeIterator.prototype.nextNode = function () { return this.__kTraverse(true); };
NodeIterator.prototype.previousNode = function () { return this.__kTraverse(false); };
NodeIterator.prototype.detach = function () {}; // 표준: 아무 일도 안 한다 (레거시)
window.NodeIterator = NodeIterator;

if (typeof document !== 'undefined') {
  document.createTreeWalker = function (root, whatToShow, filter) {
    if (root === undefined || root === null) {
      throw new TypeError('createTreeWalker: root 가 필요하다');
    }
    return new TreeWalker(root, whatToShow, filter);
  };
  document.createNodeIterator = function (root, whatToShow, filter) {
    if (root === undefined || root === null) {
      throw new TypeError('createNodeIterator: root 가 필요하다');
    }
    return new NodeIterator(root, whatToShow, filter);
  };
}
// document.createEvent (DOM §4.5.1). 레거시지만 표준이고 아직 널리 쓰인다.
// 표준이 정한 인터페이스 이름만 받고, 그 외에는 NotSupportedError 를 던진다 —
// 아무 이름이나 받아 빈 Event 를 주면 조용히 다른 동작이 된다.
if (typeof document !== 'undefined' && !document.createEvent) {
  // DOM §4.5.1 의 createEvent 표 그대로다. 이름 → 인터페이스.
  // 예전엔 이름 목록만 갖고 전부 Event 로 만들었다 — MouseEvent 를 달라고 해도
  // Event 가 나왔고, instanceof 로 구분할 수도 없었다.
  var __kEventIfaces = {
    beforeunloadevent: 'BeforeUnloadEvent',
    compositionevent: 'CompositionEvent',
    customevent: 'CustomEvent',
    devicemotionevent: 'DeviceMotionEvent',
    deviceorientationevent: 'DeviceOrientationEvent',
    dragevent: 'DragEvent',
    event: 'Event',
    events: 'Event',
    focusevent: 'FocusEvent',
    hashchangeevent: 'HashChangeEvent',
    htmlevents: 'Event',
    keyboardevent: 'KeyboardEvent',
    messageevent: 'MessageEvent',
    mouseevent: 'MouseEvent',
    mouseevents: 'MouseEvent',
    storageevent: 'StorageEvent',
    svgevents: 'Event',
    textevent: 'CompositionEvent',
    touchevent: 'TouchEvent',
    uievent: 'UIEvent',
    uievents: 'UIEvent'
  };
  document.createEvent = function (iface) {
    var name = __kEventIfaces[String(iface).toLowerCase()];
    if (!name) {
      throw new DOMException(
        "The provided value '" + iface + "' is not a valid event interface name.",
        'NotSupportedError'
      );
    }
    var e = new window[name]('');
    // createEvent 로 만든 이벤트는 초기화 전까지 dispatch 할 수 없다 (표준의
    // initialized 플래그). initEvent 가 그 플래그를 세운다.
    e.__kInitialized = false;
    e.initEvent = function (type, bubbles, cancelable) {
      this.type = String(type);
      this.bubbles = !!bubbles;
      this.cancelable = !!cancelable;
      this.__kInitialized = true;
      return undefined;
    };
    e.initCustomEvent = function (type, bubbles, cancelable, detail) {
      this.initEvent(type, bubbles, cancelable);
      this.detail = detail;
    };
    e.initUIEvent = function (type, bubbles, cancelable, view, detail) {
      this.initEvent(type, bubbles, cancelable);
      this.view = view;
      this.detail = detail;
    };
    return e;
  };
}
// 없으면 `new Image()` 한 줄에 스크립트가 죽는다.
if (!window.Image) {
  window.Image = function(w, h) {
    var img = document.createElement('img');
    if (w !== undefined) img.setAttribute('width', String(w));
    if (h !== undefined) img.setAttribute('height', String(h));
    return img;
  };
}
var Image = window.Image;
// Audio 도 같은 꼴 (미디어는 재생하지 않지만 객체는 있어야 죽지 않는다)
if (!window.Audio) {
  window.Audio = function(src) {
    var a = document.createElement('audio');
    if (src !== undefined) a.setAttribute('src', String(src));
    return a;
  };
}

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
// entries/keys/values 는 이터레이터다 (표준). 그리고 URLSearchParams 자신도 이터러블이다 —
// [...params] / for-of 가 표준 관용구인데 예전엔 통째로 안 됐다.
__kURLSearchParams.prototype.entries = function(){
  var arr = this._p.map(function(e){ return [e[0], e[1]]; });
  return arr[Symbol.iterator]();
};
__kURLSearchParams.prototype.keys = function(){
  return this._p.map(function(e){ return e[0]; })[Symbol.iterator]();
};
__kURLSearchParams.prototype.values = function(){
  return this._p.map(function(e){ return e[1]; })[Symbol.iterator]();
};
__kURLSearchParams.prototype[Symbol.iterator] = function(){ return this.entries(); };
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
function __kArrayBuffer(len, opts) {
  len = (len === undefined) ? 0 : (len | 0);
  if (len < 0) throw new RangeError('Invalid ArrayBuffer length');
  this._byteLength = len;
  this._maxByteLength = (opts && typeof opts === 'object' && typeof opts.maxByteLength === 'number')
    ? (opts.maxByteLength | 0) : -1;
  this._detached = false;
  // 0 채우기는 네이티브로 — JS 루프면 1MB 버퍼가 100만 반복이라 사실상 못 쓴다.
  this._b = __kZeroBytes(len);
}
// byteLength/maxByteLength/resizable/detached 는 ArrayBuffer.prototype 의 **accessor** 다
// (§25.1.6). 인스턴스 own 데이터가 아니다 — getOwnPropertyDescriptor(...).get 검사가 이걸 본다.
Object.defineProperty(__kArrayBuffer.prototype, 'byteLength', {
  get: function(){ return this._detached ? 0 : this._byteLength; }, configurable: true });
Object.defineProperty(__kArrayBuffer.prototype, 'maxByteLength', {
  get: function(){ return this._detached ? 0 : (this._maxByteLength >= 0 ? this._maxByteLength : this._byteLength); }, configurable: true });
Object.defineProperty(__kArrayBuffer.prototype, 'resizable', {
  get: function(){ return this._maxByteLength >= 0; }, configurable: true });
Object.defineProperty(__kArrayBuffer.prototype, 'detached', {
  get: function(){ return this._detached; }, configurable: true });
__kArrayBuffer.prototype[Symbol.toStringTag] = 'ArrayBuffer';
__kArrayBuffer.prototype.slice = function(a, b){
  var n = this.byteLength;
  a = (a === undefined) ? 0 : (a | 0); b = (b === undefined) ? n : (b | 0);
  if (a < 0) a += n; if (b < 0) b += n;
  a = Math.max(0, Math.min(a, n)); b = Math.max(0, Math.min(b, n));
  var out = new __kArrayBuffer(Math.max(0, b - a));
  for (var i = 0; i < out.byteLength; i++) out._b[i] = this._b[a + i];
  return out;
};
// resize (§25.1.6.x) — resizable 일 때만. transfer 는 버퍼를 분리(detach)한다.
__kArrayBuffer.prototype.resize = function(newLen){
  if (this._maxByteLength < 0) throw new TypeError('ArrayBuffer is not resizable');
  newLen = newLen | 0;
  if (newLen < 0 || newLen > this._maxByteLength) throw new RangeError('Invalid resize length');
  var nb = __kZeroBytes(newLen);
  for (var i = 0; i < Math.min(newLen, this._byteLength); i++) nb[i] = this._b[i];
  this._b = nb; this._byteLength = newLen;
};
__kArrayBuffer.prototype.transfer = function(newLen){
  if (this._detached) throw new TypeError('ArrayBuffer is detached');
  newLen = (newLen === undefined) ? this._byteLength : (newLen | 0);
  if (newLen < 0) throw new RangeError('Invalid length');
  var out = new __kArrayBuffer(newLen);
  for (var i = 0; i < Math.min(newLen, this._byteLength); i++) out._b[i] = this._b[i];
  this._detached = true; this._b = __kZeroBytes(0); this._byteLength = 0;
  return out;
};
__kArrayBuffer.prototype.transferToFixedLength = function(newLen){ return this.transfer(newLen); };
// ArrayBuffer.isView(x) (§25.1.5.1): x 가 typed array/DataView 뷰인가. 우리 뷰는 _spec 을 든다.
__kArrayBuffer.isView = function(x){ return !!(x && typeof x === 'object' && x._spec); };

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
                 set: function(b,o,v){ var e=__kF2B(+v,8,52,11); for(var i=0;i<8;i++) b[o+i]=e[i]; }},
  // __kBI64/__kBI63 는 아래에 정의(var 호이스팅 + 런타임 호출이라 값 확정 후 쓰임).
  // (__kB64 는 btoa 의 base64 알파벳이라 이름 충돌 금지 — __kBI* 로 둔다.)
  // BigInt64Array / BigUint64Array (ES2020). 원소는 BigInt — 리틀엔디언 8바이트로 왕복.
  // 저장 시 하위 64비트만(모듈러), 부호형은 읽을 때 2^63 이상이면 2^64 를 뺀다.
  // 바이트 인코딩은 산술(*,/,%)로 — BigInt 비트연산(<<,|)은 큰 값에서 부정확하다.
  BigInt64Array:  {size: 8, big: true,
                 get: function(b,o){ var r=0n; for(var i=7;i>=0;i--) r=r*256n+BigInt(b[o+i]); if(r>=__kBI63) r-=__kBI64; return r; },
                 set: function(b,o,v){ if(typeof v!=='bigint') v=BigInt(v); var u=((v%__kBI64)+__kBI64)%__kBI64; for(var i=0;i<8;i++){ b[o+i]=Number(u%256n); u=u/256n; } }},
  BigUint64Array: {size: 8, big: true,
                 get: function(b,o){ var r=0n; for(var i=7;i>=0;i--) r=r*256n+BigInt(b[o+i]); return r; },
                 set: function(b,o,v){ if(typeof v!=='bigint') v=BigInt(v); var u=((v%__kBI64)+__kBI64)%__kBI64; for(var i=0;i<8;i++){ b[o+i]=Number(u%256n); u=u/256n; } }}
};
var __kBI64 = 2n ** 64n, __kBI63 = 2n ** 63n;

// %TypedArray%.prototype (§23.2.3) — 공유 프로토타입. 각 생성자의 prototype 이 이걸
// 상속하므로 메서드는 own 이 아니라 **상속**된다(inherited.js 검사가 이걸 본다).
// this[i] 는 Proxy 트랩을 타므로 명시 루프로 접근하고, 새 typed array 는 this.constructor 로.
// ValidateTypedArray (§23.2.4.4): 수신자가 유효한 typed array(브랜드)이고 그 버퍼가
// 분리(detach)되지 않았는지 확인. 대부분의 프로토타입 메서드는 시작 시 이걸 부른다 —
// 브랜드 불일치·분리 버퍼면 TypeError. (getter length/byteLength/buffer/byteOffset 은
// throw 하지 않고 0/버퍼를 돌려주므로 여기 대상이 아니다.)
function __kTAValidate(o){
  if (!o || !o._spec) throw new TypeError('method called on incompatible receiver (not a TypedArray)');
  if (o._buffer && o._buffer._detached) throw new TypeError('TypedArray has a detached ArrayBuffer');
  return o;
}
// SpeciesConstructor (§7.3.22): O.constructor[Symbol.species] 로 파생 생성자를 고른다.
// undefined/null 이면 기본 생성자. 생성자 아님이면 TypeError.
function __kSpeciesCtor(O, defaultCtor){
  var C = O.constructor;
  if (C === undefined) return defaultCtor;
  if (C === null || (typeof C !== 'object' && typeof C !== 'function'))
    throw new TypeError('constructor property is not an object');
  var S = C[Symbol.species];
  if (S === undefined || S === null) return defaultCtor;
  if (typeof S === 'function') return S;
  throw new TypeError('Symbol.species is not a constructor');
}
// TypedArraySpeciesCreate (§23.2.4.1): filter/map/slice/subarray 의 반환 종을 만든다.
// 정확히 args.length 개의 인자로 Construct 해야 한다(커스텀 species 가 arguments.length
// 를 관측). 결과를 ValidateTypedArray 하고, 단일 길이 인자보다 짧으면 TypeError.
function __kTASpeciesCreate(exemplar, args){
  var C = __kSpeciesCtor(exemplar, exemplar._ctor);
  var result;
  if (args.length <= 1) result = new C(args[0]);
  else if (args.length === 2) result = new C(args[0], args[1]);
  else result = new C(args[0], args[1], args[2]);
  __kTAValidate(result);
  if (args.length === 1 && typeof args[0] === 'number' && result.length < args[0])
    throw new TypeError('derived TypedArray is smaller than requested length');
  return result;
}
var __kTAProto = {
  // §23.2.3.24 %TypedArray%.prototype.set(source [, offset]) — length 1(offset=arguments[1]).
  // 브랜드 검사(TypeError) → offset ToInteger(음수 RangeError) → 분리(TypeError) →
  // 범위(RangeError) → typed array 소스면 요소복사(오버랩은 임시배열), 아니면 array-like
  // (ToObject/ToLength/각 값 ToNumber). BigInt 컨텐트 타입 불일치는 TypeError.
  set: function(src){
    var target = this;
    if (!target || !target._spec) throw new TypeError('TypedArray.prototype.set called on incompatible receiver');
    var to = +arguments[1]; if (to !== to) to = 0; to = Math.trunc(to);
    if (to < 0) throw new RangeError('offset is out of bounds');
    if (target._buffer && target._buffer._detached) throw new TypeError('target ArrayBuffer is detached');
    var tlen = target.length;
    if (src && src._spec) {
      if (!!src._spec.big !== !!target._spec.big) throw new TypeError('cannot set a BigInt TypedArray from a non-BigInt one (or vice versa)');
      var sl = src.length;
      if (sl + to > tlen) throw new RangeError('source array is too long for the target offset');
      var tmp = []; for (var i = 0; i < sl; i++) tmp.push(src[i]);
      for (var i = 0; i < sl; i++) target[to + i] = tmp[i];
    } else {
      var so = Object(src);
      var n = Number(so.length);
      var sl = (n !== n || n <= 0) ? 0 : Math.min(Math.trunc(n), 9007199254740991);
      if (sl + to > tlen) throw new RangeError('source array is too long for the target offset');
      for (var i = 0; i < sl; i++) target[to + i] = so[i];
    }
    return undefined;
  },
  fill: function(v, a, b){ __kTAValidate(this); a = a || 0; b = (b === undefined) ? this.length : b; for (var i = a; i < b; i++) this[i] = v; return this; },
  subarray: function(a, b){ var len=this.length; a = a || 0; b = (b === undefined) ? len : b; var beginByteOffset = this.byteOffset + a * this.BYTES_PER_ELEMENT; return __kTASpeciesCreate(this, [this.buffer, beginByteOffset, Math.max(0, b - a)]); },
  slice: function(a, b){ __kTAValidate(this); var len=this.length; a = (a === undefined) ? 0 : (a|0); b = (b === undefined) ? len : (b|0); if (a<0) a+=len; if (b<0) b+=len; var count = Math.max(0, Math.min(b, len) - a); var A = __kTASpeciesCreate(this, [count]); for (var i = 0; i < count; i++) A[i] = this[a + i]; return A; },
  forEach: function(fn){ __kTAValidate(this); var t=arguments[1]; for (var i = 0; i < this.length; i++) fn.call(t, this[i], i, this); },
  map: function(fn){ __kTAValidate(this); var t=arguments[1]; var len = this.length; var A = __kTASpeciesCreate(this, [len]); for (var i = 0; i < len; i++) A[i] = fn.call(t, this[i], i, this); return A; },
  indexOf: function(v){ __kTAValidate(this); for (var i = 0; i < this.length; i++) if (this[i] === v) return i; return -1; },
  includes: function(v){ __kTAValidate(this); for (var i = 0; i < this.length; i++) if (this[i] === v) return true; return false; },
  join: function(sep){ __kTAValidate(this); var a = []; for (var i = 0; i < this.length; i++) a.push(this[i]); return a.join(sep === undefined ? ',' : sep); },
  reduce: function(fn, init){ __kTAValidate(this); var acc = init, i = 0; if (arguments.length < 2) acc = this[i++]; for (; i < this.length; i++) acc = fn(acc, this[i], i, this); return acc; },
  every: function(fn, t){ __kTAValidate(this); for (var i=0;i<this.length;i++) if(!fn.call(t,this[i],i,this)) return false; return true; },
  some: function(fn, t){ __kTAValidate(this); for (var i=0;i<this.length;i++) if(fn.call(t,this[i],i,this)) return true; return false; },
  filter: function(fn){ __kTAValidate(this); var t=arguments[1]; var out=[]; for (var i=0;i<this.length;i++) if(fn.call(t,this[i],i,this)) out.push(this[i]); var A=__kTASpeciesCreate(this,[out.length]); for (var j=0;j<out.length;j++) A[j]=out[j]; return A; },
  find: function(fn, t){ __kTAValidate(this); for (var i=0;i<this.length;i++) if(fn.call(t,this[i],i,this)) return this[i]; return undefined; },
  findIndex: function(fn, t){ __kTAValidate(this); for (var i=0;i<this.length;i++) if(fn.call(t,this[i],i,this)) return i; return -1; },
  findLast: function(fn, t){ __kTAValidate(this); for (var i=this.length-1;i>=0;i--) if(fn.call(t,this[i],i,this)) return this[i]; return undefined; },
  findLastIndex: function(fn, t){ __kTAValidate(this); for (var i=this.length-1;i>=0;i--) if(fn.call(t,this[i],i,this)) return i; return -1; },
  reduceRight: function(fn){ __kTAValidate(this); var i=this.length-1, acc; if (arguments.length>1) acc=arguments[1]; else acc=this[i--]; for (; i>=0; i--) acc=fn(acc,this[i],i,this); return acc; },
  lastIndexOf: function(v, from){ __kTAValidate(this); var i = (from===undefined) ? this.length-1 : (from|0); if (i<0) i+=this.length; for (; i>=0; i--) if (this[i]===v) return i; return -1; },
  at: function(i){ __kTAValidate(this); i = Math.trunc(+i) || 0; if (i < 0) i += this.length; return (i >= 0 && i < this.length) ? this[i] : undefined; },
  reverse: function(){ __kTAValidate(this); var n = this.length; for (var i = 0; i < (n >> 1); i++){ var t = this[i]; this[i] = this[n-1-i]; this[n-1-i] = t; } return this; },
  toReversed: function(){ __kTAValidate(this); var n = this.length, out = new this._ctor(n); for (var i = 0; i < n; i++) out[i] = this[n-1-i]; return out; },
  sort: function(cmp){ __kTAValidate(this); var a = []; for (var i=0;i<this.length;i++) a.push(this[i]); a.sort(cmp || function(x, y){ return x < y ? -1 : (x > y ? 1 : 0); }); for (var i = 0; i < a.length; i++) this[i] = a[i]; return this; },
  toSorted: function(cmp){ __kTAValidate(this); return new this._ctor(this).sort(cmp); },
  copyWithin: function(target, start, end){ __kTAValidate(this); var n=this.length; target=target|0; start=start|0; end=(end===undefined)?n:(end|0); if(target<0)target+=n; if(start<0)start+=n; if(end<0)end+=n; var tmp=[]; for(var i=start;i<end&&i<n;i++) tmp.push(this[i]); for(var i=0;i<tmp.length&&target+i<n;i++) this[target+i]=tmp[i]; return this; },
  with: function(i, v){ __kTAValidate(this); i = i | 0; if (i < 0) i += this.length; var out = new this._ctor(this); out[i] = v; return out; },
  // keys/entries/values 는 반복자를 반환하는데, 검증은 **호출 시점에 즉시**(반복자 본문
  // 지연 실행이 아니라)여야 한다 (§23.2.3.x → ValidateTypedArray 먼저). 그래서 일반 함수로
  // 감싸 먼저 검증하고 제너레이터를 만들어 돌려준다.
  keys: function(){ __kTAValidate(this); var s=this; return (function*(){ for (var i=0;i<s.length;i++) yield i; })(); },
  entries: function(){ __kTAValidate(this); var s=this; return (function*(){ for (var i=0;i<s.length;i++) yield [i, s[i]]; })(); },
  values: function(){ __kTAValidate(this); var s=this; return (function*(){ for (var i=0;i<s.length;i++) yield s[i]; })(); },
  toLocaleString: function(){ __kTAValidate(this); return this.join(','); }
};
__kTAProto[Symbol.iterator] = __kTAProto.values;
// length/byteLength 는 %TypedArray%.prototype 의 **accessor** 다 (§23.2.3.18/.2). 인스턴스
// own 이 아니다(Proxy 트랩이 값을 계산) — getOwnPropertyDescriptor(...).get 검사가 이걸 본다.
// 트랩이 ta.length/byteLength 를 가로채므로 이 accessor 는 gOPD/.get.call 에서만 쓰인다.
Object.defineProperty(__kTAProto, 'length', { get: function(){
  if (!this || !this._spec) throw new TypeError('get %TypedArray%.prototype.length called on incompatible receiver');
  var avail = Math.floor((this.buffer.byteLength - this.byteOffset) / this.BYTES_PER_ELEMENT);
  return Math.max(0, Math.min(this._len, avail));
}, configurable: true });
Object.defineProperty(__kTAProto, 'byteLength', { get: function(){
  if (!this || !this._spec) throw new TypeError('get %TypedArray%.prototype.byteLength called on incompatible receiver');
  var avail = Math.floor((this.buffer.byteLength - this.byteOffset) / this.BYTES_PER_ELEMENT);
  return Math.max(0, Math.min(this._len, avail)) * this.BYTES_PER_ELEMENT;
}, configurable: true });
// buffer/byteOffset 도 %TypedArray%.prototype accessor (§23.2.3.1/.3). 인스턴스는 내부
// 슬롯 _buffer/_byteOffset 에 든다(own 데이터 아님 — hasOwnProperty 검사 통과).
Object.defineProperty(__kTAProto, 'buffer', { get: function(){
  if (!this || !this._spec) throw new TypeError('get %TypedArray%.prototype.buffer called on incompatible receiver');
  return this._buffer;
}, configurable: true });
Object.defineProperty(__kTAProto, 'byteOffset', { get: function(){
  if (!this || !this._spec) throw new TypeError('get %TypedArray%.prototype.byteOffset called on incompatible receiver');
  // 분리(detach)된 버퍼면 0 (§23.2.3.3) — throw 하지 않는다.
  return (this._buffer && this._buffer._detached) ? 0 : this._byteOffset;
}, configurable: true });

// %TypedArray% intrinsic 생성자 (§23.2.1) — 추상. 하네스가 Object.getPrototypeOf(Int8Array)
// 로 이걸 얻는다. 직접 호출/생성은 TypeError. 각 typed array 생성자의 [[Prototype]] 이 이것.
function __kTypedArrayCtor(){ throw new TypeError('Abstract class TypedArray not directly constructable'); }
__kTypedArrayCtor.prototype = __kTAProto;
__kTAProto.constructor = __kTypedArrayCtor;
// %TypedArray%.from / of / [Symbol.species] (§23.2.2) — 서브클래스가 상속(정적 접근).
__kTypedArrayCtor.from = function(x, fn){ var a = Array.from(x, fn); return new this(a); };
__kTypedArrayCtor.of = function(){ return new this(Array.prototype.slice.call(arguments)); };
Object.defineProperty(__kTypedArrayCtor, Symbol.species, { get: function(){ return this; }, configurable: true });

function __kMakeTypedArray(name) {
  var spec = __kTA[name];
  function Ctor(arg, byteOffset, length) {
    var buf, off = 0, len = 0;
    if (arg instanceof __kArrayBuffer) {
      buf = arg;
      off = byteOffset || 0;
      // 분리된 버퍼 위에는 뷰를 만들 수 없다 (§23.2.5.1 step 11). offset/length 는
      // subarray 등에서 이미 계산·강제변환됐고, 여기서 TypeError.
      if (buf._detached) throw new TypeError('Cannot construct a TypedArray on a detached ArrayBuffer');
      len = (length === undefined) ? Math.floor((buf.byteLength - off) / spec.size) : length;
    } else if (typeof arg === 'number') {
      len = arg | 0;
      buf = new __kArrayBuffer(len * spec.size);
    } else if (arg && typeof arg === 'object' && typeof arg[Symbol.iterator] === 'function' && typeof arg.length !== 'number') {
      // 순수 iterable(@@iterator 있고 length 없음) → 값을 모아 리스트로
      // (§23.2.5.1 InitializeTypedArrayFromList). Set/제너레이터/사용자 iterable 지원.
      // 예전엔 length 없는 iterable 이 빈 배열로 떨어져 통째로 비었다.
      var vals = [], it = arg[Symbol.iterator](), step;
      while (!(step = it.next()).done) vals.push(step.value);
      arg = vals;
      len = vals.length;
      buf = new __kArrayBuffer(len * spec.size);
    } else if (arg && typeof arg.length === 'number') {
      len = arg.length;
      buf = new __kArrayBuffer(len * spec.size);
    } else {
      len = 0;
      buf = new __kArrayBuffer(0);
    }
    var self = this;
    this._buffer = buf;
    this._byteOffset = off;
    this._len = len;
    this.BYTES_PER_ELEMENT = spec.size;
    this._spec = spec;
    // 인스턴스 own constructor — 우리 엔진의 new(일반함수)는 prototype 을 스냅샷 복사라
    // this.constructor 가 프로토타입 체인으로 안 잡힌다. 메서드의 new this.constructor 와
    // ta.constructor===Ctor 검사가 이걸 쓴다.
    this.constructor = Ctor;
    this._ctor = Ctor;
    // length/byteLength 는 버퍼에서 파생한다 — 버퍼가 분리(detach)되면 0 이 돼야 한다.
    // (WebAssembly.Memory.grow 는 옛 버퍼를 분리한다. 길이를 박아 두면 죽은 뷰가
    //  살아있는 척하며 조용히 틀린 값을 읽는다 — wasm-bindgen 은 정확히 이걸로 판별한다)
    var vlen = function(t){
      var avail = Math.floor((t._buffer.byteLength - t._byteOffset) / spec.size);
      return Math.max(0, Math.min(t._len, avail));
    };
    var view = new Proxy(this, {
      get: function(t, k){
        var n = (typeof k === 'string' && k !== '' && String(+k) === k) ? +k : -1;
        if (n >= 0) {
          if (n >= vlen(t)) return undefined;
          return spec.get(t._buffer._b, t._byteOffset + n * spec.size);
        }
        if (k === 'length') return vlen(t);
        if (k === 'byteLength') return vlen(t) * spec.size;
        return t[k];
      },
      set: function(t, k, v){
        var n = (typeof k === 'string' && k !== '' && String(+k) === k) ? +k : -1;
        if (n >= 0) {
          if (n < vlen(t)) spec.set(t._buffer._b, t._byteOffset + n * spec.size, v);
          return true;
        }
        t[k] = v;
        return true;
      },
      // Integer-Indexed Exotic Object (§10.4.5): 정수 인덱스는 특별 취급.
      has: function(t, k){
        var n = (typeof k === 'string' && k !== '' && String(+k) === k) ? +k : -1;
        if (n >= 0) return Math.floor(n) === n && n < vlen(t);   // 유효 인덱스만 존재
        return k in t;
      },
      // [[GetOwnProperty]] (§10.4.5.1): 유효 인덱스면 {value, w:t, e:t, c:t}, 아니면 undefined.
      getOwnPropertyDescriptor: function(t, k){
        var n = (typeof k === 'string' && k !== '' && String(+k) === k) ? +k : -1;
        if (n >= 0) {
          if (!(Math.floor(n) === n && n < vlen(t))) return undefined;
          return { value: spec.get(t._buffer._b, t._byteOffset + n * spec.size),
                   writable: true, enumerable: true, configurable: true };
        }
        return Object.getOwnPropertyDescriptor(t, k);
      },
      // [[DefineOwnProperty]] (§10.4.5.3): 정수 인덱스는 유효할 때만, 서술자가 configurable/
      // enumerable/writable false 이거나 접근자면 false. value 있으면 요소에 쓴다.
      defineProperty: function(t, k, desc){
        var n = (typeof k === 'string' && k !== '' && String(+k) === k) ? +k : -1;
        if (n >= 0) {
          if (!(Math.floor(n) === n && n < vlen(t))) return false;
          if (desc){
            if (desc.configurable === false) return false;
            if (desc.enumerable === false) return false;
            if (('get' in desc) || ('set' in desc)) return false;
            if (desc.writable === false) return false;
            if ('value' in desc) spec.set(t._buffer._b, t._byteOffset + n * spec.size, desc.value);
          }
          return true;
        }
        Object.defineProperty(t, k, desc);
        return true;
      },
      // [[Delete]] (§10.4.5.4): 유효 인덱스는 삭제 불가(false), 그 밖은 보통 삭제.
      deleteProperty: function(t, k){
        var n = (typeof k === 'string' && k !== '' && String(+k) === k) ? +k : -1;
        if (n >= 0) return !(Math.floor(n) === n && n < vlen(t));
        delete t[k];
        return true;
      }
    });
    if (arg && typeof arg !== 'number' && !(arg instanceof __kArrayBuffer) && typeof arg.length === 'number') {
      for (var i = 0; i < len; i++) spec.set(buf._b, off + i * spec.size, arg[i]);
    }
    return view;
  }
  // 각 typed array 생성자의 prototype 은 공유 %TypedArray%.prototype 을 상속한다
  // → 메서드는 own 이 아니라 inherited (§23.2.3, inherited.js).
  Ctor.prototype = Object.create(__kTAProto);
  Ctor.prototype.constructor = Ctor;
  Ctor.prototype.BYTES_PER_ELEMENT = spec.size;
  // 각 typed array 생성자의 [[Prototype]] 은 %TypedArray% 다 (§23.2). 하네스가
  // Object.getPrototypeOf(Int8Array) 로 %TypedArray% 를 얻는다.
  Object.setPrototypeOf(Ctor, __kTypedArrayCtor);
  // from/of/[Symbol.species] 은 own 이 아니라 %TypedArray% 에서 정적 상속한다
  // (§23.2.2). Int8Array.from === %TypedArray%.from 이어야 하고, own 으로 두면
  // inherited.js 계열 테스트가 깨진다. %TypedArray%.from 은 new this(...) 라
  // this=Int8Array(수신자)로 올바른 종을 만든다.
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
var BigInt64Array = window.BigInt64Array, BigUint64Array = window.BigUint64Array;

// test262 호스트 훅 $262 (§host-defined). 다수 TypedArray/ArrayBuffer 테스트가
// $262.detachArrayBuffer 로 **표준 분리(detach)** 동작을 관측한다(detachArrayBuffer.js
// 하네스의 $DETACHBUFFER 가 이걸 부른다). detach 는 우리 ArrayBuffer 표현
// (_detached/_b/_byteLength)을 transfer 와 같은 방식으로 비운다.
if (!window.$262) {
  window.$262 = {
    global: (typeof globalThis !== 'undefined' ? globalThis : window),
    gc: function(){},
    detachArrayBuffer: function(buffer){
      if (buffer instanceof __kArrayBuffer) {
        buffer._detached = true;
        buffer._b = __kZeroBytes(0);
        buffer._byteLength = 0;
      }
      return null;
    },
    evalScript: function(src){ return eval(src); }
  };
}
var $262 = window.$262;

// ToIndex (§7.1.22): ToIntegerOrInfinity 후 [0, 2^53-1] 밖이면 RangeError. undefined→0.
// Infinity/음수/과대는 RangeError, 객체는 valueOf 관측. DataView/ArrayBuffer 오프셋에 쓴다.
function __kToIndex(v){
  if (v === undefined) return 0;
  var n = +v;                     // ToNumber(단항 +, valueOf 관측). NaN→0.
  n = (n !== n) ? 0 : Math.trunc(n);
  if (n < 0 || n > 9007199254740991) throw new RangeError('Invalid index: out of range');
  return n;
}
// DataView (§25.3) — ArrayBuffer 위의 뷰. 임의 바이트 오프셋에서 타입별로 읽고/쓰며
// 엔디언(little/big)을 지원한다. 바이트는 버퍼의 _b 를 직접 만진다.
function __kDataView(buffer, byteOffset, byteLength) {
  if (!(buffer && typeof buffer === 'object' && buffer._b)) throw new TypeError('First argument to DataView constructor must be an ArrayBuffer');
  var off = __kToIndex(byteOffset);   // ToIndex (Infinity/음수 RangeError, valueOf 관측)
  if (buffer._detached) throw new TypeError('Cannot construct DataView on a detached ArrayBuffer');
  if (off > buffer.byteLength) throw new RangeError('Start offset is outside the bounds of the buffer');
  var len = (byteLength === undefined) ? (buffer.byteLength - off) : __kToIndex(byteLength);
  if (off + len > buffer.byteLength) throw new RangeError('Invalid DataView length');
  this.__isDataView = true;
  this._buffer = buffer;
  this._byteOffset = off;
  this._byteLength = len;
}
Object.defineProperty(__kDataView.prototype, 'buffer', { get: function(){
  if (!this || this.__isDataView !== true) throw new TypeError('get DataView.prototype.buffer called on incompatible receiver');
  return this._buffer; }, configurable: true });
Object.defineProperty(__kDataView.prototype, 'byteLength', { get: function(){
  if (!this || this.__isDataView !== true) throw new TypeError('get DataView.prototype.byteLength called on incompatible receiver');
  if (this._buffer._detached) throw new TypeError('Cannot get byteLength of a DataView with a detached buffer');
  return this._byteLength; }, configurable: true });
Object.defineProperty(__kDataView.prototype, 'byteOffset', { get: function(){
  if (!this || this.__isDataView !== true) throw new TypeError('get DataView.prototype.byteOffset called on incompatible receiver');
  if (this._buffer._detached) throw new TypeError('Cannot get byteOffset of a DataView with a detached buffer');
  return this._byteOffset; }, configurable: true });
__kDataView.prototype[Symbol.toStringTag] = 'DataView';
// GetViewValue (§25.3.1.1) 순서: 브랜드(TypeError) → ToIndex(offset)(RangeError) →
// 분리(TypeError) → 범위(RangeError). size 바이트를 리틀엔디언 순서 배열로 반환.
__kDataView.prototype._rd = function(offset, size, le){
  if (!this || this.__isDataView !== true) throw new TypeError('DataView method called on incompatible receiver');
  var idx = __kToIndex(offset);
  if (this._buffer._detached) throw new TypeError('Cannot operate on a detached ArrayBuffer');
  if (idx + size > this._byteLength) throw new RangeError('Offset is outside the bounds of the DataView');
  var b = this._buffer._b, base = this._byteOffset + idx, out = [];
  for (var i = 0; i < size; i++) out.push(b[base + i]);
  if (!le) out.reverse();
  return out;
};
// SetViewValue (§25.3.1.2): 값은 호출 메서드에서 이미 ToNumber/ToBigInt 됨. 여기선
// 브랜드 → ToIndex(offset) → 분리 → 범위 확인 후 리틀엔디언 바이트를 쓴다.
__kDataView.prototype._wr = function(offset, size, le, bytesLE){
  if (!this || this.__isDataView !== true) throw new TypeError('DataView method called on incompatible receiver');
  var idx = __kToIndex(offset);
  if (this._buffer._detached) throw new TypeError('Cannot operate on a detached ArrayBuffer');
  if (idx + size > this._byteLength) throw new RangeError('Offset is outside the bounds of the DataView');
  var b = this._buffer._b, base = this._byteOffset + idx;
  var bytes = le ? bytesLE : bytesLE.slice().reverse();
  for (var i = 0; i < size; i++) b[base + i] = bytes[i];
};
__kDataView.prototype.getUint8 = function(o){ return this._rd(o, 1, true)[0]; };
__kDataView.prototype.getInt8 = function(o){ var v = this._rd(o, 1, true)[0]; return v > 127 ? v - 256 : v; };
__kDataView.prototype.getUint16 = function(o, le){ var b = this._rd(o, 2, le); return b[0] | (b[1] << 8); };
__kDataView.prototype.getInt16 = function(o, le){ var v = this.getUint16(o, le); return v > 32767 ? v - 65536 : v; };
__kDataView.prototype.getUint32 = function(o, le){ var b = this._rd(o, 4, le); var v = (b[0] | (b[1] << 8) | (b[2] << 16) | (b[3] << 24)); return v < 0 ? v + 4294967296 : v; };
__kDataView.prototype.getInt32 = function(o, le){ var b = this._rd(o, 4, le); return (b[0] | (b[1] << 8) | (b[2] << 16) | (b[3] << 24)); };
__kDataView.prototype.getFloat32 = function(o, le){ return __kB2F(this._rd(o, 4, le), 23, 8); };
__kDataView.prototype.getFloat64 = function(o, le){ return __kB2F(this._rd(o, 8, le), 52, 11); };
__kDataView.prototype.getBigInt64 = function(o, le){ var b = this._rd(o, 8, le), r = 0n; for (var i = 7; i >= 0; i--) r = r * 256n + BigInt(b[i]); if (r >= __kBI63) r -= __kBI64; return r; };
__kDataView.prototype.getBigUint64 = function(o, le){ var b = this._rd(o, 8, le), r = 0n; for (var i = 7; i >= 0; i--) r = r * 256n + BigInt(b[i]); return r; };
__kDataView.prototype.setUint8 = function(o, v){ this._wr(o, 1, true, [((v | 0) % 256 + 256) % 256]); };
__kDataView.prototype.setInt8 = function(o, v){ this._wr(o, 1, true, [((v | 0) % 256 + 256) % 256]); };
__kDataView.prototype.setUint16 = function(o, v, le){ v = ((v | 0) % 65536 + 65536) % 65536; this._wr(o, 2, le, [v & 255, (v >> 8) & 255]); };
__kDataView.prototype.setInt16 = function(o, v, le){ this.setUint16(o, v, le); };
__kDataView.prototype.setUint32 = function(o, v, le){ v = v >>> 0; this._wr(o, 4, le, [v & 255, (v >>> 8) & 255, (v >>> 16) & 255, (v >>> 24) & 255]); };
__kDataView.prototype.setInt32 = function(o, v, le){ this.setUint32(o, v, le); };
__kDataView.prototype.setFloat32 = function(o, v, le){ this._wr(o, 4, le, __kF2B(+v, 4, 23, 8)); };
__kDataView.prototype.setFloat64 = function(o, v, le){ this._wr(o, 8, le, __kF2B(+v, 8, 52, 11)); };
__kDataView.prototype.setBigInt64 = function(o, v, le){ if (typeof v !== 'bigint') v = BigInt(v); var u = ((v % __kBI64) + __kBI64) % __kBI64, by = []; for (var i = 0; i < 8; i++){ by.push(Number(u % 256n)); u = u / 256n; } this._wr(o, 8, le, by); };
__kDataView.prototype.setBigUint64 = function(o, v, le){ this.setBigInt64(o, v, le); };
if (!window.DataView) window.DataView = __kDataView;
var DataView = window.DataView;

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
// HTMLElement 는 인터페이스 객체이면서 **생성 가능**해야 한다: instanceof 판정에도
// 쓰이고, 커스텀 엘리먼트가 `class X extends HTMLElement` 로 상속도 한다.
// 앞서 만든 인터페이스 객체의 브랜드 판정(Symbol.hasInstance)을 그대로 옮겨 온다.
// (이걸 안 하면 둘 중 하나가 죽는다 — 실제로 인터페이스 객체를 얹자마자 커스텀
//  엘리먼트가 "Illegal constructor" 로 전부 죽었다.)
if (window.HTMLElement && window.HTMLElement[Symbol.hasInstance]) {
  __kHTMLElement[Symbol.hasInstance] = window.HTMLElement[Symbol.hasInstance];
}
Object.defineProperty(__kHTMLElement, 'name', { value: 'HTMLElement', configurable: true });
window.HTMLElement = __kHTMLElement;
var HTMLElement = window.HTMLElement;

var __kCE = {};      // 태그명 → 생성자
var __kCEDone = [];  // 이미 업그레이드한 요소들

function __kUpgrade(el, ctor) {
  if (!el || __kCEDone.indexOf(el) >= 0) return;
  __kCEDone.push(el);
  // 업그레이드된 요소는 그 클래스의 프로토타입 체인을 갖는다 (표준).
  // 예전엔 연결이 없어서 this.anyMethod() 가 전부 undefined 였다 —
  // Lit/Stencil 같은 웹 컴포넌트 라이브러리가 생성자 첫 줄에서 죽었다.
  __kBindElementClass(el, ctor);
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
  // Reflect.getPrototypeOf 는 엔진 네이티브(Object.getPrototypeOf)와 같은 것을 쓴다.
  // 예전엔 무조건 null 을 돌려주는 거짓말이었다.
  Reflect.getPrototypeOf = Object.getPrototypeOf;
  Reflect.setPrototypeOf = function(o, p){ Object.setPrototypeOf(o, p); return true; };
  Reflect.defineProperty = function(t, k, d){ Object.defineProperty(t, k, d); return true; };
  Reflect.getOwnPropertyDescriptor = function(t, k){ return Object.getOwnPropertyDescriptor(t, k); };
  Reflect.apply = function(fn, thisArg, args){ return fn.apply(thisArg, args); };
  Reflect.construct = function(fn, args){ return new (Function.prototype.bind.apply(fn, [null].concat(args || [])))(); };
  window.Reflect = Reflect;
}

// ── WebAssembly ────────────────────────────────────────────────────────────
// 표면은 여기(JS), 알맹이는 네이티브(src/wasm.rs). 이렇게 두는 이유:
// Memory.buffer 가 **진짜 ArrayBuffer** 여야 new Uint8Array(memory.buffer) 가
// 살아있는 뷰가 된다. 네이티브가 흉내낸 객체를 주면 조용히 틀린 사본이 된다.
function __kWasmBytes(src) {
  if (src instanceof __kArrayBuffer) return src._b;
  if (src && src.buffer instanceof __kArrayBuffer) {
    // 타입 배열 뷰 — 자기 구간만 떼어낸다
    var b = src.buffer._b, o = src.byteOffset | 0, n = src.byteLength | 0, out = [];
    for (var i = 0; i < n; i++) out.push(b[o + i]);
    return out;
  }
  if (src && typeof src.length === 'number') return src;
  throw new TypeError('WebAssembly: 바이트열이 아니다');
}

var WebAssembly = {};
class __kWasmCompileError extends Error {}
class __kWasmLinkError extends Error {}
class __kWasmRuntimeError extends Error {}
WebAssembly.CompileError = __kWasmCompileError;
WebAssembly.LinkError = __kWasmLinkError;
WebAssembly.RuntimeError = __kWasmRuntimeError;

WebAssembly.Module = function(bytes) {
  this.__idx = __kWasmCompile(__kWasmBytes(bytes));
};

WebAssembly.Memory = function(desc) {
  desc = desc || {};
  var pages = desc.initial || 0;
  this.buffer = new __kArrayBuffer(pages * 65536);
  this.__mem = __kWasmRegisterMemory(this.buffer._b, this);
};
// 커지면 이전 ArrayBuffer 는 분리되고 buffer 가 새 것으로 바뀐다 (네이티브가 갈아끼운다).
// 옛 뷰의 length 가 0 이 되어야 캐시된 뷰를 쓰는 코드(wasm-bindgen)가 뷰를 다시 만든다.
WebAssembly.Memory.prototype.grow = function(n) {
  var old = __kWasmGrow(this.__mem, n);
  if (old < 0) throw new RangeError('WebAssembly.Memory.grow: 한도 초과');
  return old;
};

WebAssembly.Global = function(desc, v) {
  this.value = (v === undefined) ? 0 : v;
};
WebAssembly.Global.prototype.valueOf = function(){ return this.value; };

WebAssembly.Instance = function(mod, imports) {
  var pages = __kWasmMemPages(mod.__idx);
  var mem = (pages >= 0) ? new WebAssembly.Memory({initial: pages}) : null;
  this.exports = __kWasmInstantiate(mod.__idx, imports || {}, mem ? mem.__mem : -1);
};

WebAssembly.validate = function(bytes) { return __kWasmValidate(__kWasmBytes(bytes)); };
WebAssembly.compile = function(bytes) {
  return new Promise(function(resolve, reject){
    try { resolve(new WebAssembly.Module(bytes)); } catch (e) { reject(e); }
  });
};
WebAssembly.instantiate = function(src, imports) {
  return new Promise(function(resolve, reject){
    try {
      if (src instanceof WebAssembly.Module) {
        resolve(new WebAssembly.Instance(src, imports));
      } else {
        var m = new WebAssembly.Module(src);
        resolve({module: m, instance: new WebAssembly.Instance(m, imports)});
      }
    } catch (e) { reject(e); }
  });
};
// …Streaming: Response(또는 그 Promise)에서 바이트를 뽑아 같은 길로 보낸다
WebAssembly.compileStreaming = function(src, imports) {
  return Promise.resolve(src).then(function(r){ return r.arrayBuffer(); })
    .then(function(buf){ return new WebAssembly.Module(buf); });
};
WebAssembly.instantiateStreaming = function(src, imports) {
  return Promise.resolve(src).then(function(r){ return r.arrayBuffer(); })
    .then(function(buf){ return WebAssembly.instantiate(buf, imports); });
};
window.WebAssembly = WebAssembly;
"#;


#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::Dom;

    fn text_of_id(dom: &Dom, id: &str) -> Option<String> {
        dom.find_by_attr_id(id).map(|n| dom.text_content(n))
    }

    #[test]
    fn dataset_is_live_and_attributes_have_named_access() {
        // dataset 은 살아있는 뷰다 (DOMStringMap). 예전엔 스냅샷이라 쓰기가 조용히 사라졌다.
        // attributes 는 NamedNodeMap 이라 이름으로도 접근한다 (jQuery 가 그렇게 쓴다).
        let mut dom = crate::html::parse_dom(
            "<div id=\"d\" data-x=\"1\" class=\"c\"></div><p id=\"t\">?</p>\
             <script>\
             var d = document.getElementById('d');\
             d.dataset.y = '2';\
             var named = d.attributes['class'] ? d.attributes['class'].value : 'none';\
             var got = d.attributes.getNamedItem('data-x').value;\
             document.getElementById('t').textContent = \
               d.getAttribute('data-y') + ',' + d.dataset.x + ',' + named + ',' + got;\
             </script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://localhost/", None);
        assert_eq!(text_of_id(&dom, "t").unwrap(), "2,1,c,1");
    }

    #[test]
    fn anchor_exposes_url_parts() {
        // <a> 는 URL 분해 속성을 갖는다 (HTMLHyperlinkElementUtils).
        // 없으면 a.pathname 같은 흔한 코드가 undefined 를 읽고 죽는다 (naver).
        let mut dom = crate::html::parse_dom(
            "<a id=\"a\" href=\"/p/x?q=1#f\">L</a><p id=\"t\">?</p>\
             <script>\
             var a = document.getElementById('a');\
             document.getElementById('t').textContent = \
               a.pathname + '|' + a.search + '|' + a.hash + '|' + a.protocol + '|' + a.hostname;\
             </script>"
                .to_string(),
        );
        run_scripts(&mut dom, "https://example.test/base/", None);
        assert_eq!(text_of_id(&dom, "t").unwrap(), "/p/x|?q=1|#f|https:|example.test");
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
