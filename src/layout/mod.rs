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
}

pub struct LayoutBox<'a> {
    pub dimensions: Dimensions,
    pub styled_node: &'a StyledNode<'a>,
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
    // 링크 밑줄/리스트 불릿 등 (사각형, 색)
    pub decorations: Vec<(Rect, Color)>,
    // 리스트 마커 텍스트 (ol: "1." / ul: "•"). build 시 부모 리스트가 부여.
    pub list_marker: Option<String>,
    // 콘텐츠의 실제 사용 폭 (shrink-to-fit float 배치용)
    pub used_width: f32,
    // float 컨텍스트(절대 좌표): (좌 float 우측 x, 우 float 좌측 x, 밴드 하단 y).
    // 텍스트 줄 상자가 이 밴드 안(y < 하단)에서 float 을 피해 짧아진다(text-wrap).
    pub float_ctx: Option<(f32, f32, f32)>,
}

impl<'a> LayoutBox<'a> {
    fn new(styled_node: &'a StyledNode<'a>) -> LayoutBox<'a> {
        LayoutBox {
            dimensions: Default::default(),
            styled_node,
            children: Vec::new(),
            glyphs: Vec::new(),
            inline_nodes: Vec::new(),
            image: None,
            background_image: None,
            gradient: None,
            links: Vec::new(),
            decorations: Vec::new(),
            list_marker: None,
            used_width: 0.0,
            float_ctx: None,
            form_control: None,
        }
    }

    fn new_anonymous(parent: &'a StyledNode<'a>, nodes: Vec<&'a StyledNode<'a>>) -> LayoutBox<'a> {
        LayoutBox {
            dimensions: Default::default(),
            styled_node: parent,
            children: Vec::new(),
            glyphs: Vec::new(),
            inline_nodes: nodes,
            image: None,
            background_image: None,
            gradient: None,
            links: Vec::new(),
            decorations: Vec::new(),
            list_marker: None,
            used_width: 0.0,
            float_ctx: None,
            form_control: None,
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
                        self.dimensions.content.width = w;
                        self.dimensions.content.height = h;
                        self.image = Some(idx);
                        return;
                    }
                }
            }
            if e.tag_name == "input" {
                self.layout_input(containing_block, fonts);
                return;
            }
            if e.tag_name == "select" {
                self.layout_select(containing_block, fonts);
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
            self.layout_children(fonts, images);
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

        // max-width: 계산된 폭이 상한을 넘으면 고정 폭으로 재계산 (auto 마진 → 가운데 정렬)
        if let Some(Length(mw, Px)) = style.value("max-width") {
            let mw = if border_box { (mw - extra).max(0.0) } else { mw };
            if cw > mw {
                let (cw2, ml2, mr2) =
                    resolve_width(&Length(mw, Px), &margin_left, &margin_right, extra, avail);
                cw = cw2;
                ml = ml2;
                mr = mr2;
            }
        }
        // min-width: max-width 보다 우선 (마지막에 적용). 계산 폭이 하한보다 작으면 하한으로.
        if let Some(Length(mw, Px)) = style.value("min-width") {
            let mw = if border_box { (mw - extra).max(0.0) } else { mw };
            if cw < mw {
                let (cw2, ml2, mr2) =
                    resolve_width(&Length(mw, Px), &margin_left, &margin_right, extra, avail);
                cw = cw2;
                ml = ml2;
                mr = mr2;
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

    fn calculate_position(&mut self, containing_block: Dimensions) {
        let style = self.styled_node;
        let zero = Length(0.0, Px);
        let d = &mut self.dimensions;

        d.margin.top = style.lookup("margin-top", "margin", &zero).to_px();
        d.margin.bottom = style.lookup("margin-bottom", "margin", &zero).to_px();
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
            let cpos = child.position();
            if cpos == "absolute" || cpos == "fixed" {
                let cb = if cpos == "fixed" { fixed_cb } else { child_abs_cb };
                let cur = child.dimensions.border_box();
                let has_left = child.styled_node.value("left").is_some();
                let has_right = child.styled_node.value("right").is_some();
                let has_top = child.styled_node.value("top").is_some();
                let has_bottom = child.styled_node.value("bottom").is_some();
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
        self.image = None;
        self.background_image = None;
        self.gradient = None;
        self.dimensions = Default::default();
        self.used_width = 0.0;
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
            _ => "static",
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
        let n = self.children.len();
        for i in 0..n {
            let cpos = self.children[i].position();
            let cfloat = self.children[i].float();
            let is_ib =
                matches!(self.children[i].styled_node.display(), Display::InlineBlock) && cfloat == "none";
            // 익명 인라인 박스(텍스트 런): inline-block 과 같은 줄에 흘러야 한다.
            // 단, 인접한 inline-block 이 있을 때만 atom 으로(홀로 있는 텍스트 블록은 정상 유지).
            let is_anon = !self.children[i].inline_nodes.is_empty() && cfloat == "none";
            let next_is_ib = self
                .children
                .get(i + 1)
                .map(|c| matches!(c.styled_node.display(), Display::InlineBlock) && c.float() == "none")
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
                let mut probe: Dimensions = Default::default();
                probe.content.x = fl_next;
                probe.content.y = band_top;
                probe.content.width = avail_band;
                child.layout(probe, fonts, images);
                // 점유 폭 결정: 명시 width(퍼센트 포함, 이미 px)면 border box, 아니면 shrink-to-fit
                let explicit = matches!(child.styled_node.value("width"), Some(Length(_, _)));
                // auto 폭 shrink-to-fit 시 재배치 폭(ow)에 margin 도 포함해야 재계산에서
                // margin 이 content 를 깎지 않는다 (auto 폭엔 phantom margin 이 없음).
                let bp = child.dimensions.margin_box().width - child.dimensions.content.width;
                let ow = if explicit {
                    child.dimensions.border_box().width.min(avail_band)
                } else {
                    (child.used_width + bp).min(avail_band)
                };
                // 2차 배치: 확정 폭·위치로 재배치 (1차 페인트 상태는 비우고 다시)
                let x = if cfloat == "left" { fl_next } else { fr_next - ow };
                child.clear_render();
                let mut cb: Dimensions = Default::default();
                cb.content.x = x;
                cb.content.y = band_top;
                cb.content.width = ow;
                child.layout(cb, fonts, images);
                if cfloat == "left" {
                    fl_next += ow;
                } else {
                    fr_next -= ow;
                }
                float_extent = float_extent.max((fl_next - cx) + ((cx + avail) - fr_next));
                band_bottom = band_bottom.max(band_top + child.dimensions.margin_box().height);
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

            // 정상 블록. float 밴드가 있을 때: 이 블록이 (margin 등으로) float 을
            // 가로로 클리어하면 밴드 옆(같은 y)에 배치, 아니면 밴드 아래로 clear(겹침 방지).
            // 이것이 float 사이드바 + margin 본문 다단을 살린다.
            if band_active {
                // 익명 텍스트 박스는 float 주위로 흐른다(text-wrap): float_ctx 설정 후
                // 밴드 top 에서 배치. 줄 상자가 float 을 피해 짧아진다.
                if is_anon {
                    self.children[i].float_ctx = Some((fl_next, fr_next, band_bottom));
                    let d0 = self.dimensions;
                    self.children[i].layout(d0, fonts, images);
                    self.children[i].float_ctx = None;
                    let mh = self.children[i].dimensions.margin_box().height;
                    let block_bottom = self.dimensions.content.height + mh;
                    self.dimensions.content.height = block_bottom.max(band_bottom - cy);
                    band_active = false;
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
            // 인접 형제 margin 상쇄: 이 블록 상단 margin 을 직전 블록 하단 margin 과
            // 겹쳐(더하지 않고) 흐름 높이에서 겹침량만큼 줄인 뒤 배치한다.
            let cur_top = {
                let z = Length(0.0, Px);
                len_px(self.children[i].styled_node.lookup("margin-top", "margin", &z), avail).to_px()
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
        }
        // 마지막이 inline-block 런이면 마감
        if ib_active {
            ib_lines.push((std::mem::take(&mut ib_cur), ib_pen_x - cx));
            let w =
                self.finish_inline_block_run(std::mem::take(&mut ib_lines), align, avail, ib_bottom, cy);
            inline_extent = inline_extent.max(w);
        }
        // float 로 끝난 경우 밴드 높이를 컨테이너에 반영
        if band_active {
            let below = band_bottom - cy;
            if below > self.dimensions.content.height {
                self.dimensions.content.height = below;
            }
        }
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

    // <table>: 모든 행의 셀을 모아 공통 열 폭을 계산해 열을 정렬한다.
    // 열 폭 = 지정 폭(있으면) 아니면 내용 기반(max-content) 선호 폭. 남는/부족한
    // 폭은 auto 열에 선호 비율로 분배해 테이블 폭을 채움. 행 높이 = 최고 셀.
    // colspan/rowspan 지원. 미지원: border-collapse, border-spacing.
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
        // 2) 열 폭 확정: 고정 열은 그대로, auto 열은 남은 폭을 선호 비율로 분배(테이블 폭 채움)
        let total_fixed: f32 = col_fixed.iter().flatten().sum();
        let auto_cols: Vec<usize> = (0..ncols).filter(|&c| col_fixed[c].is_none()).collect();
        let auto_pref_sum: f32 = auto_cols.iter().map(|&c| col_pref[c]).sum();
        let remaining = (avail - total_fixed).max(0.0);
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
            let mut x = ox;
            for c in 0..ncols {
                col_x[c] = x;
                x += col_w[c];
            }
        }
        // <caption>: 표 위에 표 폭으로 배치하고, 행 시작 y 를 캡션 높이만큼 내린다.
        let table_w: f32 = col_w.iter().sum();
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
        let mut y = oy + caption_h;
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
            y += row_h;
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
        // max-height: overflow 가 잘리는 경우에만 적용 (미클립 시 내용 겹침 방지)
        if let Some(Length(mxh, Px)) = self.styled_node.value("max-height") {
            let mxh = if border_box { (mxh - vextra).max(0.0) } else { mxh };
            if self.dimensions.content.height > mxh && self.overflow_clips_self() {
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

    // overflow(-x/-y) 가 hidden/clip/scroll/auto 여서 자손이 이 박스로 클리핑되는가.
    fn overflow_clips_self(&self) -> bool {
        for prop in ["overflow", "overflow-x", "overflow-y"] {
            if let Some(Value::Keyword(k)) = self.styled_node.value(prop) {
                if matches!(k.as_str(), "hidden" | "clip" | "scroll" | "auto") {
                    return true;
                }
            }
        }
        false
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
        // calc(pct% + px) → 기준 폭으로 해석
        Value::Calc(pct, px) => Length(pct_base * pct / 100.0 + px, Px),
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
    out.extend(root.links.iter().cloned());
    for child in &root.children {
        collect_link_regions(child, out);
    }
}

// (x, y) 문서 좌표가 가리키는 링크 href
pub fn hit_link<'a>(links: &'a [(Rect, String)], x: f32, y: f32) -> Option<&'a str> {
    links.iter().find(|(r, _)| r.contains(x, y)).map(|(_, h)| h.as_str())
}

// 이벤트 히트 테스트용: 요소 박스의 (border box, NodeId, 깊이) 수집.
// 익명 인라인 박스는 부모 요소의 id 를 공유하므로 텍스트 클릭도 매칭된다.
pub fn collect_element_rects(
    root: &LayoutBox,
    depth: usize,
    out: &mut Vec<(Rect, crate::dom::NodeId, usize)>,
) {
    if matches!(root.styled_node.node.node_type, NodeType::Element(_)) {
        out.push((root.dimensions.border_box(), root.styled_node.id, depth));
    }
    for child in &root.children {
        collect_element_rects(child, depth + 1, out);
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
        Display::Inline => contains_block_level(c),
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
                if contains_block_level(child) {
                    // 투명 래퍼: 인라인 부분은 앞뒤로 나뉘고 블록은 흐름에 편입
                    distribute_children(root, pending, anon_owner, &child.children);
                } else {
                    pending.push(child);
                }
            }
            Display::None => {}
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
        for child in &style_node.children {
            match &child.node.node_type {
                NodeType::Text(t) => {
                    if !t.trim().is_empty() {
                        root.children.push(LayoutBox::new_anonymous(style_node, vec![child]));
                    }
                }
                NodeType::Element(_) => {
                    if !matches!(child.display(), Display::None) {
                        root.children.push(build_layout_tree(child));
                    }
                }
            }
        }
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

// 인접 margin 상쇄로 흐름에서 줄여야 할 겹침량. m1=이전 하단, m2=이번 상단.
// 상쇄 결과 = 양수최대 + 음수최소. 현재는 두 margin 이 더해지므로 (m1+m2)-상쇄 만큼 뺀다.
fn collapse_overlap(m1: f32, m2: f32) -> f32 {
    let pos = m1.max(0.0).max(m2.max(0.0));
    let neg = m1.min(0.0).min(m2.min(0.0));
    (m1 + m2) - (pos + neg)
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
    root_box.layout(containing_block, fonts, images);
    // 절대/고정 위치를 올바른 컨테이닝 블록 기준으로 재배치 (transform 적용 전)
    root_box.reposition_abs(viewport_rect, viewport_rect);
    // 레이아웃 완료 후 CSS transform(translate) 을 시각 오프셋으로 적용 (흐름 불변)
    apply_transforms(&mut root_box);
    root_box
}

// 후위 순회로 transform 의 translate/scale 을 서브트리에 적용한다.
// 흐름/형제 위치에는 영향 없음(레이아웃 후 순수 시각 변환). rotate/matrix 는 미적용.
fn apply_transforms(b: &mut LayoutBox) {
    for c in &mut b.children {
        apply_transforms(c);
    }
    if let Some(Value::Keyword(t)) = b.styled_node.value("transform") {
        apply_transform_functions(b, &t);
    }
}

// transform 함수 목록을 순서대로 적용 (translate*, scale*). 원점은 border-box 중앙.
fn apply_transform_functions(b: &mut LayoutBox, text: &str) {
    let mut rest = text;
    while let Some(open) = rest.find('(') {
        let name = rest[..open]
            .trim()
            .rsplit(|c: char| c.is_whitespace() || c == ')')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let Some(close_rel) = rest[open..].find(')') else { break };
        let close = close_rel + open;
        let args = &rest[open + 1..close];
        let parts: Vec<&str> = args.split(',').map(|s| s.trim()).collect();
        let bb = b.dimensions.border_box();
        let len = |tok: &str, base: f32| -> f32 {
            if let Some(p) = tok.strip_suffix('%') {
                p.trim().parse::<f32>().map(|v| v / 100.0 * base).unwrap_or(0.0)
            } else {
                crate::css::parse_len_px(tok).unwrap_or(0.0)
            }
        };
        let num = |tok: &str| tok.parse::<f32>().unwrap_or(1.0);
        match name.as_str() {
            "translate" => {
                let dx = len(parts[0], bb.width);
                let dy = parts.get(1).map(|p| len(p, bb.height)).unwrap_or(0.0);
                b.translate(dx, dy);
            }
            "translatex" => b.translate_x(len(parts[0], bb.width)),
            "translatey" => b.translate(0.0, len(parts[0], bb.height)),
            "scale" => {
                let sx = num(parts[0]);
                let sy = parts.get(1).map(|p| num(p)).unwrap_or(sx);
                let (ox, oy) = (bb.x + bb.width / 2.0, bb.y + bb.height / 2.0);
                b.scale_subtree(ox, oy, sx, sy);
            }
            "scalex" => {
                let (ox, oy) = (bb.x + bb.width / 2.0, bb.y + bb.height / 2.0);
                b.scale_subtree(ox, oy, num(parts[0]), 1.0);
            }
            "scaley" => {
                let (ox, oy) = (bb.x + bb.width / 2.0, bb.y + bb.height / 2.0);
                b.scale_subtree(ox, oy, 1.0, num(parts[0]));
            }
            _ => {} // rotate/matrix/skew 등 미적용
        }
        rest = &rest[close + 1..];
    }
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

    #[test]
    fn transform_translate_offsets_box() {
        // translate(10px, 20px) → 박스가 그만큼 이동 (흐름 불변, 시각 오프셋)
        let d = layout_for(
            "<div></div>",
            "div { display: block; width: 100px; height: 50px; transform: translate(10px, 20px); }",
            800.0,
        );
        assert_eq!(d.content.x, 10.0, "x 오프셋");
        assert_eq!(d.content.y, 20.0, "y 오프셋");
        // 퍼센트: translateX(50%) = 자기 border-box 폭의 50%
        let d2 = layout_for(
            "<div></div>",
            "div { display: block; width: 100px; height: 50px; transform: translateX(50%); }",
            800.0,
        );
        assert_eq!(d2.content.x, 50.0, "50% × 100px = 50px");
    }

    #[test]
    fn transform_scale_grows_box_from_center() {
        // scale(2) → 폭/높이 2배, 중앙 원점 유지 (100x50 중앙 (50,25) 고정)
        let d = layout_for(
            "<div></div>",
            "div { display: block; width: 100px; height: 50px; transform: scale(2); }",
            800.0,
        );
        assert_eq!(d.content.width, 200.0, "폭 2배");
        assert_eq!(d.content.height, 100.0, "높이 2배");
        // 중앙 (50,25) 고정 → 좌상단은 (50-100, 25-50) = (-50, -25)
        assert_eq!(d.content.x, -50.0, "중앙 원점 기준 x");
        assert_eq!(d.content.y, -25.0, "중앙 원점 기준 y");
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
    fn max_height_clamps_only_when_clipping() {
        // max-height 는 overflow 가 잘릴 때만 적용 (overflow:hidden)
        let clipped = layout_for(
            "<div><div class=\"tall\"></div></div>",
            "div { display: block; } div > div { height: 300px; } \
             div:first-child, div { max-height: 100px; overflow: hidden; }",
            800.0,
        );
        assert!(clipped.content.height <= 100.5, "overflow:hidden + max-height → 클램프, 실제 {}", clipped.content.height);
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
