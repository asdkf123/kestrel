use crate::css::{Color, Value};
use crate::font::FontStack;
use crate::layout::{GlyphInstance, LayoutBox, Rect};
use crate::raster::{CoverageBitmap, GlyphCache};

pub struct Canvas {
    pub pixels: Vec<Color>,
    pub width: usize,
    pub height: usize,
}

impl Canvas {
    fn new(width: usize, height: usize) -> Canvas {
        let white = Color { r: 255, g: 255, b: 255, a: 255 };
        Canvas { pixels: vec![white; width * height], width, height }
    }

    pub fn fill_rect(&mut self, color: Color, rect: Rect) {
        let x0 = rect.x.clamp(0.0, self.width as f32) as usize;
        let y0 = rect.y.clamp(0.0, self.height as f32) as usize;
        let x1 = (rect.x + rect.width).clamp(0.0, self.width as f32) as usize;
        let y1 = (rect.y + rect.height).clamp(0.0, self.height as f32) as usize;
        for y in y0..y1 {
            for x in x0..x1 {
                self.pixels[y * self.width + x] = color;
            }
        }
    }

    // 둥근 사각형 채우기 (모서리 안티에일리어싱). radius 는 물리 px.
    pub fn fill_round_rect(&mut self, color: Color, rect: Rect, radius: f32) {
        if rect.width <= 0.0 || rect.height <= 0.0 {
            return;
        }
        let r = radius.min(rect.width / 2.0).min(rect.height / 2.0).max(0.0);
        if r <= 0.0 {
            self.fill_rect(color, rect);
            return;
        }
        let (x0, y0) = (rect.x, rect.y);
        let (x1, y1) = (rect.x + rect.width, rect.y + rect.height);
        let px0 = x0.floor().max(0.0) as usize;
        let py0 = y0.floor().max(0.0) as usize;
        let px1 = (x1.ceil().max(0.0) as usize).min(self.width);
        let py1 = (y1.ceil().max(0.0) as usize).min(self.height);
        let clamp01 = |v: f32| v.clamp(0.0, 1.0);
        for py in py0..py1 {
            let fy = py as f32 + 0.5;
            for px in px0..px1 {
                let fx = px as f32 + 0.5;
                // 모서리 밴드(양축 모두 반경 안)면 코너 중심까지 거리로 커버리지,
                // 아니면 직선 변 안티에일리어싱.
                let in_x = fx < x0 + r || fx > x1 - r;
                let in_y = fy < y0 + r || fy > y1 - r;
                let cov = if in_x && in_y {
                    let ncx = if fx < x0 + r { x0 + r } else { x1 - r };
                    let ncy = if fy < y0 + r { y0 + r } else { y1 - r };
                    clamp01(r - ((fx - ncx).powi(2) + (fy - ncy).powi(2)).sqrt() + 0.5)
                } else {
                    let cx = clamp01(fx - x0 + 0.5).min(clamp01(x1 - fx + 0.5));
                    let cy = clamp01(fy - y0 + 0.5).min(clamp01(y1 - fy + 0.5));
                    cx.min(cy)
                };
                if cov <= 0.0 {
                    continue;
                }
                let idx = py * self.width + px;
                self.pixels[idx] = blend(self.pixels[idx], color, (cov * 255.0).round() as u8);
            }
        }
    }

