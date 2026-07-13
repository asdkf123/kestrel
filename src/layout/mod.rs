use std::collections::HashMap;

use crate::css::Unit::Px;
use crate::css::Value::{Keyword, Length};
use crate::css::{Color, Value};
use crate::dom::NodeType;
use crate::font::FontStack;
use crate::style::{Display, StyledNode};

mod flex;
mod grid;
mod inline;

// src → (이미지 인덱스, 너비, 높이)
pub type ImageMap = HashMap<String, (usize, usize, usize)>;

#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct EdgeSizes {
    pub left: f32,
    pub right: f32,
    pub top: f32,
    pub bottom: f32,
}

#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Dimensions {
    pub content: Rect,
    pub padding: EdgeSizes,
    pub border: EdgeSizes,
    pub margin: EdgeSizes,
}

impl Rect {
    fn expanded_by(self, edge: EdgeSizes) -> Rect {
        Rect {
            x: self.x - edge.left,
            y: self.y - edge.top,
            width: self.width + edge.left + edge.right,
            height: self.height + edge.top + edge.bottom,
        }
    }

    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.width && py >= self.y && py < self.y + self.height
    }
}

impl Dimensions {
    pub fn padding_box(self) -> Rect {
        self.content.expanded_by(self.padding)
    }
    pub fn border_box(self) -> Rect {
        self.padding_box().expanded_by(self.border)
    }
    pub fn margin_box(self) -> Rect {
        self.border_box().expanded_by(self.margin)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct GlyphInstance {
    pub font_index: usize,
    pub glyph_id: u16,
    pub x: f32,
    pub baseline_y: f32,
    pub px: f32,
    pub color: Color,
    // 합성 볼드/이탤릭 (전용 볼드/이탤릭 폰트 부재 시 faux). raster 가 반영.
    pub bold: bool,
    pub italic: bool,
    // transform: rotate 로 회전된 각도(라디안). 0 이면 정상. raster 가 비트맵을 회전.
    pub rot: f32,
}

// 네이티브 폼 컨트롤 표식 — paint 가 폰트 없이 프리미티브로 그린다(체크/라디오/드롭다운 화살표).
#[derive(Clone, Copy, PartialEq)]
pub enum FormControl {
    Checkbox(bool), // checked
    Radio(bool),
    SelectArrow,
    Gauge { frac: f32, meter: bool }, // progress / meter (채움 비율)
}

pub struct LayoutBox<'a> {
    pub dimensions: Dimensions,
    // CSS transform (절대 좌표계 행렬). 기하는 그대로 두고 페인트/CSSOM 이 이걸 쓴다.
    pub transform: Option<Mat>,
    pub styled_node: &'a StyledNode<'a>,
    // 익명 박스인가. 익명 박스는 부모의 styled_node 를 그대로 쓰므로 NodeId 가 겹친다.
    // 요소별 사각형/메트릭을 수집할 때 반드시 제외해야 한다(안 그러면 익명 박스의
    // content-only 사각형이 진짜 박스의 border-box 를 덮어쓴다).
    pub anonymous: bool,
    pub children: Vec<LayoutBox<'a>>,
    // 네이티브 폼 컨트롤(체크박스/라디오/셀렉트 화살표) 표식
    pub form_control: Option<FormControl>,
    pub glyphs: Vec<GlyphInstance>,
    pub inline_nodes: Vec<&'a StyledNode<'a>>,
    pub image: Option<usize>,
    pub background_image: Option<usize>,
    pub gradient: Option<crate::css::Gradient>,
    // 클릭 히트 영역: (단어 단위 사각형, href)
    pub links: Vec<(Rect, String)>,
    // 인라인 요소의 조각 사각형: (요소 NodeId, 조각). 인라인 요소(span/a/b/em…)는
    // 자체 박스가 없어서 예전엔 getBoundingClientRect 가 전부 0 을 돌려줬다.
    // 조각들의 합집합이 그 요소의 박스다(CSSOM: 인라인 박스 = 조각들의 경계 합).
    pub inline_frags: Vec<(crate::dom::NodeId, Rect)>,
    // 링크 밑줄/리스트 불릿 등 (사각형, 색)
    pub decorations: Vec<(Rect, Color)>,
    // 인라인 요소 배경(<mark>, background 있는 <span>/<code> 등) — 글리프 뒤에 칠함
    pub inline_bgs: Vec<(Rect, Color)>,
    // 인라인 요소 테두리(태그/뱃지/kbd 등) — (박스, 색, 두께, radius). 병합된 조각.
    pub inline_borders: Vec<(Rect, Color, f32, f32)>,
    // 리스트 마커 텍스트 (ol: "1." / ul: "•"). build 시 부모 리스트가 부여.
    pub list_marker: Option<String>,
    // 콘텐츠의 실제 사용 폭 (shrink-to-fit float 배치용)
    pub used_width: f32,
    // float 컨텍스트(절대 좌표): (좌 float 우측 x, 우 float 좌측 x, 밴드 하단 y).
    // 텍스트 줄 상자가 이 밴드 안(y < 하단)에서 float 을 피해 짧아진다(text-wrap).
    pub float_ctx: Option<(f32, f32, f32)>,
    // 세로 margin 상쇄(§8.3.1): 이 박스의 상/하단 margin 이 조상 margin 으로 hoisting 되어
    // 자기 위치엔 0 으로 적용됨을 뜻하는 플래그. 부모의 블록 스택이 설정.
    pub collapse_top: bool,
    pub collapse_bottom: bool,
    // flex/grid 아이템 등 독립 서식 맥락(BFC): 자식과 margin 상쇄하지 않는다.
    pub bfc_item: bool,
    // 비BFC 블록이 끝날 때 아직 소진 안 된 float 밴드(절대좌표): 부모 BFC 로 "탈출"시킨다.
    // (fl_next, fr_next, band_bottom_l, band_bottom_r, band_bottom). §9.5 float 은 최근접 BFC 소속.
    pub trailing_floats: Option<(f32, f32, f32, f32, f32)>,
}

impl<'a> LayoutBox<'a> {
    fn new(styled_node: &'a StyledNode<'a>) -> LayoutBox<'a> {
        LayoutBox {
            dimensions: Default::default(),
            styled_node,
            anonymous: false,
            children: Vec::new(),
            glyphs: Vec::new(),
            inline_nodes: Vec::new(),
            image: None,
            background_image: None,
            gradient: None,
            links: Vec::new(),
            inline_frags: Vec::new(),
            transform: None,
            decorations: Vec::new(),
            inline_bgs: Vec::new(),
            inline_borders: Vec::new(),
            list_marker: None,
            used_width: 0.0,
            float_ctx: None,
            form_control: None,
            collapse_top: false,
            collapse_bottom: false,
            bfc_item: false,
            trailing_floats: None,
        }
    }

