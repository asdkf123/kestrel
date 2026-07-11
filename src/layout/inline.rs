use super::{GlyphInstance, LayoutBox, Rect};
use crate::css::{Color, Value};
use crate::dom::NodeType;
use crate::font::FontStack;
use crate::style::{Display, StyledNode};

// 인라인 텍스트 조각의 계산된 스타일 (런/단어/글리프에 실림).
#[derive(Clone, Copy)]
struct TextStyle {
    color: Color,
    px: f32,
    link: Option<usize>,
    bold: bool,
    italic: bool,
    deco: u8, // text-decoration 비트: 1=underline 2=line-through 4=overline
}

// text-decoration-line Keyword("underline overline" 등) → 비트플래그
fn deco_flags(v: Option<Value>) -> u8 {
    match v {
        Some(Value::Keyword(k)) => k.split_whitespace().fold(0u8, |f, t| match t {
            "underline" => f | 1,
            "line-through" => f | 2,
            "overline" => f | 4,
            _ => f,
        }),
        _ => 0,
    }
}

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
        let base = TextStyle {
            color: base_color,
            px: base_px,
            link: None,
            bold: self.styled_node.is_bold(),
            italic: self.styled_node.is_italic(),
            deco: deco_flags(self.styled_node.value("text-decoration-line")),
        };

        // white-space: nowrap/pre 는 폭 기반 줄바꿈 안 함. pre 계열은 \n 을 강제 개행,
        // 공백 보존. (상속 속성이라 self.styled_node 값이 곧 이 인라인 문맥의 값)
        let ws = match self.styled_node.value("white-space") {
            Some(Value::Keyword(k)) => k,
            _ => "normal".to_string(),
        };
        let can_wrap = ws != "nowrap" && ws != "pre";
        let keep_newlines = ws == "pre" || ws == "pre-wrap" || ws == "pre-line";
        let keep_spaces = ws == "pre" || ws == "pre-wrap";

        let mut runs: Vec<(String, TextStyle)> = Vec::new();
        let mut hrefs: Vec<String> = Vec::new();
        for node in &self.inline_nodes {
            collect_node(node, base, &mut runs, &mut hrefs);
        }
        // text-transform (상속 속성): 이 인라인 문맥의 모든 텍스트에 적용
        if let Some(Value::Keyword(tt)) = self.styled_node.value("text-transform") {
            for (text, _) in runs.iter_mut() {
                *text = apply_text_transform(text, &tt);
            }
        }

        // 단어 목록: (글자들, 앞의 강제 개행, 뒤에 공백 없음=glue). glue 는 break-word 로
        // 쪼갠 조각을 붙이기 위한 것 (일반 단어는 false).
        let mut words: Vec<(Vec<(char, TextStyle)>, bool, bool)> = Vec::new();
        let mut cur: Vec<(char, TextStyle)> = Vec::new();
        let mut break_before = false; // 다음에 확정될 단어 앞에 강제 개행
        let flush = |cur: &mut Vec<(char, TextStyle)>, words: &mut Vec<_>, brk: &mut bool| {
            if !cur.is_empty() {
                words.push((std::mem::take(cur), *brk, false));
                *brk = false;
            }
        };
        for (text, st) in &runs {
            for ch in text.chars() {
                if keep_newlines && ch == '\n' {
                    flush(&mut cur, &mut words, &mut break_before);
                    break_before = true; // 다음 단어(또는 빈 줄)는 개행 후
                } else if ch.is_whitespace() {
                    if keep_spaces {
                        cur.push((ch, *st)); // 공백 보존 (들여쓰기 등)
                    } else {
                        flush(&mut cur, &mut words, &mut break_before); // 공백 접기 → 단어 경계
                    }
                } else {
                    cur.push((ch, *st));
                }
            }
        }
        flush(&mut cur, &mut words, &mut break_before);
        if words.is_empty() {
            return;
        }

        let base_scale = base_px / upm;
        let ascent_px = primary.ascent() as f32 * base_scale;
        let descent_px = primary.descent() as f32 * base_scale; // 보통 음수
        let natural_lh = ascent_px - descent_px + primary.line_gap() as f32 * base_scale;
        // CSS line-height: 지정되면(px 로 확정된 값) 사용, 아니면 폰트 메트릭.
        // 반-리딩(half-leading)만큼 baseline 을 내려 줄 상자 안에서 세로 중앙 정렬.
        let line_height = match self.styled_node.value("line-height") {
            Some(Value::Length(px, crate::css::Unit::Px)) if px > 0.0 => px,
            _ => natural_lh,
        };
        let half_leading = (line_height - (ascent_px - descent_px)) / 2.0;
        // letter-spacing: 글리프마다, word-spacing: 단어 사이 공백에 추가 (상속 속성, px 확정)
        let letter_spacing =
            self.styled_node.value("letter-spacing").map(|v| v.to_px()).unwrap_or(0.0);
        let word_spacing =
            self.styled_node.value("word-spacing").map(|v| v.to_px()).unwrap_or(0.0);
        let space_adv =
            primary.advance_width(primary.glyph_index(' ')) as f32 * base_scale + word_spacing;

        let resolve = |ch: char, px: f32| -> (usize, u16, f32) {
            let (fi, gid) = fonts.glyph_for(ch);
            let f = fonts.font(fi);
            let adv = f.advance_width(gid) as f32 * (px / f.units_per_em() as f32);
            (fi, gid, adv)
        };

        let content_x = self.dimensions.content.x;
        let content_w = self.dimensions.content.width;
        // word-break: break-all / overflow-wrap: break-word|anywhere → 내용폭보다 긴
        // 단어를 글자 단위로 쪼갠다 (긴 URL/토큰 넘침 방지). 상속 속성이 아니라 이 문맥값.
        let break_word = matches!(self.styled_node.value("word-break"),
            Some(Value::Keyword(ref k)) if k == "break-all" || k == "break-word" || k == "anywhere")
            || matches!(self.styled_node.value("overflow-wrap"),
                Some(Value::Keyword(ref k)) if k == "break-word" || k == "anywhere")
            || matches!(self.styled_node.value("word-wrap"),
                Some(Value::Keyword(ref k)) if k == "break-word");
        if break_word && can_wrap && content_w > 1.0 {
            let mut split: Vec<(Vec<(char, TextStyle)>, bool, bool)> = Vec::new();
            for (word, brk, glue) in words.drain(..) {
                let ww: f32 = word.iter().map(|&(ch, st)| resolve(ch, st.px).2 + letter_spacing).sum();
                if ww <= content_w {
                    split.push((word, brk, glue));
                    continue;
                }
                // 내용폭 단위로 글자를 나눠 여러 조각으로. 조각 사이는 glue=true(공백 없음),
                // 마지막 조각만 원래 glue 계승, 첫 조각만 원래 강제개행 계승.
                let mut pieces: Vec<Vec<(char, TextStyle)>> = vec![Vec::new()];
                let mut pw = 0.0f32;
                for (ch, st) in word {
                    let cw = resolve(ch, st.px).2 + letter_spacing;
                    if !pieces.last().unwrap().is_empty() && pw + cw > content_w {
                        pieces.push(Vec::new());
                        pw = 0.0;
                    }
                    pieces.last_mut().unwrap().push((ch, st));
                    pw += cw;
                }
                let last = pieces.len() - 1;
                for (i, piece) in pieces.into_iter().enumerate() {
                    split.push((piece, i == 0 && brk, if i == last { glue } else { true }));
                }
            }
            words = split;
        }
        // float 컨텍스트: 줄이 밴드 안(baseline-ascent < 하단)이면 float 을 피해 좌우 축소.
        let fctx = self.float_ctx;
        let line_range = |baseline: f32| -> (f32, f32) {
            if let Some((fl, fr, bb)) = fctx {
                if baseline - ascent_px < bb {
                    let left = fl.max(content_x);
                    let right = (fr.min(content_x + content_w)).max(left + 1.0);
                    return (left, right);
                }
            }
            (content_x, content_x + content_w)
        };
        // text-indent: 첫 줄만 들여쓰기 (px 또는 컨테이닝 폭 기준 %). 상속 속성.
        let text_indent = match self.styled_node.value("text-indent") {
            Some(Value::Length(v, crate::css::Unit::Percent)) => v / 100.0 * content_w,
            Some(v) => v.to_px(),
            None => 0.0,
        };
        let mut baseline = self.dimensions.content.y + half_leading + ascent_px;
        let (mut line_left, mut line_right) = line_range(baseline);
        let mut pen_x = line_left + text_indent;
        let mut lines = 1;
        // 줄별 시작 인덱스 + 폭 (center/right 정렬 후처리용): (glyph, link, deco, width)
        let mut line_bounds: Vec<(usize, usize, usize, f32)> = vec![(0, 0, 0, 0.0)];

        for (word, force_break, glue) in &words {
            let word_w: f32 = word.iter().map(|&(ch, st)| resolve(ch, st.px).2 + letter_spacing).sum();
            let need_wrap = can_wrap && pen_x > line_left && pen_x + word_w > line_right;
            if *force_break || need_wrap {
                baseline += line_height;
                let (l, r) = line_range(baseline);
                line_left = l;
                line_right = r;
                pen_x = line_left;
                lines += 1;
                line_bounds.push((self.glyphs.len(), self.links.len(), self.decorations.len(), 0.0));
            }
            let word_x0 = pen_x;
            let mut word_px_max = 0.0f32;
            let mut word_color = Color { r: 0, g: 0, b: 0, a: 255 };
            for &(ch, st) in word {
                let (fi, gid, adv) = resolve(ch, st.px);
                self.glyphs.push(GlyphInstance {
                    font_index: fi,
                    glyph_id: gid,
                    x: pen_x,
                    baseline_y: baseline,
                    px: st.px,
                    color: st.color,
                    bold: st.bold,
                    italic: st.italic,
                });
                pen_x += adv + letter_spacing;
                word_px_max = word_px_max.max(st.px);
                word_color = st.color;
            }
            // 링크: 히트 영역 (단어 폭, baseline 위아래로)
            if let Some(li) = word.iter().find_map(|&(_, st)| st.link) {
                self.links.push((
                    Rect {
                        x: word_x0,
                        y: baseline - 0.9 * word_px_max,
                        width: pen_x - word_x0 + space_adv * 0.5,
                        height: 1.2 * word_px_max,
                    },
                    hrefs[li].clone(),
                ));
            }
            // text-decoration: 밑줄/취소선/윗줄 (링크 밑줄도 UA a{underline} 로 여기서)
            let deco = word.iter().fold(0u8, |f, &(_, st)| f | st.deco);
            if deco != 0 {
                let thick = (word_px_max * 0.06).max(1.0);
                let w = pen_x - word_x0;
                let mut line_at = |y: f32| self.decorations.push((
                    Rect { x: word_x0, y, width: w, height: thick },
                    word_color,
                ));
                if deco & 1 != 0 {
                    line_at(baseline + 0.08 * word_px_max); // underline
                }
                if deco & 2 != 0 {
                    line_at(baseline - 0.30 * word_px_max); // line-through
                }
                if deco & 4 != 0 {
                    line_at(baseline - 0.80 * word_px_max); // overline
                }
            }
            line_bounds.last_mut().unwrap().3 = pen_x - content_x; // 줄 폭 (trailing space 제외)
            // glue(break-word 조각)면 다음 조각을 공백 없이 붙인다
            if !*glue {
                pen_x += space_adv;
            }
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

        // text-overflow: ellipsis — nowrap 한 줄이 내용폭을 넘으면 끝을 잘라 "…" 부착.
        if !can_wrap
            && lines == 1
            && matches!(self.styled_node.value("text-overflow"),
                Some(Value::Keyword(ref k)) if k == "ellipsis")
        {
            let limit = content_x + content_w;
            let (efi, egid, eadv) = resolve('…', base_px);
            // 넘치는 글리프가 있으면: … 자리를 남기고 끝 글리프 제거 후 … 를 오른쪽 끝에 붙임
            let overflowing = self.glyphs.iter().any(|g| g.x > limit);
            if overflowing {
                while self.glyphs.last().map(|g| g.x + eadv > limit).unwrap_or(false) {
                    self.glyphs.pop();
                }
                self.glyphs.push(GlyphInstance {
                    font_index: efi,
                    glyph_id: egid,
                    x: (limit - eadv).max(content_x),
                    baseline_y: baseline,
                    px: base_px,
                    color: base.color,
                    bold: base.bold,
                    italic: base.italic,
                });
            }
        }

        self.dimensions.content.height = lines as f32 * line_height;
        // shrink-to-fit float 용: 가장 긴 줄 폭을 내용 폭으로 노출
        self.used_width = line_bounds.iter().map(|b| b.3).fold(0.0f32, f32::max);
    }
}

