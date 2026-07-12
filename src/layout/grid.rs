use super::{Dimensions, ImageMap, LayoutBox};
use crate::css::Unit::Px;
use crate::css::Value;
use crate::css::Value::Length;
use crate::font::FontStack;
use crate::style::StyledNode;

impl<'a> LayoutBox<'a> {
    // CSS Grid (실용적): grid-template-columns 로 열을 잡고 아이템을 행별 auto-placement.
    // 지원: px/fr/repeat/minmax/auto-fill, gap/row-gap/column-gap, 행 높이=최고 아이템,
    // align-items stretch(기본). 미지원: 명시적 배치(grid-column/row), template-rows/areas, span.
    pub(super) fn layout_grid_children(&mut self, fonts: &FontStack, images: &ImageMap) {
        let n = self.children.len();
        if n == 0 {
            return;
        }
        // grid-template-areas 가 있으면 명시 배치(holy-grail 등)
        if let Some(Value::Keyword(a)) = self.styled_node.value("grid-template-areas") {
            if a.contains('"') {
                self.layout_grid_areas(&a, fonts, images);
                return;
            }
        }
        let d = self.dimensions;
        let (ox, oy) = (d.content.x, d.content.y);
        let gap = self
            .styled_node
            .value("column-gap")
            .or_else(|| self.styled_node.value("gap"))
            .map(|v| v.to_px())
            .unwrap_or(0.0);
        let row_gap = self
            .styled_node
            .value("row-gap")
            .or_else(|| self.styled_node.value("gap"))
            .map(|v| v.to_px())
            .unwrap_or(0.0);
        let spec = match self.styled_node.value("grid-template-columns") {
            Some(Value::Keyword(s)) => s,
            _ => String::new(),
        };
        let gtracks = if spec.is_empty() {
            vec![GTrack::Fr(1.0)]
        } else {
            expand_grid_tracks(&spec, d.content.width, gap)
        };
        let ncols = gtracks.len().max(1);
        // 명시 행 트랙 수 — 음수 라인(-1 등) 해석 및 고정 행높이용.
        let rspec = match self.styled_node.value("grid-template-rows") {
            Some(Value::Keyword(s)) => s,
            _ => String::new(),
        };
        let nrows_explicit = if rspec.is_empty() {
            0
        } else {
            expand_grid_tracks(&rspec, 0.0, row_gap).len()
        };

        // 1) 아이템별 배치 요청 해석: grid-column/row(라인·span·-1), 없으면 auto. (CSS Grid §8)
        let req: Vec<(Option<usize>, usize, Option<usize>, usize)> = self
            .children
            .iter()
            .map(|c| resolve_placement(c.styled_node, ncols, nrows_explicit))
            .collect();

        // 2) auto-placement(grid-auto-flow: row, sparse) — 점유 격자로 최종 (r0,c0,rspan,cspan) 확정.
        let mut occ: Vec<Vec<bool>> = Vec::new();
        let mut placed: Vec<(usize, usize, usize, usize)> = vec![(0, 0, 1, 1); n];
        // A: 행·열 모두 명시
        for i in 0..n {
            if let (Some(c0), Some(r0)) = (req[i].0, req[i].2) {
                let c0 = c0.min(ncols - 1);
                let cspan = req[i].1.min(ncols - c0).max(1);
                let rspan = req[i].3.max(1);
                grid_mark(&mut occ, r0, c0, rspan, cspan, ncols);
                placed[i] = (r0, c0, rspan, cspan);
            }
        }
        // B: 열만 명시(행 auto) — 그 열에서 비는 최상단 행에 배치
        for i in 0..n {
            if let (Some(c0), None) = (req[i].0, req[i].2) {
                let c0 = c0.min(ncols - 1);
                let cspan = req[i].1.min(ncols - c0).max(1);
                let rspan = req[i].3.max(1);
                let mut r0 = 0;
                while !grid_free(&occ, r0, c0, rspan, cspan, ncols) {
                    r0 += 1;
                }
                grid_mark(&mut occ, r0, c0, rspan, cspan, ncols);
                placed[i] = (r0, c0, rspan, cspan);
            }
        }
        // C: 열 auto — 행 커서를 전진시키며 빈 셀에 배치(명시 행이면 그 행 안에서)
        let (mut cur_r, mut cur_c) = (0usize, 0usize);
        for i in 0..n {
            if req[i].0.is_some() {
                continue; // A/B 에서 처리됨
            }
            let cspan = req[i].1.min(ncols).max(1);
            let rspan = req[i].3.max(1);
            if let Some(r0) = req[i].2 {
                // 행 명시, 열 auto: 그 행에서 빈 최좌측
                let mut c0 = 0;
                while c0 + cspan <= ncols && !grid_free(&occ, r0, c0, rspan, cspan, ncols) {
                    c0 += 1;
                }
                let c0 = c0.min(ncols - cspan);
                grid_mark(&mut occ, r0, c0, rspan, cspan, ncols);
                placed[i] = (r0, c0, rspan, cspan);
            } else {
                // 완전 auto: 커서 전진(단조), 줄바꿈
                loop {
                    if cur_c + cspan > ncols {
                        cur_r += 1;
                        cur_c = 0;
                    }
                    if grid_free(&occ, cur_r, cur_c, rspan, cspan, ncols) {
                        grid_mark(&mut occ, cur_r, cur_c, rspan, cspan, ncols);
                        placed[i] = (cur_r, cur_c, rspan, cspan);
                        cur_c += cspan;
                        break;
                    }
                    cur_c += 1;
                }
            }
        }

        // 3) auto 트랙 폭: 실제 배치된 단일 열 아이템의 내용폭 최대치(max-content 근사).
        let has_auto = gtracks.iter().any(|t| matches!(t, GTrack::Auto));
        let mut auto_content = vec![0.0f32; ncols];
        if has_auto {
            for i in 0..n {
                let (_, c0, _, cspan) = placed[i];
                if cspan == 1 && matches!(gtracks.get(c0), Some(GTrack::Auto)) {
                    let mut cb: Dimensions = Default::default();
                    cb.content.width = d.content.width;
                    self.children[i].layout(cb, fonts, images);
                    let ch = &self.children[i];
                    let extra = ch.dimensions.margin_box().width - ch.dimensions.content.width;
                    auto_content[c0] = auto_content[c0].max((ch.used_width + extra).min(d.content.width));
                }
            }
        }
        let cols = resolve_tracks_sized(&gtracks, d.content.width, gap, &auto_content);
        let ncols = cols.len().max(1);
        // 열 x 위치 (누적 폭 + gap)
        let mut col_x = Vec::with_capacity(ncols);
        {
            let mut x = ox;
            for w in &cols {
                col_x.push(x);
                x += w + gap;
            }
        }
        let span_w = |c0: usize, cspan: usize| -> f32 {
            let end = (c0 + cspan).min(cols.len());
            let w: f32 = cols[c0..end].iter().sum();
            w + gap * ((end - c0) as f32 - 1.0).max(0.0)
        };

        // 4) 행 높이: 각 아이템을 열스팬 폭으로 측정. 단일행은 그 행 최대, 다중행은 마지막 행 보충.
        let nrows = placed.iter().map(|p| p.0 + p.2).max().unwrap_or(1).max(1);
        let row_fixed: Vec<Option<f32>> = if nrows_explicit == 0 {
            vec![None; nrows]
        } else {
            let tr = resolve_grid_tracks(&rspec, 0.0, row_gap);
            (0..nrows).map(|r| tr.get(r).copied().filter(|v| *v > 0.0)).collect()
        };
        // grid-auto-rows: 명시 행 트랙 밖(암시 행)의 고정 높이. px/track 값이면 그 행 높이로 사용.
        let auto_row_px: Option<f32> = match self.styled_node.value("grid-auto-rows") {
            Some(Length(p, Px)) if p > 0.0 => Some(p),
            Some(Value::Keyword(ref s)) => {
                resolve_grid_tracks(s, 0.0, 0.0).first().copied().filter(|v| *v > 0.0)
            }
            _ => None,
        };
        let mut row_h = vec![0.0f32; nrows];
        let mut item_h = vec![0.0f32; n];
        for i in 0..n {
            let (r0, c0, rspan, cspan) = placed[i];
            let mut cb: Dimensions = Default::default();
            cb.content.x = ox;
            cb.content.y = oy;
            cb.content.width = span_w(c0, cspan);
            self.children[i].layout(cb, fonts, images);
            item_h[i] = self.children[i].dimensions.margin_box().height;
            if rspan == 1 {
                row_h[r0] = row_h[r0].max(item_h[i]);
            }
        }
        for i in 0..n {
            let (r0, _, rspan, _) = placed[i];
            if rspan > 1 {
                let cur: f32 =
                    row_h[r0..r0 + rspan].iter().sum::<f32>() + row_gap * (rspan as f32 - 1.0);
                if item_h[i] > cur {
                    row_h[r0 + rspan - 1] += item_h[i] - cur;
                }
            }
        }
        for r in 0..nrows {
            if let Some(px) = row_fixed.get(r).copied().flatten() {
                row_h[r] = px; // grid-template-rows 고정 높이
            } else if r >= nrows_explicit {
                if let Some(ap) = auto_row_px {
                    row_h[r] = ap; // grid-auto-rows 고정 높이(암시 행)
                }
            }
        }
        let mut row_y = vec![oy; nrows + 1];
        {
            let mut y = oy;
            for r in 0..nrows {
                row_y[r] = y;
                y += row_h[r] + row_gap;
            }
            row_y[nrows] = y;
        }

        // 컨테이너 기본 정렬(§11): justify-items(가로)/align-items(세로). place-items 단축은
        // shorthand 에서 이미 justify-items/align-items 로 분해됨. 기본=stretch.
        let kw = |p: &str| match self.styled_node.value(p) {
            Some(Value::Keyword(k)) => k,
            _ => String::new(),
        };
        let justify_items = kw("justify-items");
        let align_items = kw("align-items");

        // 5) 최종 배치: 셀 안에서 justify/align 에 따라 stretch/start/end/center.
        for i in 0..n {
            let (r0, c0, rspan, cspan) = placed[i];
            let cell_x = col_x[c0.min(cols.len() - 1)];
            let w = span_w(c0, cspan);
            let cell_y = row_y[r0];
            let h: f32 =
                row_h[r0..r0 + rspan].iter().sum::<f32>() + row_gap * (rspan as f32 - 1.0);
            let node = self.children[i].styled_node;
            let justify = grid_item_align(node, "justify-self", &justify_items);
            let align = grid_item_align(node, "align-self", &align_items);
            let width_fixed = matches!(node.value("width"), Some(Length(_, Px)));
            let height_fixed = matches!(node.value("height"), Some(Length(_, Px)));

            // 가로: stretch(기본) → 셀 폭 채움. start/end/center → 내용 폭(shrink-to-fit) + 오프셋.
            self.children[i].clear_render();
            let mut cb: Dimensions = Default::default();
            cb.content.x = cell_x;
            cb.content.y = cell_y;
            cb.content.width = w;
            self.children[i].layout(cb, fonts, images); // 측정(셀 폭)
            let stretch_x =
                !width_fixed && (justify.is_empty() || justify == "stretch" || justify == "normal");
            let (content_w, just_off) = if stretch_x {
                (w, 0.0)
            } else {
                let ch = &self.children[i];
                let extra_w = ch.dimensions.margin_box().width - ch.dimensions.content.width;
                let cw = if width_fixed {
                    ch.dimensions.content.width
                } else {
                    ch.used_width.min((w - extra_w).max(0.0))
                };
                let item_w = cw + extra_w;
                let off = match justify.as_str() {
                    "end" | "right" | "flex-end" => (w - item_w).max(0.0),
                    "center" => ((w - item_w) / 2.0).max(0.0),
                    _ => 0.0, // start/left/flex-start
                };
                (cw, off)
            };

            // 최종 가로 레이아웃 (폭·x 확정)
            self.children[i].clear_render();
            cb.content.x = cell_x + just_off;
            cb.content.y = cell_y;
            cb.content.width = content_w;
            self.children[i].layout(cb, fonts, images);

            // 세로: stretch(기본, height auto) → 셀 높이. 아니면 내용 높이 + align 오프셋.
            let vextra = self.children[i].dimensions.margin_box().height
                - self.children[i].dimensions.content.height;
            let stretch_y =
                !height_fixed && (align.is_empty() || align == "stretch" || align == "normal");
            if stretch_y {
                self.children[i].dimensions.content.height =
                    (h - vextra).max(self.children[i].dimensions.content.height);
            } else {
                let item_h = self.children[i].dimensions.margin_box().height;
                let align_off = match align.as_str() {
                    "end" | "flex-end" => (h - item_h).max(0.0),
                    "center" => ((h - item_h) / 2.0).max(0.0),
                    _ => 0.0, // start/flex-start
                };
                if align_off > 0.5 {
                    self.children[i].clear_render();
                    cb.content.y = cell_y + align_off;
                    self.children[i].layout(cb, fonts, images);
                }
            }
        }
        self.dimensions.content.height = (row_y[nrows] - oy - row_gap).max(0.0);
        self.used_width = cols.iter().sum::<f32>() + gap * (ncols as f32 - 1.0).max(0.0);
    }
}

