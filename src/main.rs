mod bench;
mod brotli;
mod brotli_tables;
mod cff;
mod css;
mod dataurl;
mod dom;
mod encoding;
mod encoding_cjk;
mod font;
mod html;
mod http;
mod inflate;
mod jpeg;
mod js;
mod layout;
mod paint;
mod png;
mod raster;
mod style;
mod url;
mod woff2;
mod window;

use std::fs;

// 힙 사용량을 항상 추적하는 자작 allocator (측정 하네스가 읽는다).
#[global_allocator]
static GLOBAL: bench::CountingAllocator = bench::CountingAllocator;

fn main() {
    // 네트워크 fetch 모드: kestrel --fetch <url>
    let args: Vec<String> = std::env::args().collect();

    // 측정 하네스: kestrel --bench
    if args.len() >= 2 && args[1] == "--bench" {
        bench::run_bench();
        return;
    }
    if args.len() >= 3 && args[1] == "--fetch" {
        match http::fetch(&args[2]) {
            Ok(resp) => {
                println!(
                    "status {} | {} headers | {} body bytes",
                    resp.status,
                    resp.headers.len(),
                    resp.body.len()
                );
                let preview: String =
                    String::from_utf8_lossy(&resp.body).chars().take(200).collect();
                println!("--- first 200 chars ---\n{}", preview);
            }
            Err(e) => println!("fetch error: {:?}", e),
        }
        return;
    }

    // 파싱 모드: kestrel --parse <url> (fetch → tolerant parse → 요소 수)
    if args.len() >= 3 && args[1] == "--parse" {
        match http::fetch(&args[2]) {
            Ok(resp) => {
                let html = String::from_utf8_lossy(&resp.body).to_string();
                let dom = html::parse_dom(html);
                println!("parsed OK: {} elements (http {})", count_elements(&dom), resp.status);
            }
            Err(e) => println!("fetch error: {:?}", e),
        }
        return;
    }

    // URL 렌더 모드: kestrel <url>
    if args.len() >= 2 && (args[1].starts_with("http://") || args[1].starts_with("https://")) {
        render_url(&args[1]);
        return;
    }
    // http 로 시작하지만 형식이 이상하면(예: 슬래시 하나) 조용히 데모로 떨어지지 않고 안내
    if args.len() >= 2 && args[1].starts_with("http") {
        println!(
            "URL 형식이 올바르지 않습니다: {}\n예: cargo run -- https://example.com  (슬래시 두 개)",
            args[1]
        );
        return;
    }

    // 글리프 덤프 모드: KESTREL_GLYPH 문자열을 래스터화해 그레이스케일 PPM으로.
    if let Ok(text) = std::env::var("KESTREL_GLYPH") {
        let out = std::env::var("KESTREL_GLYPH_OUT").unwrap_or_else(|_| "glyphs.ppm".to_string());
        dump_glyphs(&text, &out);
        return;
    }

    let html_source = fs::read_to_string("examples/test.html").expect("read examples/test.html");
    let css_source = fs::read_to_string("examples/test.css").expect("read examples/test.css");

    let mut scripts = page_scripts(&html_source);
    let mut root_node = html::parse_dom(html_source);
    // 엔티티(&#51060; 등) 디코드 후에야 드러나는 글자도 있다
    scripts.extend(page_scripts(&root_node.text_content(root_node.root)));
    // 실제 페이지처럼 UA 스타일시트를 먼저 깔고 그 위에 저작자 CSS 를 얹는다.
    let mut stylesheet = css::user_agent_stylesheet();
    stylesheet.rules.extend(css::parse(css_source).rules);

    let viewport_width: u32 = 800;
    let viewport_height: u32 = 600;

    let fonts = load_fonts(&scripts);
    let mut cache = raster::GlyphCache::new();
    // 로컬 데모도 절대 URL <img>/배경 이미지는 가져온다 (베이스는 상대경로 해석용 임의값).
    let base = url::Url::parse("https://localhost/").unwrap();
    let mut srcs = Vec::new();
    collect_img_srcs(&root_node, &mut srcs);
    {
        let style_root = style::style_tree(&root_node, &stylesheet);
        collect_bg_urls(&style_root, &mut srcs);
    }
    let (images, img_map) = load_images(srcs, &base);

    // 스크립트 실행 (스타일시트·폰트·이미지가 준비된 뒤 — 표준 순서).
    // 강제 레이아웃 컨텍스트를 넘겨 스크립트 안 측정 API 가 실제 값을 돌려주게 한다.
    let empty_pseudo = style::PseudoStyles::new();
    let ctx = window::LayoutCtx {
        sheet: &stylesheet,
        fonts: &fonts,
        img_map: &img_map,
        pseudo: &empty_pseudo,
        vw: viewport_width as f32,
        vh: viewport_height as f32,
    };
    let js_rt = js::run_scripts(&mut root_node, "https://localhost/", Some(ctx));

    // ::before/::after 생성 콘텐츠 노드를 DOM 에 주입 (스타일/레이아웃 전 1회)
    let pseudo_styles = style::generate_pseudo_elements(&mut root_node, &stylesheet);

    let mut page = window::Page {
        dom: root_node,
        sheet: stylesheet,
        images,
        img_map,
        fonts,
        js: js_rt,
        url: base,
        viewport_width: viewport_width as f32,
        viewport_height: viewport_height as f32,
        pseudo_styles,
        items: Vec::new(),
        links: Vec::new(),
        element_rects: Vec::new(),
        doc_height: 0.0,
        focused_input: None,
        scroll_y: 0.0,
    };
    page.rebuild();

    // 헤드리스 렌더 모드: KESTREL_RENDER_TO 가 설정되면 창 대신 PPM 으로 출력하고 종료.
    if let Ok(path) = std::env::var("KESTREL_RENDER_TO") {
        page.flush_timers_headless();
        let scroll = std::env::var("KESTREL_SCROLL")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(0.0);
        let canvas = paint::rasterize(
            &page.items,
            viewport_width as usize,
            viewport_height as usize,
            scroll,
            1.0,
            &page.fonts,
            &mut cache,
            &page.images,
        );
        write_ppm(&canvas, &path);
        println!("rendered to {}", path);
        return;
    }

    window::run_page(page, viewport_width, viewport_height, build_page);
}

