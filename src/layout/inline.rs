use super::{GlyphInstance, LayoutBox, Rect};
use crate::css::{Color, Value};
use crate::dom::NodeType;
use crate::font::FontStack;
use crate::style::{Display, StyledNode};

impl<'a> LayoutBox<'a> {
    pub(super) fn layout_inline(&mut self, fonts: &FontStack) {
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
        // shrink-to-fit float 용: 가장 긴 줄 폭을 내용 폭으로 노출
        self.used_width = line_bounds.iter().map(|b| b.3).fold(0.0f32, f32::max);
    }
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
            Display::Block | Display::Flex | Display::Grid | Display::InlineBlock | Display::None => {}
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