    fn new_anonymous(parent: &'a StyledNode<'a>, nodes: Vec<&'a StyledNode<'a>>) -> LayoutBox<'a> {
        LayoutBox {
            dimensions: Default::default(),
            styled_node: parent,
            anonymous: true,
            children: Vec::new(),
            glyphs: Vec::new(),
            inline_nodes: nodes,
            image: None,
            background_image: None,
            gradient: None,
            links: Vec::new(),
            inline_frags: Vec::new(),
            transform: None,
            decorations: Vec::new(),
            inline_bgs: Vec::new(),
            inline_borders: Vec::new(),
            list_marker: None,
            used_width: 0.0,
            float_ctx: None,
            form_control: None,
            collapse_top: false,
            collapse_bottom: false,
            bfc_item: false,
            trailing_floats: None,
        }
    }

    fn layout(&mut self, containing_block: Dimensions, fonts: &FontStack, images: &ImageMap) {
        // 이미지 대체 요소: 고유 크기 박스
        if let NodeType::Element(e) = &self.styled_node.node.node_type {
            if e.tag_name == "img" {
                if let Some(src) = e.attributes.get("src") {
                    if let Some(&(idx, iw, ih)) = images.get(src) {
                        self.calculate_position(containing_block);
                        let (iw, ih) = (iw as f32, ih as f32);
                        let cbw = containing_block.content.width;
                        // CSS width/height > HTML width/height 속성 > None
                        let dim = |css: &str, attr: &str, base: f32| -> Option<f32> {
                            if let Some(v) = self.styled_node.value(css) {
                                if !matches!(v, Value::Keyword(_)) {
                                    return Some(len_px(v, base).to_px());
                                }
                            }
                            e.attributes.get(attr).and_then(|s| s.trim().parse::<f32>().ok())
                        };
                        let cw = dim("width", "width", cbw);
                        let ch = dim("height", "height", cbw);
                        // 한 축만 지정되면 종횡비 유지, 둘 다 없으면 고유 크기
                        let (w, h) = match (cw, ch) {
                            (Some(w), Some(h)) => (w, h),
                            (Some(w), None) => (w, if iw > 0.0 { w * ih / iw } else { ih }),
                            (None, Some(h)) => (if ih > 0.0 { h * iw / ih } else { iw }, h),
                            (None, None) => (iw, ih),
                        };
                        // min/max 제약 (반응형 이미지: img { max-width: 100% }). 폭이 눌리고
                        // 다른 축이 auto 면 고유 종횡비를 유지해 재계산 (height: auto 흔한 경우).
                        let sn = self.styled_node;
                        // base<=0(indefinite %) 또는 해석값<=0 은 무제약으로 무시.
                        let clamp_axis = |val: f32, min_p: &str, max_p: &str, base: f32| -> f32 {
                            let mut v = val;
                            if let Some(mv) = sn.value(max_p) {
                                if let Length(mx, Px) = len_px(mv, base) {
                                    if mx > 0.0 && v > mx { v = mx; }
                                }
                            }
                            if let Some(mv) = sn.value(min_p) {
                                if let Length(mn, Px) = len_px(mv, base) {
                                    if mn > 0.0 && v < mn { v = mn; }
                                }
                            }
                            v
                        };
                        let w2 = clamp_axis(w, "min-width", "max-width", cbw);
                        let h2 = clamp_axis(h, "min-height", "max-height", containing_block.content.height);
                        let (w, h) = if ch.is_none() && (w2 - w).abs() > 0.01 && iw > 0.0 {
                            (w2, w2 * ih / iw) // height auto: 눌린 폭에 비율 적용
                        } else if cw.is_none() && (h2 - h).abs() > 0.01 && ih > 0.0 {
                            (h2 * iw / ih, h2) // width auto: 눌린 높이에 비율 적용
                        } else {
                            (w2, h2)
                        };
                        self.dimensions.content.width = w;
                        self.dimensions.content.height = h;
                        self.image = Some(idx);
                        return;
                    }
                }
                // 이미지 미로드: width/height(CSS/속성)로 공간 예약(대체 요소 박스).
                // 없으면 0. 레이아웃 점프 방지 + 인라인 흐름 유지.
                self.calculate_position(containing_block);
                let cbw = containing_block.content.width;
                let dim = |css: &str, attr: &str| -> f32 {
                    if let Some(v) = self.styled_node.value(css) {
                        if !matches!(v, Value::Keyword(_)) {
                            return len_px(v, cbw).to_px();
                        }
                    }
                    e.attributes.get(attr).and_then(|s| s.trim().parse::<f32>().ok()).unwrap_or(0.0)
                };
                self.dimensions.content.width = dim("width", "width");
                self.dimensions.content.height = dim("height", "height");
                return;
            }
            if e.tag_name == "input" {
                self.layout_input(containing_block, fonts);
                return;
            }
            if e.tag_name == "select" {
                self.layout_select(containing_block, fonts);
                return;
            }
            if e.tag_name == "progress" || e.tag_name == "meter" {
                self.layout_gauge(containing_block, e.tag_name == "meter");
                return;
            }
            if e.tag_name == "canvas" {
                // 대체 요소: 크기 = CSS/HTML width·height > 기본 300x150. 내용은 JS 가 그림.
                self.calculate_position(containing_block);
                let cbw = containing_block.content.width;
                let dim = |css: &str, attr: &str, deflt: f32| -> f32 {
                    if let Some(v) = self.styled_node.value(css) {
                        if !matches!(v, Value::Keyword(_)) {
                            return len_px(v, cbw).to_px();
                        }
                    }
                    e.attributes
                        .get(attr)
                        .and_then(|s| s.trim().trim_end_matches("px").parse::<f32>().ok())
                        .unwrap_or(deflt)
                };
                self.dimensions.content.width = dim("width", "width", 300.0);
                self.dimensions.content.height = dim("height", "height", 150.0);
                return;
            }
            if e.tag_name == "svg" {
                self.calculate_position(containing_block);
                let cbw = containing_block.content.width;
                // 크기: CSS/HTML width·height > viewBox 비율 > 기본 (300x150 근사, 아이콘은 보통 지정)
                let dim = |css: &str, attr: &str| -> Option<f32> {
                    if let Some(v) = self.styled_node.value(css) {
                        if !matches!(v, Value::Keyword(_)) {
                            return Some(len_px(v, cbw).to_px());
                        }
                    }
                    e.attributes.get(attr).and_then(|s| s.trim().trim_end_matches("px").parse::<f32>().ok())
                };
                let vb = e.attributes.get("viewbox").and_then(|s| parse_viewbox(s));
                let (vbw, vbh) = vb.map(|v| (v.2, v.3)).unwrap_or((0.0, 0.0));
                let w = dim("width", "width");
                let h = dim("height", "height");
                let (w, h) = match (w, h) {
                    (Some(w), Some(h)) => (w, h),
                    (Some(w), None) => (w, if vbw > 0.0 { w * vbh / vbw } else { w }),
                    (None, Some(h)) => (if vbh > 0.0 { h * vbw / vbh } else { h }, h),
                    (None, None) if vbw > 0.0 => (vbw, vbh),
                    (None, None) => (300.0, 150.0),
                };
                self.dimensions.content.width = w;
                self.dimensions.content.height = h;
                return;
            }
        }

        if !self.inline_nodes.is_empty() {
            self.dimensions.content.width = containing_block.content.width;
            self.dimensions.content.x = containing_block.content.x;
            self.dimensions.content.y = containing_block.content.height + containing_block.content.y;
            self.layout_inline(fonts);
            return;
        }
        self.calculate_width(containing_block);
        self.calculate_position(containing_block);
        // 배경 이미지/그라디언트 해결 (블록 박스만)
        match self.styled_node.value("background-image") {
            Some(Value::Url(u)) => {
                if let Some(&(idx, _, _)) = images.get(&u) {
                    self.background_image = Some(idx);
                }
            }
            Some(Value::Gradient(g)) => self.gradient = Some(g),
            _ => {}
        }
        let tag = match &self.styled_node.node.node_type {
            NodeType::Element(e) => e.tag_name.as_str(),
            _ => "",
        };
        let _ = tag;
        if matches!(self.styled_node.display(), Display::Flex) {
            self.layout_flex_children(fonts, images);
        } else if matches!(self.styled_node.display(), Display::Grid) {
            self.layout_grid_children(fonts, images);
        } else if box_is_table(self) {
            self.layout_table(fonts, images);
        } else if is_tr(self) {
            self.layout_table_row(fonts, images);
        } else {
            let ncols = self.column_count();
            if ncols >= 2 {
                self.layout_columns(fonts, images, ncols);
            } else {
                self.layout_children(fonts, images);
            }
        }
        self.calculate_height();
        self.add_list_marker(fonts);
        // position: relative — 정상 흐름 위치를 유지한 채 시각적으로만 offset 이동
        // (형제 배치엔 영향 없음). absolute/fixed 는 layout_children 에서 흐름 제거 처리
        if self.position() == "relative" {
            let dx = self.offset("left", "right");
            let dy = self.offset("top", "bottom");
            if dx != 0.0 || dy != 0.0 {
                self.translate(dx, dy);
            }
        }
    }

    // <li> 앞에 리스트 마커 (콘텐츠 왼쪽 패딩 영역, 첫 줄 baseline 우측정렬).
    // ol → "1." 등 부여된 마커, ul/고아 li → 불릿.
    fn add_list_marker(&mut self, fonts: &FontStack) {
        let NodeType::Element(e) = &self.styled_node.node.node_type else { return };
        if e.tag_name != "li" {
            return;
        }
        // list-style-type:none (상속 포함) 이면 마커 없음 — None 을 기본 불릿으로 되돌리지 않는다.
        if matches!(self.styled_node.value("list-style-type"),
            Some(Value::Keyword(ref k)) if k == "none")
        {
            return;
        }
        let marker = self.list_marker.clone().unwrap_or_else(|| "\u{2022}".to_string());
        let px = self
            .styled_node
            .value("font-size")
            .map(|v| v.to_px())
            .filter(|&p| p > 0.0)
            .unwrap_or(16.0);
        let color = match self.styled_node.value("color") {
            Some(Value::Color(c)) => c,
            _ => Color { r: 0, g: 0, b: 0, a: 255 },
        };
        // 마커 폭 측정 → 콘텐츠 왼쪽에서 gap 만큼 떨어져 우측정렬
        let width: f32 = marker
            .chars()
            .map(|c| {
                let (fi, gid) = fonts.glyph_for(c);
                let f = fonts.font(fi);
                f.advance_width(gid) as f32 * (px / f.units_per_em() as f32)
            })
            .sum();
        let d = self.dimensions.content;
        let mut pen = d.x - px * 0.45 - width;
        let baseline = d.y + px * 0.95;
        for c in marker.chars() {
            let (fi, gid) = fonts.glyph_for(c);
            let f = fonts.font(fi);
            let adv = f.advance_width(gid) as f32 * (px / f.units_per_em() as f32);
            self.glyphs.push(GlyphInstance {
                font_index: fi,
                glyph_id: gid,
                x: pen,
                baseline_y: baseline,
                px,
                color,
                bold: false,
                italic: false,
                rot: 0.0,
            });
            pen += adv;
        }
    }

    // <input> 대체 요소: 폭 = CSS width > size 속성 > 기본 180px,
    // 높이 = font-size × 1.5. value 속성을 글리프로 렌더. type=hidden 은 0 크기.
    fn layout_input(&mut self, containing_block: Dimensions, fonts: &FontStack) {
        let NodeType::Element(e) = &self.styled_node.node.node_type else { return };
        let input_type =
            e.attributes.get("type").map(|t| t.to_ascii_lowercase()).unwrap_or_else(|| "text".into());
        if input_type == "hidden" {
            return; // 0 크기, 글리프 없음
        }
        self.calculate_width(containing_block);
        self.calculate_position(containing_block);
        // checkbox/radio: 작은 고정 크기 네이티브 컨트롤. paint 가 폰트 없이 직접 그린다.
        if input_type == "checkbox" || input_type == "radio" {
            let sz = 13.0;
            self.dimensions.content.width = sz;
            self.dimensions.content.height = sz;
            self.dimensions.border = EdgeSizes::default();
            self.dimensions.padding = EdgeSizes::default();
            self.used_width = sz;
            let checked = e.attributes.get("checked").is_some();
            self.form_control = Some(if input_type == "checkbox" {
                FormControl::Checkbox(checked)
            } else {
                FormControl::Radio(checked)
            });
            return;
        }
        let px = self
            .styled_node
            .value("font-size")
            .map(|v| v.to_px())
            .filter(|&p| p > 0.0)
            .unwrap_or(16.0);
        let raw_value = e.attributes.get("value").cloned().unwrap_or_default();
        // password: 글자를 • 로 마스킹
        let value = if input_type == "password" {
            "\u{2022}".repeat(raw_value.chars().count())
        } else {
            raw_value
        };
        let is_button = matches!(
            e.attributes.get("type").map(|t| t.as_str()),
            Some("submit") | Some("button") | Some("reset")
        );
        // CSS width 지정이 없으면 (auto → 컨테이너 폭이 됨) 유형별 폭으로 교체
        if self.styled_node.value("width").is_none() {
            if is_button {
                // 버튼: value 텍스트 폭 + 좌우 여백 (shrink-to-fit)
                let text_w: f32 = value
                    .chars()
                    .map(|c| {
                        let (fi, gid) = fonts.glyph_for(c);
                        let f = fonts.font(fi);
                        f.advance_width(gid) as f32 * (px / f.units_per_em() as f32)
                    })
                    .sum();
                self.dimensions.content.width = text_w + px * 1.6;
            } else {
                let size_chars =
                    e.attributes.get("size").and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0);
                self.dimensions.content.width =
                    if size_chars > 0.0 { size_chars * px * 0.55 } else { 180.0 };
            }
        }
        self.dimensions.content.height = px * 1.5;
        // inline-block 흐름에서 shrink-to-fit 폭으로 쓰이도록 노출
        self.used_width = self.dimensions.content.width;
        let color = match self.styled_node.value("color") {
            Some(Value::Color(c)) => c,
            _ => Color { r: 20, g: 20, b: 24, a: 255 },
        };
        // content.x 는 이미 CSS padding 만큼 안쪽 — 별도 하드코딩 inset 없음
        let mut pen = self.dimensions.content.x;
        let baseline = self.dimensions.content.y + px * 1.1;
        let (bold, italic) = (self.styled_node.is_bold(), self.styled_node.is_italic());
        for ch in value.chars() {
            let (fi, gid) = fonts.glyph_for(ch);
            let f = fonts.font(fi);
            let adv = f.advance_width(gid) as f32 * (px / f.units_per_em() as f32);
            if !ch.is_whitespace() {
                self.glyphs.push(GlyphInstance {
                    font_index: fi,
                    glyph_id: gid,
                    x: pen,
                    baseline_y: baseline,
                    px,
                    color,
                    bold,
                    italic,
                    rot: 0.0,
                });
            }
            pen += adv;
        }
    }

    // <select>: 선택된 option 텍스트만 보여주고, 드롭다운 화살표를 붙인다.
    // option 자식은 흐름에 배치하지 않는다(SelectArrow 표식 → paint 가 삼각형을 그림).
    fn layout_select(&mut self, containing_block: Dimensions, fonts: &FontStack) {
        self.calculate_width(containing_block);
        self.calculate_position(containing_block);
        let px = self
            .styled_node
            .value("font-size")
            .map(|v| v.to_px())
            .filter(|&p| p > 0.0)
            .unwrap_or(16.0);
        // 선택된 option: selected 속성 있는 것, 없으면 첫 번째
        let mut selected: Option<&StyledNode> = None;
        let mut first: Option<&StyledNode> = None;
        for c in &self.styled_node.children {
            if let NodeType::Element(e) = &c.node.node_type {
                if e.tag_name == "option" {
                    if first.is_none() {
                        first = Some(c);
                    }
                    if e.attributes.get("selected").is_some() {
                        selected = Some(c);
                        break;
                    }
                }
            }
        }
        let text = selected.or(first).map(styled_subtree_text).unwrap_or_default();
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        // 폭: CSS 지정 없으면 텍스트 + 화살표 여백으로 shrink-to-fit (UA block 폭 대신)
        let text_w: f32 = text
            .chars()
            .map(|c| {
                let (fi, gid) = fonts.glyph_for(c);
                let f = fonts.font(fi);
                f.advance_width(gid) as f32 * (px / f.units_per_em() as f32)
            })
            .sum();
        if self.styled_node.value("width").is_none() {
            self.dimensions.content.width = text_w + px * 2.0;
        }
        self.dimensions.content.height = px * 1.5;
        self.used_width = self.dimensions.content.width;
        let color = match self.styled_node.value("color") {
            Some(Value::Color(c)) => c,
            _ => Color { r: 20, g: 20, b: 24, a: 255 },
        };
        let mut pen = self.dimensions.content.x;
        let baseline = self.dimensions.content.y + px * 1.1;
        let (bold, italic) = (self.styled_node.is_bold(), self.styled_node.is_italic());
        for ch in text.chars() {
            let (fi, gid) = fonts.glyph_for(ch);
            let f = fonts.font(fi);
            let adv = f.advance_width(gid) as f32 * (px / f.units_per_em() as f32);
            if !ch.is_whitespace() {
                self.glyphs.push(GlyphInstance {
                    font_index: fi,
                    glyph_id: gid,
                    x: pen,
                    baseline_y: baseline,
                    px,
                    color,
                    bold,
                    italic,
                    rot: 0.0,
                });
            }
            pen += adv;
        }
        self.form_control = Some(FormControl::SelectArrow);
    }

    // <progress>/<meter>: 트랙 + 채움 막대. paint 가 프리미티브로 그린다.
    fn layout_gauge(&mut self, containing_block: Dimensions, meter: bool) {
        let NodeType::Element(e) = &self.styled_node.node.node_type else { return };
        let attr = |k: &str| e.attributes.get(k).and_then(|s| s.trim().parse::<f32>().ok());
        let frac = if meter {
            let (min, max, val) =
                (attr("min").unwrap_or(0.0), attr("max").unwrap_or(1.0), attr("value").unwrap_or(0.0));
            if max > min { ((val - min) / (max - min)).clamp(0.0, 1.0) } else { 0.0 }
        } else {
            let max = attr("max").unwrap_or(1.0);
            match attr("value") {
                Some(v) if max > 0.0 => (v / max).clamp(0.0, 1.0),
                _ => 0.0, // value 없으면 indeterminate → 빈 트랙
            }
        };
        self.calculate_width(containing_block);
        self.calculate_position(containing_block);
        let px = self
            .styled_node
            .value("font-size")
            .map(|v| v.to_px())
            .filter(|&p| p > 0.0)
            .unwrap_or(16.0);
        if self.styled_node.value("width").is_none() {
            self.dimensions.content.width = 160.0;
        }
        self.dimensions.content.height = (px * 0.9).max(12.0);
        self.dimensions.border = EdgeSizes::default();
        self.dimensions.padding = EdgeSizes::default();
        self.used_width = self.dimensions.content.width;
        self.form_control = Some(FormControl::Gauge { frac, meter });
    }

    fn calculate_width(&mut self, containing_block: Dimensions) {
        let style = self.styled_node;
        let auto = Keyword("auto".to_string());
        let zero = Length(0.0, Px);

        let avail = containing_block.content.width;
        // 퍼센트 → px (컨테이닝 블록 content 폭 기준). auto 는 보존.
        let width = len_px(style.value("width").unwrap_or(auto.clone()), avail);
        let margin_left = len_px(style.lookup("margin-left", "margin", &zero), avail);
        let margin_right = len_px(style.lookup("margin-right", "margin", &zero), avail);
        let border_left = style.lookup("border-left-width", "border-width", &zero).to_px();
        let border_right = style.lookup("border-right-width", "border-width", &zero).to_px();
        let padding_left = len_px(style.lookup("padding-left", "padding", &zero), avail).to_px();
        let padding_right = len_px(style.lookup("padding-right", "padding", &zero), avail).to_px();
        let extra = border_left + border_right + padding_left + padding_right;

        // box-sizing: border-box → 지정 width 는 border box(패딩·테두리 포함).
        // content = width - extra 로 환산 (auto/퍼센트-잔여는 그대로).
        let border_box = matches!(style.value("box-sizing"),
            Some(Value::Keyword(ref k)) if k == "border-box");
        let width = if border_box {
            match width {
                Length(w, Px) => Length((w - extra).max(0.0), Px),
                other => other,
            }
        } else {
            width
        };

        let (mut cw, mut ml, mut mr) = resolve_width(&width, &margin_left, &margin_right, extra, avail);

        // max-width: 계산된 폭이 상한을 넘으면 고정 폭으로 재계산 (auto 마진 → 가운데 정렬).
        // 퍼센트/calc/min-max 는 컨테이닝 폭 기준으로 해석 (max-width: 100% 등 반응형 핵심).
        if let Some(v) = style.value("max-width") {
            if let Length(mw, Px) = len_px(v, avail) {
                let mw = if border_box { (mw - extra).max(0.0) } else { mw };
                if cw > mw {
                    let (cw2, ml2, mr2) =
                        resolve_width(&Length(mw, Px), &margin_left, &margin_right, extra, avail);
                    cw = cw2;
                    ml = ml2;
                    mr = mr2;
                }
            }
        }
        // min-width: max-width 보다 우선 (마지막에 적용). 계산 폭이 하한보다 작으면 하한으로.
        if let Some(v) = style.value("min-width") {
          if let Length(mw, Px) = len_px(v, avail) {
            let mw = if border_box { (mw - extra).max(0.0) } else { mw };
            if cw < mw {
                let (cw2, ml2, mr2) =
                    resolve_width(&Length(mw, Px), &margin_left, &margin_right, extra, avail);
                cw = cw2;
                ml = ml2;
                mr = mr2;
            }
          }
        }

        let d = &mut self.dimensions;
        d.content.width = cw;
        d.padding.left = padding_left;
        d.padding.right = padding_right;
        d.border.left = border_left;
        d.border.right = border_right;
        d.margin.left = ml;
        d.margin.right = mr;
    }

    // ── 세로 margin 상쇄 (CSS 2.1 §8.3.1) ────────────────────────────────
    // 이 박스가 첫 정상흐름 블록 자식과 상단 margin 을 상쇄하는가:
    // 상단 테두리/패딩 없음, BFC 아님(flex/grid 아이템·overflow 등 제외), 블록,
    // 직접 인라인 내용 없음.
    fn collapse_child_top(&self, avail: f32) -> bool {
        if establishes_bfc(self) || !matches!(self.styled_node.display(), Display::Block) {
            return false;
        }
        if !self.inline_nodes.is_empty() {
            return false;
        }
        let z = Length(0.0, Px);
        let bt = len_px(self.styled_node.lookup("border-top-width", "border-width", &z), avail).to_px();
        let pt = len_px(self.styled_node.lookup("padding-top", "padding", &z), avail).to_px();
        bt <= 0.0 && pt <= 0.0
    }

    fn collapse_child_bottom(&self, avail: f32) -> bool {
        if establishes_bfc(self) || !matches!(self.styled_node.display(), Display::Block) {
            return false;
        }
        if !self.inline_nodes.is_empty() {
            return false;
        }
        // 고정 height / min-height 는 상쇄 차단(내용이 못 채울 수 있음 — §8.3.1).
        if matches!(self.styled_node.value("height"), Some(Length(_, Px)))
            || self.styled_node.value("min-height").is_some()
        {
            return false;
        }
        let z = Length(0.0, Px);
        let bb = len_px(self.styled_node.lookup("border-bottom-width", "border-width", &z), avail).to_px();
        let pb = len_px(self.styled_node.lookup("padding-bottom", "padding", &z), avail).to_px();
        bb <= 0.0 && pb <= 0.0
    }

    // 정상흐름(float/abs 아님) 첫/마지막 블록 자식. 첫(마지막) 정상흐름 자식이
    // 인라인 레벨이면 None (인라인 내용이 상쇄를 막음).
    fn first_flow_block_child(&self) -> Option<usize> {
        for (i, c) in self.children.iter().enumerate() {
            if c.float() != "none" || c.position() == "absolute" || c.position() == "fixed" {
                continue;
            }
            return if matches!(c.styled_node.display(), Display::Block) { Some(i) } else { None };
        }
        None
    }

    fn last_flow_block_child(&self) -> Option<usize> {
        for (i, c) in self.children.iter().enumerate().rev() {
            if c.float() != "none" || c.position() == "absolute" || c.position() == "fixed" {
                continue;
            }
            return if matches!(c.styled_node.display(), Display::Block) { Some(i) } else { None };
        }
        None
    }

    // 상쇄된 상/하단 margin: 자기 margin 을, 자격 되면 첫/마지막 블록 자식의
    // 상쇄 margin 과 재귀적으로 상쇄한 값.
    fn collapsed_top_margin(&self, avail: f32) -> f32 {
        let z = Length(0.0, Px);
        let own = len_px(self.styled_node.lookup("margin-top", "margin", &z), avail).to_px();
        if self.collapse_child_top(avail) {
            if let Some(i) = self.first_flow_block_child() {
                return collapse_margins(own, self.children[i].collapsed_top_margin(avail));
            }
        }
        own
    }

    fn collapsed_bottom_margin(&self, avail: f32) -> f32 {
        let z = Length(0.0, Px);
        let own = len_px(self.styled_node.lookup("margin-bottom", "margin", &z), avail).to_px();
        if self.collapse_child_bottom(avail) {
            if let Some(i) = self.last_flow_block_child() {
                return collapse_margins(own, self.children[i].collapsed_bottom_margin(avail));
            }
        }
        own
    }

    fn calculate_position(&mut self, containing_block: Dimensions) {
        // 세로 margin 은 상쇄값을 쓴다(§8.3.1). collapse_top/bottom 플래그가 서면
        // 그 margin 은 조상으로 hoisting 되었으므로 자기 위치엔 0.
        let cbw = containing_block.content.width;
        let mtop = if self.collapse_top { 0.0 } else { self.collapsed_top_margin(cbw) };
        let mbot = if self.collapse_bottom { 0.0 } else { self.collapsed_bottom_margin(cbw) };
        let style = self.styled_node;
        let zero = Length(0.0, Px);
        let d = &mut self.dimensions;

        d.margin.top = mtop;
        d.margin.bottom = mbot;
        d.border.top = style.lookup("border-top-width", "border-width", &zero).to_px();
        d.border.bottom = style.lookup("border-bottom-width", "border-width", &zero).to_px();
        d.padding.top = style.lookup("padding-top", "padding", &zero).to_px();
        d.padding.bottom = style.lookup("padding-bottom", "padding", &zero).to_px();

        d.content.x = containing_block.content.x + d.margin.left + d.border.left + d.padding.left;
        d.content.y = containing_block.content.height
            + containing_block.content.y
            + d.margin.top
            + d.border.top
            + d.padding.top;
    }

    // text-align 키워드 ("center"/"right"/else left)
    fn align(&self) -> &'static str {
        // direction:rtl 이면 start/end 및 미지정 기본이 반대로 (start=right).
        let rtl = matches!(self.styled_node.value("direction"), Some(Value::Keyword(ref k)) if k == "rtl");
        let start = if rtl { "right" } else { "left" };
        let end = if rtl { "left" } else { "right" };
        match self.styled_node.value("text-align") {
            Some(Value::Keyword(s)) => match s.as_str() {
                "center" => "center",
                "justify" => "justify",
                "right" => "right",
                "left" => "left",
                "end" => end,
                "start" => start,
                _ => start,
            },
            _ => start, // 미지정 = start (rtl 이면 오른쪽)
        }
    }

    // 서브트리 전체를 (dx, dy) 만큼 이동 (정렬/relative 위치 후처리)
    fn translate(&mut self, dx: f32, dy: f32) {
        self.dimensions.content.x += dx;
        self.dimensions.content.y += dy;
        for g in &mut self.glyphs {
            g.x += dx;
            g.baseline_y += dy;
        }
        for (r, _) in &mut self.links {
            r.x += dx;
            r.y += dy;
        }
        for (r, _) in &mut self.decorations {
            r.x += dx;
            r.y += dy;
        }
        for (r, _) in &mut self.inline_bgs {
            r.x += dx;
            r.y += dy;
        }
        for b in &mut self.inline_borders {
            b.0.x += dx;
            b.0.y += dy;
        }
        for c in &mut self.children {
            c.translate(dx, dy);
        }
    }

    fn translate_x(&mut self, dx: f32) {
        self.translate(dx, 0.0);
    }

    // 절대/고정 위치 후처리: 각 absolute 요소를 "가장 가까운 positioned 조상"의,
    // 각 fixed 요소를 뷰포트의 패딩 박스 기준으로 재배치한다. 레이아웃 단계에선
    // 직속 컨테이너 기준 정적 위치에 둔 뒤, 여기서 올바른 컨테이닝 블록으로 원점만
    // 옮긴다(서브트리째 translate). 흐름/형제 위치엔 영향 없음.
    fn reposition_abs(&mut self, abs_cb: Rect, fixed_cb: Rect) {
        // self 가 positioned 면 그 패딩 박스가 자식 absolute 의 컨테이닝 블록이 된다.
        let child_abs_cb =
            if self.position() != "static" { self.dimensions.padding_box() } else { abs_cb };
        for child in &mut self.children {
            // 익명 박스는 부모의 styled_node 를 공유한다 — position 도 부모 것으로 보인다.
            // 걸러내지 않으면 absolute 부모의 익명 인라인 박스가 **한 번 더** 이동해서
            // 글자만 두 배 위치에 그려진다 (박스는 맞고 텍스트만 어긋나는 기묘한 버그였다).
            let cpos = if child.anonymous { "static" } else { child.position() };
            if cpos == "absolute" || cpos == "fixed" {
                let cb = if cpos == "fixed" { fixed_cb } else { child_abs_cb };
                let has_left = child.styled_node.value("left").is_some();
                let has_right = child.styled_node.value("right").is_some();
                let has_top = child.styled_node.value("top").is_some();
                let has_bottom = child.styled_node.value("bottom").is_some();
                // 스트레치: 양쪽 오프셋 지정 + 크기 auto 면 컨테이닝 블록을 채운다
                // (inset:0 오버레이 등). 박스만 리사이즈(콘텐츠 재배치는 안 함 — 근사).
                let width_auto = !matches!(child.styled_node.value("width"), Some(Length(_, _)));
                let height_auto = !matches!(child.styled_node.value("height"), Some(Length(_, _)));
                if has_left && has_right && width_auto {
                    let bp = child.dimensions.border_box().width - child.dimensions.content.width;
                    let w = cb.width - child.offset_val("left") - child.offset_val("right") - bp;
                    child.dimensions.content.width = w.max(0.0);
                }
                if has_top && has_bottom && height_auto {
                    let bp = child.dimensions.border_box().height - child.dimensions.content.height;
                    let h = cb.height - child.offset_val("top") - child.offset_val("bottom") - bp;
                    child.dimensions.content.height = h.max(0.0);
                }
                let cur = child.dimensions.border_box();
                let tx = if has_right && !has_left {
                    cb.x + cb.width - cur.width - child.offset_val("right")
                } else if has_left {
                    cb.x + child.offset_val("left")
                } else {
                    cur.x // 정적 위치 유지 (auto)
                };
                let ty = if has_top {
                    cb.y + child.offset_val("top")
                } else if has_bottom {
                    cb.y + cb.height - cur.height - child.offset_val("bottom")
                } else {
                    cur.y
                };
                child.translate(tx - cur.x, ty - cur.y);
            }
            child.reposition_abs(child_abs_cb, fixed_cb);
        }
    }

    // 테이블 셀이 행 높이로 늘어났을 때 vertical-align 에 따라 내부 콘텐츠만 아래로.
    // (셀 박스 자체 위치는 유지, 글리프/자식만 이동). middle=중앙, bottom=하단.
    fn valign_content(&mut self, extra: f32) {
        if extra <= 0.0 {
            return;
        }
        let factor = match self.styled_node.value("vertical-align") {
            Some(Value::Keyword(ref k)) if k == "middle" => 0.5,
            Some(Value::Keyword(ref k)) if k == "bottom" => 1.0,
            _ => 0.0, // top/baseline(기본) → 이동 없음
        };
        if factor == 0.0 {
            return;
        }
        let dy = extra * factor;
        for g in &mut self.glyphs {
            g.baseline_y += dy;
        }
        for (r, _) in &mut self.links {
            r.y += dy;
        }
        for (r, _) in &mut self.decorations {
            r.y += dy;
        }
        for (r, _) in &mut self.inline_bgs {
            r.y += dy;
        }
        for b in &mut self.inline_borders {
            b.0.y += dy;
        }
        for c in &mut self.children {
            c.translate(0.0, dy);
        }
    }

    // 서브트리 전체를 (ox, oy) 원점 기준 (sx, sy) 배로 스케일 (transform: scale).
    // 축 정렬 유지 → 사각형/글리프 위치·크기만 조정, 글리프 px 도 스케일해 재래스터.
    fn scale_subtree(&mut self, ox: f32, oy: f32, sx: f32, sy: f32) {
        let sc = |v: f32, o: f32, s: f32| o + (v - o) * s;
        let d = &mut self.dimensions;
        d.content.x = sc(d.content.x, ox, sx);
        d.content.y = sc(d.content.y, oy, sy);
        d.content.width *= sx;
        d.content.height *= sy;
        for g in &mut self.glyphs {
            g.x = sc(g.x, ox, sx);
            g.baseline_y = sc(g.baseline_y, oy, sy);
            g.px *= (sx + sy) / 2.0; // 비균일 스케일은 평균으로 근사
        }
        for (r, _) in &mut self.links {
            r.x = sc(r.x, ox, sx);
            r.y = sc(r.y, oy, sy);
            r.width *= sx;
            r.height *= sy;
        }
        for (r, _) in &mut self.decorations {
            r.x = sc(r.x, ox, sx);
            r.y = sc(r.y, oy, sy);
            r.width *= sx;
            r.height *= sy;
        }
        for (r, _) in &mut self.inline_bgs {
            r.x = sc(r.x, ox, sx);
            r.y = sc(r.y, oy, sy);
            r.width *= sx;
            r.height *= sy;
        }
        for b in &mut self.inline_borders {
            b.0.x = sc(b.0.x, ox, sx);
            b.0.y = sc(b.0.y, oy, sy);
            b.0.width *= sx;
            b.0.height *= sy;
        }
        for c in &mut self.children {
            c.scale_subtree(ox, oy, sx, sy);
        }
    }

    // 재레이아웃 전 누적 페인트 상태를 초기화 (glyphs/links/decorations 는 push 로
    // 쌓이므로, float shrink-to-fit 2차 배치 시 중복 방지를 위해 서브트리를 비운다)
    fn clear_render(&mut self) {
        self.glyphs.clear();
        self.links.clear();
        self.decorations.clear();
        self.inline_bgs.clear();
        self.inline_borders.clear();
        self.image = None;
        self.background_image = None;
        self.gradient = None;
        self.dimensions = Default::default();
        self.used_width = 0.0;
        // collapse 플래그는 매 배치마다 부모가 다시 설정 → 재레이아웃 전 리셋.
        // (bfc_item 은 구조적 속성이라 유지.)
        self.collapse_top = false;
        self.collapse_bottom = false;
        self.trailing_floats = None;
        for c in &mut self.children {
            c.clear_render();
        }
    }

    // position 키워드
    fn position(&self) -> &'static str {
        match self.styled_node.value("position") {
            Some(Value::Keyword(s)) if s == "relative" => "relative",
            Some(Value::Keyword(s)) if s == "absolute" => "absolute",
            Some(Value::Keyword(s)) if s == "fixed" => "fixed",
            Some(Value::Keyword(s)) if s == "sticky" => "sticky",
            _ => "static",
        }
    }

    // 인셋 원값 (없거나 auto 면 None) — sticky 는 지정된 축만 붙는다
    fn inset(&self, prop: &str) -> Option<f32> {
        match self.styled_node.value(prop) {
            Some(Length(v, Px)) => Some(v),
            _ => None,
        }
    }

    // float 키워드
    fn float(&self) -> &'static str {
        match self.styled_node.value("float") {
            Some(Value::Keyword(s)) if s == "left" => "left",
            Some(Value::Keyword(s)) if s == "right" => "right",
            _ => "none",
        }
    }

    // clear 키워드 (지정 쪽 float 아래로 내림)
    fn clear(&self) -> &'static str {
        match self.styled_node.value("clear") {
            Some(Value::Keyword(s)) if s == "left" => "left",
            Some(Value::Keyword(s)) if s == "right" => "right",
            Some(Value::Keyword(s)) if s == "both" => "both",
            _ => "none",
        }
    }

    // top/left 등 오프셋 px (prop 우선, 없으면 반대편 opp 의 음수, 둘 다 없으면 0)
    fn offset(&self, prop: &str, opp: &str) -> f32 {
        match self.styled_node.value(prop) {
            Some(Length(v, Px)) => v,
            _ => match self.styled_node.value(opp) {
                Some(Length(v, Px)) => -v,
                _ => 0.0,
            },
        }
    }

    // 단일 오프셋 길이 (미지정 0)
    fn offset_val(&self, prop: &str) -> f32 {
        match self.styled_node.value(prop) {
            Some(Length(v, Px)) => v,
            _ => 0.0,
        }
    }

    fn layout_children(&mut self, fonts: &FontStack, images: &ImageMap) {
        let align = self.align();
        // 이 블록이 float 밴드 옆에 놓여(부모가 float_ctx 설정) 내용이 float 을 우회해야 하면,
        // 인라인 자식(익명 인라인 박스)에 밴드를 물려줘 줄 상자가 짧아지게 한다. 밴드 좌표는
        // 절대값이라 자식 content box 범위로 클램프되어 그대로 합성된다.
        if let Some(fc) = self.float_ctx {
            for c in self.children.iter_mut() {
                // 익명 인라인 박스는 직접 우회. 일반 블록(BFC 아님)은 밴드를 물려받아
                // 그 안의 인라인 자식까지 재귀적으로 우회하게 한다. BFC/float/abs 는 제외.
                if !c.inline_nodes.is_empty() || !establishes_bfc(c) {
                    c.float_ctx = Some(fc);
                }
            }
        }
        // 컨테이너의 안정된 원점/폭 (height 만 흐름 중 누적)
        let (cx, cy, avail) =
            (self.dimensions.content.x, self.dimensions.content.y, self.dimensions.content.width);
        // 실용적 float 밴드 상태: 연속된 float 들이 현재 흐름 y 에서 좌/우로 패킹된다.
        // fl_next: 다음 left float 이 놓일 왼쪽 x, fr_next: 다음 right float 의 오른쪽 경계.
        // band_bottom: 밴드 내 float 들의 최대 하단. 이후 정상 블록은 밴드 아래로 clear.
        let mut fl_next = cx;
        let mut fr_next = cx + avail;
        let mut band_top = cy;
        let mut band_bottom = cy;
        // clear 처리를 위해 좌/우 float 하단을 따로 추적 (clear:left/right/both).
        let mut band_bottom_l = cy;
        let mut band_bottom_r = cy;
        let mut band_active = false;
        // 이 컨테이너가 float 로 shrink-to-fit 될 때의 내용 폭: float 밴드가
        // 실제로 차지한 가로 범위 (좌 float 누적 + 우 float 누적).
        let mut float_extent = 0.0f32;
        // inline-block 런 상태: 연속된 inline-block 자식을 좌→우로 패킹, 폭 초과 시 줄바꿈.
        // ib_lines: 정렬 후처리용 (줄별 자식 인덱스, 줄 폭). ib_cur: 현재 줄.
        let mut ib_active = false;
        let mut ib_pen_x = cx;
        let mut ib_line_top = cy;
        let mut ib_line_h = 0.0f32;
        let mut ib_bottom = cy;
        let mut ib_lines: Vec<(Vec<usize>, f32)> = Vec::new();
        let mut ib_cur: Vec<usize> = Vec::new();
        // 인접 형제 세로 margin 상쇄: 직전 정상 블록의 하단 margin (다음 블록 상단과 겹침).
        // float/inline-block 등 흐름을 끊는 배치에선 0 으로 리셋.
        let mut prev_bottom = 0.0f32;
        let mut inline_extent = 0.0f32;
        // 부모-자식 margin 상쇄(§8.3.1): 자격 되면 첫/마지막 정상흐름 블록 자식의
        // 상단/하단 margin 이 이 박스 margin 으로 hoisting 된다(그 자식은 자기 위치엔 0).
        let first_bc = if self.collapse_child_top(avail) { self.first_flow_block_child() } else { None };
        let last_bc = if self.collapse_child_bottom(avail) { self.last_flow_block_child() } else { None };
        let n = self.children.len();
        for i in 0..n {
            let cpos = self.children[i].position();
            let cfloat = self.children[i].float();
            let is_ib = is_atomic_inline(&self.children[i]) && cfloat == "none";
            // 익명 인라인 박스(텍스트 런): inline-block 과 같은 줄에 흘러야 한다.
            // 단, 인접한 inline-block 이 있을 때만 atom 으로(홀로 있는 텍스트 블록은 정상 유지).
            let is_anon = !self.children[i].inline_nodes.is_empty() && cfloat == "none";
            let next_is_ib = self
                .children
                .get(i + 1)
                .map(|c| is_atomic_inline(c) && c.float() == "none")
                .unwrap_or(false);
            let is_atom = is_ib || (is_anon && (ib_active || next_is_ib));

            // 인라인 atom 이 아닌 자식을 만나면 진행 중이던 런을 마감(정렬 + 높이 반영)
            if ib_active && !is_atom {
                ib_lines.push((std::mem::take(&mut ib_cur), ib_pen_x - cx));
                let w = self.finish_inline_block_run(
                    std::mem::take(&mut ib_lines),
                    align,
                    avail,
                    ib_bottom,
                    cy,
                );
                inline_extent = inline_extent.max(w);
                ib_active = false;
                ib_pen_x = cx;
                ib_line_h = 0.0;
                prev_bottom = 0.0; // 인라인블록 런이 흐름을 끊음 → margin 상쇄 리셋
            }

            // clear: 지정 쪽 float 아래로 이 요소를 내린다 (clearfix/구획 분리).
            // 청산된 쪽 float 은 이후 이 요소에 영향 없음 (밴드에서 해제). absolute 는 제외.
            if band_active && cpos != "absolute" && cpos != "fixed" {
                let cc = self.children[i].clear();
                if cc != "none" {
                    let (cl, cr) = (cc == "left" || cc == "both", cc == "right" || cc == "both");
                    let mut target = self.dimensions.content.height + cy;
                    if cl {
                        target = target.max(band_bottom_l);
                    }
                    if cr {
                        target = target.max(band_bottom_r);
                    }
                    if target - cy > self.dimensions.content.height {
                        self.dimensions.content.height = target - cy;
                    }
                    if cl && target >= band_bottom_l - 0.5 {
                        fl_next = cx;
                    }
                    if cr && target >= band_bottom_r - 0.5 {
                        fr_next = cx + avail;
                    }
                    if fl_next <= cx + 0.5 && fr_next >= cx + avail - 0.5 {
                        band_active = false;
                    }
                    prev_bottom = 0.0; // clearance 후 margin 상쇄 리셋
                }
            }

            // position: absolute/fixed — 흐름에서 제거. 여기선 직속 컨테이너 기준
            // 정적 위치에 레이아웃만 하고, 최종 원점은 reposition_abs 후처리에서
            // "가장 가까운 positioned 조상"(absolute) 또는 뷰포트(fixed) 기준으로 옮긴다.
            if cpos == "absolute" || cpos == "fixed" {
                let child = &mut self.children[i];
                let mut cb: Dimensions = Default::default();
                cb.content.x = cx;
                cb.content.y = cy;
                cb.content.width = avail;
                child.layout(cb, fonts, images);
                continue; // 흐름 높이에 미반영
            }

            // float: left/right — 밴드에 좌/우로 패킹 (shrink-to-fit).
            if cfloat != "none" {
                let flow_y = self.dimensions.content.height + cy;
                if !band_active {
                    band_active = true;
                    band_top = flow_y;
                    band_bottom = flow_y;
                    fl_next = cx;
                    fr_next = cx + avail;
                }
                let avail_band = (fr_next - fl_next).max(0.0);
                let child = &mut self.children[i];
                // 1차(probe) 배치: 밴드 잔여 폭으로 레이아웃해 내용 폭(used_width) 측정
                // 명시 width(%/px)면 컨테이닝 블록(컨테이너 전체 avail) 기준으로 해석 —
                // 밴드 잔여가 아니라 컨테이너 폭에 대한 %(float 폭은 §10.3.5). auto 는 밴드에
                // shrink-to-fit. 최종 배치도 explicit 이면 CB 폭을 넘겨 % 를 한 번만 해석한다
                // (ow 를 넘기면 calculate_width 가 % 를 재해석해 이중 축소되던 버그).
                let explicit = matches!(child.styled_node.value("width"), Some(Length(_, _)));
                let cbw = if explicit { avail } else { avail_band };
                let mut probe: Dimensions = Default::default();
                probe.content.x = fl_next;
                probe.content.y = band_top;
                probe.content.width = cbw;
                child.layout(probe, fonts, images);
                // auto 폭 shrink-to-fit 시 재배치 폭(ow)에 margin 도 포함해야 재계산에서
                // margin 이 content 를 깎지 않는다 (auto 폭엔 phantom margin 이 없음).
                let bp = child.dimensions.margin_box().width - child.dimensions.content.width;
                let ow = if explicit {
                    child.dimensions.border_box().width
                } else {
                    (child.used_width + bp).min(avail_band)
                };
                // 2차 배치: 확정 위치로 재배치 (1차 페인트 상태는 비우고 다시).
                let x = if cfloat == "left" { fl_next } else { fr_next - ow };
                child.clear_render();
                let mut cb: Dimensions = Default::default();
                cb.content.x = x;
                cb.content.y = band_top;
                cb.content.width = if explicit { cbw } else { ow };
                child.layout(cb, fonts, images);
                let fbottom = band_top + child.dimensions.margin_box().height;
                if cfloat == "left" {
                    fl_next += ow;
                    band_bottom_l = band_bottom_l.max(fbottom);
                } else {
                    fr_next -= ow;
                    band_bottom_r = band_bottom_r.max(fbottom);
                }
                float_extent = float_extent.max((fl_next - cx) + ((cx + avail) - fr_next));
                band_bottom = band_bottom.max(fbottom);
                prev_bottom = 0.0; // float 은 흐름을 끊음 → margin 상쇄 리셋
                continue; // 정상 흐름 높이엔 직접 미반영 (밴드로 관리)
            }

            // inline-block: 가로로 흐르며 폭 초과 시 줄바꿈 (shrink-to-fit).
            if is_atom {
                if !ib_active {
                    ib_active = true;
                    ib_line_top = self.dimensions.content.height + cy;
                    ib_bottom = ib_line_top;
                    ib_pen_x = cx;
                    ib_line_h = 0.0;
                }
                let child = &mut self.children[i];
                // 폭 측정용 probe (위치는 폭 결정에 무관)
                let mut probe: Dimensions = Default::default();
                probe.content.x = ib_pen_x;
                probe.content.y = ib_line_top;
                probe.content.width = avail;
                child.layout(probe, fonts, images);
                // 익명 텍스트 박스는 항상 shrink-to-fit(내용 폭). inline-block 은 명시 width 존중.
                let explicit =
                    !is_anon && matches!(child.styled_node.value("width"), Some(Length(_, _)));
                // auto 폭 shrink-to-fit 시 재배치 폭(ow)에 margin 도 포함해야 재계산에서
                // margin 이 content 를 깎지 않는다 (auto 폭엔 phantom margin 이 없음).
                let bp = child.dimensions.margin_box().width - child.dimensions.content.width;
                let ow = if explicit {
                    child.dimensions.border_box().width.min(avail)
                } else {
                    (child.used_width + bp).min(avail)
                };
                // 줄 초과면 다음 줄로 (줄 시작이 아닐 때만)
                if ib_pen_x + ow > cx + avail + 0.5 && ib_pen_x > cx + 0.5 {
                    ib_lines.push((std::mem::take(&mut ib_cur), ib_pen_x - cx));
                    ib_pen_x = cx;
                    ib_line_top = ib_bottom;
                    ib_line_h = 0.0;
                }
                // 확정 폭·위치로 재배치
                child.clear_render();
                let mut cb: Dimensions = Default::default();
                cb.content.x = ib_pen_x;
                cb.content.y = ib_line_top;
                cb.content.width = ow;
                child.layout(cb, fonts, images);
                let mw = child.dimensions.margin_box().width;
                let mh = child.dimensions.margin_box().height;
                ib_cur.push(i);
                ib_pen_x += mw;
                ib_line_h = ib_line_h.max(mh);
                ib_bottom = ib_bottom.max(ib_line_top + ib_line_h);
                prev_bottom = 0.0; // inline-block 은 흐름을 끊음 → margin 상쇄 리셋
                continue;
            }

            // float 밴드가 활성일 때: 현재 흐름 y 가 밴드 하단을 지났으면 밴드 해제
            // (이후 블록은 float 영향 없이 전체폭). 밴드는 여러 형제에 걸쳐 지속된다.
            if band_active && (self.dimensions.content.height + cy) >= band_bottom - 0.5 {
                band_active = false;
                fl_next = cx;
                fr_next = cx + avail;
            }
            // 정상 블록. float 밴드가 있을 때: 인라인 콘텐츠를 담은 블록(문단 등)은 박스는
            // 전체폭을 유지하되 줄 상자만 float 을 우회한다(text-wrap) — 흐름에 정상 배치.
            // 인라인 콘텐츠 없는 블록은 (margin 으로) 옆에 맞으면 나란히, 아니면 밴드 아래로 clear.
            if band_active {
                // 익명 텍스트 박스 또는 인라인 콘텐츠 블록(중첩 래퍼 포함, BFC 제외):
                // float 주위로 줄이 흐른다. float_ctx 를 실어 배치하면 내부(및 중첩된
                // 자식) 줄 상자가 밴드를 피해 짧아진다.
                if is_anon
                    || (!establishes_bfc(&self.children[i])
                        && subtree_has_inline_text(&self.children[i]))
                {
                    let cur_top = self.children[i].collapsed_top_margin(avail);
                    self.dimensions.content.height -= collapse_overlap(prev_bottom, cur_top);
                    self.children[i].float_ctx = Some((fl_next, fr_next, band_bottom));
                    let d = self.dimensions;
                    self.children[i].layout(d, fonts, images);
                    self.children[i].float_ctx = None;
                    self.dimensions.content.height += self.children[i].dimensions.margin_box().height;
                    prev_bottom = self.children[i].dimensions.margin.bottom;
                    continue;
                }
                let d0 = self.dimensions; // content.height = 밴드 top 레벨 (float 은 흐름 미반영)
                self.children[i].layout(d0, fonts, images);
                let bb = self.children[i].dimensions.border_box();
                let beside = bb.x >= fl_next - 0.5 && bb.x + bb.width <= fr_next + 0.5;
                if beside {
                    // 옆에 배치 — probe 가 최종. 정렬 후 흐름 높이 갱신.
                    if align != "left" {
                        let cw = self.children[i].dimensions.border_box().width;
                        if cw < avail - 0.5 {
                            let dx =
                                if align == "center" { (avail - cw) / 2.0 } else { avail - cw };
                            self.children[i].translate_x(dx);
                        }
                    }
                    let mh = self.children[i].dimensions.margin_box().height;
                    // 이 블록 하단과 float 밴드 하단 중 큰 쪽까지 진행 (후속은 둘 다 클리어)
                    let block_bottom = self.dimensions.content.height + mh;
                    self.dimensions.content.height = block_bottom.max(band_bottom - cy);
                    band_active = false;
                    continue;
                }
                // 겹침 → 밴드 아래로 clear 후 재배치
                let below = band_bottom - cy;
                if below > self.dimensions.content.height {
                    self.dimensions.content.height = below;
                }
                band_active = false;
                self.children[i].clear_render();
            }
            // 부모-자식 상쇄 대상이면 이 자식의 상/하단 margin 을 부모로 hoisting (자기 위치엔 0).
            self.children[i].collapse_top = Some(i) == first_bc;
            self.children[i].collapse_bottom = Some(i) == last_bc;
            // 인접 형제 margin 상쇄: 이 블록의 (상쇄된) 상단 margin 을 직전 블록 하단 margin 과
            // 겹쳐(더하지 않고) 흐름 높이에서 겹침량만큼 줄인 뒤 배치한다.
            let cur_top = if self.children[i].collapse_top {
                0.0
            } else {
                self.children[i].collapsed_top_margin(avail)
            };
            self.dimensions.content.height -= collapse_overlap(prev_bottom, cur_top);
            // 정상 흐름: 누적 높이가 반영된 live dimensions 로 스택
            let d = self.dimensions;
            let child = &mut self.children[i];
            child.layout(d, fonts, images);
            if align != "left" {
                let cw = child.dimensions.border_box().width;
                if cw < avail - 0.5 {
                    let dx = if align == "center" { (avail - cw) / 2.0 } else { avail - cw };
                    child.translate_x(dx);
                }
            }
            self.dimensions.content.height += child.dimensions.margin_box().height;
            prev_bottom = child.dimensions.margin.bottom;
            // 비BFC 자식 안의 float 이 자식 밖으로 protrude 하면(자식보다 float 이 큼),
            // 그 밴드를 현재 흐름으로 이어받아 뒤 형제가 float 을 우회하게 한다(§9.5).
            if let Some((cfl, cfr, cbl, cbr, cbb)) = self.children[i].trailing_floats {
                let flow_y = self.dimensions.content.height + cy;
                if cbb > flow_y + 0.5 {
                    if band_active {
                        fl_next = fl_next.max(cfl.clamp(cx, cx + avail));
                        fr_next = fr_next.min(cfr.clamp(cx, cx + avail));
                    } else {
                        band_active = true;
                        band_top = flow_y;
                        fl_next = cfl.clamp(cx, cx + avail);
                        fr_next = cfr.clamp(cx, cx + avail);
                    }
                    band_bottom_l = band_bottom_l.max(cbl);
                    band_bottom_r = band_bottom_r.max(cbr);
                    band_bottom = band_bottom.max(cbb);
                    float_extent = float_extent.max((fl_next - cx) + ((cx + avail) - fr_next));
                }
            }
        }
        // 마지막이 inline-block 런이면 마감
        if ib_active {
            ib_lines.push((std::mem::take(&mut ib_cur), ib_pen_x - cx));
            let w =
                self.finish_inline_block_run(std::mem::take(&mut ib_lines), align, avail, ib_bottom, cy);
            inline_extent = inline_extent.max(w);
        }
        // float 로 끝난 경우: BFC 블록만 float 을 담아 높이가 자란다(§9.5). 비BFC 블록은
        // float 을 담지 않아(브라우저처럼 박스 밖으로 넘칠 수 있음) 아래 trailing 으로 넘긴다.
        if band_active && establishes_bfc(self) {
            let below = band_bottom - cy;
            if below > self.dimensions.content.height {
                self.dimensions.content.height = below;
            }
        }
        // 비BFC 블록이 소진 안 된 float 밴드를 갖고 끝나면 부모 BFC 로 넘긴다(§9.5).
        // BFC(overflow/flex/grid/float/abs 등)는 float 을 가두므로 넘기지 않는다.
        self.trailing_floats = if band_active && !establishes_bfc(self) {
            Some((fl_next, fr_next, band_bottom_l, band_bottom_r, band_bottom))
        } else {
            None
        };
        // shrink-to-fit 부모용 내재 폭(preferred width): 정상 자식 최대 폭, float 밴드,
        // inline-block 줄 중 최대. auto 폭 자식은 avail 을 채우므로 border_box 대신
        // 내용 preferred(used_width)+좌우 padding/border 로 재구성해야 실제 내용 폭이 된다.
        let child_max = self
            .children
            .iter()
            .map(|c| {
                let explicit = matches!(c.styled_node.value("width"), Some(Length(_, _)));
                if explicit {
                    c.dimensions.border_box().width
                } else {
                    c.used_width + (c.dimensions.border_box().width - c.dimensions.content.width)
                }
            })
            .fold(0.0f32, f32::max);
        self.used_width = child_max.max(float_extent).max(inline_extent);
    }

    // inline-block 런 마감: 각 줄을 text-align 에 맞춰 가로 정렬하고, 런 전체 높이를
    // 컨테이너 흐름 높이에 반영. 반환값은 런의 최대 줄 폭 (shrink-to-fit used_width 용).
    fn finish_inline_block_run(
        &mut self,
        lines: Vec<(Vec<usize>, f32)>,
        align: &str,
        avail: f32,
        bottom: f32,
        cy: f32,
    ) -> f32 {
        let mut max_w = 0.0f32;
        for (idxs, w) in &lines {
            max_w = max_w.max(*w);
            if align != "left" && *w < avail - 0.5 {
                let off = if align == "center" { (avail - w) / 2.0 } else { avail - w };
                if off > 0.5 {
                    for &idx in idxs {
                        self.children[idx].translate_x(off);
                    }
                }
            }
        }
        let below = bottom - cy;
        if below > self.dimensions.content.height {
            self.dimensions.content.height = below;
        }
        max_w
    }

    // column-count (>=2 면 다단). 숫자는 Length 또는 Keyword 로 파싱될 수 있음.
    fn column_count(&self) -> usize {
        match self.styled_node.value("column-count") {
            Some(Length(n, _)) if n >= 1.0 => n as usize,
            Some(Value::Keyword(ref k)) => k.trim().parse::<usize>().unwrap_or(1),
            _ => 1,
        }
    }

    fn column_gap(&self) -> f32 {
        match self.styled_node.value("column-gap") {
            Some(Length(v, Px)) => v,
            _ => 16.0, // normal ≈ 1em
        }
    }

    // CSS 다단(column-count): 자식을 열 폭으로 단일 열 배치한 뒤, 균형 잡히도록
    // 자식 경계에서 여러 열로 분배한다(한 자식을 열 사이로 쪼개진 않음 — 실용 근사).
    fn layout_columns(&mut self, fonts: &FontStack, images: &ImageMap, ncols: usize) {
        let full_w = self.dimensions.content.width;
        let gap = self.column_gap();
        let col_w = ((full_w - gap * (ncols as f32 - 1.0)) / ncols as f32).max(0.0);
        // 1) 자식을 열 폭으로 단일 열 스택 배치
        self.dimensions.content.width = col_w;
        self.layout_children(fonts, images);
        self.dimensions.content.width = full_w;
        let oy = self.dimensions.content.y;
        let total_h = self.dimensions.content.height;
        if total_h <= 0.0 || self.children.is_empty() {
            return;
        }
        let target = total_h / ncols as f32;
        // 2) 그리디로 자식을 열에 분배
        let mut col = 0usize;
        let mut col_start = 0.0f32; // 현재 열 시작 y (oy 상대)
        let mut max_h = 0.0f32;
        for child in &mut self.children {
            let cy = child.dimensions.content.y - oy;
            if col + 1 < ncols && cy - col_start >= target && cy > col_start + 0.1 {
                col += 1;
                col_start = cy;
            }
            let dx = col as f32 * (col_w + gap);
            child.translate(dx, -col_start);
            let bottom = (cy - col_start) + child.dimensions.margin_box().height;
            max_h = max_h.max(bottom);
        }
        self.dimensions.content.height = max_h;
    }

    // <table>: 모든 행의 셀을 모아 공통 열 폭을 계산해 열을 정렬한다.
    // 열 폭 = 지정 폭(있으면) 아니면 내용 기반(max-content) 선호 폭. 남는/부족한
    // 폭은 auto 열에 선호 비율로 분배해 테이블 폭을 채움. 행 높이 = 최고 셀.
    // border-spacing (border-collapse:separate 일 때만, 기본 0). (가로, 세로) px.
    fn table_border_spacing(&self) -> (f32, f32) {
        if matches!(self.styled_node.value("border-collapse"), Some(Value::Keyword(ref k)) if k == "collapse") {
            return (0.0, 0.0);
        }
        match self.styled_node.value("border-spacing") {
            Some(Length(v, _)) => (v, v),
            Some(Value::Keyword(ref s)) => {
                let mut it = s.split_whitespace();
                let h = it.next().and_then(crate::css::parse_len_px).unwrap_or(0.0);
                let v = it.next().and_then(crate::css::parse_len_px).unwrap_or(h);
                (h, v)
            }
            _ => (0.0, 0.0),
        }
    }

    // colspan/rowspan/border-spacing 지원. border-collapse 는 근사(테두리 미중첩).
    fn layout_table(&mut self, fonts: &FontStack, images: &ImageMap) {
        let d = self.dimensions;
        let (ox, oy, avail) = (d.content.x, d.content.y, d.content.width);
        // 행 위치: 직속 tr, 또는 tbody/thead/tfoot 안의 tr
        let mut rows: Vec<(usize, Option<usize>)> = Vec::new();
        for i in 0..self.children.len() {
            if is_tr(&self.children[i]) {
                rows.push((i, None));
            } else if is_row_group(&self.children[i]) {
                for j in 0..self.children[i].children.len() {
                    if is_tr(&self.children[i].children[j]) {
                        rows.push((i, Some(j)));
                    }
                }
            }
        }
        if rows.is_empty() {
            // CSS 테이블에서 행 없이 셀이 직속이면(익명 행 생략) 한 행으로 가로 배치.
            if self.children.iter().any(is_cell) {
                self.layout_table_row(fonts, images);
            } else {
                self.layout_children(fonts, images);
            }
            return;
        }
        // 행 박스 접근 (매크로 대용 인라인 매치)
        macro_rules! row_at {
            ($self:ident, $i:expr, $j:expr) => {
                match $j {
                    None => &mut $self.children[$i],
                    Some(j) => &mut $self.children[$i].children[j],
                }
            };
        }
        // 1) 열 수(colspan 합의 최대) + 열별 선호/지정 폭 측정
        let ncols = rows
            .iter()
            .map(|&(i, j)| row_at!(self, i, j).children.iter().map(cell_colspan).sum::<usize>())
            .max()
            .unwrap_or(0);
        if ncols == 0 {
            self.dimensions.content.height = 0.0;
            return;
        }
        let mut col_pref = vec![0.0f32; ncols];
        let mut col_fixed: Vec<Option<f32>> = vec![None; ncols];
        for &(i, j) in &rows {
            let row = row_at!(self, i, j);
            let mut c = 0usize; // 현재 열 커서 (colspan 반영)
            for cell in row.children.iter_mut() {
                if c >= ncols {
                    break;
                }
                let span = cell_colspan(cell).min(ncols - c);
                // 고정 폭/선호 폭은 스팬한 열에 균등 분배
                if let Some(w) = cell_width(cell, avail) {
                    let per = w / span as f32;
                    for k in 0..span {
                        col_fixed[c + k] = Some(col_fixed[c + k].map_or(per, |e: f32| e.max(per)));
                    }
                }
                let mut probe: Dimensions = Default::default();
                probe.content.x = ox;
                probe.content.y = oy;
                probe.content.width = avail;
                cell.layout(probe, fonts, images);
                let bp = cell.dimensions.border_box().width - cell.dimensions.content.width;
                let per_pref = (cell.used_width + bp) / span as f32;
                for k in 0..span {
                    col_pref[c + k] = col_pref[c + k].max(per_pref);
                }
                c += span;
            }
        }
        // 2) 열 폭 확정: 고정 열은 그대로, auto 열은 남은 폭을 선호 비율로 분배.
        let total_fixed: f32 = col_fixed.iter().flatten().sum();
        let auto_cols: Vec<usize> = (0..ncols).filter(|&c| col_fixed[c].is_none()).collect();
        let auto_pref_sum: f32 = auto_cols.iter().map(|&c| col_pref[c]).sum();
        // 테이블 used 폭(§17.5.2 auto layout): 명시 width(px/%)면 계산된 폭(avail)을,
        // width:auto 면 shrink-to-fit = min(선호폭 합, avail). 좁은 내용의 표는
        // border-spacing 은 열 사이·표 가장자리 공간을 먹으므로 열 배분 폭에서 뺀다.
        let (bsx, bsy) = self.table_border_spacing();
        let total_sx = bsx * (ncols as f32 + 1.0);
        let avail_cols = (avail - total_sx).max(0.0);
        // 표 폭 = 컨테이너 채움. auto 표 shrink-to-fit(§17.5.2)은 min/max-content 정식
        // 측정 후로 보류 — used_width 근사는 중첩 표에서 preferred 폭을 오측정해(HN 댓글표
        // 12px 붕괴) 회귀를 냈다. 정식 측정 전엔 항상 채우는 편이 안전.
        let _ = auto_pref_sum;
        let table_width = avail_cols;
        let remaining = (table_width - total_fixed).max(0.0);
        let mut col_w = vec![0.0f32; ncols];
        for c in 0..ncols {
            col_w[c] = match col_fixed[c] {
                Some(w) => w,
                None => {
                    if auto_pref_sum > 0.0 {
                        remaining * col_pref[c] / auto_pref_sum
                    } else if !auto_cols.is_empty() {
                        remaining / auto_cols.len() as f32
                    } else {
                        0.0
                    }
                }
            };
        }
        let mut col_x = vec![ox; ncols];
        {
            let mut x = ox + bsx;
            for c in 0..ncols {
                col_x[c] = x;
                x += col_w[c] + bsx;
            }
        }
        // <caption>: 표 위에 표 폭으로 배치하고, 행 시작 y 를 캡션 높이만큼 내린다.
        let table_w: f32 = col_w.iter().sum::<f32>() + total_sx;
        let mut caption_h = 0.0;
        let caption_idx = self.children.iter().position(|c| {
            matches!(&c.styled_node.node.node_type, NodeType::Element(e) if e.tag_name == "caption")
        });
        if let Some(ci) = caption_idx {
            let cap = &mut self.children[ci];
            let mut cb: Dimensions = Default::default();
            cb.content.x = ox;
            cb.content.y = oy;
            cb.content.width = table_w.max(1.0);
            cap.layout(cb, fonts, images);
            caption_h = cap.dimensions.margin_box().height;
        }
        // 3) 행별 배치 (공통 열 폭). rowspan 은 occupied 로 아래 행 열을 점유.
        let mut y = oy + caption_h + bsy;
        let mut occupied = vec![0usize; ncols]; // 위 행 rowspan 이 덮은 잔여 행 수
        let mut row_tops: Vec<f32> = Vec::with_capacity(rows.len());
        let mut row_heights: Vec<f32> = Vec::with_capacity(rows.len());
        // 높이 조정 대상 rowspan 셀: (i, j, cell_pos, start_row_order, rowspan)
        let mut rowspan_cells: Vec<(usize, Option<usize>, usize, usize, usize)> = Vec::new();
        for (ridx, &(i, j)) in rows.iter().enumerate() {
            row_tops.push(y);
            let row = row_at!(self, i, j);
            let mut row_h = 0.0f32;
            let mut c = 0usize;
            for (cell_pos, cell) in row.children.iter_mut().enumerate() {
                // 위에서 내려온 rowspan 이 덮은 열은 건너뛴다
                while c < ncols && occupied[c] > 0 {
                    c += 1;
                }
                if c >= ncols {
                    break;
                }
                let span = cell_colspan(cell).min(ncols - c);
                let rspan = cell_rowspan(cell);
                // colspan 셀 폭 = 스팬한 열 폭의 합
                let cell_w: f32 = (0..span).map(|k| col_w[c + k]).sum();
                cell.clear_render();
                let mut cb: Dimensions = Default::default();
                cb.content.x = col_x[c];
                cb.content.y = y;
                cb.content.width = cell_w;
                cell.layout(cb, fonts, images);
                // rowspan 셀은 높이를 나눠 각 행에 기여(0-높이 행 방지), 정확 높이는 후처리.
                row_h = row_h.max(cell.dimensions.margin_box().height / rspan as f32);
                if rspan > 1 {
                    for k in 0..span {
                        occupied[c + k] = occupied[c + k].max(rspan);
                    }
                    rowspan_cells.push((i, j, cell_pos, ridx, rspan));
                }
                c += span;
            }
            // 셀 높이를 행 높이로 stretch (rowspan>1 셀은 후처리에서 조정)
            for cell in row.children.iter_mut() {
                if cell_rowspan(cell) > 1 {
                    continue;
                }
                let vextra = cell.dimensions.margin_box().height - cell.dimensions.content.height;
                let old_h = cell.dimensions.content.height;
                cell.dimensions.content.height = (row_h - vextra).max(old_h);
                cell.valign_content(cell.dimensions.content.height - old_h);
            }
            // 행 박스 자체 크기 (행 배경/테두리용)
            row.dimensions.content.x = ox;
            row.dimensions.content.y = y;
            row.dimensions.content.width = avail;
            row.dimensions.content.height = row_h;
            row_heights.push(row_h);
            // 점유 카운트 1 감소 (이 행 소비)
            for o in occupied.iter_mut() {
                *o = o.saturating_sub(1);
            }
            y += row_h + bsy;
        }
        // rowspan 셀 높이 = 시작~끝 행에 걸친 총 높이
        for (i, j, cell_pos, ridx, rspan) in rowspan_cells {
            let end = (ridx + rspan - 1).min(rows.len() - 1);
            let span_h = (row_tops[end] + row_heights[end]) - row_tops[ridx];
            let cell = &mut row_at!(self, i, j).children[cell_pos];
            let vextra = cell.dimensions.margin_box().height - cell.dimensions.content.height;
            let old_h = cell.dimensions.content.height;
            cell.dimensions.content.height = (span_h - vextra).max(old_h);
            cell.valign_content(cell.dimensions.content.height - old_h);
        }
        self.dimensions.content.height = (y - oy).max(0.0);
    }

    // <tr> 의 셀(td/th)을 가로 컬럼으로 배치. 셀의 지정 폭(CSS width 또는 HTML
    // width 속성, px/%)은 존중하고, 지정 없는 셀은 남은 폭을 균등 분배.
    // (테이블 안의 tr 은 layout_table 이 처리; 이건 고아 tr 용)
    fn layout_table_row(&mut self, fonts: &FontStack, images: &ImageMap) {
        let n = self.children.len();
        if n == 0 {
            return;
        }
        let d = self.dimensions;
        let avail = d.content.width;
        let widths: Vec<Option<f32>> =
            self.children.iter().map(|c| cell_width(c, avail)).collect();
        let fixed_total: f32 = widths.iter().flatten().sum();
        let auto_count = widths.iter().filter(|w| w.is_none()).count();
        let auto_w = if auto_count > 0 {
            (avail - fixed_total).max(0.0) / auto_count as f32
        } else {
            0.0
        };
        let mut pen_x = d.content.x;
        let mut max_h = 0.0f32;
        for (child, w) in self.children.iter_mut().zip(widths.iter()) {
            let cw = w.unwrap_or(auto_w);
            let mut cb: Dimensions = Default::default();
            cb.content.x = pen_x;
            cb.content.y = d.content.y;
            cb.content.width = cw;
            child.layout(cb, fonts, images);
            pen_x += cw;
            max_h = max_h.max(child.dimensions.margin_box().height);
        }
        // 셀을 행 높이로 stretch 하고 vertical-align 적용 (CSS 테이블 수직 정렬)
        for child in self.children.iter_mut() {
            let vextra = child.dimensions.margin_box().height - child.dimensions.content.height;
            let old_h = child.dimensions.content.height;
            child.dimensions.content.height = (max_h - vextra).max(old_h);
            child.valign_content(child.dimensions.content.height - old_h);
        }
        self.dimensions.content.height = max_h;
    }

    fn calculate_height(&mut self) {
        // box-sizing: border-box → 지정 height 는 border box. content = height - 세로 extra.
        let border_box = matches!(self.styled_node.value("box-sizing"),
            Some(Value::Keyword(ref k)) if k == "border-box");
        let vextra = if border_box {
            let d = &self.dimensions;
            d.padding.top + d.padding.bottom + d.border.top + d.border.bottom
        } else {
            0.0
        };
        if let Some(Length(h, Px)) = self.styled_node.value("height") {
            self.dimensions.content.height = (h - vextra).max(0.0);
        } else if let Some(Length(ratio, Px)) = self.styled_node.value("aspect-ratio") {
            // aspect-ratio: 명시 height 없을 때 content 높이 = 폭 / 비율
            if ratio > 0.0 && self.dimensions.content.width > 0.0 {
                self.dimensions.content.height = self.dimensions.content.width / ratio;
            }
        }
        // max-height: 사용 높이를 항상 상한으로 클램프(CSS §10.7). overflow 가 visible 이면
        // 내용은 박스 밖으로 넘쳐 그려지고(자식 위치 유지), hidden/auto 면 잘린다.
        if let Some(Length(mxh, Px)) = self.styled_node.value("max-height") {
            let mxh = if border_box { (mxh - vextra).max(0.0) } else { mxh };
            if self.dimensions.content.height > mxh {
                self.dimensions.content.height = mxh;
            }
        }
        // min-height: content 높이가 하한보다 작으면 하한으로 확장 (min-height:100vh 등)
        if let Some(Length(mnh, Px)) = self.styled_node.value("min-height") {
            let mnh = if border_box { (mnh - vextra).max(0.0) } else { mnh };
            if self.dimensions.content.height < mnh {
                self.dimensions.content.height = mnh;
            }
        }
    }
}