fn write_ppm(canvas: &paint::Canvas, path: &str) {
    let mut data = Vec::with_capacity(canvas.width * canvas.height * 3 + 32);
    data.extend_from_slice(format!("P6\n{} {}\n255\n", canvas.width, canvas.height).as_bytes());
    for c in &canvas.pixels {
        data.push(c.r);
        data.push(c.g);
        data.push(c.b);
    }
    fs::write(path, data).expect("write ppm");
}

// 아레나 DOM 을 문서 순서(DFS)로 방문.
// noscript 하위는 건너뛴다 — 우리는 JS 를 실행하는 브라우저 (구글 결과 페이지가
// noscript 안에 "전부 display:none" 스타일을 넣어 무 JS 브라우저를 숨긴다)
fn walk_dom(dom: &dom::Dom, id: dom::NodeId, f: &mut impl FnMut(&dom::NodeData)) {
    let node = dom.get(id);
    f(node);
    if let dom::NodeType::Element(e) = &node.node_type {
        if e.tag_name == "noscript" {
            return;
        }
    }
    for &c in &node.children {
        walk_dom(dom, c, f);
    }
}

fn count_elements(dom: &dom::Dom) -> usize {
    let mut count = 0;
    walk_dom(dom, dom.root, &mut |n| {
        if matches!(n.node_type, dom::NodeType::Element(_)) {
            count += 1;
        }
    });
    count
}

fn collect_img_srcs(dom: &dom::Dom, out: &mut Vec<String>) {
    walk_dom(dom, dom.root, &mut |n| {
        if let dom::NodeType::Element(e) = &n.node_type {
            if e.tag_name == "img" {
                if let Some(src) = e.img_source() {
                    out.push(src);
                }
            }
        }
    });
}

// 매직 바이트로 포맷 판별 → 해당 디코더 (PNG / JPEG)
fn decode_image(bytes: &[u8]) -> Option<png::Image> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        png::decode(bytes)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        jpeg::decode(bytes)
    } else {
        None
    }
}

// 스타일 트리에서 적용된 background-image url 수집 (매칭된 규칙만 — 시트 전체가 아님)
fn collect_bg_urls(node: &style::StyledNode, out: &mut Vec<String>) {
    if let Some(css::Value::Url(u)) = node.value("background-image") {
        out.push(u);
    }
    for c in &node.children {
        collect_bg_urls(c, out);
    }
}

// 이미 받은 이미지 목록에 새 src 들을 이어붙인다 (인덱스 오프셋 보정).
fn merge_images(
    mut images: Vec<png::Image>,
    mut map: layout::ImageMap,
    new_srcs: Vec<String>,
    base: &url::Url,
) -> (Vec<png::Image>, layout::ImageMap) {
    let (more, more_map) = load_images(new_srcs, base);
    let offset = images.len();
    images.extend(more);
    for (src, (idx, w, h)) in more_map {
        map.insert(src, (idx + offset, w, h));
    }
    (images, map)
}

