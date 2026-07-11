use super::{GlyphInstance, LayoutBox, Rect};
use crate::css::{Color, Value};
use crate::dom::NodeType;
use crate::font::FontStack;
use crate::style::{Display, StyledNode};

// ── 양방향(bidi) 텍스트 ── (UAX#9 간소화: 강한 방향 + 중립 해소 + L2 재정렬)

// 문자의 강한 방향. Some(true)=RTL(히브리/아랍), Some(false)=LTR(문자/숫자), None=중립.
fn char_strong_rtl(c: char) -> Option<bool> {
    let u = c as u32;
    let rtl = (0x0590..=0x05FF).contains(&u)  // 히브리
        || (0xFB1D..=0xFB4F).contains(&u)      // 히브리 표현형
        || (0x0600..=0x06FF).contains(&u)      // 아랍
        || (0x0750..=0x077F).contains(&u)      // 아랍 보충
        || (0x08A0..=0x08FF).contains(&u)      // 아랍 확장-A
        || (0xFB50..=0xFDFF).contains(&u)      // 아랍 표현형-A
        || (0xFE70..=0xFEFF).contains(&u); // 아랍 표현형-B
    if rtl {
        Some(true)
    } else if c.is_alphabetic() || c.is_ascii_digit() {
        Some(false)
    } else {
        None
    }
}

// 각 문자의 임베딩 레벨. base_rtl 이면 기준 1, 아니면 0.
fn bidi_levels(chars: &[char], base_rtl: bool) -> Vec<u8> {
    let n = chars.len();
    // 중립은 직전 강한 방향(없으면 기준)으로 해소 (근사).
    let mut prev = base_rtl;
    let mut resolved = vec![base_rtl; n];
    for i in 0..n {
        match char_strong_rtl(chars[i]) {
            Some(d) => {
                resolved[i] = d;
                prev = d;
            }
            None => resolved[i] = prev,
        }
    }
    resolved
        .iter()
        .map(|&d| if base_rtl { if d { 1 } else { 2 } } else if d { 1 } else { 0 })
        .collect()
}

// RTL 런에서 시각적으로 뒤집히는 문자(괄호/부등호 등). 없으면 원문.
fn mirror_char(c: char) -> char {
    match c {
        '(' => ')',
        ')' => '(',
        '[' => ']',
        ']' => '[',
        '{' => '}',
        '}' => '{',
        '<' => '>',
        '>' => '<',
        '\u{00AB}' => '\u{00BB}', // « »
        '\u{00BB}' => '\u{00AB}',
        _ => c,
    }
}

// L2 재정렬: 레벨 배열 → 시각 순서(각 시각 위치의 논리 인덱스).
fn bidi_reorder(levels: &[u8]) -> Vec<usize> {
    let n = levels.len();
    let mut vis: Vec<usize> = (0..n).collect();
    let maxl = *levels.iter().max().unwrap_or(&0);
    for lvl in (1..=maxl).rev() {
        let mut i = 0;
        while i < n {
            if levels[vis[i]] >= lvl {
                let mut j = i;
                while j < n && levels[vis[j]] >= lvl {
                    j += 1;
                }
                vis[i..j].reverse();
                i = j;
            } else {
                i += 1;
            }
        }
    }
    vis
}

// 인라인 텍스트 조각의 계산된 스타일 (런/단어/글리프에 실림).
#[derive(Clone, Copy)]
struct TextStyle {
    color: Color,
    px: f32,
    link: Option<usize>,
    bold: bool,
    italic: bool,
    deco: u8, // text-decoration 비트: 1=underline 2=line-through 4=overline
    deco_color: Option<Color>, // text-decoration-color (없으면 글자색 사용)
    voffset: f32, // vertical-align 세로 오프셋(px, 양수=아래). super/sub/length.
}

// text-decoration-color 명시값 (currentColor/미지정 → None)
fn deco_color_of(node: &StyledNode) -> Option<Color> {
    match node.value("text-decoration-color") {
        Some(Value::Color(c)) => Some(c),
        _ => None,
    }
}

