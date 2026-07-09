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