fn load_images(srcs: Vec<String>, base: &url::Url) -> (Vec<png::Image>, layout::ImageMap) {
    // 중복 제거 (순서 보존)
    let mut uniq: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for s in srcs {
        if seen.insert(s.clone()) {
            uniq.push(s);
        }
    }
    if uniq.is_empty() {
        return (Vec::new(), layout::ImageMap::new());
    }
    println!("[이미지] {}개 다운로드 중...", uniq.len());

    // 워커 스레드들이 공유 인덱스에서 작업을 가져간다 (최대 8 동시 연결)
    use std::sync::atomic::{AtomicUsize, Ordering};
    let next = AtomicUsize::new(0);
    let results: std::sync::Mutex<Vec<Option<png::Image>>> =
        std::sync::Mutex::new((0..uniq.len()).map(|_| None).collect());
    // 호스트당 동시 연결 수 제한 (브라우저는 대개 6). 한 호스트에 8개를 한꺼번에
    // 꽂으면 레이트 리미터에 걸린다 — 위키미디어가 429 로 조이고, 그 429 본문(HTML)이
    // 이미지 디코더로 들어가 "디코드 실패"로 조용히 사라졌다. 렌더가 실행마다 달라졌다.
    let host_of = |u: &str| -> String {
        base.join(u).map(|x| x.host.clone()).unwrap_or_default()
    };
    let hosts: Vec<String> = uniq.iter().map(|u| host_of(u)).collect();
    let host_slots: std::collections::HashMap<String, std::sync::Mutex<usize>> = hosts
        .iter()
        .map(|h| (h.clone(), std::sync::Mutex::new(0usize)))
        .collect();
    let host_gate = |h: &str| -> bool {
        // 호스트당 최대 6 — 넘으면 잠깐 기다렸다 다시 시도
        if let Some(m) = host_slots.get(h) {
            let mut n = m.lock().unwrap();
            if *n >= 6 {
                return false;
            }
            *n += 1;
        }
        true
    };
    let host_release = |h: &str| {
        if let Some(m) = host_slots.get(h) {
            let mut n = m.lock().unwrap();
            *n = n.saturating_sub(1);
        }
    };
    std::thread::scope(|scope| {
        for _ in 0..uniq.len().min(8) {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= uniq.len() {
                    break;
                }
                // 호스트 슬롯이 찼으면 잠깐 양보 (호스트당 6 연결 상한)
                if !dataurl::is_data_url(&uniq[i]) {
                    while !host_gate(&hosts[i]) {
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                }
                // data: URL 은 네트워크 없이 그 자리에서 디코드 (RFC 2397).
                // 예전엔 http::fetch 로 넘겨 스킴 오류로 실패했다 — 이미지가 조용히 사라졌다.
                let img = if dataurl::is_data_url(&uniq[i]) {
                    dataurl::decode(&uniq[i]).and_then(|b| decode_image(&b))
                } else {
                    match base.join(&uniq[i]) {
                        Some(u) => match http::fetch(&u.as_string()) {
                            // 2xx 가 아니면 본문은 이미지가 아니다(429 의 HTML 오류 페이지 등).
                            // 예전엔 상태를 안 보고 그대로 디코더에 넣어서, 429 로 조여진
                            // 요청이 "디코드 실패"로 조용히 사라졌다.
                            Ok(resp) if !(200..300).contains(&resp.status) => {
                                if std::env::var("KESTREL_IMG_DEBUG").is_ok() {
                                    eprintln!(
                                        "[img] HTTP {} {}",
                                        resp.status,
                                        &uniq[i][..uniq[i].len().min(60)]
                                    );
                                }
                                None
                            }
                            Ok(resp) => {
                                let d = decode_image(&resp.body);
                                if d.is_none() && std::env::var("KESTREL_IMG_DEBUG").is_ok() {
                                    eprintln!(
                                        "[img] 디코드 실패(형식 미지원?) {} ({}바이트)",
                                        &uniq[i][..uniq[i].len().min(60)],
                                        resp.body.len()
                                    );
                                }
                                d
                            }
                            Err(e) => {
                                if std::env::var("KESTREL_IMG_DEBUG").is_ok() {
                                    eprintln!(
                                        "[img] 요청 실패 {} — {:?}",
                                        &uniq[i][..uniq[i].len().min(60)],
                                        e
                                    );
                                }
                                None
                            }
                        },
                        None => None,
                    }
                };
                if !dataurl::is_data_url(&uniq[i]) {
                    host_release(&hosts[i]);
                }
                results.lock().unwrap()[i] = img;
            });
        }
    });

    let mut images = Vec::new();
    let mut map = layout::ImageMap::new();
    for (src, img) in uniq.into_iter().zip(results.into_inner().unwrap()) {
        if let Some(img) = img {
            let (w, h) = (img.width, img.height);
            let idx = images.len();
            images.push(img);
            map.insert(src, (idx, w, h));
        }
    }
    println!("[이미지] {}개 디코드 성공", images.len());
    (images, map)
}

// 외부 스타일시트를 재귀적으로 로드 (@import 추적). 미디어 조건 충족 시만 import.
// @import 규칙은 파일 자신의 규칙보다 앞선 캐스케이드 → 먼저 추가.
fn load_stylesheet(
    css_url: &str,
    page_vw: f32,
    sheet: &mut css::Stylesheet,
    depth: u32,
    seen: &mut std::collections::HashSet<String>,
) {
    if depth > 6 || seen.contains(css_url) {
        return;
    }
    seen.insert(css_url.to_string());
    let r = match http::fetch(css_url) {
        Ok(r) => r,
        Err(e) => {
            println!("[css] 로드 실패 {:?} — {}", e, css_url);
            return;
        }
    };
    // 2xx 가 아니면 본문은 CSS 가 아니다. 예전엔 상태를 안 보고 404/429 의 HTML 오류
    // 페이지를 그대로 CSS 파서에 넣었다 — 규칙이 하나도 안 나오는데 아무 말도 없었다.
    // (스타일이 통째로 빠진 페이지가 나오고 왜 그런지 알 수가 없다)
    if !(200..300).contains(&r.status) {
        println!("[css] HTTP {} — {}", r.status, css_url);
        return;
    }
    let ctype = r
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| v.clone());
    let (text, _) = encoding::decode(&r.body, ctype.as_deref());
    let this_base = url::Url::parse(css_url).ok();
    for (imp, media) in extract_imports(&text) {
        if !media.is_empty() && !css::media_matches(&media, page_vw) {
            continue;
        }
        if let Some(b) = &this_base {
            if let Some(u) = b.join(&imp) {
                load_stylesheet(&u.as_string(), page_vw, sheet, depth + 1, seen);
            }
        }
    }
    let parsed = css::parse_viewport(text, page_vw);
    sheet.rules.extend(parsed.rules);
    sheet.font_faces.extend(parsed.font_faces);
    sheet.keyframes.extend(parsed.keyframes);
}

