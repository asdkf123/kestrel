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
        self.layout_children(fonts, images);
        self.calculate_height();
    }

    fn calculate_width(&mut self, containing_block: Dimensions) {
        let style = self.styled_node;
        let auto = Keyword("auto".to_string());
        let zero = Length(0.0, Px);

        let mut width = style.value("width").unwrap_or(auto.clone());
        let mut margin_left = style.lookup("margin-left", "margin", &zero);
        let mut margin_right = style.lookup("margin-right", "margin", &zero);
        let border_left = style.lookup("border-left-width", "border-width", &zero);
        let border_right = style.lookup("border-right-width", "border-width", &zero);
        let padding_left = style.lookup("padding-left", "padding", &zero);
        let padding_right = style.lookup("padding-right", "padding", &zero);

        let total: f32 = [
            &margin_left,
            &margin_right,
            &border_left,
            &border_right,
            &padding_left,
            &padding_right,
            &width,
        ]
        .iter()
        .map(|v| v.to_px())
        .sum();

        if width != auto && total > containing_block.content.width {
            if margin_left == auto {
                margin_left = Length(0.0, Px);
            }
            if margin_right == auto {
                margin_right = Length(0.0, Px);
            }
        }

        let underflow = containing_block.content.width - total;

        match (width == auto, margin_left == auto, margin_right == auto) {
            (false, false, false) => {
                margin_right = Length(margin_right.to_px() + underflow, Px);
            }
            (false, false, true) => {
                margin_right = Length(underflow, Px);
            }
            (false, true, false) => {
                margin_left = Length(underflow, Px);
            }
            (true, _, _) => {
                if margin_left == auto {
                    margin_left = Length(0.0, Px);
                }
                if margin_right == auto {
                    margin_right = Length(0.0, Px);
                }
                if underflow >= 0.0 {
                    width = Length(underflow, Px);
                } else {
                    width = Length(0.0, Px);
                    margin_right = Length(margin_right.to_px() + underflow, Px);
                }
            }
            (false, true, true) => {
                margin_left = Length(underflow / 2.0, Px);
                margin_right = Length(underflow / 2.0, Px);
            }
        }

        let d = &mut self.dimensions;
        d.content.width = width.to_px();
        d.padding.left = padding_left.to_px();
        d.padding.right = padding_right.to_px();
        d.border.left = border_left.to_px();
        d.border.right = border_right.to_px();
        d.margin.left = margin_left.to_px();
        d.margin.right = margin_right.to_px();
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

    fn layout_children(&mut self, fonts: &FontStack, images: &ImageMap) {
        let d = &mut self.dimensions;
        for child in &mut self.children {
            child.layout(*d, fonts, images);
            d.content.height += child.dimensions.margin_box().height;
        }
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

        let mut runs: Vec<(String, Color, f32)> = Vec::new();
        for node in &self.inline_nodes {
            collect_node(node, base_color, base_px, &mut runs);
        }

        let mut words: Vec<Vec<(char, Color, f32)>> = Vec::new();
        let mut cur: Vec<(char, Color, f32)> = Vec::new();
        for (text, color, px) in &runs {
            for ch in text.chars() {
                if ch.is_whitespace() {
                    if !cur.is_empty() {
                        words.push(std::mem::take(&mut cur));
                    }
                } else {
                    cur.push((ch, *color, *px));
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

        for word in &words {
            let word_w: f32 = word.iter().map(|&(ch, _, px)| resolve(ch, px).2).sum();
            if pen_x > content_x && pen_x + word_w > content_x + content_w {
                pen_x = content_x;
                baseline += line_height;
                lines += 1;
            }
            for &(ch, color, px) in word {
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
            }
            pen_x += space_adv;
        }

        self.dimensions.content.height = lines as f32 * line_height;
    }

    fn calculate_height(&mut self) {
        if let Some(Length(h, Px)) = self.styled_node.value("height") {
            self.dimensions.content.height = h;
        }
    }
}

fn collect_node<'a>(node: &StyledNode<'a>, color: Color, px: f32, runs: &mut Vec<(String, Color, f32)>) {
    match &node.node.node_type {
        NodeType::Text(t) => runs.push((t.clone(), color, px)),
        NodeType::Element(_) => match node.display() {
            Display::Block | Display::None => {}
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
                for child in &node.children {
                    collect_node(child, ccolor, cpx, runs);
                }
            }
        },
    }
}

fn build_layout_tree<'a>(style_node: &'a StyledNode<'a>) -> LayoutBox<'a> {
    let mut root = LayoutBox::new(style_node);
    let mut pending: Vec<&'a StyledNode<'a>> = Vec::new();
    for child in &style_node.children {
        match child.display() {
            Display::Block => {
                if !pending.is_empty() {
                    root.children
                        .push(LayoutBox::new_anonymous(style_node, std::mem::take(&mut pending)));
                }
                root.children.push(build_layout_tree(child));
            }
            Display::Inline => pending.push(child),
            Display::None => {}
        }
    }
    if !pending.is_empty() {
        root.children.push(LayoutBox::new_anonymous(style_node, pending));
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
        let root = crate::html::parse(html.to_string());
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
    fn children_stack_vertically() {
        let root = crate::html::parse(
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
        let root = crate::html::parse("<p>hello world</p>".to_string());
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
            crate::html::parse("<p>aaaa bbbb cccc dddd eeee ffff gggg hhhh</p>".to_string());
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
        let root = crate::html::parse("<p>a <span>b</span> c</p>".to_string());
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
        let root = crate::html::parse("<div><a>link</a></div>".to_string());
        let ss = crate::css::parse("div { display: block; } a { display: inline; }".to_string());
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = 400.0;
        let fs = fonts();
        let lb = layout_tree(&styled, viewport, &fs, &no_images());
        assert!(lb.dimensions.content.height > 0.0, "inline-only block must have height");
        assert!(!glyphs_of(&lb).is_empty(), "link text should render");
    }

    #[test]
    fn image_box_uses_intrinsic_size() {
        let root = crate::html::parse("<div><img src=\"a.png\"></div>".to_string());
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