    // 부드러운 둥근 사각형(드롭 섀도). 둥근 박스 SDF 로 경계에서 blur 폭에 걸쳐
    // 커버리지를 선형 감쇠시킨다. color 의 알파와 곱해 반투명 그림자를 만든다.
    pub fn fill_soft_round_rect(&mut self, color: Color, rect: Rect, radius: f32, blur: f32) {
        if rect.width <= 0.0 || rect.height <= 0.0 {
            return;
        }
        let soft = blur.max(0.75);
        let (hw, hh) = (rect.width / 2.0, rect.height / 2.0);
        let (ccx, ccy) = (rect.x + hw, rect.y + hh);
        let r = radius.min(hw).min(hh).max(0.0);
        let x0 = (rect.x - soft).floor().max(0.0) as usize;
        let y0 = (rect.y - soft).floor().max(0.0) as usize;
        let x1 = ((rect.x + rect.width + soft).ceil().max(0.0) as usize).min(self.width);
        let y1 = ((rect.y + rect.height + soft).ceil().max(0.0) as usize).min(self.height);
        let base_a = color.a as f32 / 255.0;
        for py in y0..y1 {
            let fy = py as f32 + 0.5;
            for px in x0..x1 {
                let fx = px as f32 + 0.5;
                // 둥근 박스 SDF (내부 음수, 외부 양수)
                let qx = (fx - ccx).abs() - (hw - r);
                let qy = (fy - ccy).abs() - (hh - r);
                let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
                let sdf = outside + qx.max(qy).min(0.0) - r;
                let cov = (0.5 - sdf / soft).clamp(0.0, 1.0);
                if cov <= 0.0 {
                    continue;
                }
                let a = (cov * base_a * 255.0).round() as u8;
                if a == 0 {
                    continue;
                }
                let idx = py * self.width + px;
                self.pixels[idx] = blend(self.pixels[idx], color, a);
            }
        }
    }

    pub fn to_u32_buffer(&self) -> Vec<u32> {
        self.pixels
            .iter()
            .map(|c| (c.r as u32) << 16 | (c.g as u32) << 8 | (c.b as u32))
            .collect()
    }
}

fn get_color(layout_box: &LayoutBox, name: &str) -> Option<Color> {
    match layout_box.styled_node.value(name) {
        Some(Value::Color(c)) => Some(c),
        _ => None,
    }
}

fn blend(bg: Color, fg: Color, a: u8) -> Color {
    let af = a as f32 / 255.0;
    let mix = |b: u8, f: u8| (b as f32 * (1.0 - af) + f as f32 * af).round() as u8;
    Color { r: mix(bg.r, fg.r), g: mix(bg.g, fg.g), b: mix(bg.b, fg.b), a: 255 }
}

fn blit_glyph(canvas: &mut Canvas, bm: &CoverageBitmap, gi: &GlyphInstance) {
    let ox = (gi.x + bm.left as f32).round() as i32;
    let oy = (gi.baseline_y - bm.top as f32).round() as i32;
    for y in 0..bm.height {
        let cy = oy + y as i32;
        if cy < 0 || cy as usize >= canvas.height {
            continue;
        }
        for x in 0..bm.width {
            let a = bm.data[y * bm.width + x];
            if a == 0 {
                continue;
            }
            let cx = ox + x as i32;
            if cx < 0 || cx as usize >= canvas.width {
                continue;
            }
            let idx = cy as usize * canvas.width + cx as usize;
            canvas.pixels[idx] = blend(canvas.pixels[idx], gi.color, a);
        }
    }
}

// 디스플레이 리스트: 레이아웃 트리에서 뽑아낸 소유(owned) 그리기 명령 목록.
// 트리 borrow 없이 스크롤 오프셋만 바꿔 반복 래스터화할 수 있다 (실제 브라우저 구조).
#[derive(Debug, Clone)]
pub enum DisplayItem {
    Rect { color: Color, rect: Rect },
    RoundRect { color: Color, rect: Rect, radius: f32 },
    Shadow { color: Color, rect: Rect, radius: f32, blur: f32 },
    Image { image: usize, rect: Rect },
    Glyph(GlyphInstance),
}