// CSS 텍스트에서 @import 규칙 추출 → (url, 미디어조건) 목록.
// `@import url("x") media;` / `@import "x" media;` 모두 지원.
fn extract_imports(css: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = css;
    while let Some(p) = rest.find("@import") {
        let after = &rest[p + 7..];
        let end = match after.find(';') {
            Some(e) => e,
            None => break,
        };
        let stmt = after[..end].trim();
        if let Some((url, media)) = parse_import_stmt(stmt) {
            out.push((url, media));
        }
        rest = &after[end + 1..];
    }
    out
}

// "url(\"x\") cond" 또는 "\"x\" cond" → (url, cond). url() / 따옴표 제거.
fn parse_import_stmt(stmt: &str) -> Option<(String, String)> {
    let s = stmt.trim();
    let (url, media) = if let Some(inner) = s.strip_prefix("url(") {
        let close = inner.find(')')?;
        (inner[..close].trim(), inner[close + 1..].trim())
    } else {
        // "x" 또는 'x' 로 시작
        let q = s.chars().next()?;
        if q != '"' && q != '\'' {
            return None;
        }
        let after = &s[1..];
        let close = after.find(q)?;
        (&s[..close + 2], after[close + 1..].trim())
    };
    let url = url.trim().trim_matches(|c| c == '"' || c == '\'').trim();
    if url.is_empty() {
        return None;
    }
    Some((url.to_string(), media.to_string()))
}

// <link rel=stylesheet> 의 (href, media) 수집. media 조건은 로더가 평가한다.
// <base href> — 문서의 기준 URL (HTML 표준 §4.2.3).
// 이게 없으면 이 태그를 쓰는 페이지의 CSS/스크립트/이미지/링크가 전부 엉뚱한 곳으로
// 간다. 예전엔 무시했다 — 서브리소스가 통째로 404 가 나는데 아무 말도 없었다.
fn base_href(dom: &dom::Dom) -> Option<String> {
    let mut found = None;
    walk_dom(dom, dom.root, &mut |n| {
        if found.is_some() {
            return;
        }
        if let dom::NodeType::Element(e) = &n.node_type {
            if e.tag_name == "base" {
                if let Some(h) = e.attributes.get("href") {
                    if !h.trim().is_empty() {
                        found = Some(h.trim().to_string());
                    }
                }
            }
        }
    });
    found
}

fn collect_links(dom: &dom::Dom, out: &mut Vec<(String, String)>) {
    walk_dom(dom, dom.root, &mut |n| {
        if let dom::NodeType::Element(e) = &n.node_type {
            if e.tag_name == "link" {
                let rel = e.attributes.get("rel").map(|s| s.as_str()).unwrap_or("");
                if rel.split_whitespace().any(|r| r.eq_ignore_ascii_case("stylesheet")) {
                    if let Some(href) = e.attributes.get("href") {
                        let media = e.attributes.get("media").cloned().unwrap_or_default();
                        out.push((href.clone(), media));
                    }
                }
            }
        }
    });
}

fn extract_css(dom: &dom::Dom, out: &mut String) {
    let mut seen = std::collections::HashSet::new();
    extract_new_css(dom, &mut seen, out);
}

// 아직 읽지 않은 <style> 만 모은다. 스크립트가 나중에 삽입한 스타일을 두 번 넣지 않기 위해
// 소비한 노드 id 를 기억한다 (스크립트 전 1회, 스크립트 후 1회 호출).
fn extract_new_css(
    dom: &dom::Dom,
    seen: &mut std::collections::HashSet<dom::NodeId>,
    out: &mut String,
) {
    let mut style_ids = Vec::new();
    walk_dom_ids(dom, dom.root, &mut |id| {
        if let dom::NodeType::Element(e) = &dom.get(id).node_type {
            if e.tag_name == "style" && seen.insert(id) {
                style_ids.push(id);
            }
        }
    });
    for id in style_ids {
        out.push_str(&dom.text_content(id));
        out.push('\n');
    }
}

fn walk_dom_ids(dom: &dom::Dom, id: dom::NodeId, f: &mut impl FnMut(dom::NodeId)) {
    f(id);
    if let dom::NodeType::Element(e) = &dom.get(id).node_type {
        if e.tag_name == "noscript" {
            return;
        }
    }
    for &c in &dom.get(id).children {
        walk_dom_ids(dom, c, f);
    }
}