fn box_tag<'a>(b: &'a LayoutBox) -> &'a str {
    match &b.styled_node.node.node_type {
        NodeType::Element(e) => e.tag_name.as_str(),
        _ => "",
    }
}

// display 키워드가 kws 중 하나인가 (CSS 테이블 역할 판별).
fn box_display_is(b: &LayoutBox, kws: &[&str]) -> bool {
    matches!(b.styled_node.value("display"), Some(Value::Keyword(s)) if kws.contains(&s.as_str()))
}

// 테이블 상자: <table> 태그 또는 display:table/inline-table
fn box_is_table(b: &LayoutBox) -> bool {
    box_tag(b) == "table" || box_display_is(b, &["table", "inline-table"])
}

// 행 상자: <tr> 태그 또는 display:table-row
fn is_tr(b: &LayoutBox) -> bool {
    box_tag(b) == "tr" || box_display_is(b, &["table-row"])
}

// 행 그룹: tbody/thead/tfoot 또는 display:table-*-group
fn is_row_group(b: &LayoutBox) -> bool {
    matches!(box_tag(b), "tbody" | "thead" | "tfoot")
        || box_display_is(b, &["table-row-group", "table-header-group", "table-footer-group"])
}

// 셀 상자: <td>/<th> 또는 display:table-cell
fn is_cell(b: &LayoutBox) -> bool {
    matches!(box_tag(b), "td" | "th") || box_display_is(b, &["table-cell"])
}

// SVG viewBox "minx miny width height" → (minx, miny, width, height)
pub(crate) fn parse_viewbox(s: &str) -> Option<(f32, f32, f32, f32)> {
    let nums: Vec<f32> = s
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|t| !t.is_empty())
        .filter_map(|t| t.parse::<f32>().ok())
        .collect();
    if nums.len() == 4 {
        Some((nums[0], nums[1], nums[2], nums[3]))
    } else {
        None
    }
}

// 셀의 colspan (HTML 속성). 기본 1, 최소 1.
fn cell_colspan(child: &LayoutBox) -> usize {
    cell_span_attr(child, "colspan")
}

// 셀의 rowspan (HTML 속성). 기본 1, 최소 1.
fn cell_rowspan(child: &LayoutBox) -> usize {
    cell_span_attr(child, "rowspan")
}

fn cell_span_attr(child: &LayoutBox, attr: &str) -> usize {
    if let NodeType::Element(e) = &child.styled_node.node.node_type {
        if let Some(v) = e.attributes.get(attr) {
            if let Ok(n) = v.trim().parse::<usize>() {
                return n.max(1);
            }
        }
    }
    1
}

// 테이블 셀의 지정 폭(px). CSS width(px/%) 우선, 없으면 HTML width 속성(px/%).
// 지정 없으면 None(auto → 남은 폭 균등 분배 대상).
fn cell_width(child: &LayoutBox, avail: f32) -> Option<f32> {
    match child.styled_node.value("width") {
        Some(Length(w, Px)) => return Some(w),
        Some(Length(p, crate::css::Unit::Percent)) => return Some(p / 100.0 * avail),
        _ => {}
    }
    if let NodeType::Element(e) = &child.styled_node.node.node_type {
        if let Some(w) = e.attributes.get("width") {
            let w = w.trim();
            if let Some(pct) = w.strip_suffix('%') {
                if let Ok(p) = pct.trim().parse::<f32>() {
                    return Some(p / 100.0 * avail);
                }
            } else if let Ok(px) = w.trim_end_matches("px").parse::<f32>() {
                return Some(px);
            }
        }
    }
    None
}

// 퍼센트 길이를 컨테이닝 블록 폭 기준 px 로 해석. auto/px/기타는 그대로.
// (모든 퍼센트 — 세로 margin/padding 포함 — 은 CSS 상 컨테이닝 블록 '폭' 기준)
fn len_px(v: Value, pct_base: f32) -> Value {
    match v {
        Length(f, crate::css::Unit::Percent) => Length(pct_base * f / 100.0, Px),
        // calc(pct% + px) → 기준 폭으로 해석 (문맥 단위는 style 에서 이미 px 로 접힘)
        Value::Calc(c) => Length(pct_base * c.pct / 100.0 + c.px, Px),
        // min()/max()/clamp() → 기준 폭으로 % 해석 후 계산
        Value::MinMax(kind, args) => Length(crate::css::eval_minmax(kind, &args, pct_base), Px),
        other => other,
    }
}

// 블록의 used width 와 좌우 마진을 CSS §10.3.3 규칙으로 해결한다.
// (content_width, margin_left, margin_right) 반환. max-width 재계산 시 재사용.
fn resolve_width(
    width: &Value,
    margin_left: &Value,
    margin_right: &Value,
    extra: f32,
    avail: f32,
) -> (f32, f32, f32) {
    let auto = Keyword("auto".to_string());
    let mut width = width.clone();
    let mut ml = margin_left.clone();
    let mut mr = margin_right.clone();

    let total = ml.to_px() + mr.to_px() + extra + width.to_px();

    if width != auto && total > avail {
        if ml == auto {
            ml = Length(0.0, Px);
        }
        if mr == auto {
            mr = Length(0.0, Px);
        }
    }

    let underflow = avail - total;

    match (width == auto, ml == auto, mr == auto) {
        (false, false, false) => {
            mr = Length(mr.to_px() + underflow, Px);
        }
        (false, false, true) => {
            mr = Length(underflow, Px);
        }
        (false, true, false) => {
            ml = Length(underflow, Px);
        }
        (true, _, _) => {
            if ml == auto {
                ml = Length(0.0, Px);
            }
            if mr == auto {
                mr = Length(0.0, Px);
            }
            if underflow >= 0.0 {
                width = Length(underflow, Px);
            } else {
                width = Length(0.0, Px);
                mr = Length(mr.to_px() + underflow, Px);
            }
        }
        (false, true, true) => {
            ml = Length(underflow / 2.0, Px);
            mr = Length(underflow / 2.0, Px);
        }
    }

    (width.to_px(), ml.to_px(), mr.to_px())
}

// 트리 전체의 링크 히트 영역 수집 (문서 좌표계)
pub fn collect_link_regions(root: &LayoutBox, out: &mut Vec<(Rect, String)>) {
    collect_link_regions_m(root, Mat::IDENTITY, out)
}

fn collect_link_regions_m(root: &LayoutBox, parent_m: Mat, out: &mut Vec<(Rect, String)>) {
    let m = match root.transform {
        Some(t) => t.then(&parent_m),
        None => parent_m,
    };
    for (r, href) in &root.links {
        let rr = if m.is_identity() { *r } else { m.bounds(*r) };
        out.push((rr, href.clone()));
    }
    for child in &root.children {
        collect_link_regions_m(child, m, out);
    }
}

// (x, y) 문서 좌표가 가리키는 링크 href
pub fn hit_link<'a>(links: &'a [(Rect, String)], x: f32, y: f32) -> Option<&'a str> {
    links.iter().find(|(r, _)| r.contains(x, y)).map(|(_, h)| h.as_str())
}

// 이벤트 히트 테스트용: 요소 박스의 (border box, NodeId, 깊이) 수집.
// 익명 인라인 박스는 부모 요소의 id 를 공유하므로 텍스트 클릭도 매칭된다.
// 인라인 요소들의 조각을 요소별로 합집합 (트리 전체).
pub fn collect_inline_element_rects(
    root: &LayoutBox,
    out: &mut std::collections::HashMap<crate::dom::NodeId, Rect>,
) {
    for (id, f) in &root.inline_frags {
        out.entry(*id)
            .and_modify(|r| {
                let x0 = r.x.min(f.x);
                let y0 = r.y.min(f.y);
                let x1 = (r.x + r.width).max(f.x + f.width);
                let y1 = (r.y + r.height).max(f.y + f.height);
                *r = Rect { x: x0, y: y0, width: x1 - x0, height: y1 - y0 };
            })
            .or_insert(*f);
    }
    for child in &root.children {
        collect_inline_element_rects(child, out);
    }
}

pub fn collect_element_rects(
    root: &LayoutBox,
    depth: usize,
    out: &mut Vec<(Rect, crate::dom::NodeId, usize)>,
) {
    collect_element_rects_m(root, depth, Mat::IDENTITY, out)
}

