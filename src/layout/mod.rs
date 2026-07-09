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
}

pub struct LayoutBox<'a> {
    pub dimensions: Dimensions,
    pub styled_node: &'a StyledNode<'a>,
    pub children: Vec<LayoutBox<'a>>,
    pub glyphs: Vec<GlyphInstance>,
    pub inline_nodes: Vec<&'a StyledNode<'a>>,
    pub image: Option<usize>,
    pub background_image: Option<usize>,
    // 클릭 히트 영역: (단어 단위 사각형, href)
    pub links: Vec<(Rect, String)>,
    // 링크 밑줄/리스트 불릿 등 (사각형, 색)
    pub decorations: Vec<(Rect, Color)>,
    // 리스트 마커 텍스트 (ol: "1." / ul: "•"). build 시 부모 리스트가 부여.
    pub list_marker: Option<String>,
    // 콘텐츠의 실제 사용 폭 (shrink-to-fit float 배치용)
    pub used_width: f32,
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
            links: Vec::new(),
            decorations: Vec::new(),
            list_marker: None,
            used_width: 0.0,
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
            links: Vec::new(),
            decorations: Vec::new(),
            list_marker: None,
            used_width: 0.0,
        }
    }

    fn layout(&mut self, containing_block: Dimensions, fonts: &FontStack, images: &ImageMap) {
        // 이미지 대체 요소: 고유 크기 박스
        if let NodeType::Element(e) = &self.styled_node.node.node_type {
            if e.tag_name == "img" {
                if let Some(src) = e.attributes.get("src") {
                    if let Some(&(idx, iw, ih)) = images.get(src) {
                        self.calculate_width(containing_block);
                        self.calculate_position(containing_block);
                        self.dimensions.content.width = iw as f32;
                        self.dimensions.content.height = ih as f32;
                        self.image = Some(idx);
                        return;
                    }
                }
            }
            if e.tag_name == "input" {
                self.layout_input(containing_block, fonts);
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
        // 배경 이미지 해결 (블록 박스만 — 익명 인라인 박스는 부모 스타일 공유라 제외)
        if let Some(Value::Url(u)) = self.styled_node.value("background-image") {
            if let Some(&(idx, _, _)) = images.get(&u) {
                self.background_image = Some(idx);
            }
        }
        let tag = match &self.styled_node.node.node_type {
            NodeType::Element(e) => e.tag_name.as_str(),
            _ => "",
        };
        if matches!(self.styled_node.display(), Display::Flex) {
            self.layout_flex_children(fonts, images);
        } else if matches!(self.styled_node.display(), Display::Grid) {
            self.layout_grid_children(fonts, images);
        } else if tag == "table" {
            self.layout_table(fonts, images);
        } else if tag == "tr" {
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
            });
            pen += adv;
        }
    }

    // <input> 대체 요소: 폭 = CSS width > size 속성 > 기본 180px,
    // 높이 = font-size × 1.5. value 속성을 글리프로 렌더. type=hidden 은 0 크기.
    fn layout_input(&mut self, containing_block: Dimensions, fonts: &FontStack) {
        let NodeType::Element(e) = &self.styled_node.node.node_type else { return };
        if e.attributes.get("type").map(|t| t.as_str()) == Some("hidden") {
            return; // 0 크기, 글리프 없음
        }
        self.calculate_width(containing_block);
        self.calculate_position(containing_block);
        let px = self
            .styled_node
            .value("font-size")
            .map(|v| v.to_px())
            .filter(|&p| p > 0.0)
            .unwrap_or(16.0);
        let value = e.attributes.get("value").cloned().unwrap_or_default();
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
                });
            }
            pen += adv;
        }
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
        match self.styled_node.value("text-align") {
            Some(Value::Keyword(s)) if s == "center" => "center",
            Some(Value::Keyword(s)) if s == "right" => "right",
            _ => "left",
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

    // 재레이아웃 전 누적 페인트 상태를 초기화 (glyphs/links/decorations 는 push 로
    // 쌓이므로, float shrink-to-fit 2차 배치 시 중복 방지를 위해 서브트리를 비운다)
    fn clear_render(&mut self) {
        self.glyphs.clear();
        self.links.clear();
        self.decorations.clear();
        self.image = None;
        self.background_image = None;
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
        let mut inline_extent = 0.0f32;
        let n = self.children.len();
        for i in 0..n {
            let cpos = self.children[i].position();
            let cfloat = self.children[i].float();
            let is_ib =
                matches!(self.children[i].styled_node.display(), Display::InlineBlock) && cfloat == "none";

            // 다른 종류의 자식을 만나면 진행 중이던 inline-block 런을 마감(정렬 + 높이 반영)
            if ib_active && !is_ib {
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
            }

            // position: absolute/fixed — 흐름에서 제거. 컨테이닝 블록(=이 컨테이너,
            // 정상적으론 가장 가까운 positioned 조상)의 패딩 박스 기준으로 배치.
            if cpos == "absolute" || cpos == "fixed" {
                let child = &mut self.children[i];
                let mut cb: Dimensions = Default::default();
                cb.content.x = cx;
                cb.content.y = cy;
                cb.content.width = avail;
                child.layout(cb, fonts, images);
                let bw = child.dimensions.border_box().width;
                let has = |p: &str| child.styled_node.value(p).is_some();
                let cur = child.dimensions.border_box();
                let tx = if has("right") && !has("left") {
                    cx + avail - bw - child.offset_val("right")
                } else if has("left") {
                    cx + child.offset_val("left")
                } else {
                    cur.x
                };
                let ty = if has("top") { cy + child.offset_val("top") } else { cur.y };
                child.translate(tx - cur.x, ty - cur.y);
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
                continue; // 정상 흐름 높이엔 직접 미반영 (밴드로 관리)
            }

            // inline-block: 가로로 흐르며 폭 초과 시 줄바꿈 (shrink-to-fit).
            if is_ib {
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
                let explicit = matches!(child.styled_node.value("width"), Some(Length(_, _)));
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
                continue;
            }

            // 정상 블록: 앞선 float 밴드가 있으면 그 아래로 clear
            if band_active {
                let below = band_bottom - cy;
                if below > self.dimensions.content.height {
                    self.dimensions.content.height = below;
                }
                band_active = false;
            }
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
    // 미지원: colspan/rowspan, border-collapse, border-spacing.
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
            self.layout_children(fonts, images);
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
        // 1) 열 수 + 열별 선호/지정 폭 측정 (셀을 avail 로 probe 레이아웃해 used_width 얻음)
        let ncols = rows.iter().map(|&(i, j)| row_at!(self, i, j).children.len()).max().unwrap_or(0);
        if ncols == 0 {
            self.dimensions.content.height = 0.0;
            return;
        }
        let mut col_pref = vec![0.0f32; ncols];
        let mut col_fixed: Vec<Option<f32>> = vec![None; ncols];
        for &(i, j) in &rows {
            let row = row_at!(self, i, j);
            for (c, cell) in row.children.iter_mut().enumerate() {
                if c >= ncols {
                    break;
                }
                if let Some(w) = cell_width(cell, avail) {
                    col_fixed[c] = Some(col_fixed[c].map_or(w, |e: f32| e.max(w)));
                }
                let mut probe: Dimensions = Default::default();
                probe.content.x = ox;
                probe.content.y = oy;
                probe.content.width = avail;
                cell.layout(probe, fonts, images);
                let bp = cell.dimensions.border_box().width - cell.dimensions.content.width;
                col_pref[c] = col_pref[c].max(cell.used_width + bp);
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
        // 3) 행별 배치 (공통 열 폭). 측정 패스 글리프는 clear 후 재배치. 행 높이=최고 셀.
        let mut y = oy;
        for &(i, j) in &rows {
            let row = row_at!(self, i, j);
            let mut row_h = 0.0f32;
            for (c, cell) in row.children.iter_mut().enumerate() {
                if c >= ncols {
                    break;
                }
                cell.clear_render();
                let mut cb: Dimensions = Default::default();
                cb.content.x = col_x[c];
                cb.content.y = y;
                cb.content.width = col_w[c];
                cell.layout(cb, fonts, images);
                row_h = row_h.max(cell.dimensions.margin_box().height);
            }
            // 셀 높이를 행 높이로 stretch (세로 정렬 top 근사)
            for cell in row.children.iter_mut() {
                let vextra = cell.dimensions.margin_box().height - cell.dimensions.content.height;
                cell.dimensions.content.height = (row_h - vextra).max(cell.dimensions.content.height);
            }
            // 행 박스 자체 크기 (행 배경/테두리용)
            row.dimensions.content.x = ox;
            row.dimensions.content.y = y;
            row.dimensions.content.width = avail;
            row.dimensions.content.height = row_h;
            y += row_h;
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
        self.dimensions.content.height = max_h;
    }

    fn calculate_height(&mut self) {
        if let Some(Length(h, Px)) = self.styled_node.value("height") {
            // box-sizing: border-box → 지정 height 는 border box. content = height - 세로 extra.
            let border_box = matches!(self.styled_node.value("box-sizing"),
                Some(Value::Keyword(ref k)) if k == "border-box");
            let vextra = if border_box {
                let d = &self.dimensions;
                d.padding.top + d.padding.bottom + d.border.top + d.border.bottom
            } else {
                0.0
            };
            self.dimensions.content.height = (h - vextra).max(0.0);
        }
    }
}



fn is_tr(b: &LayoutBox) -> bool {
    matches!(&b.styled_node.node.node_type, NodeType::Element(e) if e.tag_name == "tr")
}

fn is_row_group(b: &LayoutBox) -> bool {
    matches!(&b.styled_node.node.node_type,
        NodeType::Element(e) if e.tag_name == "tbody" || e.tag_name == "thead" || e.tag_name == "tfoot")
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

fn build_layout_tree<'a>(style_node: &'a StyledNode<'a>) -> LayoutBox<'a> {
    let mut root = LayoutBox::new(style_node);
    let mut pending: Vec<&'a StyledNode<'a>> = Vec::new();
    for child in &style_node.children {
        match child.display() {
            Display::Block | Display::Flex | Display::Grid | Display::InlineBlock => {
                if !pending.is_empty() {
                    let nodes = std::mem::take(&mut pending);
                    if !all_whitespace(&nodes) {
                        root.children.push(LayoutBox::new_anonymous(style_node, nodes));
                    }
                }
                root.children.push(build_layout_tree(child));
            }
            Display::Inline => pending.push(child),
            Display::None => {}
        }
    }
    if !pending.is_empty() && !all_whitespace(&pending) {
        root.children.push(LayoutBox::new_anonymous(style_node, pending));
    }
    // 리스트면 직속 li 자식에 마커 부여. 종류는 list-style-type 에서 결정.
    if let NodeType::Element(e) = &style_node.node.node_type {
        if e.tag_name == "ol" || e.tag_name == "ul" {
            let ordered = e.tag_name == "ol";
            let mut n = 0;
            for child in &mut root.children {
                if matches!(&child.styled_node.node.node_type,
                    NodeType::Element(ce) if ce.tag_name == "li")
                {
                    n += 1;
                    child.list_marker = list_marker_text(child.styled_node, style_node, ordered, n);
                }
            }
        }
    }
    root
}

// list-style-type(li → ul/ol → 기본) 에 따라 마커 문자열. none 이면 마커 없음.
fn list_marker_text(li: &StyledNode, list: &StyledNode, ordered: bool, index: usize) -> Option<String> {
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
        "lower-alpha" | "lower-latin" => Some(format!("{}.", alpha_marker(index, false))),
        "upper-alpha" | "upper-latin" => Some(format!("{}.", alpha_marker(index, true))),
        "lower-roman" => Some(format!("{}.", roman_marker(index, false))),
        "upper-roman" => Some(format!("{}.", roman_marker(index, true))),
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

pub fn layout_tree<'a>(
    node: &'a StyledNode<'a>,
    mut containing_block: Dimensions,
    fonts: &FontStack,
    images: &ImageMap,
) -> LayoutBox<'a> {
    containing_block.content.height = 0.0;
    let mut root_box = build_layout_tree(node);
    root_box.layout(containing_block, fonts, images);
    root_box
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
    fn auto_width_fills_containing_block_minus_padding() {
        let d = layout_for("<div></div>", "div { display: block; padding: 10px; }", 300.0);
        assert_eq!(d.content.width, 280.0);
        assert_eq!(d.content.x, 10.0);
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
        let d = flex_layout(
            "<tr><td width=\"25%\"></td><td></td><td width=\"25%\"></td></tr>",
            "tr, td { display: block; padding: 0; }",
            400.0,
        );
        assert_eq!(d[0].content.width, 100.0, "25% of 400");
        assert_eq!(d[0].content.x, 0.0);
        assert_eq!(d[1].content.x, 100.0);
        assert_eq!(d[1].content.width, 200.0, "auto 셀 = 남은 200");
        assert_eq!(d[2].content.x, 300.0, "우측 25% 셀");
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
}
