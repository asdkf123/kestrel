use std::collections::HashMap;

use crate::css::Unit::Px;
use crate::css::Value::{Keyword, Length};
use crate::css::{Color, Value};
use crate::dom::NodeType;
use crate::font::FontStack;
use crate::style::{Display, StyledNode};

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
        let is_row = matches!(&self.styled_node.node.node_type,
            NodeType::Element(e) if e.tag_name == "tr");
        if matches!(self.styled_node.display(), Display::Flex) {
            self.layout_flex_children(fonts, images);
        } else if is_row {
            self.layout_table_row(fonts, images);
        } else {
            self.layout_children(fonts, images);
        }
        self.calculate_height();
        self.add_list_marker(fonts);
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
        // CSS width 지정이 없으면 (auto → 컨테이너 폭이 됨) size/기본값으로 교체
        if self.styled_node.value("width").is_none() {
            let size_chars =
                e.attributes.get("size").and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0);
            self.dimensions.content.width =
                if size_chars > 0.0 { size_chars * px * 0.55 } else { 180.0 };
        }
        self.dimensions.content.height = px * 1.5;
        // value 텍스트
        let value = e.attributes.get("value").cloned().unwrap_or_default();
        let color = match self.styled_node.value("color") {
            Some(Value::Color(c)) => c,
            _ => Color { r: 20, g: 20, b: 24, a: 255 },
        };
        let mut pen = self.dimensions.content.x + 5.0;
        let baseline = self.dimensions.content.y + px * 1.1;
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
                });
            }
            pen += adv;
        }
    }

    fn calculate_width(&mut self, containing_block: Dimensions) {
        let style = self.styled_node;
        let auto = Keyword("auto".to_string());
        let zero = Length(0.0, Px);

        let width = style.value("width").unwrap_or(auto.clone());
        let margin_left = style.lookup("margin-left", "margin", &zero);
        let margin_right = style.lookup("margin-right", "margin", &zero);
        let border_left = style.lookup("border-left-width", "border-width", &zero).to_px();
        let border_right = style.lookup("border-right-width", "border-width", &zero).to_px();
        let padding_left = style.lookup("padding-left", "padding", &zero).to_px();
        let padding_right = style.lookup("padding-right", "padding", &zero).to_px();
        let extra = border_left + border_right + padding_left + padding_right;
        let avail = containing_block.content.width;

        let (mut cw, mut ml, mut mr) = resolve_width(&width, &margin_left, &margin_right, extra, avail);

        // max-width: 계산된 폭이 상한을 넘으면 고정 폭으로 재계산 (auto 마진 → 가운데 정렬)
        if let Some(Length(mw, Px)) = style.value("max-width") {
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

    // 서브트리 전체를 dx 만큼 가로 이동 (중앙/우측 정렬 후처리)
    fn translate_x(&mut self, dx: f32) {
        self.dimensions.content.x += dx;
        for g in &mut self.glyphs {
            g.x += dx;
        }
        for (r, _) in &mut self.links {
            r.x += dx;
        }
        for (r, _) in &mut self.decorations {
            r.x += dx;
        }
        for c in &mut self.children {
            c.translate_x(dx);
        }
    }

    fn layout_children(&mut self, fonts: &FontStack, images: &ImageMap) {
        let align = self.align();
        let avail = self.dimensions.content.width;
        for child in &mut self.children {
            let d = self.dimensions;
            child.layout(d, fonts, images);
            // 부모가 center/right 이고 자식이 좁으면 (고정폭/이미지) 가로 정렬
            if align != "left" {
                let cw = child.dimensions.border_box().width;
                if cw < avail - 0.5 {
                    let dx = if align == "center" { (avail - cw) / 2.0 } else { avail - cw };
                    child.translate_x(dx);
                }
            }
            self.dimensions.content.height += child.dimensions.margin_box().height;
        }
    }

    // flexbox 부분 지원: 단일 행(row, nowrap). 고정 폭 아이템은 그대로,
    // auto 아이템은 남은 공간을 균등 분배. gap 지원. 컨테이너 높이 = 최고 아이템.
    // 미지원: column/wrap/justify-content/align-items/flex-grow 비율.
    fn layout_flex_children(&mut self, fonts: &FontStack, images: &ImageMap) {
        let n = self.children.len();
        if n == 0 {
            return;
        }
        let gap = self.styled_node.value("gap").map(|v| v.to_px()).unwrap_or(0.0);
        let d = self.dimensions;
        let total_gap = gap * (n as f32 - 1.0);

        let fixed_width = |child: &LayoutBox| -> Option<f32> {
            match child.styled_node.value("width") {
                Some(Length(w, Px)) => Some(w),
                _ => None,
            }
        };
        let fixed_total: f32 = self.children.iter().filter_map(&fixed_width).sum();
        let auto_count = self.children.iter().filter(|c| fixed_width(c).is_none()).count();
        let remaining = (d.content.width - total_gap - fixed_total).max(0.0);
        let share = if auto_count > 0 { remaining / auto_count as f32 } else { 0.0 };

        let mut pen_x = d.content.x;
        let mut max_h = 0.0f32;
        for child in &mut self.children {
            let w = fixed_width(child).unwrap_or(share);
            // 아이템 전용 컨테이닝 블록: 폭 w, 원점 (pen_x, 컨테이너 y)
            let mut cb: Dimensions = Default::default();
            cb.content.x = pen_x;
            cb.content.y = d.content.y;
            cb.content.width = w;
            child.layout(cb, fonts, images);
            pen_x += child.dimensions.margin_box().width + gap;
            max_h = max_h.max(child.dimensions.margin_box().height);
        }
        self.dimensions.content.height = max_h;
    }

    // <tr> 의 셀(td/th)을 가로 컬럼으로 배치. 컬럼 폭 = 행 폭 / 셀 수 (균등 근사).
    // colspan/rowspan, 콘텐츠 기반 폭은 미지원. 행 높이 = 최고 셀.
    fn layout_table_row(&mut self, fonts: &FontStack, images: &ImageMap) {
        let n = self.children.len();
        if n == 0 {
            return;
        }
        let d = self.dimensions;
        let col_w = d.content.width / n as f32;
        let mut pen_x = d.content.x;
        let mut max_h = 0.0f32;
        for child in &mut self.children {
            let mut cb: Dimensions = Default::default();
            cb.content.x = pen_x;
            cb.content.y = d.content.y;
            cb.content.width = col_w;
            child.layout(cb, fonts, images);
            pen_x += col_w;
            max_h = max_h.max(child.dimensions.margin_box().height);
        }
        self.dimensions.content.height = max_h;
    }

    fn layout_inline(&mut self, fonts: &FontStack) {
        let primary = fonts.primary();
        let upm = primary.units_per_em() as f32;
        let base_px = self
            .styled_node
            .value("font-size")
            .map(|v| v.to_px())
            .filter(|&p| p > 0.0)
            .unwrap_or(16.0);
        let base_color = match self.styled_node.value("color") {
            Some(Value::Color(c)) => c,
            _ => Color { r: 0, g: 0, b: 0, a: 255 },
        };

        let mut runs: Vec<(String, Color, f32, Option<usize>)> = Vec::new();
        let mut hrefs: Vec<String> = Vec::new();
        for node in &self.inline_nodes {
            collect_node(node, base_color, base_px, None, &mut runs, &mut hrefs);
        }

        let mut words: Vec<Vec<(char, Color, f32, Option<usize>)>> = Vec::new();
        let mut cur: Vec<(char, Color, f32, Option<usize>)> = Vec::new();
        for (text, color, px, link) in &runs {
            for ch in text.chars() {
                if ch.is_whitespace() {
                    if !cur.is_empty() {
                        words.push(std::mem::take(&mut cur));
                    }
                } else {
                    cur.push((ch, *color, *px, *link));
                }
            }
        }
        if !cur.is_empty() {
            words.push(cur);
        }
        if words.is_empty() {
            return;
        }

        let base_scale = base_px / upm;
        let line_height =
            (primary.ascent() as f32 - primary.descent() as f32 + primary.line_gap() as f32)
                * base_scale;
        let ascent_px = primary.ascent() as f32 * base_scale;
        let space_adv = primary.advance_width(primary.glyph_index(' ')) as f32 * base_scale;

        let resolve = |ch: char, px: f32| -> (usize, u16, f32) {
            let (fi, gid) = fonts.glyph_for(ch);
            let f = fonts.font(fi);
            let adv = f.advance_width(gid) as f32 * (px / f.units_per_em() as f32);
            (fi, gid, adv)
        };

        let content_x = self.dimensions.content.x;
        let content_w = self.dimensions.content.width;
        let mut pen_x = content_x;
        let mut baseline = self.dimensions.content.y + ascent_px;
        let mut lines = 1;
        // 줄별 시작 인덱스 + 폭 (center/right 정렬 후처리용): (glyph, link, deco, width)
        let mut line_bounds: Vec<(usize, usize, usize, f32)> = vec![(0, 0, 0, 0.0)];

        for word in &words {
            let word_w: f32 = word.iter().map(|&(ch, _, px, _)| resolve(ch, px).2).sum();
            if pen_x > content_x && pen_x + word_w > content_x + content_w {
                pen_x = content_x;
                baseline += line_height;
                lines += 1;
                line_bounds.push((self.glyphs.len(), self.links.len(), self.decorations.len(), 0.0));
            }
            let word_x0 = pen_x;
            let mut word_px_max = 0.0f32;
            let mut word_color = Color { r: 0, g: 0, b: 0, a: 255 };
            for &(ch, color, px, _) in word {
                let (fi, gid, adv) = resolve(ch, px);
                self.glyphs.push(GlyphInstance {
                    font_index: fi,
                    glyph_id: gid,
                    x: pen_x,
                    baseline_y: baseline,
                    px,
                    color,
                });
                pen_x += adv;
                word_px_max = word_px_max.max(px);
                word_color = color;
            }
            // 링크: 히트 영역 + 밑줄 (단어 폭, baseline 약간 아래)
            if let Some(li) = word.iter().find_map(|&(_, _, _, l)| l) {
                self.links.push((
                    Rect {
                        x: word_x0,
                        y: baseline - 0.9 * word_px_max,
                        width: pen_x - word_x0 + space_adv * 0.5,
                        height: 1.2 * word_px_max,
                    },
                    hrefs[li].clone(),
                ));
                self.decorations.push((
                    Rect {
                        x: word_x0,
                        y: baseline + 0.08 * word_px_max,
                        width: pen_x - word_x0,
                        height: (word_px_max * 0.06).max(1.0),
                    },
                    word_color,
                ));
            }
            line_bounds.last_mut().unwrap().3 = pen_x - content_x; // 줄 폭 (trailing space 제외)
            pen_x += space_adv;
        }

        // center/right 정렬: 줄마다 남는 폭만큼 그 줄의 글리프/링크/밑줄을 이동
        let align = self.align();
        if align != "left" {
            for i in 0..line_bounds.len() {
                let (g0, l0, d0, w) = line_bounds[i];
                let off = if align == "center" { (content_w - w) / 2.0 } else { content_w - w };
                if off <= 0.5 {
                    continue;
                }
                let g1 = line_bounds.get(i + 1).map(|b| b.0).unwrap_or(self.glyphs.len());
                let l1 = line_bounds.get(i + 1).map(|b| b.1).unwrap_or(self.links.len());
                let d1 = line_bounds.get(i + 1).map(|b| b.2).unwrap_or(self.decorations.len());
                for g in &mut self.glyphs[g0..g1] {
                    g.x += off;
                }
                for (r, _) in &mut self.links[l0..l1] {
                    r.x += off;
                }
                for (r, _) in &mut self.decorations[d0..d1] {
                    r.x += off;
                }
            }
        }

        self.dimensions.content.height = lines as f32 * line_height;
    }

    fn calculate_height(&mut self) {
        if let Some(Length(h, Px)) = self.styled_node.value("height") {
            self.dimensions.content.height = h;
        }
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

fn collect_node<'a>(
    node: &StyledNode<'a>,
    color: Color,
    px: f32,
    link: Option<usize>,
    runs: &mut Vec<(String, Color, f32, Option<usize>)>,
    hrefs: &mut Vec<String>,
) {
    match &node.node.node_type {
        NodeType::Text(t) => runs.push((t.clone(), color, px, link)),
        NodeType::Element(e) => match node.display() {
            Display::Block | Display::Flex | Display::None => {}
            Display::Inline => {
                let cpx = node
                    .value("font-size")
                    .map(|v| v.to_px())
                    .filter(|&p| p > 0.0)
                    .unwrap_or(px);
                let ccolor = match node.value("color") {
                    Some(Value::Color(c)) => c,
                    _ => color,
                };
                // <a href> 는 하위 텍스트에 링크 컨텍스트를 물려준다
                let clink = match e.attributes.get("href") {
                    Some(h) if e.tag_name == "a" && !h.is_empty() => {
                        hrefs.push(h.clone());
                        Some(hrefs.len() - 1)
                    }
                    _ => link,
                };
                for child in &node.children {
                    collect_node(child, ccolor, cpx, clink, runs, hrefs);
                }
            }
        },
    }
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
            Display::Block | Display::Flex => {
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
    // 리스트면 직속 li 자식에 마커 부여 (ol: 1. 2. 3. / ul: 불릿)
    if let NodeType::Element(e) = &style_node.node.node_type {
        let ordered = e.tag_name == "ol";
        if ordered || e.tag_name == "ul" {
            let mut n = 0;
            for child in &mut root.children {
                if matches!(&child.styled_node.node.node_type,
                    NodeType::Element(ce) if ce.tag_name == "li")
                {
                    n += 1;
                    child.list_marker =
                        Some(if ordered { format!("{}.", n) } else { "\u{2022}".to_string() });
                }
            }
        }
    }
    root
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
    fn flex_auto_items_share_remaining_space() {
        let d = flex_layout(
            "<div class=\"row\"><div class=\"i\"></div><div class=\"i\"></div></div>",
            ".row { display: flex; } .i { display: block; height: 10px; }",
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
             .i { display: block; height: 10px; }",
            300.0,
        );
        // 남은 공간 = 300 - 80 - 20(gap 2개) = 200 → auto 2개 각 100
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
            ".row { display: flex; } .i { display: block; height: 10px; }",
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