fn apply_text_transform(s: &str, tt: &str) -> String {
    match tt {
        "uppercase" => s.to_uppercase(),
        "lowercase" => s.to_lowercase(),
        "capitalize" => {
            let mut out = String::with_capacity(s.len());
            let mut at_start = true;
            for ch in s.chars() {
                if ch.is_whitespace() {
                    at_start = true;
                    out.push(ch);
                } else if at_start {
                    out.extend(ch.to_uppercase());
                    at_start = false;
                } else {
                    out.push(ch);
                }
            }
            out
        }
        _ => s.to_string(),
    }
}

fn collect_node<'a>(
    node: &StyledNode<'a>,
    style: TextStyle,
    runs: &mut Vec<(String, TextStyle)>,
    hrefs: &mut Vec<String>,
) {
    match &node.node.node_type {
        NodeType::Text(t) => runs.push((t.clone(), style)),
        NodeType::Element(e) => match node.display() {
            Display::Block | Display::Flex | Display::Grid | Display::InlineBlock | Display::None => {}
            Display::Inline => {
                // 요소의 계산값(상속 반영)으로 자식 텍스트 스타일 갱신
                let cpx = node
                    .value("font-size")
                    .map(|v| v.to_px())
                    .filter(|&p| p > 0.0)
                    .unwrap_or(style.px);
                let ccolor = match node.value("color") {
                    Some(Value::Color(c)) => c,
                    _ => style.color,
                };
                // <a href> 는 하위 텍스트에 링크 컨텍스트를 물려준다
                let clink = match e.attributes.get("href") {
                    Some(h) if e.tag_name == "a" && !h.is_empty() => {
                        hrefs.push(h.clone());
                        Some(hrefs.len() - 1)
                    }
                    _ => style.link,
                };
                let cstyle = TextStyle {
                    color: ccolor,
                    px: cpx,
                    link: clink,
                    bold: node.is_bold(),
                    italic: node.is_italic(),
                    // 데코는 조상에서 자손으로 누적(자손이 끌 수 없음 — CSS 전파 규칙)
                    deco: style.deco | deco_flags(node.value("text-decoration-line")),
                };
                for child in &node.children {
                    collect_node(child, cstyle, runs, hrefs);
                }
            }
        },
    }
}