// URL 을 fetch 해 렌더 준비가 끝난 Page 로 만든다. 창의 링크 내비게이션에서 재사용.
fn build_page(url: &str) -> Option<window::Page> {
    let resp = match http::fetch(url) {
        Ok(r) => r,
        Err(e) => {
            println!("fetch error: {:?}", e);
            return None;
        }
    };
    println!("fetched {} ({} bytes, http {})", url, resp.body.len(), resp.status);

    // 문자 인코딩 감지 → 디코딩. 예전엔 무조건 UTF-8 로 읽어 EUC-KR 페이지가
    // 조용히 깨진 글자로 렌더됐다(렌더는 되는데 내용이 쓰레기).
    let ctype = resp
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| v.clone());
    let (html, charset) = encoding::decode(&resp.body, ctype.as_deref());
    if !matches!(charset, encoding::Charset::Utf8) {
        println!("[charset] {:?} 로 디코딩", charset);
    }
    let mut scripts = page_scripts(&html);
    let mut dom = html::parse_dom(html);
    // 원문엔 없어도 엔티티(&#51060; 등) 디코드 후 한글이 나올 수 있다 (google.co.kr)
    scripts.extend(page_scripts(&dom.text_content(dom.root)));

    let doc_url = url::Url::parse(url).ok()?;
    // <base href> 가 있으면 그것이 문서의 기준 URL 이다 (HTML 표준 §4.2.3).
    // CSS/스크립트/이미지/링크의 상대 URL 이 전부 이 기준으로 해석된다.
    let base = match base_href(&dom) {
        Some(h) => match doc_url.join(&h) {
            Some(b) => {
                println!("[base] <base href> → {}", b.as_string());
                b
            }
            None => doc_url.clone(),
        },
        None => doc_url.clone(),
    };

    // 스타일: UA → 외부 <link> CSS → 인라인 <style> 순서로 합침.
    // 저작자 CSS 는 실제 뷰포트 폭으로 파싱해 @media 를 평가한다.
    let page_vw = 1000.0f32;
    let page_vh = 800.0f32; // vh 단위 해석용 기본 뷰포트 높이
    let mut sheet = css::user_agent_stylesheet();
    let mut hrefs = Vec::new();
    collect_links(&dom, &mut hrefs);
    if !hrefs.is_empty() {
        println!("[css] 외부 스타일시트 {}개 로드 중...", hrefs.len().min(10));
    }
    let mut seen_css = std::collections::HashSet::new();
    for (href, media) in hrefs.iter().take(10) {
        // <link media="..."> 조건 (다크 테마·print 등) 불일치 시 건너뜀
        if !media.is_empty() && !css::media_matches(media, page_vw) {
            continue;
        }
        if let Some(u) = base.join(href) {
            load_stylesheet(&u.as_string(), page_vw, &mut sheet, 0, &mut seen_css);
        }
    }
    let mut seen_styles = std::collections::HashSet::new();
    let mut inline_css = String::new();
    extract_new_css(&dom, &mut seen_styles, &mut inline_css);
    let parsed_inline = css::parse_viewport(inline_css, page_vw);
    sheet.rules.extend(parsed_inline.rules);
    sheet.font_faces.extend(parsed_inline.font_faces);
    sheet.keyframes.extend(parsed_inline.keyframes);

    // 이미지: <img src> (DOM) + 적용된 background-image (스타일 트리는 일시 사용)
    let mut srcs = Vec::new();
    collect_img_srcs(&dom, &mut srcs);
    {
        let style_root = style::style_tree(&dom, &sheet);
        collect_bg_urls(&style_root, &mut srcs);
    }
    let (images, img_map) = load_images(srcs, &base);
    if std::env::var("KESTREL_IMG_DEBUG").is_ok() {
        let mut want = Vec::new();
        collect_img_srcs(&dom, &mut want);
        for s in &want {
            eprintln!(
                "[img] {} → {}",
                &s[..s.len().min(80)],
                if img_map.contains_key(s) { "맵에 있음" } else { "없음!!" }
            );
        }
    }

    let mut fonts = load_fonts(&scripts);
    // @font-face 웹폰트 로드 (ttf/otf 만 — woff/woff2 미지원). 각 패밀리 첫 성공 src 사용.
    let mut loaded_faces = 0;
    for ff in &sheet.font_faces {
        for src in &ff.srcs {
            if let Some(u) = base.join(src) {
                if let Ok(r) = http::fetch(&u.as_string()) {
                    // woff2 는 brotli 압축 + glyf/loca 변환이 걸린 sfnt 다 — 되돌려서 넘긴다.
                    // 모던 사이트의 웹폰트는 사실상 전부 이 형식이다.
                    let bytes = if r.body.starts_with(b"wOF2") {
                        match woff2::decode(&r.body) {
                            Some(sfnt) => sfnt,
                            None => {
                                if std::env::var("KESTREL_FONT_DEBUG").is_ok() {
                                    eprintln!("[font] woff2 복원 실패: {}", src);
                                }
                                continue;
                            }
                        }
                    } else {
                        r.body
                    };
                    if let Ok(font) = font::Font::from_bytes(bytes) {
                        fonts.add_named_font(font, ff.family.clone());
                        loaded_faces += 1;
                        break;
                    }
                }
            }
        }
    }
    println!("[fonts] {} font(s) loaded (@font-face {}개)", fonts.fonts.len(), loaded_faces);

    // 스크립트 실행. HTML 표준에서 파서가 삽입한 스크립트는 보류된 스타일시트를 기다린 뒤
    // 실행된다 — 그래서 CSS/폰트/이미지가 준비된 이 시점이 맞다. 예전엔 스크립트를 CSS 보다
    // 먼저 돌려서, 스크립트 안의 측정 API 가 전부 0 을 돌려줬다(스타일도 레이아웃도 없었다).
    // layout_ctx 를 넘기면 측정 시점에 강제 레이아웃이 돌아 실제 값이 나온다.
    let empty_pseudo = style::PseudoStyles::new();
    let js_rt = {
        let ctx = window::LayoutCtx {
            sheet: &sheet,
            fonts: &fonts,
            img_map: &img_map,
            pseudo: &empty_pseudo,
            vw: page_vw,
            vh: page_vh,
        };
        js::run_scripts_with_base(&mut dom, url, &base.as_string(), Some(ctx))
    };

    // 스크립트가 <style> 을 주입했으면 그때 생긴 것만 추가로 반영한다.
    let mut injected_css = String::new();
    extract_new_css(&dom, &mut seen_styles, &mut injected_css);
    if !injected_css.trim().is_empty() {
        let parsed = css::parse_viewport(injected_css, page_vw);
        sheet.rules.extend(parsed.rules);
        sheet.font_faces.extend(parsed.font_faces);
        sheet.keyframes.extend(parsed.keyframes);
    }

    // 스크립트가 넣은 <img>/배경 이미지도 가져온다 (이미 받은 것은 건너뜀).
    let mut new_srcs = Vec::new();
    collect_img_srcs(&dom, &mut new_srcs);
    {
        let style_root = style::style_tree(&dom, &sheet);
        collect_bg_urls(&style_root, &mut new_srcs);
    }
    new_srcs.retain(|s| !img_map.contains_key(s));
    let (images, img_map) = if new_srcs.is_empty() {
        (images, img_map)
    } else {
        merge_images(images, img_map, new_srcs, &base)
    };

    // ::before/::after 생성 콘텐츠 노드를 DOM 에 주입 (스타일/레이아웃 전 1회)
    let pseudo_styles = style::generate_pseudo_elements(&mut dom, &sheet);

    let mut page = window::Page {
        dom,
        sheet,
        images,
        img_map,
        fonts,
        js: js_rt,
        url: base,
        viewport_width: page_vw,
        viewport_height: page_vh,
        pseudo_styles,
        items: Vec::new(),
        links: Vec::new(),
        element_rects: Vec::new(),
        doc_height: 0.0,
        focused_input: None,
        scroll_y: 0.0,
    };
    page.rebuild();
    println!("[문서 높이 {}px, 링크 {}개]", page.doc_height as u32, page.links.len());
    Some(page)
}

