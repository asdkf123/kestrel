use super::{Dimensions, ImageMap, LayoutBox};
use crate::css::Unit::Px;
use crate::css::Value;
use crate::css::Value::Length;
use crate::font::FontStack;

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
        let cols = if spec.is_empty() {
            vec![d.content.width]
        } else {
            resolve_grid_tracks(&spec, d.content.width, gap)
        };
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
        // 아이템을 행별로 배치 (auto-placement, 한 셀씩)
        let mut y = oy;
        let mut idx = 0;
        while idx < n {
            let end = (idx + ncols).min(n);
            let mut row_h = 0.0f32;
            for (k, i) in (idx..end).enumerate() {
                let mut cb: Dimensions = Default::default();
                cb.content.x = col_x[k];
                cb.content.y = y;
                cb.content.width = cols[k];
                let child = &mut self.children[i];
                child.layout(cb, fonts, images);
                row_h = row_h.max(child.dimensions.margin_box().height);
            }
            // align-items stretch (기본): 각 아이템 높이를 행 높이로 늘림
            for i in idx..end {
                let child = &mut self.children[i];
                let cross_fixed = matches!(child.styled_node.value("height"), Some(Length(_, Px)));
                if !cross_fixed {
                    let vextra =
                        child.dimensions.margin_box().height - child.dimensions.content.height;
                    child.dimensions.content.height =
                        (row_h - vextra).max(child.dimensions.content.height);
                }
            }
            y += row_h + row_gap;
            idx += ncols;
        }
        self.dimensions.content.height = (y - oy - row_gap).max(0.0);
        self.used_width = cols.iter().sum::<f32>() + gap * (ncols as f32 - 1.0).max(0.0);
    }
}

enum GTrack {
    Px(f32),
    Fr(f32),
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

// grid-template-columns 문자열 → 각 트랙의 픽셀 폭. fr 은 남는 공간을 비율 배분.
// repeat(N, ..)/repeat(auto-fill, minmax(..))/minmax(..)/px/fr/auto 지원(근사).
fn resolve_grid_tracks(spec: &str, avail: f32, gap: f32) -> Vec<f32> {
    let tracks = expand_grid_tracks(spec, avail, gap);
    let n = tracks.len();
    if n == 0 {
        return Vec::new();
    }
    let total_gap = gap * (n as f32 - 1.0);
    let fixed: f32 = tracks.iter().filter_map(|t| if let GTrack::Px(p) = t { Some(*p) } else { None }).sum();
    let total_fr: f32 = tracks.iter().filter_map(|t| if let GTrack::Fr(f) = t { Some(*f) } else { None }).sum();
    let free = (avail - total_gap - fixed).max(0.0);
    let fr_unit = if total_fr > 0.0 { free / total_fr } else { 0.0 };
    tracks
        .iter()
        .map(|t| match t {
            GTrack::Px(p) => *p,
            GTrack::Fr(f) => fr_unit * f,
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
            out.push(parse_one_grid_track(t));
        }
    }
    out
}

fn parse_one_grid_track(t: &str) -> GTrack {
    if let Some(inner) = strip_func(t, "minmax") {
        let max = inner.split(',').nth(1).unwrap_or("").trim();
        return parse_one_grid_track(max);
    }
    if let Some(num) = t.strip_suffix("fr") {
        GTrack::Fr(num.trim().parse().unwrap_or(1.0))
    } else if let Some(num) = t.strip_suffix("px") {
        GTrack::Px(num.trim().parse().unwrap_or(0.0))
    } else {
        GTrack::Fr(1.0) // auto/% 등 → 1fr 근사
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