// ── CSS Grid 라인 기반 배치 (§8) ────────────────────────────────────────────
#[derive(Clone, Copy)]
enum GLine {
    Auto,
    Num(i32),
    Span(usize),
}

// 아이템 정렬 값: justify-self/align-self(auto 아니면 우선) → 없으면 컨테이너 기본.
fn grid_item_align(node: &StyledNode, self_prop: &str, container_default: &str) -> String {
    match node.value(self_prop) {
        Some(Value::Keyword(k)) if k != "auto" => k,
        _ => container_default.to_string(),
    }
}

fn parse_gline(s: &str) -> GLine {
    let s = s.trim();
    if s.is_empty() || s == "auto" {
        return GLine::Auto;
    }
    if let Some(rest) = s.strip_prefix("span") {
        return match rest.trim().parse::<usize>() {
            Ok(k) => GLine::Span(k.max(1)),
            Err(_) => GLine::Span(1), // span <name> → 1 근사
        };
    }
    if let Ok(v) = s.parse::<i32>() {
        if v != 0 {
            return GLine::Num(v);
        }
    }
    GLine::Auto // 명명 라인 미지원 → auto
}

// (start, end) 라인 → (0-based 시작 트랙 인덱스 or None=auto, span). ntracks 는 음수 라인 해석용.
fn resolve_axis(start: GLine, end: GLine, ntracks: usize) -> (Option<usize>, usize) {
    let nt = ntracks as i32;
    let to_line = |n: i32| -> i32 { if n < 0 { nt + 2 + n } else { n } };
    let idx = |line: i32| -> usize { (line - 1).max(0) as usize };
    match (start, end) {
        (GLine::Num(a), GLine::Num(b)) => {
            let (mut a, mut b) = (to_line(a), to_line(b));
            if a > b {
                std::mem::swap(&mut a, &mut b);
            }
            (Some(idx(a)), (b - a).max(1) as usize)
        }
        (GLine::Num(a), GLine::Span(s)) => (Some(idx(to_line(a))), s),
        (GLine::Num(a), GLine::Auto) => (Some(idx(to_line(a))), 1),
        (GLine::Auto, GLine::Num(b)) => (Some(idx(to_line(b) - 1)), 1),
        (GLine::Span(s), GLine::Num(b)) => {
            let start_line = (to_line(b) - s as i32).max(1);
            (Some(idx(start_line)), s)
        }
        (GLine::Span(s), _) => (None, s),
        (GLine::Auto, GLine::Span(s)) => (None, s),
        (GLine::Auto, GLine::Auto) => (None, 1),
    }
}

