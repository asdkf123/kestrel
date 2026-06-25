use crate::css::Unit::Px;
use crate::css::Value::{Keyword, Length};
use crate::style::{Display, StyledNode};

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

pub struct LayoutBox<'a> {
    pub dimensions: Dimensions,
    pub styled_node: &'a StyledNode<'a>,
    pub children: Vec<LayoutBox<'a>>,
}

impl<'a> LayoutBox<'a> {
    fn new(styled_node: &'a StyledNode<'a>) -> LayoutBox<'a> {
        LayoutBox { dimensions: Default::default(), styled_node, children: Vec::new() }
    }

    fn layout(&mut self, containing_block: Dimensions) {
        self.calculate_width(containing_block);
        self.calculate_position(containing_block);
        self.layout_children();
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

    fn layout_children(&mut self) {
        let d = &mut self.dimensions;
        for child in &mut self.children {
            child.layout(*d);
            d.content.height += child.dimensions.margin_box().height;
        }
    }

    fn calculate_height(&mut self) {
        if let Some(Length(h, Px)) = self.styled_node.value("height") {
            self.dimensions.content.height = h;
        }
    }
}

fn build_layout_tree<'a>(style_node: &'a StyledNode<'a>) -> LayoutBox<'a> {
    let mut root = LayoutBox::new(style_node);
    for child in &style_node.children {
        match child.display() {
            Display::Block => root.children.push(build_layout_tree(child)),
            // M1: 인라인/텍스트와 display:none 은 박스를 만들지 않는다 (M2에서 인라인 도입).
            Display::Inline | Display::None => {}
        }
    }
    root
}

pub fn layout_tree<'a>(node: &'a StyledNode<'a>, mut containing_block: Dimensions) -> LayoutBox<'a> {
    // 자식 y 위치 누적의 기준점이 되므로 높이를 0으로 초기화.
    containing_block.content.height = 0.0;
    let mut root_box = build_layout_tree(node);
    root_box.layout(containing_block);
    root_box
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout_for(html: &str, css: &str, viewport_width: f32) -> Dimensions {
        let root = crate::html::parse(html.to_string());
        let ss = crate::css::parse(css.to_string());
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = viewport_width;
        let lb = layout_tree(&styled, viewport);
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
        // content width = 300 - left/right padding(10+10) = 280
        assert_eq!(d.content.width, 280.0);
        // content x offset by left padding
        assert_eq!(d.content.x, 10.0);
    }

    #[test]
    fn children_stack_vertically() {
        // 바깥 div 에는 height 를 주지 않아 자식 합으로 높이가 결정되게 한다.
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
        let lb = layout_tree(&styled, viewport);
        // outer has 2 child boxes
        assert_eq!(lb.children.len(), 2);
        // first child at y=0, second child at y=50 (stacked)
        assert_eq!(lb.children[0].dimensions.content.y, 0.0);
        assert_eq!(lb.children[1].dimensions.content.y, 50.0);
        // outer height = sum of children heights = 100 (height auto → 자식 합)
        assert_eq!(lb.dimensions.content.height, 100.0);
    }
}
