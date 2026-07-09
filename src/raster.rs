use std::collections::HashMap;

use crate::font::{Font, FontStack};

pub struct CoverageBitmap {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
    pub left: i32,
    pub top: i32,
    pub advance: f32,
}

pub fn rasterize_glyph(font: &Font, glyph_id: u16, px_per_em: f32) -> CoverageBitmap {
    rasterize_glyph_styled(font, glyph_id, px_per_em, false, false)
}

// 전용 볼드/이탤릭 폰트가 없을 때의 합성(faux):
//  - italic: 아웃라인을 베이스라인 위쪽일수록 오른쪽으로 전단(shear).
//  - bold: 스캔 스팬을 좌우로 약간 늘려 가로로 두껍게.
pub fn rasterize_glyph_styled(
    font: &Font,
    glyph_id: u16,
    px_per_em: f32,
    bold: bool,
    italic: bool,
) -> CoverageBitmap {
    let scale = px_per_em / font.units_per_em() as f32;
    let advance = font.advance_width(glyph_id) as f32 * scale;
    let slant = if italic { 0.25 } else { 0.0 }; // ≈14° 오블리크
    let polylines: Vec<Vec<(f32, f32)>> = font
        .outline(glyph_id)
        .into_iter()
        .map(|poly| poly.into_iter().map(|(x, y)| (x + slant * y, y)).collect())
        .collect();

    let empty = || CoverageBitmap { width: 0, height: 0, data: vec![], left: 0, top: 0, advance };
    if polylines.is_empty() {
        return empty();
    }
    // 합성 볼드: 디바이스 px 기준 좌우 확장량
    let grow = if bold { 0.03 * px_per_em } else { 0.0 };

    // 바운드 계산
    let (mut minx, mut miny, mut maxx, mut maxy) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for poly in &polylines {
        for &(px, py) in poly {
            minx = minx.min(px);
            miny = miny.min(py);
            maxx = maxx.max(px);
            maxy = maxy.max(py);
        }
    }
    if !(maxx > minx && maxy > miny) {
        return empty();
    }

    let pad = (1.0f32 + grow).ceil(); // 정수값 f32 (offset/width/left 일관)
    let width = ((maxx - minx) * scale).ceil() as usize + 2 * pad as usize;
    let height = ((maxy - miny) * scale).ceil() as usize + 2 * pad as usize;
    if width == 0 || height == 0 {
        return empty();
    }

    // device 변환 (Y 반전)
    let to_dev = |px: f32, py: f32| -> (f32, f32) {
        ((px - minx) * scale + pad, (maxy - py) * scale + pad)
    };

    // 3) 에지 목록 (수평 에지 제외)
    let mut edges: Vec<[f32; 4]> = Vec::new();
    for poly in &polylines {
        for w in poly.windows(2) {
            let (x0, y0) = to_dev(w[0].0, w[0].1);
            let (x1, y1) = to_dev(w[1].0, w[1].1);
            if y0 != y1 {
                edges.push([x0, y0, x1, y1]);
            }
        }
    }

    // 4) 커버리지 스캔라인 (분석적 수평 + 수직 오버샘플)
    const SUB: usize = 5;
    let mut cov = vec![0f32; width * height];
    for row in 0..height {
        for k in 0..SUB {
            let sy = row as f32 + (k as f32 + 0.5) / SUB as f32;
            let mut xs: Vec<(f32, i32)> = Vec::new();
            for e in &edges {
                let (y0, y1) = (e[1], e[3]);
                let (lo, hi) = if y0 < y1 { (y0, y1) } else { (y1, y0) };
                if sy >= lo && sy < hi {
                    let t = (sy - y0) / (y1 - y0);
                    let x = e[0] + t * (e[2] - e[0]);
                    let dir = if y1 > y0 { 1 } else { -1 };
                    xs.push((x, dir));
                }
            }
            if xs.len() < 2 {
                continue;
            }
            xs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            let mut winding = 0;
            for w in 0..xs.len() - 1 {
                winding += xs[w].1;
                if winding != 0 {
                    let x0 = (xs[w].0 - grow).max(0.0);
                    let x1 = (xs[w + 1].0 + grow).min(width as f32);
                    if x1 > x0 {
                        add_span(&mut cov, row * width, width, x0, x1, 1.0 / SUB as f32);
                    }
                }
            }
        }
    }

    let data: Vec<u8> = cov.iter().map(|&v| (v.clamp(0.0, 1.0) * 255.0).round() as u8).collect();
    let left = (minx * scale).round() as i32 - pad as i32;
    let top = (maxy * scale).round() as i32 + pad as i32;

    CoverageBitmap { width, height, data, left, top, advance }
}

fn add_span(cov: &mut [f32], row_off: usize, width: usize, x0: f32, x1: f32, weight: f32) {
    let c0 = x0.floor() as usize;
    let c1 = (x1.ceil() as usize).min(width);
    for c in c0..c1 {
        let cell0 = c as f32;
        let cell1 = c as f32 + 1.0;
        let overlap = (x1.min(cell1) - x0.max(cell0)).max(0.0);
        cov[row_off + c] += overlap * weight;
    }
}

pub struct GlyphCache {
    map: HashMap<(usize, u16, u32, u8), CoverageBitmap>,
}

impl GlyphCache {
    pub fn new() -> GlyphCache {
        GlyphCache { map: HashMap::new() }
    }

    pub fn get(
        &mut self,
        stack: &FontStack,
        font_index: usize,
        glyph_id: u16,
        px_per_em: f32,
        bold: bool,
        italic: bool,
    ) -> &CoverageBitmap {
        let synth = (bold as u8) | ((italic as u8) << 1);
        let key = (font_index, glyph_id, px_per_em.to_bits(), synth);
        self.map.entry(key).or_insert_with(|| {
            rasterize_glyph_styled(stack.font(font_index), glyph_id, px_per_em, bold, italic)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::Font;

    fn load() -> Font {
        Font::from_bytes(std::fs::read("assets/fonts/Latin.ttf").unwrap()).unwrap()
    }

    #[test]
    fn rasterizes_glyph_with_antialiasing() {
        let f = load();
        let bm = rasterize_glyph(&f, f.glyph_index('A'), 64.0);
        assert!(bm.width > 0 && bm.height > 0);
        let ink: u32 = bm.data.iter().map(|&v| v as u32).sum();
        assert!(ink > 0, "glyph should have ink");
        assert!(bm.data.iter().any(|&v| v > 0 && v < 255), "expected AA edge pixels");
        assert!(bm.advance > 0.0);
    }

    #[test]
    fn empty_glyph_has_advance_but_no_pixels() {
        let f = load();
        let bm = rasterize_glyph(&f, f.glyph_index(' '), 64.0);
        assert_eq!(bm.width * bm.height, 0);
        assert!(bm.advance > 0.0);
    }

    #[test]
    fn cache_returns_consistent_bitmap() {
        let f = load();
        let gid = f.glyph_index('g');
        let stack = crate::font::FontStack::new(vec![f]);
        let mut cache = GlyphCache::new();
        let (w1, h1) = {
            let bm = cache.get(&stack, 0, gid, 48.0, false, false);
            (bm.width, bm.height)
        };
        let (w2, h2) = {
            let bm = cache.get(&stack, 0, gid, 48.0, false, false);
            (bm.width, bm.height)
        };
        assert_eq!((w1, h1), (w2, h2));
    }
}
