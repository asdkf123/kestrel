use crate::css::{Color, Value};
use crate::font::FontStack;
use crate::layout::{GlyphInstance, LayoutBox, Rect};
use crate::raster::{CoverageBitmap, GlyphCache};

pub struct Canvas {
    pub pixels: Vec<Color>,
    pub width: usize,
    pub height: usize,
    // 활성 클립 마스크 (물리 좌표). 설정되면 모든 픽셀 쓰기가 커버리지로 감쇠된다.
    clip: Option<ClipShape>,
    // 활성 mix-blend-mode. Normal 이면 일반 알파합성.
    blend_mode: BlendMode,
    // 레이어(오프스크린) 모드: 픽셀 알파를 추적해 source-over 로 누적 (그룹 합성용).
    is_layer: bool,
}

// 캔버스 그라디언트 (createLinearGradient/createRadialGradient): 두 점 또는 두 원으로
// 정의된다 (CSS 그라디언트와 좌표 규약이 다르다).
#[derive(Clone, Copy, Debug)]
pub enum CanvasGrad {
    Linear { x0: f32, y0: f32, x1: f32, y1: f32 },
    Radial { x0: f32, y0: f32, r0: f32, x1: f32, y1: f32, r1: f32 },
}

// 픽셀 마스크 도형 (물리 px). 둥근 사각형(사각/원 포함), 타원, 다각형.
#[derive(Debug, Clone)]
pub enum ClipShape {
    RoundRect { rect: Rect, radii: [f32; 4] },
    Ellipse { cx: f32, cy: f32, rx: f32, ry: f32 },
    Polygon(Vec<(f32, f32)>),
}

impl ClipShape {
    // 픽셀 중심 (x,y) 의 클립 커버리지 0..1 (경계 안티에일리어싱; 다각형은 1/0).
    fn coverage(&self, x: f32, y: f32) -> f32 {
        match self {
            ClipShape::RoundRect { rect, radii } => round_rect_coverage(*rect, *radii, x, y),
            ClipShape::Ellipse { cx, cy, rx, ry } => {
                if *rx <= 0.0 || *ry <= 0.0 {
                    return 0.0;
                }
                let nx = (x - cx) / rx;
                let ny = (y - cy) / ry;
                let d = (nx * nx + ny * ny).sqrt();
                ((1.0 - d) * rx.min(*ry) + 0.5).clamp(0.0, 1.0)
            }
            ClipShape::Polygon(pts) => {
                if point_in_polygon(pts, x, y) {
                    1.0
                } else {
                    0.0
                }
            }
        }
    }
}

// 분리형 박스 블러 한 축 1회 (접두합으로 O(n)). horizontal=true 면 가로, false 면 세로.
// 경계는 클램프(윈도우가 영역 안으로 잘림). 3회 반복하면 가우시안 근사.
fn box_pass(
    src: &[(f32, f32, f32)],
    dst: &mut [(f32, f32, f32)],
    rw: usize,
    rh: usize,
    r: usize,
    horizontal: bool,
) {
    let line_len = if horizontal { rw } else { rh };
    let lines = if horizontal { rh } else { rw };
    let mut pr = vec![(0f32, 0f32, 0f32); line_len + 1];
    let idx = |i: usize, j: usize| if horizontal { i * rw + j } else { j * rw + i };
    for i in 0..lines {
        // 접두합
        for j in 0..line_len {
            let c = src[idx(i, j)];
            pr[j + 1] = (pr[j].0 + c.0, pr[j].1 + c.1, pr[j].2 + c.2);
        }
        for j in 0..line_len {
            let lo = j.saturating_sub(r);
            let hi = (j + r).min(line_len - 1);
            let cnt = (hi - lo + 1) as f32;
            let s = (pr[hi + 1].0 - pr[lo].0, pr[hi + 1].1 - pr[lo].1, pr[hi + 1].2 - pr[lo].2);
            dst[idx(i, j)] = (s.0 / cnt, s.1 / cnt, s.2 / cnt);
        }
    }
}

// 오차함수 erf 근사 (Abramowitz & Stegun 7.1.26, |오차|<1.5e-7). 가우시안 섀도 전이용.
fn erf(x: f32) -> f32 {
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    let t = 1.0 / (1.0 + 0.3275911 * x);
    let y = 1.0
        - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t
            + 0.254829592)
            * t
            * (-x * x).exp();
    sign * y
}

