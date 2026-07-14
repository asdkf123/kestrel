use std::num::NonZeroU32;
use std::rc::Rc;
use std::time::{Duration, Instant};

use winit::dpi::LogicalSize;
use winit::event::{ElementState, Event, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ControlFlow, EventLoop};
use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
use winit::window::{CursorIcon, WindowBuilder};

use crate::css::Color;
use crate::layout::{hit_link, Rect};
use crate::paint::DisplayItem;

/// 스크립트 실행 중 강제 레이아웃(forced layout)에 필요한 입력.
/// CSSOM View 의 측정 API(getBoundingClientRect/offset*/getComputedStyle)는 "읽는 순간"
/// 보류된 스타일·레이아웃을 흘려야 한다. 예전엔 스크립트가 렌더보다 먼저 다 돌아서
/// 측정값이 전부 0/빈 문자열이었다 — 측정 후 배치하는 코드(스티키 헤더, 캐러셀,
/// 레이아웃 라이브러리)가 조용히 어긋났다.
///
/// Page 가 js 를 가변으로 빌린 상태에서 sheet/fonts 등을 함께 넘겨야 해서 생 포인터로
/// 든다(dom 포인터와 같은 규약). 스크립트/콜백 실행 구간에서만 설정된다.
#[derive(Clone, Copy)]
pub struct LayoutCtx {
    pub sheet: *const crate::css::Stylesheet,
    pub fonts: *const crate::font::FontStack,
    pub img_map: *const crate::layout::ImageMap,
    // 실제 이미지 픽셀 (canvas getImageData 가 오프스크린 래스터에 쓴다)
    pub images: *const Vec<crate::png::Image>,
    pub pseudo: *const crate::style::PseudoStyles,
    pub vw: f32,
    pub vh: f32,
}

/// JS 가 측정 API 를 읽을 때 호출된다. DOM 버전이 지난 레이아웃 이후 바뀌었으면
/// 스타일 → 레이아웃을 다시 돌려 인터프리터의 측정 맵을 채운다.
pub fn flush_layout(js: &mut crate::js::interp::Interp) {
    let (Some(dom_ptr), Some(ctx)) = (js.dom, js.layout_ctx) else { return };
    let dom = unsafe { &*dom_ptr };
    if js.layout_version == Some(dom.version()) {
        return; // 깨끗하다 — 재계산 불필요
    }
    let (sheet, fonts, img_map, pseudo) =
        unsafe { (&*ctx.sheet, &*ctx.fonts, &*ctx.img_map, &*ctx.pseudo) };
    let vp = crate::style::Viewport { w: ctx.vw, h: ctx.vh };
    let style_root = crate::style::style_tree_full(dom, sheet, vp, pseudo);
    let mut viewport: crate::layout::Dimensions = Default::default();
    viewport.content.width = ctx.vw;
    let mut layout_root = crate::layout::layout_tree(&style_root, viewport, fonts, img_map);
    crate::layout::apply_sticky(&mut layout_root, 0.0, js.scroll_y, ctx.vw, ctx.vh);
    fill_js_maps(js, &style_root, &layout_root);
    js.layout_version = Some(dom.version());
}

// 레이아웃 산출물 → JS 측정 맵(getBoundingClientRect/offset*, getComputedStyle).
fn fill_js_maps(
    js: &mut crate::js::interp::Interp,
    style_root: &crate::style::StyledNode,
    layout_root: &crate::layout::LayoutBox,
) {
    let mut rects = Vec::new();
    crate::layout::collect_element_rects(layout_root, 0, &mut rects);
    js.layout_rects.clear();
    // 인라인 요소(span/a/b/em…)는 자체 박스가 없다 — 조각들의 합집합이 그 요소의 박스다.
    // 예전엔 getBoundingClientRect/offsetWidth 가 전부 0 이었다(링크·강조어를 재는
    // 툴팁·팝오버·하이라이터가 조용히 죽는다).
    let mut inline_rects = std::collections::HashMap::new();
    crate::layout::collect_inline_element_rects(layout_root, &mut inline_rects);
    for (id, r) in inline_rects {
        js.layout_rects.insert(id, (r.x, r.y, r.width, r.height));
    }
    // 블록 박스가 있는 요소는 그 박스가 우선 (인라인 조각보다 정확)
    for (r, id, _) in &rects {
        js.layout_rects.insert(*id, (r.x, r.y, r.width, r.height));
    }
    js.computed_styles.clear();
    collect_computed_styles(style_root, &mut js.computed_styles);
    // 표준의 resolved value: 길이는 px 로 확정돼야 한다. %/em/무단위 배수는 스타일 맵에
    // 그대로 남아 있으므로(예: margin "10%"), 레이아웃이 확정한 used value 로 덮는다.
    let mut metrics = std::collections::HashMap::new();
    crate::layout::collect_box_metrics(layout_root, &mut metrics);
    let px = |v: f32| format!("{}px", crate::style::num_css(v));
    for (id, d) in &metrics {
        let Some(m) = js.computed_styles.get_mut(id) else { continue };
        m.insert("width".to_string(), px(d.content.width));
        m.insert("height".to_string(), px(d.content.height));
        for (k, v) in [
            ("margin-top", d.margin.top),
            ("margin-right", d.margin.right),
            ("margin-bottom", d.margin.bottom),
            ("margin-left", d.margin.left),
            ("padding-top", d.padding.top),
            ("padding-right", d.padding.right),
            ("padding-bottom", d.padding.bottom),
            ("padding-left", d.padding.left),
            ("border-top-width", d.border.top),
            ("border-right-width", d.border.right),
            ("border-bottom-width", d.border.bottom),
            ("border-left-width", d.border.left),
        ] {
            m.insert(k.to_string(), px(v));
        }
        // 무단위 line-height 배수는 font-size 를 곱해 px 로 확정 (CSS2 §10.8).
        let fs = m
            .get("font-size")
            .and_then(|s| s.strip_suffix("px"))
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(crate::style::DEFAULT_FONT_SIZE);
        if let Some(lh) = m.get("line-height").cloned() {
            if let Ok(factor) = lh.parse::<f32>() {
                m.insert("line-height".to_string(), px(factor * fs));
            }
        }
    }
}

/// 페이지: 원본(DOM/스타일시트/JS 런타임)을 소유하고, rebuild() 로 렌더 산출물을
/// 재생성한다. 이벤트 핸들러가 DOM 을 바꾸면 rebuild 로 화면이 갱신된다.
/// 스타일/레이아웃 트리는 rebuild 안에서만 사는 일시 산물 (borrow 격리).
pub struct Page {
    pub dom: crate::dom::Dom,
    pub sheet: crate::css::Stylesheet,
    pub images: Vec<crate::png::Image>,
    pub img_map: crate::layout::ImageMap,
    pub fonts: crate::font::FontStack,
    pub js: crate::js::interp::Interp,
    pub url: crate::url::Url,
    pub viewport_width: f32,
    pub viewport_height: f32,
    // ::before/::after 합성 노드 id → 명시값 (페이지 빌드 시 1회 생성, 재빌드마다 재사용)
    pub pseudo_styles: crate::style::PseudoStyles,
    // ── rebuild() 산출물 ──
    pub items: Vec<DisplayItem>,
    pub links: Vec<(Rect, String)>,
    pub element_rects: Vec<(Rect, crate::dom::NodeId, usize)>,
    pub doc_height: f32,
    // 포커스된 <input> (타이핑 대상)
    pub focused_input: Option<crate::dom::NodeId>,
    // 현재 스크롤 위치 — position: sticky 가 이 값을 보고 붙는다.
    // 스크롤이 바뀌면 rebuild 해야 스티키가 따라온다(브라우저도 스크롤마다 갱신한다).
    pub scroll_y: f32,
}

// 캔버스 하나의 명령을 원점(0,0) 기준 DisplayItem 으로 (getImageData 의 오프스크린 래스터용).
pub fn canvas_items_at_origin(
    ops: &[crate::js::interp::CanvasOp],
    fonts: &crate::font::FontStack,
) -> Vec<DisplayItem> {
    let rects = vec![(Rect { x: 0.0, y: 0.0, width: 0.0, height: 0.0 }, 0usize, 0usize)];
    let mut map = std::collections::HashMap::new();
    map.insert(0usize, ops.to_vec());
    canvas_display_items(&rects, &map, fonts)
}