// vertical-align → baseline 오프셋(px, 양수=아래로). CSS 양수는 위로 올림.
fn vertical_offset(node: &StyledNode, px: f32) -> f32 {
    match node.value("vertical-align") {
        Some(Value::Keyword(k)) => match k.as_str() {
            "super" => -0.35 * px,
            "sub" => 0.2 * px,
            "top" | "text-top" => -0.4 * px,
            "middle" => -0.25 * px,
            "bottom" | "text-bottom" => 0.25 * px,
            _ => 0.0, // baseline
        },
        Some(Value::Length(v, crate::css::Unit::Px)) => -v, // 양수 = 위로
        _ => 0.0,
    }
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
            deco_color: deco_color_of(self.styled_node),
            voffset: vertical_offset(self.styled_node, base_px),
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

        // font-family 첫 이름 (@font-face 아이콘 폰트 선택용). 따옴표 제거.
        let font_family: Option<String> = match self.styled_node.value("font-family") {
            Some(Value::Keyword(k)) => k
                .split(',')
                .next()
                .map(|s| s.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
                .filter(|s| !s.is_empty()),
            _ => None,
        };
        let fam = font_family.as_deref();
        let resolve = |ch: char, px: f32| -> (usize, u16, f32) {
            let (fi, gid) = fonts.glyph_for_family(fam, ch);
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
        // 줄별 각 단어의 시작 글리프 인덱스 (justify 정렬용)
        let mut line_words: Vec<Vec<usize>> = vec![Vec::new()];

        // bidi: 문자 시퀀스의 임베딩 레벨(글리프와 1:1) + 글리프별 advance(재정렬용)
        let base_rtl = matches!(self.styled_node.value("direction"),
            Some(Value::Keyword(ref k)) if k == "rtl");
        let all_chars: Vec<char> =
            words.iter().flat_map(|(w, _, _)| w.iter().map(|&(c, _)| c)).collect();
        let levels = bidi_levels(&all_chars, base_rtl);
        let mut glyph_adv: Vec<f32> = Vec::with_capacity(all_chars.len());

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
                line_words.push(Vec::new());
            }
            line_words.last_mut().unwrap().push(self.glyphs.len());
            let word_x0 = pen_x;
            let mut word_px_max = 0.0f32;
            let mut word_color = Color { r: 0, g: 0, b: 0, a: 255 };
            for &(ch, st) in word {
                // RTL 런(홀수 레벨)에서 괄호/부등호 등은 시각적으로 미러 (UAX#9 L4)
                let ch = if levels.get(self.glyphs.len()).map_or(false, |&l| l % 2 == 1) {
                    mirror_char(ch)
                } else {
                    ch
                };
                let (fi, gid, adv) = resolve(ch, st.px);
                self.glyphs.push(GlyphInstance {
                    font_index: fi,
                    glyph_id: gid,
                    x: pen_x,
                    baseline_y: baseline + st.voffset,
                    px: st.px,
                    color: st.color,
                    bold: st.bold,
                    italic: st.italic,
                    rot: 0.0,
                });
                pen_x += adv + letter_spacing;
                glyph_adv.push(adv + letter_spacing);
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
                // text-decoration-color 우선, 없으면 글자색
                let dcolor = word.iter().find_map(|&(_, st)| st.deco_color).unwrap_or(word_color);
                let mut line_at = |y: f32| self.decorations.push((
                    Rect { x: word_x0, y, width: w, height: thick },
                    dcolor,
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
                if let Some(a) = glyph_adv.last_mut() {
                    *a += space_adv; // 단어 뒤 공백을 마지막 글리프 advance 에 포함(재정렬 대비)
                }
            }
        }

        // bidi 재정렬: RTL 문자가 있으면 줄마다 시각 순서로 x 재배치(advance 보존).
        if levels.iter().any(|&l| l > 0) && glyph_adv.len() == self.glyphs.len() {
            for i in 0..line_bounds.len() {
                let start = line_bounds[i].0;
                let end = line_bounds.get(i + 1).map(|b| b.0).unwrap_or(self.glyphs.len());
                if end < start + 2 || end > levels.len() {
                    continue;
                }
                let vis = bidi_reorder(&levels[start..end]);
                let start_x = self.glyphs[start].x;
                let mut x = start_x;
                let mut newx = vec![0.0f32; end - start];
                for &local in &vis {
                    newx[local] = x;
                    x += glyph_adv[start + local];
                }
                for k in 0..(end - start) {
                    self.glyphs[start + k].x = newx[k];
                }
            }
        }

        // justify 정렬: 마지막 줄 제외, 각 줄의 남는 폭을 단어 사이에 균등 분배.
        let align = self.align();
        if align == "justify" {
            let last_line = line_bounds.len().saturating_sub(1);
            for i in 0..line_bounds.len() {
                if i == last_line {
                    continue; // 마지막 줄은 justify 안 함
                }
                let starts = &line_words[i];
                if starts.len() < 2 {
                    continue;
                }
                let w = line_bounds[i].3;
                let extra = (content_w - w) / (starts.len() as f32 - 1.0);
                if extra <= 0.1 {
                    continue;
                }
                let g_end = line_bounds.get(i + 1).map(|b| b.0).unwrap_or(self.glyphs.len());
                // 단어 k(0-기반)의 글리프를 k*extra 만큼 오른쪽으로
                for k in 1..starts.len() {
                    let from = starts[k];
                    let to = starts.get(k + 1).copied().unwrap_or(g_end);
                    for g in &mut self.glyphs[from..to] {
                        g.x += k as f32 * extra;
                    }
                }
            }
        }
        // center/right 정렬: 줄마다 남는 폭만큼 그 줄의 글리프/링크/밑줄을 이동
        if align != "left" && align != "justify" {
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
                    rot: 0.0,
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
                    deco_color: deco_color_of(node).or(style.deco_color),
                    voffset: vertical_offset(node, cpx),
                };
                for child in &node.children {
                    collect_node(child, cstyle, runs, hrefs);
                }
            }
        },
    }
}

