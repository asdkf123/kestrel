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

    // linear-gradient 채우기. angle 은 CSS 각도(0deg=위쪽, 90deg=오른쪽).
    // 각 픽셀을 그라디언트 축에 투영해 0..1 위치를 구하고 스톱 사이를 보간한다.
    // 그라디언트 선 길이 = |w*sin| + |h*cos| (CSS: 모서리가 0/1 에 대응).
    pub fn fill_gradient(&mut self, rect: Rect, angle_deg: f32, stops: &[(Color, f32)]) {
        if rect.width <= 0.0 || rect.height <= 0.0 || stops.is_empty() {
            return;
        }
        let a = angle_deg.to_radians();
        let (dx, dy) = (a.sin(), -a.cos());
        let (cx, cy) = (rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
        let len = ((rect.width * dx).abs() + (rect.height * dy).abs()).max(1.0);
        let x0 = rect.x.clamp(0.0, self.width as f32) as usize;
        let y0 = rect.y.clamp(0.0, self.height as f32) as usize;
        let x1 = (rect.x + rect.width).clamp(0.0, self.width as f32) as usize;
        let y1 = (rect.y + rect.height).clamp(0.0, self.height as f32) as usize;
        for y in y0..y1 {
            let fy = y as f32 + 0.5;
            for x in x0..x1 {
                let fx = x as f32 + 0.5;
                let t = (fx - cx) * dx + (fy - cy) * dy;
                let p = ((t + len / 2.0) / len).clamp(0.0, 1.0);
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

// 디스플레이 리스트: 레이아웃 트리에서 뽑아낸 소유(owned) 그리기 명령 목록.
// 트리 borrow 없이 스크롤 오프셋만 바꿔 반복 래스터화할 수 있다 (실제 브라우저 구조).
#[derive(Debug, Clone)]
pub enum DisplayItem {
    Rect { color: Color, rect: Rect },
    RoundRect { color: Color, rect: Rect, radius: f32 },
    Shadow { color: Color, rect: Rect, radius: f32, blur: f32 },
    Image { image: usize, rect: Rect },
    // linear-gradient 배경. angle: CSS 각도, stops: (색, 위치 0-1).
    Gradient { rect: Rect, angle: f32, stops: Vec<(Color, f32)> },
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
        DisplayItem::Image { image, rect } => {
            rect_intersect(rect, c).map(|r| DisplayItem::Image { image, rect: r })
        }
        // 그라디언트: 보이는 영역으로 rect 만 자르고 각도/스톱은 유지
        // (클립된 부분만 다시 계산 — overflow 클립 하의 그라디언트는 드묾, 근사).
        DisplayItem::Gradient { rect, angle, stops } => {
            rect_intersect(rect, c).map(|r| DisplayItem::Gradient { rect: r, angle, stops })
        }
        DisplayItem::RoundRect { color, rect, radius } => {
            rect_intersect(rect, c).map(|r| DisplayItem::RoundRect { color, rect: r, radius })
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
    // 그림자 → 배경/테두리(border-radius 포함) → 배경이미지 → 이미지 → 글리프 → 장식
    emit_box_shadow(layout_box, &mut local);
    emit_box_decorations(layout_box, &mut local);
    if let Some(idx) = layout_box.background_image {
        local.push(DisplayItem::Image { image: idx, rect: layout_box.dimensions.border_box() });
    }
    if let Some(g) = &layout_box.gradient {
        local.push(DisplayItem::Gradient {
            rect: layout_box.dimensions.border_box(),
            angle: g.angle_deg,
            stops: g.stops.clone(),
        });
    }
    if let Some(idx) = layout_box.image {
        local.push(DisplayItem::Image { image: idx, rect: layout_box.dimensions.content });
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
    // opacity < 1: 방금 채운 서브트리 구간의 알파에 opacity 를 곱한다.
    // 중첩 opacity 는 자연히 누적(자식 패스 ×op_child 후 부모 패스 ×op_parent).
    if let Some(op) = element_opacity(layout_box) {
        for (_, item) in buf[subtree_start..].iter_mut() {
            scale_item_alpha(item, op);
        }
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
        DisplayItem::Glyph(gi) => gi.color.a = s(gi.color.a),
        DisplayItem::Gradient { stops, .. } => {
            for (c, _) in stops.iter_mut() {
                c.a = s(c.a);
            }
        }
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
                DisplayItem::Image { image, rect } => eprintln!(
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
        DisplayItem::Image { image, rect } => {
            let r = scale_rect(rect);
            if r.y + r.height < 0.0 || r.y > vh {
                return;
            }
            if let Some(img) = images.get(*image) {
                blit_image(canvas, img, r, scale);
            }
        }
        DisplayItem::Gradient { rect, angle, stops } => {
            let r = scale_rect(rect);
            if r.y + r.height < 0.0 || r.y > vh {
                return;
            }
            canvas.fill_gradient(r, *angle, stops);
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
        canvas.fill_gradient(Rect { x: 0.0, y: 0.0, width: 4.0, height: 1.0 }, 90.0, &stops);
        assert!(canvas.pixels[0].r < canvas.pixels[3].r, "왼쪽이 오른쪽보다 어두워야 함");
        assert!(canvas.pixels[0].r < 64, "왼쪽 끝은 검정에 가까움");
        assert!(canvas.pixels[3].r > 192, "오른쪽 끝은 흰색에 가까움");
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