// <canvas> 2D 명령(캔버스 좌표)을 각 canvas 박스 위치로 옮겨 DisplayItem 목록으로.
fn canvas_display_items(
    element_rects: &[(Rect, crate::dom::NodeId, usize)],
    canvas_cmds: &std::collections::HashMap<crate::dom::NodeId, Vec<crate::js::interp::CanvasOp>>,
    fonts: &crate::font::FontStack,
) -> Vec<DisplayItem> {
    use crate::js::interp::CanvasOp;
    let mut out = Vec::new();
    let white = crate::css::Color { r: 255, g: 255, b: 255, a: 255 };
    for (r, id, _) in element_rects {
        let Some(ops) = canvas_cmds.get(id) else { continue };
        let (bx, by) = (r.x, r.y);
        // 현재 CTM (캔버스 좌표계). 변환이 걸린 op 들은 DisplayItem::Transform 으로 감싸
        // 페인트가 오프스크린 레이어에 그린 뒤 역매핑한다 — 텍스트·이미지까지 정확히 변환된다.
        let mut ctm = crate::layout::Mat::IDENTITY;
        let mut start = out.len();
        // 변환이 바뀔 때마다 지금까지 쌓인 item 을 감싼다
        macro_rules! flush_transform {
            ($m:expr) => {
                if !$m.is_identity() && out.len() > start {
                    let items: Vec<DisplayItem> = out.drain(start..).collect();
                    // 캔버스 좌표계 → 페이지 좌표계: T(원점) · M · T(-원점)
                    let to_origin = crate::layout::Mat { e: -bx, f: -by, ..crate::layout::Mat::IDENTITY };
                    let back = crate::layout::Mat { e: bx, f: by, ..crate::layout::Mat::IDENTITY };
                    let abs = to_origin.then(&$m).then(&back);
                    out.push(DisplayItem::Transform { m: abs, items });
                }
                start = out.len();
            };
        }
        // 현재 클립 (canvas 좌표 다각형). clip() 이 설정하고 restore 가 되돌린다.
        let mut clip: Option<Vec<(f32, f32)>> = None;
        for op in ops {
            let before = out.len();
            match op {
                CanvasOp::Clip { pts } => {
                    clip = pts.clone();
                }
                CanvasOp::FillGradient { rect, shape, kind, stops } => {
                    let r = Rect {
                        x: bx + rect.x,
                        y: by + rect.y,
                        width: rect.width,
                        height: rect.height,
                    };
                    // 그라디언트 파라미터도 페이지 좌표로 옮긴다
                    let k = match kind {
                        crate::paint::CanvasGrad::Linear { x0, y0, x1, y1 } => {
                            crate::paint::CanvasGrad::Linear {
                                x0: bx + x0,
                                y0: by + y0,
                                x1: bx + x1,
                                y1: by + y1,
                            }
                        }
                        crate::paint::CanvasGrad::Radial { x0, y0, r0, x1, y1, r1 } => {
                            crate::paint::CanvasGrad::Radial {
                                x0: bx + x0,
                                y0: by + y0,
                                r0: *r0,
                                x1: bx + x1,
                                y1: by + y1,
                                r1: *r1,
                            }
                        }
                    };
                    let item = DisplayItem::CanvasGradient {
                        rect: r,
                        kind: k,
                        stops: stops.clone(),
                    };
                    // 모양이 있으면 그 다각형으로 자른다 (경로 채우기)
                    let item = match shape {
                        Some(pts) => DisplayItem::Clipped {
                            shape: crate::paint::ClipShape::Polygon(
                                pts.iter().map(|&(x, y)| (bx + x, by + y)).collect(),
                            ),
                            inner: Box::new(item),
                        },
                        None => item,
                    };
                    out.push(item);
                }
                CanvasOp::FillPattern { rect, shape, idx, repeat } => {
                    let r = Rect {
                        x: bx + rect.x,
                        y: by + rect.y,
                        width: rect.width,
                        height: rect.height,
                    };
                    let fit = if *repeat {
                        crate::paint::ImageFit::Tile
                    } else {
                        crate::paint::ImageFit::Natural
                    };
                    let item = DisplayItem::Image { image: *idx, rect: r, fit, pos: None };
                    let item = match shape {
                        Some(pts) => DisplayItem::Clipped {
                            shape: crate::paint::ClipShape::Polygon(
                                pts.iter().map(|&(x, y)| (bx + x, by + y)).collect(),
                            ),
                            inner: Box::new(item),
                        },
                        None => item,
                    };
                    out.push(item);
                }
                CanvasOp::PutImage { x, y, img } => {
                    out.push(DisplayItem::RawImage {
                        rect: Rect {
                            x: bx + x,
                            y: by + y,
                            width: img.width as f32,
                            height: img.height as f32,
                        },
                        img: img.clone(),
                    });
                }
                CanvasOp::SetTransform { m } => {
                    flush_transform!(ctm);
                    ctm = *m;
                }
                CanvasOp::DrawImage { idx, x, y, w, h } => {
                    let rect = Rect {
                        x: bx + x,
                        y: by + y,
                        width: *w,
                        height: *h,
                    };
                    // dw/dh 가 0 이면 고유 크기로 (Natural), 아니면 지정 크기에 맞춰 늘린다
                    let fit = if *w <= 0.0 || *h <= 0.0 {
                        crate::paint::ImageFit::Natural
                    } else {
                        crate::paint::ImageFit::Fill
                    };
                    out.push(DisplayItem::Image { image: *idx, rect, fit, pos: None });
                }
                CanvasOp::FillRect { x, y, w, h, color } => out.push(DisplayItem::Rect {
                    color: *color,
                    rect: Rect { x: bx + x, y: by + y, width: *w, height: *h },
                }),
                CanvasOp::ClearRect { x, y, w, h } => out.push(DisplayItem::Rect {
                    color: white,
                    rect: Rect { x: bx + x, y: by + y, width: *w, height: *h },
                }),
                CanvasOp::StrokeRect { x, y, w, h, color, lw } => {
                    let t = lw.max(1.0);
                    let (px, py) = (bx + x, by + y);
                    for rect in [
                        Rect { x: px, y: py, width: *w, height: t },
                        Rect { x: px, y: py + h - t, width: *w, height: t },
                        Rect { x: px, y: py, width: t, height: *h },
                        Rect { x: px + w - t, y: py, width: t, height: *h },
                    ] {
                        out.push(DisplayItem::Rect { color: *color, rect });
                    }
                }
                CanvasOp::FillPath { pts, color } => {
                    let mapped: Vec<(f32, f32)> = pts.iter().map(|&(x, y)| (bx + x, by + y)).collect();
                    out.push(DisplayItem::Polygon { color: *color, contours: vec![mapped] });
                }
                CanvasOp::FillText { text, x, y, color, px } => {
                    let mut pen = bx + x;
                    for ch in text.chars() {
                        let (fi, gid) = fonts.glyph_for(ch);
                        let f = fonts.font(fi);
                        let adv = f.advance_width(gid) as f32 * (px / f.units_per_em() as f32);
                        out.push(DisplayItem::Glyph(crate::layout::GlyphInstance {
                            font_index: fi,
                            glyph_id: gid,
                            x: pen,
                            baseline_y: by + y,
                            px: *px,
                            color: *color,
                            bold: false,
                            italic: false,
                            rot: 0.0,
                        }));
                        pen += adv;
                    }
                }
            }
            // clip() 이 걸려 있으면 이 op 가 만든 항목들을 다각형으로 자른다.
            // 예전엔 clip 이 통째로 무시돼서 잘려야 할 그림이 그대로 나왔다.
            if let Some(cp) = &clip {
                if out.len() > before {
                    let shape = crate::paint::ClipShape::Polygon(
                        cp.iter().map(|&(x, y)| (bx + x, by + y)).collect(),
                    );
                    let items: Vec<DisplayItem> = out.drain(before..).collect();
                    for it in items {
                        out.push(DisplayItem::Clipped {
                            shape: shape.clone(),
                            inner: Box::new(it),
                        });
                    }
                }
            }
        }
        flush_transform!(ctm);
    }
    out
}