// 변환(transform)을 누적하며 요소 사각형을 모은다.
// 표준: getBoundingClientRect 는 **변환된** 경계 상자를 돌려준다. 히트 테스트도 마찬가지다.
// (변환을 무시하면 회전된 버튼을 클릭해도 안 눌린다)
fn collect_element_rects_m(
    root: &LayoutBox,
    depth: usize,
    parent_m: Mat,
    out: &mut Vec<(Rect, crate::dom::NodeId, usize)>,
) {
    let m = match root.transform {
        Some(t) => t.then(&parent_m),
        None => parent_m,
    };
    let xf = |r: Rect| if m.is_identity() { r } else { m.bounds(r) };
    if !root.anonymous && matches!(root.styled_node.node.node_type, NodeType::Element(_)) {
        out.push((xf(root.dimensions.border_box()), root.styled_node.id, depth));
    }
    // 인라인 요소(span/a/b/em…)는 자체 박스가 없다. 조각들의 합집합을 이 박스보다
    // 한 단계 깊은 것으로 넣어야 클릭이 인라인 요소를 타깃으로 잡는다.
    // 예전엔 인라인 요소가 히트 목록에 아예 없어서 <span onclick> 이 발화하지 않았다
    // (블록 조상이 타깃이 되고, 스팬은 그 조상의 자손이라 버블링에도 안 걸린다).
    if !root.inline_frags.is_empty() {
        let mut merged: std::collections::HashMap<crate::dom::NodeId, Rect> =
            std::collections::HashMap::new();
        for (id, f) in &root.inline_frags {
            merged
                .entry(*id)
                .and_modify(|r| {
                    let x0 = r.x.min(f.x);
                    let y0 = r.y.min(f.y);
                    let x1 = (r.x + r.width).max(f.x + f.width);
                    let y1 = (r.y + r.height).max(f.y + f.height);
                    *r = Rect { x: x0, y: y0, width: x1 - x0, height: y1 - y0 };
                })
                .or_insert(*f);
        }
        for (id, r) in merged {
            out.push((xf(r), id, depth + 1));
        }
    }
    for child in &root.children {
        collect_element_rects_m(child, depth + 1, m, out);
    }
}

// 요소별 박스 메트릭(px 확정된 used value). getComputedStyle 이 표준의 resolved value
// (길이는 px)를 돌려주려면 % / em / 무단위 배수를 레이아웃이 확정한 값으로 써야 한다.
pub fn collect_box_metrics(
    root: &LayoutBox,
    out: &mut std::collections::HashMap<crate::dom::NodeId, Dimensions>,
) {
    if !root.anonymous && matches!(root.styled_node.node.node_type, NodeType::Element(_)) {
        out.insert(root.styled_node.id, root.dimensions);
    }
    for child in &root.children {
        collect_box_metrics(child, out);
    }
}

// 클릭 지점을 포함하는 가장 깊은 요소의 NodeId (동률이면 나중에 그려진 쪽)
pub fn hit_element(
    rects: &[(Rect, crate::dom::NodeId, usize)],
    x: f32,
    y: f32,
) -> Option<crate::dom::NodeId> {
    let mut best: Option<&(Rect, crate::dom::NodeId, usize)> = None;
    for er in rects {
        if er.0.contains(x, y) {
            match best {
                Some(b) if b.2 > er.2 => {}
                _ => best = Some(er),
            }
        }
    }
    best.map(|&(_, id, _)| id)
}

// 공백뿐인 인라인 묶음인지 (태그 사이 줄바꿈 등). 이런 익명 박스는 만들지 않는다 —
// 블록 흐름에선 높이 0 으로 무해하지만 flex 에선 아이템이 되어 공간을 차지한다.
fn all_whitespace(nodes: &[&StyledNode]) -> bool {
    nodes.iter().all(|n| match &n.node.node_type {
        NodeType::Text(t) => t.trim().is_empty(),
        _ => false,
    })
}

// 인라인 요소가 블록 레벨 자손을 품고 있는지 (block-in-inline 분리 판정용).
fn contains_block_level(node: &StyledNode) -> bool {
    node.children.iter().any(|c| match c.display() {
        Display::Block | Display::Flex | Display::Grid | Display::InlineBlock => true,
        // contents 는 박스가 없으므로 자식이 블록이면 블록을 품은 셈이다
        Display::Inline | Display::Contents => contains_block_level(c),
        Display::None => false,
    })
}

// 블록 컨테이너의 자식을 분류해 root.children(블록) 과 pending(익명 인라인 묶음) 으로.
// 블록을 품은 인라인 래퍼(<span><div>..</div></span> 등)는 투명 취급하여 그 자식을
// 현재 블록 흐름으로 끌어올린다 — CSS 의 block-in-inline 분리 근사.
fn distribute_children<'a>(
    root: &mut LayoutBox<'a>,
    pending: &mut Vec<&'a StyledNode<'a>>,
    anon_owner: &'a StyledNode<'a>,
    children: &'a [StyledNode<'a>],
) {
    for child in children {
        match child.display() {
            Display::Block | Display::Flex | Display::Grid | Display::InlineBlock => {
                if !pending.is_empty() {
                    let nodes = std::mem::take(pending);
                    if !all_whitespace(&nodes) {
                        root.children.push(LayoutBox::new_anonymous(anon_owner, nodes));
                    }
                }
                root.children.push(build_layout_tree(child));
            }
            Display::Inline => {
                // 인라인 레벨 대체 요소(img/svg/input 등)는 원자적 인라인 — 텍스트 런에
                // 넣을 수 없으므로(대체 콘텐츠) 별도 자식 박스로 만들어 줄 안에 흐르게 한다.
                if is_replaced_element(child) {
                    if !pending.is_empty() {
                        let nodes = std::mem::take(pending);
                        if !all_whitespace(&nodes) {
                            root.children.push(LayoutBox::new_anonymous(anon_owner, nodes));
                        }
                    }
                    root.children.push(build_layout_tree(child));
                } else if contains_block_level(child) {
                    // 투명 래퍼: 인라인 부분은 앞뒤로 나뉘고 블록은 흐름에 편입
                    distribute_children(root, pending, anon_owner, &child.children);
                } else {
                    pending.push(child);
                }
            }
            // display: contents — 박스를 만들지 않고 자식을 현재 흐름에 그대로 넣는다.
            // 부모가 flex/grid 면 자식들이 그대로 flex/grid 아이템이 된다(표준).
            Display::Contents => {
                distribute_children(root, pending, anon_owner, &child.children);
            }
            Display::None => {}
        }
    }
}

// flex/grid 컨테이너의 아이템 수집. display:contents 인 자식은 박스를 만들지 않고
// 그 자식들이 대신 아이템이 된다(CSS Display §3.3 — 박스 트리에서 요소가 사라진다).
fn push_flex_items<'a>(
    root: &mut LayoutBox<'a>,
    container: &'a StyledNode<'a>,
    children: &'a [StyledNode<'a>],
) {
    for child in children {
        match &child.node.node_type {
            NodeType::Text(t) => {
                if !t.trim().is_empty() {
                    root.children.push(LayoutBox::new_anonymous(container, vec![child]));
                }
            }
            NodeType::Element(_) => match child.display() {
                Display::None => {}
                Display::Contents => push_flex_items(root, container, &child.children),
                _ => root.children.push(build_layout_tree(child)),
            },
        }
    }
}

fn build_layout_tree<'a>(style_node: &'a StyledNode<'a>) -> LayoutBox<'a> {
    let mut root = LayoutBox::new(style_node);
    // <svg> 는 대체 요소: CSS 자식 박스를 만들지 않는다 (paint 가 도형을 직접 그림).
    if matches!(&style_node.node.node_type, NodeType::Element(e) if e.tag_name == "svg") {
        return root;
    }
    // flex/grid 컨테이너: 각 자식은 플렉스/그리드 아이템(인라인 요소도 블록화).
    // 인라인 자식을 익명 인라인 묶음으로 모으지 않는다 — 그렇지 않으면 <a>/<span>
    // 로 만든 가로 내비게이션이 하나의 상자로 뭉쳐 세로로 무너진다.
    if matches!(style_node.display(), Display::Flex | Display::Grid) {
        push_flex_items(&mut root, style_node, &style_node.children);
        return root;
    }
    let mut pending: Vec<&'a StyledNode<'a>> = Vec::new();
    distribute_children(&mut root, &mut pending, style_node, &style_node.children);
    if !pending.is_empty() && !all_whitespace(&pending) {
        root.children.push(LayoutBox::new_anonymous(style_node, pending));
    }
    // 리스트면 직속 li 자식에 마커 부여. 종류는 list-style-type 에서 결정.
    if let NodeType::Element(e) = &style_node.node.node_type {
        if e.tag_name == "ol" || e.tag_name == "ul" {
            let ordered = e.tag_name == "ol";
            let reversed = ordered && e.attributes.get("reversed").is_some();
            let li_count = root
                .children
                .iter()
                .filter(|c| matches!(&c.styled_node.node.node_type,
                    NodeType::Element(ce) if ce.tag_name == "li"))
                .count() as i64;
            // ol start: 시작 번호(기본 1). reversed 면 항목 수부터 감소.
            let start = e.attributes.get("start").and_then(|s| s.trim().parse::<i64>().ok());
            let mut n: i64 =
                if reversed { start.unwrap_or(li_count) + 1 } else { start.unwrap_or(1) - 1 };
            for child in &mut root.children {
                if let NodeType::Element(ce) = &child.styled_node.node.node_type {
                    if ce.tag_name != "li" {
                        continue;
                    }
                    // <li value="N"> 로 카운터 재설정
                    if let Some(v) = ce.attributes.get("value").and_then(|s| s.trim().parse::<i64>().ok())
                    {
                        n = if reversed { v + 1 } else { v - 1 };
                    }
                    n += if reversed { -1 } else { 1 };
                    child.list_marker = list_marker_text(child.styled_node, style_node, ordered, n);
                }
            }
        }
    }
    root
}

// list-style-type(li → ul/ol → 기본) 에 따라 마커 문자열. none 이면 마커 없음.
fn list_marker_text(li: &StyledNode, list: &StyledNode, ordered: bool, index: i64) -> Option<String> {
    let idx = index.max(1) as usize; // alpha/roman 은 1 이상 기준
    let ty = li
        .value("list-style-type")
        .or_else(|| li.value("list-style"))
        .or_else(|| list.value("list-style-type"))
        .or_else(|| list.value("list-style"))
        .and_then(|v| if let Value::Keyword(k) = v { Some(k) } else { None })
        .unwrap_or_else(|| if ordered { "decimal".to_string() } else { "disc".to_string() });
    match ty.as_str() {
        "none" => None,
        "disc" => Some("\u{2022}".to_string()),   // •
        "circle" => Some("\u{25E6}".to_string()),  // ◦
        "square" => Some("\u{25AA}".to_string()),  // ▪
        "decimal" => Some(format!("{}.", index)),
        "lower-alpha" | "lower-latin" => Some(format!("{}.", alpha_marker(idx, false))),
        "upper-alpha" | "upper-latin" => Some(format!("{}.", alpha_marker(idx, true))),
        "lower-roman" => Some(format!("{}.", roman_marker(idx, false))),
        "upper-roman" => Some(format!("{}.", roman_marker(idx, true))),
        _ => Some(format!("{}.", index)), // 미지원 종류 → decimal 근사
    }
}

// 1→a, 26→z, 27→aa (알파벳 리스트 마커)
fn alpha_marker(index: usize, upper: bool) -> String {
    let mut n = index;
    let mut s = String::new();
    while n > 0 {
        n -= 1;
        s.insert(0, (b'a' + (n % 26) as u8) as char);
        n /= 26;
    }
    if upper { s.to_ascii_uppercase() } else { s }
}

// 로마 숫자 마커 (1→i, 4→iv, 9→ix ...)
fn roman_marker(index: usize, upper: bool) -> String {
    const VALS: [(usize, &str); 13] = [
        (1000, "m"), (900, "cm"), (500, "d"), (400, "cd"), (100, "c"), (90, "xc"),
        (50, "l"), (40, "xl"), (10, "x"), (9, "ix"), (5, "v"), (4, "iv"), (1, "i"),
    ];
    let mut n = index;
    let mut s = String::new();
    for (v, r) in VALS {
        while n >= v {
            s.push_str(r);
            n -= v;
        }
    }
    if upper { s.to_ascii_uppercase() } else { s }
}

// 대체 요소(replaced element)인가 — 고유 콘텐츠(이미지/폼컨트롤 등)로 텍스트 런에
// 넣을 수 없고 원자적 인라인 박스로 흐른다.
fn is_replaced_element(node: &StyledNode) -> bool {
    matches!(&node.node.node_type, NodeType::Element(e)
        if matches!(e.tag_name.as_str(),
            "img" | "svg" | "input" | "button" | "canvas" | "video" | "textarea" | "select"
            | "progress" | "meter"))
}

// 원자적 인라인 박스(atomic inline)인가 — inline-block 이거나, 인라인 레벨 대체 요소
// (img/svg/input 등)이면 줄 안에서 하나의 원자로 흐른다(줄바꿈 안 함, 텍스트와 한 줄).
fn is_atomic_inline(c: &LayoutBox) -> bool {
    let disp = c.styled_node.display();
    if matches!(disp, Display::InlineBlock) {
        return true;
    }
    if matches!(disp, Display::Inline) {
        if let NodeType::Element(e) = &c.styled_node.node.node_type {
            return matches!(
                e.tag_name.as_str(),
                "img" | "svg" | "input" | "button" | "canvas" | "video" | "textarea" | "select"
            );
        }
    }
    false
}

// 이 박스가 새 블록 서식 맥락(BFC)을 만드는가. BFC 블록은 float 과 겹치지 않고
// 밴드 옆으로 줄어들거나 아래로 clear 되므로, float text-wrap 전파에서 제외한다.
fn establishes_bfc(b: &LayoutBox) -> bool {
    let clips = |p: &str| {
        matches!(b.styled_node.value(p),
            Some(Value::Keyword(ref k)) if k == "hidden" || k == "auto" || k == "scroll" || k == "clip")
    };
    b.bfc_item
        || clips("overflow") || clips("overflow-x") || clips("overflow-y")
        || matches!(b.styled_node.display(), Display::Flex | Display::Grid | Display::InlineBlock)
        || box_is_table(b)
        || b.float() != "none"
        || b.position() == "absolute"
        || b.position() == "fixed"
}

// 서브트리에 float 을 우회해 흐를 인라인 텍스트가 있는가 (BFC 경계에서 멈춤 — BFC 안
// 텍스트는 그 BFC 가 따로 처리). float 밴드 옆에서 블록이 줄만 우회할지 판단에 쓰인다.
fn subtree_has_inline_text(b: &LayoutBox) -> bool {
    if !b.inline_nodes.is_empty() {
        return true;
    }
    b.children.iter().any(|c| !establishes_bfc(c) && subtree_has_inline_text(c))
}

// 인접 margin 상쇄로 흐름에서 줄여야 할 겹침량. m1=이전 하단, m2=이번 상단.
// 상쇄 결과 = 양수최대 + 음수최소. 현재는 두 margin 이 더해지므로 (m1+m2)-상쇄 만큼 뺀다.
fn collapse_overlap(m1: f32, m2: f32) -> f32 {
    let pos = m1.max(0.0).max(m2.max(0.0));
    let neg = m1.min(0.0).min(m2.min(0.0));
    (m1 + m2) - (pos + neg)
}

// 두 margin 의 상쇄 결과값 = 양수 최대 + 음수 최소 (§8.3.1).
fn collapse_margins(a: f32, b: f32) -> f32 {
    a.max(b).max(0.0) + a.min(b).min(0.0)
}

// StyledNode 서브트리의 텍스트를 모은다 (select 의 선택 option 텍스트 추출용).
fn styled_subtree_text(sn: &StyledNode) -> String {
    let mut out = String::new();
    fn walk(sn: &StyledNode, out: &mut String) {
        if let NodeType::Text(t) = &sn.node.node_type {
            out.push_str(t);
        }
        for c in &sn.children {
            walk(c, out);
        }
    }
    walk(sn, &mut out);
    out
}

// position: sticky (CSS Position §3.4).
// 정상 흐름에 남아 있되(형제 배치에 영향 없음), 스크롤포트를 인셋만큼 줄인 사각형 안에
// 머물도록 시각적으로 밀린다. 단 컨테이닝 블록(부모 콘텐츠 상자)을 벗어나지 않는다.
// 정적 렌더에서도 스크롤 위치가 주어지면(KESTREL_SCROLL / window.scrollTo) 실제로 붙는다.
pub fn apply_sticky(root: &mut LayoutBox, scroll_x: f32, scroll_y: f32, vw: f32, vh: f32) {
    fn walk(b: &mut LayoutBox, parent: Rect, sx: f32, sy: f32, vw: f32, vh: f32) {
        let my_content = b.dimensions.content;
        for c in &mut b.children {
            walk(c, my_content, sx, sy, vw, vh);
        }
        // 익명 박스는 부모의 styled_node 를 공유 → 부모가 sticky 면 자식도 sticky 로 보인다.
        if b.anonymous || b.position() != "sticky" {
            return;
        }
        let bb = b.dimensions.border_box();
        let mut dy = 0.0f32;
        if let Some(t) = b.inset("top") {
            let want = sy + t; // 스크롤포트 상단에서 t 만큼 아래
            if bb.y < want {
                // 컨테이닝 블록 하단을 넘지 않는 선까지만
                let room = (parent.y + parent.height) - (bb.y + bb.height);
                dy = (want - bb.y).min(room.max(0.0));
            }
        } else if let Some(bo) = b.inset("bottom") {
            let want = sy + vh - bo - bb.height;
            if bb.y > want {
                let room = bb.y - parent.y;
                dy = -((bb.y - want).min(room.max(0.0)));
            }
        }
        let mut dx = 0.0f32;
        if let Some(l) = b.inset("left") {
            let want = sx + l;
            if bb.x < want {
                let room = (parent.x + parent.width) - (bb.x + bb.width);
                dx = (want - bb.x).min(room.max(0.0));
            }
        } else if let Some(r) = b.inset("right") {
            let want = sx + vw - r - bb.width;
            if bb.x > want {
                let room = bb.x - parent.x;
                dx = -((bb.x - want).min(room.max(0.0)));
            }
        }
        if dx != 0.0 || dy != 0.0 {
            if std::env::var("KESTREL_STICKY_DEBUG").is_ok() {
                eprintln!(
                    "[sticky] bb.y={} parent.y={} parent.h={} scroll={} → dy={}",
                    bb.y, parent.y, parent.height, sy, dy
                );
            }
            b.translate(dx, dy);
        }
    }
    let vp = Rect { x: 0.0, y: 0.0, width: vw, height: f32::MAX / 4.0 };
    walk(root, vp, scroll_x, scroll_y, vw, vh);
}

pub fn layout_tree<'a>(
    node: &'a StyledNode<'a>,
    mut containing_block: Dimensions,
    fonts: &FontStack,
    images: &ImageMap,
) -> LayoutBox<'a> {
    // 초기 컨테이닝 블록(뷰포트) — absolute/fixed 후처리 기준. height 0 화 전에 보존.
    let viewport_rect = containing_block.content;
    containing_block.content.height = 0.0;
    let mut root_box = build_layout_tree(node);
    // 초기 컨테이닝 블록은 BFC 를 만든다 — 루트의 float 은 문서를 벗어나지 않고 담긴다(§9.4.1).
    root_box.bfc_item = true;
    root_box.layout(containing_block, fonts, images);
    // 절대/고정 위치를 올바른 컨테이닝 블록 기준으로 재배치 (transform 적용 전)
    root_box.reposition_abs(viewport_rect, viewport_rect);
    // 레이아웃 완료 후 CSS transform(translate) 을 시각 오프셋으로 적용 (흐름 불변)
    apply_transforms(&mut root_box);
    root_box
}

// 후위 순회로 transform 의 translate/scale 을 서브트리에 적용한다.
// 흐름/형제 위치에는 영향 없음(레이아웃 후 순수 시각 변환). rotate/matrix 는 미적용.
// ── CSS 2D 변환 행렬 ──
// x' = a·x + c·y + e ;  y' = b·x + d·y + f   (CSS matrix(a,b,c,d,e,f) 와 같은 순서)
//
// 예전엔 translate/scale 만 박스 좌표를 직접 밀고 늘리는 식으로 처리하고
// rotate/skew/matrix 는 `_ => {}` 로 **조용히 무시**했다. 회전을 무시하면 화면은
// 멀쩡해 보이는데 실제와 다르다 — 가장 알아채기 어려운 종류의 거짓말이다.
// 이제 모든 함수를 행렬로 합성하고, 페인트가 서브트리 전체(글자·이미지·그림자 포함)를
// 그 행렬로 변환한다.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mat {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub e: f32,
    pub f: f32,
}

impl Mat {
    pub const IDENTITY: Mat = Mat { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: 0.0 };

    pub fn is_identity(&self) -> bool {
        *self == Mat::IDENTITY
    }

    // 축 정렬인가 (회전/기울임 없음) — 사각형이 사각형으로 남는가
    pub fn is_axis_aligned(&self) -> bool {
        self.b.abs() < 1e-6 && self.c.abs() < 1e-6
    }

    // self 다음에 m 을 적용 (m ∘ self)
    pub fn then(&self, m: &Mat) -> Mat {
        Mat {
            a: m.a * self.a + m.c * self.b,
            b: m.b * self.a + m.d * self.b,
            c: m.a * self.c + m.c * self.d,
            d: m.b * self.c + m.d * self.d,
            e: m.a * self.e + m.c * self.f + m.e,
            f: m.b * self.e + m.d * self.f + m.f,
        }
    }

    pub fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        (self.a * x + self.c * y + self.e, self.b * x + self.d * y + self.f)
    }

    pub fn invert(&self) -> Option<Mat> {
        let det = self.a * self.d - self.b * self.c;
        if det.abs() < 1e-9 {
            return None;
        }
        let id = 1.0 / det;
        Some(Mat {
            a: self.d * id,
            b: -self.b * id,
            c: -self.c * id,
            d: self.a * id,
            e: (self.c * self.f - self.d * self.e) * id,
            f: (self.b * self.e - self.a * self.f) * id,
        })
    }

    // 사각형의 네 꼭짓점을 변환한 축 정렬 경계 상자
    pub fn bounds(&self, r: Rect) -> Rect {
        let pts = [
            self.apply(r.x, r.y),
            self.apply(r.x + r.width, r.y),
            self.apply(r.x, r.y + r.height),
            self.apply(r.x + r.width, r.y + r.height),
        ];
        let (mut x0, mut y0) = (f32::MAX, f32::MAX);
        let (mut x1, mut y1) = (f32::MIN, f32::MIN);
        for (x, y) in pts {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
        Rect { x: x0, y: y0, width: x1 - x0, height: y1 - y0 }
    }
}

// 각도 문자열 → 라디안 (deg/rad/grad/turn)
fn parse_angle(s: &str) -> f32 {
    let t = s.trim();
    let num = |suf: &str| t.strip_suffix(suf).and_then(|n| n.trim().parse::<f32>().ok());
    if let Some(v) = num("deg") {
        return v.to_radians();
    }
    if let Some(v) = num("rad") {
        return v;
    }
    if let Some(v) = num("grad") {
        return v * std::f32::consts::PI / 200.0;
    }
    if let Some(v) = num("turn") {
        return v * std::f32::consts::TAU;
    }
    t.parse::<f32>().map(|v| v.to_radians()).unwrap_or(0.0)
}

// transform 함수 목록 → 행렬 (요소 로컬 좌표, 원점은 transform-origin).
// bw/bh 는 border box 크기 (translate 의 % 해석 기준).
pub fn parse_transform(text: &str, bw: f32, bh: f32) -> Mat {
    let mut m = Mat::IDENTITY;
    let mut rest = text;
    while let Some(open) = rest.find('(') {
        let name = rest[..open]
            .trim()
            .rsplit(|c: char| c.is_whitespace() || c == ')' || c == ',')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let Some(close_rel) = rest[open..].find(')') else { break };
        let close = close_rel + open;
        let args: Vec<&str> = rest[open + 1..close].split(',').map(|s| s.trim()).collect();
        let len = |t: &str, base: f32| -> f32 {
            if let Some(p) = t.strip_suffix('%') {
                p.trim().parse::<f32>().map(|v| v / 100.0 * base).unwrap_or(0.0)
            } else {
                crate::css::parse_len_px(t).unwrap_or(0.0)
            }
        };
        let num = |t: &str| t.parse::<f32>().unwrap_or(1.0);
        let get = |i: usize| args.get(i).copied().unwrap_or("");
        let step = match name.as_str() {
            "translate" => Mat {
                e: len(get(0), bw),
                f: args.get(1).map(|t| len(t, bh)).unwrap_or(0.0),
                ..Mat::IDENTITY
            },
            "translatex" => Mat { e: len(get(0), bw), ..Mat::IDENTITY },
            "translatey" => Mat { f: len(get(0), bh), ..Mat::IDENTITY },
            "scale" => {
                let sx = num(get(0));
                let sy = args.get(1).map(|t| num(t)).unwrap_or(sx);
                Mat { a: sx, d: sy, ..Mat::IDENTITY }
            }
            "scalex" => Mat { a: num(get(0)), ..Mat::IDENTITY },
            "scaley" => Mat { d: num(get(0)), ..Mat::IDENTITY },
            "rotate" | "rotatez" => {
                let (s, c) = parse_angle(get(0)).sin_cos();
                Mat { a: c, b: s, c: -s, d: c, e: 0.0, f: 0.0 }
            }
            "skew" => {
                let ax = parse_angle(get(0)).tan();
                let ay = args.get(1).map(|t| parse_angle(t).tan()).unwrap_or(0.0);
                Mat { a: 1.0, b: ay, c: ax, d: 1.0, e: 0.0, f: 0.0 }
            }
            "skewx" => Mat { c: parse_angle(get(0)).tan(), ..Mat::IDENTITY },
            "skewy" => Mat { b: parse_angle(get(0)).tan(), ..Mat::IDENTITY },
            "matrix" => Mat {
                a: args.first().map(|t| num(t)).unwrap_or(1.0),
                b: args.get(1).map(|t| t.parse().unwrap_or(0.0)).unwrap_or(0.0),
                c: args.get(2).map(|t| t.parse().unwrap_or(0.0)).unwrap_or(0.0),
                d: args.get(3).map(|t| num(t)).unwrap_or(1.0),
                e: args.get(4).map(|t| t.parse().unwrap_or(0.0)).unwrap_or(0.0),
                f: args.get(5).map(|t| t.parse().unwrap_or(0.0)).unwrap_or(0.0),
            },
            // 3D 변환은 아직 없다 — 조용히 무시하지 않고 항등으로 두되,
            // @supports 도 지원한다고 하지 않는다(supports.rs).
            _ => Mat::IDENTITY,
        };
        m = step.then(&m); // CSS 는 왼쪽 함수가 바깥쪽
        rest = &rest[close + 1..];
    }
    m
}

// transform-origin → 절대 좌표 (기본 50% 50%, border box 기준)
fn transform_origin(b: &LayoutBox) -> (f32, f32) {
    let bb = b.dimensions.border_box();
    let (mut ox, mut oy) = (bb.width / 2.0, bb.height / 2.0);
    if let Some(Value::Keyword(s)) = b.styled_node.value("transform-origin") {
        let mut toks: Vec<&str> = s.split_whitespace().collect();
        toks.truncate(2); // 3번째 값은 z (2D 에서는 무시)
        // 키워드 두 개는 순서를 뒤집어 쓸 수 있다 (CSS Transforms §6): "top left" == "left top".
        if toks.len() == 2
            && matches!(toks[0], "top" | "bottom")
            && matches!(toks[1], "left" | "right" | "center")
        {
            toks.swap(0, 1);
        }
        let axis = |t: &str, base: f32, def: f32| -> f32 {
            match t {
                "left" | "top" => 0.0,
                "center" => base / 2.0,
                "right" | "bottom" => base,
                "0" => 0.0,
                other => {
                    if let Some(p) = other.strip_suffix('%') {
                        p.parse::<f32>().map(|v| v / 100.0 * base).unwrap_or(def)
                    } else {
                        crate::css::parse_len_px(other).unwrap_or(def)
                    }
                }
            }
        };
        if let Some(t) = toks.first() {
            ox = axis(t, bb.width, ox);
        }
        if let Some(t) = toks.get(1) {
            oy = axis(t, bb.height, oy);
        }
    }
    (bb.x + ox, bb.y + oy)
}

// 각 박스의 transform 을 절대 좌표계 행렬로 계산해 저장한다 (기하는 건드리지 않는다).
// 페인트가 이 행렬로 서브트리 전체를 변환하고, CSSOM 사각형은 변환된 경계로 보고한다.
fn apply_transforms(b: &mut LayoutBox) {
    for c in &mut b.children {
        apply_transforms(c);
    }
    // 익명 박스는 부모의 styled_node 를 공유한다 — 여기서 걸러내지 않으면 부모의 transform 이
    // 자식 익명 박스에도 다시 걸려 두 번 변환된다.
    if b.anonymous {
        return;
    }
    let Some(Value::Keyword(t)) = b.styled_node.value("transform") else { return };
    if t.trim().eq_ignore_ascii_case("none") || t.trim().is_empty() {
        return;
    }
    let bb = b.dimensions.border_box();
    let local = parse_transform(&t, bb.width, bb.height);
    if local.is_identity() {
        return;
    }
    let (ox, oy) = transform_origin(b);
    // M_abs = T(o) · M · T(-o)
    let to_origin = Mat { e: -ox, f: -oy, ..Mat::IDENTITY };
    let back = Mat { e: ox, f: oy, ..Mat::IDENTITY };
    b.transform = Some(to_origin.then(&local).then(&back));
}


