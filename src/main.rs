mod css;
mod dom;
mod html;
mod layout;
mod paint;
mod style;
mod window;

use std::fs;

fn main() {
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

    let layout_root = layout::layout_tree(&style_root, viewport);
    let canvas = paint::paint(
        &layout_root,
        layout::Rect {
            x: 0.0,
            y: 0.0,
            width: viewport_width as f32,
            height: viewport_height as f32,
        },
    );

    // 헤드리스 렌더 모드: KESTREL_RENDER_TO 가 설정되면 창 대신 PPM 이미지로 출력하고 종료.
    // (GUI 없이 렌더링 결과를 검증할 때 사용. 기본 동작은 창 띄우기.)
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