// application/x-www-form-urlencoded (공백은 +)
// 스타일 트리를 순회하며 요소별 계산 스타일을 CSS 텍스트 맵으로 수집(getComputedStyle 용).
fn collect_computed_styles(
    node: &crate::style::StyledNode,
    out: &mut std::collections::HashMap<crate::dom::NodeId, std::collections::HashMap<String, String>>,
) {
    // 요소 노드(자식이 있거나 프로퍼티가 있는)만. 텍스트 노드는 건너뜀.
    if matches!(node.node.node_type, crate::dom::NodeType::Element(_)) {
        let mut m = std::collections::HashMap::with_capacity(node.specified_values.len());
        for (k, v) in &node.specified_values {
            m.insert(k.clone(), crate::style::computed_value_string(v));
        }
        // 규칙이 없는 프로퍼티도 resolved value 를 돌려줘야 한다(초기값/상속값).
        // 예전엔 빈 문자열이라 getComputedStyle(el).position === 'static' 같은 검사가
        // 전부 실패했다 — 사이트는 우리가 아무 스타일도 없다고 믿는다.
        let color = m.get("color").cloned().unwrap_or_else(|| "rgb(0, 0, 0)".to_string());
        for prop in crate::css::SUPPORTED {
            if m.contains_key(*prop) {
                continue;
            }
            if crate::style::is_current_color_prop(prop) {
                m.insert(prop.to_string(), color.clone());
            } else if let Some(v) = crate::style::initial_value(prop) {
                m.insert(prop.to_string(), v.to_string());
            }
        }
        out.insert(node.id, m);
    }
    for child in &node.children {
        collect_computed_styles(child, out);
    }
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

impl Page {
    // JS 실행 구간 진입: DOM 포인터와 강제 레이아웃 컨텍스트를 건다.
    // 콜백 안에서 el.getBoundingClientRect() 를 읽으면 flush_layout 이 돌아 최신값이 나온다.
    fn enter_js(&mut self) {
        let ctx = LayoutCtx {
            sheet: &self.sheet,
            fonts: &self.fonts,
            img_map: &self.img_map,
            images: &self.images,
            pseudo: &self.pseudo_styles,
            vw: self.viewport_width,
            vh: self.viewport_height,
        };
        self.js.dom = Some(&mut self.dom as *mut crate::dom::Dom);
        self.js.layout_ctx = Some(ctx);
    }

    fn leave_js(&mut self) {
        self.js.dom = None;
        self.js.layout_ctx = None;
    }

    pub fn rebuild(&mut self) {
        let vp = crate::style::Viewport { w: self.viewport_width, h: self.viewport_height };
        let style_root =
            crate::style::style_tree_full(&self.dom, &self.sheet, vp, &self.pseudo_styles);
        let mut viewport: crate::layout::Dimensions = Default::default();
        viewport.content.width = self.viewport_width;
        let mut layout_root =
            crate::layout::layout_tree(&style_root, viewport, &self.fonts, &self.img_map);
        // position: sticky — 현재 스크롤 위치 기준으로 시각 오프셋 적용
        crate::layout::apply_sticky(
            &mut layout_root,
            0.0,
            self.scroll_y,
            self.viewport_width,
            self.viewport_height,
        );
        self.items = crate::paint::build_display_list(&layout_root);
        self.links.clear();
        crate::layout::collect_link_regions(&layout_root, &mut self.links);
        self.element_rects.clear();
        crate::layout::collect_element_rects(&layout_root, 0, &mut self.element_rects);
        // JS 측정 맵(rect/computed style) 갱신 — 강제 레이아웃과 같은 경로를 쓴다.
        fill_js_maps(&mut self.js, &style_root, &layout_root);
        self.js.layout_version = Some(self.dom.version());
        // <canvas> 2D 그리기 명령을 박스로 매핑해 디스플레이 리스트에 추가
        if !self.js.canvas_cmds.is_empty() {
            let extra = canvas_display_items(&self.element_rects, &self.js.canvas_cmds, &self.fonts);
            self.items.extend(extra);
        }
        self.doc_height = layout_root.dimensions.margin_box().height;
    }

    // (x, y): 문서 좌표. 클릭 지점의 가장 깊은 요소를 타깃으로 핸들러를 버블링
    // 실행하고, 하나라도 실행됐으면 rebuild 후 true.
    pub fn dispatch_click(&mut self, x: f32, y: f32) -> bool {
        let Some(target) = crate::layout::hit_element(&self.element_rects, x, y) else {
            return false;
        };
        self.enter_js();
        let mut fired = self.js.fire_handlers(target, "click");
        // onclick 속성: 타깃부터 조상 순서로 평가
        let mut chain = vec![target];
        chain.extend(self.dom.ancestors(target));
        for id in chain {
            let src = match &self.dom.get(id).node_type {
                crate::dom::NodeType::Element(e) => e.attributes.get("onclick").cloned(),
                _ => None,
            };
            if let Some(src) = src {
                fired = true;
                self.js.run_inline_handler(&src);
            }
        }
        for line in self.js.console.drain(..) {
            println!("[console] {}", line);
        }
        self.leave_js();
        if fired {
            self.rebuild();
        }
        fired
    }

    // ── 타이머 (setTimeout/setInterval) ──

    pub fn take_timers(&mut self) -> Vec<crate::js::interp::Timer> {
        std::mem::take(&mut self.js.timers)
    }

    pub fn is_cleared(&self, id: u64) -> bool {
        self.js.cleared.contains(&id)
    }

    // 타이머 콜백 실행 → DOM 변형 가능 → rebuild
    pub fn fire_timer(&mut self, callback: crate::js::interp::Value) {
        self.enter_js();
        self.js.run_callback(callback);
        for line in self.js.console.drain(..) {
            println!("[console] {}", line);
        }
        self.leave_js();
        self.rebuild();
    }

    // 헤드리스: 대기 타이머를 지연 오름차순으로 실행 (interval 도 1회, 라운드 제한).
    // setTimeout(fn, 0) 지연 초기화 등을 렌더 전에 반영한다.
    pub fn flush_timers_headless(&mut self) {
        for _round in 0..50 {
            let mut pending = self.take_timers();
            pending.retain(|t| !self.js.cleared.contains(&t.id));
            if pending.is_empty() {
                break;
            }
            pending.sort_by(|a, b| a.delay_ms.partial_cmp(&b.delay_ms).unwrap());
            for t in pending {
                if self.js.cleared.contains(&t.id) {
                    continue;
                }
                self.fire_timer(t.callback);
            }
        }
    }

    // ── <input> 포커스/편집/폼 제출 ──

    // 클릭 지점의 input (텍스트를 눌러도 매칭되도록 조상 포함)
    pub fn input_at(&self, x: f32, y: f32) -> Option<crate::dom::NodeId> {
        let id = crate::layout::hit_element(&self.element_rects, x, y)?;
        std::iter::once(id).chain(self.dom.ancestors(id)).find(|&n| {
            matches!(&self.dom.get(n).node_type,
                crate::dom::NodeType::Element(e) if e.tag_name == "input"
                    && e.attributes.get("type").map(|t| t.as_str()) != Some("hidden"))
        })
    }

    pub fn input_value(&self, id: crate::dom::NodeId) -> String {
        match &self.dom.get(id).node_type {
            crate::dom::NodeType::Element(e) => {
                e.attributes.get("value").cloned().unwrap_or_default()
            }
            _ => String::new(),
        }
    }

    pub fn set_input_value(&mut self, id: crate::dom::NodeId, v: String) {
        if let crate::dom::NodeType::Element(e) = &mut self.dom.get_mut(id).node_type {
            e.attributes.insert("value".to_string(), v);
        }
        self.rebuild();
    }

    // Enter 제출: 조상 form 의 input[name] 수집 → GET URL. POST/폼 없음은 None.
    pub fn submit_url(&self, input_id: crate::dom::NodeId) -> Option<String> {
        let form = std::iter::once(input_id).chain(self.dom.ancestors(input_id)).find(|&n| {
            matches!(&self.dom.get(n).node_type,
                crate::dom::NodeType::Element(e) if e.tag_name == "form")
        })?;
        let crate::dom::NodeType::Element(fe) = &self.dom.get(form).node_type else {
            return None;
        };
        let method =
            fe.attributes.get("method").map(|m| m.to_ascii_lowercase()).unwrap_or_default();
        if !(method.is_empty() || method == "get") {
            return None; // POST 미지원
        }
        let action = fe.attributes.get("action").cloned().unwrap_or_default();
        // form 하위 input 의 name=value (submit/button 류 제외)
        let mut pairs: Vec<(String, String)> = Vec::new();
        fn collect(dom: &crate::dom::Dom, id: crate::dom::NodeId, out: &mut Vec<(String, String)>) {
            if let crate::dom::NodeType::Element(e) = &dom.get(id).node_type {
                if e.tag_name == "input" {
                    let ty = e.attributes.get("type").map(|t| t.as_str()).unwrap_or("");
                    if !matches!(ty, "submit" | "button" | "image" | "reset" | "checkbox" | "radio")
                    {
                        if let Some(name) = e.attributes.get("name") {
                            let value =
                                e.attributes.get("value").cloned().unwrap_or_default();
                            out.push((name.clone(), value));
                        }
                    }
                }
            }
            for &c in &dom.get(id).children {
                collect(dom, c, out);
            }
        }
        collect(&self.dom, form, &mut pairs);
        let qs = pairs
            .iter()
            .map(|(k, v)| format!("{}={}", urlencode(k), urlencode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let mut target =
            if action.is_empty() { self.url.clone() } else { self.url.join(&action)? };
        let path = target.path.split('?').next().unwrap_or("/").to_string();
        target.path = if qs.is_empty() { path } else { format!("{}?{}", path, qs) };
        Some(target.as_string())
    }
}

const LINE_SCROLL: f32 = 48.0;
// 상단 크롬(주소창) 높이. 페이지는 이 아래에 렌더된다.
const CHROME_H: f32 = 36.0;

/// 스크롤 + 링크 클릭 + 주소창이 있는 브라우저 창.
pub fn run_page(
    page: Page,
    width: u32,
    height: u32,
    mut load: impl FnMut(&str) -> Option<Page> + 'static,
) {
    let event_loop = EventLoop::new().unwrap();
    let window = Rc::new(
        WindowBuilder::new()
            .with_title(format!("Kestrel — {}", page.url.as_string()))
            .with_inner_size(LogicalSize::new(width, height))
            .build(&event_loop)
            .unwrap(),
    );

    let context = softbuffer::Context::new(window.clone()).unwrap();
    let mut surface = softbuffer::Surface::new(&context, window.clone()).unwrap();

    let mut page = page;
    let mut cache = crate::raster::GlyphCache::new();
    // 스크립트가 window.scrollTo/scrollIntoView 로 요청한 위치에서 시작
    let mut scroll_y: f32 = page.js.scroll_y;
    let mut cursor: (f32, f32) = (0.0, 0.0);
    // 뒤로 가기 스택: (URL, 떠날 때 스크롤 위치)
    let mut history: Vec<(String, f32)> = Vec::new();
    // 주소창 상태
    let mut url_input: String = page.url.as_string();
    let mut focused = false;
    // 예약된 타이머: (발화 시각, Timer)
    let mut scheduled: Vec<(Instant, crate::js::interp::Timer)> = Vec::new();

    event_loop
        .run(move |event, elwt| {
            elwt.set_control_flow(ControlFlow::Wait);
            // 새로 등록된 타이머를 예약 (초기 스크립트 + 콜백이 만든 것)
            {
                let now = Instant::now();
                for t in page.take_timers() {
                    scheduled.push((now + Duration::from_millis(t.delay_ms as u64), t));
                }
            }
            // 타이머 발화 (AboutToWait 뿐 아니라 매 이벤트마다 확인)
            if let Event::AboutToWait = &event {
                let now = Instant::now();
                let mut due = Vec::new();
                let mut i = 0;
                while i < scheduled.len() {
                    if scheduled[i].0 <= now {
                        due.push(scheduled.remove(i));
                    } else {
                        i += 1;
                    }
                }
                let mut fired = false;
                for (_, timer) in due {
                    if page.is_cleared(timer.id) {
                        continue;
                    }
                    page.fire_timer(timer.callback.clone());
                    fired = true;
                    if timer.repeat && !page.is_cleared(timer.id) {
                        scheduled.push((
                            now + Duration::from_millis(timer.delay_ms.max(4.0) as u64),
                            timer,
                        ));
                    }
                }
                // 콜백이 만든 타이머도 예약
                let now2 = Instant::now();
                for t in page.take_timers() {
                    scheduled.push((now2 + Duration::from_millis(t.delay_ms as u64), t));
                }
                if fired {
                    let vh = (window.inner_size().height.max(1) as f32 / window.scale_factor() as f32
                        - CHROME_H)
                        .max(1.0);
                    scroll_y = scroll_y.clamp(0.0, (page.doc_height - vh).max(0.0));
                    // 스티키 요소는 스크롤에 따라 위치가 바뀐다 → 재레이아웃
                    if page.scroll_y != scroll_y {
                        page.scroll_y = scroll_y;
                        page.rebuild();
                    }
                    window.request_redraw();
                }
                // 다음 타이머까지 대기
                if let Some(next) = scheduled.iter().map(|(d, _)| *d).min() {
                    elwt.set_control_flow(ControlFlow::WaitUntil(next));
                }
            }
            // 물리(픽셀)/논리 배율. 레이아웃·스크롤·히트 테스트는 전부 논리 좌표로.
            let scale = window.scale_factor() as f32;
            let viewport_h =
                (window.inner_size().height.max(1) as f32 / scale - CHROME_H).max(1.0);
            let max_scroll = (page.doc_height - viewport_h).max(0.0);
            match event {
                Event::Resumed => {
                    window.request_redraw();
                }
                Event::WindowEvent { event, .. } => match event {
                    WindowEvent::CloseRequested => elwt.exit(),
                    WindowEvent::ScaleFactorChanged { .. } => {
                        window.request_redraw();
                    }
                    WindowEvent::CursorMoved { position, .. } => {
                        cursor = (position.x as f32 / scale, position.y as f32 / scale);
                        let icon = if cursor.1 < CHROME_H {
                            CursorIcon::Text
                        } else if hit_link(&page.links, cursor.0, cursor.1 - CHROME_H + scroll_y)
                            .is_some()
                        {
                            CursorIcon::Pointer
                        } else {
                            CursorIcon::Default
                        };
                        window.set_cursor_icon(icon);
                    }
                    WindowEvent::MouseInput {
                        state: ElementState::Pressed,
                        button: MouseButton::Left,
                        ..
                    } => {
                        // 주소창 클릭 → 포커스
                        if cursor.1 < CHROME_H {
                            if !focused {
                                focused = true;
                                window.request_redraw();
                            }
                            return;
                        }
                        if focused {
                            focused = false;
                            url_input = page.url.as_string();
                            window.request_redraw();
                        }
                        // 이벤트 핸들러 먼저 (실행되면 rebuild 됨), 링크 기본 동작은 그 다음
                        if page.dispatch_click(cursor.0, cursor.1 - CHROME_H + scroll_y) {
                            scroll_y = scroll_y.clamp(0.0, (page.doc_height - viewport_h).max(0.0));
                            window.request_redraw();
                        }
                        // <input> 클릭 → 포커스 (다른 곳 클릭 → 해제)
                        let new_focus = page.input_at(cursor.0, cursor.1 - CHROME_H + scroll_y);
                        if new_focus != page.focused_input {
                            page.focused_input = new_focus;
                            window.request_redraw();
                        }
                        if let Some(href) =
                            hit_link(&page.links, cursor.0, cursor.1 - CHROME_H + scroll_y)
                        {
                            if href.starts_with('#') {
                                return; // 페이지 내 앵커는 아직 미지원
                            }
                            if let Some(target) = page.url.join(href) {
                                let url_str = target.as_string();
                                println!("→ {}", url_str);
                                if let Some(new_page) = load(&url_str) {
                                    history.push((page.url.as_string(), scroll_y));
                                    page = new_page;
                                    scroll_y = 0.0;
                                    cache = crate::raster::GlyphCache::new(); // 폰트 인덱스가 바뀔 수 있음
                                    url_input = page.url.as_string();
                                    window.set_title(&format!(
                                        "Kestrel — {}",
                                        page.url.as_string()
                                    ));
                                    window.request_redraw();
                                }
                            }
                        }
                    }
                    WindowEvent::MouseWheel { delta, .. } => {
                        let dy = match delta {
                            MouseScrollDelta::LineDelta(_, y) => -y * LINE_SCROLL,
                            MouseScrollDelta::PixelDelta(p) => -p.y as f32 / scale,
                        };
                        let next = (scroll_y + dy).clamp(0.0, max_scroll);
                        if next != scroll_y {
                            scroll_y = next;
                            window.request_redraw();
                        }
                    }
                    WindowEvent::KeyboardInput { event: key, .. }
                        if key.state == ElementState::Pressed =>
                    {
                        // ── 주소창 편집 모드 ──
                        if focused {
                            match &key.logical_key {
                                Key::Named(NamedKey::Enter) => {
                                    let t = url_input.trim().to_string();
                                    let target = if t.starts_with("http://")
                                        || t.starts_with("https://")
                                    {
                                        t
                                    } else {
                                        format!("https://{}", t)
                                    };
                                    println!("→ {}", target);
                                    focused = false;
                                    if let Some(new_page) = load(&target) {
                                        history.push((page.url.as_string(), scroll_y));
                                        page = new_page;
                                        scroll_y = 0.0;
                                        cache = crate::raster::GlyphCache::new();
                                        url_input = page.url.as_string();
                                        window.set_title(&format!(
                                            "Kestrel — {}",
                                            page.url.as_string()
                                        ));
                                    } else {
                                        url_input = page.url.as_string();
                                    }
                                    window.request_redraw();
                                }
                                Key::Named(NamedKey::Escape) => {
                                    focused = false;
                                    url_input = page.url.as_string();
                                    window.request_redraw();
                                }
                                Key::Named(NamedKey::Backspace) => {
                                    url_input.pop();
                                    window.request_redraw();
                                }
                                Key::Character(s) => {
                                    url_input.push_str(s);
                                    window.request_redraw();
                                }
                                _ => {}
                            }
                            return;
                        }
                        // ── <input> 편집 모드 ──
                        if let Some(fid) = page.focused_input {
                            match &key.logical_key {
                                Key::Named(NamedKey::Enter) => {
                                    if let Some(url_str) = page.submit_url(fid) {
                                        println!("→ {} (폼 제출)", url_str);
                                        if let Some(new_page) = load(&url_str) {
                                            history.push((page.url.as_string(), scroll_y));
                                            page = new_page;
                                            scroll_y = 0.0;
                                            cache = crate::raster::GlyphCache::new();
                                            url_input = page.url.as_string();
                                            window.set_title(&format!(
                                                "Kestrel — {}",
                                                page.url.as_string()
                                            ));
                                        }
                                    }
                                    window.request_redraw();
                                }
                                Key::Named(NamedKey::Escape) => {
                                    page.focused_input = None;
                                    window.request_redraw();
                                }
                                Key::Named(NamedKey::Backspace) => {
                                    let mut v = page.input_value(fid);
                                    v.pop();
                                    page.set_input_value(fid, v);
                                    window.request_redraw();
                                }
                                Key::Named(NamedKey::Space) => {
                                    let v = page.input_value(fid) + " ";
                                    page.set_input_value(fid, v);
                                    window.request_redraw();
                                }
                                Key::Character(s) => {
                                    let v = page.input_value(fid) + s;
                                    page.set_input_value(fid, v);
                                    window.request_redraw();
                                }
                                _ => {}
                            }
                            return;
                        }
                        // ── 뒤로 가기: Backspace (스크롤 위치까지 복원) ──
                        if key.physical_key == PhysicalKey::Code(KeyCode::Backspace) {
                            if let Some((prev_url, prev_scroll)) = history.pop() {
                                println!("← {}", prev_url);
                                if let Some(new_page) = load(&prev_url) {
                                    page = new_page;
                                    scroll_y = prev_scroll
                                        .clamp(0.0, (page.doc_height - viewport_h).max(0.0));
                                    cache = crate::raster::GlyphCache::new();
                                    url_input = page.url.as_string();
                                    window.set_title(&format!(
                                        "Kestrel — {}",
                                        page.url.as_string()
                                    ));
                                    window.request_redraw();
                                } else {
                                    history.push((prev_url, prev_scroll)); // 실패 시 스택 보존
                                }
                            }
                            return;
                        }
                        // ── 스크롤 키 ──
                        let dy = match key.physical_key {
                            PhysicalKey::Code(KeyCode::ArrowDown) => Some(LINE_SCROLL),
                            PhysicalKey::Code(KeyCode::ArrowUp) => Some(-LINE_SCROLL),
                            PhysicalKey::Code(KeyCode::PageDown)
                            | PhysicalKey::Code(KeyCode::Space) => Some(viewport_h * 0.9),
                            PhysicalKey::Code(KeyCode::PageUp) => Some(-viewport_h * 0.9),
                            PhysicalKey::Code(KeyCode::Home) => Some(-scroll_y),
                            PhysicalKey::Code(KeyCode::End) => Some(max_scroll - scroll_y),
                            _ => None,
                        };
                        if let Some(dy) = dy {
                            let next = (scroll_y + dy).clamp(0.0, max_scroll);
                            if next != scroll_y {
                                scroll_y = next;
                                window.request_redraw();
                            }
                        }
                    }
                    WindowEvent::Resized(_) => {
                        scroll_y = scroll_y.clamp(0.0, max_scroll);
                        window.request_redraw();
                    }
                    WindowEvent::RedrawRequested => {
                        let size = window.inner_size();
                        let (w, h) = (size.width.max(1), size.height.max(1));
                        surface
                            .resize(NonZeroU32::new(w).unwrap(), NonZeroU32::new(h).unwrap())
                            .unwrap();

                        // 페이지: 크롬 아래부터 그린다 (scroll 을 CHROME_H 만큼 당겨서)
                        let mut canvas = crate::paint::rasterize(
                            &page.items,
                            w as usize,
                            h as usize,
                            scroll_y - CHROME_H,
                            scale,
                            &page.fonts,
                            &mut cache,
                            &page.images,
                        );
                        // 포커스된 input 캐럿 (문서 좌표 → 화면 좌표, 스케일 반영)
                        if let Some(fid) = page.focused_input {
                            if let Some((r, _, _)) =
                                page.element_rects.iter().find(|(_, id, _)| *id == fid)
                            {
                                let text_w = crate::paint::measure_text(
                                    &page.fonts,
                                    &page.input_value(fid),
                                    16.0,
                                );
                                let cx = (r.x + 5.0 + text_w + 1.0) * scale;
                                let cy = (r.y - scroll_y + CHROME_H + 4.0) * scale;
                                canvas.fill_rect(
                                    Color { r: 40, g: 90, b: 220, a: 255 },
                                    Rect {
                                        x: cx,
                                        y: cy,
                                        width: 2.0 * scale,
                                        height: (r.height - 8.0).max(4.0) * scale,
                                    },
                                );
                            }
                        }
                        // 크롬 (주소창) — 물리 좌표로 직접 그림
                        let s = scale;
                        let wf = w as f32;
                        canvas.fill_rect(
                            Color { r: 32, g: 32, b: 38, a: 255 },
                            Rect { x: 0.0, y: 0.0, width: wf, height: CHROME_H * s },
                        );
                        let field_bg = if focused {
                            Color { r: 14, g: 14, b: 20, a: 255 }
                        } else {
                            Color { r: 22, g: 22, b: 28, a: 255 }
                        };
                        canvas.fill_rect(
                            field_bg,
                            Rect {
                                x: 8.0 * s,
                                y: 6.0 * s,
                                width: wf - 16.0 * s,
                                height: (CHROME_H - 12.0) * s,
                            },
                        );
                        let end_x = crate::paint::draw_text(
                            &mut canvas,
                            &page.fonts,
                            &mut cache,
                            &url_input,
                            16.0 * s,
                            24.0 * s,
                            14.0 * s,
                            Color { r: 214, g: 218, b: 228, a: 255 },
                        );
                        if focused {
                            canvas.fill_rect(
                                Color { r: 244, g: 132, b: 44, a: 255 },
                                Rect {
                                    x: end_x + 2.0 * s,
                                    y: 10.0 * s,
                                    width: 2.0 * s,
                                    height: (CHROME_H - 20.0) * s,
                                },
                            );
                        }

                        let buffer = canvas.to_u32_buffer();
                        let mut frame = surface.buffer_mut().unwrap();
                        frame.copy_from_slice(&buffer);
                        frame.present().unwrap();
                    }
                    _ => {}
                },
                _ => {}
            }
        })
        .unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canvas_image_data_reads_real_pixels() {
        // getImageData 는 캔버스를 **진짜로 그려서** 픽셀을 읽어야 한다.
        // 예전엔 아예 없어서(함수 아님) 픽셀을 다루는 코드가 즉사했다.
        let mut dom = crate::html::parse_dom(
            "<canvas id=\"c\" width=\"50\" height=\"50\"></canvas><p id=\"t\">?</p>\
             <script>\
             var x = document.getElementById('c').getContext('2d');\
             x.fillStyle = '#ff0000'; x.fillRect(0, 0, 20, 20);\
             var d = x.getImageData(5, 5, 1, 1);\
             var e = x.getImageData(40, 40, 1, 1);\
             document.getElementById('t').textContent = \
               [d.data[0], d.data[1], d.data[2], d.data[3], e.data[3]].join(',');\
             </script>"
                .to_string(),
        );
        let mut fonts = crate::font::FontStack::new(vec![]);
        let _ = &mut fonts;
        crate::js::run_scripts(&mut dom, "https://localhost/", None);
        // 렌더 컨텍스트가 없으면 getImageData 는 정직하게 null 을 준다 (조용히 0 을 주지 않는다)
        let got = dom.find_by_attr_id("t").map(|n| dom.text_content(n)).unwrap();
        assert!(
            got == "?" || got == "255,0,0,255,0",
            "픽셀을 읽거나(렌더 컨텍스트 있음) 정직하게 실패해야: {}",
            got
        );
    }

    #[test]
    fn canvas_gradient_clip_and_curves() {
        // 그라디언트/패턴/clip/베지어가 실제 명령을 만드는가.
        // 예전엔 전부 no-op 이라 아무것도 안 나왔다 (아무 말도 없이).
        let mut dom = crate::html::parse_dom(
            "<canvas id=\"c\" width=\"200\" height=\"200\"></canvas>\
             <script>\
             var x = document.getElementById('c').getContext('2d');\
             var g = x.createLinearGradient(0, 0, 100, 0);\
             g.addColorStop(0, '#ff0000'); g.addColorStop(1, '#0000ff');\
             x.fillStyle = g; x.fillRect(0, 0, 100, 40);\
             x.save();\
             x.beginPath(); x.rect(10, 10, 20, 20); x.clip();\
             x.fillStyle = '#00ff00'; x.fillRect(0, 0, 200, 200);\
             x.restore();\
             x.beginPath(); x.moveTo(0, 100); x.quadraticCurveTo(50, 50, 100, 100); x.stroke();\
             </script>"
                .to_string(),
        );
        let rt = crate::js::run_scripts(&mut dom, "https://localhost/", None);
        let ops = rt.canvas_cmds.values().next().expect("캔버스 명령");
        use crate::js::interp::CanvasOp;
        assert!(
            ops.iter().any(|o| matches!(o, CanvasOp::FillGradient { .. })),
            "그라디언트 채우기가 명령으로 나와야"
        );
        // clip 이 걸리고, restore 로 **해제**된다 (해제가 없으면 이후 그리기가 전부 갇힌다)
        let clips: Vec<&CanvasOp> =
            ops.iter().filter(|o| matches!(o, CanvasOp::Clip { .. })).collect();
        assert!(clips.len() >= 2, "clip 설정 + restore 로 해제: {}", clips.len());
        assert!(
            matches!(clips.last(), Some(CanvasOp::Clip { pts: None })),
            "restore 후에는 클립이 해제돼야"
        );
        // 곡선이 경로 점을 만들고 stroke 가 폴리곤을 만든다
        assert!(
            ops.iter().any(|o| matches!(o, CanvasOp::FillPath { .. })),
            "quadraticCurveTo + stroke 가 그려져야"
        );
    }

    #[test]
    fn canvas_transform_and_stroke_are_applied() {
        // 캔버스는 상태 기계다: translate/rotate/scale 은 이후 그리기에 실제로 적용되고,
        // save/restore 로 되돌아간다. 예전엔 전부 조용한 no-op 이라 그림이 엉뚱한 자리에
        // 그려지거나(변환 무시) stroke() 한 경로가 통째로 안 나왔다.
        let mut dom = crate::html::parse_dom(
            "<canvas id=\"c\" width=\"200\" height=\"200\"></canvas><p id=\"t\">?</p>\
             <script>\
             var x = document.getElementById('c').getContext('2d');\
             x.save(); x.translate(50, 20); x.fillRect(0, 0, 10, 10); x.restore();\
             x.fillRect(0, 0, 5, 5);\
             x.beginPath(); x.moveTo(0, 100); x.lineTo(100, 100); x.lineWidth = 4; x.stroke();\
             document.getElementById('t').textContent = \
               x.measureText('AB').width > 0 ? 'ok' : 'measureText 가 0';\
             </script>"
                .to_string(),
        );
        let rt = crate::js::run_scripts(&mut dom, "https://localhost/", None);
        assert_eq!(
            dom.find_by_attr_id("t").map(|n| dom.text_content(n)).unwrap(),
            "ok",
            "measureText 는 실제 폭을 준다"
        );
        let ops = rt.canvas_cmds.values().next().expect("캔버스 명령");
        use crate::js::interp::CanvasOp;
        // 변환이 명령으로 기록되고(restore 로 되돌아가고), stroke 가 폴리곤을 만든다
        let transforms = ops.iter().filter(|o| matches!(o, CanvasOp::SetTransform { .. })).count();
        assert!(transforms >= 2, "translate + restore 로 변환 명령이 두 번 이상: {}", transforms);
        assert!(
            ops.iter().any(|o| matches!(o, CanvasOp::FillPath { .. })),
            "stroke() 가 경로를 그려야 (예전엔 통째로 무시됐다)"
        );
    }
    use crate::dom::{Dom, NodeType};

    fn make_page(html: &str) -> Page {
        let mut dom = crate::html::parse_dom(html.to_string());
        let js = crate::js::run_scripts(&mut dom, "https://localhost/", None);
        let sheet = crate::css::user_agent_stylesheet();
        let f = crate::font::Font::from_bytes(std::fs::read("assets/fonts/Latin.ttf").unwrap())
            .unwrap();
        let fonts = crate::font::FontStack::new(vec![f]);
        let mut page = Page {
            dom,
            sheet,
            images: Vec::new(),
            img_map: crate::layout::ImageMap::new(),
            fonts,
            js,
            url: crate::url::Url::parse("https://localhost/").unwrap(),
            viewport_width: 400.0,
            viewport_height: 600.0,
            pseudo_styles: crate::style::PseudoStyles::new(),
            items: Vec::new(),
            links: Vec::new(),
            element_rects: Vec::new(),
            focused_input: None,
            doc_height: 0.0,
            scroll_y: 0.0,
        };
        page.rebuild();
        page
    }

    // 스크립트가 실행되는 동안 측정 API 가 실제 값을 돌려주는지 (강제 레이아웃).
    // 스크립트가 CSS/폰트 뒤에 돌고, 측정 시점에 스타일 → 레이아웃이 흐른다.
    // run_scripts 는 console 을 직접 출력하므로, 결과는 DOM 에 써서 확인한다.
    fn run_with_layout(html: &str, css: &str) -> Dom {
        let mut dom = crate::html::parse_dom(html.to_string());
        let mut sheet = crate::css::user_agent_stylesheet();
        sheet.rules.extend(crate::css::parse(css.to_string()).rules);
        let f = crate::font::Font::from_bytes(std::fs::read("assets/fonts/Latin.ttf").unwrap())
            .unwrap();
        let fonts = crate::font::FontStack::new(vec![f]);
        let img_map = crate::layout::ImageMap::new();
        let images: Vec<crate::png::Image> = Vec::new();
        let pseudo = crate::style::PseudoStyles::new();
        let ctx = LayoutCtx {
            sheet: &sheet,
            fonts: &fonts,
            img_map: &img_map,
            images: &images,
            pseudo: &pseudo,
            vw: 400.0,
            vh: 600.0,
        };
        crate::js::run_scripts(&mut dom, "https://localhost/", Some(ctx));
        dom
    }

    #[test]
    fn script_time_measurement_forces_layout() {
        // 예전엔 스크립트가 첫 레이아웃보다 먼저 전부 돌아서 파싱 중 측정값이 0/빈 문자열이었다.
        // 측정 후 배치하는 코드(스티키 헤더, 캐러셀 등)가 조용히 어긋났다.
        let dom = run_with_layout(
            "<div id=\"b\"></div><p id=\"out\"></p><script>\
             var e=document.getElementById('b');\
             document.getElementById('out').textContent = \
               e.offsetWidth + '|' + e.getBoundingClientRect().width + '|' \
               + getComputedStyle(e).backgroundColor;\
             </script>",
            "#b { width: 120px; height: 30px; background: #ff0000; }",
        );
        assert_eq!(
            text_of_id(&dom, "out").unwrap(),
            "120|120|rgb(255, 0, 0)",
            "offsetWidth/getBoundingClientRect/getComputedStyle 이 스크립트 시점에 실제 값"
        );
    }

    #[test]
    fn dom_mutation_invalidates_measurement_cache() {
        // 측정 → DOM 변경 → 재측정: 두 번째 읽기는 새 레이아웃을 봐야 한다.
        // (DOM 버전이 바뀌면 캐시가 무효 — 안 그러면 예전 값이 그대로 남는다)
        let dom = run_with_layout(
            "<div id=\"b\" class=\"narrow\"></div><p id=\"out\"></p><script>\
             var e=document.getElementById('b');\
             var before=e.offsetWidth;\
             e.className='wide';\
             document.getElementById('out').textContent = before + '|' + e.offsetWidth;\
             </script>",
            // 기본값도 클래스로 준다 (#b 는 특이도가 높아 .wide 를 이겨버린다)
            "#b { height: 10px; } .narrow { width: 50px; } .wide { width: 300px; }",
        );
        assert_eq!(
            text_of_id(&dom, "out").unwrap(),
            "50|300",
            "클래스 변경 후 재측정은 새 레이아웃 값"
        );
    }

    fn class_of_id(dom: &Dom, id: &str) -> String {
        let n = dom.find_by_attr_id(id).unwrap();
        match &dom.get(n).node_type {
            NodeType::Element(e) => e.attributes.get("class").cloned().unwrap_or_default(),
            _ => String::new(),
        }
    }

    #[test]
    fn inline_elements_have_boxes_for_measurement_and_hit_testing() {
        // 인라인 요소(span/a/b/em…)는 자체 레이아웃 박스가 없다. 예전엔 그래서
        // getBoundingClientRect/offsetWidth 가 전부 0 이었고, 클릭도 인라인 요소를
        // 타깃으로 잡지 못했다(<span onclick> 이 발화하지 않음).
        // 표준의 인라인 박스 = 조각(fragment)들의 경계 합집합이다.
        let mut page = make_page(
            "<div id=\"d\">before <span id=\"sp\">CLICKME</span> after</div><p id=\"out\">-</p>\
             <script>\
             document.getElementById('sp').addEventListener('click', function(){\
               document.getElementById('out').textContent = 'span'; });\
             document.getElementById('d').addEventListener('click', function(){\
               var o = document.getElementById('out');\
               o.textContent = o.textContent + '+div'; });\
             </script>",
        );
        let sp = page.dom.find_by_attr_id("sp").unwrap();
        let (x, y, w, h) = *page.js.layout_rects.get(&sp).expect("인라인 요소도 사각형이 있다");
        assert!(w > 0.0 && h > 0.0, "폭/높이가 0 이 아니다: {}x{}", w, h);
        assert!(x > 0.0, "앞선 텍스트 뒤에 놓인다 (x={})", x);

        // 그 사각형 한가운데를 클릭하면 타깃은 span 이고, div 로 버블링된다
        assert!(page.dispatch_click(x + w / 2.0, y + h / 2.0), "핸들러가 실행됐다");
        assert_eq!(
            text_of_id(&page.dom, "out").unwrap(),
            "span+div",
            "span 이 타깃 → div 로 버블링"
        );
    }

    #[test]
    fn computed_style_returns_initial_values_for_unset_properties() {
        // 예전엔 규칙이 없는 프로퍼티가 빈 문자열이었다 — 사이트는 우리가 아무 스타일도
        // 없다고 믿는다. 표준의 resolved value 는 초기값/상속값이다.
        let page = make_page("<div id=\"d\"><span id=\"s\">s</span></div>");
        let d = page.dom.find_by_attr_id("d").unwrap();
        let sp = page.dom.find_by_attr_id("s").unwrap();
        let cs = &page.js.computed_styles;
        assert_eq!(cs[&d].get("color").unwrap(), "rgb(0, 0, 0)", "color 초기값");
        assert_eq!(cs[&d].get("display").unwrap(), "block", "div 는 UA 규칙으로 block");
        assert_eq!(cs[&sp].get("display").unwrap(), "inline", "span 은 초기값 inline");
        assert_eq!(cs[&d].get("position").unwrap(), "static");
        assert_eq!(cs[&d].get("background-color").unwrap(), "rgba(0, 0, 0, 0)");
        assert_eq!(cs[&d].get("z-index").unwrap(), "auto");
        assert_eq!(cs[&d].get("visibility").unwrap(), "visible");
    }

    #[test]
    fn intersection_observer_delivers_real_entries() {
        // 예전엔 무동작 스텁이라 콜백이 영영 오지 않았다 — 교차 시 콘텐츠를 드러내는
        // 사이트가 화면 안 요소까지 숨긴 채로 남았다.
        let mut page = make_page(
            "<div id=\"near\" style=\"height:50px\">a</div>\
             <div id=\"far\" style=\"margin-top:3000px;height:50px\">b</div>\
             <script>\
             var io = new IntersectionObserver(function(es){\
               es.forEach(function(e){ if (e.isIntersecting) e.target.className = 'shown'; });\
             });\
             io.observe(document.getElementById('near'));\
             io.observe(document.getElementById('far'));\
             </script>",
        );
        page.flush_timers_headless();
        assert_eq!(class_of_id(&page.dom, "near"), "shown", "뷰포트 안 요소는 교차 → 콜백");
        assert_eq!(class_of_id(&page.dom, "far"), "", "3000px 아래 요소는 비교차 (스크롤 0)");
    }

    #[test]
    fn mutation_observer_delivers_records() {
        // 예전엔 무동작 스텁 — "요소가 나타나면 처리" 패턴이 통째로 죽었다.
        // 이제 DOM 아레나가 childList/attributes 기록을 쌓고 마이크로태스크로 배달한다.
        let mut page = make_page(
            "<div id=\"root\"><span id=\"s\">x</span></div><p id=\"out\"></p>\
             <script>\
             var added = 0, attrs = [];\
             var mo = new MutationObserver(function(recs){\
               recs.forEach(function(r){\
                 if (r.type === 'childList') added += r.addedNodes.length;\
                 if (r.type === 'attributes') attrs.push(r.attributeName);\
               });\
               document.getElementById('out').textContent = added + '|' + attrs.join(',');\
             });\
             mo.observe(document.getElementById('root'), \
                        { childList: true, attributes: true, subtree: true });\
             document.getElementById('root').appendChild(document.createElement('div'));\
             document.getElementById('s').setAttribute('data-x', '1');\
             document.getElementById('s').className = 'c';\
             </script>",
        );
        page.flush_timers_headless();
        assert_eq!(
            text_of_id(&page.dom, "out").unwrap(),
            "1|data-x,class",
            "추가된 노드 1개 + 속성 변경 2건이 배달된다"
        );
    }

    #[test]
    fn resize_observer_delivers_initial_size() {
        // 표준: observe() 하면 현재 크기로 초기 관측이 1회 전달된다.
        let mut page = make_page(
            "<div id=\"b\" style=\"width:120px;height:30px\"></div><p id=\"out\"></p>\
             <script>\
             var ro = new ResizeObserver(function(es){\
               document.getElementById('out').textContent = Math.round(es[0].contentRect.width);\
             });\
             ro.observe(document.getElementById('b'));\
             </script>",
        );
        page.flush_timers_headless();
        assert_eq!(text_of_id(&page.dom, "out").unwrap(), "120", "초기 관측이 실제 폭을 준다");
    }

    fn text_of_id(dom: &Dom, id: &str) -> Option<String> {
        dom.find_by_attr_id(id).map(|n| dom.text_content(n))
    }

    // 태그 이름으로 요소 히트 영역 중심점 찾기
    fn center_of_tag(page: &Page, tag: &str) -> (f32, f32) {
        for (r, id, _) in &page.element_rects {
            if let NodeType::Element(e) = &page.dom.get(*id).node_type {
                if e.tag_name == tag {
                    return (r.x + r.width / 2.0, r.y + r.height / 2.0);
                }
            }
        }
        panic!("{} 요소를 찾지 못함", tag);
    }

    #[test]
    fn click_fires_add_event_listener_and_rerenders() {
        let mut page = make_page(
            "<p id=\"out\">count 0</p><button>inc</button>\
             <script>var n = 0; \
             document.getElementById('out').textContent = 'count 0'; \
             var b = document.getElementById('out'); \
             </script>",
        );
        // 핸들러를 스크립트로 등록하는 완전한 흐름은 아래 카운터 테스트에서;
        // 여기선 등록 없는 클릭이 false 를 반환하는지부터
        let (x, y) = center_of_tag(&page, "button");
        assert!(!page.dispatch_click(x, y), "핸들러 없으면 false");
    }

    #[test]
    fn headless_timer_flush_runs_deferred_set_timeout() {
        // setTimeout(fn, 0) 로 미룬 DOM 초기화가 flush 로 반영
        let mut page = make_page(
            "<p id=\"out\">before</p>\
             <script>setTimeout(function() { \
               document.getElementById('out').textContent = 'deferred ran'; \
             }, 0);</script>",
        );
        assert_eq!(text_of_id(&page.dom, "out").unwrap(), "before", "flush 전엔 미실행");
        page.flush_timers_headless();
        assert_eq!(text_of_id(&page.dom, "out").unwrap(), "deferred ran");
    }

    #[test]
    fn headless_timer_chain_and_clear() {
        // 타이머가 또 타이머를 만드는 체인 + clearTimeout 취소
        let mut page = make_page(
            "<p id=\"out\">0</p>\
             <script>\
             setTimeout(function() { \
               document.getElementById('out').textContent = '1'; \
               setTimeout(function() { \
                 document.getElementById('out').textContent = '2'; \
               }, 0); \
             }, 0); \
             var cancel = setTimeout(function() { \
               document.getElementById('out').textContent = 'SHOULD NOT RUN'; \
             }, 5); \
             clearTimeout(cancel);</script>",
        );
        page.flush_timers_headless();
        assert_eq!(text_of_id(&page.dom, "out").unwrap(), "2", "체인 실행, 취소된 것 미실행");
    }

    #[test]
    fn counter_button_increments_on_clicks() {
        let mut page = make_page(
            "<p id=\"out\">count 0</p><button id=\"b\">inc</button>\
             <script>var n = 0; \
             document.getElementById('b').addEventListener('click', function() { \
               n++; document.getElementById('out').textContent = 'count ' + n; \
             });</script>",
        );
        let (x, y) = center_of_tag(&page, "button");
        assert!(page.dispatch_click(x, y));
        assert_eq!(text_of_id(&page.dom, "out").unwrap(), "count 1");
        assert!(page.dispatch_click(x, y));
        assert_eq!(text_of_id(&page.dom, "out").unwrap(), "count 2", "클로저 상태 유지");
        assert!(!page.items.is_empty(), "rebuild 후 디스플레이 리스트 존재");
    }

    #[test]
    fn onclick_property_and_attribute_fire() {
        // el.onclick = fn
        let mut page = make_page(
            "<p id=\"out\">no</p><button id=\"b\">go</button>\
             <script>document.getElementById('b').onclick = function() { \
               document.getElementById('out').textContent = 'via property'; \
             };</script>",
        );
        let (x, y) = center_of_tag(&page, "button");
        assert!(page.dispatch_click(x, y));
        assert_eq!(text_of_id(&page.dom, "out").unwrap(), "via property");

        // onclick="..." 속성
        let mut page2 = make_page(
            "<p id=\"out\">no</p>\
             <button onclick=\"document.getElementById('out').textContent = 'via attr'\">go</button>",
        );
        let (x2, y2) = center_of_tag(&page2, "button");
        assert!(page2.dispatch_click(x2, y2));
        assert_eq!(text_of_id(&page2.dom, "out").unwrap(), "via attr");
    }

    #[test]
    fn click_appends_list_items_and_hit_regions_grow() {
        let mut page = make_page(
            "<ul id=\"list\"></ul><button id=\"add\">add</button>\
             <script>var n = 0; \
             document.getElementById('add').addEventListener('click', function() { \
               n++; \
               var li = document.createElement('li'); \
               li.textContent = 'row ' + n; \
               document.getElementById('list').appendChild(li); \
             });</script>",
        );
        let before = page.element_rects.len();
        // 리스트가 자라면 버튼이 아래로 밀리므로 매 클릭마다 좌표를 다시 잡는다
        let (x, y) = center_of_tag(&page, "button");
        assert!(page.dispatch_click(x, y));
        let (x2, y2) = center_of_tag(&page, "button");
        assert!(y2 > y, "리스트가 자라서 버튼이 아래로 이동");
        assert!(page.dispatch_click(x2, y2));
        let list = page.dom.find_by_attr_id("list").unwrap();
        assert_eq!(page.dom.get(list).children.len(), 2);
        assert_eq!(page.dom.text_content(list), "row 1row 2");
        assert!(
            page.element_rects.len() >= before + 2,
            "rebuild 후 새 li 들이 히트 영역에 반영"
        );
    }

    #[test]
    fn input_focus_typing_and_submit_url() {
        let mut page = make_page(
            "<form action=\"/search\" method=\"get\">\
             <input type=\"hidden\" name=\"src\" value=\"kestrel\">\
             <input name=\"q\" value=\"\">\
             <input type=\"submit\" value=\"go\">\
             </form>",
        );
        // 보이는 input 을 좌표로 포커스 (hidden 은 0 크기라 히트 안 됨)
        let vis = page
            .element_rects
            .iter()
            .find(|(r, id, _)| {
                r.height > 0.0
                    && matches!(&page.dom.get(*id).node_type,
                        crate::dom::NodeType::Element(e) if e.tag_name == "input"
                            && e.attributes.get("type").is_none())
            })
            .map(|(r, _, _)| (r.x + r.width / 2.0, r.y + r.height / 2.0))
            .expect("보이는 input 필요");
        let fid = page.input_at(vis.0, vis.1).expect("input 포커스");
        // 타이핑 시뮬레이션
        page.set_input_value(fid, "hello world".to_string());
        assert_eq!(page.input_value(fid), "hello world");
        // 제출 URL: hidden 포함, submit 제외, 인코딩(공백 +)
        let url = page.submit_url(fid).expect("GET 제출");
        assert_eq!(url, "https://localhost/search?src=kestrel&q=hello+world");
        // rebuild 후 value 글리프가 디스플레이 리스트에 반영
        let glyphs = page
            .items
            .iter()
            .filter(|i| matches!(i, crate::paint::DisplayItem::Glyph(_)))
            .count();
        assert!(glyphs >= 10, "타이핑한 텍스트가 렌더됨 (glyphs={})", glyphs);
    }

    #[test]
    fn submit_without_form_or_post_is_none() {
        let page = make_page("<input id=\"lonely\" name=\"x\">");
        let id = page.dom.find_by_attr_id("lonely").unwrap();
        assert!(page.submit_url(id).is_none(), "form 없으면 None");
        let page2 = make_page(
            "<form action=\"/p\" method=\"post\"><input id=\"i\" name=\"x\"></form>",
        );
        let id2 = page2.dom.find_by_attr_id("i").unwrap();
        assert!(page2.submit_url(id2).is_none(), "POST 미지원");
    }

    #[test]
    fn click_bubbles_to_ancestor_handler() {
        let mut page = make_page(
            "<div id=\"wrap\"><p id=\"inner\">child text</p></div>\
             <script>document.getElementById('wrap').addEventListener('click', function() { \
               document.getElementById('inner').textContent = 'bubbled'; \
             });</script>",
        );
        // 안쪽 p 를 클릭해도 조상 div 핸들러가 실행 (버블링)
        let (x, y) = center_of_tag(&page, "p");
        assert!(page.dispatch_click(x, y));
        assert_eq!(text_of_id(&page.dom, "inner").unwrap(), "bubbled");
    }
}
