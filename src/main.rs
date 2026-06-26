mod cff;
mod css;
mod dom;
mod font;
mod html;
mod http;
mod inflate;
mod layout;
mod paint;
mod png;
mod raster;
mod style;
mod url;
mod window;

use std::fs;

fn main() {
    // 네트워크 fetch 모드: kestrel --fetch <url>
    let args: Vec<String> = std::env::args().collect();
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
    let root_node = html::parse(html_source);
    let stylesheet = css::parse(css_source);
    let style_root = style::style_tree(&root_node, &stylesheet);

    let viewport_width: u32 = 800;
    let viewport_height: u32 = 600;

    let mut viewport: layout::Dimensions = Default::default();
    viewport.content.width = viewport_width as f32;
    viewport.content.height = viewport_height as f32;

    let fonts = load_fonts(needs_korean);
    let mut cache = raster::GlyphCache::new();
    // 로컬 데모도 절대 URL <img> 는 가져온다 (베이스는 상대경로 해석용 임의값).
    let base = url::Url::parse("https://localhost/").unwrap();
    let (images, img_map) = load_images(&root_node, &base);

    let layout_root = layout::layout_tree(&style_root, viewport, &fonts, &img_map);
    let canvas = paint::paint(
        &layout_root,
        layout::Rect { x: 0.0, y: 0.0, width: viewport_width as f32, height: viewport_height as f32 },
        &fonts,
        &mut cache,
        &images,
    );

    // 헤드리스 렌더 모드: KESTREL_RENDER_TO 가 설정되면 창 대신 PPM 으로 출력하고 종료.
    if let Ok(path) = std::env::var("KESTREL_RENDER_TO") {
        write_ppm(&canvas, &path);
        println!("rendered to {}", path);
        return;
    }

    let buffer = canvas.to_u32_buffer();
    window::run(buffer, viewport_width, viewport_height);
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

fn load_images(dom: &dom::Node, base: &url::Url) -> (Vec<png::Image>, layout::ImageMap) {
    let mut srcs = Vec::new();
    collect_img_srcs(dom, &mut srcs);
    let mut images = Vec::new();
    let mut map = layout::ImageMap::new();
    for src in srcs {
        if map.contains_key(&src) {
            continue;
        }
        if let Some(u) = base.join(&src) {
            if let Ok(resp) = http::fetch(&u.as_string()) {
                if let Some(img) = png::decode(&resp.body) {
                    let (w, h) = (img.width, img.height);
                    let idx = images.len();
                    images.push(img);
                    map.insert(src, (idx, w, h));
                }
            }
        }
    }
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

fn render_url(url: &str) {
    let resp = match http::fetch(url) {
        Ok(r) => r,
        Err(e) => {
            println!("fetch error: {:?}", e);
            return;
        }
    };
    println!("fetched {} ({} bytes, http {})", url, resp.body.len(), resp.status);

    let html = String::from_utf8_lossy(&resp.body).to_string();
    let needs_korean = page_needs_korean(&html);
    let dom = html::parse(html);

    let base = url::Url::parse(url).ok();
    let (images, img_map) = match &base {
        Some(b) => load_images(&dom, b),
        None => (Vec::new(), layout::ImageMap::new()),
    };

    // 스타일: UA → 외부 <link> CSS → 인라인 <style> 순서로 합침
    let mut sheet = css::user_agent_stylesheet();
    if let Some(base) = &base {
        let mut hrefs = Vec::new();
        collect_links(&dom, &mut hrefs);
        for href in hrefs.iter().take(10) {
            if let Some(u) = base.join(href) {
                if let Ok(r) = http::fetch(&u.as_string()) {
                    let css_text = String::from_utf8_lossy(&r.body).to_string();
                    sheet.rules.extend(css::parse(css_text).rules);
                }
            }
        }
    }
    let mut inline_css = String::new();
    extract_css(&dom, &mut inline_css);
    sheet.rules.extend(css::parse(inline_css).rules);

    let style_root = style::style_tree(&dom, &sheet);

    let viewport_width: u32 = 1000;
    let viewport_height: u32 = 1400;
    let mut viewport: layout::Dimensions = Default::default();
    viewport.content.width = viewport_width as f32;
    viewport.content.height = viewport_height as f32;

    let fonts = load_fonts(needs_korean);
    println!(
        "[fonts] page_korean={} → {} font(s) loaded (한글 폰트 {})",
        needs_korean,
        fonts.fonts.len(),
        if needs_korean { "로드" } else { "생략" }
    );
    let mut cache = raster::GlyphCache::new();

    let layout_root = layout::layout_tree(&style_root, viewport, &fonts, &img_map);
    let canvas = paint::paint(
        &layout_root,
        layout::Rect { x: 0.0, y: 0.0, width: viewport_width as f32, height: viewport_height as f32 },
        &fonts,
        &mut cache,
        &images,
    );

    if let Ok(path) = std::env::var("KESTREL_RENDER_TO") {
        write_ppm(&canvas, &path);
        println!("rendered to {}", path);
        return;
    }
    window::run(canvas.to_u32_buffer(), viewport_width, viewport_height);
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
