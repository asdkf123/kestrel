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
                let idx = y * self.width + x;
                self.pixels[idx] = blend(self.pixels[idx], color, color.a);
            }
        }
    }

    // 그라디언트 채우기. linear 는 픽셀을 축(angle: 0deg=위, 90deg=오른쪽)에 투영,
    // radial 은 중심에서의 거리를 farthest-corner 반경으로 정규화해 0..1 위치를 구하고
    // 스톱 사이를 보간한다. linear 선 길이 = |w*sin| + |h*cos| (모서리가 0/1 에 대응).
    pub fn fill_gradient(&mut self, rect: Rect, angle_deg: f32, radial: bool, conic: bool, stops: &[(Color, f32)]) {
        if rect.width <= 0.0 || rect.height <= 0.0 || stops.is_empty() {
            return;
        }
        let (cx, cy) = (rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
        let a = angle_deg.to_radians();
        let (dx, dy) = (a.sin(), -a.cos());
        let len = ((rect.width * dx).abs() + (rect.height * dy).abs()).max(1.0);
        // radial: 중심에서 가장 먼 모서리까지 거리 (farthest-corner)
        let radius = ((rect.width / 2.0).powi(2) + (rect.height / 2.0).powi(2)).sqrt().max(1.0);
        let x0 = rect.x.clamp(0.0, self.width as f32) as usize;
        let y0 = rect.y.clamp(0.0, self.height as f32) as usize;
        let x1 = (rect.x + rect.width).clamp(0.0, self.width as f32) as usize;
        let y1 = (rect.y + rect.height).clamp(0.0, self.height as f32) as usize;
        for y in y0..y1 {
            let fy = y as f32 + 0.5;
            for x in x0..x1 {
                let fx = x as f32 + 0.5;
                let p = if conic {
                    // 중심 기준 각도(위쪽 0, 시계방향) 0..1
                    let ang = (fx - cx).atan2(-(fy - cy)); // -π..π, 위쪽=0
                    let norm = if ang < 0.0 { ang + std::f32::consts::TAU } else { ang };
                    norm / std::f32::consts::TAU
                } else if radial {
                    (((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt() / radius).clamp(0.0, 1.0)
                } else {
                    let t = (fx - cx) * dx + (fy - cy) * dy;
                    ((t + len / 2.0) / len).clamp(0.0, 1.0)
                };
                let color = gradient_color_at(stops, p);
                if color.a == 0 {
                    continue;
                }
                let idx = y * self.width + x;
                self.pixels[idx] = blend(self.pixels[idx], color, color.a);
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
                // 색의 알파를 엣지 커버리지와 곱한다. 이걸 빼먹으면
                // rgba(0,0,0,0)/반투명 오버레이가 불투명 검정으로 칠해진다.
                let a = (cov * (color.a as f32 / 255.0) * 255.0).round() as u8;
                if a == 0 {
                    continue;
                }
                let idx = py * self.width + px;
                self.pixels[idx] = blend(self.pixels[idx], color, a);
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

    // 안쪽 그림자: 박스 내부에서 경계(오프셋 반영)로부터 blur 만큼 안으로 감쇠.
    // 경계 근처가 가장 진하고 중심으로 갈수록 옅어진다. rect(=border box)로 클립.
    pub fn fill_inner_shadow(&mut self, color: Color, rect: Rect, radius: f32, blur: f32, dx: f32, dy: f32) {
        if rect.width <= 0.0 || rect.height <= 0.0 {
            return;
        }
        let soft = blur.max(1.0);
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
                let sdf = outside + qx.max(qy).min(0.0) - r;
                // 경계(sdf~0)에서 1, 안으로 soft 만큼 들어가면(sdf=-soft) 0
                let cov = (1.0 + sdf / soft).clamp(0.0, 1.0);
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

    // 폴리곤 채우기 (nonzero winding 스캔라인). contours 는 물리 좌표. 홀(안쪽 윤곽) 지원.
    pub fn fill_polygon(&mut self, color: Color, contours: &[Vec<(f32, f32)>]) {
        if color.a == 0 {
            return;
        }
        let mut ymin = f32::INFINITY;
        let mut ymax = f32::NEG_INFINITY;
        for c in contours {
            for &(_, y) in c {
                ymin = ymin.min(y);
                ymax = ymax.max(y);
            }
        }
        let y0 = ymin.floor().max(0.0) as usize;
        let y1 = (ymax.ceil().max(0.0) as usize).min(self.height);
        for py in y0..y1 {
            let yc = py as f32 + 0.5;
            // 교차점 (x, winding 방향) 수집
            let mut xs: Vec<(f32, i32)> = Vec::new();
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
                if wind != 0 {
                    // 픽셀 중심 기준 반올림 (경계 픽셀 과포함 방지)
                    let xa = w[0].0.round().max(0.0) as usize;
                    let xb = (w[1].0.round().max(0.0) as usize).min(self.width);
                    for px in xa..xb {
                        let idx = py * self.width + px;
                        self.pixels[idx] = blend(self.pixels[idx], color, color.a);
                    }
                }
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

// 위치 p(0..1)에서 그라디언트 색을 선형 보간. stops 는 위치 오름차순 가정.
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
            let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * f).round() as u8;
            return Color {
                r: lerp(c0.r, c1.r),
                g: lerp(c0.g, c1.g),
                b: lerp(c0.b, c1.b),
                a: lerp(c0.a, c1.a),
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
            let idx = cy as usize * canvas.width + cx as usize;
            canvas.pixels[idx] = blend(canvas.pixels[idx], gi.color, a);
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
            let idx = py * canvas.width + px;
            canvas.pixels[idx] = blend(canvas.pixels[idx], gi.color, a);
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
    Natural, // 배경 이미지: 좌상단 고유 크기, 클립
}

// 트리 borrow 없이 스크롤 오프셋만 바꿔 반복 래스터화할 수 있다 (실제 브라우저 구조).
#[derive(Debug, Clone)]
pub enum DisplayItem {
    Rect { color: Color, rect: Rect },
    RoundRect { color: Color, rect: Rect, radius: f32 },
    Shadow { color: Color, rect: Rect, radius: f32, blur: f32 },
    // 안쪽 그림자 (box-shadow inset). dx/dy 는 오프셋, rect 는 border box.
    InnerShadow { color: Color, rect: Rect, radius: f32, blur: f32, dx: f32, dy: f32 },
    Image { image: usize, rect: Rect, fit: ImageFit },
    // 그라디언트 배경. angle: CSS 각도(linear), radial: 방사 여부, stops: (색, 위치 0-1).
    Gradient { rect: Rect, angle: f32, radial: bool, conic: bool, stops: Vec<(Color, f32)> },
    // SVG path 채우기 (여러 윤곽선, nonzero winding). points 는 논리 좌표.
    Polygon { color: Color, contours: Vec<Vec<(f32, f32)>> },
    Glyph(GlyphInstance),
    // position: sticky — 스크롤 시 뷰포트 상단 top 만큼 아래에 고정. top=스티키 임계,
    // y0=요소의 자연 문서 y. 렌더 시 inner 를 보정된 스크롤로 그린다.
    Sticky { top: f32, y0: f32, inner: Box<DisplayItem> },
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
    let inset = matches!(lb.styled_node.value("box-shadow-inset"),
        Some(Value::Keyword(ref k)) if k == "inset");
    if inset {
        return; // 안쪽 그림자는 emit_inner_shadow 가 배경 이후에 발행
    }
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

// SVG path d 속성 → 서브패스(윤곽선) 폴리라인 목록. 베지어는 평탄화, 상대/절대 지원.
// 지원: M/L/H/V/C/S/Q/T/Z (대소문자). A(호)는 끝점으로 직선 근사.
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
                    // 호는 끝점으로 직선 근사 (정확한 호 평탄화는 후속)
                    x = if rel { x + a[5] } else { a[5] };
                    y = if rel { y + a[6] } else { a[6] };
                    cur.push((x, y));
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

// 인라인 SVG 의 기본 도형(rect/circle/ellipse/line)을 viewBox 매핑으로 발행한다.
// path 등 복잡 도형은 미지원(후속). 대각선 line 은 근사(수평/수직만 정확).
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
    let mx = |x: f32| box_rect.x + (x - vx) * sx;
    let my = |y: f32| box_rect.y + (y - vy) * sy;
    for shape in &lb.styled_node.children {
        let crate::dom::NodeType::Element(e) = &shape.node.node_type else { continue };
        let num = |k: &str| e.attributes.get(k).and_then(|v| v.trim().parse::<f32>().ok());
        // fill: 속성 > 기본 검정. "none" 이면 채우지 않음.
        let fill = match e.attributes.get("fill").map(|s| s.as_str()) {
            Some("none") => None,
            Some(f) => crate::css::parse_color(f),
            None => Some(Color { r: 0, g: 0, b: 0, a: 255 }),
        };
        match e.tag_name.as_str() {
            "rect" => {
                if let Some(color) = fill {
                    let (x, y) = (mx(num("x").unwrap_or(0.0)), my(num("y").unwrap_or(0.0)));
                    let (w, h) = (num("width").unwrap_or(0.0) * sx, num("height").unwrap_or(0.0) * sy);
                    let r = num("rx").map(|r| r * sx).unwrap_or(0.0);
                    if w > 0.0 && h > 0.0 {
                        let rect = Rect { x, y, width: w, height: h };
                        if r > 0.0 {
                            items.push(DisplayItem::RoundRect { color, rect, radius: r });
                        } else {
                            items.push(DisplayItem::Rect { color, rect });
                        }
                    }
                }
            }
            "circle" => {
                if let Some(color) = fill {
                    let r = num("r").unwrap_or(0.0);
                    let (cx, cy) = (num("cx").unwrap_or(0.0), num("cy").unwrap_or(0.0));
                    let rect = Rect { x: mx(cx - r), y: my(cy - r), width: 2.0 * r * sx, height: 2.0 * r * sy };
                    items.push(DisplayItem::RoundRect { color, rect, radius: r * sx });
                }
            }
            "ellipse" => {
                if let Some(color) = fill {
                    let (rx, ry) = (num("rx").unwrap_or(0.0), num("ry").unwrap_or(0.0));
                    let (cx, cy) = (num("cx").unwrap_or(0.0), num("cy").unwrap_or(0.0));
                    let rect = Rect { x: mx(cx - rx), y: my(cy - ry), width: 2.0 * rx * sx, height: 2.0 * ry * sy };
                    items.push(DisplayItem::RoundRect { color, rect, radius: rx.min(ry) * sx });
                }
            }
            "line" => {
                // 수평/수직 선만 정확 (얇은 사각형). stroke 색/굵기 사용.
                let stroke = e.attributes.get("stroke").and_then(|s| crate::css::parse_color(s));
                if let Some(color) = stroke {
                    let sw = (num("stroke-width").unwrap_or(1.0) * sx).max(1.0);
                    let (x1, y1) = (mx(num("x1").unwrap_or(0.0)), my(num("y1").unwrap_or(0.0)));
                    let (x2, y2) = (mx(num("x2").unwrap_or(0.0)), my(num("y2").unwrap_or(0.0)));
                    let rect = Rect {
                        x: x1.min(x2),
                        y: y1.min(y2),
                        width: (x2 - x1).abs().max(sw),
                        height: (y2 - y1).abs().max(sw),
                    };
                    items.push(DisplayItem::Rect { color, rect });
                }
            }
            "path" => {
                if let Some(color) = fill {
                    if let Some(d) = e.attributes.get("d") {
                        let contours: Vec<Vec<(f32, f32)>> = flatten_path(d)
                            .into_iter()
                            .filter(|c| c.len() >= 3)
                            .map(|c| c.iter().map(|&(px, py)| (mx(px), my(py))).collect())
                            .collect();
                        if !contours.is_empty() {
                            items.push(DisplayItem::Polygon { color, contours });
                        }
                    }
                }
            }
            "polygon" | "polyline" => {
                if let Some(color) = fill {
                    if let Some(pts) = e.attributes.get("points") {
                        let nums: Vec<f32> = pts
                            .split(|c: char| c == ',' || c.is_whitespace())
                            .filter_map(|t| t.parse::<f32>().ok())
                            .collect();
                        let contour: Vec<(f32, f32)> =
                            nums.chunks(2).filter(|p| p.len() == 2).map(|p| (mx(p[0]), my(p[1]))).collect();
                        if contour.len() >= 3 {
                            items.push(DisplayItem::Polygon { color, contours: vec![contour] });
                        }
                    }
                }
            }
            _ => {} // text 등 미지원
        }
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
    let len = |name: &str| match lb.styled_node.value(name) {
        Some(Value::Length(v, crate::css::Unit::Px)) => Some(v),
        _ => None,
    };
    let (dx, dy) = match (len("box-shadow-x"), len("box-shadow-y")) {
        (Some(x), Some(y)) => (x, y),
        _ => return,
    };
    if !matches!(lb.styled_node.value("box-shadow-inset"),
        Some(Value::Keyword(ref k)) if k == "inset")
    {
        return;
    }
    let blur = len("box-shadow-blur").unwrap_or(0.0);
    let color = match lb.styled_node.value("box-shadow-color") {
        Some(Value::Color(c)) => c,
        _ => Color { r: 0, g: 0, b: 0, a: 128 },
    };
    let radius = uniform_radius(lb).max(0.0);
    items.push(DisplayItem::InnerShadow {
        color,
        rect: lb.dimensions.border_box(),
        radius,
        blur,
        dx,
        dy,
    });
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
    // (스택 레벨, 아이템) 수집 후 레벨로 안정 정렬 → 높은 z-index 가 위에 그려짐.
    // 같은 레벨은 문서 순서 유지(안정 정렬). 정식 스태킹 컨텍스트의 근사.
    let mut buf: Vec<(i32, DisplayItem)> = Vec::new();
    collect_items(root, 0, None, None, &mut buf);
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
        if let Some(Value::Length(n, _)) = lb.styled_node.value("z-index") {
            return n as i32;
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

// 클립 사각형에 아이템을 맞춰 자른다. 사각형/이미지는 교집합, 글리프는 완전히
// 밖이면 컬링(부분 픽셀 클립은 근사로 생략). clip=None 이면 그대로.
fn clip_apply(item: DisplayItem, clip: Option<Rect>) -> Option<DisplayItem> {
    let Some(c) = clip else { return Some(item) };
    match item {
        DisplayItem::Rect { color, rect } => {
            rect_intersect(rect, c).map(|r| DisplayItem::Rect { color, rect: r })
        }
        DisplayItem::Image { image, rect, fit } => {
            rect_intersect(rect, c).map(|r| DisplayItem::Image { image, rect: r, fit })
        }
        // 그라디언트: 보이는 영역으로 rect 만 자르고 각도/스톱은 유지
        // (클립된 부분만 다시 계산 — overflow 클립 하의 그라디언트는 드묾, 근사).
        DisplayItem::Gradient { rect, angle, radial, conic, stops } => {
            rect_intersect(rect, c).map(|r| DisplayItem::Gradient { rect: r, angle, radial, conic, stops })
        }
        DisplayItem::RoundRect { color, rect, radius } => {
            rect_intersect(rect, c).map(|r| DisplayItem::RoundRect { color, rect: r, radius })
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
            rect_intersect(bbox, c).map(|_| DisplayItem::Polygon { color, contours })
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
        DisplayItem::InnerShadow { color, rect, radius, blur, dx, dy } => {
            // 박스 안에만 그려지므로 rect 로 컬링만 (SDF 파라미터 유지)
            rect_intersect(rect, c).map(|_| DisplayItem::InnerShadow { color, rect, radius, blur, dx, dy })
        }
        DisplayItem::Glyph(gi) => {
            // 글리프 대략 bbox: x..x+px, baseline 위 1.1em ~ 아래 0.4em
            let gbox = Rect {
                x: gi.x,
                y: gi.baseline_y - 1.1 * gi.px,
                width: gi.px,
                height: 1.5 * gi.px,
            };
            rect_intersect(gbox, c).map(|_| DisplayItem::Glyph(gi))
        }
        // sticky 래퍼는 클립 전에 감싸지 않으므로 여기 도달 안 함 (exhaustive 용)
        sticky @ DisplayItem::Sticky { .. } => Some(sticky),
    }
}

fn collect_items(
    layout_box: &LayoutBox,
    parent_z: i32,
    clip: Option<Rect>,
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
    let mut local: Vec<DisplayItem> = Vec::new();
    // 그림자 → 배경/테두리(border-radius 포함) → 안쪽그림자 → 배경이미지 → 이미지 → 글리프 → 장식
    emit_box_shadow(layout_box, &mut local);
    emit_box_decorations(layout_box, &mut local);
    emit_inner_shadow(layout_box, &mut local);
    emit_outline(layout_box, &mut local);
    emit_svg(layout_box, &mut local);
    if let Some(idx) = layout_box.background_image {
        // background-size: cover/contain 지원. 그 외/미지정은 Natural(좌상단 고유크기).
        let fit = match layout_box.styled_node.value("background-size") {
            Some(Value::Keyword(ref k)) if k == "cover" => ImageFit::Cover,
            Some(Value::Keyword(ref k)) if k == "contain" => ImageFit::Contain,
            _ => ImageFit::Natural,
        };
        local.push(DisplayItem::Image {
            image: idx,
            rect: layout_box.dimensions.border_box(),
            fit,
        });
    }
    if let Some(g) = &layout_box.gradient {
        local.push(DisplayItem::Gradient {
            rect: layout_box.dimensions.border_box(),
            angle: g.angle_deg,
            radial: g.radial,
            conic: g.conic,
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
        local.push(DisplayItem::Image { image: idx, rect: layout_box.dimensions.content, fit });
    }
    for gi in &layout_box.glyphs {
        local.push(DisplayItem::Glyph(*gi));
    }
    for (rect, color) in &layout_box.decorations {
        local.push(DisplayItem::Rect { color: *color, rect: *rect });
    }
    // 이 박스 자신의 아이템은 부모 클립으로 자르고, sticky 면 래핑
    for it in local {
        if let Some(clipped) = clip_apply(it, clip) {
            let final_it = match sticky_here {
                Some((top, y0)) => DisplayItem::Sticky { top, y0, inner: Box::new(clipped) },
                None => clipped,
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
        collect_items(child, z, child_clip, sticky_here, buf);
    }
    // transform: rotate — 서브트리 아이템을 border-box 중심 기준으로 회전.
    // (translate/scale 는 레이아웃 단계에서 이미 박스에 반영됨.)
    if let Some(Value::Keyword(t)) = layout_box.styled_node.value("transform") {
        if let Some(angle) = transform_rotate_rad(&t) {
            let b = layout_box.dimensions.border_box();
            let (cx, cy) = (b.x + b.width / 2.0, b.y + b.height / 2.0);
            for (_, item) in buf[subtree_start..].iter_mut() {
                rotate_item(item, cx, cy, angle);
            }
        }
    }
    // filter: 서브트리 아이템 색을 함수 체인으로 변환 (grayscale/brightness/invert/sepia/contrast).
    if let Some(Value::Keyword(f)) = layout_box.styled_node.value("filter") {
        let funcs = parse_filters(&f);
        if !funcs.is_empty() {
            for (_, item) in buf[subtree_start..].iter_mut() {
                filter_item(item, &funcs);
            }
        }
    }
    // opacity < 1: 방금 채운 서브트리 구간의 알파에 opacity 를 곱한다.
    // 중첩 opacity 는 자연히 누적(자식 패스 ×op_child 후 부모 패스 ×op_parent).
    if let Some(op) = element_opacity(layout_box) {
        for (_, item) in buf[subtree_start..].iter_mut() {
            scale_item_alpha(item, op);
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

// 색에 filter 함수 체인을 적용.
fn apply_filters(c: Color, funcs: &[(String, f32)]) -> Color {
    let (mut r, mut g, mut b) = (c.r as f32, c.g as f32, c.b as f32);
    for (name, amt) in funcs {
        match name.as_str() {
            "grayscale" => {
                let luma = 0.299 * r + 0.587 * g + 0.114 * b;
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
            _ => {} // blur/drop-shadow/hue-rotate/saturate 등 미지원
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
        | DisplayItem::Shadow { color, .. }
        | DisplayItem::InnerShadow { color, .. }
        | DisplayItem::Polygon { color, .. } => *color = apply_filters(*color, funcs),
        DisplayItem::Glyph(gi) => gi.color = apply_filters(gi.color, funcs),
        DisplayItem::Gradient { stops, .. } => {
            for (c, _) in stops.iter_mut() {
                *c = apply_filters(*c, funcs);
            }
        }
        DisplayItem::Image { .. } => {} // 이미지 per-pixel 변환은 미지원(근사)
        DisplayItem::Sticky { inner, .. } => filter_item(inner, funcs),
    }
}

// transform 문자열에서 rotate 각도(라디안)를 추출. deg/rad/turn/무단위(deg) 지원.
fn transform_rotate_rad(text: &str) -> Option<f32> {
    let lower = text.to_ascii_lowercase();
    let idx = lower.find("rotate(")?;
    let rest = &text[idx + 7..];
    let close = rest.find(')')?;
    let arg = rest[..close].trim();
    let a = if let Some(n) = arg.strip_suffix("deg") {
        n.trim().parse::<f32>().ok()?.to_radians()
    } else if let Some(n) = arg.strip_suffix("turn") {
        n.trim().parse::<f32>().ok()? * std::f32::consts::TAU
    } else if let Some(n) = arg.strip_suffix("rad") {
        n.trim().parse::<f32>().ok()?
    } else {
        arg.parse::<f32>().ok()?.to_radians()
    };
    if a == 0.0 {
        None
    } else {
        Some(a)
    }
}

// 한 점을 (cx,cy) 기준 angle(rad) 회전.
fn rotate_pt(x: f32, y: f32, cx: f32, cy: f32, c: f32, s: f32) -> (f32, f32) {
    let (dx, dy) = (x - cx, y - cy);
    (cx + dx * c - dy * s, cy + dx * s + dy * c)
}

// 디스플레이 아이템을 (cx,cy) 기준 회전. 사각형은 폴리곤(4모서리)으로, 글리프는
// 위치 회전 + rot 각도 부여, 폴리곤은 점 회전. 그라디언트/이미지/그림자는 근사(중심만).
fn rotate_item(item: &mut DisplayItem, cx: f32, cy: f32, angle: f32) {
    let (c, s) = (angle.cos(), angle.sin());
    let quad = |rect: &Rect| -> Vec<(f32, f32)> {
        vec![
            rotate_pt(rect.x, rect.y, cx, cy, c, s),
            rotate_pt(rect.x + rect.width, rect.y, cx, cy, c, s),
            rotate_pt(rect.x + rect.width, rect.y + rect.height, cx, cy, c, s),
            rotate_pt(rect.x, rect.y + rect.height, cx, cy, c, s),
        ]
    };
    match item {
        DisplayItem::Rect { color, rect } | DisplayItem::RoundRect { color, rect, .. } => {
            *item = DisplayItem::Polygon { color: *color, contours: vec![quad(rect)] };
        }
        DisplayItem::Polygon { contours, .. } => {
            for ct in contours.iter_mut() {
                for p in ct.iter_mut() {
                    *p = rotate_pt(p.0, p.1, cx, cy, c, s);
                }
            }
        }
        DisplayItem::Glyph(gi) => {
            let (nx, ny) = rotate_pt(gi.x, gi.baseline_y, cx, cy, c, s);
            gi.x = nx;
            gi.baseline_y = ny;
            gi.rot += angle;
        }
        // 그라디언트/이미지/그림자: 중심을 회전해 이동만(축 정렬 유지, 근사)
        DisplayItem::Gradient { rect, .. }
        | DisplayItem::Image { rect, .. }
        | DisplayItem::Shadow { rect, .. }
        | DisplayItem::InnerShadow { rect, .. } => {
            let cxr = rect.x + rect.width / 2.0;
            let cyr = rect.y + rect.height / 2.0;
            let (nx, ny) = rotate_pt(cxr, cyr, cx, cy, c, s);
            rect.x = nx - rect.width / 2.0;
            rect.y = ny - rect.height / 2.0;
        }
        DisplayItem::Sticky { inner, .. } => rotate_item(inner, cx, cy, angle),
    }
}

// opacity 프로퍼티 (Length 로 실려옴). 1 미만일 때만 Some.
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
        DisplayItem::Rect { color, .. } => color.a = s(color.a),
        DisplayItem::RoundRect { color, .. } => color.a = s(color.a),
        DisplayItem::Shadow { color, .. } => color.a = s(color.a),
        DisplayItem::InnerShadow { color, .. } => color.a = s(color.a),
        DisplayItem::Glyph(gi) => gi.color.a = s(gi.color.a),
        DisplayItem::Gradient { stops, .. } => {
            for (c, _) in stops.iter_mut() {
                c.a = s(c.a);
            }
        }
        DisplayItem::Polygon { color, .. } => color.a = s(color.a),
        DisplayItem::Image { .. } => {} // 이미지 per-pixel 알파는 별도 — 근사로 스킵
        DisplayItem::Sticky { inner, .. } => scale_item_alpha(inner, f),
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
        DisplayItem::RoundRect { color, rect, radius } => {
            let r = scale_rect(rect);
            if r.y + r.height < 0.0 || r.y > vh {
                return;
            }
            canvas.fill_round_rect(*color, r, radius * scale);
        }
        DisplayItem::Shadow { color, rect, radius, blur } => {
            let r = scale_rect(rect);
            let m = blur * scale;
            if r.y + r.height + m < 0.0 || r.y - m > vh {
                return;
            }
            canvas.fill_soft_round_rect(*color, r, radius * scale, blur * scale);
        }
        DisplayItem::InnerShadow { color, rect, radius, blur, dx, dy } => {
            let r = scale_rect(rect);
            if r.y + r.height < 0.0 || r.y > vh {
                return;
            }
            canvas.fill_inner_shadow(*color, r, radius * scale, blur * scale, dx * scale, dy * scale);
        }
        DisplayItem::Image { image, rect, fit } => {
            let r = scale_rect(rect);
            if r.y + r.height < 0.0 || r.y > vh {
                return;
            }
            if let Some(img) = images.get(*image) {
                blit_image(canvas, img, r, scale, *fit);
            }
        }
        DisplayItem::Gradient { rect, angle, radial, conic, stops } => {
            let r = scale_rect(rect);
            if r.y + r.height < 0.0 || r.y > vh {
                return;
            }
            canvas.fill_gradient(r, *angle, *radial, *conic, stops);
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
    }
}

// rect(물리 px) 좌상단에 이미지를 scale 배로 그린다 (최근접 샘플링).
// rect 크기로 클리핑 (<img> 는 rect == 고유 크기 × scale 이라 무손실).
// rect(물리 px)에 이미지를 fit 방식으로 그린다. 목적지 하위영역 `dr` 을 구하고,
// dr 안 각 픽셀의 상대 위치로 소스 픽셀을 샘플(최근접), rect 로 클립한다.
fn blit_image(canvas: &mut Canvas, img: &crate::png::Image, rect: Rect, scale: f32, fit: ImageFit) {
    let (iw, ih) = (img.width as f32, img.height as f32);
    if iw <= 0.0 || ih <= 0.0 || rect.width <= 0.0 || rect.height <= 0.0 {
        return;
    }
    let (cx, cy) = (rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
    // 그려질 목적지 사각형 dr (이미지 전체가 매핑되는 영역; rect 밖은 클립)
    let dr = match fit {
        ImageFit::Fill => rect,
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
    // 실제로 칠할 영역 = dr ∩ rect ∩ 캔버스
    let x0 = dr.x.max(rect.x).max(0.0) as usize;
    let y0 = dr.y.max(rect.y).max(0.0) as usize;
    let x1 = (dr.x + dr.width).min(rect.x + rect.width).min(canvas.width as f32).max(0.0) as usize;
    let y1 = (dr.y + dr.height).min(rect.y + rect.height).min(canvas.height as f32).max(0.0) as usize;
    for py in y0..y1 {
        let fy = (py as f32 + 0.5 - dr.y) / dr.height;
        let sy = ((fy * ih) as i32).clamp(0, img.height as i32 - 1) as usize;
        for px in x0..x1 {
            let fx = (px as f32 + 0.5 - dr.x) / dr.width;
            let sx = ((fx * iw) as i32).clamp(0, img.width as i32 - 1) as usize;
            let s = (sy * img.width + sx) * 4;
            let fg = Color { r: img.rgba[s], g: img.rgba[s + 1], b: img.rgba[s + 2], a: 255 };
            let alpha = img.rgba[s + 3];
            let idx = py * canvas.width + px;
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
    fn filter_grayscale_and_invert() {
        // grayscale(100%) 빨강 → 회색 (r=g=b=luma≈76)
        let gray = canvas_for(
            "<div></div>",
            "div { display: block; width: 2px; height: 2px; background-color: #ff0000; filter: grayscale(100%); }",
            4.0,
            4.0,
        );
        let p = gray.pixels[0];
        assert_eq!(p.r, p.g);
        assert_eq!(p.g, p.b);
        assert!((p.r as i32 - 76).abs() <= 2, "빨강 luma ~76, 실제 {}", p.r);
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

    #[test]
    fn gradient_color_interpolates_between_stops() {
        let black = Color { r: 0, g: 0, b: 0, a: 255 };
        let white = Color { r: 255, g: 255, b: 255, a: 255 };
        let stops = vec![(black, 0.0), (white, 1.0)];
        assert_eq!(gradient_color_at(&stops, 0.0), black);
        assert_eq!(gradient_color_at(&stops, 1.0), white);
        let mid = gradient_color_at(&stops, 0.5);
        assert!((mid.r as i32 - 128).abs() <= 1, "중간은 ~128, 실제 {}", mid.r);
        // 범위 밖은 양끝으로 클램프
        assert_eq!(gradient_color_at(&stops, -1.0), black);
        assert_eq!(gradient_color_at(&stops, 2.0), white);
    }

    #[test]
    fn fill_gradient_90deg_varies_left_to_right() {
        // 90deg = 오른쪽 방향 → 왼쪽은 첫 스톱, 오른쪽은 마지막 스톱
        let mut canvas = Canvas::new(4, 1);
        let black = Color { r: 0, g: 0, b: 0, a: 255 };
        let white = Color { r: 255, g: 255, b: 255, a: 255 };
        let stops = vec![(black, 0.0), (white, 1.0)];
        canvas.fill_gradient(Rect { x: 0.0, y: 0.0, width: 4.0, height: 1.0 }, 90.0, false, false, &stops);
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
        canvas.fill_inner_shadow(black, Rect { x: 0.0, y: 0.0, width: 20.0, height: 20.0 }, 0.0, 8.0, 0.0, 0.0);
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
        let stops = vec![(black, 0.0), (white, 1.0)];
        canvas.fill_gradient(Rect { x: 0.0, y: 0.0, width: 11.0, height: 11.0 }, 0.0, false, true, &stops);
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
        let stops = vec![(black, 0.0), (white, 1.0)];
        canvas.fill_gradient(Rect { x: 0.0, y: 0.0, width: 5.0, height: 5.0 }, 0.0, true, false, &stops);
        let center = canvas.pixels[2 * 5 + 2]; // (2,2)
        let corner = canvas.pixels[0]; // (0,0)
        assert!(center.r < 40, "중심은 검정에 가까움, 실제 {}", center.r);
        assert!(corner.r > center.r, "모서리가 중심보다 밝아야");
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