// CSS 테두리 4변을 사각형으로 발행. 변마다 그리는 조건: border-<side>-width > 0
// 이고 border-<side>-style 이 명시되고 none/hidden 이 아님 (기본 none → 안 그림).
// 색은 border-<side>-color > border-color > 요소 color(currentColor) 순.
fn emit_borders(lb: &LayoutBox, items: &mut Vec<DisplayItem>) {
    let bw = lb.dimensions.border;
    if bw.top <= 0.0 && bw.right <= 0.0 && bw.bottom <= 0.0 && bw.left <= 0.0 {
        return;
    }
    let b = lb.dimensions.border_box();
    let side_color = |side: &str| border_side_color(lb, side);
    let drawn = |side: &str| border_side_drawn(lb, side);
    if bw.top > 0.0 && drawn("top") {
        items.push(DisplayItem::Rect {
            color: side_color("top"),
            rect: Rect { x: b.x, y: b.y, width: b.width, height: bw.top },
        });
    }
    if bw.bottom > 0.0 && drawn("bottom") {
        items.push(DisplayItem::Rect {
            color: side_color("bottom"),
            rect: Rect { x: b.x, y: b.y + b.height - bw.bottom, width: b.width, height: bw.bottom },
        });
    }
    if bw.left > 0.0 && drawn("left") {
        items.push(DisplayItem::Rect {
            color: side_color("left"),
            rect: Rect { x: b.x, y: b.y, width: bw.left, height: b.height },
        });
    }
    if bw.right > 0.0 && drawn("right") {
        items.push(DisplayItem::Rect {
            color: side_color("right"),
            rect: Rect { x: b.x + b.width - bw.right, y: b.y, width: bw.right, height: b.height },
        });
    }
}

// 균일 border-radius (물리 아님, 논리 px). 퍼센트는 박스 짧은 변 기준.
fn uniform_radius(lb: &LayoutBox) -> f32 {
    let b = lb.dimensions.border_box();
    match lb.styled_node.value("border-radius") {
        Some(Value::Length(v, crate::css::Unit::Px)) => v.max(0.0),
        Some(Value::Length(v, crate::css::Unit::Percent)) => {
            v / 100.0 * b.width.min(b.height)
        }
        _ => 0.0,
    }
}

// box-shadow(outset) 를 박스 뒤에 발행. rect = border_box + spread, (x,y) 만큼 이동.
fn emit_box_shadow(lb: &LayoutBox, items: &mut Vec<DisplayItem>) {
    let len = |name: &str| match lb.styled_node.value(name) {
        Some(Value::Length(v, crate::css::Unit::Px)) => Some(v),
        _ => None,
    };
    let (dx, dy) = match (len("box-shadow-x"), len("box-shadow-y")) {
        (Some(x), Some(y)) => (x, y),
        _ => return,
    };
    let blur = len("box-shadow-blur").unwrap_or(0.0);
    let spread = len("box-shadow-spread").unwrap_or(0.0);
    let color = match lb.styled_node.value("box-shadow-color") {
        Some(Value::Color(c)) => c,
        _ => Color { r: 0, g: 0, b: 0, a: 128 },
    };
    let b = lb.dimensions.border_box();
    let rect = Rect {
        x: b.x + dx - spread,
        y: b.y + dy - spread,
        width: b.width + 2.0 * spread,
        height: b.height + 2.0 * spread,
    };
    let radius = (uniform_radius(lb) + spread).max(0.0);
    items.push(DisplayItem::Shadow { color, rect, radius, blur });
}