#[cfg(test)]
mod bidi_tests {
    use super::{bidi_levels, bidi_reorder};

    #[test]
    fn ltr_base_reverses_rtl_run() {
        // "AB" + 히브리 "אב" → 레벨 [0,0,1,1], 시각순서 [0,1,3,2] (RTL 런 역순)
        let chars: Vec<char> = "AB\u{05D0}\u{05D1}".chars().collect();
        let levels = bidi_levels(&chars, false);
        assert_eq!(levels, vec![0, 0, 1, 1]);
        assert_eq!(bidi_reorder(&levels), vec![0, 1, 3, 2]);
    }

    #[test]
    fn rtl_base_reverses_whole_and_keeps_ltr() {
        // 기준 RTL: 히브리 "אב" + 라틴 "AB" → 레벨 [1,1,2,2]
        // L2: lvl2 로 [2,3] 역순 → 그다음 lvl1 로 전체 역순
        let chars: Vec<char> = "\u{05D0}\u{05D1}AB".chars().collect();
        let levels = bidi_levels(&chars, true);
        assert_eq!(levels, vec![1, 1, 2, 2]);
        // 시각: lvl2 [2,3]→[3,2] → vis=[0,1,3,2]; lvl1 전체 역순 → [2,3,1,0]
        assert_eq!(bidi_reorder(&levels), vec![2, 3, 1, 0]);
    }

    #[test]
    fn mirror_brackets_flip() {
        use super::mirror_char;
        assert_eq!(mirror_char('('), ')');
        assert_eq!(mirror_char(']'), '[');
        assert_eq!(mirror_char('a'), 'a');
    }

    #[test]
    fn pure_ltr_is_identity() {
        let chars: Vec<char> = "hello".chars().collect();
        let levels = bidi_levels(&chars, false);
        assert!(levels.iter().all(|&l| l == 0));
        assert_eq!(bidi_reorder(&levels), vec![0, 1, 2, 3, 4]);
    }
}
