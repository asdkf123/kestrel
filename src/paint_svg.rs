// SVG 렌더링: 도형/경로/텍스트를 디스플레이 아이템으로. fill 과 stroke 를 모두 그리고,
// fill/stroke/stroke-width 는 조상에서 상속된다 (아이콘 세트가 루트 <svg> 에 준다).
use crate::paint::*;
use crate::css::Color;
use crate::font::FontStack;
use crate::layout::{LayoutBox, Rect};
use crate::raster::GlyphCache;

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

pub(crate) fn emit_svg(lb: &LayoutBox, items: &mut Vec<DisplayItem>) {
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

impl Default for SvgInherit {
    fn default() -> Self {
        SvgInherit {
            fill: Some(Color { r: 0, g: 0, b: 0, a: 255 }), // SVG 기본 fill 은 검정
            stroke: None,
            stroke_width: 1.0,
        }
    }
}