// 박스 배경 + 테두리를 발행. border-radius 가 있으면 둥근 사각형으로,
// 균일 테두리는 "테두리색 라운드 → 안쪽 배경 라운드" 레이어링으로 둥근 테두리 근사.
fn emit_box_decorations(lb: &LayoutBox, items: &mut Vec<DisplayItem>) {
    let bg = get_color(lb, "background-color");
    let r = uniform_radius(lb);
    let bw = lb.dimensions.border;
    let b = lb.dimensions.border_box();
    let border_uniform = bw.top > 0.0
        && bw.top == bw.right
        && bw.top == bw.bottom
        && bw.top == bw.left
        && border_side_drawn(lb, "top");

    // 라운드 + 균일 테두리 + 배경: 레이어드로 둥근 테두리
    if r > 0.0 && border_uniform && bg.is_some() {
        items.push(DisplayItem::RoundRect { color: border_side_color(lb, "top"), rect: b, radius: r });
        let inner_r = (r - bw.top).max(0.0);
        items.push(DisplayItem::RoundRect {
            color: bg.unwrap(),
            rect: lb.dimensions.padding_box(),
            radius: inner_r,
        });
        return;
    }
    // 라운드 + 배경(테두리 없음/비균일): 배경만 둥글게, 테두리는 사각으로
    if r > 0.0 && bg.is_some() {
        items.push(DisplayItem::RoundRect { color: bg.unwrap(), rect: b, radius: r });
        emit_borders(lb, items);
        return;
    }
    // 그 외: 기존 사각 경로
    if let Some(color) = bg {
        items.push(DisplayItem::Rect { color, rect: b });
    }
    emit_borders(lb, items);
}

// emit_borders 와 공유되는 변별 색/그리기 판정.
fn border_side_color(lb: &LayoutBox, side: &str) -> Color {
    let default_color = get_color(lb, "color").unwrap_or(Color { r: 0, g: 0, b: 0, a: 255 });
    get_color(lb, &format!("border-{}-color", side))
        .or_else(|| get_color(lb, "border-color"))
        .unwrap_or(default_color)
}

fn border_side_drawn(lb: &LayoutBox, side: &str) -> bool {
    let style = lb
        .styled_node
        .value(&format!("border-{}-style", side))
        .or_else(|| lb.styled_node.value("border-style"));
    matches!(style, Some(Value::Keyword(ref k)) if k != "none" && k != "hidden")
}

pub fn build_display_list(root: &LayoutBox) -> Vec<DisplayItem> {
    let mut items = Vec::new();
    collect_items(root, &mut items);
    items
}

fn collect_items(layout_box: &LayoutBox, items: &mut Vec<DisplayItem>) {
    // 그림자는 박스 뒤(가장 먼저)에 그린다.
    emit_box_shadow(layout_box, items);
    // 배경 + 테두리 (border-radius 포함). input/button 도 UA CSS 로 테두리/배경을
    // 받으므로 이 공통 경로로 그려진다 (하드코딩 외형 제거 — 저작자 CSS 가 덮을 수 있음).
    emit_box_decorations(layout_box, items);
    // 배경 이미지: 박스 좌상단에 1회, 박스 크기로 클리핑 (repeat/position 미지원)
    if let Some(idx) = layout_box.background_image {
        items.push(DisplayItem::Image { image: idx, rect: layout_box.dimensions.border_box() });
    }
    if let Some(idx) = layout_box.image {
        items.push(DisplayItem::Image { image: idx, rect: layout_box.dimensions.content });
    }
    for gi in &layout_box.glyphs {
        items.push(DisplayItem::Glyph(*gi));
    }
    // 링크 밑줄 등 장식 (글리프 위에 그려도 얇아 무해)
    for (rect, color) in &layout_box.decorations {
        items.push(DisplayItem::Rect { color: *color, rect: *rect });
    }
    for child in &layout_box.children {
        collect_items(child, items);
    }
}

