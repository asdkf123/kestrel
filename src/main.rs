mod bench;
mod cff;
mod css;
mod dom;
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

    let source_korean = page_needs_korean(&html_source);
    let mut root_node = html::parse_dom(html_source);
    let needs_korean = source_korean || page_needs_korean(&root_node.text_content(root_node.root));
    let js_rt = js::run_scripts(&mut root_node, "https://localhost/");
    // 실제 페이지처럼 UA 스타일시트를 먼저 깔고 그 위에 저작자 CSS 를 얹는다.
    let mut stylesheet = css::user_agent_stylesheet();
    stylesheet.rules.extend(css::parse(css_source).rules);

    let viewport_width: u32 = 800;
    let viewport_height: u32 = 600;

    let fonts = load_fonts(needs_korean);
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

    let mut page = window::Page {
        dom: root_node,
        sheet: stylesheet,
        images,
        img_map,
        fonts,
        js: js_rt,
        url: base,
        viewport_width: viewport_width as f32,
        items: Vec::new(),
        links: Vec::new(),
        element_rects: Vec::new(),
        doc_height: 0.0,
        focused_input: None,
    };
    page.rebuild();

    // 헤드리스 렌더 모드: KESTREL_RENDER_TO 가 설정되면 창 대신 PPM 으로 출력하고 종료.
    if let Ok(path) = std::env::var("KESTREL_RENDER_TO") {
        page.flush_timers_headless();
        let canvas = paint::rasterize(
            &page.items,
            viewport_width as usize,
            viewport_height as usize,
            0.0,
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
                if let Some(src) = e.attributes.get("src") {
                    if !src.is_empty() {
                        out.push(src.clone());
                    }
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
    std::thread::scope(|scope| {
        for _ in 0..uniq.len().min(8) {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= uniq.len() {
                    break;
                }
                let img = base
                    .join(&uniq[i])
                    .and_then(|u| http::fetch(&u.as_string()).ok())
                    .and_then(|resp| decode_image(&resp.body));
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

fn collect_links(dom: &dom::Dom, out: &mut Vec<String>) {
    walk_dom(dom, dom.root, &mut |n| {
        if let dom::NodeType::Element(e) = &n.node_type {
            if e.tag_name == "link" {
                let rel = e.attributes.get("rel").map(|s| s.as_str()).unwrap_or("");
                if rel.split_whitespace().any(|r| r.eq_ignore_ascii_case("stylesheet")) {
                    if let Some(href) = e.attributes.get("href") {
                        out.push(href.clone());
                    }
                }
            }
        }
    });
}

fn extract_css(dom: &dom::Dom, out: &mut String) {
    let mut style_ids = Vec::new();
    walk_dom_ids(dom, dom.root, &mut |id| {
        if let dom::NodeType::Element(e) = &dom.get(id).node_type {
            if e.tag_name == "style" {
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

    let html = String::from_utf8_lossy(&resp.body).to_string();
    let source_korean = page_needs_korean(&html);
    let mut dom = html::parse_dom(html);
    // 원문엔 없어도 엔티티(&#51060; 등) 디코드 후 한글이 나올 수 있다 (google.co.kr)
    let needs_korean = source_korean || page_needs_korean(&dom.text_content(dom.root));

    // 인라인 <script> 실행 (동기 스크립트처럼 첫 렌더 전, DOM 변형 가능).
    // 반환된 JS 런타임은 이벤트 핸들러(클로저)를 들고 Page 에 보관된다.
    let js_rt = js::run_scripts(&mut dom, url);

    let base = url::Url::parse(url).ok()?;

    // 스타일: UA → 외부 <link> CSS → 인라인 <style> 순서로 합침.
    // 저작자 CSS 는 실제 뷰포트 폭으로 파싱해 @media 를 평가한다.
    let page_vw = 1000.0f32;
    let mut sheet = css::user_agent_stylesheet();
    let mut hrefs = Vec::new();
    collect_links(&dom, &mut hrefs);
    if !hrefs.is_empty() {
        println!("[css] 외부 스타일시트 {}개 로드 중...", hrefs.len().min(10));
    }
    for href in hrefs.iter().take(10) {
        if let Some(u) = base.join(href) {
            if let Ok(r) = http::fetch(&u.as_string()) {
                let css_text = String::from_utf8_lossy(&r.body).to_string();
                sheet.rules.extend(css::parse_viewport(css_text, page_vw).rules);
            }
        }
    }
    let mut inline_css = String::new();
    extract_css(&dom, &mut inline_css);
    sheet.rules.extend(css::parse_viewport(inline_css, page_vw).rules);

    // 이미지: <img src> (DOM) + 적용된 background-image (스타일 트리는 일시 사용)
    let mut srcs = Vec::new();
    collect_img_srcs(&dom, &mut srcs);
    {
        let style_root = style::style_tree(&dom, &sheet);
        collect_bg_urls(&style_root, &mut srcs);
    }
    let (images, img_map) = load_images(srcs, &base);

    let fonts = load_fonts(needs_korean);
    println!(
        "[fonts] page_korean={} → {} font(s) loaded (한글 폰트 {})",
        needs_korean,
        fonts.fonts.len(),
        if needs_korean { "로드" } else { "생략" }
    );

    let mut page = window::Page {
        dom,
        sheet,
        images,
        img_map,
        fonts,
        js: js_rt,
        url: base,
        viewport_width: page_vw,
        items: Vec::new(),
        links: Vec::new(),
        element_rects: Vec::new(),
        doc_height: 0.0,
        focused_input: None,
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
        let scroll = std::env::var("KESTREL_SCROLL")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(0.0)
            .clamp(0.0, (page.doc_height - vh as f32).max(0.0));
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
fn page_needs_korean(text: &str) -> bool {
    text.chars().any(|c| {
        ('\u{AC00}'..='\u{D7A3}').contains(&c)
            || ('\u{1100}'..='\u{11FF}').contains(&c)
            || ('\u{3130}'..='\u{318F}').contains(&c)
    })
}

// 시스템 폰트로 폰트 스택 구성 (번들 없음). 라틴 주 폰트 + (필요시) 한글 폴백.
// 무거운 한글 폰트(수십 MB)는 페이지에 한글이 있을 때만 읽어 RAM 을 아낀다.
fn load_fonts(needs_korean: bool) -> font::FontStack {
    let latin: &[&str] = &[
        "/System/Library/Fonts/Helvetica.ttc",
        "/System/Library/Fonts/HelveticaNeue.ttc",
        "/System/Library/Fonts/SFNS.ttf",
        "assets/fonts/Latin.ttf",
    ];
    let korean: &[&str] = &[
        "/System/Library/Fonts/Supplemental/AppleGothic.ttf",
        "/System/Library/Fonts/AppleSDGothicNeo.ttc",
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
    if needs_korean {
        if let Some(f) = try_load(korean, '한') {
            fonts.push(f);
        }
    }
    if fonts.is_empty() {
        if let Ok(b) = fs::read("assets/fonts/Latin.ttf") {
            if let Ok(f) = font::Font::from_bytes(b) {
                fonts.push(f);
            }
        }
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
        Err(_) => load_fonts(page_needs_korean(text)),
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