fn resolve_placement(
    node: &StyledNode,
    ncols: usize,
    nrows: usize,
) -> (Option<usize>, usize, Option<usize>, usize) {
    let (mut rs, mut re, mut cs, mut ce) = (GLine::Auto, GLine::Auto, GLine::Auto, GLine::Auto);
    // grid-area: row-start / col-start / row-end / col-end (라인 기반)
    if let Some(Value::Keyword(a)) = node.value("grid-area") {
        if a.contains('/') {
            let p: Vec<&str> = a.split('/').collect();
            rs = parse_gline(p[0]);
            if p.len() > 1 {
                cs = parse_gline(p[1]);
            }
            if p.len() > 2 {
                re = parse_gline(p[2]);
            }
            if p.len() > 3 {
                ce = parse_gline(p[3]);
            }
        }
    }
    if let Some(Value::Keyword(gc)) = node.value("grid-column") {
        let p: Vec<&str> = gc.split('/').collect();
        cs = parse_gline(p[0]);
        ce = if p.len() > 1 { parse_gline(p[1]) } else { GLine::Auto };
    }
    if let Some(Value::Keyword(gr)) = node.value("grid-row") {
        let p: Vec<&str> = gr.split('/').collect();
        rs = parse_gline(p[0]);
        re = if p.len() > 1 { parse_gline(p[1]) } else { GLine::Auto };
    }
    // 롱핸드 우선 적용
    if let Some(Value::Keyword(v)) = node.value("grid-column-start") {
        cs = parse_gline(&v);
    }
    if let Some(Value::Keyword(v)) = node.value("grid-column-end") {
        ce = parse_gline(&v);
    }
    if let Some(Value::Keyword(v)) = node.value("grid-row-start") {
        rs = parse_gline(&v);
    }
    if let Some(Value::Keyword(v)) = node.value("grid-row-end") {
        re = parse_gline(&v);
    }
    let (c0, cspan) = resolve_axis(cs, ce, ncols);
    let (r0, rspan) = resolve_axis(rs, re, nrows);
    (c0, cspan, r0, rspan)
}