// 디스플레이 리스트를 scroll_y(논리 px) 만큼 위로 민 상태로 (width x height 물리 px)
// 캔버스에 그린다. scale = 물리/논리 배율 (레티나 2.0). 글리프는 scale 배 크기로
// 다시 래스터화되어 HiDPI 에서 선명하다. 뷰포트 밖 아이템은 컬링.
pub fn rasterize(
    items: &[DisplayItem],
    width: usize,
    height: usize,
    scroll_y: f32,
    scale: f32,
    fonts: &FontStack,
    cache: &mut GlyphCache,
    images: &[crate::png::Image],
) -> Canvas {
    let mut canvas = Canvas::new(width, height);
    let vh = height as f32;
    let scale_rect = |rect: &Rect| Rect {
        x: rect.x * scale,
        y: (rect.y - scroll_y) * scale,
        width: rect.width * scale,
        height: rect.height * scale,
    };
    for item in items {
        match item {
            DisplayItem::Rect { color, rect } => {
                let r = scale_rect(rect);
                if r.y + r.height < 0.0 || r.y > vh {
                    continue;
                }
                canvas.fill_rect(*color, r);
            }
            DisplayItem::RoundRect { color, rect, radius } => {
                let r = scale_rect(rect);
                if r.y + r.height < 0.0 || r.y > vh {
                    continue;
                }
                canvas.fill_round_rect(*color, r, radius * scale);
            }
            DisplayItem::Shadow { color, rect, radius, blur } => {
                let r = scale_rect(rect);
                let m = blur * scale;
                if r.y + r.height + m < 0.0 || r.y - m > vh {
                    continue;
                }
                canvas.fill_soft_round_rect(*color, r, radius * scale, blur * scale);
            }
            DisplayItem::Image { image, rect } => {
                let r = scale_rect(rect);
                if r.y + r.height < 0.0 || r.y > vh {
                    continue;
                }
                if let Some(img) = images.get(*image) {
                    blit_image(&mut canvas, img, r, scale);
                }
            }
            DisplayItem::Glyph(gi) => {
                let baseline = (gi.baseline_y - scroll_y) * scale;
                let px = gi.px * scale;
                // 대략적 글리프 세로 범위로 컬링 (ascent ~1.2em, descent ~0.4em)
                if baseline + 0.4 * px < 0.0 || baseline - 1.2 * px > vh {
                    continue;
                }
                let shifted = GlyphInstance { x: gi.x * scale, baseline_y: baseline, px, ..*gi };
                let bm = cache.get(fonts, gi.font_index, gi.glyph_id, px, gi.bold, gi.italic);
                blit_glyph(&mut canvas, bm, &shifted);
            }
        }
    }
    canvas
}

// rect(물리 px) 좌상단에 이미지를 scale 배로 그린다 (최근접 샘플링).
// rect 크기로 클리핑 (<img> 는 rect == 고유 크기 × scale 이라 무손실).
fn blit_image(canvas: &mut Canvas, img: &crate::png::Image, rect: Rect, scale: f32) {
    let ox = rect.x.round() as i32;
    let oy = rect.y.round() as i32;
    let clip_w = ((img.width as f32 * scale).min(rect.width).round()) as usize;
    let clip_h = ((img.height as f32 * scale).min(rect.height).round()) as usize;
    for y in 0..clip_h {
        let cy = oy + y as i32;
        if cy < 0 || cy as usize >= canvas.height {
            continue;
        }
        let sy = ((y as f32 / scale) as usize).min(img.height - 1);
        for x in 0..clip_w {
            let cx = ox + x as i32;
            if cx < 0 || cx as usize >= canvas.width {
                continue;
            }
            let sx = ((x as f32 / scale) as usize).min(img.width - 1);
            let s = (sy * img.width + sx) * 4;
            let fg = Color { r: img.rgba[s], g: img.rgba[s + 1], b: img.rgba[s + 2], a: 255 };
            let alpha = img.rgba[s + 3];
            let idx = cy as usize * canvas.width + cx as usize;
            canvas.pixels[idx] = blend(canvas.pixels[idx], fg, alpha);
        }
    }
}

// 텍스트 폭 측정 (캐럿 위치 계산용)
pub fn measure_text(fonts: &FontStack, text: &str, px: f32) -> f32 {
    let mut w = 0.0;
    for ch in text.chars() {
        let (fi, gid) = fonts.glyph_for(ch);
        let f = fonts.font(fi);
        w += f.advance_width(gid) as f32 * (px / f.units_per_em() as f32);
    }
    w
}