// 짝수-홀수 규칙 점-다각형 판정 (ray casting).
fn point_in_polygon(pts: &[(f32, f32)], x: f32, y: f32) -> bool {
    let n = pts.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = pts[i];
        let (xj, yj) = pts[j];
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

// 둥근 사각형의 (fx,fy) 픽셀 커버리지 0..1. 코너별 반경, 경계 AA. 채우기·클립 공용.
fn round_rect_coverage(rect: Rect, radii: [f32; 4], fx: f32, fy: f32) -> f32 {
    let maxr = (rect.width / 2.0).min(rect.height / 2.0).max(0.0);
    let r = [
        radii[0].clamp(0.0, maxr),
        radii[1].clamp(0.0, maxr),
        radii[2].clamp(0.0, maxr),
        radii[3].clamp(0.0, maxr),
    ];
    let (x0, y0) = (rect.x, rect.y);
    let (x1, y1) = (rect.x + rect.width, rect.y + rect.height);
    let clamp01 = |v: f32| v.clamp(0.0, 1.0);
    let corner = |ncx: f32, ncy: f32, rr: f32| {
        clamp01(rr - ((fx - ncx).powi(2) + (fy - ncy).powi(2)).sqrt() + 0.5)
    };
    if fx < x0 + r[0] && fy < y0 + r[0] {
        corner(x0 + r[0], y0 + r[0], r[0])
    } else if fx > x1 - r[1] && fy < y0 + r[1] {
        corner(x1 - r[1], y0 + r[1], r[1])
    } else if fx > x1 - r[2] && fy > y1 - r[2] {
        corner(x1 - r[2], y1 - r[2], r[2])
    } else if fx < x0 + r[3] && fy > y1 - r[3] {
        corner(x0 + r[3], y1 - r[3], r[3])
    } else {
        let cx = clamp01(fx - x0 + 0.5).min(clamp01(x1 - fx + 0.5));
        let cy = clamp01(fy - y0 + 0.5).min(clamp01(y1 - fy + 0.5));
        cx.min(cy)
    }
}

impl Canvas {
    fn new(width: usize, height: usize) -> Canvas {
        let white = Color { r: 255, g: 255, b: 255, a: 255 };
        Canvas { pixels: vec![white; width * height], width, height, clip: None, blend_mode: BlendMode::Normal, is_layer: false }
    }

    // 투명 오프스크린 레이어 (알파 추적). 그룹 opacity/blend 합성용.
    fn new_layer(width: usize, height: usize) -> Canvas {
        Canvas {
            pixels: vec![Color { r: 0, g: 0, b: 0, a: 0 }; width * height],
            width,
            height,
            clip: None,
            blend_mode: BlendMode::Normal,
            is_layer: true,
        }
    }

    // 지역 가우시안 블러 근사. backdrop-filter: blur() 용. 물리 px 반경(≈σ).
    // 박스 블러를 축마다 3회 반복하면 중심극한정리로 가우시안에 수렴(SVG feGaussianBlur
    // 표준 방식). 각 패스는 접두합(prefix sum)으로 O(픽셀).
    fn blur_region(&mut self, rect: Rect, radius: f32) {
        let r = (radius.round() as i32).max(1) as usize;
        let x0 = rect.x.floor().max(0.0) as i32;
        let y0 = rect.y.floor().max(0.0) as i32;
        let x1 = ((rect.x + rect.width).ceil() as i32).min(self.width as i32);
        let y1 = ((rect.y + rect.height).ceil() as i32).min(self.height as i32);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let (x0, y0) = (x0 as usize, y0 as usize);
        let (rw, rh) = ((x1 as usize - x0), (y1 as usize - y0));
        let w = self.width;
        let mut buf = vec![(0f32, 0f32, 0f32); rw * rh];
        for yy in 0..rh {
            for xx in 0..rw {
                let p = self.pixels[(y0 + yy) * w + (x0 + xx)];
                buf[yy * rw + xx] = (p.r as f32, p.g as f32, p.b as f32);
            }
        }
        let mut tmp = vec![(0f32, 0f32, 0f32); rw * rh];
        // 가로 3회, 세로 3회 (핑퐁)
        for _ in 0..3 {
            box_pass(&buf, &mut tmp, rw, rh, r, true);
            std::mem::swap(&mut buf, &mut tmp);
        }
        for _ in 0..3 {
            box_pass(&buf, &mut tmp, rw, rh, r, false);
            std::mem::swap(&mut buf, &mut tmp);
        }
        for yy in 0..rh {
            for xx in 0..rw {
                let c = buf[yy * rw + xx];
                self.pixels[(y0 + yy) * w + (x0 + xx)] = Color {
                    r: c.0.round().clamp(0.0, 255.0) as u8,
                    g: c.1.round().clamp(0.0, 255.0) as u8,
                    b: c.2.round().clamp(0.0, 255.0) as u8,
                    a: 255,
                };
            }
        }
    }

    // 픽셀 쓰기 관문: 활성 클립 커버리지를 알파에 곱해 블렌드. 모든 그리기가 이걸 통한다.
    #[inline]
    fn put(&mut self, px: usize, py: usize, color: Color, alpha: u8) {
        if alpha == 0 || px >= self.width || py >= self.height {
            return;
        }
        let a = match &self.clip {
            Some(shape) => {
                let cov = shape.coverage(px as f32 + 0.5, py as f32 + 0.5);
                if cov <= 0.0 {
                    return;
                }
                ((alpha as f32) * cov).round() as u8
            }
            None => alpha,
        };
        if a == 0 {
            return;
        }
        let idx = py * self.width + px;
        let dst = self.pixels[idx];
        self.pixels[idx] = if self.is_layer {
            // 알파 추적 source-over (레이어 안에선 blend_mode 미적용 — 합성 시 적용)
            let sa = a as f32 / 255.0;
            let da = dst.a as f32 / 255.0;
            let oa = sa + da * (1.0 - sa);
            if oa <= 0.0 {
                return;
            }
            let comp = |s: u8, d: u8| {
                ((s as f32 * sa + d as f32 * da * (1.0 - sa)) / oa).round().clamp(0.0, 255.0) as u8
            };
            Color {
                r: comp(color.r, dst.r),
                g: comp(color.g, dst.g),
                b: comp(color.b, dst.b),
                a: (oa * 255.0).round() as u8,
            }
        } else if self.blend_mode == BlendMode::Normal {
            blend(dst, color, a)
        } else {
            blend_mode_compose(dst, color, a, self.blend_mode)
        };
    }

    pub fn fill_rect(&mut self, color: Color, rect: Rect) {
        // 색의 알파로 블렌드한다. 예전엔 픽셀에 색을 그대로 덮어써서
        // rgba(0,0,0,0)/반투명 배경이 불투명 검정으로 찍혔다(캔버스는 RGB 출력).
        if color.a == 0 {
            return;
        }
        let x0 = rect.x.clamp(0.0, self.width as f32) as usize;
        let y0 = rect.y.clamp(0.0, self.height as f32) as usize;
        let x1 = (rect.x + rect.width).clamp(0.0, self.width as f32) as usize;
        let y1 = (rect.y + rect.height).clamp(0.0, self.height as f32) as usize;
        for y in y0..y1 {
            for x in x0..x1 {
                self.put(x, y, color, color.a);
            }
        }
    }

    // 그라디언트 채우기. linear 는 픽셀을 축(angle: 0deg=위, 90deg=오른쪽)에 투영,
    // radial 은 중심에서의 거리를 farthest-corner 반경으로 정규화해 0..1 위치를 구하고
    // 스톱 사이를 보간한다. linear 선 길이 = |w*sin| + |h*cos| (모서리가 0/1 에 대응).
    // CSS 그라디언트. 스톱 위치(px/%/auto)는 여기서 **그라디언트 선 길이**로 푼다 —
    // px 위치는 박스 크기를 알아야 정규화된다 (파싱 시점엔 알 수 없다).
    // repeating-* 은 스톱 구간을 반복한다.
    #[allow(clippy::too_many_arguments)]
    pub fn fill_gradient(
        &mut self,
        rect: Rect,
        angle_deg: f32,
        radial: bool,
        circle: bool,
        conic: bool,
        repeating: bool,
        stops: &[(Color, crate::css::StopPos)],
        scale: f32,
    ) {
        if rect.width <= 0.0 || rect.height <= 0.0 || stops.is_empty() {
            return;
        }
        let (cx, cy) = (rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
        let a = angle_deg.to_radians();
        let (dx, dy) = (a.sin(), -a.cos());
        let len = ((rect.width * dx).abs() + (rect.height * dy).abs()).max(1.0);
        // radial farthest-corner: circle 는 중심~가장 먼 모서리 거리(단일 반경),
        // ellipse(기본)는 축별 반경 rx=(w/2)√2, ry=(h/2)√2 (모서리가 p=1 에 대응).
        let radius = ((rect.width / 2.0).powi(2) + (rect.height / 2.0).powi(2)).sqrt().max(1.0);
        let (rx, ry) = (
            (rect.width / 2.0 * std::f32::consts::SQRT_2).max(1.0),
            (rect.height / 2.0 * std::f32::consts::SQRT_2).max(1.0),
        );
        // 그라디언트 선의 길이 (px 스톱을 0..1 로 옮길 기준). 물리 px 가 아니라 CSS px 다.
        let line_len = if conic {
            360.0 // conic 의 px 위치는 의미 없다 (deg/% 만)
        } else if radial {
            (if circle { radius } else { rx.max(ry) }) / scale
        } else {
            len / scale
        };
        let resolved = resolve_stops(stops, line_len);
        if resolved.is_empty() {
            return;
        }
        // 반복 구간 [p0, p1]
        let p0 = resolved[0].1;
        let p1 = resolved[resolved.len() - 1].1;
        let period = p1 - p0;

        let x0 = rect.x.clamp(0.0, self.width as f32) as usize;
        let y0 = rect.y.clamp(0.0, self.height as f32) as usize;
        let x1 = (rect.x + rect.width).clamp(0.0, self.width as f32) as usize;
        let y1 = (rect.y + rect.height).clamp(0.0, self.height as f32) as usize;
        for y in y0..y1 {
            let fy = y as f32 + 0.5;
            for x in x0..x1 {
                let fx = x as f32 + 0.5;
                let mut p = if conic {
                    // 중심 기준 각도(위쪽 0, 시계방향) 0..1
                    let ang = (fx - cx).atan2(-(fy - cy)); // -π..π, 위쪽=0
                    let norm = if ang < 0.0 { ang + std::f32::consts::TAU } else { ang };
                    norm / std::f32::consts::TAU
                } else if radial {
                    if circle {
                        ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt() / radius
                    } else {
                        (((fx - cx) / rx).powi(2) + ((fy - cy) / ry).powi(2)).sqrt()
                    }
                } else {
                    let t = (fx - cx) * dx + (fy - cy) * dy;
                    (t + len / 2.0) / len
                };
                if repeating && period > 1e-6 {
                    // 구간을 반복 (표준: 스톱 구간 밖은 주기적으로 되풀이)
                    p = p0 + (p - p0).rem_euclid(period);
                } else {
                    p = p.clamp(0.0, 1.0);
                }
                let color = gradient_color_at(&resolved, p);
                if color.a == 0 {
                    continue;
                }
                self.put(x, y, color, color.a);
            }
        }
    }

    // 네 모서리 반경(물리 px): [top-left, top-right, bottom-right, bottom-left].
    pub fn fill_round_rect4(&mut self, color: Color, rect: Rect, radii: [f32; 4]) {
        if rect.width <= 0.0 || rect.height <= 0.0 {
            return;
        }
        let px0 = rect.x.floor().max(0.0) as usize;
        let py0 = rect.y.floor().max(0.0) as usize;
        let px1 = ((rect.x + rect.width).ceil().max(0.0) as usize).min(self.width);
        let py1 = ((rect.y + rect.height).ceil().max(0.0) as usize).min(self.height);
        for py in py0..py1 {
            for px in px0..px1 {
                let cov = round_rect_coverage(rect, radii, px as f32 + 0.5, py as f32 + 0.5);
                if cov <= 0.0 {
                    continue;
                }
                // 색의 알파를 엣지 커버리지와 곱한다 (반투명 오버레이 정확도).
                let a = (cov * (color.a as f32 / 255.0) * 255.0).round() as u8;
                self.put(px, py, color, a);
            }
        }
    }

    // 둥근 테두리 링(annulus): outer 안이면서 inner 밖인 영역을 칠한다. 투명 배경
    // 요소의 border-radius 테두리(고스트/아웃라인 버튼)를 각지지 않게 정확히 그린다.
    pub fn fill_round_rect_ring(
        &mut self,
        color: Color,
        outer: Rect,
        outer_radii: [f32; 4],
        inner: Rect,
        inner_radii: [f32; 4],
    ) {
        if outer.width <= 0.0 || outer.height <= 0.0 {
            return;
        }
        let px0 = outer.x.floor().max(0.0) as usize;
        let py0 = outer.y.floor().max(0.0) as usize;
        let px1 = ((outer.x + outer.width).ceil().max(0.0) as usize).min(self.width);
        let py1 = ((outer.y + outer.height).ceil().max(0.0) as usize).min(self.height);
        for py in py0..py1 {
            for px in px0..px1 {
                let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
                let oc = round_rect_coverage(outer, outer_radii, fx, fy);
                if oc <= 0.0 {
                    continue;
                }
                let ic = round_rect_coverage(inner, inner_radii, fx, fy);
                let cov = (oc * (1.0 - ic)).max(0.0);
                if cov <= 0.0 {
                    continue;
                }
                let a = (cov * (color.a as f32 / 255.0) * 255.0).round() as u8;
                self.put(px, py, color, a);
            }
        }
    }

    // 부드러운 둥근 사각형(드롭 섀도). 둥근 박스 SDF 를 가우시안으로 흐린 근사 —
    // 직선 경계에서 커버리지는 erf 전이(가우시안 적분). CSS blur 반경 ≈ 2σ 이므로
    // σ=blur/2. color 의 알파와 곱해 반투명 그림자.
    pub fn fill_soft_round_rect(&mut self, color: Color, rect: Rect, radius: f32, blur: f32) {
        if rect.width <= 0.0 || rect.height <= 0.0 {
            return;
        }
        let sigma = (blur * 0.5).max(0.5);
        // 가우시안 꼬리(~3σ)까지 칠하도록 여유를 둔다.
        let ext = (blur * 1.5).max(0.75);
        let (hw, hh) = (rect.width / 2.0, rect.height / 2.0);
        let (ccx, ccy) = (rect.x + hw, rect.y + hh);
        let r = radius.min(hw).min(hh).max(0.0);
        let x0 = (rect.x - ext).floor().max(0.0) as usize;
        let y0 = (rect.y - ext).floor().max(0.0) as usize;
        let x1 = ((rect.x + rect.width + ext).ceil().max(0.0) as usize).min(self.width);
        let y1 = ((rect.y + rect.height + ext).ceil().max(0.0) as usize).min(self.height);
        let base_a = color.a as f32 / 255.0;
        let denom = sigma * std::f32::consts::SQRT_2;
        for py in y0..y1 {
            let fy = py as f32 + 0.5;
            for px in x0..x1 {
                let fx = px as f32 + 0.5;
                // 둥근 박스 SDF (내부 음수, 외부 양수)
                let qx = (fx - ccx).abs() - (hw - r);
                let qy = (fy - ccy).abs() - (hh - r);
                let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
                let sdf = outside + qx.max(qy).min(0.0) - r;
                // 가우시안 경계 전이: 안쪽(sdf<0)=1, 바깥=0, 폭은 σ 로 결정.
                let cov = 0.5 * (1.0 - erf(sdf / denom));
                if cov <= 0.0 {
                    continue;
                }
                let a = (cov * base_a * 255.0).round() as u8;
                if a == 0 {
                    continue;
                }
                self.put(px, py, color, a);
            }
        }
    }

    // 안쪽 그림자: 박스 내부에서 경계(오프셋 반영)로부터 blur 만큼 안으로 감쇠.
    // 경계 근처가 가장 진하고 중심으로 갈수록 옅어진다. rect(=border box)로 클립.
    // 안쪽 그림자. spread 는 그림자의 안쪽 모서리를 **박스 안으로** 밀어 넣는다
    // (box-shadow: inset 0 0 0 10px c 는 두께 10px 의 안쪽 링이 된다).
    #[allow(clippy::too_many_arguments)]
    pub fn fill_inner_shadow(
        &mut self,
        color: Color,
        rect: Rect,
        radius: f32,
        blur: f32,
        spread: f32,
        dx: f32,
        dy: f32,
    ) {
        if rect.width <= 0.0 || rect.height <= 0.0 {
            return;
        }
        // blur 가 0 이고 spread 만 있으면 딱딱한 링이어야 한다 (soft 를 1 로 두면 1px 만 나온다)
        let soft = if blur > 0.0 { blur } else { 0.5 };
        let (hw, hh) = (rect.width / 2.0, rect.height / 2.0);
        let (ccx, ccy) = (rect.x + hw, rect.y + hh);
        let r = radius.min(hw).min(hh).max(0.0);
        let base_a = color.a as f32 / 255.0;
        let x0 = rect.x.max(0.0) as usize;
        let y0 = rect.y.max(0.0) as usize;
        let x1 = ((rect.x + rect.width).min(self.width as f32)).max(0.0) as usize;
        let y1 = ((rect.y + rect.height).min(self.height as f32)).max(0.0) as usize;
        for py in y0..y1 {
            let fy = py as f32 + 0.5;
            for px in x0..x1 {
                let fx = px as f32 + 0.5;
                // 오프셋 반영 샘플점의 둥근 박스 SDF (내부 음수). 오프셋 반대편 경계에서 진함.
                let sx = fx - dx;
                let sy = fy - dy;
                let qx = (sx - ccx).abs() - (hw - r);
                let qy = (sy - ccy).abs() - (hh - r);
                let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
                // spread 만큼 그림자의 안쪽 경계를 안으로 민다 (sdf 를 spread 만큼 더한다)
                let sdf = outside + qx.max(qy).min(0.0) - r + spread;
                // 경계(sdf~0)에서 1, 안으로 soft 만큼 들어가면(sdf=-soft) 0
                let cov = (1.0 + sdf / soft).clamp(0.0, 1.0);
                if cov <= 0.0 {
                    continue;
                }
                let a = (cov * base_a * 255.0).round() as u8;
                if a == 0 {
                    continue;
                }
                self.put(px, py, color, a);
            }
        }
    }

    // 폴리곤 채우기 (nonzero winding). contours 는 물리 좌표. 홀(안쪽 윤곽) 지원.
    // 안티에일리어싱: 한 픽셀 행을 S개 서브스캔라인으로 나눠 세로 AA, 각 스팬의
    // 좌우 끝은 부분 픽셀 커버리지로 가로 AA. 픽셀당 커버리지를 누적해 알파로 칠한다.
    pub fn fill_polygon(&mut self, color: Color, contours: &[Vec<(f32, f32)>]) {
        if color.a == 0 {
            return;
        }
        let (mut ymin, mut ymax) = (f32::INFINITY, f32::NEG_INFINITY);
        let (mut xmin, mut xmax) = (f32::INFINITY, f32::NEG_INFINITY);
        for c in contours {
            for &(x, y) in c {
                ymin = ymin.min(y);
                ymax = ymax.max(y);
                xmin = xmin.min(x);
                xmax = xmax.max(x);
            }
        }
        if !ymin.is_finite() || !xmin.is_finite() {
            return;
        }
        let y0 = ymin.floor().max(0.0) as usize;
        let y1 = (ymax.ceil().max(0.0) as usize).min(self.height);
        let x0 = xmin.floor().max(0.0) as usize;
        let x1 = (xmax.ceil().max(0.0) as usize).min(self.width);
        if x1 <= x0 {
            return;
        }
        const S: usize = 4; // 픽셀당 세로 서브샘플 수
        let span_w = x1 - x0;
        let mut cov = vec![0f32; span_w];
        let mut xs: Vec<(f32, i32)> = Vec::new();
        for py in y0..y1 {
            for c in cov.iter_mut() {
                *c = 0.0;
            }
            for s in 0..S {
                let yc = py as f32 + (s as f32 + 0.5) / S as f32;
                xs.clear();
                for c in contours {
                    let m = c.len();
                    for k in 0..m {
                        let (ax, ay) = c[k];
                        let (bx, by) = c[(k + 1) % m];
                        if (ay <= yc && by > yc) || (by <= yc && ay > yc) {
                            let t = (yc - ay) / (by - ay);
                            let x = ax + t * (bx - ax);
                            xs.push((x, if by > ay { 1 } else { -1 }));
                        }
                    }
                }
                if xs.len() < 2 {
                    continue;
                }
                xs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
                let mut wind = 0;
                for w in xs.windows(2) {
                    wind += w[0].1;
                    if wind == 0 {
                        continue;
                    }
                    let xa = w[0].0.max(x0 as f32);
                    let xb = w[1].0.min(x1 as f32);
                    if xb <= xa {
                        continue;
                    }
                    let ia = xa.floor() as usize; // >= x0
                    let ib = (xb.ceil() as usize).min(x1);
                    for px in ia..ib {
                        // 픽셀 [px, px+1] 과 스팬 [xa, xb] 겹침 길이 = 가로 커버리지.
                        let l = (px as f32).max(xa);
                        let r = ((px + 1) as f32).min(xb);
                        let c = (r - l).clamp(0.0, 1.0);
                        cov[px - x0] += c / S as f32;
                    }
                }
            }
            for (i, &cv) in cov.iter().enumerate() {
                if cv <= 0.0 {
                    continue;
                }
                let a = (cv.min(1.0) * color.a as f32).round() as u8;
                if a == 0 {
                    continue;
                }
                self.put(x0 + i, py, color, a);
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

// mix-blend-mode 블렌드 모드.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BlendMode {
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    Difference,
    Exclusion,
    HardLight,
}

fn parse_blend_mode(s: &str) -> Option<BlendMode> {
    Some(match s.trim() {
        "multiply" => BlendMode::Multiply,
        "screen" => BlendMode::Screen,
        "overlay" => BlendMode::Overlay,
        "darken" => BlendMode::Darken,
        "lighten" => BlendMode::Lighten,
        "difference" => BlendMode::Difference,
        "exclusion" => BlendMode::Exclusion,
        "hard-light" => BlendMode::HardLight,
        _ => return None, // normal 등은 일반 알파합성
    })
}

// 한 채널(0..1) 블렌드. B(backdrop, source).
fn blend_channel(mode: BlendMode, b: f32, f: f32) -> f32 {
    match mode {
        BlendMode::Normal => f,
        BlendMode::Multiply => b * f,
        BlendMode::Screen => 1.0 - (1.0 - b) * (1.0 - f),
        BlendMode::Darken => b.min(f),
        BlendMode::Lighten => b.max(f),
        BlendMode::Difference => (b - f).abs(),
        BlendMode::Exclusion => b + f - 2.0 * b * f,
        BlendMode::Overlay => {
            if b < 0.5 {
                2.0 * b * f
            } else {
                1.0 - 2.0 * (1.0 - b) * (1.0 - f)
            }
        }
        BlendMode::HardLight => {
            if f < 0.5 {
                2.0 * b * f
            } else {
                1.0 - 2.0 * (1.0 - b) * (1.0 - f)
            }
        }
    }
}

// mix-blend-mode 로 합성: 결과 = backdrop*(1-a) + B(backdrop,src)*a
fn blend_mode_compose(bg: Color, fg: Color, a: u8, mode: BlendMode) -> Color {
    let af = a as f32 / 255.0;
    let ch = |d: u8, s: u8| {
        let (df, sf) = (d as f32 / 255.0, s as f32 / 255.0);
        let bl = blend_channel(mode, df, sf).clamp(0.0, 1.0);
        ((df * (1.0 - af) + bl * af) * 255.0).round() as u8
    };
    Color { r: ch(bg.r, fg.r), g: ch(bg.g, fg.g), b: ch(bg.b, fg.b), a: 255 }
}

// 위치 p(0..1)에서 그라디언트 색을 선형 보간. stops 는 위치 오름차순 가정.
// 스톱 위치를 0..1 로 확정한다. px 는 그라디언트 선 길이 기준, auto 는 이웃 사이 균등 보간,
// 그리고 위치는 단조 증가해야 한다 (앞 스톱보다 작으면 앞 값으로 끌어올린다 — 표준).
pub fn resolve_stops(
    stops: &[(Color, crate::css::StopPos)],
    line_len: f32,
) -> Vec<(Color, f32)> {
    use crate::css::StopPos;
    let n = stops.len();
    let mut pos: Vec<Option<f32>> = stops
        .iter()
        .map(|(_, sp)| match sp {
            StopPos::Auto => None,
            StopPos::Pct(v) => Some(*v),
            StopPos::Px(v) => Some(if line_len > 0.0 { v / line_len } else { 0.0 }),
            StopPos::Deg(d) => Some(d / 360.0),
        })
        .collect();
    // 양 끝은 위치가 없으면 0 과 1
    if pos[0].is_none() {
        pos[0] = Some(0.0);
    }
    if pos[n - 1].is_none() {
        pos[n - 1] = Some(1.0);
    }
    // 사이의 빈 위치는 알려진 두 이웃 사이로 균등 분배
    let mut i = 0usize;
    while i < n {
        if pos[i].is_some() {
            i += 1;
            continue;
        }
        let start = i - 1;
        let mut end = i;
        while end < n && pos[end].is_none() {
            end += 1;
        }
        let (a, b) = (pos[start].unwrap(), pos[end].unwrap());
        let gap = end - start;
        for (k, slot) in pos.iter_mut().enumerate().take(end).skip(i) {
            *slot = Some(a + (b - a) * ((k - start) as f32 / gap as f32));
        }
        i = end;
    }
    // 단조 증가 보정
    let mut out: Vec<(Color, f32)> = Vec::with_capacity(n);
    let mut last = f32::NEG_INFINITY;
    for (k, (c, _)) in stops.iter().enumerate() {
        let mut p = pos[k].unwrap();
        if p < last {
            p = last;
        }
        last = p;
        out.push((*c, p));
    }
    out
}

fn gradient_color_at(stops: &[(Color, f32)], p: f32) -> Color {
    if stops.is_empty() {
        return Color { r: 0, g: 0, b: 0, a: 0 };
    }
    if p <= stops[0].1 {
        return stops[0].0;
    }
    if p >= stops[stops.len() - 1].1 {
        return stops[stops.len() - 1].0;
    }
    for w in stops.windows(2) {
        let (c0, p0) = w[0];
        let (c1, p1) = w[1];
        if p >= p0 && p <= p1 {
            let f = if p1 > p0 { (p - p0) / (p1 - p0) } else { 0.0 };
            // CSS: 그라디언트는 premultiplied alpha 로 보간 (투명 페이드가 탁해지지 않게).
            let (a0, a1) = (c0.a as f32 / 255.0, c1.a as f32 / 255.0);
            let a = a0 + (a1 - a0) * f;
            let chan = |ch0: u8, ch1: u8| -> u8 {
                let pm = ch0 as f32 * a0 + (ch1 as f32 * a1 - ch0 as f32 * a0) * f; // premultiplied lerp
                if a > 0.0 {
                    (pm / a).round().clamp(0.0, 255.0) as u8
                } else {
                    0
                }
            };
            return Color {
                r: chan(c0.r, c1.r),
                g: chan(c0.g, c1.g),
                b: chan(c0.b, c1.b),
                a: (a * 255.0).round() as u8,
            };
        }
    }
    stops[stops.len() - 1].0
}

fn blit_glyph(canvas: &mut Canvas, bm: &CoverageBitmap, gi: &GlyphInstance) {
    if gi.rot != 0.0 {
        blit_glyph_rotated(canvas, bm, gi);
        return;
    }
    let ox = (gi.x + bm.left as f32).round() as i32;
    let oy = (gi.baseline_y - bm.top as f32).round() as i32;
    for y in 0..bm.height {
        let cy = oy + y as i32;
        if cy < 0 || cy as usize >= canvas.height {
            continue;
        }
        for x in 0..bm.width {
            let cov = bm.data[y * bm.width + x];
            if cov == 0 {
                continue;
            }
            let cx = ox + x as i32;
            if cx < 0 || cx as usize >= canvas.width {
                continue;
            }
            // 커버리지(안티에일리어싱)와 글리프 색 알파(opacity 반영)를 결합
            let a = (cov as u32 * gi.color.a as u32 / 255) as u8;
            if a == 0 {
                continue;
            }
            canvas.put(cx as usize, cy as usize, gi.color, a);
        }
    }
}

// 회전된 글리프: 원점(pen 위치) 기준 rot 만큼 회전. 목적지 회전 bbox 를 훑으며
// 역회전으로 커버리지 비트맵을 샘플(최근접). 비트맵 원점은 (gi.x+left, gi.baseline_y-top).
fn blit_glyph_rotated(canvas: &mut Canvas, bm: &CoverageBitmap, gi: &GlyphInstance) {
    if bm.width == 0 || bm.height == 0 {
        return;
    }
    let (ox, oy) = (gi.x, gi.baseline_y); // 회전 중심 = pen 위치
    let (bx, by) = (gi.x + bm.left as f32, gi.baseline_y - bm.top as f32); // 비트맵 좌상단(비회전)
    let (c, s) = (gi.rot.cos(), gi.rot.sin());
    // 비트맵 4모서리를 회전해 목적지 bbox 계산
    let corners = [
        (bx, by),
        (bx + bm.width as f32, by),
        (bx, by + bm.height as f32),
        (bx + bm.width as f32, by + bm.height as f32),
    ];
    let (mut x0, mut y0, mut x1, mut y1) = (f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
    for &(px, py) in &corners {
        let rx = ox + (px - ox) * c - (py - oy) * s;
        let ry = oy + (px - ox) * s + (py - oy) * c;
        x0 = x0.min(rx);
        y0 = y0.min(ry);
        x1 = x1.max(rx);
        y1 = y1.max(ry);
    }
    let dx0 = x0.floor().max(0.0) as usize;
    let dy0 = y0.floor().max(0.0) as usize;
    let dx1 = (x1.ceil().max(0.0) as usize).min(canvas.width);
    let dy1 = (y1.ceil().max(0.0) as usize).min(canvas.height);
    for py in dy0..dy1 {
        for px in dx0..dx1 {
            // 목적지 → 원점 기준 역회전 → 비트맵 좌표
            let (fx, fy) = (px as f32 + 0.5 - ox, py as f32 + 0.5 - oy);
            let ux = ox + fx * c + fy * s;
            let uy = oy - fx * s + fy * c;
            let sx = (ux - bx).floor() as i32;
            let sy = (uy - by).floor() as i32;
            if sx < 0 || sy < 0 || sx as usize >= bm.width || sy as usize >= bm.height {
                continue;
            }
            let cov = bm.data[sy as usize * bm.width + sx as usize];
            if cov == 0 {
                continue;
            }
            let a = (cov as u32 * gi.color.a as u32 / 255) as u8;
            if a == 0 {
                continue;
            }
            canvas.put(px, py, gi.color, a);
        }
    }
}

// 디스플레이 리스트: 레이아웃 트리에서 뽑아낸 소유(owned) 그리기 명령 목록.
// 이미지를 박스에 맞추는 방식. Natural 은 배경용(좌상단 고유크기, 클립).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ImageFit {
    Fill,    // 박스에 늘려 채움 (object-fit 기본)
    Contain, // 종횡비 유지, 박스 안에 들어가게 (레터박스)
    Cover,   // 종횡비 유지, 박스를 덮게 (넘치는 부분 크롭)
    None,    // 고유 크기, 중앙, 클립
    Natural, // 배경 이미지 no-repeat: 좌상단 고유 크기, 클립
    Tile,    // background-repeat: repeat — 양축 타일
    TileX,   // repeat-x — 가로만 타일
    TileY,   // repeat-y — 세로만 타일
    // background-size 로 타일 크기가 지정된 경우 (repeat 여부는 background-repeat).
    Sized { w: BgSize, h: BgSize, tile_x: bool, tile_y: bool },
}

// background-size 한 축. Pct 는 0..1 (배경 배치 영역 기준). Auto 는 다른 축에서 종횡비 유지.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BgSize {
    Auto,
    Px(f32),
    Pct(f32),
}

// "50% 25%" / "20px" / "100% auto" / "auto" → (가로, 세로). cover/contain 은 여기서 다루지 않는다.
pub fn parse_bg_size(text: &str) -> Option<(BgSize, BgSize)> {
    let one = |t: &str| -> Option<BgSize> {
        let t = t.trim();
        if t.eq_ignore_ascii_case("auto") {
            return Some(BgSize::Auto);
        }
        if let Some(p) = t.strip_suffix('%') {
            return p.trim().parse::<f32>().ok().map(|v| BgSize::Pct(v / 100.0));
        }
        crate::css::parse_len_px(t).map(BgSize::Px)
    };
    let toks: Vec<&str> = text.split_whitespace().collect();
    match toks.len() {
        1 => one(toks[0]).map(|w| (w, BgSize::Auto)), // 한 값이면 세로는 auto (종횡비 유지)
        2 => Some((one(toks[0])?, one(toks[1])?)),
        _ => None,
    }
}

// background-position 한 축 값. Pct 는 0..1 (박스-이미지 정렬 기준).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BgCoord {
    Px(f32),
    Pct(f32),
}

impl BgCoord {
    // 축 방향 오프셋 px. box_dim/img_dim 은 해당 축 길이.
    fn resolve(self, box_dim: f32, img_dim: f32) -> f32 {
        match self {
            BgCoord::Px(v) => v,
            BgCoord::Pct(p) => (box_dim - img_dim) * p,
        }
    }
}

// "center" / "right top" / "50% 50%" / "10px 20px" → (x, y) 축 좌표.
fn parse_bg_position(s: &str) -> (BgCoord, BgCoord) {
    let toks: Vec<&str> = s.split_whitespace().collect();
    let coord = |t: &str, horizontal: bool| -> Option<BgCoord> {
        match t {
            "left" if horizontal => Some(BgCoord::Pct(0.0)),
            "right" if horizontal => Some(BgCoord::Pct(1.0)),
            "top" if !horizontal => Some(BgCoord::Pct(0.0)),
            "bottom" if !horizontal => Some(BgCoord::Pct(1.0)),
            "center" => Some(BgCoord::Pct(0.5)),
            _ => {
                if let Some(p) = t.strip_suffix('%') {
                    p.trim().parse::<f32>().ok().map(|v| BgCoord::Pct(v / 100.0))
                } else {
                    t.trim_end_matches("px").parse::<f32>().ok().map(BgCoord::Px)
                }
            }
        }
    };
    match toks.as_slice() {
        [a] => {
            // 한 값: 나머지 축은 center. 세로 키워드면 x=center 로.
            if *a == "top" || *a == "bottom" {
                (BgCoord::Pct(0.5), coord(a, false).unwrap_or(BgCoord::Pct(0.0)))
            } else {
                (coord(a, true).unwrap_or(BgCoord::Pct(0.5)), BgCoord::Pct(0.5))
            }
        }
        [a, b, ..] => {
            // "top left" 처럼 순서가 바뀐 키워드도 허용
            let (ax, ay) = if matches!(*a, "top" | "bottom") || matches!(*b, "left" | "right") {
                (b, a)
            } else {
                (a, b)
            };
            (
                coord(ax, true).unwrap_or(BgCoord::Pct(0.5)),
                coord(ay, false).unwrap_or(BgCoord::Pct(0.5)),
            )
        }
        [] => (BgCoord::Pct(0.0), BgCoord::Pct(0.0)),
    }
}

// 트리 borrow 없이 스크롤 오프셋만 바꿔 반복 래스터화할 수 있다 (실제 브라우저 구조).
// 이미지에 걸린 색/알파 보정. 이미지는 색 하나가 아니라 픽셀 배열이라, 다른 아이템처럼
// "칠할 색을 미리 바꾸는" 방식이 통하지 않는다 — blit 할 때 픽셀마다 적용해야 한다.
// (예전엔 filter/opacity 함수가 이미지에서 조용히 무시됐다: filter: grayscale(1) 인
//  로고가 컬러로 나왔다.)
#[derive(Clone, Debug, PartialEq)]
pub struct ImgAdj {
    pub alpha: f32,
    pub filters: Vec<(String, f32)>,
}

impl ImgAdj {
    pub fn none() -> ImgAdj {
        ImgAdj { alpha: 1.0, filters: Vec::new() }
    }
    // 픽셀 색/알파에 적용
    pub fn apply(&self, c: Color, a: u8) -> (Color, u8) {
        let c = if self.filters.is_empty() { c } else { apply_filters(c, &self.filters) };
        let a = if self.alpha >= 1.0 {
            a
        } else {
            (a as f32 * self.alpha).round().clamp(0.0, 255.0) as u8
        };
        (c, a)
    }
}

#[derive(Debug, Clone)]
pub enum DisplayItem {
    Rect { color: Color, rect: Rect },
    RoundRect { color: Color, rect: Rect, radii: [f32; 4] },
    // 둥근 테두리 링: outer 안 && inner 밖 (투명 배경 border-radius 테두리)
    RoundRectRing { color: Color, outer: Rect, outer_radii: [f32; 4], inner: Rect, inner_radii: [f32; 4] },
    Shadow { color: Color, rect: Rect, radius: f32, blur: f32 },
    // 안쪽 그림자 (box-shadow inset). dx/dy 는 오프셋, rect 는 border box.
    // mask-image: 서브트리를 그린 뒤 마스크의 **알파**로 곱한다 (CSS Masking §3).
    // 예전엔 통째로 무시했다 — 페이드아웃 마스크나 아이콘 마스크가 전혀 안 먹었다.
    Masked {
        rect: Rect,
        gradient: Option<crate::css::Gradient>,
        image: Option<usize>,
        items: Vec<DisplayItem>,
    },
    // filter: drop-shadow(dx dy blur color) — 서브트리의 **실루엣**(알파)을 밀고 흐려서
    // 뒤에 깔고, 그 위에 원본을 그린다. box-shadow 와 달리 박스가 아니라 그려진 모양을 따른다.
    // 예전엔 통째로 무시됐다 (그림자가 아예 안 나왔다).
    DropShadow {
        dx: f32,
        dy: f32,
        blur: f32,
        color: Color,
        items: Vec<DisplayItem>,
    },
    InnerShadow {
        color: Color,
        rect: Rect,
        radius: f32,
        blur: f32,
        spread: f32,
        dx: f32,
        dy: f32,
    },
    Image { image: usize, rect: Rect, fit: ImageFit, pos: Option<(BgCoord, BgCoord)>, adj: ImgAdj },
    // 인라인 픽셀 이미지 (canvas putImageData). 이미지 배열에 없는 즉석 픽셀.
    RawImage { rect: Rect, img: std::rc::Rc<crate::png::Image>, adj: ImgAdj },
    // 그라디언트 배경. angle: CSS 각도(linear), radial: 방사 여부, circle: 원/타원, stops: (색, 위치 0-1).
    Gradient {
        rect: Rect,
        angle: f32,
        radial: bool,
        circle: bool,
        conic: bool,
        repeating: bool,
        // 스톱 위치는 px/%/auto 그대로 — 그라디언트 선 길이를 알아야 풀린다(페인트 시점).
        stops: Vec<(Color, crate::css::StopPos)>,
    },
    // SVG path 채우기 (여러 윤곽선, nonzero winding). points 는 논리 좌표.
    Polygon { color: Color, contours: Vec<Vec<(f32, f32)>> },
    // 캔버스 그라디언트 (createLinearGradient/createRadialGradient). CSS 그라디언트와 달리
    // 두 점(또는 두 원)으로 정의되므로 별도 항목이다. 모양은 Clipped 로 자른다.
    CanvasGradient { rect: Rect, kind: CanvasGrad, stops: Vec<(Color, f32)> },
    Glyph(GlyphInstance),
    // position: sticky — 스크롤 시 뷰포트 상단 top 만큼 아래에 고정. top=스티키 임계,
    // y0=요소의 자연 문서 y. 렌더 시 inner 를 보정된 스크롤로 그린다.
    Sticky { top: f32, y0: f32, inner: Box<DisplayItem> },
    // 픽셀 마스크 클립 (둥근 overflow / clip-path circle·ellipse). inner 를 shape 로 마스킹.
    Clipped { shape: ClipShape, inner: Box<DisplayItem> },
    // backdrop-filter: blur() — 뒤 배경(이미 그려진 캔버스)을 rect 영역에서 흐린다.
    BackdropBlur { rect: Rect, radius: f32 },
    // 오프스크린 레이어: 서브트리를 격리 합성한 뒤 opacity + blend 로 한 번에 얹는다
    // (그룹 opacity/mix-blend 정확 — 겹치는 자손이 이중 블렌드되지 않음).
    Layer { opacity: f32, blend: BlendMode, items: Vec<DisplayItem> },
    // CSS transform: 서브트리를 격리 렌더한 뒤 2D 행렬로 매핑한다.
    // 회전/기울임은 사각형이 사각형으로 남지 않으므로 개별 프리미티브를 밀고 늘리는
    // 방식으로는 표현할 수 없다 — 글자·이미지·그림자·그라디언트가 **함께** 돌아야 한다.
    // (예전엔 translate/scale 만 좌표를 밀고 rotate/skew/matrix 는 조용히 무시했다)
    Transform { m: crate::layout::Mat3, items: Vec<DisplayItem> },
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
        emit_border_side(items, side_color("top"),
            Rect { x: b.x, y: b.y, width: b.width, height: bw.top },
            &border_side_style(lb, "top"), true);
    }
    if bw.bottom > 0.0 && drawn("bottom") {
        emit_border_side(items, side_color("bottom"),
            Rect { x: b.x, y: b.y + b.height - bw.bottom, width: b.width, height: bw.bottom },
            &border_side_style(lb, "bottom"), true);
    }
    if bw.left > 0.0 && drawn("left") {
        emit_border_side(items, side_color("left"),
            Rect { x: b.x, y: b.y, width: bw.left, height: b.height },
            &border_side_style(lb, "left"), false);
    }
    if bw.right > 0.0 && drawn("right") {
        emit_border_side(items, side_color("right"),
            Rect { x: b.x + b.width - bw.right, y: b.y, width: bw.right, height: b.height },
            &border_side_style(lb, "right"), false);
    }
}

// 균일 border-radius (논리 px). 퍼센트는 박스 짧은 변 기준. box-shadow 등 균일
// 근사가 필요한 곳에서 사용 (네 모서리 중 최대).
fn uniform_radius(lb: &LayoutBox) -> f32 {
    corner_radii(lb).into_iter().fold(0.0, f32::max)
}

// 한 반경 속성(px/%) 을 논리 px 로. 짧은 변 기준 퍼센트.
fn radius_prop(lb: &LayoutBox, name: &str, short: f32) -> Option<f32> {
    match lb.styled_node.value(name) {
        Some(Value::Length(v, crate::css::Unit::Px)) => Some(v.max(0.0)),
        Some(Value::Length(v, crate::css::Unit::Percent)) => Some(v / 100.0 * short),
        _ => None,
    }
}

// 네 모서리 반경(논리 px): [top-left, top-right, bottom-right, bottom-left].
// 개별 longhand(border-top-left-radius 등) 우선, 없으면 border-radius 로 폴백.
fn corner_radii(lb: &LayoutBox) -> [f32; 4] {
    let b = lb.dimensions.border_box();
    let short = b.width.min(b.height);
    let base = radius_prop(lb, "border-radius", short).unwrap_or(0.0);
    [
        radius_prop(lb, "border-top-left-radius", short).unwrap_or(base),
        radius_prop(lb, "border-top-right-radius", short).unwrap_or(base),
        radius_prop(lb, "border-bottom-right-radius", short).unwrap_or(base),
        radius_prop(lb, "border-bottom-left-radius", short).unwrap_or(base),
    ]
}

// box-shadow 원문 한 조각: [inset] <dx> <dy> [blur] [spread] [color]
struct ParsedShadow {
    dx: f32,
    dy: f32,
    blur: f32,
    spread: f32,
    color: Color,
    inset: bool,
}

// 콤마로 나뉜 다중 그림자를 모두 파싱 (rgba(...) 안 콤마는 괄호 깊이로 보호).
fn parse_box_shadows(s: &str) -> Vec<ParsedShadow> {
    let mut segs: Vec<&str> = Vec::new();
    let (mut depth, mut start) = (0i32, 0usize);
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                segs.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    segs.push(&s[start..]);
    let parse_len = |t: &str| -> Option<f32> {
        let t = t.trim();
        if let Some(n) = t.strip_suffix("px") {
            n.parse().ok()
        } else {
            t.parse().ok()
        }
    };
    let mut out = Vec::new();
    for seg in segs {
        let seg = seg.trim();
        if seg.is_empty() {
            continue;
        }
        let mut lens: Vec<f32> = Vec::new();
        let mut color: Option<Color> = None;
        let mut inset = false;
        for tok in seg.split_whitespace() {
            if tok == "inset" {
                inset = true;
            } else if let Some(px) = parse_len(tok) {
                lens.push(px);
            } else if let Some(c) = crate::css::parse_color(tok) {
                color = Some(c);
            }
        }
        if lens.len() < 2 {
            continue;
        }
        out.push(ParsedShadow {
            dx: lens[0],
            dy: lens[1],
            blur: lens.get(2).copied().unwrap_or(0.0),
            spread: lens.get(3).copied().unwrap_or(0.0),
            color: color.unwrap_or(Color { r: 0, g: 0, b: 0, a: 128 }),
            inset,
        });
    }
    out
}

// box-shadow(outset) 를 박스 뒤에 발행 — 다중 그림자 지원 (Material elevation 등).
fn emit_box_shadow(lb: &LayoutBox, items: &mut Vec<DisplayItem>) {
    let Some(Value::Keyword(raw)) = lb.styled_node.value("box-shadow") else { return };
    let shadows = parse_box_shadows(&raw);
    let base_r = uniform_radius(lb);
    let b = lb.dimensions.border_box();
    // 첫 그림자가 위에 오도록 역순 push (뒤에 push 될수록 위에 그려짐).
    for sh in shadows.iter().rev() {
        if sh.inset {
            continue; // 안쪽 그림자는 emit_inner_shadow 가 배경 이후 발행
        }
        let rect = Rect {
            x: b.x + sh.dx - sh.spread,
            y: b.y + sh.dy - sh.spread,
            width: b.width + 2.0 * sh.spread,
            height: b.height + 2.0 * sh.spread,
        };
        let radius = (base_r + sh.spread).max(0.0);
        items.push(DisplayItem::Shadow { color: sh.color, rect, radius, blur: sh.blur });
    }
}

// SVG 타원 호(A 명령)를 선분으로 평탄화 (SVG 구현 노트 F.6). (x0,y0)=시작,
// (x1,y1)=끝, phi=x축 회전(rad). cubic/quad 와 같은 규약: 끝점은 push, 시작점은
// 이미 out 에 있다고 가정.
fn flatten_arc(
    out: &mut Vec<(f32, f32)>,
    x0: f32,
    y0: f32,
    mut rx: f32,
    mut ry: f32,
    phi: f32,
    large_arc: bool,
    sweep: bool,
    x1: f32,
    y1: f32,
) {
    if rx == 0.0 || ry == 0.0 {
        out.push((x1, y1)); // 반경 0 → 직선
        return;
    }
    rx = rx.abs();
    ry = ry.abs();
    let (sp, cp) = phi.sin_cos();
    // F.6.5.1: 중점 기준 좌표계로 (회전 제거)
    let dx = (x0 - x1) / 2.0;
    let dy = (y0 - y1) / 2.0;
    let x1p = cp * dx + sp * dy;
    let y1p = -sp * dx + cp * dy;
    // F.6.6.2: 반경이 너무 작으면 키운다
    let lambda = x1p * x1p / (rx * rx) + y1p * y1p / (ry * ry);
    if lambda > 1.0 {
        let s = lambda.sqrt();
        rx *= s;
        ry *= s;
    }
    let (rx2, ry2) = (rx * rx, ry * ry);
    // F.6.5.2: 중심
    let num = (rx2 * ry2 - rx2 * y1p * y1p - ry2 * x1p * x1p).max(0.0);
    let den = rx2 * y1p * y1p + ry2 * x1p * x1p;
    let co = if den == 0.0 { 0.0 } else { (num / den).sqrt() };
    let sign = if large_arc != sweep { 1.0 } else { -1.0 };
    let cxp = sign * co * (rx * y1p / ry);
    let cyp = sign * co * (-ry * x1p / rx);
    let cx = cp * cxp - sp * cyp + (x0 + x1) / 2.0;
    let cy = sp * cxp + cp * cyp + (y0 + y1) / 2.0;
    // F.6.5.4: 시작각과 스윕각
    let (ux, uy) = ((x1p - cxp) / rx, (y1p - cyp) / ry);
    let (vx, vy) = ((-x1p - cxp) / rx, (-y1p - cyp) / ry);
    let ang = |ux: f32, uy: f32, vx: f32, vy: f32| -> f32 {
        let dot = ux * vx + uy * vy;
        let len = ((ux * ux + uy * uy) * (vx * vx + vy * vy)).sqrt();
        let mut a = if len == 0.0 { 0.0 } else { (dot / len).clamp(-1.0, 1.0).acos() };
        if ux * vy - uy * vx < 0.0 {
            a = -a;
        }
        a
    };
    let theta1 = ang(1.0, 0.0, ux, uy);
    let mut dtheta = ang(ux, uy, vx, vy);
    if !sweep && dtheta > 0.0 {
        dtheta -= std::f32::consts::TAU;
    }
    if sweep && dtheta < 0.0 {
        dtheta += std::f32::consts::TAU;
    }
    // 스윕각 크기에 비례해 분할 (약 32/원)
    let n = ((dtheta.abs() / (std::f32::consts::PI / 16.0)).ceil() as usize).clamp(2, 64);
    for i in 1..=n {
        let t = theta1 + dtheta * (i as f32 / n as f32);
        let (st, ct) = t.sin_cos();
        let px = cp * rx * ct - sp * ry * st + cx;
        let py = sp * rx * ct + cp * ry * st + cy;
        out.push((px, py));
    }
}

// SVG path d 속성 → 서브패스(윤곽선) 폴리라인 목록. 베지어/호는 평탄화, 상대/절대 지원.
// 지원: M/L/H/V/C/S/Q/T/A/Z (대소문자).
fn flatten_path(d: &str) -> Vec<Vec<(f32, f32)>> {
    // 1) 명령 그룹으로 토큰화
    struct Group {
        cmd: char,
        nums: Vec<f32>,
    }
    let mut groups: Vec<Group> = Vec::new();
    let ch: Vec<char> = d.chars().collect();
    let n = ch.len();
    let mut i = 0;
    while i < n {
        let c = ch[i];
        if c.is_whitespace() || c == ',' {
            i += 1;
            continue;
        }
        if c.is_ascii_alphabetic() {
            groups.push(Group { cmd: c, nums: Vec::new() });
            i += 1;
            continue;
        }
        // 숫자: 부호/소수/지수
        let start = i;
        if ch[i] == '+' || ch[i] == '-' {
            i += 1;
        }
        while i < n && ch[i].is_ascii_digit() {
            i += 1;
        }
        if i < n && ch[i] == '.' {
            i += 1;
            while i < n && ch[i].is_ascii_digit() {
                i += 1;
            }
        }
        if i < n && (ch[i] == 'e' || ch[i] == 'E') {
            i += 1;
            if i < n && (ch[i] == '+' || ch[i] == '-') {
                i += 1;
            }
            while i < n && ch[i].is_ascii_digit() {
                i += 1;
            }
        }
        if i == start {
            i += 1; // 진행 보장
            continue;
        }
        if let Ok(v) = ch[start..i].iter().collect::<String>().parse::<f32>() {
            if let Some(g) = groups.last_mut() {
                g.nums.push(v);
            }
        }
    }
    // 2) 그룹 해석 (명령별 인자 개수만큼 반복)
    let arity = |c: char| match c.to_ascii_uppercase() {
        'M' | 'L' | 'T' => 2,
        'H' | 'V' => 1,
        'C' => 6,
        'S' | 'Q' => 4,
        'A' => 7,
        _ => 0,
    };
    let mut subs: Vec<Vec<(f32, f32)>> = Vec::new();
    let mut cur: Vec<(f32, f32)> = Vec::new();
    let (mut x, mut y) = (0.0f32, 0.0f32);
    let (mut startx, mut starty) = (0.0f32, 0.0f32);
    let (mut ctrlx, mut ctrly) = (0.0f32, 0.0f32); // S/T 반사용 직전 제어점
    let mut prev_cmd = ' ';
    let flatten_cubic =
        |p: &mut Vec<(f32, f32)>, x0: f32, y0: f32, x1: f32, y1: f32, x2: f32, y2: f32, x3: f32, y3: f32| {
            for s in 1..=16 {
                let t = s as f32 / 16.0;
                let u = 1.0 - t;
                let bx = u * u * u * x0 + 3.0 * u * u * t * x1 + 3.0 * u * t * t * x2 + t * t * t * x3;
                let by = u * u * u * y0 + 3.0 * u * u * t * y1 + 3.0 * u * t * t * y2 + t * t * t * y3;
                p.push((bx, by));
            }
        };
    let flatten_quad = |p: &mut Vec<(f32, f32)>, x0: f32, y0: f32, x1: f32, y1: f32, x2: f32, y2: f32| {
        for s in 1..=12 {
            let t = s as f32 / 12.0;
            let u = 1.0 - t;
            let bx = u * u * x0 + 2.0 * u * t * x1 + t * t * x2;
            let by = u * u * y0 + 2.0 * u * t * y1 + t * t * y2;
            p.push((bx, by));
        }
    };
    for g in &groups {
        let up = g.cmd.to_ascii_uppercase();
        let rel = g.cmd.is_ascii_lowercase();
        if up == 'Z' {
            if !cur.is_empty() {
                cur.push((startx, starty));
                subs.push(std::mem::take(&mut cur));
            }
            x = startx;
            y = starty;
            prev_cmd = up;
            continue;
        }
        let ar = arity(up);
        if ar == 0 {
            continue;
        }
        let mut idx = 0;
        let mut first = true;
        while idx + ar <= g.nums.len() {
            let a = &g.nums[idx..idx + ar];
            let eff = if first {
                up
            } else if up == 'M' {
                'L'
            } else {
                up
            };
            match eff {
                'M' => {
                    if !cur.is_empty() {
                        subs.push(std::mem::take(&mut cur));
                    }
                    x = if rel { x + a[0] } else { a[0] };
                    y = if rel { y + a[1] } else { a[1] };
                    startx = x;
                    starty = y;
                    cur.push((x, y));
                }
                'L' => {
                    x = if rel { x + a[0] } else { a[0] };
                    y = if rel { y + a[1] } else { a[1] };
                    cur.push((x, y));
                }
                'H' => {
                    x = if rel { x + a[0] } else { a[0] };
                    cur.push((x, y));
                }
                'V' => {
                    y = if rel { y + a[0] } else { a[0] };
                    cur.push((x, y));
                }
                'C' => {
                    let (x1, y1) = (if rel { x + a[0] } else { a[0] }, if rel { y + a[1] } else { a[1] });
                    let (x2, y2) = (if rel { x + a[2] } else { a[2] }, if rel { y + a[3] } else { a[3] });
                    let (x3, y3) = (if rel { x + a[4] } else { a[4] }, if rel { y + a[5] } else { a[5] });
                    flatten_cubic(&mut cur, x, y, x1, y1, x2, y2, x3, y3);
                    ctrlx = x2;
                    ctrly = y2;
                    x = x3;
                    y = y3;
                }
                'S' => {
                    // 부드러운 3차: 첫 제어점 = 직전 제어점의 반사 (C/S 뒤일 때)
                    let (rx, ry) = if matches!(prev_cmd, 'C' | 'S') {
                        (2.0 * x - ctrlx, 2.0 * y - ctrly)
                    } else {
                        (x, y)
                    };
                    let (x2, y2) = (if rel { x + a[0] } else { a[0] }, if rel { y + a[1] } else { a[1] });
                    let (x3, y3) = (if rel { x + a[2] } else { a[2] }, if rel { y + a[3] } else { a[3] });
                    flatten_cubic(&mut cur, x, y, rx, ry, x2, y2, x3, y3);
                    ctrlx = x2;
                    ctrly = y2;
                    x = x3;
                    y = y3;
                }
                'Q' => {
                    let (x1, y1) = (if rel { x + a[0] } else { a[0] }, if rel { y + a[1] } else { a[1] });
                    let (x2, y2) = (if rel { x + a[2] } else { a[2] }, if rel { y + a[3] } else { a[3] });
                    flatten_quad(&mut cur, x, y, x1, y1, x2, y2);
                    ctrlx = x1;
                    ctrly = y1;
                    x = x2;
                    y = y2;
                }
                'T' => {
                    let (rx, ry) = if matches!(prev_cmd, 'Q' | 'T') {
                        (2.0 * x - ctrlx, 2.0 * y - ctrly)
                    } else {
                        (x, y)
                    };
                    let (x2, y2) = (if rel { x + a[0] } else { a[0] }, if rel { y + a[1] } else { a[1] });
                    flatten_quad(&mut cur, x, y, rx, ry, x2, y2);
                    ctrlx = rx;
                    ctrly = ry;
                    x = x2;
                    y = y2;
                }
                'A' => {
                    // rx ry x축회전 large-arc sweep x y
                    let (rx, ry) = (a[0], a[1]);
                    let phi = a[2].to_radians();
                    let large = a[3] != 0.0;
                    let sweep = a[4] != 0.0;
                    let ex = if rel { x + a[5] } else { a[5] };
                    let ey = if rel { y + a[6] } else { a[6] };
                    flatten_arc(&mut cur, x, y, rx, ry, phi, large, sweep, ex, ey);
                    x = ex;
                    y = ey;
                }
                _ => {}
            }
            prev_cmd = eff;
            idx += ar;
            first = false;
        }
    }
    if !cur.is_empty() {
        subs.push(cur);
    }
    subs
}

// (x1,y1)-(x2,y2) 를 잇는 굵기 sw 의 선을 방향에 맞춘 사각형(quad) 4점으로.
// 길이가 0 이면 None(호출부가 점으로 처리). butt cap(끝 연장 없음).
fn stroke_line_quad(x1: f32, y1: f32, x2: f32, y2: f32, sw: f32) -> Option<Vec<(f32, f32)>> {
    let (dx, dy) = (x2 - x1, y2 - y1);
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-3 {
        return None;
    }
    // 선에 수직인 반-굵기 벡터
    let (nx, ny) = (-dy / len * sw / 2.0, dx / len * sw / 2.0);
    Some(vec![
        (x1 + nx, y1 + ny),
        (x2 + nx, y2 + ny),
        (x2 - nx, y2 - ny),
        (x1 - nx, y1 - ny),
    ])
}

// 인라인 SVG 의 기본 도형(rect/circle/ellipse/line/path/polygon)을 viewBox 매핑으로 발행.
// line 은 방향 맞춘 quad 로 대각선도 정확. arc(A)는 아직 현(chord) 근사.
// DisplayItem 목록을 오프스크린 RGBA 로 래스터화한다 (canvas getImageData 용).
// 캔버스의 실제 픽셀을 읽으려면 진짜로 그려 봐야 한다 — 명령 목록만으로는 알 수 없다.
pub fn rasterize_items(
    items: &[DisplayItem],
    w: usize,
    h: usize,
    fonts: &FontStack,
    images: &[crate::png::Image],
) -> crate::png::Image {
    let mut canvas = Canvas::new_layer(w, h);
    let mut cache = GlyphCache::new();
    for item in items {
        draw_item(&mut canvas, item, 0.0, 1.0, h as f32, fonts, &mut cache, images);
    }
    let mut rgba = Vec::with_capacity(w * h * 4);
    for px in &canvas.pixels {
        rgba.extend_from_slice(&[px.r, px.g, px.b, px.a]);
    }
    crate::png::Image { width: w, height: h, rgba }
}

// SVG 소스를 RGBA 이미지로 래스터화한다 (CSS background-image: url(*.svg) 용).
// <img src=*.svg> 는 DOM 에서 인라인 <svg> 로 바꿔치기해 그리지만, CSS 배경은 그럴 수
// 없다 — 실제 픽셀이 필요하다. 로고/아이콘이 대부분 SVG 라 이게 없으면 통째로 빈다.
pub fn rasterize_svg(
    source: &str,
    w: usize,
    h: usize,
    fonts: &FontStack,
) -> Option<crate::png::Image> {
    if w == 0 || h == 0 || w > 4096 || h > 4096 {
        return None;
    }
    let dom = crate::html::parse_dom(source.to_string());
    let sheet = crate::css::parse(String::new());
    let styled = crate::style::style_tree(&dom, &sheet);
    // 트리에서 <svg> 요소 찾기
    fn find_svg<'a, 'b>(n: &'b crate::style::StyledNode<'a>) -> Option<&'b crate::style::StyledNode<'a>> {
        if let crate::dom::NodeType::Element(e) = &n.node.node_type {
            if e.tag_name == "svg" {
                return Some(n);
            }
        }
        n.children.iter().find_map(find_svg)
    }
    let svg_node = find_svg(&styled)?;
    let crate::dom::NodeType::Element(svg) = &svg_node.node.node_type else { return None };
    let box_rect = Rect { x: 0.0, y: 0.0, width: w as f32, height: h as f32 };
    let (vx, vy, sx, sy) = match svg.attributes.get("viewbox").and_then(|s| crate::layout::parse_viewbox(s)) {
        Some((vx, vy, vw, vh)) if vw > 0.0 && vh > 0.0 => {
            (vx, vy, box_rect.width / vw, box_rect.height / vh)
        }
        _ => (0.0, 0.0, 1.0, 1.0),
    };
    let mut items = Vec::new();
    let crate::dom::NodeType::Element(root_e) = &svg_node.node.node_type else { return None };
    let inherit = svg_inherit(root_e, SvgInherit::default());
    emit_svg_children(svg_node, svg_node, box_rect, vx, vy, sx, sy, &mut items, 0, inherit);
    // <text> → 글리프 (예전엔 빈 FontStack 이라 독립 SVG 의 글자가 통째로 사라졌다)
    let mut glyphs = Vec::new();
    crate::layout::collect_svg_text_public(
        svg_node,
        box_rect,
        (vx, vy, sx, sy),
        fonts,
        &mut glyphs,
    );
    for g in glyphs {
        items.push(DisplayItem::Glyph(g));
    }
    // 투명 배경 레이어에 그린다 (알파 유지 → 배경 위에 합성된다)
    let mut canvas = Canvas::new_layer(w, h);
    let mut cache = GlyphCache::new();
    for item in &items {
        draw_item(&mut canvas, item, 0.0, 1.0, h as f32, fonts, &mut cache, &[]);
    }
    let mut rgba = Vec::with_capacity(w * h * 4);
    for px in &canvas.pixels {
        rgba.extend_from_slice(&[px.r, px.g, px.b, px.a]);
    }
    Some(crate::png::Image { width: w, height: h, rgba })
}

// SVG 의 고유 크기: width/height 속성 → viewBox → 기본 100x100.
pub fn svg_natural_size(source: &str) -> (usize, usize) {
    let num = |tag: &str| -> Option<f32> {
        let key = format!("{}=\"", tag);
        let i = source.find(&key)? + key.len();
        let rest = &source[i..];
        let end = rest.find('"')?;
        rest[..end].trim().trim_end_matches("px").parse::<f32>().ok()
    };
    if let (Some(w), Some(h)) = (num("width"), num("height")) {
        if w > 0.0 && h > 0.0 {
            return (w.ceil() as usize, h.ceil() as usize);
        }
    }
    if let Some(i) = source.find("viewBox=\"").or_else(|| source.find("viewbox=\"")) {
        let rest = &source[i + 9..];
        if let Some(end) = rest.find('"') {
            if let Some((_, _, vw, vh)) = crate::layout::parse_viewbox(&rest[..end]) {
                if vw > 0.0 && vh > 0.0 {
                    return (vw.ceil() as usize, vh.ceil() as usize);
                }
            }
        }
    }
    (100, 100)
}

fn emit_svg(lb: &LayoutBox, items: &mut Vec<DisplayItem>) {
    let crate::dom::NodeType::Element(svg) = &lb.styled_node.node.node_type else { return };
    if svg.tag_name != "svg" {
        return;
    }
    let box_rect = lb.dimensions.content;
    // viewBox → 박스 좌표 매핑
    let (vx, vy, sx, sy) = match svg.attributes.get("viewbox").and_then(|s| crate::layout::parse_viewbox(s)) {
        Some((vx, vy, vw, vh)) if vw > 0.0 && vh > 0.0 => {
            (vx, vy, box_rect.width / vw, box_rect.height / vh)
        }
        _ => (0.0, 0.0, 1.0, 1.0),
    };
    let inherit = svg_inherit(svg, SvgInherit::default());
    emit_svg_children(lb.styled_node, lb.styled_node, box_rect, vx, vy, sx, sy, items, 0, inherit);
}

// SVG 도형을 재귀적으로 그린다. <g> 그룹 안의 도형도 그려야 한다 — 예전엔 <svg> 의
// **직계 자식만** 봐서, 그룹으로 감싼 아이콘(대부분의 실제 SVG)이 통째로 안 그려졌다.
#[allow(clippy::too_many_arguments)]
// SVG 의 표현 속성(fill/stroke/stroke-width/opacity)은 **상속된다**.
// 아이콘 세트는 전부 <svg fill="none" stroke="currentColor" stroke-width="2"> 처럼
// 루트에 걸어 둔다 — 상속을 안 하면 path 가 기본값(검정 채우기)으로 그려져
// 얇은 윤곽선 아이콘이 **시커먼 덩어리**가 된다 (조용히 틀린 그림).
#[derive(Clone, Copy)]
struct SvgInherit {
    fill: Option<Color>,
    stroke: Option<Color>,
    stroke_width: f32,
}

impl Default for SvgInherit {
    fn default() -> Self {
        SvgInherit {
            fill: Some(Color { r: 0, g: 0, b: 0, a: 255 }), // SVG 기본 fill 은 검정
            stroke: None,
            stroke_width: 1.0,
        }
    }
}

fn svg_inherit(e: &crate::dom::ElementData, base: SvgInherit) -> SvgInherit {
    let mut out = base;
    if let Some(v) = e.attributes.get("fill") {
        out.fill = if v.trim() == "none" { None } else { crate::css::parse_color(v.trim()) };
    }
    if let Some(v) = e.attributes.get("stroke") {
        out.stroke = if v.trim() == "none" { None } else { crate::css::parse_color(v.trim()) };
    }
    if let Some(v) = e.attributes.get("stroke-width").and_then(|v| v.trim().parse::<f32>().ok()) {
        out.stroke_width = v;
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn emit_svg_children<'a, 'b>(
    parent: &'b crate::style::StyledNode<'a>,
    root: &'b crate::style::StyledNode<'a>,
    box_rect: Rect,
    vx: f32,
    vy: f32,
    sx: f32,
    sy: f32,
    items: &mut Vec<DisplayItem>,
    depth: usize,
    inherit: SvgInherit,
) {
    if depth > 16 {
        return; // 병적으로 깊은 중첩 방어
    }
    for shape in &parent.children {
        let crate::dom::NodeType::Element(e) = &shape.node.node_type else { continue };
        let tag = e.tag_name.as_str();
        // <defs> 안의 도형은 직접 그리지 않는다 (<use> 로 참조된다)
        if tag == "defs" || tag == "text" || tag == "tspan" {
            continue;
        }
        // 이 요소가 만들 아이템은 여기서부터
        let start_len = items.len();

        if tag == "g" || tag == "svg" {
            emit_svg_children(
                shape,
                root,
                box_rect,
                vx,
                vy,
                sx,
                sy,
                items,
                depth + 1,
                svg_inherit(e, inherit),
            );
        } else if tag == "use" {
            // <use href="#id"> — 참조한 도형을 x/y 만큼 옮겨 그린다.
            // 예전엔 통째로 무시돼서 아이콘 스프라이트가 전부 빈 화면이었다.
            let href = e
                .attributes
                .get("href")
                .or_else(|| e.attributes.get("xlink:href"))
                .cloned()
                .unwrap_or_default();
            if let Some(id) = href.strip_prefix('#') {
                if let Some(target) = find_svg_by_id(root, id, 0) {
                    // 참조 대상을 임시 부모로 삼아 그린다 (자기 자신 하나만)
                    let before = items.len();
                    emit_one_svg_shape(target, box_rect, vx, vy, sx, sy, items, inherit);
                    let (ux, uy) = (
                        e.attributes.get("x").and_then(|v| v.trim().parse::<f32>().ok()).unwrap_or(0.0),
                        e.attributes.get("y").and_then(|v| v.trim().parse::<f32>().ok()).unwrap_or(0.0),
                    );
                    if ux != 0.0 || uy != 0.0 {
                        let m = crate::layout::Mat3 {
                            m: [[1.0, 0.0, ux * sx], [0.0, 1.0, uy * sy], [0.0, 0.0, 1.0]],
                        };
                        let inner: Vec<DisplayItem> = items.drain(before..).collect();
                        items.push(DisplayItem::Transform { m, items: inner });
                    }
                }
            }
        } else {
            emit_one_svg_shape(shape, box_rect, vx, vy, sx, sy, items, inherit);
        }

        // transform 속성 (g 와 도형 공통). 예전엔 <g transform> 을 통째로 무시해서
        // 그룹 전체가 **엉뚱한 자리에** 그려졌다 (조용히 틀린 그림).
        if let Some(t) = e.attributes.get("transform") {
            if items.len() > start_len {
                let local = crate::layout::parse_svg_transform(t);
                // viewBox 스케일 좌표계로 옮긴다: S · M · S⁻¹ (S 는 mx/my 매핑)
                let to_box = crate::layout::Mat3 {
                    m: [[sx, 0.0, box_rect.x - vx * sx], [0.0, sy, box_rect.y - vy * sy], [0.0, 0.0, 1.0]],
                };
                let from_box = crate::layout::Mat3 {
                    m: [
                        [1.0 / sx, 0.0, (vx * sx - box_rect.x) / sx],
                        [0.0, 1.0 / sy, (vy * sy - box_rect.y) / sy],
                        [0.0, 0.0, 1.0],
                    ],
                };
                let m = from_box.then(&local).then(&to_box);
                let inner: Vec<DisplayItem> = items.drain(start_len..).collect();
                items.push(DisplayItem::Transform { m, items: inner });
            }
        }
        // opacity: 그룹/도형 투명도 (예전엔 무시돼서 전부 불투명하게 나왔다)
        if let Some(op) = e.attributes.get("opacity").and_then(|v| v.trim().parse::<f32>().ok()) {
            if op < 1.0 && items.len() > start_len {
                let inner: Vec<DisplayItem> = items.drain(start_len..).collect();
                items.push(DisplayItem::Layer {
                    opacity: op.clamp(0.0, 1.0),
                    blend: BlendMode::Normal,
                    items: inner,
                });
            }
        }
    }
}

fn find_svg_by_id<'a, 'b>(
    n: &'b crate::style::StyledNode<'a>,
    id: &str,
    depth: usize,
) -> Option<&'b crate::style::StyledNode<'a>> {
    if depth > 24 {
        return None;
    }
    if let crate::dom::NodeType::Element(e) = &n.node.node_type {
        if e.attributes.get("id").map(|s| s.as_str()) == Some(id) {
            return Some(n);
        }
    }
    n.children.iter().find_map(|c| find_svg_by_id(c, id, depth + 1))
}

// 도형 하나 → 아이템들 (fill 과 stroke 를 **둘 다** 그린다).
// 예전엔 fill 만 그렸다 — Feather/Lucide 같은 아이콘 세트는 전부 fill="none" + stroke 라서
// **아무것도 안 나왔다** (아이콘이 통째로 사라졌다).
#[allow(clippy::too_many_arguments)]
fn emit_one_svg_shape(
    shape: &crate::style::StyledNode,
    box_rect: Rect,
    vx: f32,
    vy: f32,
    sx: f32,
    sy: f32,
    items: &mut Vec<DisplayItem>,
    inherit: SvgInherit,
) {
    let crate::dom::NodeType::Element(e) = &shape.node.node_type else { return };
    let mx = |x: f32| box_rect.x + (x - vx) * sx;
    let my = |y: f32| box_rect.y + (y - vy) * sy;
    let num = |k: &str| e.attributes.get(k).and_then(|v| v.trim().parse::<f32>().ok());
    let with_op = |c: Option<Color>, key: &str| -> Option<Color> {
        let mut c = c?;
        if let Some(o) = e.attributes.get(key).and_then(|v| v.trim().parse::<f32>().ok()) {
            c.a = (c.a as f32 * o.clamp(0.0, 1.0)).round() as u8;
        }
        Some(c)
    };
    // 자기 속성이 있으면 그것, 없으면 **상속값** (아이콘 세트는 루트에 걸어 둔다)
    let own = svg_inherit(e, inherit);
    let fill = with_op(own.fill, "fill-opacity");
    let stroke = with_op(own.stroke, "stroke-opacity");
    let sw = (own.stroke_width * sx).max(1.0);

    // 겹치는 스트로크 조각들의 방향이 뒤섞이면 nonzero winding 에서 **서로 상쇄돼 구멍**이 난다
    // (원의 윤곽선이 흐릿하게 사라졌다). 전부 같은 방향(양의 면적)으로 맞춘다.
    fn ccw(mut c: Vec<(f32, f32)>) -> Vec<(f32, f32)> {
        let n = c.len();
        let area: f32 = (0..n)
            .map(|i| {
                let (x1, y1) = c[i];
                let (x2, y2) = c[(i + 1) % n];
                x1 * y2 - x2 * y1
            })
            .sum();
        if area < 0.0 {
            c.reverse();
        }
        c
    }

    // 열린 경로(윤곽선)를 굵기 sw 의 폴리곤들로
    let stroke_contour = |pts: &[(f32, f32)], closed: bool, color: Color, out: &mut Vec<DisplayItem>| {
        let n = pts.len();
        if n < 2 {
            return;
        }
        let segs: Vec<((f32, f32), (f32, f32))> = if closed {
            (0..n).map(|i| (pts[i], pts[(i + 1) % n])).collect()
        } else {
            (0..n - 1).map(|i| (pts[i], pts[i + 1])).collect()
        };
        let mut contours = Vec::new();
        for ((x1, y1), (x2, y2)) in segs {
            if let Some(q) = stroke_line_quad(x1, y1, x2, y2, sw) {
                contours.push(ccw(q));
            }
        }
        // 이음새를 원으로 메운다 (SVG 기본 join 은 miter 지만, 얇은 선에선 차이가 안 보인다)
        if sw > 2.0 {
            for &(px, py) in pts.iter() {
                let r = sw / 2.0;
                contours.push(ccw(
                    (0..12)
                        .map(|k| {
                            let t = k as f32 / 12.0 * std::f32::consts::TAU;
                            (px + t.cos() * r, py + t.sin() * r)
                        })
                        .collect(),
                ));
            }
        }
        if !contours.is_empty() {
            out.push(DisplayItem::Polygon { color, contours });
        }
    };

    match e.tag_name.as_str() {
        "rect" => {
            let (x, y) = (mx(num("x").unwrap_or(0.0)), my(num("y").unwrap_or(0.0)));
            let (w, h) = (num("width").unwrap_or(0.0) * sx, num("height").unwrap_or(0.0) * sy);
            if w <= 0.0 || h <= 0.0 {
                return;
            }
            let r = num("rx").map(|r| r * sx).unwrap_or(0.0);
            let rect = Rect { x, y, width: w, height: h };
            if let Some(color) = fill {
                if r > 0.0 {
                    items.push(DisplayItem::RoundRect { color, rect, radii: [r; 4] });
                } else {
                    items.push(DisplayItem::Rect { color, rect });
                }
            }
            if let Some(color) = stroke {
                let pts = [
                    (x, y),
                    (x + w, y),
                    (x + w, y + h),
                    (x, y + h),
                ];
                stroke_contour(&pts, true, color, items);
            }
        }
        "circle" | "ellipse" => {
            let (rx, ry) = if e.tag_name == "circle" {
                let r = num("r").unwrap_or(0.0);
                (r, r)
            } else {
                (num("rx").unwrap_or(0.0), num("ry").unwrap_or(0.0))
            };
            let (cx, cy) = (num("cx").unwrap_or(0.0), num("cy").unwrap_or(0.0));
            let rect = Rect {
                x: mx(cx - rx),
                y: my(cy - ry),
                width: 2.0 * rx * sx,
                height: 2.0 * ry * sy,
            };
            if let Some(color) = fill {
                items.push(DisplayItem::RoundRect {
                    color,
                    rect,
                    radii: [rx.min(ry) * sx; 4],
                });
            }
            if let Some(color) = stroke {
                // 타원 둘레를 다각형으로 근사해 스트로크
                let pts: Vec<(f32, f32)> = (0..48)
                    .map(|k| {
                        let t = k as f32 / 48.0 * std::f32::consts::TAU;
                        (mx(cx + rx * t.cos()), my(cy + ry * t.sin()))
                    })
                    .collect();
                stroke_contour(&pts, true, color, items);
            }
        }
        "line" => {
            if let Some(color) = stroke {
                let (x1, y1) = (mx(num("x1").unwrap_or(0.0)), my(num("y1").unwrap_or(0.0)));
                let (x2, y2) = (mx(num("x2").unwrap_or(0.0)), my(num("y2").unwrap_or(0.0)));
                match stroke_line_quad(x1, y1, x2, y2, sw) {
                    Some(quad) => items.push(DisplayItem::Polygon { color, contours: vec![quad] }),
                    None => {
                        let h = sw / 2.0;
                        items.push(DisplayItem::Rect {
                            color,
                            rect: Rect { x: x1 - h, y: y1 - h, width: sw, height: sw },
                        });
                    }
                }
            }
        }
        "path" => {
            let Some(d) = e.attributes.get("d") else { return };
            let raw = flatten_path(d);
            let contours: Vec<Vec<(f32, f32)>> = raw
                .iter()
                .map(|c| c.iter().map(|&(px, py)| (mx(px), my(py))).collect())
                .collect();
            if let Some(color) = fill {
                let filled: Vec<Vec<(f32, f32)>> =
                    contours.iter().filter(|c| c.len() >= 3).cloned().collect();
                if !filled.is_empty() {
                    items.push(DisplayItem::Polygon { color, contours: filled });
                }
            }
            if let Some(color) = stroke {
                for c in &contours {
                    stroke_contour(c, false, color, items);
                }
            }
        }
        "polygon" | "polyline" => {
            let Some(pts_attr) = e.attributes.get("points") else { return };
            let nums: Vec<f32> = pts_attr
                .split(|c: char| c == ',' || c.is_whitespace())
                .filter_map(|t| t.parse::<f32>().ok())
                .collect();
            let contour: Vec<(f32, f32)> = nums
                .chunks(2)
                .filter(|p| p.len() == 2)
                .map(|p| (mx(p[0]), my(p[1])))
                .collect();
            let closed = e.tag_name == "polygon";
            if let Some(color) = fill {
                if contour.len() >= 3 && closed {
                    items.push(DisplayItem::Polygon { color, contours: vec![contour.clone()] });
                }
            }
            if let Some(color) = stroke {
                stroke_contour(&contour, closed, color, items);
            }
        }
        _ => {}
    }
}


// outline: border box 밖으로 offset+width 만큼 나온 균일 링 (4개 사각형). 레이아웃 불변.
fn emit_outline(lb: &LayoutBox, items: &mut Vec<DisplayItem>) {
    let w = match lb.styled_node.value("outline-width") {
        Some(Value::Length(v, crate::css::Unit::Px)) if v > 0.0 => v,
        _ => return,
    };
    // outline-style 이 none 이면 안 그림 (지정 없으면 색이 있을 때만 관용적으로 그림)
    if matches!(lb.styled_node.value("outline-style"), Some(Value::Keyword(ref k)) if k == "none") {
        return;
    }
    let color = match lb.styled_node.value("outline-color") {
        Some(Value::Color(c)) => c,
        _ => get_color(lb, "color").unwrap_or(Color { r: 0, g: 0, b: 0, a: 255 }),
    };
    let offset = match lb.styled_node.value("outline-offset") {
        Some(Value::Length(v, crate::css::Unit::Px)) => v,
        _ => 0.0,
    };
    let b = lb.dimensions.border_box();
    // 안쪽 경계(offset), 바깥 경계(offset+width)
    let o = offset;
    let ow = offset + w;
    // 위/아래/좌/우 띠
    items.push(DisplayItem::Rect { color, rect: Rect { x: b.x - ow, y: b.y - ow, width: b.width + 2.0 * ow, height: w } });
    items.push(DisplayItem::Rect { color, rect: Rect { x: b.x - ow, y: b.y + b.height + o, width: b.width + 2.0 * ow, height: w } });
    items.push(DisplayItem::Rect { color, rect: Rect { x: b.x - ow, y: b.y - o, width: w, height: b.height + 2.0 * o } });
    items.push(DisplayItem::Rect { color, rect: Rect { x: b.x + b.width + o, y: b.y - o, width: w, height: b.height + 2.0 * o } });
}

// 안쪽 그림자(inset): 박스 내부 경계에서 안으로 번진다. 배경/테두리 위에 그린다.
fn emit_inner_shadow(lb: &LayoutBox, items: &mut Vec<DisplayItem>) {
    // outset 과 **같은 파서**를 쓴다. 예전엔 inset 만 롱핸드(box-shadow-x/y/blur/color)를
    // 읽었고, 그래서 **spread 를 통째로 무시**했고 다중 inset 도 못 그렸다
    // (box-shadow: inset 0 0 0 10px green 같은 "안쪽 링"이 아예 안 나왔다).
    let Some(Value::Keyword(raw)) = lb.styled_node.value("box-shadow") else { return };
    let base_r = uniform_radius(lb).max(0.0);
    let b = lb.dimensions.border_box();
    for sh in parse_box_shadows(&raw).iter().rev() {
        if !sh.inset {
            continue; // 바깥 그림자는 emit_box_shadow 가 배경 앞에 발행
        }
        items.push(DisplayItem::InnerShadow {
            color: sh.color,
            rect: b,
            radius: base_r,
            blur: sh.blur,
            spread: sh.spread,
            dx: sh.dx,
            dy: sh.dy,
        });
    }
}

// 박스 배경 + 테두리를 발행. border-radius 가 있으면 둥근 사각형으로,
// 균일 테두리는 "테두리색 라운드 → 안쪽 배경 라운드" 레이어링으로 둥근 테두리 근사.
// (x0,y0)-(x1,y1) 을 두께 t 의 사각 리본(4점 다각형)으로. 체크마크 획 그리기용.
fn thick_segment(a: (f32, f32), b: (f32, f32), t: f32) -> Vec<(f32, f32)> {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = (dx * dx + dy * dy).sqrt().max(1e-3);
    let (nx, ny) = (-dy / len * t / 2.0, dx / len * t / 2.0);
    vec![(a.0 + nx, a.1 + ny), (b.0 + nx, b.1 + ny), (b.0 - nx, b.1 - ny), (a.0 - nx, a.1 - ny)]
}

// 네이티브 폼 컨트롤(체크박스/라디오)을 폰트 없이 프리미티브로 그린다.
fn emit_form_control(lb: &LayoutBox, fc: crate::layout::FormControl, items: &mut Vec<DisplayItem>) {
    use crate::layout::FormControl;
    let b = lb.dimensions.border_box();
    let gray = Color { r: 118, g: 118, b: 118, a: 255 };
    let white = Color { r: 255, g: 255, b: 255, a: 255 };
    let accent = Color { r: 26, g: 115, b: 232, a: 255 };
    match fc {
        FormControl::Checkbox(checked) => {
            let radii = [2.0f32; 4];
            if checked {
                items.push(DisplayItem::RoundRect { color: accent, rect: b, radii });
                let p = |nx: f32, ny: f32| (b.x + nx * b.width, b.y + ny * b.height);
                let t = b.width * 0.15;
                let contours = vec![
                    thick_segment(p(0.22, 0.52), p(0.42, 0.70), t),
                    thick_segment(p(0.42, 0.70), p(0.78, 0.28), t),
                ];
                items.push(DisplayItem::Polygon { color: white, contours });
            } else {
                items.push(DisplayItem::RoundRect { color: gray, rect: b, radii });
                let inset =
                    Rect { x: b.x + 1.0, y: b.y + 1.0, width: b.width - 2.0, height: b.height - 2.0 };
                items.push(DisplayItem::RoundRect { color: white, rect: inset, radii: [1.5; 4] });
            }
        }
        FormControl::Radio(checked) => {
            let full = [b.width / 2.0; 4];
            items.push(DisplayItem::RoundRect { color: gray, rect: b, radii: full });
            let inset =
                Rect { x: b.x + 1.0, y: b.y + 1.0, width: b.width - 2.0, height: b.height - 2.0 };
            items.push(DisplayItem::RoundRect {
                color: white,
                rect: inset,
                radii: [inset.width / 2.0; 4],
            });
            if checked {
                let d = b.width * 0.30;
                let dot = Rect {
                    x: b.x + d,
                    y: b.y + d,
                    width: b.width - 2.0 * d,
                    height: b.height - 2.0 * d,
                };
                items.push(DisplayItem::RoundRect {
                    color: accent,
                    rect: dot,
                    radii: [dot.width / 2.0; 4],
                });
            }
        }
        FormControl::Gauge { frac, meter } => {
            let track = Color { r: 225, g: 225, b: 227, a: 255 };
            let fill = if meter {
                Color { r: 70, g: 180, b: 80, a: 255 }
            } else {
                Color { r: 40, g: 120, b: 230, a: 255 }
            };
            let radii = [b.height / 2.0; 4];
            items.push(DisplayItem::RoundRect { color: track, rect: b, radii });
            let fw = b.width * frac.clamp(0.0, 1.0);
            if fw > 0.5 {
                let fr = Rect { x: b.x, y: b.y, width: fw, height: b.height };
                items.push(DisplayItem::RoundRect { color: fill, rect: fr, radii });
            }
        }
        FormControl::SelectArrow => {
            let cx = b.x + b.width - 14.0;
            let cy = b.y + b.height / 2.0;
            let (w, h) = (8.0f32, 5.0f32);
            let tri = vec![
                (cx - w / 2.0, cy - h / 2.0),
                (cx + w / 2.0, cy - h / 2.0),
                (cx, cy + h / 2.0),
            ];
            items.push(DisplayItem::Polygon {
                color: Color { r: 90, g: 90, b: 90, a: 255 },
                contours: vec![tri],
            });
        }
    }
}

fn emit_box_decorations(lb: &LayoutBox, items: &mut Vec<DisplayItem>) {
    // 체크박스/라디오/게이지는 기본 배경·테두리 대신 네이티브 컨트롤을 직접 그린다.
    match lb.form_control {
        Some(fc @ crate::layout::FormControl::Checkbox(_))
        | Some(fc @ crate::layout::FormControl::Radio(_))
        | Some(fc @ crate::layout::FormControl::Gauge { .. }) => {
            emit_form_control(lb, fc, items);
            return;
        }
        _ => {}
    }
    let bg = get_color(lb, "background-color");
    let r = uniform_radius(lb);
    let bw = lb.dimensions.border;
    let b = lb.dimensions.border_box();
    let border_uniform = bw.top > 0.0
        && bw.top == bw.right
        && bw.top == bw.bottom
        && bw.top == bw.left
        && border_side_drawn(lb, "top");

    let radii = corner_radii(lb);
    // 라운드 + 균일 테두리: 배경(있으면 padding box 를 둥글게) + 테두리 링.
    // 배경 유무 무관 — 투명 배경 고스트/아웃라인 버튼도 각지지 않는다.
    if r > 0.0 && border_uniform {
        let inner_rect = lb.dimensions.padding_box();
        let inner = radii.map(|c| (c - bw.top).max(0.0));
        if let Some(bgc) = bg {
            items.push(DisplayItem::RoundRect { color: bgc, rect: inner_rect, radii: inner });
        }
        items.push(DisplayItem::RoundRectRing {
            color: border_side_color(lb, "top"),
            outer: b,
            outer_radii: radii,
            inner: inner_rect,
            inner_radii: inner,
        });
        return;
    }
    // 라운드 + 배경(테두리 없음/비균일): 배경만 둥글게, 테두리는 사각으로
    if r > 0.0 && bg.is_some() {
        items.push(DisplayItem::RoundRect { color: bg.unwrap(), rect: b, radii });
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
    !matches!(border_side_style(lb, side).as_str(), "none" | "hidden")
}

// border-<side>-style (없으면 border-style, 없으면 none). solid/dashed/dotted/double 등.
fn border_side_style(lb: &LayoutBox, side: &str) -> String {
    match lb
        .styled_node
        .value(&format!("border-{}-style", side))
        .or_else(|| lb.styled_node.value("border-style"))
    {
        Some(Value::Keyword(k)) => k,
        _ => "none".to_string(),
    }
}

// 파선/점선 한 변을 세그먼트로 (수평이면 x축, 수직이면 y축을 따라). rect 는 변 전체 영역.
fn emit_dashed_side(items: &mut Vec<DisplayItem>, color: Color, rect: Rect, horizontal: bool, dotted: bool) {
    let thick = if horizontal { rect.height } else { rect.width };
    let (dash, gap) = if dotted { (thick, thick) } else { (thick * 3.0, thick * 2.0) };
    let period = (dash + gap).max(1.0);
    let len = if horizontal { rect.width } else { rect.height };
    let mut pos = 0.0;
    while pos < len {
        let seg = dash.min(len - pos);
        let r = if horizontal {
            Rect { x: rect.x + pos, y: rect.y, width: seg, height: rect.height }
        } else {
            Rect { x: rect.x, y: rect.y + pos, width: rect.width, height: seg }
        };
        items.push(DisplayItem::Rect { color, rect: r });
        pos += period;
    }
}

// 한 변을 스타일에 맞게 그린다 (solid=사각, dashed/dotted=세그먼트).
fn emit_border_side(items: &mut Vec<DisplayItem>, color: Color, rect: Rect, style: &str, horizontal: bool) {
    match style {
        "dashed" => emit_dashed_side(items, color, rect, horizontal, false),
        "dotted" => emit_dashed_side(items, color, rect, horizontal, true),
        _ => items.push(DisplayItem::Rect { color, rect }), // solid/double/기타 → 실선 근사
    }
}

pub fn build_display_list(root: &LayoutBox) -> Vec<DisplayItem> {
    // (스택 레벨, 아이템) 수집 후 레벨로 안정 정렬 → 높은 z-index 가 위에 그려짐.
    // 같은 레벨은 문서 순서 유지(안정 정렬). 정식 스태킹 컨텍스트의 근사.
    let mut buf: Vec<(i32, DisplayItem)> = Vec::new();
    collect_items(root, 0, None, None, None, &mut buf);
    buf.sort_by_key(|(z, _)| *z);
    buf.into_iter().map(|(_, it)| it).collect()
}

fn is_sticky(lb: &LayoutBox) -> bool {
    matches!(lb.styled_node.value("position"), Some(Value::Keyword(ref k)) if k == "sticky")
}

// positioned 요소의 z-index 를 서브트리 스택 레벨로 전파. static 은 부모 레벨 유지.
fn stack_level(lb: &LayoutBox, parent_z: i32) -> i32 {
    let positioned = matches!(lb.styled_node.value("position"),
        Some(Value::Keyword(ref k)) if k == "relative" || k == "absolute"
            || k == "fixed" || k == "sticky");
    if positioned {
        match lb.styled_node.value("z-index") {
            Some(Value::Length(n, _)) => return n as i32,
            Some(Value::Keyword(ref k)) => {
                if let Ok(n) = k.trim().parse::<i32>() {
                    return n;
                }
            }
            _ => {}
        }
    }
    parent_z
}

// overflow 가 hidden/clip/scroll/auto 면 자손을 이 박스로 클리핑.
fn overflow_clips(lb: &LayoutBox) -> bool {
    for prop in ["overflow", "overflow-x", "overflow-y"] {
        if let Some(Value::Keyword(k)) = lb.styled_node.value(prop) {
            if k == "hidden" || k == "clip" || k == "scroll" || k == "auto" {
                return true;
            }
        }
    }
    false
}

fn rect_intersect(a: Rect, b: Rect) -> Option<Rect> {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = (a.x + a.width).min(b.x + b.width);
    let y1 = (a.y + a.height).min(b.y + b.height);
    if x1 > x0 && y1 > y0 {
        Some(Rect { x: x0, y: y0, width: x1 - x0, height: y1 - y0 })
    } else {
        None
    }
}

// outer 가 inner 를 완전히 포함하는가.
fn rect_contains(outer: Rect, inner: Rect) -> bool {
    inner.x >= outer.x
        && inner.y >= outer.y
        && inner.x + inner.width <= outer.x + outer.width
        && inner.y + inner.height <= outer.y + outer.height
}

// 클립 사각형에 아이템을 맞춰 자른다. 사각형/이미지는 rect 교집합, 글리프/폴리곤은
// 경계에 걸치면 사각 클립(ClipShape)으로 감싸 픽셀 단위로 자른다. round_active=true
// 면 이미 바깥 둥근 Clipped 래퍼가 픽셀 클립을 하므로 이중 래핑을 피한다.
// clip=None 이면 그대로.
fn clip_apply(item: DisplayItem, clip: Option<Rect>, round_active: bool) -> Option<DisplayItem> {
    let Some(c) = clip else { return Some(item) };
    // 경계에 걸친 아이템을 사각 클립으로 감싼다(둥근 클립 활성 시엔 바깥 래퍼가 처리).
    let rect_clip = |it: DisplayItem| -> DisplayItem {
        if round_active {
            it
        } else {
            DisplayItem::Clipped {
                shape: ClipShape::RoundRect { rect: c, radii: [0.0; 4] },
                inner: Box::new(it),
            }
        }
    };
    match item {
        DisplayItem::Rect { color, rect } => {
            rect_intersect(rect, c).map(|r| DisplayItem::Rect { color, rect: r })
        }
        // 링은 outer 가 클립과 겹치면 유지(정밀 교차는 생략 — 테두리라 영향 미미)
        DisplayItem::RoundRectRing { color, outer, outer_radii, inner, inner_radii } => {
            if rect_intersect(outer, c).is_some() {
                Some(DisplayItem::RoundRectRing { color, outer, outer_radii, inner, inner_radii })
            } else {
                None
            }
        }
        DisplayItem::Image { image, rect, fit, pos, adj } => {
            rect_intersect(rect, c).map(|r| DisplayItem::Image { image, rect: r, fit, pos, adj })
        }
        DisplayItem::BackdropBlur { rect, radius } => {
            rect_intersect(rect, c).map(|r| DisplayItem::BackdropBlur { rect: r, radius })
        }
        // 그라디언트: 보이는 영역으로 rect 만 자르고 각도/스톱은 유지
        // (클립된 부분만 다시 계산 — overflow 클립 하의 그라디언트는 드묾, 근사).
        DisplayItem::Gradient { rect, angle, radial, circle, conic, repeating, stops } => {
            rect_intersect(rect, c).map(|r| DisplayItem::Gradient {
                rect: r,
                angle,
                radial,
                circle,
                conic,
                repeating,
                stops,
            })
        }
        // 캔버스 그라디언트: 보이는 영역으로 rect 만 자른다 (좌표계는 유지 —
        // 그라디언트 정의가 절대 좌표라 자른 뒤에도 색이 그대로여야 한다).
        DisplayItem::CanvasGradient { rect, kind, stops } => {
            rect_intersect(rect, c).map(|r| DisplayItem::CanvasGradient { rect: r, kind, stops })
        }
        DisplayItem::RoundRect { color, rect, radii } => {
            rect_intersect(rect, c).map(|r| DisplayItem::RoundRect { color, rect: r, radii })
        }
        DisplayItem::RawImage { rect, img, adj } => {
            rect_intersect(rect, c).map(|r| DisplayItem::RawImage { rect: r, img, adj })
        }
        DisplayItem::Polygon { color, contours } => {
            // bbox 로 컬링만 (윤곽 좌표는 유지)
            let (mut x0, mut y0, mut x1, mut y1) =
                (f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
            for ct in &contours {
                for &(x, y) in ct {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x);
                    y1 = y1.max(y);
                }
            }
            let bbox = Rect { x: x0, y: y0, width: x1 - x0, height: y1 - y0 };
            match rect_intersect(bbox, c) {
                None => None,
                Some(_) if rect_contains(c, bbox) => Some(DisplayItem::Polygon { color, contours }),
                Some(_) => Some(rect_clip(DisplayItem::Polygon { color, contours })),
            }
        }
        DisplayItem::Shadow { color, rect, radius, blur } => {
            // 그림자는 blur 만큼 넉넉히 — 경계 밖이면 컬링만
            let expanded = Rect {
                x: rect.x - blur,
                y: rect.y - blur,
                width: rect.width + 2.0 * blur,
                height: rect.height + 2.0 * blur,
            };
            rect_intersect(expanded, c).map(|_| DisplayItem::Shadow { color, rect, radius, blur })
        }
        DisplayItem::InnerShadow { color, rect, radius, blur, spread, dx, dy } => {
            // 박스 안에만 그려지므로 rect 로 컬링만 (SDF 파라미터 유지)
            rect_intersect(rect, c)
                .map(|_| DisplayItem::InnerShadow { color, rect, radius, blur, spread, dx, dy })
        }
        DisplayItem::Glyph(gi) => {
            // 글리프 대략 bbox: x..x+px, baseline 위 1.1em ~ 아래 0.4em
            let gbox = Rect {
                x: gi.x,
                y: gi.baseline_y - 1.1 * gi.px,
                width: gi.px,
                height: 1.5 * gi.px,
            };
            match rect_intersect(gbox, c) {
                None => None,
                // 완전히 안이면 그대로, 경계에 걸치면 픽셀 단위 사각 클립으로 감싼다.
                Some(_) if rect_contains(c, gbox) => Some(DisplayItem::Glyph(gi)),
                Some(_) => Some(rect_clip(DisplayItem::Glyph(gi))),
            }
        }
        // sticky 래퍼는 클립 전에 감싸지 않으므로 여기 도달 안 함 (exhaustive 용)
        sticky @ DisplayItem::Sticky { .. } => Some(sticky),
        clipped @ DisplayItem::Clipped { .. } => Some(clipped),
        layer @ DisplayItem::Layer { .. } => Some(layer),
        // 변환된 서브트리는 클립 사각형과 축이 다를 수 있다 — 그대로 둔다
        // (정확한 클립은 변환 후 마스크가 필요하다. 여기서 잘라내면 회전된 내용이 사라진다)
        t @ DisplayItem::Transform { .. } => Some(t),
        t @ DisplayItem::DropShadow { .. } => Some(t),
        t @ DisplayItem::Masked { .. } => Some(t),
    }
}

// clip-path: inset(t r b l [round …]) → 클립 사각형 (border box 기준). inset 만 지원
// (circle/ellipse/polygon 은 픽셀 마스크가 필요해 미지원 → 클립 안 함).
fn clip_path_rect(lb: &LayoutBox) -> Option<Rect> {
    let raw = match lb.styled_node.value("clip-path") {
        Some(Value::Keyword(s)) => s,
        _ => return None,
    };
    let inner = raw.trim().strip_prefix("inset(")?.strip_suffix(')')?;
    let inner = inner.split("round").next().unwrap_or(inner).trim();
    let b = lb.dimensions.border_box();
    let resolve = |t: &str, base: f32| -> Option<f32> {
        if let Some(p) = t.strip_suffix('%') {
            p.trim().parse::<f32>().ok().map(|v| v / 100.0 * base)
        } else {
            t.trim_end_matches("px").parse::<f32>().ok()
        }
    };
    let toks: Vec<&str> = inner.split_whitespace().collect();
    let (top, right, bottom, left) = match toks.len() {
        1 => (resolve(toks[0], b.height)?, resolve(toks[0], b.width)?, resolve(toks[0], b.height)?, resolve(toks[0], b.width)?),
        2 => {
            let tb = resolve(toks[0], b.height)?;
            let lr = resolve(toks[1], b.width)?;
            (tb, lr, tb, lr)
        }
        3 => (resolve(toks[0], b.height)?, resolve(toks[1], b.width)?, resolve(toks[2], b.height)?, resolve(toks[1], b.width)?),
        n if n >= 4 => (resolve(toks[0], b.height)?, resolve(toks[1], b.width)?, resolve(toks[2], b.height)?, resolve(toks[3], b.width)?),
        _ => return None,
    };
    Some(Rect {
        x: b.x + left,
        y: b.y + top,
        width: (b.width - left - right).max(0.0),
        height: (b.height - top - bottom).max(0.0),
    })
}

// backdrop-filter: blur(Npx) 의 반경(논리 px). blur 함수만 지원.
fn backdrop_blur_radius(lb: &LayoutBox) -> Option<f32> {
    let raw = match lb.styled_node.value("backdrop-filter") {
        Some(Value::Keyword(s)) => s,
        _ => return None,
    };
    let inner = raw.trim().strip_prefix("blur(")?.strip_suffix(')')?;
    inner.trim().trim_end_matches("px").trim().parse::<f32>().ok().filter(|&r| r > 0.0)
}

// 이 박스가 세우는 둥근 픽셀 마스크: clip-path circle()/ellipse()/inset(...round),
// 또는 overflow 클립 + border-radius(둥근 카드가 자식을 코너에서 자름).
fn round_clip_shape(lb: &LayoutBox) -> Option<ClipShape> {
    let b = lb.dimensions.border_box();
    if let Some(Value::Keyword(raw)) = lb.styled_node.value("clip-path") {
        let s = raw.trim();
        // 위치("at ...") 분리 후 반경 토큰 파싱
        let pos_center = |rest: &str| -> (f32, f32) {
            if let Some(at) = rest.find("at ") {
                let p = parse_bg_position(rest[at + 3..].trim());
                (b.x + p.0.resolve(b.width, 0.0), b.y + p.1.resolve(b.height, 0.0))
            } else {
                (b.x + b.width / 2.0, b.y + b.height / 2.0)
            }
        };
        let len = |t: &str, base: f32| -> Option<f32> {
            match t {
                "closest-side" => Some(b.width.min(b.height) / 2.0),
                "farthest-side" => Some(b.width.max(b.height) / 2.0),
                _ if t.ends_with('%') => t.trim_end_matches('%').parse::<f32>().ok().map(|v| v / 100.0 * base),
                _ => t.trim_end_matches("px").parse::<f32>().ok(),
            }
        };
        if let Some(inner) = s.strip_prefix("circle(").and_then(|x| x.strip_suffix(')')) {
            let (cx, cy) = pos_center(inner);
            let rpart = inner.split("at").next().unwrap_or("").trim();
            let r = if rpart.is_empty() {
                b.width.min(b.height) / 2.0
            } else {
                len(rpart, b.width.min(b.height))?
            };
            return Some(ClipShape::Ellipse { cx, cy, rx: r, ry: r });
        }
        if let Some(inner) = s.strip_prefix("ellipse(").and_then(|x| x.strip_suffix(')')) {
            let (cx, cy) = pos_center(inner);
            let radii: Vec<&str> = inner.split("at").next().unwrap_or("").split_whitespace().collect();
            let rx = radii.first().and_then(|t| len(t, b.width)).unwrap_or(b.width / 2.0);
            let ry = radii.get(1).and_then(|t| len(t, b.height)).unwrap_or(b.height / 2.0);
            return Some(ClipShape::Ellipse { cx, cy, rx, ry });
        }
        // polygon([fill-rule,]? x1 y1, x2 y2, ...): % 는 박스 크기 기준, 박스 원점 오프셋
        if let Some(inner) = s.strip_prefix("polygon(").and_then(|x| x.strip_suffix(')')) {
            let coord = |t: &str, base: f32| -> Option<f32> {
                if t.ends_with('%') {
                    t.trim_end_matches('%').parse::<f32>().ok().map(|v| v / 100.0 * base)
                } else {
                    t.trim_end_matches("px").parse::<f32>().ok()
                }
            };
            let mut segs: Vec<&str> = inner.split(',').map(|x| x.trim()).collect();
            if segs.first().map_or(false, |t| *t == "nonzero" || *t == "evenodd") {
                segs.remove(0);
            }
            let mut pts = Vec::new();
            for seg in segs {
                let mut it = seg.split_whitespace();
                if let (Some(xs), Some(ys)) = (it.next(), it.next()) {
                    if let (Some(px), Some(py)) = (coord(xs, b.width), coord(ys, b.height)) {
                        pts.push((b.x + px, b.y + py));
                    }
                }
            }
            if pts.len() >= 3 {
                return Some(ClipShape::Polygon(pts));
            }
        }
        // inset(... round R): 둥근 사각형
        if s.starts_with("inset(") && s.contains("round") {
            if let Some(rect) = clip_path_rect(lb) {
                let rpart = s.rsplit("round").next().unwrap_or("").trim_end_matches(')').trim();
                let r = len(rpart.split_whitespace().next().unwrap_or(""), rect.width.min(rect.height)).unwrap_or(0.0);
                return Some(ClipShape::RoundRect { rect, radii: [r; 4] });
            }
        }
    }
    // overflow 클립 + border-radius → 둥근 사각형으로 자손 코너 클립
    if overflow_clips(lb) {
        let radii = corner_radii(lb);
        if radii.iter().any(|&r| r > 0.0) {
            return Some(ClipShape::RoundRect { rect: lb.dimensions.padding_box(), radii });
        }
    }
    None
}

fn collect_items(
    layout_box: &LayoutBox,
    parent_z: i32,
    clip: Option<Rect>,
    round_clip: Option<ClipShape>,
    sticky: Option<(f32, f32)>,
    buf: &mut Vec<(i32, DisplayItem)>,
) {
    let z = stack_level(layout_box, parent_z);
    // 이 박스가 position:sticky 면 서브트리 sticky 파라미터 갱신 (top, 자연 y0)
    let sticky_here = if is_sticky(layout_box) {
        let top = match layout_box.styled_node.value("top") {
            Some(Value::Length(v, _)) => v,
            _ => 0.0,
        };
        Some((top, layout_box.dimensions.border_box().y))
    } else {
        sticky
    };
    // opacity: 이 서브트리(자신+자손)의 모든 아이템 알파에 곱해질 지점 표시.
    // 근사(그룹 합성 아님): 겹치는 자손은 개별 블렌드되지만 대다수 UI 엔 충분.
    let subtree_start = buf.len();
    // clip-path: 이 서브트리(자신+자손) 전체를 inset 사각형으로 클립 (기존 clip 과 교집합).
    let clip = match clip_path_rect(layout_box) {
        Some(cp) => match clip {
            Some(c) => rect_intersect(c, cp).or(Some(Rect::default())),
            None => Some(cp),
        },
        None => clip,
    };
    // 둥근 픽셀 마스크(둥근 overflow / clip-path circle·ellipse). 안쪽(자신) 우선.
    let round_clip = round_clip_shape(layout_box).or(round_clip);
    let mut local: Vec<DisplayItem> = Vec::new();
    // backdrop-filter: blur() — 배경(뒤 캔버스)을 이 박스 영역에서 흐린다. 배경 그리기 전에.
    if let Some(radius) = backdrop_blur_radius(layout_box) {
        local.push(DisplayItem::BackdropBlur { rect: layout_box.dimensions.border_box(), radius });
    }
    // 그림자 → 배경/테두리(border-radius 포함) → 안쪽그림자 → 배경이미지 → 이미지 → 글리프 → 장식
    emit_box_shadow(layout_box, &mut local);
    emit_box_decorations(layout_box, &mut local);
    // <select> 드롭다운 화살표는 배경/테두리 위에 그린다
    if matches!(layout_box.form_control, Some(crate::layout::FormControl::SelectArrow)) {
        emit_form_control(layout_box, crate::layout::FormControl::SelectArrow, &mut local);
    }
    emit_inner_shadow(layout_box, &mut local);
    emit_outline(layout_box, &mut local);
    emit_svg(layout_box, &mut local);
    if let Some(idx) = layout_box.background_image {
        // background-size: cover/contain 우선. 아니면 background-repeat 로 타일 여부 결정
        // (CSS 기본은 repeat → Tile; no-repeat 이면 Natural).
        let repeat = layout_box.styled_node.value("background-repeat");
        let (tile_x, tile_y) = match repeat {
            Some(Value::Keyword(ref k)) if k == "no-repeat" => (false, false),
            Some(Value::Keyword(ref k)) if k == "repeat-x" => (true, false),
            Some(Value::Keyword(ref k)) if k == "repeat-y" => (false, true),
            _ => (true, true), // 기본 repeat
        };
        let bg_size = match layout_box.styled_node.value("background-size") {
            Some(Value::Keyword(ref k)) => parse_bg_size(k),
            Some(Value::Length(v, crate::css::Unit::Px)) => Some((BgSize::Px(v), BgSize::Auto)),
            Some(Value::Length(v, crate::css::Unit::Percent)) => Some((BgSize::Pct(v / 100.0), BgSize::Auto)),
            _ => None,
        };
        let fit = match layout_box.styled_node.value("background-size") {
            Some(Value::Keyword(ref k)) if k == "cover" => ImageFit::Cover,
            Some(Value::Keyword(ref k)) if k == "contain" => ImageFit::Contain,
            _ => match bg_size {
                Some((w, h)) => ImageFit::Sized { w, h, tile_x, tile_y },
                None if !tile_x && !tile_y => ImageFit::Natural,
                None if tile_x && !tile_y => ImageFit::TileX,
                None if !tile_x && tile_y => ImageFit::TileY,
                None => ImageFit::Tile,
            },
        };
        let pos = layout_box.styled_node.value("background-position").and_then(|v| match v {
            Value::Keyword(s) => Some(parse_bg_position(&s)),
            _ => None,
        });
        local.push(DisplayItem::Image {
            image: idx,
            rect: layout_box.dimensions.border_box(),
            fit,
            pos,
            adj: ImgAdj::none(),
        });
    }
    if let Some(g) = &layout_box.gradient {
        local.push(DisplayItem::Gradient {
            rect: layout_box.dimensions.border_box(),
            angle: g.angle_deg,
            radial: g.radial,
            circle: g.circle,
            conic: g.conic,
            repeating: g.repeating,
            stops: g.stops.clone(),
        });
    }
    if let Some(idx) = layout_box.image {
        let fit = match layout_box.styled_node.value("object-fit") {
            Some(Value::Keyword(k)) => match k.as_str() {
                "contain" => ImageFit::Contain,
                "cover" => ImageFit::Cover,
                "none" => ImageFit::None,
                "scale-down" => ImageFit::Contain, // scale-down ≈ contain(축소만) 근사
                _ => ImageFit::Fill,
            },
            _ => ImageFit::Fill, // object-fit 기본값
        };
        let pos = layout_box.styled_node.value("object-position").and_then(|v| match v {
            Value::Keyword(s) => Some(parse_bg_position(&s)),
            _ => None,
        });
        local.push(DisplayItem::Image {
            image: idx,
            rect: layout_box.dimensions.content,
            fit,
            pos,
            adj: ImgAdj::none(),
        });
    }
    // text-shadow: 글리프 뒤에 오프셋+색으로 복제 (blur 미지원, 단일 그림자)
    let text_shadow = {
        let len = |n: &str| match layout_box.styled_node.value(n) {
            Some(Value::Length(v, crate::css::Unit::Px)) => Some(v),
            _ => None,
        };
        match (len("text-shadow-x"), len("text-shadow-y")) {
            (Some(dx), Some(dy)) => {
                let color = match layout_box.styled_node.value("text-shadow-color") {
                    Some(Value::Color(c)) => c,
                    _ => Color { r: 0, g: 0, b: 0, a: 128 },
                };
                Some((dx, dy, color))
            }
            _ => None,
        }
    };
    // 인라인 요소 배경(<mark> 등) — 글리프/장식보다 뒤에 칠한다
    for (rect, color) in &layout_box.inline_bgs {
        local.push(DisplayItem::Rect { color: *color, rect: *rect });
    }
    // 인라인 요소 테두리(태그/뱃지/kbd) — 4변을 얇은 사각으로. 글리프 위를 덮지 않게
    // 윤곽만 그린다 (배경 뒤, 글리프 앞). radius 는 근사(모서리 직각).
    for (rect, color, w, _radius) in &layout_box.inline_borders {
        let (x, y, bw, bh, t) = (rect.x, rect.y, rect.width, rect.height, w.max(1.0));
        local.push(DisplayItem::Rect { color: *color, rect: Rect { x, y, width: bw, height: t } });
        local.push(DisplayItem::Rect { color: *color, rect: Rect { x, y: y + bh - t, width: bw, height: t } });
        local.push(DisplayItem::Rect { color: *color, rect: Rect { x, y, width: t, height: bh } });
        local.push(DisplayItem::Rect { color: *color, rect: Rect { x: x + bw - t, y, width: t, height: bh } });
    }
    if let Some((dx, dy, color)) = text_shadow {
        for gi in &layout_box.glyphs {
            let mut sh = *gi;
            sh.x += dx;
            sh.baseline_y += dy;
            sh.color = color;
            local.push(DisplayItem::Glyph(sh));
        }
    }
    for gi in &layout_box.glyphs {
        local.push(DisplayItem::Glyph(*gi));
    }
    for (rect, color) in &layout_box.decorations {
        local.push(DisplayItem::Rect { color: *color, rect: *rect });
    }
    // visibility: hidden/collapse — 이 박스 자신의 아이템은 그리지 않는다(자식은 상속으로
    // 함께 숨거나 visible 로 재정의 가능하므로 각자 판단). 공간은 유지.
    if matches!(layout_box.styled_node.value("visibility"),
        Some(Value::Keyword(ref k)) if k == "hidden" || k == "collapse")
    {
        local.clear();
    }
    // 이 박스 자신의 아이템은 부모 클립으로 자르고, 둥근 클립·sticky 면 래핑
    for it in local {
        if let Some(clipped) = clip_apply(it, clip, round_clip.is_some()) {
            let masked = match &round_clip {
                Some(shape) => DisplayItem::Clipped { shape: shape.clone(), inner: Box::new(clipped) },
                None => clipped,
            };
            let final_it = match sticky_here {
                Some((top, y0)) => DisplayItem::Sticky { top, y0, inner: Box::new(masked) },
                None => masked,
            };
            buf.push((z, final_it));
        }
    }
    // 자손 클립: overflow 면 이 박스 padding box 와 교집합
    let child_clip = if overflow_clips(layout_box) {
        let pad = layout_box.dimensions.padding_box();
        match clip {
            Some(c) => rect_intersect(c, pad).or(Some(Rect::default())),
            None => Some(pad),
        }
    } else {
        clip
    };
    for child in &layout_box.children {
        collect_items(child, z, child_clip, round_clip.clone(), sticky_here, buf);
    }
    // (transform 은 아래에서 DisplayItem::Transform 으로 서브트리 전체를 변환한다.
    //  예전엔 여기서 rotate 만 개별 아이템에 적용하고 그라디언트/이미지/그림자는
    //  "중심만 근사" 했다 — 회전된 그림자가 회전하지 않는 식으로 조용히 틀렸다.)
    // filter: 서브트리 아이템 색을 함수 체인으로 변환 (grayscale/brightness/invert/sepia/contrast/saturate/hue-rotate).
    if let Some(Value::Keyword(f)) = layout_box.styled_node.value("filter") {
        let funcs = parse_filters(&f);
        if !funcs.is_empty() {
            for (_, item) in buf[subtree_start..].iter_mut() {
                filter_item(item, &funcs);
            }
        }
        // blur(N): 색변환 뒤, 서브트리 콘텐츠 영역을 흐린다 (불투명 요소 근사; 반경만큼 번짐).
        if let Some(radius) = funcs.iter().find(|(n, _)| n == "blur").map(|(_, r)| *r) {
            if radius > 0.0 {
                let b = layout_box.dimensions.border_box();
                let ex = Rect {
                    x: b.x - radius,
                    y: b.y - radius,
                    width: b.width + 2.0 * radius,
                    height: b.height + 2.0 * radius,
                };
                let maxz = buf[subtree_start..].iter().map(|(zz, _)| *zz).max().unwrap_or(z);
                buf.push((maxz, DisplayItem::BackdropBlur { rect: ex, radius }));
            }
        }
        // drop-shadow(dx dy [blur] [color]): 서브트리를 하나의 그룹으로 묶어 실루엣 그림자를
        // 만든다. box-shadow 와 달리 **그려진 모양**(투명 배경의 아이콘 등)을 따라간다.
        for ds in parse_drop_shadows(&f) {
            let items: Vec<DisplayItem> =
                buf.split_off(subtree_start).into_iter().map(|(_, it)| it).collect();
            buf.push((
                z,
                DisplayItem::DropShadow {
                    dx: ds.0,
                    dy: ds.1,
                    blur: ds.2,
                    color: ds.3,
                    items,
                },
            ));
        }
    }
    // mask-image: 서브트리를 마스크의 알파로 곱한다 (CSS Masking §3).
    {
        let mask = layout_box
            .styled_node
            .value("mask-image")
            .or_else(|| layout_box.styled_node.value("-webkit-mask-image"));
        match mask {
            Some(Value::Gradient(g)) => {
                let items: Vec<DisplayItem> =
                    buf.split_off(subtree_start).into_iter().map(|(_, it)| it).collect();
                buf.push((
                    z,
                    DisplayItem::Masked {
                        rect: layout_box.dimensions.border_box(),
                        gradient: Some(g),
                        image: None,
                        items,
                    },
                ));
            }
            Some(Value::Url(_)) => {
                if let Some(idx) = layout_box.mask_image {
                    let items: Vec<DisplayItem> =
                        buf.split_off(subtree_start).into_iter().map(|(_, it)| it).collect();
                    buf.push((
                        z,
                        DisplayItem::Masked {
                            rect: layout_box.dimensions.border_box(),
                            gradient: None,
                            image: Some(idx),
                            items,
                        },
                    ));
                }
            }
            _ => {}
        }
    }
    // 그룹 opacity / mix-blend-mode → 오프스크린 레이어로 서브트리를 격리 합성.
    // (겹치는 반투명 자손이 이중 블렌드되지 않고, 그룹 blend 가 정확해진다.)
    let op = element_opacity(layout_box).unwrap_or(1.0);
    let blend = match layout_box.styled_node.value("mix-blend-mode") {
        Some(Value::Keyword(m)) => parse_blend_mode(&m).unwrap_or(BlendMode::Normal),
        _ => BlendMode::Normal,
    };
    if op < 1.0 || blend != BlendMode::Normal {
        let mut sub: Vec<(i32, DisplayItem)> = buf.drain(subtree_start..).collect();
        sub.sort_by(|a, b| a.0.cmp(&b.0)); // 레이어 내부 z 순서 (안정 정렬)
        let items: Vec<DisplayItem> = sub.into_iter().map(|(_, it)| it).collect();
        buf.push((z, DisplayItem::Layer { opacity: op, blend, items }));
    }
    // CSS transform — 서브트리 전체(자신 + 자손)를 행렬로 변환한다.
    // opacity 레이어보다 바깥이다: 먼저 격리 합성하고, 그 결과를 변환한다.
    if let Some(m) = layout_box.transform {
        let mut sub: Vec<(i32, DisplayItem)> = buf.drain(subtree_start..).collect();
        sub.sort_by(|a, b| a.0.cmp(&b.0));
        let items: Vec<DisplayItem> = sub.into_iter().map(|(_, it)| it).collect();
        if !items.is_empty() {
            buf.push((z, DisplayItem::Transform { m, items }));
        }
    }
}

// filter 함수 목록 파싱 → (이름, 강도 0..) 벡터. 퍼센트는 0..1, 무단위 수 그대로.
fn parse_filters(text: &str) -> Vec<(String, f32)> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('(') {
        let name = rest[..open].trim().rsplit(|c: char| c.is_whitespace()).next().unwrap_or("").to_ascii_lowercase();
        let Some(close) = rest[open..].find(')') else { break };
        let close = close + open;
        let arg = rest[open + 1..close].trim();
        let amt = if let Some(p) = arg.strip_suffix('%') {
            p.trim().parse::<f32>().ok().map(|v| v / 100.0)
        } else if let Some(p) = arg.strip_suffix("deg") {
            p.trim().parse::<f32>().ok()
        } else {
            arg.parse::<f32>().ok()
        };
        if let Some(a) = amt {
            out.push((name, a));
        } else if !name.is_empty() {
            out.push((name, 1.0)); // 인자 없는 경우 기본 강도
        }
        rest = &rest[close + 1..];
    }
    out
}

// filter 문자열에서 drop-shadow(...) 들을 뽑는다 → (dx, dy, blur, color)
fn parse_drop_shadows(f: &str) -> Vec<(f32, f32, f32, Color)> {
    let mut out = Vec::new();
    let lower = f.to_ascii_lowercase();
    let mut rest = lower.as_str();
    let mut base = 0usize;
    while let Some(i) = rest.find("drop-shadow(") {
        let start = base + i + "drop-shadow(".len();
        // 괄호 균형 (색 함수 rgba(...) 가 안에 올 수 있다)
        let bytes = f.as_bytes();
        let mut depth = 1i32;
        let mut j = start;
        while j < bytes.len() {
            match bytes[j] {
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            j += 1;
        }
        if j >= bytes.len() {
            break;
        }
        let args = &f[start..j];
        let mut lens: Vec<f32> = Vec::new();
        let mut color = Color { r: 0, g: 0, b: 0, a: 255 };
        for tok in split_top_level_ws(args) {
            let t = tok.trim();
            if let Some(px) = t.strip_suffix("px").and_then(|n| n.trim().parse::<f32>().ok()) {
                lens.push(px);
            } else if let Ok(v) = t.parse::<f32>() {
                lens.push(v);
            } else if let Some(c) = crate::css::parse_color(t) {
                color = c;
            }
        }
        if lens.len() >= 2 {
            out.push((lens[0], lens[1], lens.get(2).copied().unwrap_or(0.0), color));
        }
        base = j + 1;
        rest = &lower[base.min(lower.len())..];
    }
    out
}

// 괄호를 존중하며 공백으로 토큰 분리 (rgba(1, 2, 3) 을 한 토큰으로)
fn split_top_level_ws(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    for c in s.chars() {
        match c {
            '(' => {
                depth += 1;
                cur.push(c);
            }
            ')' => {
                depth -= 1;
                cur.push(c);
            }
            c if c.is_whitespace() && depth == 0 => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

// 색에 filter 함수 체인을 적용.
fn apply_filters(c: Color, funcs: &[(String, f32)]) -> Color {
    let (mut r, mut g, mut b) = (c.r as f32, c.g as f32, c.b as f32);
    for (name, amt) in funcs {
        match name.as_str() {
            "grayscale" => {
                let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
                r += (luma - r) * amt;
                g += (luma - g) * amt;
                b += (luma - b) * amt;
            }
            "brightness" => {
                r *= amt;
                g *= amt;
                b *= amt;
            }
            "invert" => {
                r += (255.0 - 2.0 * r) * amt;
                g += (255.0 - 2.0 * g) * amt;
                b += (255.0 - 2.0 * b) * amt;
            }
            "contrast" => {
                r = (r - 128.0) * amt + 128.0;
                g = (g - 128.0) * amt + 128.0;
                b = (b - 128.0) * amt + 128.0;
            }
            "sepia" => {
                let (nr, ng, nb) = (
                    0.393 * r + 0.769 * g + 0.189 * b,
                    0.349 * r + 0.686 * g + 0.168 * b,
                    0.272 * r + 0.534 * g + 0.131 * b,
                );
                r += (nr - r) * amt;
                g += (ng - g) * amt;
                b += (nb - b) * amt;
            }
            "saturate" => {
                let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
                r = luma + (r - luma) * amt;
                g = luma + (g - luma) * amt;
                b = luma + (b - luma) * amt;
            }
            "hue-rotate" => {
                let rad = amt * std::f32::consts::PI / 180.0;
                let (co, si) = (rad.cos(), rad.sin());
                // 휘도 보존 색상 회전 행렬 (SVG feColorMatrix hueRotate)
                let m = [
                    0.213 + co * 0.787 - si * 0.213,
                    0.715 - co * 0.715 - si * 0.715,
                    0.072 - co * 0.072 + si * 0.928,
                    0.213 - co * 0.213 + si * 0.143,
                    0.715 + co * 0.285 + si * 0.140,
                    0.072 - co * 0.072 - si * 0.283,
                    0.213 - co * 0.213 - si * 0.787,
                    0.715 - co * 0.715 + si * 0.715,
                    0.072 + co * 0.928 + si * 0.072,
                ];
                let (nr, ng, nb) = (
                    r * m[0] + g * m[1] + b * m[2],
                    r * m[3] + g * m[4] + b * m[5],
                    r * m[6] + g * m[7] + b * m[8],
                );
                r = nr;
                g = ng;
                b = nb;
            }
            _ => {} // blur/drop-shadow 등은 별도 (색변환 아님)
        }
    }
    let clamp = |v: f32| v.clamp(0.0, 255.0) as u8;
    Color { r: clamp(r), g: clamp(g), b: clamp(b), a: c.a }
}

// 디스플레이 아이템의 색들에 filter 적용. opacity 함수는 알파에.
fn filter_item(item: &mut DisplayItem, funcs: &[(String, f32)]) {
    // opacity(n) filter 는 알파 스케일로 처리
    for (name, amt) in funcs {
        if name == "opacity" {
            scale_item_alpha(item, *amt);
        }
    }
    match item {
        DisplayItem::Rect { color, .. }
        | DisplayItem::RoundRect { color, .. }
        | DisplayItem::RoundRectRing { color, .. }
        | DisplayItem::Shadow { color, .. }
        | DisplayItem::InnerShadow { color, .. }
        | DisplayItem::Polygon { color, .. } => *color = apply_filters(*color, funcs),
        DisplayItem::Glyph(gi) => gi.color = apply_filters(gi.color, funcs),
        DisplayItem::Gradient { stops, .. } => {
            for (c, _) in stops.iter_mut() {
                *c = apply_filters(*c, funcs);
            }
        }
        DisplayItem::CanvasGradient { stops, .. } => {
            for (c, _) in stops.iter_mut() {
                *c = apply_filters(*c, funcs);
            }
        }
        // 이미지는 픽셀마다 적용해야 한다 — 여기서는 보정만 실어 둔다 (blit 시 적용).
        DisplayItem::Image { adj, .. } | DisplayItem::RawImage { adj, .. } => {
            adj.filters.extend(funcs.iter().filter(|(n, _)| n != "opacity" && n != "blur").cloned());
        }
        DisplayItem::Sticky { inner, .. } => filter_item(inner, funcs),
        DisplayItem::Clipped { inner, .. } => filter_item(inner, funcs),
        DisplayItem::Layer { items, .. }
        | DisplayItem::DropShadow { items, .. }
        | DisplayItem::Masked { items, .. } => {
            items.iter_mut().for_each(|it| filter_item(it, funcs))
        }
        DisplayItem::Transform { items, .. } => {
            items.iter_mut().for_each(|it| filter_item(it, funcs))
        }
        DisplayItem::BackdropBlur { .. } => {}
    }
}

fn element_opacity(lb: &LayoutBox) -> Option<f32> {
    match lb.styled_node.value("opacity") {
        Some(Value::Length(op, _)) if op < 1.0 => Some(op.max(0.0)),
        _ => None,
    }
}

// 디스플레이 아이템의 색 알파에 factor(0..1)를 곱한다 (이미지는 근사로 스킵).
fn scale_item_alpha(item: &mut DisplayItem, f: f32) {
    let s = |a: u8| (a as f32 * f).round().clamp(0.0, 255.0) as u8;
    match item {
        DisplayItem::RawImage { adj, .. } => adj.alpha *= f,
        DisplayItem::Transform { items, .. }
        | DisplayItem::DropShadow { items, .. }
        | DisplayItem::Masked { items, .. } => {
            items.iter_mut().for_each(|it| scale_item_alpha(it, f))
        }
        DisplayItem::CanvasGradient { stops, .. } => {
            for (c, _) in stops.iter_mut() {
                c.a = s(c.a);
            }
        }
        DisplayItem::Rect { color, .. } => color.a = s(color.a),
        DisplayItem::RoundRect { color, .. } => color.a = s(color.a),
        DisplayItem::RoundRectRing { color, .. } => color.a = s(color.a),
        DisplayItem::Shadow { color, .. } => color.a = s(color.a),
        DisplayItem::InnerShadow { color, .. } => color.a = s(color.a),
        DisplayItem::Glyph(gi) => gi.color.a = s(gi.color.a),
        DisplayItem::Gradient { stops, .. } => {
            for (c, _) in stops.iter_mut() {
                c.a = s(c.a);
            }
        }
        DisplayItem::Polygon { color, .. } => color.a = s(color.a),
        DisplayItem::Image { adj, .. } => adj.alpha *= f,
        DisplayItem::Sticky { inner, .. } => scale_item_alpha(inner, f),
        DisplayItem::Clipped { inner, .. } => scale_item_alpha(inner, f),
        DisplayItem::Layer { opacity, .. } => *opacity *= f,
        DisplayItem::BackdropBlur { .. } => {}
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
    let dbg = std::env::var("KESTREL_PAINT_DEBUG").is_ok();
    for item in items {
        if dbg {
            match item {
                DisplayItem::Rect { color, rect } => eprintln!(
                    "[paint] Rect  ({:.0},{:.0} {:.0}x{:.0}) rgba({},{},{},{})",
                    rect.x, rect.y, rect.width, rect.height, color.r, color.g, color.b, color.a
                ),
                DisplayItem::Image { image, rect, .. } => eprintln!(
                    "[paint] Image#{} ({:.0},{:.0} {:.0}x{:.0})",
                    image, rect.x, rect.y, rect.width, rect.height
                ),
                _ => {}
            }
        }
        draw_item(&mut canvas, item, scroll_y, scale, vh, fonts, cache, images);
    }
    canvas
}

#[allow(clippy::too_many_arguments)]
fn draw_item(
    canvas: &mut Canvas,
    item: &DisplayItem,
    scroll_y: f32,
    scale: f32,
    vh: f32,
    fonts: &FontStack,
    cache: &mut GlyphCache,
    images: &[crate::png::Image],
) {
    let scale_rect = |rect: &Rect| Rect {
        x: rect.x * scale,
        y: (rect.y - scroll_y) * scale,
        width: rect.width * scale,
        height: rect.height * scale,
    };
    match item {
        DisplayItem::Rect { color, rect } => {
            let r = scale_rect(rect);
            if r.y + r.height < 0.0 || r.y > vh {
                return;
            }
            canvas.fill_rect(*color, r);
        }
        DisplayItem::RoundRect { color, rect, radii } => {
            let r = scale_rect(rect);
            if r.y + r.height < 0.0 || r.y > vh {
                return;
            }
            canvas.fill_round_rect4(*color, r, radii.map(|c| c * scale));
        }
        DisplayItem::RoundRectRing { color, outer, outer_radii, inner, inner_radii } => {
            let o = scale_rect(outer);
            if o.y + o.height < 0.0 || o.y > vh {
                return;
            }
            let i = scale_rect(inner);
            canvas.fill_round_rect_ring(
                *color,
                o,
                outer_radii.map(|c| c * scale),
                i,
                inner_radii.map(|c| c * scale),
            );
        }
        DisplayItem::Shadow { color, rect, radius, blur } => {
            let r = scale_rect(rect);
            let m = blur * scale;
            if r.y + r.height + m < 0.0 || r.y - m > vh {
                return;
            }
            canvas.fill_soft_round_rect(*color, r, radius * scale, blur * scale);
        }
        DisplayItem::InnerShadow { color, rect, radius, blur, spread, dx, dy } => {
            let r = scale_rect(rect);
            if r.y + r.height < 0.0 || r.y > vh {
                return;
            }
            canvas.fill_inner_shadow(
                *color,
                r,
                radius * scale,
                blur * scale,
                spread * scale,
                dx * scale,
                dy * scale,
            );
        }
        DisplayItem::Image { image, rect, fit, pos, adj } => {
            let r = scale_rect(rect);
            if r.y + r.height < 0.0 || r.y > vh {
                return;
            }
            if let Some(img) = images.get(*image) {
                blit_image(canvas, img, r, scale, *fit, *pos, adj);
            }
        }
        DisplayItem::Gradient { rect, angle, radial, circle, conic, repeating, stops } => {
            let r = scale_rect(rect);
            if r.y + r.height < 0.0 || r.y > vh {
                return;
            }
            canvas.fill_gradient(r, *angle, *radial, *circle, *conic, *repeating, stops, scale);
        }
        DisplayItem::RawImage { rect, img, adj } => {
            let r = scale_rect(rect);
            if r.y + r.height < 0.0 || r.y > vh {
                return;
            }
            blit_image(canvas, img, r, scale, ImageFit::Fill, None, adj);
        }
        // 캔버스 그라디언트: 픽셀마다 t 를 구해 색을 정한다. 모양은 바깥의 Clipped 가 자른다.
        DisplayItem::CanvasGradient { rect, kind, stops } => {
            let r = scale_rect(rect);
            if r.y + r.height < 0.0 || r.y > vh {
                return;
            }
            let x0 = r.x.max(0.0) as usize;
            let y0 = r.y.max(0.0) as usize;
            let x1 = (r.x + r.width).min(canvas.width as f32).max(0.0) as usize;
            let y1 = (r.y + r.height).min(canvas.height as f32).max(0.0) as usize;
            for py in y0..y1 {
                for px in x0..x1 {
                    // 논리(캔버스) 좌표로 되돌려 그라디언트 파라미터와 같은 계에서 계산
                    let lx = (px as f32 + 0.5) / scale;
                    let ly = (py as f32 + 0.5) / scale + scroll_y;
                    let t = match kind {
                        CanvasGrad::Linear { x0: gx0, y0: gy0, x1: gx1, y1: gy1 } => {
                            let (dx, dy) = (gx1 - gx0, gy1 - gy0);
                            let len2 = dx * dx + dy * dy;
                            if len2 <= 0.0 {
                                0.0
                            } else {
                                ((lx - gx0) * dx + (ly - gy0) * dy) / len2
                            }
                        }
                        // 방사: 두 원 사이의 보간 (표준은 원뿔 방정식이지만, 중심이 같은
                        // 흔한 경우엔 반지름 비로 정확하다)
                        CanvasGrad::Radial { x0: gx0, y0: gy0, r0, x1: gx1, y1: gy1, r1 } => {
                            let d = ((lx - gx1).powi(2) + (ly - gy1).powi(2)).sqrt();
                            let _ = (gx0, gy0);
                            if (r1 - r0).abs() < 0.001 {
                                0.0
                            } else {
                                (d - r0) / (r1 - r0)
                            }
                        }
                    };
                    let color = gradient_color_at(stops, t.clamp(0.0, 1.0));
                    if color.a > 0 {
                        canvas.put(px, py, color, color.a);
                    }
                }
            }
        }
        DisplayItem::Polygon { color, contours } => {            let scaled: Vec<Vec<(f32, f32)>> = contours
                .iter()
                .map(|ct| ct.iter().map(|&(x, y)| (x * scale, (y - scroll_y) * scale)).collect())
                .collect();
            canvas.fill_polygon(*color, &scaled);
        }
        DisplayItem::Glyph(gi) => {
            let baseline = (gi.baseline_y - scroll_y) * scale;
            let px = gi.px * scale;
            // 대략적 글리프 세로 범위로 컬링 (ascent ~1.2em, descent ~0.4em)
            if baseline + 0.4 * px < 0.0 || baseline - 1.2 * px > vh {
                return;
            }
            let shifted = GlyphInstance { x: gi.x * scale, baseline_y: baseline, px, ..*gi };
            let bm = cache.get(fonts, gi.font_index, gi.glyph_id, px, gi.bold, gi.italic);
            blit_glyph(canvas, bm, &shifted);
        }
        DisplayItem::Sticky { top, y0, inner } => {
            // 스크롤이 요소 위쪽을 지나가면 뷰포트 top 에 고정 (dy 만큼 아래로 유지).
            let dy = (scroll_y + top - y0).max(0.0);
            draw_item(canvas, inner, scroll_y - dy, scale, vh, fonts, cache, images);
        }
        DisplayItem::Clipped { shape, inner } => {
            // 논리 좌표 shape 를 물리 좌표로 변환해 활성 클립으로 설정, inner 그린 뒤 복원.
            let phys = match shape {
                ClipShape::RoundRect { rect, radii } => {
                    ClipShape::RoundRect { rect: scale_rect(rect), radii: radii.map(|r| r * scale) }
                }
                ClipShape::Ellipse { cx, cy, rx, ry } => ClipShape::Ellipse {
                    cx: cx * scale,
                    cy: (cy - scroll_y) * scale,
                    rx: rx * scale,
                    ry: ry * scale,
                },
                ClipShape::Polygon(pts) => ClipShape::Polygon(
                    pts.iter().map(|&(x, y)| (x * scale, (y - scroll_y) * scale)).collect(),
                ),
            };
            let prev = canvas.clip.take();
            canvas.clip = Some(phys);
            draw_item(canvas, inner, scroll_y, scale, vh, fonts, cache, images);
            canvas.clip = prev;
        }
        DisplayItem::BackdropBlur { rect, radius } => {
            let r = scale_rect(rect);
            if r.y + r.height < 0.0 || r.y > vh {
                return;
            }
            canvas.blur_region(r, radius * scale);
        }
        // mask-image: 서브트리를 그린 뒤 마스크의 **알파**로 곱한다 (CSS Masking §3).
        DisplayItem::Masked { rect, gradient, image, items } => {
            let mut layer = Canvas::new_layer(canvas.width, canvas.height);
            for it in items {
                draw_item(&mut layer, it, scroll_y, scale, vh, fonts, cache, images);
            }
            // 마스크를 별도 레이어에 그린다 (알파만 쓴다)
            let r = scale_rect(rect);
            let mut mask = Canvas::new_layer(canvas.width, canvas.height);
            if let Some(g) = gradient {
                mask.fill_gradient(
                    r,
                    g.angle_deg,
                    g.radial,
                    g.circle,
                    g.conic,
                    g.repeating,
                    &g.stops,
                    scale,
                );
            } else if let Some(idx) = image {
                if let Some(img) = images.get(*idx) {
                    blit_image(&mut mask, img, r, scale, ImageFit::Fill, None, &ImgAdj::none());
                }
            }
            for py in 0..canvas.height {
                for px in 0..canvas.width {
                    let lp = layer.pixels[py * canvas.width + px];
                    if lp.a == 0 {
                        continue;
                    }
                    let m = mask.pixels[py * canvas.width + px].a;
                    let a = ((lp.a as u32 * m as u32) / 255) as u8;
                    if a > 0 {
                        canvas.put(px, py, lp, a);
                    }
                }
            }
        }
        DisplayItem::DropShadow { dx, dy, blur, color, items } => {
            // 1) 서브트리를 격리 레이어에 그린다
            let mut layer = Canvas::new_layer(canvas.width, canvas.height);
            for it in items {
                draw_item(&mut layer, it, scroll_y, scale, vh, fonts, cache, images);
            }
            // 2) 알파 실루엣을 오프셋해 그림자 색으로 칠한다
            let (odx, ody) = ((dx * scale).round() as i32, (dy * scale).round() as i32);
            let mut shadow = Canvas::new_layer(canvas.width, canvas.height);
            let (mut minx, mut miny, mut maxx, mut maxy) =
                (canvas.width as i32, canvas.height as i32, 0i32, 0i32);
            for py in 0..canvas.height as i32 {
                for px in 0..canvas.width as i32 {
                    let lp = layer.pixels[py as usize * canvas.width + px as usize];
                    if lp.a == 0 {
                        continue;
                    }
                    let (tx, ty) = (px + odx, py + ody);
                    if tx < 0 || ty < 0 || tx >= canvas.width as i32 || ty >= canvas.height as i32 {
                        continue;
                    }
                    let a = ((lp.a as f32 / 255.0) * (color.a as f32 / 255.0) * 255.0) as u8;
                    shadow.put(tx as usize, ty as usize, *color, a);
                    minx = minx.min(tx);
                    miny = miny.min(ty);
                    maxx = maxx.max(tx);
                    maxy = maxy.max(ty);
                }
            }
            if maxx >= minx && *blur > 0.0 {
                let m = (blur * scale).ceil() as i32 + 2;
                let r = Rect {
                    x: (minx - m).max(0) as f32,
                    y: (miny - m).max(0) as f32,
                    width: (maxx - minx + 2 * m) as f32,
                    height: (maxy - miny + 2 * m) as f32,
                };
                shadow.blur_region(r, blur * scale);
            }
            // 3) 그림자 → 원본 순으로 합성
            for py in 0..canvas.height {
                for px in 0..canvas.width {
                    let sp = shadow.pixels[py * canvas.width + px];
                    if sp.a > 0 {
                        canvas.put(px, py, sp, sp.a);
                    }
                }
            }
            for py in 0..canvas.height {
                for px in 0..canvas.width {
                    let lp = layer.pixels[py * canvas.width + px];
                    if lp.a > 0 {
                        canvas.put(px, py, lp, lp.a);
                    }
                }
            }
        }
        DisplayItem::Layer { opacity, blend, items } => {
            // 서브트리를 투명 레이어에 격리 렌더 → opacity/blend 로 한 번에 합성
            let mut layer = Canvas::new_layer(canvas.width, canvas.height);
            for it in items {
                draw_item(&mut layer, it, scroll_y, scale, vh, fonts, cache, images);
            }
            let prev = canvas.blend_mode;
            canvas.blend_mode = *blend;
            for py in 0..canvas.height {
                for px in 0..canvas.width {
                    let lp = layer.pixels[py * canvas.width + px];
                    if lp.a == 0 {
                        continue;
                    }
                    let a = ((lp.a as f32 / 255.0) * opacity * 255.0).round() as u8;
                    canvas.put(px, py, lp, a);
                }
            }
            canvas.blend_mode = prev;
        }
        // CSS transform: 서브트리를 격리 렌더한 뒤 역행렬로 매핑한다.
        // (GPU 합성기가 변환 레이어를 처리하는 방식과 같다 — 글자·이미지·그림자가
        //  전부 함께 변환된다. 개별 프리미티브를 밀고 늘리는 방식으로는 회전을 표현할 수 없다.)
        DisplayItem::Transform { m, items } => {
            let mut layer = Canvas::new_layer(canvas.width, canvas.height);
            for it in items {
                draw_item(&mut layer, it, scroll_y, scale, vh, fonts, cache, images);
            }
            // 논리(CSS) 좌표 투영행렬 m 을 장치(캔버스) 좌표 행렬 F 로 옮긴다.
            //   장치: X = s·x ,  Y = s·(y - k)   (s=scale, k=scroll)
            //   F = S ∘ m ∘ S⁻¹   (S 는 스케일+스크롤 아핀)
            let (s, k) = (scale, scroll_y);
            let s_mat = crate::layout::Mat3 {
                m: [[s, 0.0, 0.0], [0.0, s, -s * k], [0.0, 0.0, 1.0]],
            };
            let s_inv = crate::layout::Mat3 {
                m: [[1.0 / s, 0.0, 0.0], [0.0, 1.0 / s, k], [0.0, 0.0, 1.0]],
            };
            // S⁻¹ 먼저, 그다음 m, 그다음 S
            let fwd = s_inv.then(m).then(&s_mat);
            let Some(inv) = fwd.invert() else { return };
            // 대상 픽셀마다 역매핑해 레이어를 이중선형 샘플 (프리멀티플라이로 가장자리 보존)
            for py in 0..canvas.height {
                for px in 0..canvas.width {
                    let (sx, sy) = inv.apply(px as f32 + 0.5, py as f32 + 0.5);
                    let (sx, sy) = (sx - 0.5, sy - 0.5);
                    if sx < -1.0
                        || sy < -1.0
                        || sx > layer.width as f32
                        || sy > layer.height as f32
                    {
                        continue;
                    }
                    let x0 = sx.floor();
                    let y0 = sy.floor();
                    let fx = sx - x0;
                    let fy = sy - y0;
                    let (x0, y0) = (x0 as i32, y0 as i32);
                    let mut acc = [0.0f32; 4]; // 프리멀티플라이 rgba
                    for (dx, dy, w) in [
                        (0, 0, (1.0 - fx) * (1.0 - fy)),
                        (1, 0, fx * (1.0 - fy)),
                        (0, 1, (1.0 - fx) * fy),
                        (1, 1, fx * fy),
                    ] {
                        let (x, y) = (x0 + dx, y0 + dy);
                        if x < 0 || y < 0 || x >= layer.width as i32 || y >= layer.height as i32 {
                            continue;
                        }
                        let p = layer.pixels[y as usize * layer.width + x as usize];
                        let a = p.a as f32 / 255.0;
                        acc[0] += p.r as f32 * a * w;
                        acc[1] += p.g as f32 * a * w;
                        acc[2] += p.b as f32 * a * w;
                        acc[3] += a * w;
                    }
                    if acc[3] <= 0.001 {
                        continue;
                    }
                    let col = Color {
                        r: (acc[0] / acc[3]).round().clamp(0.0, 255.0) as u8,
                        g: (acc[1] / acc[3]).round().clamp(0.0, 255.0) as u8,
                        b: (acc[2] / acc[3]).round().clamp(0.0, 255.0) as u8,
                        a: 255,
                    };
                    let alpha = (acc[3] * 255.0).round().clamp(0.0, 255.0) as u8;
                    canvas.put(px, py, col, alpha);
                }
            }
        }
    }
}

// rect(물리 px) 좌상단에 이미지를 scale 배로 그린다 (최근접 샘플링).
// rect 크기로 클리핑 (<img> 는 rect == 고유 크기 × scale 이라 무손실).
// rect(물리 px)에 이미지를 fit 방식으로 그린다. 목적지 하위영역 `dr` 을 구하고,
// dr 안 각 픽셀의 상대 위치로 소스 픽셀을 샘플(최근접), rect 로 클립한다.
#[allow(clippy::too_many_arguments)]
fn blit_image(
    canvas: &mut Canvas,
    img: &crate::png::Image,
    rect: Rect,
    scale: f32,
    fit: ImageFit,
    pos: Option<(BgCoord, BgCoord)>,
    adj: &ImgAdj,
) {
    let (iw, ih) = (img.width as f32, img.height as f32);
    if iw <= 0.0 || ih <= 0.0 || rect.width <= 0.0 || rect.height <= 0.0 {
        return;
    }
    // background-position 오프셋은 타일/단일 경로가 각자 실제 그림 크기 기준으로 푼다.
    // 타일링(background-repeat) + background-size. 타일 한 장의 크기를 먼저 정하고,
    // 축별 반복 여부에 따라 rect 안을 채운다 (반복 안 하는 축은 한 장만 그리고 클립).
    if matches!(fit, ImageFit::Tile | ImageFit::TileX | ImageFit::TileY | ImageFit::Sized { .. }) {
        let (tile_x, tile_y) = match fit {
            ImageFit::Tile => (true, true),
            ImageFit::TileX => (true, false),
            ImageFit::TileY => (false, true),
            ImageFit::Sized { tile_x, tile_y, .. } => (tile_x, tile_y),
            _ => (false, false),
        };
        // background-size 로 크기가 지정되면 그 크기가 타일 크기다. auto 축은 종횡비 유지.
        let (tw, th) = match fit {
            ImageFit::Sized { w, h, .. } => {
                let res = |s: BgSize, base: f32| match s {
                    BgSize::Auto => f32::NAN,
                    BgSize::Px(v) => v * scale, // CSS px → 장치 px
                    BgSize::Pct(p) => base * p, // 배경 배치 영역 기준 (이미 장치 px)
                };
                let (mut w, mut h) = (res(w, rect.width), res(h, rect.height));
                if w.is_nan() && h.is_nan() {
                    w = iw * scale;
                    h = ih * scale;
                } else if w.is_nan() {
                    w = h * iw / ih;
                } else if h.is_nan() {
                    h = w * ih / iw;
                }
                (w, h)
            }
            _ => (iw * scale, ih * scale),
        };
        if tw <= 0.0 || th <= 0.0 {
            return;
        }
        // background-position 은 실제 타일 크기 기준으로 다시 푼다 (퍼센트 정렬은 타일 크기에 의존).
        let off_x = pos.map_or(0.0, |(px, _)| px.resolve(rect.width, tw));
        let off_y = pos.map_or(0.0, |(_, py)| py.resolve(rect.height, th));
        let x0 = rect.x.max(0.0) as usize;
        let y0 = rect.y.max(0.0) as usize;
        let x1 = ((rect.x + rect.width).min(canvas.width as f32)).max(0.0) as usize;
        let y1 = ((rect.y + rect.height).min(canvas.height as f32)).max(0.0) as usize;
        for py in y0..y1 {
            let ty = (py as f32 + 0.5 - rect.y - off_y) / th;
            if !tile_y && ty >= 1.0 {
                continue; // 세로 미반복: 한 이미지 높이까지만
            }
            let fy = if tile_y { ty - ty.floor() } else { ty };
            let sy = ((fy * ih) as i32).clamp(0, img.height as i32 - 1) as usize;
            for px in x0..x1 {
                let tx = (px as f32 + 0.5 - rect.x - off_x) / tw;
                if !tile_x && tx >= 1.0 {
                    continue;
                }
                let fx = if tile_x { tx - tx.floor() } else { tx };
                let sx = ((fx * iw) as i32).clamp(0, img.width as i32 - 1) as usize;
                let s = (sy * img.width + sx) * 4;
                let alpha = img.rgba[s + 3];
                if alpha == 0 {
                    continue;
                }
                let fg = Color { r: img.rgba[s], g: img.rgba[s + 1], b: img.rgba[s + 2], a: 255 };
                let (fg, alpha) = adj.apply(fg, alpha);
                canvas.put(px, py, fg, alpha);
            }
        }
        return;
    }
    let (cx, cy) = (rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
    // 그려질 목적지 사각형 dr (이미지 전체가 매핑되는 영역; rect 밖은 클립)
    let mut dr = match fit {
        ImageFit::Fill | ImageFit::Tile | ImageFit::TileX | ImageFit::TileY | ImageFit::Sized { .. } => rect,
        ImageFit::Contain => {
            let s = (rect.width / iw).min(rect.height / ih);
            let (w, h) = (iw * s, ih * s);
            Rect { x: cx - w / 2.0, y: cy - h / 2.0, width: w, height: h }
        }
        ImageFit::Cover => {
            let s = (rect.width / iw).max(rect.height / ih);
            let (w, h) = (iw * s, ih * s);
            Rect { x: cx - w / 2.0, y: cy - h / 2.0, width: w, height: h }
        }
        ImageFit::None => {
            let (w, h) = (iw * scale, ih * scale);
            Rect { x: cx - w / 2.0, y: cy - h / 2.0, width: w, height: h }
        }
        ImageFit::Natural => {
            Rect { x: rect.x, y: rect.y, width: iw * scale, height: ih * scale }
        }
    };
    // background-position / object-position: 지정 시 중앙 정렬 대신 위치 오프셋으로 배치.
    // (Fill 은 dr=rect 라 오프셋 0 → 무영향.)
    if let Some((px, py)) = pos {
        dr.x = rect.x + px.resolve(rect.width, dr.width);
        dr.y = rect.y + py.resolve(rect.height, dr.height);
    }
    // 실제로 칠할 영역 = dr ∩ rect ∩ 캔버스
    let x0 = dr.x.max(rect.x).max(0.0) as usize;
    let y0 = dr.y.max(rect.y).max(0.0) as usize;
    let x1 = (dr.x + dr.width).min(rect.x + rect.width).min(canvas.width as f32).max(0.0) as usize;
    let y1 = (dr.y + dr.height).min(rect.y + rect.height).min(canvas.height as f32).max(0.0) as usize;
    for py in y0..y1 {
        // 목적지 픽셀 중심 → 소스 텍셀 좌표. -0.5 로 텍셀 중심 정렬(바이리니어 기준).
        let syf = (py as f32 + 0.5 - dr.y) / dr.height * ih - 0.5;
        for px in x0..x1 {
            let sxf = (px as f32 + 0.5 - dr.x) / dr.width * iw - 0.5;
            let (r, g, b, alpha) = sample_bilinear(img, sxf, syf);
            if alpha == 0 {
                continue;
            }
            let (c, alpha) = adj.apply(Color { r, g, b, a: 255 }, alpha);
            canvas.put(px, py, c, alpha);
        }
    }
}

// 바이리니어 샘플링(프리멀티플라이 보간). u,v 는 소스 텍셀 좌표(중심 정렬). 투명 픽셀의
// 색이 새지 않도록 rgb 를 alpha 가중해 섞고 다시 나눈다. (image-rendering: auto = smooth)
fn sample_bilinear(img: &crate::png::Image, u: f32, v: f32) -> (u8, u8, u8, u8) {
    let (iw, ih) = (img.width as i32, img.height as i32);
    let x0 = u.floor() as i32;
    let y0 = v.floor() as i32;
    let tx = u - x0 as f32;
    let ty = v - y0 as f32;
    let at = |x: i32, y: i32| -> (f32, f32, f32, f32) {
        let xc = x.clamp(0, iw - 1) as usize;
        let yc = y.clamp(0, ih - 1) as usize;
        let s = (yc * img.width + xc) * 4;
        let a = img.rgba[s + 3] as f32 / 255.0;
        // 프리멀티플라이: rgb × a
        (img.rgba[s] as f32 * a, img.rgba[s + 1] as f32 * a, img.rgba[s + 2] as f32 * a, a)
    };
    let (c00, c10, c01, c11) = (at(x0, y0), at(x0 + 1, y0), at(x0, y0 + 1), at(x0 + 1, y0 + 1));
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let mix = |p: (f32, f32, f32, f32), q: (f32, f32, f32, f32), t: f32| {
        (lerp(p.0, q.0, t), lerp(p.1, q.1, t), lerp(p.2, q.2, t), lerp(p.3, q.3, t))
    };
    let top = mix(c00, c10, tx);
    let bot = mix(c01, c11, tx);
    let (pr, pg, pb, pa) = mix(top, bot, ty);
    if pa <= 0.0 {
        return (0, 0, 0, 0);
    }
    // 언프리멀티플라이: 보간된 rgb 를 alpha 로 되돌린다.
    let a = pa.clamp(0.0, 1.0);
    let unpm = |c: f32| (c / pa).clamp(0.0, 255.0).round() as u8;
    (unpm(pr), unpm(pg), unpm(pb), (a * 255.0).round() as u8)
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
                rot: 0.0,
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

    #[test]
    fn svg_rasterizes_including_groups() {
        // CSS background-image: url(*.svg) 는 실제 픽셀이 필요하다 (DOM 치환이 안 된다).
        // 게다가 예전 emit_svg 는 <svg> 의 직계 자식만 봐서 <g> 로 감싼 도형 —
        // 즉 실제 SVG 대부분 — 이 통째로 안 그려졌다.
        let src = r##"<svg width="20" height="20" viewBox="0 0 20 20">
            <g><rect x="0" y="0" width="20" height="10" fill="#ff0000"/></g>
            <rect x="0" y="10" width="20" height="10" fill="#0000ff"/>
        </svg>"##;
        assert_eq!(svg_natural_size(src), (20, 20));
        let fs = FontStack::new(Vec::new());
        let img = rasterize_svg(src, 20, 20, &fs).expect("래스터화");
        assert_eq!((img.width, img.height), (20, 20));
        let at = |x: usize, y: usize| {
            let o = (y * img.width + x) * 4;
            (img.rgba[o], img.rgba[o + 1], img.rgba[o + 2], img.rgba[o + 3])
        };
        assert_eq!(at(10, 5), (255, 0, 0, 255), "<g> 안의 빨강 사각");
        assert_eq!(at(10, 15), (0, 0, 255, 255), "직계 파랑 사각");
        // viewBox 스케일: 40x40 으로 키워도 같은 색 배치
        let big = rasterize_svg(src, 40, 40, &fs).expect("래스터화");
        let o = (10 * 40 + 20) * 4;
        assert_eq!(big.rgba[o], 255, "확대해도 위쪽은 빨강");
    }
    // filter/opacity 가 **이미지에도** 걸린다. 예전엔 이미지만 조용히 건너뛰어
    // filter: grayscale(1) 인 로고가 컬러로 나왔다 (흔한 패턴이다).
    // SVG 아이콘 세트는 전부 <svg fill="none" stroke="…" stroke-width="2"> 형태다.
    // 표현 속성이 **상속되지 않으면** path 가 기본값(검정 채우기)으로 그려져
    // 얇은 윤곽선 아이콘이 시커먼 덩어리가 된다. 게다가 stroke 자체가 없으면
    // 아이콘이 **통째로 안 보인다**.
    #[test]
    fn svg_stroke_icon_renders_as_outline_not_blob() {
        let src = r##"<svg width="40" height="40" viewBox="0 0 40 40" fill="none" stroke="#ff0000" stroke-width="4">
            <rect x="8" y="8" width="24" height="24"/>
        </svg>"##;
        let fs = FontStack::new(Vec::new());
        let img = rasterize_svg(src, 40, 40, &fs).expect("래스터화");
        let at = |x: usize, y: usize| {
            let o = (y * 40 + x) * 4;
            (img.rgba[o], img.rgba[o + 1], img.rgba[o + 2], img.rgba[o + 3])
        };
        // 테두리 위 (y=8) 는 빨강, 안쪽 (20,20) 은 **비어 있어야** 한다
        assert_eq!(at(20, 8).0, 255, "윤곽선이 그려져야 (stroke 상속)");
        assert_eq!(at(20, 8).3, 255);
        assert_eq!(at(20, 20).3, 0, "fill=none 이 상속돼 안쪽은 비어야 (덩어리가 되면 안 된다)");
    }

    #[test]
    fn image_filters_apply_per_pixel() {
        let red = crate::png::Image {
            width: 2,
            height: 2,
            rgba: vec![255, 0, 0, 255].repeat(4),
        };
        let rect = Rect { x: 0.0, y: 0.0, width: 2.0, height: 2.0 };
        let px = |adj: ImgAdj| {
            let mut canvas = Canvas::new_layer(2, 2);
            blit_image(&mut canvas, &red, rect, 1.0, ImageFit::Fill, None, &adj);
            let p = canvas.pixels[0];
            (p.r, p.g, p.b, p.a)
        };
        assert_eq!(px(ImgAdj::none()), (255, 0, 0, 255), "보정 없으면 그대로");
        // 회색조: 순수 빨강의 휘도 = 0.2126 × 255 ≈ 54
        let gray = px(ImgAdj { alpha: 1.0, filters: vec![("grayscale".into(), 1.0)] });
        assert_eq!(gray, (54, 54, 54, 255), "grayscale(1)");
        let inv = px(ImgAdj { alpha: 1.0, filters: vec![("invert".into(), 1.0)] });
        assert_eq!(inv, (0, 255, 255, 255), "invert(1)");
        let half = px(ImgAdj { alpha: 0.5, filters: vec![] });
        assert_eq!(half.3, 128, "opacity(0.5) 는 알파에");
    }

    #[test]
    fn background_size_parses_lengths_and_auto() {
        assert_eq!(parse_bg_size("50% 25%"), Some((BgSize::Pct(0.5), BgSize::Pct(0.25))));
        assert_eq!(parse_bg_size("40px auto"), Some((BgSize::Px(40.0), BgSize::Auto)));
        assert_eq!(parse_bg_size("auto"), Some((BgSize::Auto, BgSize::Auto)));
        // 한 값이면 세로는 auto (종횡비 유지)
        assert_eq!(parse_bg_size("20px"), Some((BgSize::Px(20.0), BgSize::Auto)));
        // cover/contain 은 여기서 다루지 않는다 (별도 fit)
        assert_eq!(parse_bg_size("cover"), None);
    }

    use crate::css::Color;

    #[test]
    fn bilinear_interpolates_between_texels() {
        // 2x1 이미지: 왼쪽 검정(0), 오른쪽 흰색(255). 텍셀 중심은 x=0,1.
        let img = crate::png::Image {
            width: 2,
            height: 1,
            rgba: vec![0, 0, 0, 255, 255, 255, 255, 255],
        };
        // 텍셀 중심 정확히 → 원색.
        assert_eq!(sample_bilinear(&img, 0.0, 0.0), (0, 0, 0, 255));
        assert_eq!(sample_bilinear(&img, 1.0, 0.0), (255, 255, 255, 255));
        // 두 텍셀 정중앙(x=0.5) → 회색(128 근처).
        let (r, _, _, a) = sample_bilinear(&img, 0.5, 0.0);
        assert_eq!(a, 255);
        assert!((r as i32 - 128).abs() <= 1, "중간값 회색 근처, 실제 {}", r);
        // 가장자리 밖은 클램프 → 원색 유지.
        assert_eq!(sample_bilinear(&img, -1.0, 0.0), (0, 0, 0, 255));
    }

    #[test]
    fn bilinear_premultiplies_transparent_edge() {
        // 왼쪽 완전투명(빨강이지만 a=0), 오른쪽 불투명 파랑. 투명쪽 빨강이 새면 안 됨.
        let img = crate::png::Image {
            width: 2,
            height: 1,
            rgba: vec![255, 0, 0, 0, 0, 0, 255, 255],
        };
        // 중앙: 색은 파랑에 가깝고 빨강 성분 0(프리멀티플라이 보간).
        let (r, _g, b, a) = sample_bilinear(&img, 0.5, 0.0);
        assert_eq!(r, 0, "투명 픽셀의 빨강이 새면 안 됨");
        assert!(b > 200, "파랑 유지");
        assert!((a as i32 - 128).abs() <= 1, "알파는 0~255 중간, 실제 {}", a);
    }

    #[test]
    fn bg_position_parse_and_resolve() {
        // center → 50%/50%; 박스 100, 이미지 20 → (100-20)*0.5 = 40
        let (x, y) = parse_bg_position("center");
        assert_eq!(x.resolve(100.0, 20.0), 40.0);
        assert_eq!(y.resolve(100.0, 20.0), 40.0);
        // right top → x=100%(=80), y=0
        let (x, y) = parse_bg_position("right top");
        assert_eq!(x.resolve(100.0, 20.0), 80.0);
        assert_eq!(y.resolve(100.0, 20.0), 0.0);
        // 픽셀 오프셋은 절대
        let (x, y) = parse_bg_position("10px 20px");
        assert_eq!(x.resolve(999.0, 999.0), 10.0);
        assert_eq!(y.resolve(999.0, 999.0), 20.0);
        // 순서 뒤바뀐 키워드(top left)도 축 인식
        let (x, y) = parse_bg_position("top left");
        assert_eq!(x.resolve(100.0, 20.0), 0.0);
        assert_eq!(y.resolve(100.0, 20.0), 0.0);
    }

    #[test]
    fn parse_multiple_box_shadows_test() {
        let sh = parse_box_shadows(
            "0 1px 2px rgba(0,0,0,0.1), 0 4px 8px 1px #333333, inset 0 0 5px red",
        );
        assert_eq!(sh.len(), 3, "그림자 3개");
        assert_eq!(sh[0].dy, 1.0);
        assert_eq!(sh[0].blur, 2.0);
        assert!(!sh[0].inset);
        assert_eq!(sh[1].dy, 4.0);
        assert_eq!(sh[1].blur, 8.0);
        assert_eq!(sh[1].spread, 1.0);
        assert!(sh[2].inset, "셋째는 inset");
        assert_eq!(sh[2].blur, 5.0);
    }

    #[test]
    fn group_opacity_no_double_blend_on_overlap() {
        // 부모 opacity:0.5, 겹치는 빨강 자식 둘 → 겹침 영역도 non-overlap 과 같은 분홍
        // (per-item 이면 겹침이 이중 블렌드로 더 진해짐. 그룹 레이어면 동일.)
        let c = canvas_for(
            "<div class=\"p\"><div class=\"a\"></div><div class=\"b\"></div></div>",
            "html,body{display:block} .p{display:block;position:relative;opacity:0.5} \
             .a{display:block;position:absolute;top:0;left:0;width:4px;height:4px;background-color:#ff0000} \
             .b{display:block;position:absolute;top:0;left:2px;width:4px;height:4px;background-color:#ff0000}",
            8.0,
            8.0,
        );
        let non = c.pixels[0]; // (0,0): a 만
        let ov = c.pixels[3]; // (3,0): a+b 겹침
        assert_eq!(non, ov, "그룹 opacity: 겹침도 동일 non={:?} ov={:?}", non, ov);
        assert!(non.r > 250 && (non.g as i32 - 128).abs() < 6, "분홍이어야 {:?}", non);
    }

    #[test]
    fn filter_saturate_and_hue_rotate() {
        let red = Color { r: 255, g: 0, b: 0, a: 255 };
        // saturate(0) → 회색 (빨강 휘도 ≈ 54, BT.709)
        let g = apply_filters(red, &[("saturate".to_string(), 0.0)]);
        assert!(g.r == g.g && g.g == g.b && (g.r as i32 - 54).abs() <= 3, "saturate(0)→회색 {:?}", g);
        // hue-rotate(120deg) 로 빨강 → 초록쪽
        let h = apply_filters(red, &[("hue-rotate".to_string(), 120.0)]);
        assert!(h.g > h.r && h.g > h.b, "hue-rotate(120) 빨강→초록쪽 {:?}", h);
    }

    #[test]
    fn mix_blend_mode_multiply() {
        // 노랑 배경 위 시안 박스 multiply → 초록 (255,255,0)×(0,255,255)=(0,255,0)
        let c = canvas_for(
            "<div class=\"bg\"><div class=\"fg\"></div></div>",
            "html,body{display:block} \
             .bg{display:block;width:4px;height:4px;background-color:#ffff00} \
             .fg{display:block;width:4px;height:4px;background-color:#00ffff;mix-blend-mode:multiply}",
            4.0,
            4.0,
        );
        let p = c.pixels[c.width + 1];
        assert!(p.r < 40 && p.g > 200 && p.b < 40, "multiply → 초록, 실제 {:?}", p);
    }

    #[test]
    fn rounded_overflow_clips_child_corner() {
        // 반경 20(=원) overflow:hidden 컨테이너 안 자식 배경이 코너에서 잘림
        let c = canvas_for(
            "<div class=\"card\"><div class=\"fill\"></div></div>",
            "html,body{display:block} \
             .card{display:block;width:40px;height:40px;border-radius:20px;overflow:hidden} \
             .fill{display:block;width:40px;height:40px;background-color:#ff0000}",
            40.0,
            40.0,
        );
        let red = Color { r: 255, g: 0, b: 0, a: 255 };
        let at = |x: usize, y: usize| c.pixels[y * c.width + x];
        assert_ne!(at(2, 2), red, "코너는 둥근 클립으로 잘림");
        assert_eq!(at(20, 20), red, "중앙은 채워짐");
    }

    #[test]
    fn clip_path_circle_masks_corners() {
        let c = canvas_for(
            "<div class=\"b\"></div>",
            "html,body{display:block} \
             .b{display:block;width:40px;height:40px;background-color:#0000ff;clip-path:circle(20px)}",
            40.0,
            40.0,
        );
        let blue = Color { r: 0, g: 0, b: 255, a: 255 };
        let at = |x: usize, y: usize| c.pixels[y * c.width + x];
        assert_ne!(at(2, 2), blue, "원 밖 코너 잘림");
        assert_eq!(at(20, 20), blue, "원 중앙 채워짐");
    }

    #[test]
    fn clip_path_polygon_triangle() {
        // polygon(50% 0, 100% 100%, 0 100%) → 위가 뾰족한 삼각형
        let c = canvas_for(
            "<div class=\"b\"></div>",
            "html,body{display:block} \
             .b{display:block;width:40px;height:40px;background-color:#008800;\
             clip-path:polygon(50% 0, 100% 100%, 0 100%)}",
            40.0,
            40.0,
        );
        let green = Color { r: 0, g: 136, b: 0, a: 255 };
        let at = |x: usize, y: usize| c.pixels[y * c.width + x];
        assert_ne!(at(2, 2), green, "좌상단 코너는 삼각형 밖");
        assert_eq!(at(20, 20), green, "중앙은 삼각형 안");
        assert_eq!(at(20, 37), green, "하단 중앙은 삼각형 안");
    }

    #[test]
    fn clip_path_inset_clips_subtree() {
        // 40x40 빨강 박스에 clip-path: inset(10px) → 중앙 20x20 만 남음
        let c = canvas_for(
            "<div class=\"b\"></div>",
            "html,body{display:block} .b{display:block;width:40px;height:40px;\
             background-color:#ff0000;clip-path:inset(10px)}",
            40.0,
            40.0,
        );
        let red = Color { r: 255, g: 0, b: 0, a: 255 };
        let at = |x: usize, y: usize| c.pixels[y * c.width + x];
        assert_ne!(at(5, 5), red, "좌상단 모서리 클립됨");
        assert_eq!(at(20, 20), red, "중앙 채워짐");
        assert_ne!(at(35, 35), red, "우하단 모서리 클립됨");
    }

    #[test]
    fn round_rect4_per_corner() {
        // TL 만 반경 8, 나머지 직각 → TL 코너만 비고 나머지 코너는 채워짐
        let mut c = Canvas::new(20, 20);
        let red = Color { r: 255, g: 0, b: 0, a: 255 };
        let white = Color { r: 255, g: 255, b: 255, a: 255 };
        c.fill_round_rect4(red, Rect { x: 0.0, y: 0.0, width: 20.0, height: 20.0 }, [8.0, 0.0, 0.0, 0.0]);
        assert_eq!(c.pixels[0], white, "TL(0,0) 둥글어 비어야");
        assert_eq!(c.pixels[19], red, "TR(19,0) 직각이라 채워짐");
        assert_eq!(c.pixels[19 * 20], red, "BL(0,19) 직각");
        assert_eq!(c.pixels[19 * 20 + 19], red, "BR(19,19) 직각");
    }

    #[test]
    fn bg_position_center_offsets_blit() {
        // 4x4 캔버스에 2x2 빨강 이미지 no-repeat, position center → 오프셋 (4-2)/2=1
        let img = crate::png::Image { width: 2, height: 2, rgba: [255, 0, 0, 255].repeat(4) };
        let mut canvas = Canvas::new(4, 4);
        let rect = Rect { x: 0.0, y: 0.0, width: 4.0, height: 4.0 };
        blit_image(
            &mut canvas,
            &img,
            rect,
            1.0,
            ImageFit::Natural,
            Some((BgCoord::Pct(0.5), BgCoord::Pct(0.5))),
            &ImgAdj::none(),
        );
        let red = Color { r: 255, g: 0, b: 0, a: 255 };
        let white = Color { r: 255, g: 255, b: 255, a: 255 };
        assert_eq!(canvas.pixels[0], white, "좌상단(0,0)은 비어있어야 (중앙 배치)");
        assert_eq!(canvas.pixels[1 * 4 + 1], red, "(1,1) 빨강");
        assert_eq!(canvas.pixels[2 * 4 + 2], red, "(2,2) 빨강");
        assert_eq!(canvas.pixels[3 * 4 + 3], white, "우하단(3,3)은 비어있어야");
    }

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
    fn dashed_border_has_gaps() {
        // border-top: dashed → 위 변에 빨강 대시와 빈 갭이 번갈아 (이전엔 전부 실선).
        let c = canvas_for(
            "<div></div>",
            "div { display: block; width: 40px; height: 4px; border-top: 4px dashed #ff0000; }",
            44.0,
            44.0,
        );
        let (mut red, mut gap) = (0, 0);
        for x in 0..40 {
            let p = c.pixels[44 + x]; // y=1
            if p.r > 150 && p.g < 100 {
                red += 1;
            } else {
                gap += 1;
            }
        }
        assert!(red > 3 && gap > 3, "파선은 빨강 대시+갭 둘 다: red={} gap={}", red, gap);
    }

    #[test]
    fn rounded_transparent_border_has_round_corners() {
        // border-radius + border + 투명 배경 → 링으로 그려 모서리가 둥글다(각지지 않음).
        // 이전엔 배경 없으면 사각 테두리(고스트/아웃라인 버튼 각짐).
        let c = canvas_for(
            "<div></div>",
            "div { display: block; width: 20px; height: 20px; \
             border: 3px solid #ff0000; border-radius: 8px; }",
            20.0,
            20.0,
        );
        let px = |x: usize, y: usize| c.pixels[y * 20 + x];
        // 모서리(0,0)는 둥근 경계 밖 → 배경(밝음, 빨강 아님)
        let corner = px(0, 0);
        assert!(corner.r > 200 && corner.g > 150, "모서리는 테두리 밖(밝음): {:?}", corner);
        // 위 변 중앙(10,1)은 빨강 테두리
        let top_mid = px(10, 1);
        assert!(top_mid.r > 150 && top_mid.g < 120, "위 변 중앙은 빨강 테두리: {:?}", top_mid);
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
    fn opacity_fades_background_toward_white() {
        // opacity: 0.5 인 빨강 박스를 흰 배경 위에 → 분홍(약 255,128,128)
        let canvas = canvas_for(
            "<div></div>",
            "div { display: block; width: 2px; height: 2px; background-color: #ff0000; opacity: 0.5; }",
            4.0,
            4.0,
        );
        let p = canvas.pixels[0];
        assert_eq!(p.r, 255, "빨강 채널은 유지");
        assert!((p.g as i32 - 128).abs() <= 2, "초록 ~128 (흰색과 블렌드), 실제 {}", p.g);
        assert!((p.b as i32 - 128).abs() <= 2, "파랑 ~128, 실제 {}", p.b);
    }

    #[test]
    fn opacity_multiplies_through_descendants() {
        // 부모 opacity:0.5, 자식 배경 검정 → 자식도 반투명 (약 128 회색)
        let canvas = canvas_for(
            "<div><span></span></div>",
            "div { display: block; opacity: 0.5; } \
             span { display: block; width: 2px; height: 2px; background-color: #000000; }",
            4.0,
            4.0,
        );
        let p = canvas.pixels[0];
        assert!((p.r as i32 - 128).abs() <= 2, "자손도 부모 opacity 적용, 실제 {}", p.r);
    }

    #[test]
    fn svg_rect_and_circle_render() {
        // 20x20 svg, viewBox 0 0 10 10. 왼쪽 절반 빨강 rect + 오른쪽 파랑 circle.
        let canvas = canvas_for(
            "<svg width=\"20\" height=\"20\" viewBox=\"0 0 10 10\">\
             <rect x=\"0\" y=\"0\" width=\"5\" height=\"10\" fill=\"#ff0000\"></rect>\
             <circle cx=\"7\" cy=\"5\" r=\"3\" fill=\"#0000ff\"></circle>\
             </svg>",
            "svg { display: block; }",
            20.0,
            20.0,
        );
        // rect: viewBox x 0..5 → 박스 0..10. (2,10) 은 빨강
        assert_eq!(canvas.pixels[10 * 20 + 2], Color { r: 255, g: 0, b: 0, a: 255 }, "rect 빨강");
        // circle 중심 cx=7,cy=5 → 박스 (14,10). 파랑
        let c = canvas.pixels[10 * 20 + 14];
        assert!(c.b > 200 && c.r < 60, "circle 파랑, 실제 {:?}", c);
    }

    #[test]
    fn background_tile_fills_box() {
        // 1x1 빨강 이미지를 4x4 rect 에 Tile → 전체가 빨강
        let img = crate::png::Image { width: 1, height: 1, rgba: vec![255, 0, 0, 255] };
        let mut canvas = Canvas::new(4, 4);
        blit_image(&mut canvas, &img, Rect { x: 0.0, y: 0.0, width: 4.0, height: 4.0 }, 1.0, ImageFit::Tile, None, &ImgAdj::none());
        let red = Color { r: 255, g: 0, b: 0, a: 255 };
        assert_eq!(canvas.pixels[0], red);
        assert_eq!(canvas.pixels[3 * 4 + 3], red, "우하단도 타일로 채워짐");
        // TileX: 세로는 한 줄만 (1px 높이) → (0,2)는 안 칠해짐(흰색)
        let mut c2 = Canvas::new(4, 4);
        blit_image(&mut c2, &img, Rect { x: 0.0, y: 0.0, width: 4.0, height: 4.0 }, 1.0, ImageFit::TileX, None, &ImgAdj::none());
        assert_eq!(c2.pixels[0], red, "가로 타일 첫 줄");
        assert_eq!(c2.pixels[2 * 4 + 0], Color { r: 255, g: 255, b: 255, a: 255 }, "TileX 는 세로 미반복");
    }

    #[test]
    fn visibility_hidden_not_painted() {
        // visibility: hidden 박스의 배경은 안 그려짐 (공간은 유지 → 흰색)
        let canvas = canvas_for(
            "<div></div>",
            "div { display: block; width: 2px; height: 2px; background-color: #ff0000; visibility: hidden; }",
            4.0,
            4.0,
        );
        assert_eq!(canvas.pixels[0], Color { r: 255, g: 255, b: 255, a: 255 }, "숨긴 박스는 안 칠해짐");
    }

    #[test]
    fn filter_grayscale_and_invert() {
        // grayscale(100%) 빨강 → 회색 (BT.709: r=g=b=luma≈54)
        let gray = canvas_for(
            "<div></div>",
            "div { display: block; width: 2px; height: 2px; background-color: #ff0000; filter: grayscale(100%); }",
            4.0,
            4.0,
        );
        let p = gray.pixels[0];
        assert_eq!(p.r, p.g);
        assert_eq!(p.g, p.b);
        assert!((p.r as i32 - 54).abs() <= 2, "빨강 luma ~54(709), 실제 {}", p.r);
        // invert(100%) 검정 → 흰색
        let inv = canvas_for(
            "<div></div>",
            "div { display: block; width: 2px; height: 2px; background-color: #000000; filter: invert(100%); }",
            4.0,
            4.0,
        );
        assert_eq!(inv.pixels[0], Color { r: 255, g: 255, b: 255, a: 255 });
    }

    #[test]
    fn transform_rotate_makes_diamond() {
        // 10x10 박스를 (5,5)에 두고 45° 회전 → 다이아몬드. 중심은 채워지고
        // 원래 좌상 모서리(6,6)는 회전 다이아 밖(x+y<13)이라 비어야 한다.
        let canvas = canvas_for(
            "<div></div>",
            "div { display: block; width: 10px; height: 10px; margin: 5px; \
             background-color: #ff0000; transform: rotate(45deg); }",
            20.0,
            20.0,
        );
        let red = Color { r: 255, g: 0, b: 0, a: 255 };
        assert_eq!(canvas.pixels[10 * 20 + 10], red, "중심은 채워짐");
        // (4,4) 는 회전 다이아 밖(x+y=9 < 좌상 엣지 ~13) → 흰색
        assert_eq!(canvas.pixels[4 * 20 + 4], Color { r: 255, g: 255, b: 255, a: 255 }, "회전으로 좌상 모서리는 비어야");
    }

    #[test]
    fn svg_path_triangle_fills() {
        // viewBox 0 0 10 10, 삼각형 path (0,0)-(10,0)-(0,10) 채움 → 좌상 삼각형 안이 초록
        let canvas = canvas_for(
            "<svg width=\"20\" height=\"20\" viewBox=\"0 0 10 10\">\
             <path d=\"M0 0 L10 0 L0 10 Z\" fill=\"#00ff00\"></path>\
             </svg>",
            "svg { display: block; }",
            20.0,
            20.0,
        );
        // (2,2) 는 삼각형 안 (좌상) → 초록
        let inside = canvas.pixels[2 * 20 + 2];
        assert!(inside.g > 200 && inside.r < 60, "삼각형 안 초록, 실제 {:?}", inside);
        // (18,18) 우하단은 삼각형 밖 → 흰색
        assert_eq!(canvas.pixels[18 * 20 + 18], Color { r: 255, g: 255, b: 255, a: 255 }, "밖은 흰색");
    }

    #[test]
    fn outline_draws_ring_outside_box() {
        // 10x10 박스를 (5,5)에 두고 2px 빨강 outline → 박스 왼쪽 바깥(3,10)이 빨강
        let canvas = canvas_for(
            "<div></div>",
            "div { display: block; width: 10px; height: 10px; margin: 5px; \
             background-color: #ffffff; outline: 2px solid #ff0000; }",
            20.0,
            20.0,
        );
        let red = Color { r: 255, g: 0, b: 0, a: 255 };
        // 박스는 x=5..15, y=5..15. outline 은 x=3..5 (왼쪽 띠). (3,10) 은 빨강
        assert_eq!(canvas.pixels[10 * 20 + 3], red, "왼쪽 outline 띠");
        assert_eq!(canvas.pixels[3 * 20 + 10], red, "위쪽 outline 띠");
    }

    #[test]
    fn transparent_background_does_not_paint_black() {
        // rgba(0,0,0,0) 은 완전 투명 — 흰 배경 그대로여야 (검정 박스 버그 회귀 방지)
        let canvas = canvas_for(
            "<div></div>",
            "div { display: block; width: 2px; height: 2px; background-color: rgba(0,0,0,0); }",
            4.0,
            4.0,
        );
        assert_eq!(canvas.pixels[0], Color { r: 255, g: 255, b: 255, a: 255 });
    }

    #[test]
    fn semitransparent_background_blends() {
        // rgba(0,0,0,0.5) 를 흰 위에 → 회색(약 128)
        let canvas = canvas_for(
            "<div></div>",
            "div { display: block; width: 2px; height: 2px; background-color: rgba(0,0,0,0.5); }",
            4.0,
            4.0,
        );
        let p = canvas.pixels[0];
        assert!((p.r as i32 - 128).abs() <= 2, "기대 ~128, 실제 {}", p.r);
        assert_eq!(p.r, p.g);
        assert_eq!(p.g, p.b);
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

    // 스톱 위치 해석: px 는 그라디언트 선 길이 기준, auto 는 이웃 사이 균등 분배.
    // 예전엔 % 만 읽고 **px 는 통째로 무시**했다 (균등 분배로 뭉개졌다) —
    // linear-gradient(#fff 0, #fff 10px, transparent 10px) 처럼 흔한 패턴이 조용히 틀렸다.
    // repeating-linear-gradient: 스톱 구간이 반복돼야 한다.
    // 예전엔 함수 이름을 몰라 값 파싱이 실패했고 → **배경이 통째로 안 그려졌다**.
    #[test]
    fn repeating_gradient_tiles_the_stop_range() {
        use crate::css::StopPos;
        let red = Color { r: 255, g: 0, b: 0, a: 255 };
        let blue = Color { r: 0, g: 0, b: 255, a: 255 };
        // 90deg(오른쪽), 0-10px 빨강 / 10-20px 파랑 이 반복
        let stops = vec![
            (red, StopPos::Px(0.0)),
            (red, StopPos::Px(10.0)),
            (blue, StopPos::Px(10.0)),
            (blue, StopPos::Px(20.0)),
        ];
        let mut c = Canvas::new(40, 1);
        c.fill_gradient(
            Rect { x: 0.0, y: 0.0, width: 40.0, height: 1.0 },
            90.0,
            false,
            false,
            false,
            true,
            &stops,
            1.0,
        );
        let px = |x: usize| c.pixels[x];
        assert_eq!(px(5).r, 255, "0-10px 빨강");
        assert_eq!(px(15).b, 255, "10-20px 파랑");
        assert_eq!(px(25).r, 255, "20-30px 다시 빨강 (반복)");
        assert_eq!(px(35).b, 255, "30-40px 다시 파랑");
    }

    // inset box-shadow 의 spread — 예전엔 inset 만 롱핸드를 읽어서 spread 를 무시했다
    // (box-shadow: inset 0 0 0 10px c 같은 "안쪽 링"이 아예 안 나왔다).
    #[test]
    fn inset_shadow_spread_draws_inner_ring() {
        let green = Color { r: 0, g: 255, b: 0, a: 255 };
        // 투명 레이어에 그려야 "중앙이 비었는가"를 알파로 볼 수 있다
        let mut c = Canvas::new_layer(40, 40);
        c.fill_inner_shadow(
            green,
            Rect { x: 0.0, y: 0.0, width: 40.0, height: 40.0 },
            0.0,
            0.0,  // blur 0
            10.0, // spread 10 → 두께 10 의 안쪽 링
            0.0,
            0.0,
        );
        assert_eq!(c.pixels[20 * 40 + 5].g, 255, "가장자리 5px 는 링 안");
        assert_eq!(c.pixels[20 * 40 + 20].a, 0, "중앙은 비어 있어야");
    }

    #[test]
    fn stop_positions_resolve_px_and_auto() {
        use crate::css::StopPos;
        let c = Color { r: 0, g: 0, b: 0, a: 255 };
        // px: 선 길이 100 → 10px = 0.1
        let r = resolve_stops(&[(c, StopPos::Px(0.0)), (c, StopPos::Px(10.0))], 100.0);
        assert_eq!(r[0].1, 0.0);
        assert!((r[1].1 - 0.1).abs() < 1e-6, "10px/100 = 0.1, 실제 {}", r[1].1);
        // auto: 양끝 0/1, 중간은 균등
        let r = resolve_stops(&[(c, StopPos::Auto), (c, StopPos::Auto), (c, StopPos::Auto)], 100.0);
        assert_eq!(r.iter().map(|(_, p)| *p).collect::<Vec<_>>(), vec![0.0, 0.5, 1.0]);
        // 단조 증가 보정: 앞보다 작은 위치는 앞 값으로 끌어올린다 (표준)
        let r = resolve_stops(&[(c, StopPos::Pct(0.5)), (c, StopPos::Pct(0.2))], 100.0);
        assert_eq!(r[1].1, 0.5);
    }

    #[test]
    fn gradient_color_interpolates_between_stops() {
        let black = Color { r: 0, g: 0, b: 0, a: 255 };
        let white = Color { r: 255, g: 255, b: 255, a: 255 };
        let stops = vec![(black, 0.0f32), (white, 1.0f32)];
        assert_eq!(gradient_color_at(&stops, 0.0), black);
        assert_eq!(gradient_color_at(&stops, 1.0), white);
        let mid = gradient_color_at(&stops, 0.5);
        assert!((mid.r as i32 - 128).abs() <= 1, "중간은 ~128, 실제 {}", mid.r);
        // 범위 밖은 양끝으로 클램프
        assert_eq!(gradient_color_at(&stops, -1.0), black);
        assert_eq!(gradient_color_at(&stops, 2.0), white);
    }

    #[test]
    fn gradient_fade_to_transparent_premultiplied() {
        // red → transparent 중간점: premultiplied 보간이면 rgb 는 빨강 유지, a≈127.
        // (straight 보간이면 r≈127 로 탁해짐)
        let stops = vec![
            (Color { r: 255, g: 0, b: 0, a: 255 }, 0.0),
            (Color { r: 0, g: 0, b: 0, a: 0 }, 1.0),
        ];
        let mid = gradient_color_at(&stops, 0.5);
        assert!(mid.r > 240, "프리멀티 보간: 중간점도 빨강 유지 (r={})", mid.r);
        assert!((mid.a as i32 - 127).abs() <= 2, "알파 ~127 (a={})", mid.a);
    }

    #[test]
    fn fill_gradient_90deg_varies_left_to_right() {
        // 90deg = 오른쪽 방향 → 왼쪽은 첫 스톱, 오른쪽은 마지막 스톱
        let mut canvas = Canvas::new(4, 1);
        let black = Color { r: 0, g: 0, b: 0, a: 255 };
        let white = Color { r: 255, g: 255, b: 255, a: 255 };
        let stops = vec![
            (black, crate::css::StopPos::Pct(0.0)),
            (white, crate::css::StopPos::Pct(1.0)),
        ];
        canvas.fill_gradient(
            Rect { x: 0.0, y: 0.0, width: 4.0, height: 1.0 },
            90.0,
            false,
            false,
            false,
            false,
            &stops,
            1.0,
        );
        assert!(canvas.pixels[0].r < canvas.pixels[3].r, "왼쪽이 오른쪽보다 어두워야 함");
        assert!(canvas.pixels[0].r < 64, "왼쪽 끝은 검정에 가까움");
        assert!(canvas.pixels[3].r > 192, "오른쪽 끝은 흰색에 가까움");
    }

    #[test]
    fn inner_shadow_darkens_near_edge() {
        // 흰 배경에 검정 inset 그림자(오프셋 0, blur 넉넉) → 가장자리가 중심보다 어둡다
        let mut canvas = Canvas::new(20, 20);
        for p in canvas.pixels.iter_mut() {
            *p = Color { r: 255, g: 255, b: 255, a: 255 };
        }
        let black = Color { r: 0, g: 0, b: 0, a: 255 };
        canvas.fill_inner_shadow(black, Rect { x: 0.0, y: 0.0, width: 20.0, height: 20.0 }, 0.0, 8.0, 0.0, 0.0, 0.0);
        let edge = canvas.pixels[10 * 20 + 0]; // 왼쪽 가장자리 (0,10)
        let center = canvas.pixels[10 * 20 + 10]; // 중심 (10,10)
        assert!(edge.r < center.r, "가장자리({})가 중심({})보다 어두워야", edge.r, center.r);
        assert!(edge.r < 80, "가장자리는 꽤 어둡다");
    }

    #[test]
    fn conic_gradient_varies_by_angle() {
        // conic 검정→흰색: 위쪽(각도 0)은 검정, 오른쪽/아래로 갈수록 밝아진다
        let mut canvas = Canvas::new(11, 11);
        let black = Color { r: 0, g: 0, b: 0, a: 255 };
        let white = Color { r: 255, g: 255, b: 255, a: 255 };
        let stops = vec![
            (black, crate::css::StopPos::Pct(0.0)),
            (white, crate::css::StopPos::Pct(1.0)),
        ];
        canvas.fill_gradient(
            Rect { x: 0.0, y: 0.0, width: 11.0, height: 11.0 },
            0.0,
            false,
            false,
            true,
            false,
            &stops,
            1.0,
        );
        let top = canvas.pixels[0 * 11 + 5]; // 중심 위쪽 (5,0) 각도~0 → 검정
        let left = canvas.pixels[5 * 11 + 0]; // 중심 왼쪽 (0,5) 각도~270 → 밝음
        assert!(top.r < left.r, "위쪽({})이 왼쪽({})보다 어두워야", top.r, left.r);
    }

    #[test]
    fn radial_gradient_darkens_from_center() {
        // radial: 중심은 첫 스톱(검정), 모서리로 갈수록 마지막 스톱(흰색)
        let mut canvas = Canvas::new(5, 5);
        let black = Color { r: 0, g: 0, b: 0, a: 255 };
        let white = Color { r: 255, g: 255, b: 255, a: 255 };
        let stops = vec![(black, crate::css::StopPos::Pct(0.0)), (white, crate::css::StopPos::Pct(1.0))];
        canvas.fill_gradient(Rect { x: 0.0, y: 0.0, width: 5.0, height: 5.0 }, 0.0, true, false, false, false, &stops, 1.0);
        let center = canvas.pixels[2 * 5 + 2]; // (2,2)
        let corner = canvas.pixels[0]; // (0,0)
        assert!(center.r < 40, "중심은 검정에 가까움, 실제 {}", center.r);
        assert!(corner.r > center.r, "모서리가 중심보다 밝아야");
    }

    #[test]
    fn box_pass_spreads_spike_symmetrically() {
        // 11칸 중 가운데(5)만 255. r=2 가로 패스 → [3..7] 로 균일 확산(255/5=51).
        let mut src = vec![(0f32, 0f32, 0f32); 11];
        src[5] = (255.0, 255.0, 255.0);
        let mut dst = vec![(0f32, 0f32, 0f32); 11];
        box_pass(&src, &mut dst, 11, 1, 2, true);
        assert!((dst[5].0 - 51.0).abs() < 0.5, "중심 255/5=51, 실제 {}", dst[5].0);
        assert!((dst[3].0 - dst[7].0).abs() < 0.01, "좌우 대칭");
        assert!((dst[3].0 - 51.0).abs() < 0.5, "창 안 균일");
        assert!(dst[2].0 < 0.01 && dst[8].0 < 0.01, "창 밖은 0");
        // 질량 보존(가장자리 클램프 없는 중앙부): 합 ≈ 255
        let sum: f32 = dst.iter().map(|c| c.0).sum();
        assert!((sum - 255.0).abs() < 1.0, "질량 보존 ~255, 실제 {}", sum);
    }

    #[test]
    fn erf_matches_known_values() {
        assert!(erf(0.0).abs() < 1e-4, "erf(0)=0");
        assert!((erf(1.0) - 0.8427).abs() < 1e-3, "erf(1)≈0.8427, 실제 {}", erf(1.0));
        assert!((erf(-1.0) + 0.8427).abs() < 1e-3, "erf(-1)≈-0.8427");
        assert!((erf(2.0) - 0.9953).abs() < 1e-3, "erf(2)≈0.9953, 실제 {}", erf(2.0));
    }

    #[test]
    fn gaussian_shadow_edge_is_half_and_falls_off() {
        // 흰 캔버스에 검정 드롭섀도(반경 0, blur 6). 경계에서 ~50%, 밖으로 갈수록 옅어짐.
        let mut canvas = Canvas::new(40, 40);
        let black = Color { r: 0, g: 0, b: 0, a: 255 };
        // 박스 [10..30] x [10..30], blur 6
        canvas.fill_soft_round_rect(black, Rect { x: 10.0, y: 10.0, width: 20.0, height: 20.0 }, 0.0, 6.0);
        // 중심(20,20) 내부 → 거의 검정
        let center = canvas.pixels[20 * 40 + 20];
        assert!(center.r < 20, "중심 진함, 실제 {}", center.r);
        // 왼쪽 경계 픽셀 (10,20): sdf≈0 → cov≈0.5 → r≈128
        let edge = canvas.pixels[20 * 40 + 10];
        assert!((edge.r as i32 - 128).abs() < 40, "경계 ~50%(회색), 실제 {}", edge.r);
        // 경계에서 더 바깥 (4,20) 은 더 옅다(밝다)
        let outer = canvas.pixels[20 * 40 + 4];
        assert!(outer.r > edge.r, "바깥이 경계보다 옅어야: {} > {}", outer.r, edge.r);
    }

    #[test]
    fn svg_arc_flattens_to_curve_not_chord() {
        // (0,0)→(10,0) 반지름 5 반원. 예전엔 끝점 직선(현)만 → y 항상 0.
        let mut out = vec![(0.0f32, 0.0f32)];
        flatten_arc(&mut out, 0.0, 0.0, 5.0, 5.0, 0.0, false, false, 10.0, 0.0);
        // 끝점 도달
        let last = *out.last().unwrap();
        assert!((last.0 - 10.0).abs() < 0.1 && last.1.abs() < 0.1, "끝점 (10,0): {:?}", last);
        // 중간에 크게 부풀어야(현 아님) — |y| 최대가 반지름 근처
        let max_bulge = out.iter().map(|&(_, y)| y.abs()).fold(0.0f32, f32::max);
        assert!(max_bulge > 3.0, "호가 부풀어야(현 아님), 최대 |y|={}", max_bulge);
        // 충분히 세분화
        assert!(out.len() > 6, "선분 수 충분: {}", out.len());
    }

    #[test]
    fn overflow_clip_pixel_clips_straddling_glyph() {
        let glyph = || DisplayItem::Glyph(crate::layout::GlyphInstance {
            font_index: 0,
            glyph_id: 1,
            x: 100.0,
            baseline_y: 100.0,
            px: 20.0,
            color: Color { r: 0, g: 0, b: 0, a: 255 },
            bold: false,
            italic: false,
            rot: 0.0,
        });
        // gbox ≈ [100..120] x [78..108].
        // 완전히 포함 → 글리프 그대로.
        let full = Rect { x: 0.0, y: 0.0, width: 200.0, height: 200.0 };
        assert!(matches!(clip_apply(glyph(), Some(full), false), Some(DisplayItem::Glyph(_))));
        // 오른쪽 경계에 걸침 → 사각 Clipped 로 감쌈(픽셀 클립).
        let straddle = Rect { x: 0.0, y: 0.0, width: 110.0, height: 200.0 };
        assert!(matches!(clip_apply(glyph(), Some(straddle), false), Some(DisplayItem::Clipped { .. })));
        // 걸치지만 둥근 클립 활성 → 바깥 래퍼가 처리하므로 이중 래핑 안 함.
        assert!(matches!(clip_apply(glyph(), Some(straddle), true), Some(DisplayItem::Glyph(_))));
        // 완전히 밖 → 컬링.
        let outside = Rect { x: 0.0, y: 0.0, width: 50.0, height: 50.0 };
        assert!(clip_apply(glyph(), Some(outside), false).is_none());
    }

    #[test]
    fn svg_line_stroke_quad_is_perpendicular() {
        // 수평선 (0,0)-(10,0), 굵기 4 → 세로로 ±2 벌어진 사각형.
        let q = stroke_line_quad(0.0, 0.0, 10.0, 0.0, 4.0).unwrap();
        assert_eq!(q.len(), 4);
        // y 좌표는 -2 또는 +2, x 는 0/10.
        assert!(q.iter().all(|&(_, y)| (y.abs() - 2.0).abs() < 1e-4), "수직 반굵기 2: {:?}", q);
        // 대각선 (0,0)-(10,10), 굵기 √2·2 → 수직 오프셋이 (−1,+1) 방향.
        let d = stroke_line_quad(0.0, 0.0, 10.0, 10.0, 2.0 * std::f32::consts::SQRT_2).unwrap();
        // 첫 점은 (x1+nx, y1+ny). n = perp(1/√2,1/√2)*half. half=√2. nx=-1, ny=1.
        assert!((d[0].0 - (-1.0)).abs() < 1e-4 && (d[0].1 - 1.0).abs() < 1e-4, "대각선 수직 오프셋: {:?}", d[0]);
        // 길이 0 → None
        assert!(stroke_line_quad(5.0, 5.0, 5.0, 5.0, 3.0).is_none());
    }

    #[test]
    fn polygon_edge_is_antialiased() {
        // 흰 캔버스(기본)에 검정 직각삼각형 (0,0)-(10,0)-(0,10). 빗변 x+y=10.
        let mut canvas = Canvas::new(12, 12);
        let black = Color { r: 0, g: 0, b: 0, a: 255 };
        let tri = vec![vec![(0.0, 0.0), (10.0, 0.0), (0.0, 10.0)]];
        canvas.fill_polygon(black, &tri);
        // 내부 깊숙이 → 꽉 찬 검정.
        let inside = canvas.pixels[1 * 12 + 1];
        assert!(inside.r < 5, "내부는 검정, 실제 {}", inside.r);
        // 빗변 위 픽셀 (5,4) 중심 (5.5,4.5) → 부분 커버리지(중간 알파, 계단 아님).
        let edge = canvas.pixels[4 * 12 + 5];
        assert!(edge.r > 20 && edge.r < 235, "빗변은 반투명(계단 아님), 실제 {}", edge.r);
        // 삼각형 밖 → 흰색.
        let outside = canvas.pixels[10 * 12 + 10];
        assert!(outside.r > 250, "밖은 흰색, 실제 {}", outside.r);
    }

    #[test]
    fn radial_ellipse_differs_from_circle_on_wide_box() {
        // 넓은 박스(20x4)에서 타원(기본)은 가로로 늘어나 오른쪽 가장자리 중앙이
        // 원보다 어둡다(정규화 거리 p 가 더 작음). 원과 결과가 달라야 함.
        let black = Color { r: 0, g: 0, b: 0, a: 255 };
        let white = Color { r: 255, g: 255, b: 255, a: 255 };
        let stops = vec![(black, crate::css::StopPos::Pct(0.0)), (white, crate::css::StopPos::Pct(1.0))];
        let rect = Rect { x: 0.0, y: 0.0, width: 20.0, height: 4.0 };
        let mut ell = Canvas::new(20, 4);
        ell.fill_gradient(rect, 0.0, true, false, false, false, &stops, 1.0); // ellipse
        let mut cir = Canvas::new(20, 4);
        cir.fill_gradient(rect, 0.0, true, true, false, false, &stops, 1.0); // circle
        // 오른쪽 가장자리 중앙 (19,2)
        let ell_edge = ell.pixels[2 * 20 + 19];
        let cir_edge = cir.pixels[2 * 20 + 19];
        assert!(ell_edge.r < cir_edge.r,
            "타원이 원보다 어두워야(가로로 늘어남): ell {} < cir {}", ell_edge.r, cir_edge.r);
    }

    #[test]
    fn text_shadow_doubles_glyphs() {
        // text-shadow → 그림자 글리프 + 본 글리프 = 2배
        let root = crate::html::parse_dom("<p>hi</p>".to_string());
        let ss = crate::css::parse(
            "p { display: block; font-size: 20px; text-shadow: 2px 2px 1px #ff0000; }".to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: crate::layout::Dimensions = Default::default();
        viewport.content.width = 200.0;
        let fs = fonts();
        let imgs = crate::layout::ImageMap::new();
        let layout_root = crate::layout::layout_tree(&styled, viewport, &fs, &imgs);
        let items = build_display_list(&layout_root);
        let glyphs = items.iter().filter(|i| matches!(i, DisplayItem::Glyph(_))).count();
        assert_eq!(glyphs, 4, "'hi' 2글자 × (그림자+본) = 4");
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