// 점유 격자 헬퍼 (행은 필요 시 확장)
fn grid_free(occ: &[Vec<bool>], r0: usize, c0: usize, rspan: usize, cspan: usize, ncols: usize) -> bool {
    if c0 + cspan > ncols {
        return false;
    }
    for r in r0..r0 + rspan {
        if r >= occ.len() {
            continue;
        }
        for c in c0..c0 + cspan {
            if occ[r][c] {
                return false;
            }
        }
    }
    true
}

fn grid_mark(occ: &mut Vec<Vec<bool>>, r0: usize, c0: usize, rspan: usize, cspan: usize, ncols: usize) {
    while occ.len() < r0 + rspan {
        occ.push(vec![false; ncols]);
    }
    for r in r0..r0 + rspan {
        for c in c0..c0 + cspan {
            if c < ncols {
                occ[r][c] = true;
            }
        }
    }
}

#[derive(Clone, Copy)]
enum GTrack {
    Px(f32),
    Fr(f32),
    Auto, // 내용(max-content) 기반 — 배치 시 측정
}

impl<'a> LayoutBox<'a> {
    // grid-template-areas 기반 명시 배치. 아이템의 grid-area 이름으로 셀 영역에 배치.
    // 열 폭 = grid-template-columns, 행 높이 = grid-template-rows(px) 또는 내용 기반.
    fn layout_grid_areas(&mut self, areas: &str, fonts: &FontStack, images: &ImageMap) {
        let d = self.dimensions;
        let (ox, oy) = (d.content.x, d.content.y);
        let gap = self
            .styled_node
            .value("column-gap")
            .or_else(|| self.styled_node.value("gap"))
            .map(|v| v.to_px())
            .unwrap_or(0.0);
        let row_gap = self
            .styled_node
            .value("row-gap")
            .or_else(|| self.styled_node.value("gap"))
            .map(|v| v.to_px())
            .unwrap_or(0.0);

        // 영역 문자열 파싱: 각 "..." 이 한 행, 공백으로 셀 이름 분리. '.' 은 빈 셀.
        let grid: Vec<Vec<String>> = areas
            .split('"')
            .filter(|s| !s.trim().is_empty())
            .map(|row| row.split_whitespace().map(|s| s.to_string()).collect::<Vec<_>>())
            .filter(|r: &Vec<String>| !r.is_empty())
            .collect();
        let nrows = grid.len();
        if nrows == 0 {
            return;
        }
        let ncols = grid.iter().map(|r| r.len()).max().unwrap_or(1).max(1);

        // 이름 → (r0, c0, r1, c1) 경계 상자
        use std::collections::HashMap;
        let mut boxes: HashMap<String, (usize, usize, usize, usize)> = HashMap::new();
        for (r, row) in grid.iter().enumerate() {
            for (c, name) in row.iter().enumerate() {
                if name == "." {
                    continue;
                }
                let e = boxes.entry(name.clone()).or_insert((r, c, r + 1, c + 1));
                e.0 = e.0.min(r);
                e.1 = e.1.min(c);
                e.2 = e.2.max(r + 1);
                e.3 = e.3.max(c + 1);
            }
        }

        // 열 폭
        let cspec = match self.styled_node.value("grid-template-columns") {
            Some(Value::Keyword(s)) => s,
            _ => String::new(),
        };
        let cols = if cspec.is_empty() {
            let w = (d.content.width - gap * (ncols as f32 - 1.0)) / ncols as f32;
            vec![w; ncols]
        } else {
            let mut c = resolve_grid_tracks(&cspec, d.content.width, gap);
            c.resize(ncols, c.last().copied().unwrap_or(0.0));
            c
        };
        let mut col_x = vec![ox; ncols + 1];
        {
            let mut x = ox;
            for c in 0..ncols {
                col_x[c] = x;
                x += cols[c] + gap;
            }
            col_x[ncols] = x;
        }
        let span_w = |c0: usize, c1: usize| -> f32 {
            let w: f32 = cols[c0..c1].iter().sum();
            w + gap * ((c1 - c0) as f32 - 1.0).max(0.0)
        };

        // 각 아이템의 영역 배치 결정 + 폭에 맞춰 측정
        let mut placed: Vec<Option<(usize, usize, usize, usize)>> = vec![None; self.children.len()];
        for i in 0..self.children.len() {
            let name = match self.children[i].styled_node.value("grid-area") {
                Some(Value::Keyword(s)) => s.trim().to_string(),
                _ => String::new(),
            };
            if let Some(&b) = boxes.get(&name) {
                placed[i] = Some(b);
            }
        }

        // 행 높이: grid-template-rows(px) 우선, 없으면 내용 기반.
        let rspec = match self.styled_node.value("grid-template-rows") {
            Some(Value::Keyword(s)) => s,
            _ => String::new(),
        };
        let row_fixed: Vec<Option<f32>> = if rspec.is_empty() {
            vec![None; nrows]
        } else {
            let tr = resolve_grid_tracks(&rspec, 0.0, row_gap);
            (0..nrows).map(|r| tr.get(r).copied()).collect()
        };
        let mut row_h = vec![0.0f32; nrows];
        // 1행 스팬 아이템으로 행 높이 측정
        for i in 0..self.children.len() {
            if let Some((r0, c0, r1, c1)) = placed[i] {
                let w = span_w(c0, c1);
                let mut cb: Dimensions = Default::default();
                cb.content.x = ox;
                cb.content.y = oy;
                cb.content.width = w;
                self.children[i].layout(cb, fonts, images);
                let h = self.children[i].dimensions.margin_box().height;
                if r1 - r0 == 1 {
                    row_h[r0] = row_h[r0].max(h);
                }
            }
        }
        // 다중 행 스팬 아이템: 부족분을 마지막 행에 보충
        for i in 0..self.children.len() {
            if let Some((r0, _, r1, _)) = placed[i] {
                if r1 - r0 > 1 {
                    let h = self.children[i].dimensions.margin_box().height;
                    let cur: f32 = row_h[r0..r1].iter().sum::<f32>() + row_gap * ((r1 - r0) as f32 - 1.0);
                    if h > cur {
                        row_h[r1 - 1] += h - cur;
                    }
                }
            }
        }
        // 고정 행 높이 반영
        for r in 0..nrows {
            if let Some(px) = row_fixed[r] {
                if px > 0.0 {
                    row_h[r] = px;
                }
            }
        }
        let mut row_y = vec![oy; nrows + 1];
        {
            let mut y = oy;
            for r in 0..nrows {
                row_y[r] = y;
                y += row_h[r] + row_gap;
            }
            row_y[nrows] = y;
        }

        // 최종 배치: 영역 셀 사각형으로 재배치 (측정 글리프 clear)
        for i in 0..self.children.len() {
            if let Some((r0, c0, r1, c1)) = placed[i] {
                let x = col_x[c0];
                let w = span_w(c0, c1);
                let y = row_y[r0];
                let h: f32 = row_h[r0..r1].iter().sum::<f32>() + row_gap * ((r1 - r0) as f32 - 1.0);
                self.children[i].clear_render();
                let mut cb: Dimensions = Default::default();
                cb.content.x = x;
                cb.content.y = y;
                cb.content.width = w;
                self.children[i].layout(cb, fonts, images);
                // 셀 높이로 stretch (height 고정 없을 때)
                let cross_fixed =
                    matches!(self.children[i].styled_node.value("height"), Some(Length(_, Px)));
                if !cross_fixed {
                    let vextra = self.children[i].dimensions.margin_box().height
                        - self.children[i].dimensions.content.height;
                    self.children[i].dimensions.content.height =
                        (h - vextra).max(self.children[i].dimensions.content.height);
                }
            }
        }
        self.dimensions.content.height = (row_y[nrows] - oy - row_gap).max(0.0);
        self.used_width = cols.iter().sum::<f32>() + gap * (ncols as f32 - 1.0).max(0.0);
    }
}