fn render_url(url: &str) {
    let Some(mut page) = build_page(url) else { return };

    // 헤드리스 클릭 (검증용): KESTREL_CLICK=x,y → 문서 좌표에 클릭 디스패치
    if let Ok(spec) = std::env::var("KESTREL_CLICK") {
        for one in spec.split(';') {
            if let Some((xs, ys)) = one.split_once(',') {
                if let (Ok(x), Ok(y)) = (xs.trim().parse::<f32>(), ys.trim().parse::<f32>()) {
                    let fired = page.dispatch_click(x, y);
                    println!("[click] ({}, {}) fired={}", x, y, fired);
                }
            }
        }
    }

    // 헤드리스 폼 입력/제출 (검증용): KESTREL_TYPE="name=값" [+ KESTREL_SUBMIT=1]
    if let Ok(spec) = std::env::var("KESTREL_TYPE") {
        if let Some((name, val)) = spec.split_once('=') {
            let mut found = None;
            walk_dom_ids(&page.dom, page.dom.root, &mut |id| {
                if found.is_none() {
                    if let dom::NodeType::Element(e) = &page.dom.get(id).node_type {
                        if e.tag_name == "input"
                            && e.attributes.get("name").map(|n| n.as_str()) == Some(name)
                        {
                            found = Some(id);
                        }
                    }
                }
            });
            if let Some(nid) = found {
                page.set_input_value(nid, val.to_string());
                println!("[type] {} = {:?}", name, val);
                if std::env::var("KESTREL_SUBMIT").is_ok() {
                    if let Some(u) = page.submit_url(nid) {
                        println!("[submit] → {}", u);
                        if let Some(p2) = build_page(&u) {
                            page = p2;
                        }
                    }
                }
            } else {
                println!("[type] name={} 인 input 없음", name);
            }
        }
    }

    // 헤드리스: (viewport 크기) 슬라이스를 PPM 으로. KESTREL_SCROLL 로 시작 오프셋 지정.
    if let Ok(path) = std::env::var("KESTREL_RENDER_TO") {
        page.flush_timers_headless();
        let (vw, vh) = (1000usize, 1400usize);
        // 스크롤 위치: KESTREL_SCROLL 이 우선, 없으면 스크립트가 요청한 위치
        // (window.scrollTo/scrollIntoView). 상태만 바꾸고 렌더는 안 움직이면 반쪽 거짓말이다.
        let scroll = std::env::var("KESTREL_SCROLL")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(page.js.scroll_y)
            .clamp(0.0, (page.doc_height - vh as f32).max(0.0));
        // 스티키 요소는 스크롤 위치를 알아야 붙는다 → 그 위치로 재레이아웃
        if scroll != page.scroll_y {
            page.scroll_y = scroll;
            page.rebuild();
        }
        let mut cache = raster::GlyphCache::new();
        let canvas = paint::rasterize(
            &page.items,
            vw,
            vh,
            scroll,
            1.0,
            &page.fonts,
            &mut cache,
            &page.images,
        );
        write_ppm(&canvas, &path);
        println!("rendered to {}", path);
        return;
    }
    println!("휠/트랙패드/방향키/PageUp·Down/Home/End 스크롤 · 링크 클릭으로 이동");
    window::run_page(page, 1000, 800, build_page);
}

