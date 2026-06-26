mod css;
mod dom;
mod font;
mod html;
mod http;
mod layout;
mod paint;
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
    if args.len() >= 2 && args[1].contains("://") {
        render_url(&args[1]);
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

    let root_node = html::parse(html_source);
    let stylesheet = css::parse(css_source);
    let style_root = style::style_tree(&root_node, &stylesheet);

    let viewport_width: u32 = 800;
    let viewport_height: u32 = 600;

    let mut viewport: layout::Dimensions = Default::default();
    viewport.content.width = viewport_width as f32;
    viewport.content.height = viewport_height as f32;

    let font_bytes = fs::read("assets/fonts/Kestrel.ttf").expect("read font");
    let font = font::Font::from_bytes(font_bytes).expect("parse font");
    let mut cache = raster::GlyphCache::new();

    let layout_root = layout::layout_tree(&style_root, viewport, &font);
    let canvas = paint::paint(
        &layout_root,
        layout::Rect { x: 0.0, y: 0.0, width: viewport_width as f32, height: viewport_height as f32 },
        &font,
        &mut cache,
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
    let dom = html::parse(html);

    let mut page_css = String::new();
    extract_css(&dom, &mut page_css);

    let mut sheet = css::user_agent_stylesheet();
    sheet.rules.extend(css::parse(page_css).rules);

    let style_root = style::style_tree(&dom, &sheet);

    let viewport_width: u32 = 1000;
    let viewport_height: u32 = 1400;
    let mut viewport: layout::Dimensions = Default::default();
    viewport.content.width = viewport_width as f32;
    viewport.content.height = viewport_height as f32;

    let font = font::Font::from_bytes(fs::read("assets/fonts/Kestrel.ttf").expect("read font"))
        .expect("parse font");
    let mut cache = raster::GlyphCache::new();

    let layout_root = layout::layout_tree(&style_root, viewport, &font);
    let canvas = paint::paint(
        &layout_root,
        layout::Rect { x: 0.0, y: 0.0, width: viewport_width as f32, height: viewport_height as f32 },
        &font,
        &mut cache,
    );

    if let Ok(path) = std::env::var("KESTREL_RENDER_TO") {
        write_ppm(&canvas, &path);
        println!("rendered to {}", path);
        return;
    }
    window::run(canvas.to_u32_buffer(), viewport_width, viewport_height);
}

fn dump_glyphs(text: &str, path: &str) {
    let bytes = fs::read("assets/fonts/Kestrel.ttf").expect("read font");
    let font = font::Font::from_bytes(bytes).expect("parse font");
    let px = 96.0f32;

    let mut cells: Vec<raster::CoverageBitmap> = Vec::new();
    for ch in text.chars() {
        let gid = font.glyph_index(ch);
        cells.push(raster::rasterize_glyph(&font, gid, px));
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