// grid-template-columns 문자열 → 각 트랙의 픽셀 폭. auto 는 내용 없이 fr(1) 근사
// (grid-template-areas 경로용 — 내용 측정 없음). 컬럼 경로는 resolve_tracks_sized 사용.
fn resolve_grid_tracks(spec: &str, avail: f32, gap: f32) -> Vec<f32> {
    let tracks = expand_grid_tracks(spec, avail, gap);
    resolve_tracks_sized(&tracks, avail, gap, &[])
}

// 트랙 목록 → 픽셀 폭. auto_content[k] 가 있으면 그 auto 트랙은 내용폭, 없으면 fr(1) 근사.
fn resolve_tracks_sized(tracks: &[GTrack], avail: f32, gap: f32, auto_content: &[f32]) -> Vec<f32> {
    let n = tracks.len();
    if n == 0 {
        return Vec::new();
    }
    // auto 트랙 폭 확정: 측정값 있으면 그것, 없으면 fr 로 취급.
    let auto_w = |k: usize| auto_content.get(k).copied().filter(|w| *w > 0.0);
    let total_gap = gap * (n as f32 - 1.0);
    let mut fixed = 0.0f32;
    let mut total_fr = 0.0f32;
    for (k, t) in tracks.iter().enumerate() {
        match t {
            GTrack::Px(p) => fixed += p,
            GTrack::Fr(f) => total_fr += f,
            GTrack::Auto => match auto_w(k) {
                Some(w) => fixed += w,
                None => total_fr += 1.0, // 측정값 없음 → fr(1) 근사
            },
        }
    }
    let free = (avail - total_gap - fixed).max(0.0);
    let fr_unit = if total_fr > 0.0 { free / total_fr } else { 0.0 };
    tracks
        .iter()
        .enumerate()
        .map(|(k, t)| match t {
            GTrack::Px(p) => *p,
            GTrack::Fr(f) => fr_unit * f,
            GTrack::Auto => auto_w(k).unwrap_or(fr_unit),
        })
        .collect()
}