// 텍스트에 한글(자모/완성형)이 있는지.
// 페이지에 쓰인 문자 체계(스크립트) 판별.
// 예전엔 한글만 봤다. 일본어 가나/중국어/아랍어/태국어 텍스트는 디코딩은 되는데
// 글리프가 없어서 조용히 안 보였다 — 레이아웃은 자리를 잡고 화면만 빈다.
// 어느 쪽이 더 나쁜지 분명하다: 실패했다는 것조차 안 보인다.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Script {
    Korean,
    Japanese, // 가나 (한자는 CJK 폰트가 함께 덮는다)
    Han,      // 한자/중국어
    Cyrillic,
    Greek,
    Arabic,
    Hebrew,
    Thai,
    Devanagari,
}

impl Script {
    fn probe(self) -> char {
        match self {
            Script::Korean => '한',
            Script::Japanese => 'あ',
            Script::Han => '漢',
            Script::Cyrillic => 'А',
            Script::Greek => 'Α',
            Script::Arabic => 'ا',
            Script::Hebrew => 'א',
            Script::Thai => 'ก',
            Script::Devanagari => 'क',
        }
    }

    fn label(self) -> &'static str {
        match self {
            Script::Korean => "한글",
            Script::Japanese => "일본어 가나",
            Script::Han => "한자",
            Script::Cyrillic => "키릴",
            Script::Greek => "그리스",
            Script::Arabic => "아랍",
            Script::Hebrew => "히브리",
            Script::Thai => "타이",
            Script::Devanagari => "데바나가리",
        }
    }

    // macOS 시스템 폰트 후보 (앞에서부터 시도). Arial Unicode 는 마지막 보루.
    fn candidates(self) -> &'static [&'static str] {
        match self {
            Script::Korean => &[
                "/System/Library/Fonts/AppleSDGothicNeo.ttc",
                "/System/Library/Fonts/Supplemental/AppleGothic.ttf",
                "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
            ],
            Script::Japanese => &[
                "/System/Library/Fonts/Hiragino Sans GB.ttc",
                "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc",
                "/System/Library/Fonts/AquaKana.ttc",
                "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
            ],
            Script::Han => &[
                "/System/Library/Fonts/Hiragino Sans GB.ttc",
                "/System/Library/Fonts/Supplemental/Songti.ttc",
                "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
            ],
            Script::Cyrillic | Script::Greek => &[
                "/System/Library/Fonts/Helvetica.ttc",
                "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
            ],
            Script::Arabic => &[
                "/System/Library/Fonts/GeezaPro.ttc",
                "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
            ],
            Script::Hebrew => &[
                "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
                "/System/Library/Fonts/ArialHB.ttc",
            ],
            Script::Thai => &[
                "/System/Library/Fonts/Supplemental/Thonburi.ttc",
                "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
            ],
            Script::Devanagari => &[
                "/System/Library/Fonts/Supplemental/DevanagariMT.ttc",
                "/System/Library/Fonts/Kohinoor.ttc",
                "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
            ],
        }
    }
}

fn script_of(c: char) -> Option<Script> {
    let u = c as u32;
    Some(match u {
        0xAC00..=0xD7A3 | 0x1100..=0x11FF | 0x3130..=0x318F => Script::Korean,
        0x3040..=0x309F | 0x30A0..=0x30FF | 0xFF66..=0xFF9D => Script::Japanese,
        0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0xF900..=0xFAFF => Script::Han,
        0x0400..=0x04FF => Script::Cyrillic,
        0x0370..=0x03FF => Script::Greek,
        0x0600..=0x06FF | 0x0750..=0x077F => Script::Arabic,
        0x0590..=0x05FF => Script::Hebrew,
        0x0E00..=0x0E7F => Script::Thai,
        0x0900..=0x097F => Script::Devanagari,
        _ => return None,
    })
}

// 페이지 텍스트에 실제로 쓰인 스크립트 집합
fn page_scripts(text: &str) -> std::collections::HashSet<Script> {
    let mut out = std::collections::HashSet::new();
    for c in text.chars() {
        if let Some(s) = script_of(c) {
            out.insert(s);
        }
    }
    out
}