// UI 크롬용 단순 텍스트 드로잉 (주소창 등). 끝 pen x 를 반환한다 (캐럿 위치).
pub fn draw_text(
    canvas: &mut Canvas,
    fonts: &FontStack,
    cache: &mut GlyphCache,
    text: &str,
    x: f32,
    baseline_y: f32,
    px: f32,
    color: Color,
) -> f32 {
    let mut pen = x;
    for ch in text.chars() {
        let (fi, gid) = fonts.glyph_for(ch);
        let f = fonts.font(fi);
        let adv = f.advance_width(gid) as f32 * (px / f.units_per_em() as f32);
        if !ch.is_whitespace() {
            let gi = GlyphInstance {
                font_index: fi,
                glyph_id: gid,
                x: pen,
                baseline_y,
                px,
                color,
                bold: false,
                italic: false,
            };
            let bm = cache.get(fonts, fi, gid, px, false, false);
            blit_glyph(canvas, bm, &gi);
        }
        pen += adv;
    }
    pen
}

pub fn paint(
    layout_root: &LayoutBox,
    bounds: Rect,
    fonts: &FontStack,
    cache: &mut GlyphCache,
    images: &[crate::png::Image],
) -> Canvas {
    let items = build_display_list(layout_root);
    rasterize(&items, bounds.width as usize, bounds.height as usize, 0.0, 1.0, fonts, cache, images)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::Color;

    fn fonts() -> crate::font::FontStack {
        let f = crate::font::Font::from_bytes(std::fs::read("assets/fonts/Latin.ttf").unwrap())
            .unwrap();
        crate::font::FontStack::new(vec![f])
    }

    fn canvas_for(html: &str, css: &str, w: f32, h: f32) -> Canvas {
        let root = crate::html::parse_dom(html.to_string());
        let ss = crate::css::parse(css.to_string());
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: crate::layout::Dimensions = Default::default();
        viewport.content.width = w;
        let fs = fonts();
        let imgs = crate::layout::ImageMap::new();
        let layout_root = crate::layout::layout_tree(&styled, viewport, &fs, &imgs);
        let mut cache = crate::raster::GlyphCache::new();
        paint(
            &layout_root,
            crate::layout::Rect { x: 0.0, y: 0.0, width: w, height: h },
            &fs,
            &mut cache,
            &[],
        )
    }

    #[test]
    fn fills_background_rect_red_over_white() {
        let canvas = canvas_for(
            "<div></div>",
            "div { display: block; width: 2px; height: 2px; background-color: #ff0000; }",
            4.0,
            4.0,
        );
        assert_eq!(canvas.pixels[0], Color { r: 255, g: 0, b: 0, a: 255 });
        assert_eq!(canvas.pixels[3 * 4 + 3], Color { r: 255, g: 255, b: 255, a: 255 });
    }

    #[test]
    fn to_u32_packs_rgb() {
        let canvas = canvas_for(
            "<div></div>",
            "div { display: block; width: 1px; height: 1px; background-color: #ff0000; }",
            1.0,
            1.0,
        );
        let buf = canvas.to_u32_buffer();
        assert_eq!(buf[0], 0x00_ff_00_00);
    }

    #[test]
    fn rasterize_applies_scroll_offset() {
        let red = Color { r: 255, g: 0, b: 0, a: 255 };
        let white = Color { r: 255, g: 255, b: 255, a: 255 };
        let items = vec![DisplayItem::Rect {
            color: red,
            rect: crate::layout::Rect { x: 0.0, y: 10.0, width: 2.0, height: 2.0 },
        }];
        let fs = fonts();
        let mut cache = crate::raster::GlyphCache::new();
        // 스크롤 0: y=10 은 4px 캔버스 밖 → 전부 흰색
        let c0 = rasterize(&items, 4, 4, 0.0, 1.0, &fs, &mut cache, &[]);
        assert!(c0.pixels.iter().all(|p| *p == white));
        // 스크롤 10: 사각형이 y=0 으로 올라옴
        let c1 = rasterize(&items, 4, 4, 10.0, 1.0, &fs, &mut cache, &[]);
        assert_eq!(c1.pixels[0], red);
        assert_eq!(c1.pixels[2 * 4 + 0], white, "높이 2px 이후는 흰색");
    }

    #[test]
    fn rasterize_scale_doubles_rect() {
        let red = Color { r: 255, g: 0, b: 0, a: 255 };
        let items = vec![DisplayItem::Rect {
            color: red,
            rect: crate::layout::Rect { x: 1.0, y: 0.0, width: 1.0, height: 1.0 },
        }];
        let fs = fonts();
        let mut cache = crate::raster::GlyphCache::new();
        // scale 2: 논리 (1,0,1x1) → 물리 (2,0,2x2)
        let c = rasterize(&items, 6, 4, 0.0, 2.0, &fs, &mut cache, &[]);
        let white = Color { r: 255, g: 255, b: 255, a: 255 };
        assert_eq!(c.pixels[1], white, "x=1 은 스케일된 사각형 왼쪽 밖");
        assert_eq!(c.pixels[2], red, "x=2..4 가 사각형");
        assert_eq!(c.pixels[3], red);
        assert_eq!(c.pixels[6 + 2], red, "두 번째 행도 채워짐 (2x2)");
        assert_eq!(c.pixels[4], white);
    }

    #[test]
    fn display_list_emits_rect_and_glyphs() {
        let root = crate::html::parse_dom("<p>hi</p>".to_string());
        let ss = crate::css::parse(
            "p { display: block; font-size: 20px; background-color: #101010; }".to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: crate::layout::Dimensions = Default::default();
        viewport.content.width = 200.0;
        let fs = fonts();
        let imgs = crate::layout::ImageMap::new();
        let layout_root = crate::layout::layout_tree(&styled, viewport, &fs, &imgs);
        let items = build_display_list(&layout_root);
        let rects = items.iter().filter(|i| matches!(i, DisplayItem::Rect { .. })).count();
        let glyphs = items.iter().filter(|i| matches!(i, DisplayItem::Glyph(_))).count();
        assert!(rects >= 1, "배경 사각형");
        assert_eq!(glyphs, 2, "'hi' 글리프 2개");
    }

    #[test]
    fn background_image_paints_clipped_to_box() {
        let root = crate::html::parse_dom("<div></div>".to_string());
        let ss = crate::css::parse(
            "div { display: block; width: 2px; height: 2px; background-image: url(bg.png); }"
                .to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: crate::layout::Dimensions = Default::default();
        viewport.content.width = 4.0;
        let fs = fonts();
        let mut map = crate::layout::ImageMap::new();
        map.insert("bg.png".to_string(), (0, 3, 1)); // 3x1 이미지, 박스는 2x2
        let img = crate::png::Image { width: 3, height: 1, rgba: vec![255, 0, 0, 255].repeat(3) };
        let layout_root = crate::layout::layout_tree(&styled, viewport, &fs, &map);
        let mut cache = crate::raster::GlyphCache::new();
        let canvas = paint(
            &layout_root,
            crate::layout::Rect { x: 0.0, y: 0.0, width: 4.0, height: 4.0 },
            &fs,
            &mut cache,
            &[img],
        );
        let red = Color { r: 255, g: 0, b: 0, a: 255 };
        let white = Color { r: 255, g: 255, b: 255, a: 255 };
        assert_eq!(canvas.pixels[0], red, "박스 안 (0,0) 은 배경 이미지");
        assert_eq!(canvas.pixels[2], white, "(2,0) 은 박스 폭 2px 로 클리핑되어야 함");
    }

    #[test]
    fn text_paints_colored_pixels() {
        let canvas = canvas_for(
            "<p>Illi</p>",
            "p { display: block; font-size: 40px; color: #ff0000; }",
            200.0,
            80.0,
        );
        assert!(
            canvas.pixels.iter().any(|p| *p == Color { r: 255, g: 0, b: 0, a: 255 }),
            "expected fully-covered red text pixel"
        );
    }
}
