use crate::css::{Color, Value};
use crate::font::Font;
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

    fn fill_rect(&mut self, color: Color, rect: Rect) {
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

fn paint_box(canvas: &mut Canvas, layout_box: &LayoutBox, font: &Font, cache: &mut GlyphCache) {
    if let Some(color) = get_color(layout_box, "background-color") {
        canvas.fill_rect(color, layout_box.dimensions.border_box());
    }
    for gi in &layout_box.glyphs {
        let bm = cache.get(font, gi.glyph_id, gi.px);
        blit_glyph(canvas, bm, gi);
    }
    for child in &layout_box.children {
        paint_box(canvas, child, font, cache);
    }
}

pub fn paint(layout_root: &LayoutBox, bounds: Rect, font: &Font, cache: &mut GlyphCache) -> Canvas {
    let mut canvas = Canvas::new(bounds.width as usize, bounds.height as usize);
    paint_box(&mut canvas, layout_root, font, cache);
    canvas
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::Color;

    fn font() -> crate::font::Font {
        crate::font::Font::from_bytes(std::fs::read("assets/fonts/Kestrel.ttf").unwrap()).unwrap()
    }

    fn canvas_for(html: &str, css: &str, w: f32, h: f32) -> Canvas {
        let root = crate::html::parse(html.to_string());
        let ss = crate::css::parse(css.to_string());
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: crate::layout::Dimensions = Default::default();
        viewport.content.width = w;
        let f = font();
        let layout_root = crate::layout::layout_tree(&styled, viewport, &f);
        let mut cache = crate::raster::GlyphCache::new();
        paint(&layout_root, crate::layout::Rect { x: 0.0, y: 0.0, width: w, height: h }, &f, &mut cache)
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
