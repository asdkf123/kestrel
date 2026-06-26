use std::collections::HashMap;

use crate::font::{Font, Point};

pub struct CoverageBitmap {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
    pub left: i32,
    pub top: i32,
    pub advance: f32,
}

pub fn rasterize_glyph(font: &Font, glyph_id: u16, px_per_em: f32) -> CoverageBitmap {
    let scale = px_per_em / font.units_per_em() as f32;
    let advance = font.advance_width(glyph_id) as f32 * scale;
    let glyph = font.outline(glyph_id);

    let empty = || CoverageBitmap { width: 0, height: 0, data: vec![], left: 0, top: 0, advance };
    if glyph.contours.is_empty() {
        return empty();
    }

    // 1) 윤곽선을 폰트 단위 폴리라인으로 평탄화
    let polylines: Vec<Vec<(f32, f32)>> =
        glyph.contours.iter().map(|c| flatten_contour(&c.points)).collect();

    // 2) 바운드 계산
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

    let pad = 1.0f32;
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
                    let x0 = xs[w].0.max(0.0);
                    let x1 = xs[w + 1].0.min(width as f32);
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

fn flatten_quad(poly: &mut Vec<(f32, f32)>, p0: (f32, f32), p1: (f32, f32), p2: (f32, f32)) {
    const STEPS: usize = 8;
    for s in 1..=STEPS {
        let t = s as f32 / STEPS as f32;
        let mt = 1.0 - t;
        let x = mt * mt * p0.0 + 2.0 * mt * t * p1.0 + t * t * p2.0;
        let y = mt * mt * p0.1 + 2.0 * mt * t * p1.1 + t * t * p2.1;
        poly.push((x, y));
    }
}

fn flatten_contour(pts: &[Point]) -> Vec<(f32, f32)> {
    let n = pts.len();
    if n == 0 {
        return vec![];
    }
    // 1) 연속 off-curve 사이 암묵 중점(on-curve) 삽입
    let mut exp: Vec<(f32, f32, bool)> = Vec::with_capacity(n * 2);
    for i in 0..n {
        let p = pts[i];
        let q = pts[(i + 1) % n];
        exp.push((p.x, p.y, p.on_curve));
        if !p.on_curve && !q.on_curve {
            exp.push(((p.x + q.x) / 2.0, (p.y + q.y) / 2.0, true));
        }
    }
    // 2) on-curve 점에서 시작하도록 회전
    let start = match exp.iter().position(|e| e.2) {
        Some(s) => s,
        None => return vec![],
    };
    let m = exp.len();
    let mut ring: Vec<(f32, f32, bool)> = (0..m).map(|i| exp[(start + i) % m]).collect();
    ring.push(ring[0]); // 닫기 (마지막은 on-curve 시작점)

    // 3) 폴리라인 생성
    let mut poly = vec![(ring[0].0, ring[0].1)];
    let mut cur = (ring[0].0, ring[0].1);
    let mut i = 1;
    while i < ring.len() {
        let p = ring[i];
        if p.2 {
            poly.push((p.0, p.1));
            cur = (p.0, p.1);
            i += 1;
        } else {
            let end = ring[i + 1]; // off-curve 다음은 항상 on-curve (확장으로 보장)
            flatten_quad(&mut poly, cur, (p.0, p.1), (end.0, end.1));
            cur = (end.0, end.1);
            i += 2;
        }
    }
    poly
}

pub struct GlyphCache {
    map: HashMap<(u16, u32), CoverageBitmap>,
}

impl GlyphCache {
    pub fn new() -> GlyphCache {
        GlyphCache { map: HashMap::new() }
    }

    pub fn get(&mut self, font: &Font, glyph_id: u16, px_per_em: f32) -> &CoverageBitmap {
        let key = (glyph_id, px_per_em.to_bits());
        self.map
            .entry(key)
            .or_insert_with(|| rasterize_glyph(font, glyph_id, px_per_em))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::Font;

    fn load() -> Font {
        Font::from_bytes(std::fs::read("assets/fonts/Kestrel.ttf").unwrap()).unwrap()
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
        let mut cache = GlyphCache::new();
        let (w1, h1) = {
            let bm = cache.get(&f, f.glyph_index('g'), 48.0);
            (bm.width, bm.height)
        };
        let (w2, h2) = {
            let bm = cache.get(&f, f.glyph_index('g'), 48.0);
            (bm.width, bm.height)
        };
        assert_eq!((w1, h1), (w2, h2));
    }
}