#[cfg(test)]
mod tests {
    use super::*;

    fn fonts() -> FontStack {
        let f = crate::font::Font::from_bytes(std::fs::read("assets/fonts/Latin.ttf").unwrap())
            .unwrap();
        FontStack::new(vec![f])
    }

    fn no_images() -> ImageMap {
        ImageMap::new()
    }

    fn all_glyphs(b: &LayoutBox, out: &mut Vec<GlyphInstance>) {
        out.extend(b.glyphs.iter().cloned());
        for c in &b.children {
            all_glyphs(c, out);
        }
    }

    fn glyphs_of(b: &LayoutBox) -> Vec<GlyphInstance> {
        let mut v = Vec::new();
        all_glyphs(b, &mut v);
        v
    }

    fn count_decorations(b: &LayoutBox) -> usize {
        b.decorations.len() + b.children.iter().map(count_decorations).sum::<usize>()
    }

    fn count_inline_borders(b: &LayoutBox) -> usize {
        b.inline_borders.len() + b.children.iter().map(count_inline_borders).sum::<usize>()
    }

    fn layout_tree_for<'a>(root: &'a StyledNode<'a>, fs: &FontStack) -> LayoutBox<'a> {
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = 800.0;
        layout_tree(root, viewport, fs, &no_images())
    }

    #[test]
    fn text_decoration_line_through_emits_decoration() {
        let fs = fonts();
        // line-through 지정 → 장식 1개 이상
        let root = crate::html::parse_dom("<p>hi</p>".to_string());
        let ss = crate::css::parse("p { display: block; text-decoration: line-through; }".to_string());
        let styled = crate::style::style_tree(&root, &ss);
        let lb = layout_tree_for(&styled, &fs);
        assert!(count_decorations(&lb) >= 1, "line-through 장식이 있어야");
        // 데코 없는 문단은 장식 0
        let root2 = crate::html::parse_dom("<p>hi</p>".to_string());
        let ss2 = crate::css::parse("p { display: block; }".to_string());
        let styled2 = crate::style::style_tree(&root2, &ss2);
        let lb2 = layout_tree_for(&styled2, &fs);
        assert_eq!(count_decorations(&lb2), 0, "장식 없어야");
    }

    // 트리에서 transform 행렬이 붙은 첫 박스를 찾는다.
    fn find_transformed<'a, 'b>(b: &'b LayoutBox<'a>) -> Option<&'b LayoutBox<'a>> {
        if b.transform.is_some() {
            return Some(b);
        }
        b.children.iter().find_map(find_transformed)
    }

    fn tree_for<'a>(root: &'a StyledNode<'a>, fs: &FontStack) -> LayoutBox<'a> {
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = 800.0;
        viewport.content.height = 600.0;
        layout_tree(root, viewport, fs, &no_images())
    }

    fn transformed_for(dom: &crate::dom::Dom, ss: &crate::css::Stylesheet, fs: &FontStack) -> (Rect, Mat) {
        // styled tree 를 여기서 만들면 수명이 짧아 반환할 수 없으므로 필요한 값만 뽑는다.
        let styled = crate::style::style_tree(dom, ss);
        let lb = tree_for(&styled, fs);
        let t = find_transformed(&lb).expect("transform 박스가 있어야");
        (t.dimensions.border_box(), t.transform.unwrap())
    }

    #[test]
    fn transform_translate_is_visual_only() {
        // transform 은 레이아웃 기하를 바꾸지 않는다 (CSS Transforms §3: 시각 변환).
        // 박스는 제자리에 있고, 행렬만 붙는다.
        let fs = fonts();
        let dom = crate::html::parse_dom("<div></div>".to_string());
        let ss = crate::css::parse(
            "div { display: block; width: 100px; height: 50px; transform: translate(10px, 20px); }".to_string(),
        );
        let (bb, m) = transformed_for(&dom, &ss, &fs);
        assert_eq!((bb.x, bb.y), (0.0, 0.0), "레이아웃 위치는 그대로");
        assert_eq!(m.apply(0.0, 0.0), (10.0, 20.0), "행렬이 (10,20) 이동");

        // 퍼센트: translateX(50%) = 자기 border-box 폭의 50%
        let ss2 = crate::css::parse(
            "div { display: block; width: 100px; height: 50px; transform: translateX(50%); }".to_string(),
        );
        let (_, m2) = transformed_for(&dom, &ss2, &fs);
        assert_eq!(m2.apply(0.0, 0.0), (50.0, 0.0), "50% × 100px = 50px");
    }

    #[test]
    fn transform_scale_maps_around_center() {
        // scale(2) → 기본 원점은 중심(50,25). 좌상단은 (-50,-25), 우하단은 (150,75).
        let fs = fonts();
        let dom = crate::html::parse_dom("<div></div>".to_string());
        let ss = crate::css::parse(
            "div { display: block; width: 100px; height: 50px; transform: scale(2); }".to_string(),
        );
        let (bb, m) = transformed_for(&dom, &ss, &fs);
        assert_eq!(m.apply(bb.x, bb.y), (-50.0, -25.0), "좌상단");
        assert_eq!(m.apply(bb.x + bb.width, bb.y + bb.height), (150.0, 75.0), "우하단");
        let b = m.bounds(bb);
        assert_eq!((b.width, b.height), (200.0, 100.0), "변환된 경계는 2배");
    }

    #[test]
    fn transform_rotate_honors_transform_origin() {
        // rotate(90deg) + transform-origin: 0 0 → 원점은 고정, (1,0) 은 (0,1) 로 간다.
        // transform-origin 은 다중 토큰이라 예전에는 값 파서가 통째로 버려서
        // **항상 중심 기준**으로 돌았다 (요행).
        let fs = fonts();
        let dom = crate::html::parse_dom("<div></div>".to_string());
        let ss = crate::css::parse(
            "div { display: block; width: 100px; height: 50px; transform: rotate(90deg); transform-origin: 0 0; }"
                .to_string(),
        );
        let (_, m) = transformed_for(&dom, &ss, &fs);
        let (ox, oy) = m.apply(0.0, 0.0);
        assert!(ox.abs() < 1e-4 && oy.abs() < 1e-4, "원점 (0,0) 은 고정: {ox},{oy}");
        let (x, y) = m.apply(1.0, 0.0);
        assert!((x - 0.0).abs() < 1e-4 && (y - 1.0).abs() < 1e-4, "(1,0) → (0,1): {x},{y}");

        // 키워드는 순서를 뒤집어 쓸 수 있다: "top left" == "left top"
        let ss2 = crate::css::parse(
            "div { display: block; width: 100px; height: 50px; transform: rotate(90deg); transform-origin: top left; }"
                .to_string(),
        );
        let (_, m2) = transformed_for(&dom, &ss2, &fs);
        let (x2, y2) = m2.apply(1.0, 0.0);
        assert!((x2 - 0.0).abs() < 1e-4 && (y2 - 1.0).abs() < 1e-4, "top left 도 같은 결과");
    }

    #[test]
    fn transform_matrix_and_skew_parse() {
        // matrix(a,b,c,d,e,f) 는 CSS 순서 그대로
        let m = parse_transform("matrix(2, 0, 0, 3, 10, 20)", 100.0, 50.0);
        assert_eq!(m.apply(1.0, 1.0), (12.0, 23.0));
        // skewX(45deg): x' = x + tan(45)·y = x + y
        let s = parse_transform("skewX(45deg)", 100.0, 50.0);
        let (x, y) = s.apply(1.0, 2.0);
        assert!((x - 3.0).abs() < 1e-4 && (y - 2.0).abs() < 1e-4, "{x},{y}");
        // 여러 함수는 왼쪽부터 차례로 적용 (좌→우 곱)
        let c = parse_transform("translate(10px, 0) scale(2)", 100.0, 50.0);
        assert_eq!(c.apply(1.0, 0.0), (12.0, 0.0), "scale 먼저, 그 다음 translate");
    }

    #[test]
    fn absolute_text_is_not_double_offset() {
        // 익명 박스는 부모의 styled_node 를 공유한다. 예전에는 reposition_abs 가
        // 익명 자식도 absolute 로 보고 **한 번 더** 옮겨서, 박스는 맞는데
        // 글자만 좌표 2배 위치에 그려졌다.
        let fs = fonts();
        let dom = crate::html::parse_dom("<main><div id=a>hi</div></main>".to_string());
        let ss = crate::css::parse(
            "main { display: block; } #a { display: block; position: absolute; left: 100px; top: 100px; }"
                .to_string(),
        );
        let styled = crate::style::style_tree(&dom, &ss);
        let lb = tree_for(&styled, &fs);
        fn glyph_xs(b: &LayoutBox, out: &mut Vec<f32>) {
            out.extend(b.glyphs.iter().map(|g| g.x));
            for c in &b.children {
                glyph_xs(c, out);
            }
        }
        let mut xs = Vec::new();
        glyph_xs(&lb, &mut xs);
        assert!(!xs.is_empty(), "글리프가 있어야");
        let min = xs.iter().cloned().fold(f32::INFINITY, f32::min);
        assert!(
            (100.0..110.0).contains(&min),
            "글자는 left:100px 에서 시작해야 (2배인 200 이면 익명 박스 이중 이동): {min}"
        );
    }

    #[test]
    fn before_pseudo_content_renders_glyphs() {
        let fs = fonts();
        // ::before content 가 있으면 글리프가 늘어난다 (생성 텍스트가 흐름에 들어감)
        let mut dom = crate::html::parse_dom("<p class=\"a\">x</p>".to_string());
        let ss = crate::css::parse(
            "p { display: block; } .a::before { content: \"AB\"; }".to_string(),
        );
        let map = crate::style::generate_pseudo_elements(&mut dom, &ss);
        let styled = crate::style::style_tree_full(&dom, &ss, crate::style::Viewport::default(), &map);
        let lb = layout_tree_for(&styled, &fs);
        let glyphs = glyphs_of(&lb);
        // "AB" (2) + "x" (1) = 3 글리프
        assert_eq!(glyphs.len(), 3, "생성 콘텐츠 AB + 본문 x = 3 글리프, 실제 {}", glyphs.len());
    }

    #[test]
    fn text_overflow_ellipsis_truncates() {
        let fs = fonts();
        let text = "a".repeat(40);
        // nowrap + ellipsis: 40px 폭이면 글리프가 잘리고 … 하나 붙음 → 40개 미만
        let root = crate::html::parse_dom(format!("<p>{}</p>", text));
        let ss = crate::css::parse(
            "p { display: block; width: 40px; white-space: nowrap; text-overflow: ellipsis; font-size: 16px; }"
                .to_string(),
        );
        let s = crate::style::style_tree(&root, &ss);
        let lb = layout_tree_for(&s, &fs);
        let g = glyphs_of(&lb);
        assert!(g.len() < 40, "잘려서 40개 미만이어야 (실제 {})", g.len());
        assert!(g.len() > 1, "일부 글자 + … 는 남아야");
    }

    #[test]
    fn word_break_wraps_long_word() {
        let long = "a".repeat(60);
        // break-all: 긴 단어가 좁은 폭에서 여러 줄로 → 높이 큼
        let broken = layout_for(
            &format!("<p>{}</p>", long),
            "p { display: block; width: 60px; word-break: break-all; font-size: 16px; }",
            200.0,
        );
        // 미지정: 한 줄로 넘침 → 높이 작음(1줄)
        let overflow = layout_for(
            &format!("<p>{}</p>", long),
            "p { display: block; width: 60px; font-size: 16px; }",
            200.0,
        );
        assert!(broken.content.height > overflow.content.height + 1.0,
            "break-all 이 여러 줄로 나눠 더 높아야 ({} > {})", broken.content.height, overflow.content.height);
    }

    #[test]
    fn word_break_inherits_to_child() {
        // word-break 는 상속 속성 — 부모에 걸면 자식 텍스트도 나뉘어야 함.
        let long = "a".repeat(60);
        let broken = layout_for(
            &format!("<div class=\"w\"><p>{}</p></div>", long),
            ".w { word-break: break-all; } \
             p { display: block; width: 60px; font-size: 16px; margin: 0; }",
            200.0,
        );
        let overflow = layout_for(
            &format!("<div><p>{}</p></div>", long),
            "p { display: block; width: 60px; font-size: 16px; margin: 0; }",
            200.0,
        );
        assert!(broken.content.height > overflow.content.height + 1.0,
            "상속된 break-all 로 자식이 여러 줄 ({} > {})", broken.content.height, overflow.content.height);
    }

    #[test]
    fn br_forces_line_breaks() {
        let fs = fonts();
        // "aaa<br>bbb<br>ccc" — <br> 두 개로 3줄. 서로 다른 baseline_y 3개여야.
        let root = crate::html::parse_dom("<p>aaa<br>bbb<br>ccc</p>".to_string());
        let ss = crate::css::parse("p { display: block; font-size: 16px; }".to_string());
        let s = crate::style::style_tree(&root, &ss);
        let lb = layout_tree_for(&s, &fs);
        let g = glyphs_of(&lb);
        let mut ys: Vec<f32> = g.iter().map(|g| g.baseline_y).collect();
        ys.sort_by(|a, b| a.partial_cmp(b).unwrap());
        ys.dedup_by(|a, b| (*a - *b).abs() < 0.5);
        assert_eq!(ys.len(), 3, "<br> 두 개면 3줄이어야 (실제 {}: {:?})", ys.len(), ys);

        // 연속 <br><br> 는 빈 줄을 만든다: "a<br><br>b" 의 두 글리프 줄 간격이
        // 한 번 개행("a<br>b")보다 정확히 한 줄 더 크다.
        let gap = |html: &str| {
            let r = crate::html::parse_dom(html.to_string());
            let st = crate::style::style_tree(&r, &ss);
            let l = layout_tree_for(&st, &fs);
            let gs = glyphs_of(&l);
            let ymin = gs.iter().map(|g| g.baseline_y).fold(f32::INFINITY, f32::min);
            let ymax = gs.iter().map(|g| g.baseline_y).fold(f32::NEG_INFINITY, f32::max);
            ymax - ymin
        };
        let one = gap("<p>a<br>b</p>");
        let two = gap("<p>a<br><br>b</p>");
        assert!(two > one * 1.5, "<br><br> 가 빈 줄로 더 벌어져야 ({} vs {})", two, one);
    }

    #[test]
    fn text_align_justify_fills_line() {
        let fs = fonts();
        let text = "one two three four five six seven eight nine ten eleven twelve";
        // justify: 첫 줄이 오른쪽 끝까지 채워짐 (마지막 글리프 x 가 크다)
        let jroot = crate::html::parse_dom(format!("<p>{}</p>", text));
        let jss = crate::css::parse("p { display: block; width: 120px; text-align: justify; font-size: 14px; }".to_string());
        let js = crate::style::style_tree(&jroot, &jss);
        let jlb = layout_tree_for(&js, &fs);
        let jg = glyphs_of(&jlb);
        // 왼쪽 정렬과 비교: justify 의 첫 줄 오른쪽 끝 글리프가 더 오른쪽
        let lroot = crate::html::parse_dom(format!("<p>{}</p>", text));
        let lss = crate::css::parse("p { display: block; width: 120px; text-align: left; font-size: 14px; }".to_string());
        let ls = crate::style::style_tree(&lroot, &lss);
        let llb = layout_tree_for(&ls, &fs);
        let lg = glyphs_of(&llb);
        // 첫 줄(y 최소) 글리프들의 최대 x
        let first_y = jg.iter().map(|g| g.baseline_y).fold(f32::INFINITY, f32::min);
        let jmax = jg.iter().filter(|g| (g.baseline_y - first_y).abs() < 1.0).map(|g| g.x).fold(0.0f32, f32::max);
        let lmax = lg.iter().filter(|g| (g.baseline_y - first_y).abs() < 1.0).map(|g| g.x).fold(0.0f32, f32::max);
        assert!(jmax > lmax + 5.0, "justify 첫 줄이 더 넓게 퍼짐 ({} > {})", jmax, lmax);
    }

    #[test]
    fn vertical_align_super_raises_glyph() {
        let fs = fonts();
        // "a<sup>1</sup>" — sup 글리프가 baseline 위로(작은 y)
        let root = crate::html::parse_dom("<p>a<sup>1</sup></p>".to_string());
        let mut ss = crate::css::user_agent_stylesheet();
        ss.rules.extend(crate::css::parse("p { display: block; } sup { vertical-align: super; }".to_string()).rules);
        let styled = crate::style::style_tree(&root, &ss);
        let lb = layout_tree_for(&styled, &fs);
        let g = glyphs_of(&lb);
        assert_eq!(g.len(), 2, "'a' + '1'");
        // 두 번째 글리프(1, sup)의 baseline_y 가 첫 글리프(a)보다 작다(위로 올라감)
        assert!(g[1].baseline_y < g[0].baseline_y, "sup 이 위로 ({} < {})", g[1].baseline_y, g[0].baseline_y);
    }

    #[test]
    fn text_indent_offsets_first_line() {
        let fs = fonts();
        let root = crate::html::parse_dom("<p>hello</p>".to_string());
        let ss = crate::css::parse("p { display: block; text-indent: 30px; }".to_string());
        let s = crate::style::style_tree(&root, &ss);
        let lb = layout_tree_for(&s, &fs);
        let g = glyphs_of(&lb);
        // 첫 글리프가 들여쓰기(30px)만큼 오른쪽에서 시작
        assert!(g[0].x >= 30.0, "첫 글자 x >= 30, 실제 {}", g[0].x);
    }

    #[test]
    fn letter_spacing_widens_text() {
        let fs = fonts();
        let base = crate::html::parse_dom("<p>hello</p>".to_string());
        let ss0 = crate::css::parse("p { display: block; }".to_string());
        let s0 = crate::style::style_tree(&base, &ss0);
        let lb0 = layout_tree_for(&s0, &fs);
        let g0 = glyphs_of(&lb0);
        // letter-spacing 5px → 마지막 글리프가 더 오른쪽
        let sp = crate::html::parse_dom("<p>hello</p>".to_string());
        let ss1 = crate::css::parse("p { display: block; letter-spacing: 5px; }".to_string());
        let s1 = crate::style::style_tree(&sp, &ss1);
        let lb1 = layout_tree_for(&s1, &fs);
        let g1 = glyphs_of(&lb1);
        assert_eq!(g0.len(), g1.len(), "글리프 수 동일");
        assert!(g1.last().unwrap().x > g0.last().unwrap().x + 10.0, "letter-spacing 이 글자 간격을 넓혀야");
    }

    #[test]
    fn ua_underlines_links() {
        let fs = fonts();
        let root = crate::html::parse_dom("<p><a href=\"/x\">link</a></p>".to_string());
        let mut ss = crate::css::user_agent_stylesheet();
        ss.rules.extend(crate::css::parse("p { display: block; }".to_string()).rules);
        let styled = crate::style::style_tree(&root, &ss);
        let lb = layout_tree_for(&styled, &fs);
        assert!(count_decorations(&lb) >= 1, "UA 스타일시트로 링크에 밑줄이 있어야");
    }

    fn layout_for(html: &str, css: &str, viewport_width: f32) -> Dimensions {
        let root = crate::html::parse_dom(html.to_string());
        let ss = crate::css::parse(css.to_string());
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = viewport_width;
        let fs = fonts();
        let lb = layout_tree(&styled, viewport, &fs, &no_images());
        lb.dimensions
    }

    #[test]
    fn aspect_ratio_derives_height_from_width() {
        // width 200px, aspect-ratio 2/1 → 높이 100px
        let d = layout_for(
            "<div></div>",
            "div { display: block; width: 200px; aspect-ratio: 2 / 1; }",
            800.0,
        );
        assert_eq!(d.content.width, 200.0);
        assert_eq!(d.content.height, 100.0, "200 / 2 = 100");
        // 명시 height 는 aspect-ratio 를 이긴다
        let d2 = layout_for(
            "<div></div>",
            "div { display: block; width: 200px; height: 30px; aspect-ratio: 2 / 1; }",
            800.0,
        );
        assert_eq!(d2.content.height, 30.0, "명시 height 우선");
    }

    #[test]
    fn min_width_expands_narrow_box() {
        // width:50px 이지만 min-width:200px 이면 200 으로 확장
        let d = layout_for(
            "<div></div>",
            "div { display: block; width: 50px; min-width: 200px; }",
            800.0,
        );
        assert!((d.content.width - 200.0).abs() < 0.5, "min-width 200, 실제 {}", d.content.width);
    }

    #[test]
    fn min_height_expands_short_box() {
        // 내용이 없어 높이 0 이지만 min-height:120px → 120
        let d = layout_for(
            "<div></div>",
            "div { display: block; min-height: 120px; }",
            800.0,
        );
        assert!((d.content.height - 120.0).abs() < 0.5, "min-height 120, 실제 {}", d.content.height);
    }

    #[test]
    fn max_height_clamps_box_always() {
        // max-height 는 overflow 와 무관하게 사용 높이를 항상 클램프(CSS §10.7).
        // overflow:hidden 케이스
        let clipped = layout_for(
            "<div><div class=\"tall\"></div></div>",
            "div { display: block; } div > div { height: 300px; } \
             div:first-child, div { max-height: 100px; overflow: hidden; }",
            800.0,
        );
        assert!(clipped.content.height <= 100.5, "overflow:hidden + max-height → 클램프, 실제 {}", clipped.content.height);
        // overflow:visible 여도 박스는 클램프(내용은 넘쳐도 박스 높이는 상한)
        let visible = layout_for(
            "<div class=\"o\"><div class=\"c\"></div></div>",
            ".o { display: block; max-height: 100px; } .c { display: block; height: 300px; }",
            800.0,
        );
        assert!((visible.content.height - 100.0).abs() < 0.5,
            "overflow:visible 여도 max-height 클램프, 실제 {}", visible.content.height);
    }

    #[test]
    fn fixed_width_and_height_block() {
        let d = layout_for(
            "<div></div>",
            "div { display: block; width: 200px; height: 100px; }",
            800.0,
        );
        assert_eq!(d.content.width, 200.0);
        assert_eq!(d.content.height, 100.0);
        assert_eq!(d.content.x, 0.0);
        assert_eq!(d.content.y, 0.0);
    }

    #[test]
    fn line_height_sets_line_box_height() {
        // 단위 없는 line-height:2 는 font-size(20px) 배수 → 줄 높이 40px
        let tall = layout_for(
            "<p>hi</p>",
            "p { display: block; font-size: 20px; line-height: 2; }",
            800.0,
        );
        assert!((tall.content.height - 40.0).abs() < 0.5, "2 × 20px = 40, 실제 {}", tall.content.height);
        // 명시 안 하면 폰트 메트릭 기반(더 작음)
        let normal = layout_for("<p>hi</p>", "p { display: block; font-size: 20px; }", 800.0);
        assert!(normal.content.height < tall.content.height, "기본이 line-height:2 보다 작아야");
    }

    #[test]
    fn unitless_line_height_inherits_as_factor() {
        // 부모 line-height:2 (배수). 자식 font-size 40 은 상속받은 factor 2 를 자기
        // font-size 에 곱해야 함 → 자식 줄 높이 80. (예전엔 부모 20×2=40px 가 그대로
        // 상속돼 자식도 40 이 됐다.) 외곽 블록 높이 = 자식 블록 높이.
        let d = layout_for(
            "<div class=\"o\"><p>hi</p></div>",
            ".o { display: block; font-size: 20px; line-height: 2; } \
             p { display: block; font-size: 40px; margin: 0; }",
            800.0,
        );
        assert!((d.content.height - 80.0).abs() < 0.5, "2 × 40px = 80, 실제 {}", d.content.height);
    }

    #[test]
    fn auto_width_fills_containing_block_minus_padding() {
        let d = layout_for("<div></div>", "div { display: block; padding: 10px; }", 300.0);
        assert_eq!(d.content.width, 280.0);
        assert_eq!(d.content.x, 10.0);
    }

    #[test]
    fn min_max_clamp_width() {
        // 컨테이닝 블록 400. min(100px, 50%) = min(100, 200) = 100
        let d = layout_for("<div></div>", "div { display: block; width: min(100px, 50%); }", 400.0);
        assert_eq!(d.content.width, 100.0);
        // max(100px, 50%) = max(100, 200) = 200
        let d2 = layout_for("<div></div>", "div { display: block; width: max(100px, 50%); }", 400.0);
        assert_eq!(d2.content.width, 200.0);
        // clamp(50px, 50%, 150px): 50%=200 → 150 으로 상한
        let d3 = layout_for("<div></div>", "div { display: block; width: clamp(50px, 50%, 150px); }", 400.0);
        assert_eq!(d3.content.width, 150.0);
    }

    #[test]
    fn max_width_clamps_auto_width() {
        let d = layout_for("<div></div>", "div { display: block; max-width: 200px; }", 800.0);
        assert_eq!(d.content.width, 200.0);
    }

    #[test]
    fn max_width_does_not_grow_smaller_width() {
        let d = layout_for(
            "<div></div>",
            "div { display: block; width: 100px; max-width: 200px; }",
            800.0,
        );
        assert_eq!(d.content.width, 100.0);
    }

    #[test]
    fn max_width_with_auto_margins_centers() {
        let d = layout_for(
            "<div></div>",
            "div { display: block; max-width: 200px; margin: 0 auto; }",
            800.0,
        );
        assert_eq!(d.content.width, 200.0);
        assert_eq!(d.content.x, 300.0);
    }