// 시스템 폰트로 폰트 스택 구성. 라틴 주 폰트 + 페이지가 실제로 쓰는 스크립트별 폴백.
// 폰트를 못 찾은 스크립트는 조용히 넘기지 않고 알린다 — 그 글자는 화면에서 사라진다.
fn load_fonts(scripts: &std::collections::HashSet<Script>) -> font::FontStack {
    let latin: &[&str] = &[
        "/System/Library/Fonts/Helvetica.ttc",
        "/System/Library/Fonts/HelveticaNeue.ttc",
        "/System/Library/Fonts/SFNS.ttf",
        "assets/fonts/Latin.ttf",
    ];
    let try_load = |paths: &[&str], probe: char| -> Option<font::Font> {
        for p in paths {
            if let Ok(b) = fs::read(p) {
                if let Ok(f) = font::Font::from_bytes(b) {
                    if f.glyph_index(probe) != 0 {
                        return Some(f);
                    }
                }
            }
        }
        None
    };

    let mut fonts = Vec::new();
    if let Some(f) = try_load(latin, 'A') {
        fonts.push(f);
    }
    // 스크립트 순서를 고정해 렌더가 실행마다 달라지지 않게 한다 (HashSet 순회 순서 무관).
    let order = [
        Script::Korean,
        Script::Japanese,
        Script::Han,
        Script::Cyrillic,
        Script::Greek,
        Script::Arabic,
        Script::Hebrew,
        Script::Thai,
        Script::Devanagari,
    ];
    let mut loaded = Vec::new();
    let mut missing = Vec::new();
    for sc in order {
        if !scripts.contains(&sc) {
            continue;
        }
        match try_load(sc.candidates(), sc.probe()) {
            Some(f) => {
                fonts.push(f);
                loaded.push(sc.label());
            }
            None => missing.push(sc.label()),
        }
    }
    if fonts.is_empty() {
        if let Ok(b) = fs::read("assets/fonts/Latin.ttf") {
            if let Ok(f) = font::Font::from_bytes(b) {
                fonts.push(f);
            }
        }
    }
    if !missing.is_empty() {
        println!("[fonts] 글꼴 없음: {} — 이 글자들은 안 보인다", missing.join(", "));
    }
    if !loaded.is_empty() {
        println!("[fonts] 스크립트 폰트: {}", loaded.join(", "));
    }
    assert!(!fonts.is_empty(), "no usable font found (system or bundled)");
    font::FontStack::new(fonts)
}

fn dump_glyphs(text: &str, path: &str) {
    // KESTREL_FONT 가 지정되면 그 폰트 하나로(예: CFF .otf 검증). 아니면 기본 스택.
    let fonts = match std::env::var("KESTREL_FONT") {
        Ok(p) => {
            let f = font::Font::from_bytes(fs::read(&p).expect("read font")).expect("parse font");
            font::FontStack::new(vec![f])
        }
        Err(_) => load_fonts(&page_scripts(text)),
    };
    let px = 96.0f32;

    let mut cells: Vec<raster::CoverageBitmap> = Vec::new();
    for ch in text.chars() {
        let (fi, gid) = fonts.glyph_for(ch);
        cells.push(raster::rasterize_glyph(fonts.font(fi), gid, px));
    }

    // 베이스라인 정렬 + advance 기반 자간 (M2b 인라인 레이아웃 미리보기)
    let baseline = cells.iter().map(|b| b.top).max().unwrap_or(0).max(0);
    let below = cells.iter().map(|b| (b.height as i32 - b.top).max(0)).max().unwrap_or(0);
    let canvas_h = (baseline + below + 2).max(1) as usize;
    let total_adv: f32 = cells.iter().map(|b| b.advance).sum();
    let canvas_w = (total_adv.ceil() as usize + 8).max(1);

    // 어두운 배경 + 흰 글자 (커버리지 = 밝기)
    let mut img = vec![20u8; canvas_w * canvas_h * 3];
    let mut pen_x = 4.0f32;
    for bm in &cells {
        let gx = pen_x + bm.left as f32;
        let y_off = baseline - bm.top + 1;
        for y in 0..bm.height {
            let cy = y_off + y as i32;
            if cy < 0 || cy as usize >= canvas_h {
                continue;
            }
            for x in 0..bm.width {
                let v = bm.data[y * bm.width + x];
                if v == 0 {
                    continue;
                }
                let px_x = gx as i32 + x as i32;
                if px_x < 0 || px_x as usize >= canvas_w {
                    continue;
                }
                let idx = (cy as usize * canvas_w + px_x as usize) * 3;
                let g = 20u8.saturating_add(v);
                img[idx] = g;
                img[idx + 1] = g;
                img[idx + 2] = g;
            }
        }
        pen_x += bm.advance;
    }

    let mut data = format!("P6\n{} {}\n255\n", canvas_w, canvas_h).into_bytes();
    data.extend_from_slice(&img);
    fs::write(path, data).expect("write ppm");
    println!("glyphs rendered to {}", path);
}

#[cfg(test)]
mod import_tests {
    use super::{extract_imports, parse_import_stmt};

    #[test]
    fn extracts_url_and_bare_imports_with_media() {
        let css = r#"@import url("theme.css");
@import "high.css" (prefers-contrast: more);
@import url(dark.css) (prefers-color-scheme: dark) and (min-width: 100px);
p { color: red; }"#;
        let imps = extract_imports(css);
        assert_eq!(imps.len(), 3);
        assert_eq!(imps[0], ("theme.css".to_string(), "".to_string()));
        assert_eq!(imps[1].0, "high.css");
        assert_eq!(imps[1].1, "(prefers-contrast: more)");
        assert_eq!(imps[2].0, "dark.css");
        assert!(imps[2].1.contains("prefers-color-scheme"));
    }

    #[test]
    fn parse_import_stmt_strips_quotes_and_url() {
        assert_eq!(parse_import_stmt("url(\"a.css\")"), Some(("a.css".into(), "".into())));
        assert_eq!(parse_import_stmt("url(b.css) screen"), Some(("b.css".into(), "screen".into())));
        assert_eq!(parse_import_stmt("'c.css'"), Some(("c.css".into(), "".into())));
        assert_eq!(parse_import_stmt("nonsense"), None);
    }
}