fn expand_grid_tracks(spec: &str, avail: f32, gap: f32) -> Vec<GTrack> {
    let mut out = Vec::new();
    for tok in split_top_level(spec) {
        let t = tok.trim();
        if let Some(inner) = strip_func(t, "repeat") {
            if let Some(ci) = inner.find(',') {
                let count_str = inner[..ci].trim();
                let sub = inner[ci + 1..].trim();
                if count_str == "auto-fill" || count_str == "auto-fit" {
                    let min_w = grid_track_min(sub).unwrap_or(100.0).max(1.0);
                    let count = (((avail + gap) / (min_w + gap)).floor() as i32).max(1) as usize;
                    for _ in 0..count {
                        out.extend(expand_grid_tracks(sub, avail, gap));
                    }
                } else if let Ok(count) = count_str.parse::<usize>() {
                    for _ in 0..count {
                        out.extend(expand_grid_tracks(sub, avail, gap));
                    }
                }
            }
        } else {
            out.push(parse_one_grid_track(t, avail));
        }
    }
    out
}

fn parse_one_grid_track(t: &str, avail: f32) -> GTrack {
    let t = t.trim();
    if let Some(inner) = strip_func(t, "minmax") {
        // minmax(min, max): max 로 근사 (max 가 fr 이면 fr, auto/content 면 auto)
        let max = inner.split(',').nth(1).unwrap_or("").trim();
        return parse_one_grid_track(max, avail);
    }
    if let Some(inner) = strip_func(t, "fit-content") {
        return parse_one_grid_track(inner.trim(), avail);
    }
    if let Some(num) = t.strip_suffix("fr") {
        GTrack::Fr(num.trim().parse().unwrap_or(1.0))
    } else if let Some(num) = t.strip_suffix('%') {
        GTrack::Px(num.trim().parse::<f32>().map(|p| p / 100.0 * avail).unwrap_or(0.0))
    } else if let Some(num) = t.strip_suffix("px") {
        GTrack::Px(num.trim().parse().unwrap_or(0.0))
    } else {
        GTrack::Auto // auto/min-content/max-content/기타 → 내용 기반 (이전엔 1fr 근사)
    }
}

fn grid_track_min(t: &str) -> Option<f32> {
    let t = t.trim();
    if let Some(inner) = strip_func(t, "minmax") {
        let min = inner.split(',').next().unwrap_or("").trim();
        return min.strip_suffix("px").and_then(|n| n.trim().parse().ok());
    }
    t.strip_suffix("px").and_then(|n| n.trim().parse().ok())
}

fn strip_func<'a>(t: &'a str, name: &str) -> Option<&'a str> {
    let prefix = format!("{}(", name);
    t.strip_prefix(prefix.as_str())?.strip_suffix(')')
}

fn split_top_level(spec: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    for c in spec.chars() {
        match c {
            '(' => {
                depth += 1;
                cur.push(c);
            }
            ')' => {
                depth -= 1;
                cur.push(c);
            }
            c if c.is_whitespace() && depth == 0 => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}
