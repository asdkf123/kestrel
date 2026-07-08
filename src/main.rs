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
                let dom = html::parse(html);
                let mut count = 0usize;
                count_elements(&dom, &mut count);
                println!("parsed OK: {} elements (http {})", count, resp.status);
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

    let needs_korean = page_needs_korean(&html_source);
    let mut root_node = html::parse(html_source);
    js::run_scripts(&mut root_node);
    let stylesheet = css::parse(css_source);
    let style_root = style::style_tree(&root_node, &stylesheet);

    let viewport_width: u32 = 800;
    let viewport_height: u32 = 600;

    let mut viewport: layout::Dimensions = Default::default();
    viewport.content.width = viewport_width as f32;
    viewport.content.height = viewport_height as f32;

    let fonts = load_fonts(needs_korean);
    let mut cache = raster::GlyphCache::new();
    // 로컬 데모도 절대 URL <img>/배경 이미지는 가져온다 (베이스는 상대경로 해석용 임의값).
    let base = url::Url::parse("https://localhost/").unwrap();
    let mut srcs = Vec::new();
    collect_img_srcs(&root_node, &mut srcs);
    collect_bg_urls(&style_root, &mut srcs);
    let (images, img_map) = load_images(srcs, &base);

    let layout_root = layout::layout_tree(&style_root, viewport, &fonts, &img_map);
    let items = paint::build_display_list(&layout_root);
    let doc_height = layout_root.dimensions.margin_box().height;
    let mut links = Vec::new();
    layout::collect_link_regions(&layout_root, &mut links);

    // 헤드리스 렌더 모드: KESTREL_RENDER_TO 가 설정되면 창 대신 PPM 으로 출력하고 종료.
    if let Ok(path) = std::env::var("KESTREL_RENDER_TO") {
        let canvas = paint::rasterize(
            &items,
            viewport_width as usize,
            viewport_height as usize,
            0.0,
            1.0,
            &fonts,
            &mut cache,
            &images,
        );
        write_ppm(&canvas, &path);
        println!("rendered to {}", path);
        return;
    }

    window::run_page(
        window::Page { items, images, fonts, doc_height, links, url: base },
        viewport_width,
        viewport_height,
        build_page,
    );
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

fn count_elements(node: &dom::Node, count: &mut usize) {
    if let dom::NodeType::Element(_) = &node.node_type {
        *count += 1;
    }
    for c in &node.children {
        count_elements(c, count);
    }
}

fn collect_img_srcs(node: &dom::Node, out: &mut Vec<String>) {
    if let dom::NodeType::Element(e) = &node.node_type {
        if e.tag_name == "img" {
            if let Some(src) = e.attributes.get("src") {
                if !src.is_empty() {
                    out.push(src.clone());
                }
            }
        }
    }
    for c in &node.children {
        collect_img_srcs(c, out);
    }
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

fn collect_links(node: &dom::Node, out: &mut Vec<String>) {
    if let dom::NodeType::Element(e) = &node.node_type {
        if e.tag_name == "link" {
            let rel = e.attributes.get("rel").map(|s| s.as_str()).unwrap_or("");
            if rel.split_whitespace().any(|r| r.eq_ignore_ascii_case("stylesheet")) {
                if let Some(href) = e.attributes.get("href") {
                    out.push(href.clone());
                }
            }
        }
    }
    for c in &node.children {
        collect_links(c, out);
    }
}

fn extract_css(node: &dom::Node, out: &mut String) {
    if let dom::NodeType::Element(e) = &node.node_type {
        if e.tag_name == "style" {
            for c in &node.children {
                if let dom::NodeType::Text(t) = &c.node_type {
                    out.push_str(t);
                    out.push('\n');
                }
            }
        }
    }
    for c in &node.children {
        extract_css(c, out);
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
    let needs_korean = page_needs_korean(&html);
    let mut dom = html::parse(html);

    // 인라인 <script> 실행 (동기 스크립트처럼 첫 렌더 전, DOM 변형 가능)
    js::run_scripts(&mut dom);

    let base = url::Url::parse(url).ok()?;

    // 스타일: UA → 외부 <link> CSS → 인라인 <style> 순서로 합침
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
                sheet.rules.extend(css::parse(css_text).rules);
            }
        }
    }
    let mut inline_css = String::new();
    extract_css(&dom, &mut inline_css);
    sheet.rules.extend(css::parse(inline_css).rules);

    let style_root = style::style_tree(&dom, &sheet);

    // 이미지: <img src> (DOM) + 적용된 background-image (스타일 트리)
    let (images, img_map) = {
        let mut srcs = Vec::new();
        collect_img_srcs(&dom, &mut srcs);
        collect_bg_urls(&style_root, &mut srcs);
        load_images(srcs, &base)
    };

    let viewport_width: u32 = 1000;
    let mut viewport: layout::Dimensions = Default::default();
    viewport.content.width = viewport_width as f32;

    let fonts = load_fonts(needs_korean);
    println!(
        "[fonts] page_korean={} → {} font(s) loaded (한글 폰트 {})",
        needs_korean,
        fonts.fonts.len(),
        if needs_korean { "로드" } else { "생략" }
    );

    let layout_root = layout::layout_tree(&style_root, viewport, &fonts, &img_map);
    let items = paint::build_display_list(&layout_root);
    let doc_height = layout_root.dimensions.margin_box().height;
    let mut links = Vec::new();
    layout::collect_link_regions(&layout_root, &mut links);
    println!("[문서 높이 {}px, 링크 {}개]", doc_height as u32, links.len());

    Some(window::Page { items, images, fonts, doc_height, links, url: base })
}

fn render_url(url: &str) {
    let Some(page) = build_page(url) else { return };

    // 헤드리스: (viewport 크기) 슬라이스를 PPM 으로. KESTREL_SCROLL 로 시작 오프셋 지정.
    if let Ok(path) = std::env::var("KESTREL_RENDER_TO") {
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