    #[test]
    fn children_stack_vertically() {
        let root = crate::html::parse_dom(
            "<div class=\"outer\"><div class=\"inner\"></div><div class=\"inner\"></div></div>"
                .to_string(),
        );
        let ss = crate::css::parse(
            ".outer { display: block; } .inner { display: block; height: 50px; }".to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = 400.0;
        let fs = fonts();
        let lb = layout_tree(&styled, viewport, &fs, &no_images());
        assert_eq!(lb.children.len(), 2);
        assert_eq!(lb.children[0].dimensions.content.y, 0.0);
        assert_eq!(lb.children[1].dimensions.content.y, 50.0);
        assert_eq!(lb.dimensions.content.height, 100.0);
    }

    #[test]
    fn text_box_produces_glyphs_and_height() {
        let root = crate::html::parse_dom("<p>hello world</p>".to_string());
        let ss = crate::css::parse("p { display: block; font-size: 20px; }".to_string());
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = 400.0;
        let fs = fonts();
        let lb = layout_tree(&styled, viewport, &fs, &no_images());
        assert!(!glyphs_of(&lb).is_empty(), "text should produce glyphs");
        assert!(lb.dimensions.content.height > 0.0);
    }

    #[test]
    fn long_text_wraps_to_multiple_lines() {
        let root =
            crate::html::parse_dom("<p>aaaa bbbb cccc dddd eeee ffff gggg hhhh</p>".to_string());
        let ss = crate::css::parse("p { display: block; font-size: 20px; }".to_string());
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = 120.0;
        let fs = fonts();
        let lb = layout_tree(&styled, viewport, &fs, &no_images());
        let gs = glyphs_of(&lb);
        let first = gs.first().unwrap().baseline_y;
        let last = gs.last().unwrap().baseline_y;
        assert!(last > first, "later glyphs should be on lower lines");
    }

    #[test]
    fn inline_element_text_is_collected() {
        let root = crate::html::parse_dom("<p>a <span>b</span> c</p>".to_string());
        let ss = crate::css::parse("p { display: block; font-size: 20px; }".to_string());
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = 400.0;
        let fs = fonts();
        let lb = layout_tree(&styled, viewport, &fs, &no_images());
        assert!(glyphs_of(&lb).len() >= 3, "inline text should be collected");
    }

    #[test]
    fn inline_only_block_has_nonzero_height() {
        let root = crate::html::parse_dom("<div><a>link</a></div>".to_string());
        let ss = crate::css::parse("div { display: block; } a { display: inline; }".to_string());
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = 400.0;
        let fs = fonts();
        let lb = layout_tree(&styled, viewport, &fs, &no_images());
        assert!(lb.dimensions.content.height > 0.0, "inline-only block must have height");
        assert!(!glyphs_of(&lb).is_empty(), "link text should render");
    }

    fn flex_layout(html: &str, css: &str, width: f32) -> Vec<Dimensions> {
        let root = crate::html::parse_dom(html.to_string());
        let ss = crate::css::parse(css.to_string());
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = width;
        let fs = fonts();
        let lb = layout_tree(&styled, viewport, &fs, &no_images());
        lb.children.iter().map(|c| c.dimensions).collect()
    }

    // 스크롤 위치를 주고 sticky 후처리까지 돌린 뒤, 루트의 자손 박스들을 돌려준다.
    fn sticky_layout(html: &str, css: &str, scroll_y: f32) -> Vec<(String, Rect)> {
        let root = crate::html::parse_dom(html.to_string());
        let ss = crate::css::parse(css.to_string());
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = 800.0;
        let fs = fonts();
        let mut lb = layout_tree(&styled, viewport, &fs, &no_images());
        apply_sticky(&mut lb, 0.0, scroll_y, 800.0, 600.0);
        let mut out = Vec::new();
        fn walk(b: &LayoutBox, out: &mut Vec<(String, Rect)>) {
            if let NodeType::Element(e) = &b.styled_node.node.node_type {
                let id = e.attributes.get("id").cloned().unwrap_or_default();
                if !id.is_empty() {
                    out.push((id, b.dimensions.border_box()));
                }
            }
            for c in &b.children {
                walk(c, out);
            }
        }
        walk(&lb, &mut out);
        out
    }

    #[test]
    fn position_sticky_sticks_and_releases() {
        // position: sticky (CSS Position §3.4) — 정상 흐름에 남되, 스크롤포트를 인셋만큼
        // 줄인 사각형 안에 머물도록 밀리고, 컨테이닝 블록을 벗어나지는 않는다.
        // 예전엔 미구현이라 static 으로 떨어졌다 (@supports 도 거짓으로 보고했다).
        let html = "<div id=\"sp\"></div><div id=\"wrap\"><div id=\"h\"></div></div>";
        let css = "#sp { display:block; height: 400px; } \
                   #wrap { display:block; height: 1000px; } \
                   #h { display:block; position: sticky; top: 10px; height: 40px; }";
        let find = |v: &Vec<(String, Rect)>, id: &str| -> Rect {
            v.iter().find(|(k, _)| k == id).unwrap().1
        };

        // 스크롤 0: 정상 흐름 위치 (y=400)
        let v = sticky_layout(html, css, 0.0);
        assert_eq!(find(&v, "h").y, 400.0, "스크롤 전엔 정상 위치");

        // 스크롤 600: 스크롤포트 상단 + 10 에 붙는다 (문서 좌표 610)
        let v = sticky_layout(html, css, 600.0);
        assert_eq!(find(&v, "h").y, 610.0, "top:10 위치에 붙는다");

        // 스크롤 2000: 컨테이닝 블록(400..1400) 밖으로는 못 나간다 → 1400-40 = 1360 에서 멈춤
        let v = sticky_layout(html, css, 2000.0);
        assert_eq!(find(&v, "h").y, 1360.0, "컨테이닝 블록 하단에서 놓아준다");
    }

    #[test]
    fn position_relative_offsets_without_affecting_siblings() {
        let root = crate::html::parse_dom(
            "<div class=\"a\"></div><div class=\"b\"></div>".to_string(),
        );
        let ss = crate::css::parse(
            ".a { display: block; height: 20px; position: relative; top: 10px; left: 15px; } \
             .b { display: block; height: 20px; }"
                .to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = 300.0;
        let fs = fonts();
        let lb = layout_tree(&styled, viewport, &fs, &no_images());
        // a 는 (15, 10) 으로 이동
        assert_eq!(lb.children[0].dimensions.content.x, 15.0);
        assert_eq!(lb.children[0].dimensions.content.y, 10.0);
        // b 는 a 의 정상 흐름 위치(y=20) 유지 — relative 는 형제에 영향 없음
        assert_eq!(lb.children[1].dimensions.content.y, 20.0);
    }

    #[test]
    fn position_absolute_out_of_flow_and_placed() {
        let root = crate::html::parse_dom(
            "<div class=\"wrap\"><div class=\"abs\"></div><div class=\"flow\"></div></div>"
                .to_string(),
        );
        let ss = crate::css::parse(
            ".wrap { display: block; position: relative; } \
             .abs { display: block; position: absolute; top: 5px; right: 0; width: 40px; height: 30px; } \
             .flow { display: block; height: 25px; }"
                .to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = 200.0;
        let fs = fonts();
        let lb = layout_tree(&styled, viewport, &fs, &no_images());
        // 단일 루트라 lb 자체가 wrap
        assert_eq!(lb.dimensions.content.height, 25.0, "wrap 높이 = flow 만 (abs 흐름 제외)");
        let abs = &lb.children[0];
        // right:0 → x = 200 - 40 = 160, top:5 → y = 5
        assert_eq!(abs.dimensions.content.x, 160.0);
        assert_eq!(abs.dimensions.content.y, 5.0);
        // flow 는 y=0 (abs 가 공간을 안 차지하므로 맨 위)
        assert_eq!(lb.children[1].dimensions.content.y, 0.0);
    }

    #[test]
    fn absolute_inset_stretches_to_fill() {
        let fs = fonts();
        let mut ss = crate::css::user_agent_stylesheet();
        ss.rules.extend(
            crate::css::parse(
                ".p{position:relative;display:block;width:200px;height:100px;} \
                 .c{position:absolute;display:block;left:10px;right:10px;top:5px;bottom:5px;}"
                    .to_string(),
            )
            .rules,
        );
        let root =
            crate::html::parse_dom("<div class=\"p\"><div class=\"c\"></div></div>".to_string());
        let styled = crate::style::style_tree(&root, &ss);
        let mut vp: Dimensions = Default::default();
        vp.content.width = 400.0;
        let lb = layout_tree(&styled, vp, &fs, &no_images());
        fn find_abs<'b>(b: &'b LayoutBox<'b>) -> Option<&'b LayoutBox<'b>> {
            if b.position() == "absolute" {
                return Some(b);
            }
            b.children.iter().find_map(find_abs)
        }
        let c = find_abs(&lb).expect("절대 위치 자식");
        // 좌우10/상하5 인셋 → 200-20=180 x 100-10=90, (10,5)
        assert!((c.dimensions.content.width - 180.0).abs() < 0.5, "폭 180 (실제 {})", c.dimensions.content.width);
        assert!((c.dimensions.content.height - 90.0).abs() < 0.5, "높이 90 (실제 {})", c.dimensions.content.height);
        assert!((c.dimensions.content.x - 10.0).abs() < 0.5, "x=10 (실제 {})", c.dimensions.content.x);
        assert!((c.dimensions.content.y - 5.0).abs() < 0.5, "y=5 (실제 {})", c.dimensions.content.y);
    }

    #[test]
    fn position_absolute_uses_nearest_positioned_ancestor() {
        // abs 는 정적 wrapper(.mid)를 건너뛰고 positioned 조상(.rel) 기준으로 배치돼야.
        let root = crate::html::parse_dom(
            "<div class=\"rel\"><div class=\"mid\"><div class=\"abs\"></div></div></div>".to_string(),
        );
        let ss = crate::css::parse(
            ".rel { display: block; position: relative; width: 300px; height: 150px; } \
             .mid { display: block; margin-left: 100px; width: 120px; } \
             .abs { display: block; position: absolute; top: 0; right: 0; width: 40px; height: 24px; }"
                .to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = 500.0;
        let fs = fonts();
        let lb = layout_tree(&styled, viewport, &fs, &no_images());
        let abs = &lb.children[0].children[0];
        // right:0 을 .rel(폭300) 기준으로 → x = 0 + 300 - 40 = 260 (.mid 기준이면 180)
        assert_eq!(abs.dimensions.content.x, 260.0, "가장 가까운 positioned 조상(.rel) 기준이어야");
        assert_eq!(abs.dimensions.content.y, 0.0);
    }

    #[test]
    fn checkbox_and_radio_render_as_native_controls() {
        let fs = fonts();
        let root = crate::html::parse_dom(
            "<div><input type=\"checkbox\" checked><input type=\"radio\"></div>".to_string(),
        );
        let ss = crate::css::user_agent_stylesheet();
        let styled = crate::style::style_tree(&root, &ss);
        let mut vp: Dimensions = Default::default();
        vp.content.width = 300.0;
        let lb = layout_tree(&styled, vp, &fs, &no_images());
        fn collect(b: &LayoutBox, out: &mut Vec<(crate::layout::FormControl, f32)>) {
            if let Some(fc) = b.form_control {
                out.push((fc, b.dimensions.content.width));
            }
            for c in &b.children {
                collect(c, out);
            }
        }
        let mut fcs = Vec::new();
        collect(&lb, &mut fcs);
        assert!(
            fcs.iter().any(|(fc, _)| *fc == crate::layout::FormControl::Checkbox(true)),
            "체크된 체크박스 표식"
        );
        assert!(
            fcs.iter().any(|(fc, _)| *fc == crate::layout::FormControl::Radio(false)),
            "라디오 표식"
        );
        // 작은 고정 크기(≈13px)로 줄어야 (기존 180px 텍스트박스 버그 회귀 방지)
        assert!(fcs.iter().all(|(_, w)| (*w - 13.0).abs() < 0.5), "13px 컨트롤: {:?}", fcs.iter().map(|(_, w)| *w).collect::<Vec<_>>());
    }

    #[test]
    fn select_shows_only_selected_option() {
        let fs = fonts();
        let root = crate::html::parse_dom(
            "<select><option>Apple</option><option selected>Banana</option><option>Cherry</option></select>"
                .to_string(),
        );
        let ss = crate::css::user_agent_stylesheet();
        let styled = crate::style::style_tree(&root, &ss);
        let mut vp: Dimensions = Default::default();
        vp.content.width = 300.0;
        let lb = layout_tree(&styled, vp, &fs, &no_images());
        // "Banana" = 6 글리프만 (Apple/Cherry 는 렌더 안 됨)
        let g = glyphs_of(&lb);
        assert_eq!(g.len(), 6, "선택된 option(Banana)만 렌더 (실제 {}글리프)", g.len());
    }

    #[test]
    fn table_caption_renders_above_rows() {
        let fs = fonts();
        let root = crate::html::parse_dom(
            "<table><caption>Cap</caption><tr><td>A</td></tr></table>".to_string(),
        );
        let ss = crate::css::user_agent_stylesheet();
        let styled = crate::style::style_tree(&root, &ss);
        let mut vp: Dimensions = Default::default();
        vp.content.width = 300.0;
        let lb = layout_tree(&styled, vp, &fs, &no_images());
        fn subtree_glyphs(b: &LayoutBox) -> usize {
            b.glyphs.len() + b.children.iter().map(subtree_glyphs).sum::<usize>()
        }
        fn collect_boxes(b: &LayoutBox, out: &mut Vec<(String, f32, usize)>) {
            if let NodeType::Element(e) = &b.styled_node.node.node_type {
                out.push((e.tag_name.clone(), b.dimensions.content.y, subtree_glyphs(b)));
            }
            for c in &b.children {
                collect_boxes(c, out);
            }
        }
        let mut boxes = Vec::new();
        collect_boxes(&lb, &mut boxes);
        let cap = boxes.iter().find(|(t, _, _)| t == "caption").expect("캡션 박스");
        let td = boxes.iter().find(|(t, _, _)| t == "td").expect("셀 박스");
        assert!(cap.2 > 0, "캡션 텍스트가 렌더돼야");
        assert!(cap.1 < td.1, "캡션이 셀 위에 있어야 ({} < {})", cap.1, td.1);
    }

    #[test]
    fn ua_heading_font_sizes() {
        let ss = crate::css::user_agent_stylesheet();
        let root = crate::html::parse_dom("<h1>T</h1><h2>S</h2><p>b</p>".to_string());
        let styled = crate::style::style_tree(&root, &ss);
        fn find(n: &StyledNode, tag: &str) -> Option<f32> {
            if let NodeType::Element(e) = &n.node.node_type {
                if e.tag_name == tag {
                    return n.value("font-size").map(|v| v.to_px());
                }
            }
            n.children.iter().find_map(|c| find(c, tag))
        }
        // h1 = 2em = 32px, h2 = 1.5em = 24px (기본 16 기준)
        assert!((find(&styled, "h1").unwrap() - 32.0).abs() < 0.5, "h1=2em");
        assert!((find(&styled, "h2").unwrap() - 24.0).abs() < 0.5, "h2=1.5em");
    }

    #[test]
    fn column_count_distributes_children_into_columns() {
        let fs = fonts();
        let mut html = String::from("<div class=\"c\">");
        for i in 0..6 {
            html.push_str(&format!("<div class=\"b\">{}</div>", i));
        }
        html.push_str("</div>");
        let ss = crate::css::parse(
            ".c{display:block;column-count:3;column-gap:10px;} .b{display:block;height:20px;}"
                .to_string(),
        );
        let root = crate::html::parse_dom(html);
        let styled = crate::style::style_tree(&root, &ss);
        let mut vp: Dimensions = Default::default();
        vp.content.width = 330.0;
        let lb = layout_tree(&styled, vp, &fs, &no_images());
        assert_eq!(lb.children.len(), 6);
        let x = |i: usize| lb.children[i].dimensions.content.x;
        let y = |i: usize| lb.children[i].dimensions.content.y;
        // 6개 → 3열에 2개씩: (0,1),(2,3),(4,5)
        assert!((x(0) - x(1)).abs() < 0.5, "0,1 같은 열");
        assert!(x(2) > x(0) + 50.0, "2 는 둘째 열 (실제 {} vs {})", x(2), x(0));
        assert!(x(4) > x(2) + 50.0, "4 는 셋째 열");
        assert!((y(0) - y(2)).abs() < 0.5 && (y(0) - y(4)).abs() < 0.5, "각 열 top 정렬");
    }

    #[test]
    fn adjacent_block_margins_collapse() {
        let fs = fonts();
        let ss = crate::css::parse(
            ".a{display:block;height:20px;margin-bottom:30px;} \
             .b{display:block;height:20px;margin-top:30px;}"
                .to_string(),
        );
        let root =
            crate::html::parse_dom("<div class=\"a\"></div><div class=\"b\"></div>".to_string());
        let styled = crate::style::style_tree(&root, &ss);
        let mut vp: Dimensions = Default::default();
        vp.content.width = 300.0;
        let lb = layout_tree(&styled, vp, &fs, &no_images());
        assert_eq!(lb.children[0].dimensions.content.y, 0.0);
        // A 하단(20) + 상쇄 margin max(30,30)=30 → B 는 y=50 (더해서 80 이 아님)
        assert_eq!(
            lb.children[1].dimensions.content.y, 50.0,
            "인접 형제 margin 상쇄: B 는 y=50"
        );
    }

    #[test]
    fn parent_child_top_margin_collapses() {
        let fs = fonts();
        // A(20) 다음 B(패딩/테두리 없음) > inner(margin-top:40). inner 의 상단 margin 은
        // B 로 hoisting 되어 A 와 상쇄 → inner 는 B 상단에 붙고, B 는 y=60(=20+40).
        // 대조: P 는 padding-top 있어 상쇄 차단 → innerP 의 40 이 P 내부에 남는다.
        let ss = crate::css::parse(
            ".a{display:block;height:20px;} \
             .b{display:block;} \
             .inner{display:block;height:20px;margin-top:40px;} \
             .p{display:block;padding-top:10px;} \
             .innerp{display:block;height:20px;margin-top:40px;}"
                .to_string(),
        );
        let root = crate::html::parse_dom(
            "<div class=\"a\"></div>\
             <div class=\"b\"><div class=\"inner\"></div></div>\
             <div class=\"p\"><div class=\"innerp\"></div></div>"
                .to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut vp: Dimensions = Default::default();
        vp.content.width = 300.0;
        let lb = layout_tree(&styled, vp, &fs, &no_images());
        let b = &lb.children[1];
        let inner = &b.children[0];
        assert_eq!(b.dimensions.content.y, 60.0, "상쇄 통과: B 는 A하단(20)+상쇄margin(40)=60");
        assert_eq!(
            inner.dimensions.content.y - b.dimensions.content.y,
            0.0,
            "inner 상단이 B 상단에 붙음(내부 40 여백 없음)"
        );
        let p = &lb.children[2];
        let innerp = &p.children[0];
        assert_eq!(
            innerp.dimensions.content.y - p.dimensions.content.y,
            40.0,
            "padding-top 이 상쇄 차단 → innerP 의 margin 40 이 P 내부에 남음"
        );
    }

    #[test]
    fn grid_justify_content_center_offsets_tracks() {
        let fs = fonts();
        // 3×100px 트랙(gap 0), 컨테이너 400 → 여유 100. justify-content:center → 시작 오프셋 50.
        let ss = crate::css::parse(
            ".g { display: grid; grid-template-columns: 100px 100px 100px; justify-content: center; } \
             .c { display: block; }"
                .to_string(),
        );
        let root = crate::html::parse_dom(
            "<div class=\"g\"><div class=\"c\"></div><div class=\"c\"></div><div class=\"c\"></div></div>"
                .to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut vp: Dimensions = Default::default();
        vp.content.width = 400.0;
        let lb = layout_tree(&styled, vp, &fs, &no_images());
        let first = &lb.children[0];
        assert!(
            (first.dimensions.content.x - 50.0).abs() < 1.0,
            "justify-content:center → 첫 셀 x=50, 실제 {}",
            first.dimensions.content.x
        );
    }

    #[test]
    fn float_percentage_width_resolves_against_container() {
        let fs = fonts();
        // float 의 % width 는 컨테이너 폭 기준으로 한 번만 해석돼야(이중 축소·밴드기준 버그 방지).
        let ss = crate::css::parse(
            ".row { display: block; } \
             .a { display: block; float: left; width: 60%; } \
             .b { display: block; float: left; width: 40%; }"
                .to_string(),
        );
        let root = crate::html::parse_dom(
            "<div class=\"row\"><div class=\"a\"></div><div class=\"b\"></div></div>".to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut vp: Dimensions = Default::default();
        vp.content.width = 1000.0;
        let lb = layout_tree(&styled, vp, &fs, &no_images());
        let a = &lb.children[0];
        let b = &lb.children[1];
        assert!((a.dimensions.content.width - 600.0).abs() < 1.0, "a=60% → 600, 실제 {}", a.dimensions.content.width);
        assert!((b.dimensions.content.width - 400.0).abs() < 1.0, "b=40% → 400, 실제 {}", b.dimensions.content.width);
        assert!((b.dimensions.content.x - 600.0).abs() < 1.0, "b 는 a 오른쪽(x=600), 실제 {}", b.dimensions.content.x);
    }

    #[test]
    fn inline_horizontal_padding_advances_content() {
        let fs = fonts();
        // 인라인 <span> 의 padding-left 30 이 뒤 글리프를 최소 30px 밀어야 한다(§10.3.1).
        let ss = crate::css::parse(
            "div { display: block; font-size: 16px; } .p { padding-left: 30px; }".to_string(),
        );
        let root = crate::html::parse_dom("<div>A<span class=\"p\">B</span></div>".to_string());
        let styled = crate::style::style_tree(&root, &ss);
        let mut vp: Dimensions = Default::default();
        vp.content.width = 300.0;
        let lb = layout_tree(&styled, vp, &fs, &no_images());
        fn glyph_xs(b: &LayoutBox, out: &mut Vec<f32>) {
            out.extend(b.glyphs.iter().map(|g| g.x));
            for c in &b.children {
                glyph_xs(c, out);
            }
        }
        let mut xs = Vec::new();
        glyph_xs(&lb, &mut xs);
        assert!(xs.len() >= 2, "A,B 두 글리프");
        let (a, b) = (xs[0], xs[xs.len() - 1]);
        assert!(b - a >= 30.0, "padding-left 30 이 B 를 밀어냄: 실제 간격 {}", b - a);
    }

    #[test]
    fn float_escapes_non_bfc_wrapper() {
        let fs = fonts();
        // §9.5: float 은 최근접 BFC 소속. 비BFC 래퍼는 float 을 담지 않아 높이가 0 이고,
        // float 은 래퍼 밖(부모 BFC)으로 넘쳐 뒤 형제가 우회하게 된다.
        let root = crate::html::parse_dom(
            "<div class=\"outer\"><div class=\"wrap\"><div class=\"fl\"></div></div></div>".to_string(),
        );
        let ss = crate::css::parse(
            ".outer { display: block; } \
             .wrap { display: block; } \
             .fl { display: block; float: left; width: 80px; height: 80px; }"
                .to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut vp: Dimensions = Default::default();
        vp.content.width = 300.0;
        let lb = layout_tree(&styled, vp, &fs, &no_images());
        // lb = outer(BFC 아님이지만 루트라 담음? 아니 — outer 는 비BFC). wrap 은 lb.children[0].
        let wrap = &lb.children[0];
        let fl = &wrap.children[0];
        assert_eq!(fl.dimensions.content.height, 80.0, "float 자체는 80");
        assert_eq!(wrap.dimensions.content.height, 0.0, "비BFC 래퍼는 float 을 담지 않음(높이 0)");
    }

    #[test]
    fn ol_start_and_reversed_number_correctly() {
        let fs = fonts();
        let ss = crate::css::user_agent_stylesheet();
        let markers = |html: &str| -> Vec<String> {
            let root = crate::html::parse_dom(html.to_string());
            let styled = crate::style::style_tree(&root, &ss);
            let mut vp: Dimensions = Default::default();
            vp.content.width = 300.0;
            let lb = layout_tree(&styled, vp, &fs, &no_images());
            fn walk(b: &LayoutBox, out: &mut Vec<String>) {
                if let NodeType::Element(e) = &b.styled_node.node.node_type {
                    if e.tag_name == "li" {
                        if let Some(m) = &b.list_marker {
                            out.push(m.clone());
                        }
                    }
                }
                for c in &b.children {
                    walk(c, out);
                }
            }
            let mut out = Vec::new();
            walk(&lb, &mut out);
            out
        };
        assert_eq!(markers("<ol start=\"5\"><li>a</li><li>b</li></ol>"), vec!["5.", "6."]);
        assert_eq!(
            markers("<ol reversed><li>a</li><li>b</li><li>c</li></ol>"),
            vec!["3.", "2.", "1."]
        );
    }

    #[test]
    fn hr_rule_and_blockquote_dd_indent() {
        let fs = fonts();
        let ss = crate::css::user_agent_stylesheet();
        let root = crate::html::parse_dom(
            "<hr><blockquote>q</blockquote><dl><dd>d</dd></dl>".to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut vp: Dimensions = Default::default();
        vp.content.width = 300.0;
        let lb = layout_tree(&styled, vp, &fs, &no_images());
        fn find(b: &LayoutBox, tag: &str, out: &mut Vec<(f32, f32)>) {
            if let NodeType::Element(e) = &b.styled_node.node.node_type {
                if e.tag_name == tag {
                    out.push((b.dimensions.content.x, b.dimensions.border.top));
                }
            }
            for c in &b.children {
                find(c, tag, out);
            }
        }
        let mut hr = Vec::new();
        find(&lb, "hr", &mut hr);
        assert!(hr[0].1 >= 1.0, "hr 은 border-top 로 선을 그림 ({})", hr[0].1);
        let mut bq = Vec::new();
        find(&lb, "blockquote", &mut bq);
        assert!((bq[0].0 - 40.0).abs() < 0.5, "blockquote 좌여백 40 (실제 {})", bq[0].0);
        let mut dd = Vec::new();
        find(&lb, "dd", &mut dd);
        assert!((dd[0].0 - 40.0).abs() < 0.5, "dd 좌여백 40 (실제 {})", dd[0].0);
    }

    #[test]
    fn inline_element_background_paints() {
        let fs = fonts();
        let ss = crate::css::user_agent_stylesheet();
        let root = crate::html::parse_dom("<p>a <mark>hi</mark> b</p>".to_string());
        let styled = crate::style::style_tree(&root, &ss);
        let mut vp: Dimensions = Default::default();
        vp.content.width = 300.0;
        let lb = layout_tree(&styled, vp, &fs, &no_images());
        fn collect(b: &LayoutBox, out: &mut Vec<Color>) {
            for (_, c) in &b.inline_bgs {
                out.push(*c);
            }
            for c in &b.children {
                collect(c, out);
            }
        }
        let mut bgs = Vec::new();
        collect(&lb, &mut bgs);
        // UA mark { background-color: #ffff00 } → 노랑 inline 배경
        assert!(
            bgs.iter().any(|c| c.r == 255 && c.g == 255 && c.b == 0),
            "mark 노랑 배경 (실제 {:?})",
            bgs
        );
    }

    #[test]
    fn progress_and_meter_render_as_gauges() {
        let fs = fonts();
        let ss = crate::css::user_agent_stylesheet();
        let root = crate::html::parse_dom(
            "<progress value=\"70\" max=\"100\"></progress><meter value=\"30\" min=\"0\" max=\"100\"></meter>"
                .to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut vp: Dimensions = Default::default();
        vp.content.width = 400.0;
        let lb = layout_tree(&styled, vp, &fs, &no_images());
        fn collect(b: &LayoutBox, out: &mut Vec<crate::layout::FormControl>) {
            if let Some(fc) = b.form_control {
                out.push(fc);
            }
            for c in &b.children {
                collect(c, out);
            }
        }
        let mut fcs = Vec::new();
        collect(&lb, &mut fcs);
        let has = |frac: f32, meter: bool| {
            fcs.iter().any(|fc| matches!(fc,
                crate::layout::FormControl::Gauge { frac: f, meter: m }
                if (*f - frac).abs() < 0.01 && *m == meter))
        };
        assert!(has(0.7, false), "progress 70/100=0.7");
        assert!(has(0.3, true), "meter 30/100=0.3");
    }

    #[test]
    fn password_input_is_masked() {
        let fs = fonts();
        let root =
            crate::html::parse_dom("<input type=\"password\" value=\"secret\">".to_string());
        let ss = crate::css::user_agent_stylesheet();
        let styled = crate::style::style_tree(&root, &ss);
        let mut vp: Dimensions = Default::default();
        vp.content.width = 300.0;
        let lb = layout_tree(&styled, vp, &fs, &no_images());
        // 6글자 → 6개 • 글리프, 원문 문자 글리프는 없음
        let g = glyphs_of(&lb);
        assert_eq!(g.len(), 6, "6글자 마스킹");
        let bullet = fs.glyph_for('\u{2022}').1;
        assert!(g.iter().all(|gi| gi.glyph_id == bullet), "모두 • 글리프여야");
    }

    #[test]
    fn text_align_center_offsets_inline_line() {
        // 가운데 정렬 문단: 글리프가 왼쪽 밖으로 밀려 시작 (content_x 보다 큼)
        let root = crate::html::parse_dom("<p>hi</p>".to_string());
        let ss = crate::css::parse(
            "p { display: block; font-size: 20px; text-align: center; }".to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = 400.0;
        let fs = fonts();
        let lb = layout_tree(&styled, viewport, &fs, &no_images());
        let first_x = glyphs_of(&lb).first().unwrap().x;
        assert!(first_x > 100.0, "가운데 정렬로 글리프가 오른쪽으로 밀림, got {}", first_x);
    }

    #[test]
    fn rtl_block_right_aligns_text() {
        // dir="rtl" 블록은 text-align 미지정 시 오른쪽 정렬(start=right)
        let root = crate::html::parse_dom("<p dir=\"rtl\">hi</p>".to_string());
        let ss = crate::css::parse("p { display: block; font-size: 20px; width: 400px; }".to_string());
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = 400.0;
        let fs = fonts();
        let lb = layout_tree(&styled, viewport, &fs, &no_images());
        let first_x = glyphs_of(&lb).first().unwrap().x;
        assert!(first_x > 300.0, "rtl 블록은 오른쪽 정렬, first_x={}", first_x);
    }

    #[test]
    fn center_element_centers_narrow_block_child() {
        // <center> 안의 고정폭 블록이 가로 중앙으로 이동
        let root = crate::html::parse_dom(
            "<center><div class=\"box\"></div></center>".to_string(),
        );
        let ss = crate::css::parse(
            "center { display: block; text-align: center; } \
             .box { display: block; width: 100px; height: 10px; }"
                .to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = 500.0;
        let fs = fonts();
        let lb = layout_tree(&styled, viewport, &fs, &no_images());
        // center > div(box). box 의 x 는 (500-100)/2 = 200 근처
        let box_x = lb.children[0].dimensions.content.x;
        assert!((box_x - 200.0).abs() < 1.0, "블록이 중앙 정렬, got x={}", box_x);
    }

    #[test]
    fn flex_row_places_fixed_children_side_by_side() {
        let d = flex_layout(
            "<div class=\"row\"><div class=\"a\"></div><div class=\"b\"></div></div>",
            ".row { display: flex; } \
             .a { display: block; width: 50px; height: 30px; } \
             .b { display: block; width: 70px; height: 20px; }",
            300.0,
        );
        assert_eq!(d[0].content.x, 0.0);
        assert_eq!(d[0].content.width, 50.0);
        assert_eq!(d[1].content.x, 50.0, "두 번째 아이템은 첫 아이템 오른쪽");
        assert_eq!(d[0].content.y, d[1].content.y, "같은 행 = 같은 y");
    }

    #[test]
    fn calc_resolves_width() {
        // calc(100% - 40px) of 400 = 360
        let d = layout_for(
            "<div class=\"b\"></div>",
            ".b { display: block; width: calc(100% - 40px); height: 10px; }",
            400.0,
        );
        assert_eq!(d.content.width, 360.0, "calc(100% - 40px)");
        // 순수 px calc
        let d2 = layout_for(
            "<div class=\"b\"></div>",
            ".b { display: block; width: calc(50px + 10px); height: 10px; }",
            400.0,
        );
        assert_eq!(d2.content.width, 60.0, "calc(50px + 10px)");
        // 곱셈
        let d3 = layout_for(
            "<div class=\"b\"></div>",
            ".b { display: block; width: calc(25% * 2); height: 10px; }",
            400.0,
        );
        assert_eq!(d3.content.width, 200.0, "calc(25% * 2) = 50% = 200");
    }

    #[test]
    fn calc_resolves_context_units() {
        // calc(100% - 2rem): 루트 font-size 기본 16 → 2rem=32 → 400-32 = 368.
        // 예전엔 rem 을 만나면 선언 전체가 드롭돼 width:auto(=400)가 됐다.
        let d = layout_for(
            "<div class=\"b\"></div>",
            ".b { display: block; width: calc(100% - 2rem); height: 10px; }",
            400.0,
        );
        assert_eq!(d.content.width, 368.0, "calc(100% - 2rem), rem=16");
        // calc(1em + 10px): 요소 font-size 30 → 1em=30 → 40.
        let d2 = layout_for(
            "<div class=\"b\"></div>",
            ".b { display: block; font-size: 30px; width: calc(1em + 10px); height: 10px; }",
            400.0,
        );
        assert_eq!(d2.content.width, 40.0, "calc(1em + 10px), em=30");
    }

    #[test]
    fn percentage_width_resolves_against_container() {
        let root = crate::html::parse_dom("<div class=\"half\"></div>".to_string());
        let ss = crate::css::parse(
            ".half { display: block; width: 50%; height: 10px; }".to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = 400.0;
        let fs = fonts();
        let lb = layout_tree(&styled, viewport, &fs, &no_images());
        assert_eq!(lb.dimensions.content.width, 200.0, "50% of 400 = 200");
    }

    #[test]
    fn float_left_packs_side_by_side() {
        let root = crate::html::parse_dom(
            "<div class=\"wrap\"><div class=\"a\"></div><div class=\"b\"></div></div>".to_string(),
        );
        let ss = crate::css::parse(
            ".wrap { display: block; } \
             .a { display: block; float: left; width: 100px; height: 30px; } \
             .b { display: block; float: left; width: 80px; height: 20px; }"
                .to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = 400.0;
        let fs = fonts();
        let lb = layout_tree(&styled, viewport, &fs, &no_images());
        let a = &lb.children[0];
        let b = &lb.children[1];
        assert_eq!(a.dimensions.content.x, 0.0, "첫 left float 은 왼쪽");
        assert_eq!(b.dimensions.content.x, 100.0, "둘째 left float 은 첫 것 오른쪽");
        assert_eq!(a.dimensions.content.y, b.dimensions.content.y, "같은 밴드 = 같은 y");
    }

    #[test]
    fn float_right_anchors_right_edge() {
        let root = crate::html::parse_dom(
            "<div class=\"wrap\"><div class=\"r\"></div></div>".to_string(),
        );
        let ss = crate::css::parse(
            ".wrap { display: block; } \
             .r { display: block; float: right; width: 100px; height: 20px; }"
                .to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = 400.0;
        let fs = fonts();
        let lb = layout_tree(&styled, viewport, &fs, &no_images());
        let r = &lb.children[0];
        assert_eq!(r.dimensions.content.x, 300.0, "float:right → 오른쪽 정렬 (400-100)");
    }

    #[test]
    fn inline_block_flows_horizontally() {
        let root = crate::html::parse_dom(
            "<div class=\"wrap\"><div class=\"a\"></div><div class=\"b\"></div></div>".to_string(),
        );
        let ss = crate::css::parse(
            ".wrap { display: block; } \
             .a { display: inline-block; width: 100px; height: 20px; } \
             .b { display: inline-block; width: 80px; height: 30px; }"
                .to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = 400.0;
        let fs = fonts();
        let lb = layout_tree(&styled, viewport, &fs, &no_images());
        let a = &lb.children[0];
        let b = &lb.children[1];
        assert_eq!(a.dimensions.content.x, 0.0, "첫 inline-block 은 왼쪽");
        assert_eq!(b.dimensions.content.x, 100.0, "둘째는 첫 것 오른쪽에 나란히");
        assert_eq!(a.dimensions.content.y, b.dimensions.content.y, "같은 줄 = 같은 y");
        // 줄 높이 = 최고 아이템(30) → 컨테이너 높이 30
        assert_eq!(lb.dimensions.content.height, 30.0);
    }

    #[test]
    fn inline_block_wraps_when_exceeding_width() {
        let root = crate::html::parse_dom(
            "<div class=\"wrap\"><div class=\"i\"></div><div class=\"i\"></div><div class=\"i\"></div></div>"
                .to_string(),
        );
        let ss = crate::css::parse(
            ".wrap { display: block; } \
             .i { display: inline-block; width: 150px; height: 20px; }"
                .to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = 400.0;
        let fs = fonts();
        let lb = layout_tree(&styled, viewport, &fs, &no_images());
        // 150*3 = 450 > 400 → 셋째는 다음 줄
        assert_eq!(lb.children[0].dimensions.content.x, 0.0);
        assert_eq!(lb.children[1].dimensions.content.x, 150.0);
        assert_eq!(lb.children[2].dimensions.content.x, 0.0, "셋째는 줄바꿈으로 왼쪽");
        assert_eq!(lb.children[2].dimensions.content.y, 20.0, "셋째는 둘째 줄(y=20)");
    }

    #[test]
    fn inline_block_shrinks_to_nested_auto_block() {
        // 구글 버튼 구조: inline-block 안에 auto 폭 블록, 그 안에 고정 폭 리프.
        // inline-block 은 avail 을 채우지 않고 내부 리프 폭으로 줄어들어 나란히 놓여야 함.
        let root = crate::html::parse_dom(
            "<div class=\"wrap\">\
             <div class=\"ib\"><div class=\"inner\"><div class=\"leaf\"></div></div></div>\
             <div class=\"ib\"><div class=\"inner\"><div class=\"leaf\"></div></div></div>\
             </div>"
                .to_string(),
        );
        let ss = crate::css::parse(
            ".wrap { display: block; } \
             .ib { display: inline-block; } \
             .inner { display: block; } \
             .leaf { display: block; width: 40px; height: 20px; }"
                .to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = 400.0;
        let fs = fonts();
        let lb = layout_tree(&styled, viewport, &fs, &no_images());
        assert_eq!(lb.children[0].dimensions.content.x, 0.0);
        assert_eq!(lb.children[1].dimensions.content.x, 40.0, "둘째 버튼은 첫째 오른쪽 (avail 안 채움)");
        assert_eq!(
            lb.children[0].dimensions.content.y, lb.children[1].dimensions.content.y,
            "나란히 = 같은 y"
        );
    }

    #[test]
    fn inline_block_margin_absorbed_not_stolen_from_content() {
        // margin-right 가 있는 auto 폭 inline-block: margin 이 내부 content 를 깎지 않고
        // (내부 40px 리프 유지), 다음 형제는 리프폭+margin 만큼 뒤에 놓여야 함.
        let root = crate::html::parse_dom(
            "<div class=\"wrap\">\
             <div class=\"ib\"><div class=\"leaf\"></div></div>\
             <div class=\"ib\"><div class=\"leaf\"></div></div>\
             </div>"
                .to_string(),
        );
        let ss = crate::css::parse(
            ".wrap { display: block; } \
             .ib { display: inline-block; margin-right: 10px; } \
             .leaf { display: block; width: 40px; height: 20px; }"
                .to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = 400.0;
        let fs = fonts();
        let lb = layout_tree(&styled, viewport, &fs, &no_images());
        // 첫 리프는 40px 유지 (margin 이 깎지 않음)
        assert_eq!(lb.children[0].children[0].dimensions.content.width, 40.0);
        assert_eq!(lb.children[0].dimensions.content.x, 0.0);
        // 둘째는 40(리프) + 10(margin) = 50
        assert_eq!(lb.children[1].dimensions.content.x, 50.0, "리프폭+margin 뒤에 배치");
    }

    #[test]
    fn inline_block_line_centers_with_text_align() {
        let root = crate::html::parse_dom(
            "<div class=\"wrap\"><div class=\"i\"></div></div>".to_string(),
        );
        let ss = crate::css::parse(
            ".wrap { display: block; text-align: center; } \
             .i { display: inline-block; width: 100px; height: 20px; }"
                .to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = 400.0;
        let fs = fonts();
        let lb = layout_tree(&styled, viewport, &fs, &no_images());
        // (400-100)/2 = 150
        assert_eq!(lb.children[0].dimensions.content.x, 150.0, "inline-block 줄 가운데 정렬");
    }

    #[test]
    fn float_band_clears_following_block() {
        let root = crate::html::parse_dom(
            "<div class=\"wrap\"><div class=\"f\"></div><div class=\"after\"></div></div>"
                .to_string(),
        );
        let ss = crate::css::parse(
            ".wrap { display: block; } \
             .f { display: block; float: left; width: 100px; height: 40px; } \
             .after { display: block; height: 15px; }"
                .to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = 400.0;
        let fs = fonts();
        let lb = layout_tree(&styled, viewport, &fs, &no_images());
        // after 는 float 밴드(높이 40) 아래로 clear
        assert_eq!(lb.children[1].dimensions.content.y, 40.0, "정상 블록은 float 밴드 아래");
    }

    #[test]
    fn text_and_inline_block_share_line() {
        // "Home" 텍스트 + inline-block 버튼 + "tail" 이 같은 줄에 (세로로 안 쌓임).
        // 네비게이션 바/버튼 그룹의 기본 패턴.
        let root = crate::html::parse_dom(
            "<div class=\"nav\">Home <span class=\"btn\"></span> tail</div>".to_string(),
        );
        let ss = crate::css::parse(
            ".nav { display: block; font-size: 16px; } \
             .btn { display: inline-block; width: 30px; height: 16px; }"
                .to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut vp: Dimensions = Default::default();
        vp.content.width = 400.0;
        let fs = fonts();
        let lb = layout_tree(&styled, vp, &fs, &no_images());
        // children: anon("Home "), btn, anon(" tail")
        let btn = &lb.children[1];
        assert!(btn.dimensions.content.x > 20.0, "버튼은 'Home ' 텍스트 뒤: {}", btn.dimensions.content.x);
        assert!(btn.dimensions.content.y < 8.0, "버튼은 첫 줄(텍스트와 같은 줄): {}", btn.dimensions.content.y);
        // 컨테이너 높이 = 한 줄 (~18px). 세로로 쌓였다면 3줄(~54px).
        assert!(lb.dimensions.content.height < 30.0, "한 줄이어야: {}", lb.dimensions.content.height);
    }

    #[test]
    fn text_wraps_around_left_float() {
        // float left(100px, 30px 높이) + 텍스트: 초반 줄은 float 우측(x>=95),
        // float 아래 줄은 전체폭(x<95). 이미지 주위 텍스트 흐름.
        let root = crate::html::parse_dom(
            "<div class=\"wrap\"><div class=\"f\"></div>aaa bbb ccc ddd eee fff ggg hhh iii jjj kkk lll mmm nnn ooo ppp qqq rrr sss ttt uuu vvv www xxx yyy zzz a1 b2 c3 d4 e5 f6 g7 h8 i9</div>"
                .to_string(),
        );
        let ss = crate::css::parse(
            ".wrap { display: block; font-size: 16px; } \
             .f { display: block; float: left; width: 100px; height: 30px; }"
                .to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut vp: Dimensions = Default::default();
        vp.content.width = 400.0;
        let fs = fonts();
        let lb = layout_tree(&styled, vp, &fs, &no_images());
        // wrap children: [.f(float), anon(text)]
        let gs = glyphs_of(&lb.children[1]);
        let first = gs.first().unwrap();
        assert!(first.x >= 95.0, "첫 줄 텍스트는 float 우측에서 시작: {}", first.x);
        let below = gs
            .iter()
            .find(|g| g.baseline_y > 40.0)
            .expect("float(30px) 아래로 흐르는 줄이 있어야");
        assert!(below.x < 95.0, "float 아래 줄은 전체폭(x<95): {}", below.x);
    }

    #[test]
    fn block_paragraph_wraps_around_float() {
        // float:left(100px, 40px 높이) 뒤 문단 블록(<div> 텍스트): 박스는 전체폭이되
        // 줄만 float 을 우회한다. 밴드 아래로 clear 되지 않고 float 옆(같은 y)에 배치.
        // (위키백과 인포박스 옆 본문 흐름의 핵심 패턴 — 예전엔 문단이 float 아래로 밀렸음)
        let words = "aaa bbb ccc ddd eee fff ggg hhh iii jjj kkk lll mmm nnn ooo ppp qqq rrr sss ttt uuu vvv www xxx yyy zzz a1 b2 c3 d4 e5 f6 g7 h8 i9 j0 k1 l2 m3 n4";
        let root = crate::html::parse_dom(format!(
            "<div class=\"wrap\"><div class=\"f\"></div><div class=\"t\">{words}</div></div>"
        ));
        let ss = crate::css::parse(
            ".wrap { display: block; font-size: 16px; } \
             .f { display: block; float: left; width: 100px; height: 40px; } \
             .t { display: block; }"
                .to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut vp: Dimensions = Default::default();
        vp.content.width = 400.0;
        let fs = fonts();
        let lb = layout_tree(&styled, vp, &fs, &no_images());
        // .t 블록은 float 옆(같은 y), 아래로 clear 되지 않음
        let t = &lb.children[1];
        assert!(t.dimensions.content.y < 1.0, "문단 블록은 float 옆(y≈0), 아래로 안 밀림: {}", t.dimensions.content.y);
        // 밴드 안 줄은 float 우측(x>=95)에서 시작, float 아래 줄은 전체폭(x<95)
        let gs = glyphs_of(t);
        let first = gs.first().unwrap();
        assert!(first.x >= 95.0, "첫 줄 텍스트는 float 우측에서 시작: {}", first.x);
        let below = gs
            .iter()
            .find(|g| g.baseline_y > 65.0)
            .expect("float(40px) 아래로 흐르는 줄이 있어야");
        assert!(below.x < 95.0, "float 아래 줄은 전체폭(x<95): {}", below.x);
    }

    #[test]
    fn nested_block_wraps_around_float() {
        // float 뒤 중첩 래퍼(<div><div>텍스트</div></div>): 밴드가 BFC 아닌 블록을
        // 통해 재귀적으로 전파돼 안쪽 텍스트가 float 을 우회한다. (예: float + 감싼 본문)
        let words = "aaa bbb ccc ddd eee fff ggg hhh iii jjj kkk lll mmm nnn ooo ppp qqq rrr sss ttt uuu vvv www xxx yyy zzz a1 b2 c3 d4 e5 f6 g7 h8 i9 j0 k1 l2 m3 n4";
        let root = crate::html::parse_dom(format!(
            "<div class=\"cont\"><div class=\"f\"></div><div class=\"outer\"><div class=\"inner\">{words}</div></div></div>"
        ));
        let ss = crate::css::parse(
            ".cont { display: block; font-size: 16px; } \
             .f { display: block; float: left; width: 100px; height: 40px; } \
             .outer { display: block; } .inner { display: block; }"
                .to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut vp: Dimensions = Default::default();
        vp.content.width = 400.0;
        let fs = fonts();
        let lb = layout_tree(&styled, vp, &fs, &no_images());
        let outer = &lb.children[1];
        assert!(outer.dimensions.content.y < 1.0, "래퍼는 float 옆(y≈0): {}", outer.dimensions.content.y);
        let gs = glyphs_of(outer); // .inner 의 글리프까지 재귀 수집
        let first = gs.first().unwrap();
        assert!(first.x >= 95.0, "중첩 텍스트 첫 줄은 float 우측: {}", first.x);
        let below = gs
            .iter()
            .find(|g| g.baseline_y > 65.0)
            .expect("float 아래로 흐르는 줄이 있어야");
        assert!(below.x < 95.0, "float 아래 줄은 전체폭(x<95): {}", below.x);
    }

    #[test]
    fn inline_elements_get_borders() {
        // 인접한 두 인라인 태그가 각각 별개 테두리를 얻는다 (하나로 병합 안 됨).
        // 태그/뱃지/kbd 등 인라인 요소 border 렌더.
        let root = crate::html::parse_dom(
            "<p>x <span class=\"t\">foo</span> <span class=\"t\">bar</span> y</p>".to_string(),
        );
        let ss = crate::css::parse(
            "p { display: block; font-size: 16px; } .t { border: 1px solid #333333; }".to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut vp: Dimensions = Default::default();
        vp.content.width = 400.0;
        let fs = fonts();
        let lb = layout_tree(&styled, vp, &fs, &no_images());
        assert_eq!(count_inline_borders(&lb), 2, "두 태그 → 별개 테두리 2개 (병합 안 됨)");
    }

    #[test]
    fn clear_both_drops_below_float() {
        // float:left(100x40) 뒤 clear:both 블록: 옆으로 우회하지 않고 float 아래로 내려간다.
        // (clearfix 의 핵심 — clear 없으면 텍스트가 float 옆으로 흐름)
        let root = crate::html::parse_dom(
            "<div class=\"cont\"><div class=\"f\"></div><div class=\"t\">hello world text</div></div>"
                .to_string(),
        );
        let ss = crate::css::parse(
            ".cont { display: block; font-size: 16px; } \
             .f { display: block; float: left; width: 100px; height: 40px; } \
             .t { display: block; clear: both; }"
                .to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut vp: Dimensions = Default::default();
        vp.content.width = 400.0;
        let fs = fonts();
        let lb = layout_tree(&styled, vp, &fs, &no_images());
        let t = &lb.children[1];
        assert!(t.dimensions.content.y >= 40.0 - 0.5, "clear:both 는 float(40px) 아래: {}", t.dimensions.content.y);
        // 첫 글리프가 전체폭(x<95)에서 시작 — float 옆으로 우회하지 않음
        let first = glyphs_of(t).into_iter().next().unwrap();
        assert!(first.x < 95.0, "clear 블록 텍스트는 float 옆 우회 안 함(x<95): {}", first.x);
    }

    #[test]
    fn float_column_main_sits_beside_with_margin() {
        // float 사이드바 + margin 으로 float 을 클리어하는 본문 → 같은 y 에 나란히
        // (clear 되어 아래로 밀리지 않음). 고전 float 다단 레이아웃.
        let root = crate::html::parse_dom(
            "<div class=\"wrap\"><div class=\"side\"></div><div class=\"main\"></div></div>"
                .to_string(),
        );
        let ss = crate::css::parse(
            ".wrap { display: block; } \
             .side { display: block; float: left; width: 100px; height: 40px; } \
             .main { display: block; margin-left: 120px; height: 15px; }"
                .to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut vp: Dimensions = Default::default();
        vp.content.width = 400.0;
        let fs = fonts();
        let lb = layout_tree(&styled, vp, &fs, &no_images());
        let side = &lb.children[0];
        let main = &lb.children[1];
        assert_eq!(side.dimensions.content.y, 0.0, "float 사이드바는 밴드 top");
        assert_eq!(main.dimensions.content.y, 0.0, "본문은 float 옆(같은 y), 아래로 안 밀림");
        assert!(main.dimensions.content.x >= 120.0, "본문은 margin 으로 float 우측: {}", main.dimensions.content.x);
        // wrap 높이 = float(40) 과 본문(15) 중 큰 쪽 = 40
        assert_eq!(lb.dimensions.content.height, 40.0, "컨테이너는 float 밴드 하단까지");
    }

    #[test]
    fn flex_auto_items_share_remaining_space() {
        // flex-grow: 1 인 두 아이템이 남은 공간을 균등 분배 (spec: grow 없으면 안 늘어남)
        let d = flex_layout(
            "<div class=\"row\"><div class=\"i\"></div><div class=\"i\"></div></div>",
            ".row { display: flex; } .i { display: block; height: 10px; flex-grow: 1; }",
            300.0,
        );
        assert_eq!(d[0].content.width, 150.0);
        assert_eq!(d[1].content.width, 150.0);
        assert_eq!(d[1].content.x, 150.0);
    }

    #[test]
    fn flex_one_makes_equal_columns() {
        // flex:1 = 1 1 0% → 내용 폭이 달라도 등폭 (basis 0, grow 가 균등 분배).
        // 이전엔 flex-basis 를 버려 내용폭 기준으로 불균등했음.
        let d = flex_layout(
            "<div class=\"row\"><div class=\"a\">hi</div><div class=\"b\">much longer content text</div></div>",
            ".row { display: flex; font-size: 16px; } .a { flex: 1; } .b { flex: 1; }",
            300.0,
        );
        assert!(
            (d[0].content.width - d[1].content.width).abs() < 1.0,
            "flex:1 등폭이어야: {} vs {}",
            d[0].content.width,
            d[1].content.width
        );
        assert!((d[0].content.width - 150.0).abs() < 1.0, "각 ~150px: {}", d[0].content.width);
    }

    #[test]
    fn flex_gap_and_mixed_widths() {
        let d = flex_layout(
            "<div class=\"row\"><div class=\"f\"></div><div class=\"i\"></div><div class=\"i\"></div></div>",
            ".row { display: flex; gap: 10px; } \
             .f { display: block; width: 80px; height: 10px; } \
             .i { display: block; height: 10px; flex-grow: 1; }",
            300.0,
        );
        // 남은 공간 = 300 - 80 - 20(gap 2개) = 200 → grow 아이템 2개 각 100
        assert_eq!(d[0].content.width, 80.0);
        assert_eq!(d[1].content.width, 100.0);
        assert_eq!(d[1].content.x, 90.0, "80 + gap 10");
        assert_eq!(d[2].content.x, 200.0, "90 + 100 + gap 10");
    }

    #[test]
    fn flex_ignores_whitespace_between_items() {
        // 태그 사이 줄바꿈이 익명 아이템으로 끼어들어 공간을 나누면 안 된다
        let d = flex_layout(
            "<div class=\"row\">\n  <div class=\"i\"></div>\n  <div class=\"i\"></div>\n</div>",
            ".row { display: flex; } .i { display: block; height: 10px; flex-grow: 1; }",
            300.0,
        );
        assert_eq!(d.len(), 2, "공백 텍스트는 아이템이 아님");
        assert_eq!(d[0].content.width, 150.0);
        assert_eq!(d[1].content.width, 150.0);
    }

    #[test]
    fn flex_container_height_is_tallest_item() {
        let root = crate::html::parse_dom(
            "<div class=\"row\"><div class=\"a\"></div><div class=\"b\"></div></div>".to_string(),
        );
        let ss = crate::css::parse(
            ".row { display: flex; } \
             .a { display: block; width: 50px; height: 30px; } \
             .b { display: block; width: 50px; height: 80px; }"
                .to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = 300.0;
        let fs = fonts();
        let lb = layout_tree(&styled, viewport, &fs, &no_images());
        assert_eq!(lb.dimensions.content.height, 80.0);
    }

    #[test]
    fn flex_column_stacks_vertically() {
        let d = flex_layout(
            "<div class=\"col\"><div class=\"a\"></div><div class=\"b\"></div></div>",
            ".col { display: flex; flex-direction: column; } \
             .a { display: block; width: 40px; height: 20px; } \
             .b { display: block; width: 40px; height: 30px; }",
            300.0,
        );
        assert_eq!(d[0].content.y, 0.0);
        assert_eq!(d[1].content.y, 20.0, "column 은 세로로 쌓임");
    }

    #[test]
    fn flex_justify_space_between() {
        let d = flex_layout(
            "<div class=\"row\"><div class=\"a\"></div><div class=\"b\"></div></div>",
            ".row { display: flex; justify-content: space-between; } \
             .a { display: block; width: 50px; height: 10px; } \
             .b { display: block; width: 50px; height: 10px; }",
            300.0,
        );
        assert_eq!(d[0].content.x, 0.0);
        assert_eq!(d[1].content.x, 250.0, "space-between: 마지막은 오른쪽 끝");
    }

    #[test]
    fn flex_shrink_prevents_overflow() {
        // 고정 폭 합(200+200=400) > 컨테이너(300) → 기본 shrink 1 로 반씩 줄여 300 에 맞춤
        let d = flex_layout(
            "<div class=\"row\"><div class=\"a\"></div><div class=\"b\"></div></div>",
            ".row { display: flex; width: 300px; } \
             .a { display: block; width: 200px; height: 10px; } \
             .b { display: block; width: 200px; height: 10px; }",
            300.0,
        );
        // 각 200 에서 (400-300)/2=50 씩 줄어 150
        assert!((d[0].content.width - 150.0).abs() < 1.0, "a 폭 ~150, 실제 {}", d[0].content.width);
        assert!((d[1].content.width - 150.0).abs() < 1.0, "b 폭 ~150, 실제 {}", d[1].content.width);
        // b 는 a 바로 오른쪽(150) — 넘치지 않음
        assert!((d[1].content.x - 150.0).abs() < 1.0, "b x ~150");
    }

    #[test]
    fn flex_order_reorders_items() {
        // order 로 시각 순서 재정렬: b(order 1) 가 a(order 2) 보다 앞
        let d = flex_layout(
            "<div class=\"row\"><div class=\"a\"></div><div class=\"b\"></div></div>",
            ".row { display: flex; } \
             .a { display: block; width: 20px; height: 10px; order: 2; } \
             .b { display: block; width: 20px; height: 10px; order: 1; }",
            300.0,
        );
        // b(order1)가 x=0, a(order2)가 x=20
        assert_eq!(d[1].content.x, 0.0, "b(order 1)가 먼저");
        assert_eq!(d[0].content.x, 20.0, "a(order 2)가 나중");
    }

    #[test]
    fn flex_align_self_overrides_container() {
        // 컨테이너 align-items: flex-start, 둘째 아이템만 align-self: center
        let d = flex_layout(
            "<div class=\"row\"><div class=\"tall\"></div><div class=\"s\"></div></div>",
            ".row { display: flex; align-items: flex-start; } \
             .tall { display: block; width: 20px; height: 40px; } \
             .s { display: block; width: 20px; height: 10px; align-self: center; }",
            300.0,
        );
        assert_eq!(d[0].content.y, 0.0, "tall 은 flex-start");
        assert_eq!(d[1].content.y, 15.0, "s 는 align-self center → (40-10)/2");
    }

    #[test]
    fn flex_shrink_zero_keeps_size() {
        // flex-shrink: 0 → 줄지 않고 넘침 허용
        let d = flex_layout(
            "<div class=\"row\"><div class=\"a\"></div><div class=\"b\"></div></div>",
            ".row { display: flex; width: 300px; } \
             .a { display: block; width: 200px; height: 10px; flex-shrink: 0; } \
             .b { display: block; width: 200px; height: 10px; flex-shrink: 0; }",
            300.0,
        );
        assert!((d[0].content.width - 200.0).abs() < 1.0, "shrink:0 은 200 유지");
    }

    #[test]
    fn flex_align_items_center_row() {
        let d = flex_layout(
            "<div class=\"row\"><div class=\"tall\"></div><div class=\"short\"></div></div>",
            ".row { display: flex; align-items: center; } \
             .tall { display: block; width: 20px; height: 40px; } \
             .short { display: block; width: 20px; height: 10px; }",
            300.0,
        );
        // 줄 cross = 40. short(10)는 (40-10)/2 = 15 만큼 내려 중앙정렬
        assert_eq!(d[0].content.y, 0.0);
        assert_eq!(d[1].content.y, 15.0, "align-items center 세로 중앙");
    }

    #[test]
    fn flex_wrap_moves_overflow_to_next_line() {
        let d = flex_layout(
            "<div class=\"row\"><div class=\"i\"></div><div class=\"i\"></div><div class=\"i\"></div></div>",
            ".row { display: flex; flex-wrap: wrap; } \
             .i { display: block; width: 150px; height: 20px; }",
            400.0,
        );
        // 150*3 = 450 > 400 → 셋째는 다음 줄
        assert_eq!(d[0].content.x, 0.0);
        assert_eq!(d[1].content.x, 150.0);
        assert_eq!(d[2].content.x, 0.0, "셋째는 줄바꿈");
        assert_eq!(d[2].content.y, 20.0, "둘째 줄(y=20)");
    }

    #[test]
    fn cjk_text_breaks_between_characters() {
        // CJK 는 공백이 없다. 공백에서만 끊으면 문단 전체가 한 줄로 끝없이 흘러 넘친다
        // (실제로 그랬다 — 112자가 300px 상자에서 한 줄이었다).
        // UAX #14: 표의문자/가나 사이는 줄바꿈 기회다.
        let jp = "日本語のテキストは空白がないので行分割の規則が違います。".repeat(4);
        let d = flex_layout(
            &format!("<div class=\"p\">{}</div>", jp),
            ".p { display: block; width: 300px; font-size: 16px; }",
            300.0,
        );
        let h = d[0].content.height;
        assert!(h > 60.0, "여러 줄로 나뉘어야 한다 (높이 {}px)", h);
        assert!(h < 300.0, "한 글자씩 세로로 흐르면 안 된다 (높이 {}px)", h);
    }

    #[test]
    fn display_contents_removes_box_and_lifts_children() {
        // display: contents — 래퍼의 박스는 생기지 않고 자식이 부모 flex 의 아이템이 된다.
        // 예전엔 미지원 값이라 block 으로 떨어져 래퍼가 통짜 한 아이템이 됐다(자식은 세로 쌓임).
        let d = flex_layout(
            "<div class=\"row\"><div class=\"w\"><div class=\"i\"></div><div class=\"i\"></div></div><div class=\"i\"></div></div>",
            ".row { display: flex; } .w { display: contents; } \
             .i { display: block; width: 100px; height: 20px; }",
            400.0,
        );
        assert_eq!(d.len(), 3, "래퍼 박스는 없고 아이템 3개");
        assert_eq!(d[0].content.x, 0.0);
        assert_eq!(d[1].content.x, 100.0, "래퍼 자식도 같은 flex 행");
        assert_eq!(d[2].content.x, 200.0);
        assert!(d.iter().all(|b| b.content.y == 0.0), "셋 다 한 행");
    }

    #[test]
    fn grid_repeat_three_columns_wraps() {
        let d = flex_layout(
            "<div class=\"g\"><div class=\"i\"></div><div class=\"i\"></div><div class=\"i\"></div><div class=\"i\"></div></div>",
            ".g { display: grid; grid-template-columns: repeat(3, 1fr); } \
             .i { display: block; height: 20px; }",
            300.0,
        );
        assert_eq!(d[0].content.x, 0.0);
        assert_eq!(d[0].content.width, 100.0, "3등분 → 100");
        assert_eq!(d[1].content.x, 100.0);
        assert_eq!(d[2].content.x, 200.0);
        assert_eq!(d[3].content.x, 0.0, "4번째는 다음 행 첫 열");
        assert_eq!(d[3].content.y, 20.0, "다음 행 y");
    }

    #[test]
    fn grid_fixed_plus_fr_columns() {
        let d = flex_layout(
            "<div class=\"g\"><div class=\"i\"></div><div class=\"i\"></div></div>",
            ".g { display: grid; grid-template-columns: 200px 1fr; } \
             .i { display: block; height: 10px; }",
            500.0,
        );
        assert_eq!(d[0].content.width, 200.0, "고정 200");
        assert_eq!(d[1].content.x, 200.0);
        assert_eq!(d[1].content.width, 300.0, "1fr = 남은 300");
    }

    #[test]
    fn line_box_grows_for_larger_inline() {
        // 더 큰 font-size 인라인이 오면 줄 상자가 그만큼 커진다(겹침 방지). 균일 문단은 불변.
        let fs = fonts();
        let mk = |html: &str, css: &str| -> f32 {
            let root = crate::html::parse_dom(html.to_string());
            let ss = crate::css::parse(css.to_string());
            let styled = crate::style::style_tree(&root, &ss);
            let mut vp: Dimensions = Default::default();
            vp.content.width = 600.0;
            layout_tree(&styled, vp, &fs, &no_images()).dimensions.content.height
        };
        let uniform = mk("<p>hello world text</p>", "p { display: block; font-size: 16px; }");
        let mixed = mk(
            "<p>hi <span class=\"b\">BIG</span> yo</p>",
            "p { display: block; font-size: 16px; } .b { font-size: 48px; }",
        );
        assert!(uniform < 25.0, "균일 16px 한 줄: {}", uniform);
        assert!(mixed > 40.0, "48px span 있으면 줄 높이 커짐: {}", mixed);
    }

    #[test]
    fn grid_auto_track_sizes_to_content() {
        // grid-template-columns: auto 1fr (라벨+필드) → auto 는 내용폭, 1fr 이 나머지.
        // 이전엔 auto=1fr 근사라 반반으로 잘렸다.
        let d = flex_layout(
            "<div class=\"g\"><div class=\"a\">Hi</div><div class=\"b\"></div></div>",
            ".g { display: grid; grid-template-columns: auto 1fr; font-size: 16px; } \
             .a { display: block; } .b { display: block; height: 10px; }",
            400.0,
        );
        assert!(d[0].content.width < 100.0, "auto 는 내용폭(작음): {}", d[0].content.width);
        assert!(d[1].content.width > 250.0, "1fr 은 나머지 대부분: {}", d[1].content.width);
        assert!(
            (d[0].content.width + d[1].content.width - 400.0).abs() < 1.0,
            "합 = 400: {} + {}",
            d[0].content.width,
            d[1].content.width
        );
    }

    #[test]
    fn grid_template_areas_holy_grail() {
        // 명시 배치: header/footer 는 두 열 span, nav/main 은 나란히 (holy-grail)
        let d = flex_layout(
            "<div class=\"page\"><div class=\"hd\"></div><div class=\"nv\"></div><div class=\"mn\"></div><div class=\"ft\"></div></div>",
            ".page { display: grid; grid-template-columns: 150px 1fr; \
             grid-template-areas: \"header header\" \"nav main\" \"footer footer\"; } \
             .hd { display: block; grid-area: header; height: 30px; } \
             .nv { display: block; grid-area: nav; height: 80px; } \
             .mn { display: block; grid-area: main; height: 80px; } \
             .ft { display: block; grid-area: footer; height: 25px; }",
            400.0,
        );
        // header: 두 열 span, 맨 위
        assert_eq!(d[0].content.x, 0.0);
        assert_eq!(d[0].content.width, 400.0, "header 는 두 열 span");
        assert_eq!(d[0].content.y, 0.0);
        // nav(좌 150) / main(우 250) 나란히, header 아래
        assert_eq!(d[1].content.x, 0.0);
        assert_eq!(d[1].content.width, 150.0);
        assert_eq!(d[2].content.x, 150.0, "main 은 오른쪽 열");
        assert_eq!(d[2].content.width, 250.0, "1fr = 250");
        assert_eq!(d[1].content.y, d[2].content.y, "nav/main 같은 행");
        assert_eq!(d[1].content.y, 30.0, "header 아래");
        // footer: 두 열 span, nav/main 아래
        assert_eq!(d[3].content.width, 400.0, "footer 는 두 열 span");
        assert_eq!(d[3].content.y, 110.0, "nav/main(30+80) 아래");
    }

    #[test]
    fn grid_gap_between_columns() {
        let d = flex_layout(
            "<div class=\"g\"><div class=\"i\"></div><div class=\"i\"></div></div>",
            ".g { display: grid; grid-template-columns: repeat(2, 1fr); gap: 20px; } \
             .i { display: block; height: 10px; }",
            220.0,
        );
        // (220 - 20 gap) / 2 = 100 each; 둘째는 100 + 20 = 120
        assert_eq!(d[0].content.width, 100.0);
        assert_eq!(d[1].content.x, 120.0, "열 사이 gap 20");
    }

    fn ul_markers(css: &str) -> Vec<Option<String>> {
        let root = crate::html::parse_dom("<ul><li></li><li></li><li></li></ul>".to_string());
        // 실제 렌더처럼 UA 스타일시트(li→block 등) 위에 테스트 CSS 를 얹는다
        let mut ss = crate::css::user_agent_stylesheet();
        ss.rules.extend(crate::css::parse(css.to_string()).rules);
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = 300.0;
        let fs = fonts();
        let lb = layout_tree(&styled, viewport, &fs, &no_images());
        lb.children.iter().map(|c| c.list_marker.clone()).collect()
    }

    #[test]
    fn list_style_none_draws_no_marker_glyph() {
        // list-style:none 이면 마커 글리프가 실제로 안 그려져야 (add_list_marker 가 None→불릿 하지 않음)
        let count = |css: &str| {
            let root = crate::html::parse_dom("<ul class=\"l\"><li>x</li></ul>".to_string());
            let mut ss = crate::css::user_agent_stylesheet();
            ss.rules.extend(crate::css::parse(css.to_string()).rules);
            let styled = crate::style::style_tree(&root, &ss);
            let mut vp: Dimensions = Default::default();
            vp.content.width = 400.0;
            let fs = fonts();
            let lb = layout_tree(&styled, vp, &fs, &no_images());
            glyphs_of(&lb).len()
        };
        let bulleted = count(".l { list-style: disc; }"); // 불릿 + x
        let none = count(".l { list-style: none; }"); // x 만
        assert!(none < bulleted, "none 은 마커 글리프 없음 none={} bulleted={}", none, bulleted);
    }

    #[test]
    fn list_markers_default_and_types() {
        // 기본 ul → disc(•)
        assert_eq!(ul_markers("")[0].as_deref(), Some("\u{2022}"));
        // square 지정
        assert_eq!(ul_markers("li { list-style-type: square; }")[0].as_deref(), Some("\u{25AA}"));
        // none → 마커 없음
        assert_eq!(ul_markers("li { list-style-type: none; }")[0], None);
        // decimal → "1." "2." "3."
        let dec = ul_markers("li { list-style-type: decimal; }");
        assert_eq!(dec[0].as_deref(), Some("1."));
        assert_eq!(dec[2].as_deref(), Some("3."));
        // lower-alpha → a. b. c.
        let al = ul_markers("li { list-style-type: lower-alpha; }");
        assert_eq!(al[0].as_deref(), Some("a."));
        assert_eq!(al[2].as_deref(), Some("c."));
    }

    #[test]
    fn box_sizing_border_box_subtracts_padding_border() {
        let mk = |css: &str| -> (f32, f32) {
            let root = crate::html::parse_dom("<div class=\"b\"></div>".to_string());
            let ss = crate::css::parse(css.to_string());
            let styled = crate::style::style_tree(&root, &ss);
            let mut vp: Dimensions = Default::default();
            vp.content.width = 500.0;
            let fs = fonts();
            let lb = layout_tree(&styled, vp, &fs, &no_images());
            (lb.dimensions.content.width, lb.dimensions.border_box().width)
        };
        // content-box(기본): content=100, border box=100+패딩20+테두리10=130
        let (cw, bw) = mk(".b { display: block; width: 100px; padding: 10px; border: 5px solid #000; }");
        assert_eq!(cw, 100.0);
        assert_eq!(bw, 130.0);
        // border-box: 지정 100 이 border box → content = 100-20-10 = 70
        let (cw2, bw2) =
            mk(".b { display: block; box-sizing: border-box; width: 100px; padding: 10px; border: 5px solid #000; }");
        assert_eq!(bw2, 100.0, "border-box: 지정 width=border box");
        assert_eq!(cw2, 70.0, "content = 100 - 패딩 - 테두리");
    }

    #[test]
    fn sticky_wraps_items_with_offset() {
        let root = crate::html::parse_dom(
            "<div class=\"wrap\"><div class=\"h\">x</div></div>".to_string(),
        );
        let ss = crate::css::parse(
            ".wrap { display: block; } \
             .h { display: block; position: sticky; top: 5px; background-color: #0000ff; height: 20px; }"
                .to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut vp: Dimensions = Default::default();
        vp.content.width = 200.0;
        let fs = fonts();
        let lb = layout_tree(&styled, vp, &fs, &no_images());
        let dl = crate::paint::build_display_list(&lb);
        let sticky_top = dl.iter().find_map(|it| match it {
            crate::paint::DisplayItem::Sticky { top, .. } => Some(*top),
            _ => None,
        });
        assert_eq!(sticky_top, Some(5.0), "position:sticky 가 top=5 Sticky 아이템 생성");
    }

    #[test]
    fn overflow_hidden_clips_child() {
        // overflow:hidden 부모(100px) 안의 넓은 자식(300px) 배경이 부모로 클리핑됨
        let root = crate::html::parse_dom(
            "<div class=\"clip\"><div class=\"big\"></div></div>".to_string(),
        );
        let ss = crate::css::parse(
            ".clip { display: block; width: 100px; height: 50px; overflow: hidden; } \
             .big { display: block; width: 300px; height: 50px; background-color: #ff0000; }"
                .to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut vp: Dimensions = Default::default();
        vp.content.width = 400.0;
        let fs = fonts();
        let lb = layout_tree(&styled, vp, &fs, &no_images());
        let dl = crate::paint::build_display_list(&lb);
        let red = dl.iter().find_map(|it| match it {
            crate::paint::DisplayItem::Rect { color, rect } if color.r == 255 && color.b == 0 => Some(*rect),
            _ => None,
        });
        assert!(red.is_some(), "빨강 배경 사각형이 있어야");
        assert!(red.unwrap().width <= 100.5, "300px 자식이 100px 부모로 클리핑: {}", red.unwrap().width);
    }

    #[test]
    fn z_index_paints_higher_on_top() {
        // 문서상 먼저인 A(z:2)가 나중인 B(z:1)보다 디스플레이 리스트 뒤(=위)에 와야 함
        let root = crate::html::parse_dom(
            "<div class=\"w\"><div class=\"a\"></div><div class=\"b\"></div></div>".to_string(),
        );
        let ss = crate::css::parse(
            ".w { display: block; position: relative; } \
             .a { display: block; position: absolute; z-index: 2; background-color: #ff0000; width: 10px; height: 10px; } \
             .b { display: block; position: absolute; z-index: 1; background-color: #0000ff; width: 10px; height: 10px; }"
                .to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut vp: Dimensions = Default::default();
        vp.content.width = 200.0;
        let fs = fonts();
        let lb = layout_tree(&styled, vp, &fs, &no_images());
        let dl = crate::paint::build_display_list(&lb);
        let red = dl.iter().position(|it| {
            matches!(it, crate::paint::DisplayItem::Rect { color, .. } if color.r == 255 && color.b == 0)
        });
        let blue = dl.iter().position(|it| {
            matches!(it, crate::paint::DisplayItem::Rect { color, .. } if color.b == 255 && color.r == 0)
        });
        assert!(red.is_some() && blue.is_some(), "두 배경 사각형이 있어야");
        assert!(red > blue, "z-index:2(빨강)가 z-index:1(파랑)보다 나중에(위에) 그려짐");
    }

    #[test]
    fn white_space_nowrap_stays_one_line() {
        let long = "<p>aaaa bbbb cccc dddd eeee ffff gggg hhhh iiii</p>";
        let height = |css: &str| -> f32 {
            let root = crate::html::parse_dom(long.to_string());
            let mut ss = crate::css::user_agent_stylesheet();
            ss.rules.extend(crate::css::parse(css.to_string()).rules);
            let styled = crate::style::style_tree(&root, &ss);
            let mut vp: Dimensions = Default::default();
            vp.content.width = 80.0;
            let fs = fonts();
            layout_tree(&styled, vp, &fs, &no_images()).dimensions.content.height
        };
        let normal = height("");
        let nowrap = height("p { white-space: nowrap; }");
        assert!(nowrap < normal, "nowrap 은 한 줄 → normal(여러 줄)보다 낮음: {} < {}", nowrap, normal);
    }

    #[test]
    fn table_rowspan_spans_rows() {
        // 1열 첫 셀 rowspan=2. 둘째 행은 그 열을 건너뛰고 한 셀만.
        let root = crate::html::parse_dom(
            "<table><tbody>\
             <tr><td rowspan=\"2\">L</td><td>a</td></tr>\
             <tr><td>b</td></tr>\
             </tbody></table>"
                .to_string(),
        );
        let mut ss = crate::css::user_agent_stylesheet();
        ss.rules.extend(crate::css::parse("table { width: 200px; } td { padding: 0; }".to_string()).rules);
        let styled = crate::style::style_tree(&root, &ss);
        let mut vp: Dimensions = Default::default();
        vp.content.width = 400.0;
        let fs = fonts();
        let lb = layout_tree(&styled, vp, &fs, &no_images());
        let tbody = &lb.children[0];
        let l = &tbody.children[0].children[0]; // rowspan=2 셀
        let a = &tbody.children[0].children[1]; // 1행 2열
        let b = &tbody.children[1].children[0]; // 2행: 2열에 위치해야 함
        // L 은 두 행 높이만큼 → a 높이보다 큼 (대략 2배)
        assert!(l.dimensions.content.height > a.dimensions.content.height + 1.0,
            "rowspan 셀 높이({})가 단일 행({})보다 커야", l.dimensions.content.height, a.dimensions.content.height);
        // b 는 L 아래가 아니라 둘째 열에 위치 (L 의 x 보다 오른쪽)
        assert!(b.dimensions.content.x > l.dimensions.content.x,
            "둘째 행 셀 b({})가 rowspan 열 L({}) 오른쪽", b.dimensions.content.x, l.dimensions.content.x);
        // b 의 x 는 a 의 x 와 같은 열
        assert!((b.dimensions.content.x - a.dimensions.content.x).abs() < 1.0, "b 와 a 같은 열");
    }

    #[test]
    fn table_colspan_spans_columns() {
        // 첫 행: colspan=2 헤더. 둘째 행: 두 셀. 헤더 폭 = 두 열 합
        let root = crate::html::parse_dom(
            "<table><tbody><tr><td colspan=\"2\">head</td></tr><tr><td>a</td><td>b</td></tr></tbody></table>"
                .to_string(),
        );
        let mut ss = crate::css::user_agent_stylesheet();
        ss.rules.extend(crate::css::parse("table { width: 200px; } td { padding: 0; }".to_string()).rules);
        let styled = crate::style::style_tree(&root, &ss);
        let mut vp: Dimensions = Default::default();
        vp.content.width = 400.0;
        let fs = fonts();
        let lb = layout_tree(&styled, vp, &fs, &no_images());
        let tbody = &lb.children[0];
        let head = &tbody.children[0].children[0]; // colspan=2 셀
        let a = &tbody.children[1].children[0];
        let b = &tbody.children[1].children[1];
        // colspan 셀 폭 ≈ 두 열(a+b) 폭 합
        let sum = a.dimensions.content.width + b.dimensions.content.width;
        assert!((head.dimensions.content.width - sum).abs() < 0.5,
            "colspan=2 폭({}) ≈ 두 열 합({})", head.dimensions.content.width, sum);
        // 둘째 셀 b 는 첫째 셀 a 오른쪽
        assert!(b.dimensions.content.x > a.dimensions.content.x);
    }

    #[test]
    fn table_columns_align_across_rows() {
        // 내용 폭이 다른 셀이 행마다 있어도 열은 공통 폭으로 정렬돼야 함
        let root = crate::html::parse_dom(
            "<table><tbody><tr><td>a</td><td>bbbbbbbbbb</td></tr><tr><td>cccc</td><td>d</td></tr></tbody></table>"
                .to_string(),
        );
        let mut ss = crate::css::user_agent_stylesheet();
        ss.rules.extend(crate::css::parse("table { width: 300px; } td { padding: 0; }".to_string()).rules);
        let styled = crate::style::style_tree(&root, &ss);
        let mut vp: Dimensions = Default::default();
        vp.content.width = 400.0;
        let fs = fonts();
        let lb = layout_tree(&styled, vp, &fs, &no_images());
        let tbody = &lb.children[0];
        let r0 = &tbody.children[0];
        let r1 = &tbody.children[1];
        assert_eq!(r0.children[1].dimensions.content.x, r1.children[1].dimensions.content.x, "2열 x 정렬");
        assert_eq!(
            r0.children[1].dimensions.content.width, r1.children[1].dimensions.content.width,
            "2열 폭 동일"
        );
        assert_eq!(
            r0.children[0].dimensions.content.width, r1.children[0].dimensions.content.width,
            "1열 폭 동일"
        );
    }

    #[test]
    fn table_row_respects_cell_width_attribute() {
        // 구글 검색 테이블 사례: 25% | auto | 25% (HTML width 속성)
        // §13 정식 파서는 bare <tr> 을 무시하므로 <table> 로 감싼다.
        let root = crate::html::parse_dom(
            "<table><tr><td width=\"25%\"></td><td></td><td width=\"25%\"></td></tr></table>"
                .to_string(),
        );
        let ss = crate::css::parse("table, tbody, tr, td { display: block; padding: 0; }".to_string());
        let styled = crate::style::style_tree(&root, &ss);
        let mut vp: Dimensions = Default::default();
        vp.content.width = 400.0;
        let fs = fonts();
        let lb = layout_tree(&styled, vp, &fs, &no_images());
        let tr = &lb.children[0].children[0]; // table > tbody > tr
        let d: Vec<Dimensions> = tr.children.iter().map(|c| c.dimensions).collect();
        assert_eq!(d[0].content.width, 100.0, "25% of 400");
        assert_eq!(d[0].content.x, 0.0);
        assert_eq!(d[1].content.x, 100.0);
        assert_eq!(d[1].content.width, 200.0, "auto 셀 = 남은 200");
        assert_eq!(d[2].content.x, 300.0, "우측 25% 셀");
    }

    #[test]
    fn flex_row_with_inline_children_lays_horizontal() {
        // display:flex 안의 인라인 <a> 3개가 각각 플렉스 아이템으로 가로 배치돼야
        // (익명 인라인 상자 하나로 뭉치면 안 됨 — 가로 내비 무너짐 방지)
        let d = flex_layout(
            "<div class=\"f\"><a>AAA</a><a>BBB</a><a>CCC</a></div>",
            ".f { display: flex; } a { display: inline; }",
            400.0,
        );
        assert_eq!(d.len(), 3, "인라인 자식 3개가 각각 아이템, 실제 {}", d.len());
        assert!(d[1].content.x > d[0].content.x, "둘째가 오른쪽");
        assert!(d[2].content.x > d[1].content.x, "셋째가 더 오른쪽");
        assert_eq!(d[0].content.y, d[1].content.y, "같은 줄");
    }

    #[test]
    fn css_display_table_direct_cells_horizontal() {
        // display:table + display:table-cell (익명 행) → 가로 배치
        let d = flex_layout(
            "<div class=\"t\"><div class=\"c\">a</div><div class=\"c\">b</div><div class=\"c\">d</div></div>",
            ".t { display: table; width: 300px; } .c { display: table-cell; }",
            400.0,
        );
        assert_eq!(d.len(), 3, "셀 3개");
        assert!(d[1].content.x > d[0].content.x, "둘째 셀이 오른쪽");
        assert!(d[2].content.x > d[1].content.x, "셋째 셀이 더 오른쪽");
        assert_eq!(d[0].content.y, d[1].content.y, "같은 행이라 y 동일");
    }

    #[test]
    fn table_cell_vertical_align_middle() {
        // 짧은 셀 내용이 행 높이(60) 안에서 vertical-align:middle 로 내려감
        let root = crate::html::parse_dom(
            "<div class=\"t\"><div class=\"r\"><div class=\"tall\"></div>\
             <div class=\"mid\"><div class=\"inner\">x</div></div></div></div>"
                .to_string(),
        );
        let ss = crate::css::parse(
            ".t{display:table;width:200px} .r{display:table-row} \
             .tall{display:table-cell;width:100px;height:60px} \
             .mid{display:table-cell;width:100px;vertical-align:middle} \
             .inner{display:block;height:10px}"
                .to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut vp: Dimensions = Default::default();
        vp.content.width = 400.0;
        let fs = fonts();
        let lb = layout_tree(&styled, vp, &fs, &no_images());
        let row = &lb.children[0];
        let mid = &row.children[1];
        assert!(
            (mid.dimensions.content.height - 60.0).abs() < 2.0,
            "셀이 행 높이 60 으로 stretch, 실제 {}",
            mid.dimensions.content.height
        );
        let inner = &mid.children[0];
        let rel_y = inner.dimensions.content.y - mid.dimensions.content.y;
        assert!(rel_y > 15.0, "vertical-align:middle 로 내부가 중앙으로, rel_y={}", rel_y);
    }

    #[test]
    fn css_display_table_full_structure() {
        // display:table > table-row > table-cell 완전 구조
        let root = crate::html::parse_dom(
            "<div class=\"t\"><div class=\"r\"><div class=\"c\">a</div><div class=\"c\">b</div></div></div>"
                .to_string(),
        );
        let ss = crate::css::parse(
            ".t { display: table; width: 200px; } .r { display: table-row; } .c { display: table-cell; }"
                .to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut vp: Dimensions = Default::default();
        vp.content.width = 400.0;
        let fs = fonts();
        let lb = layout_tree(&styled, vp, &fs, &no_images());
        let row = &lb.children[0];
        assert!(row.children.len() >= 2, "행에 셀 2개");
        assert!(
            row.children[1].dimensions.content.x > row.children[0].dimensions.content.x,
            "둘째 셀이 오른쪽"
        );
    }

    #[test]
    fn block_inside_inline_is_hoisted() {
        // <span> 안의 블록 <div> 는 인라인 흐름에서 사라지지 않고 블록으로 편입 (구글 푸터 사례)
        let root = crate::html::parse_dom(
            "<div class=\"wrap\"><span id=\"f\"><div class=\"blk\">x</div></span></div>".to_string(),
        );
        let ss = crate::css::parse(
            "div { display: block; } span { display: inline; } .blk { display: block; height: 20px; }"
                .to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut vp: Dimensions = Default::default();
        vp.content.width = 300.0;
        let fs = fonts();
        let lb = layout_tree(&styled, vp, &fs, &no_images());
        // wrap 의 직계 자식에 blk 블록 박스가 있어야 (span 은 투명 처리)
        let has_blk = lb.children.iter().any(|c| {
            matches!(&c.styled_node.node.node_type,
                NodeType::Element(e) if e.classes().contains("blk"))
        });
        assert!(has_blk, "span 안 블록 div 가 블록 흐름으로 편입돼야");
        // 문서 높이가 블록 높이(20px)를 반영해야 (드롭되지 않음)
        assert!(lb.dimensions.content.height >= 20.0, "블록 높이 반영: {}", lb.dimensions.content.height);
    }

    #[test]
    fn link_regions_cover_anchor_words_only() {
        let root = crate::html::parse_dom(
            "<p>plain <a href=\"https://x.com/a\">click here</a> tail</p>".to_string(),
        );
        let ss = crate::css::parse("p { display: block; font-size: 20px; }".to_string());
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = 600.0;
        let fs = fonts();
        let lb = layout_tree(&styled, viewport, &fs, &no_images());
        let mut links = Vec::new();
        collect_link_regions(&lb, &mut links);
        assert_eq!(links.len(), 2, "'click' 'here' 두 단어 = 히트 영역 2개");
        assert!(links.iter().all(|(_, h)| h == "https://x.com/a"));
        // 링크 단어 중심점은 히트, 문서 시작(plain 위치)은 미스
        let (r, _) = &links[0];
        assert!(
            hit_link(&links, r.x + r.width / 2.0, r.y + r.height / 2.0).is_some(),
            "링크 단어 중심은 히트"
        );
        assert!(hit_link(&links, 1.0, r.y + r.height / 2.0).is_none(), "'plain' 쪽은 미스");
    }

    #[test]
    fn background_image_resolves_from_map() {
        let root = crate::html::parse_dom("<div class=\"hero\"></div>".to_string());
        let ss = crate::css::parse(
            ".hero { display: block; height: 40px; background-image: url(bg.jpg); }".to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = 400.0;
        let fs = fonts();
        let mut images = ImageMap::new();
        images.insert("bg.jpg".to_string(), (3, 100, 50));
        let lb = layout_tree(&styled, viewport, &fs, &images);
        assert_eq!(lb.background_image, Some(3));
        // 맵에 없으면 None
        let lb2 = layout_tree(&styled, viewport, &fs, &no_images());
        assert_eq!(lb2.background_image, None);
    }

    #[test]
    fn image_box_uses_intrinsic_size() {
        let root = crate::html::parse_dom("<div><img src=\"a.png\"></div>".to_string());
        let ss = crate::css::parse("div { display: block; } img { display: block; }".to_string());
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = 400.0;
        let fs = fonts();
        let mut images = ImageMap::new();
        images.insert("a.png".to_string(), (0, 32, 24));
        let lb = layout_tree(&styled, viewport, &fs, &images);
        // div 의 자식 = img 박스
        let img = &lb.children[0];
        assert_eq!(img.image, Some(0));
        assert_eq!(img.dimensions.content.width, 32.0);
        assert_eq!(img.dimensions.content.height, 24.0);
    }

    #[test]
    fn image_css_width_preserves_aspect_ratio() {
        // 고유 32x24, CSS width:64px → 높이는 종횡비 유지로 48px
        let root = crate::html::parse_dom("<div><img src=\"a.png\"></div>".to_string());
        let ss = crate::css::parse(
            "div { display: block; } img { display: block; width: 64px; }".to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = 400.0;
        let fs = fonts();
        let mut images = ImageMap::new();
        images.insert("a.png".to_string(), (0, 32, 24));
        let lb = layout_tree(&styled, viewport, &fs, &images);
        let img = &lb.children[0];
        assert_eq!(img.dimensions.content.width, 64.0);
        assert_eq!(img.dimensions.content.height, 48.0, "종횡비 유지 (64 × 24/32)");
    }

    #[test]
    fn image_max_width_percent_scales_down() {
        // 고유 800x400 이미지, 컨테이닝 폭 400, img { max-width: 100% } → 폭 400 으로 축소,
        // height: auto 이므로 종횡비 유지해 200 으로 재계산 (반응형 이미지 핵심 패턴).
        let root = crate::html::parse_dom("<div><img src=\"a.png\"></div>".to_string());
        let ss = crate::css::parse(
            "div { display: block; } img { display: block; max-width: 100%; }".to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = 400.0;
        let fs = fonts();
        let mut images = ImageMap::new();
        images.insert("a.png".to_string(), (0, 800, 400));
        let lb = layout_tree(&styled, viewport, &fs, &images);
        let img = &lb.children[0];
        assert_eq!(img.dimensions.content.width, 400.0, "max-width:100% 로 폭 축소");
        assert_eq!(img.dimensions.content.height, 200.0, "종횡비 유지 (400 × 400/800)");
    }

    #[test]
    fn block_max_width_percent_clamps() {
        // 컨테이닝 폭 400, div { max-width: 50% } → 폭 200 으로 상한 적용 (기존엔 % 무시됨).
        let root = crate::html::parse_dom("<div class=\"c\"></div>".to_string());
        let ss = crate::css::parse(
            "div { display: block; } .c { max-width: 50%; height: 10px; }".to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = 400.0;
        let fs = fonts();
        let lb = layout_tree(&styled, viewport, &fs, &no_images());
        assert_eq!(lb.dimensions.content.width, 200.0, "max-width:50% → 200");
    }

    #[test]
    fn image_html_width_height_attrs() {
        // HTML width/height 속성으로 크기 지정
        let root = crate::html::parse_dom(
            "<div><img src=\"a.png\" width=\"100\" height=\"40\"></div>".to_string(),
        );
        let ss = crate::css::parse("div { display: block; } img { display: block; }".to_string());
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = 400.0;
        let fs = fonts();
        let mut images = ImageMap::new();
        images.insert("a.png".to_string(), (0, 32, 24));
        let lb = layout_tree(&styled, viewport, &fs, &images);
        let img = &lb.children[0];
        assert_eq!(img.dimensions.content.width, 100.0);
        assert_eq!(img.dimensions.content.height, 40.0);
    }
}
