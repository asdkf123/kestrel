use super::{Dimensions, ImageMap, LayoutBox};
use crate::css::Unit::Px;
use crate::css::Value;
use crate::css::Value::Length;
use crate::font::FontStack;

impl<'a> LayoutBox<'a> {
    fn flex_keyword(&self, prop: &str) -> String {
        match self.styled_node.value(prop) {
            Some(Value::Keyword(k)) => k,
            _ => String::new(),
        }
    }

    // flexbox: row/column, flex-wrap, justify-content, align-items/align-self, align-content,
    // gap(row/column), flex-grow/shrink/basis, order, 자동 최소 크기(min-content).
    // 미지원: wrap-reverse 의 줄 역순, 절대 위치 자식의 정적 위치.
    pub(super) fn layout_flex_children(&mut self, fonts: &FontStack, images: &ImageMap) {
        let n = self.children.len();
        if n == 0 {
            return;
        }
        // flex 아이템은 독립 서식 맥락(BFC) — 자식/컨테이너와 margin 상쇄 안 함(§8.3.1).
        for c in self.children.iter_mut() {
            c.bfc_item = true;
        }
        let row = self.flex_keyword("flex-direction") != "column";
        let wrap = matches!(self.styled_node.value("flex-wrap"),
            Some(Value::Keyword(ref k)) if k == "wrap" || k == "wrap-reverse");
        let justify = self.flex_keyword("justify-content");
        let align = self.flex_keyword("align-items");
        // main 축 간격은 row 면 column-gap, column 이면 row-gap (CSS Box Alignment §8).
        // 줄 사이(cross) 간격은 그 반대다. 예전엔 한 값(gap)을 양쪽에 다 썼다.
        let px = |p: &str| self.styled_node.value(p).map(|v| v.to_px()).unwrap_or(0.0);
        let (row_gap, col_gap) = (px("row-gap"), px("column-gap"));
        let (gap, cross_gap) = if row { (col_gap, row_gap) } else { (row_gap, col_gap) };
        let d = self.dimensions;
        let (ox, oy) = (d.content.x, d.content.y);

        // 컨테이너 main 크기 (row=폭 확정, column=height 명시 시 확정 아니면 무한)
        let height_px = match self.styled_node.value("height") {
            Some(Length(h, Px)) => Some(h),
            _ => None,
        };
        let cont_main = if row { d.content.width } else { height_px.unwrap_or(f32::INFINITY) };
        // 컨테이너 cross 크기 (row=height 명시 시, column=폭 확정)
        let cont_cross = if row { height_px } else { Some(d.content.width) };

        // 1) 측정: 각 아이템 base main / cross (margin box 기준)
        let main_prop = if row { "width" } else { "height" };
        let cross_prop = if row { "height" } else { "width" };
        let mut basis = vec![0.0f32; n];
        let mut cross = vec![0.0f32; n];
        let mut grow = vec![0.0f32; n];
        let mut shrink = vec![1.0f32; n]; // flex-shrink 기본 1
        // 자동 최소 크기(min-content): 아이템은 이 아래로 줄지 않는다(CSS Flexbox §4.5).
        // min-width/height:auto(기본)일 때 적용. 명시 min 이 있으면 그 값 사용.
        let mut min_main = vec![0.0f32; n];
        let mut main_fixed = vec![false; n];
        let mut cross_fixed = vec![false; n];
        let measure_w = if row {
            if cont_main.is_finite() { cont_main } else { 100000.0 }
        } else {
            cont_cross.unwrap_or(100000.0)
        };
        for (i, child) in self.children.iter_mut().enumerate() {
            let mut cb: Dimensions = Default::default();
            cb.content.x = ox;
            cb.content.y = oy;
            cb.content.width = measure_w;
            // min-content 측정(row): 폭 0 으로 레이아웃 → used_width = 최장 단어(줄 수 없는 최소).
            if row {
                let mut cb0 = cb;
                cb0.content.width = 0.0;
                child.layout(cb0, fonts, images);
                let extra = child.dimensions.margin_box().width - child.dimensions.content.width;
                let mc = child.used_width + extra;
                // 명시 min-width 가 있으면 그것을(0 포함), 없으면 자동 최소=min-content.
                min_main[i] = match child.styled_node.value("min-width") {
                    Some(Length(m, Px)) => m + extra,
                    Some(Value::Keyword(ref k)) if k == "0" => 0.0,
                    _ => mc,
                };
                // 폭 0 측정으로 쌓인 글리프/치수(줄바꿈으로 부풀린 높이)를 제거해
                // 뒤이은 실제 측정이 오염되지 않게 한다(layout 은 비멱등 — 글리프 누적).
                child.clear_render();
            }
            child.layout(cb, fonts, images);
            let mbox = child.dimensions.margin_box();
            main_fixed[i] = matches!(child.styled_node.value(main_prop), Some(Length(_, Px)));
            cross_fixed[i] = matches!(child.styled_node.value(cross_prop), Some(Length(_, Px)));
            // flex-basis: 확정 길이/%(auto/content 는 내용 기반) 면 base main size 로 사용.
            // flex:1 = basis 0% → 모든 아이템 base 0, grow 가 자유공간 균등 분배 → 등폭.
            let (mbw, cw) = (mbox.width, child.dimensions.content.width);
            let basis_override: Option<f32> = match child.styled_node.value("flex-basis") {
                Some(Length(b, Px)) => Some(b),
                Some(Length(b, crate::css::Unit::Percent)) if cont_main.is_finite() => {
                    Some(b / 100.0 * cont_main)
                }
                _ => None,
            };
            // 고정 main 은 border_box (phantom margin 배제), auto 는 내용 preferred+box.
            basis[i] = if let Some(b) = basis_override {
                // flex-basis 는 content-box 크기 → box extras(테두리/패딩/마진) 더해 margin-box 기준
                let extra = if row { mbw - cw } else { mbox.height - child.dimensions.content.height };
                (b + extra).max(0.0)
            } else if row {
                if main_fixed[i] {
                    child.dimensions.border_box().width
                } else {
                    child.used_width + (mbw - cw)
                }
            } else if main_fixed[i] {
                child.dimensions.border_box().height
            } else {
                mbox.height
            };
            // cross: row=높이(내용), column=폭(고정이면 border_box, auto 는 shrink-to-fit)
            cross[i] = if row {
                mbox.height
            } else if cross_fixed[i] {
                child.dimensions.border_box().width
            } else {
                child.used_width + (mbox.width - child.dimensions.content.width)
            };
            grow[i] = child.styled_node.value("flex-grow").map(|v| v.to_px()).unwrap_or(0.0);
            shrink[i] = child.styled_node.value("flex-shrink").map(|v| v.to_px()).unwrap_or(1.0);
        }

        // order: 아이템을 order 값 오름차순으로 재정렬 (안정 정렬 → 동일 order 는 DOM 순서)
        let mut order_seq: Vec<usize> = (0..n).collect();
        let orders: Vec<i32> = (0..n)
            .map(|i| self.children[i].styled_node.value("order").map(|v| v.to_px() as i32).unwrap_or(0))
            .collect();
        order_seq.sort_by_key(|&i| orders[i]);

        // 2) 줄 나누기 (wrap 이고 main 확정일 때만). order_seq 순서로 배치.
        let mut lines: Vec<Vec<usize>> = Vec::new();
        if wrap && cont_main.is_finite() {
            let mut cur: Vec<usize> = Vec::new();
            let mut cur_main = 0.0f32;
            for &i in &order_seq {
                let add = basis[i] + if cur.is_empty() { 0.0 } else { gap };
                if !cur.is_empty() && cur_main + add > cont_main + 0.5 {
                    lines.push(std::mem::take(&mut cur));
                    cur_main = 0.0;
                }
                cur_main += basis[i] + if cur.is_empty() { 0.0 } else { gap };
                cur.push(i);
            }
            if !cur.is_empty() {
                lines.push(cur);
            }
        } else {
            lines.push(order_seq.clone());
        }

        // 3) 줄마다 main 크기 배분 (grow/shrink) — **먼저** 확정한다.
        let mut all_sizes: Vec<Vec<f32>> = Vec::with_capacity(lines.len());
        let mut natural_main = 0.0f32; // 내용 기반 main 폭 (shrink-to-fit used_width 용)
        let mut max_main = 0.0f32;
        for line in &lines {
            let cnt = line.len();
            let sum_basis: f32 = line.iter().map(|&i| basis[i]).sum();
            let sum_gap = gap * (cnt as f32 - 1.0).max(0.0);
            natural_main = natural_main.max(sum_basis + sum_gap);
            let free = if cont_main.is_finite() { cont_main - sum_basis - sum_gap } else { 0.0 };
            let total_grow: f32 = line.iter().map(|&i| grow[i]).sum();
            let mut sizes: Vec<f32> = line.iter().map(|&i| basis[i]).collect();
            if free > 0.0 && total_grow > 0.0 {
                for (k, &i) in line.iter().enumerate() {
                    sizes[k] += free * grow[i] / total_grow;
                }
            } else if free < 0.0 {
                // flex-shrink: 음수 공간을 shrink[i]×basis[i] 가중치로 분배 (넘침 방지).
                // 단 아이템은 자동 최소 크기(min-content) 아래로 줄지 않는다(§4.5).
                let weighted: f32 = line.iter().map(|&i| shrink[i] * basis[i]).sum();
                if weighted > 0.0 {
                    for (k, &i) in line.iter().enumerate() {
                        sizes[k] += free * (shrink[i] * basis[i]) / weighted;
                        sizes[k] = sizes[k].max(min_main[i]);
                    }
                }
            }
            max_main =
                max_main.max(sum_basis + sum_gap + if total_grow > 0.0 { free.max(0.0) } else { 0.0 });
            all_sizes.push(sizes);
        }

        // 3b) cross 재측정 — 표준 순서다 (§9.4: 가설 cross 크기는 main 확정 **뒤**에 잰다).
        // row 에서 아이템 폭이 바뀌면 글이 다시 줄바꿈되어 높이가 달라진다. 예전엔 컨테이너
        // 폭에서 잰 높이를 그대로 써서, flex:1 로 폭이 줄어든 아이템이 두 줄이 돼도 줄 높이는
        // 한 줄이었다 — 컨테이너가 짜부라지고 stretch 도 틀린 높이로 늘렸다 (가장 흔한 모양).
        //
        // 비용을 아끼려고 **최종 main 위치**에서 잰다 (main 위치는 cross 를 몰라도 정해진다).
        // 그러면 정렬 오프셋이 0 이고 stretch 가 높이를 안 바꾸는 흔한 경우엔 아래 배치
        // 패스에서 다시 레이아웃할 필요가 없다 — 패스가 늘지 않는다.
        // measured_at[i] = 이 아이템이 이미 레이아웃된 (main 좌표, cross 좌표)
        let mut measured_at: Vec<Option<(f32, f32)>> = vec![None; n];
        if row {
            let mut cross_probe = oy;
            for (li, line) in lines.iter().enumerate() {
                let cnt = line.len();
                let sum_basis: f32 = line.iter().map(|&i| basis[i]).sum();
                let sum_gap = gap * (cnt as f32 - 1.0).max(0.0);
                let free =
                    if cont_main.is_finite() { cont_main - sum_basis - sum_gap } else { 0.0 };
                let total_grow: f32 = line.iter().map(|&i| grow[i]).sum();
                let leftover = if total_grow > 0.0 { 0.0 } else { free.max(0.0) };
                let (start_off, between_extra) = justify_offsets(&justify, leftover, cnt);
                let mut main_pen = ox + start_off;
                for (k, &i) in line.iter().enumerate() {
                    let msize = all_sizes[li][k];
                    let child = &mut self.children[i];
                    let mut cb: Dimensions = Default::default();
                    cb.content.x = main_pen;
                    cb.content.y = cross_probe;
                    cb.content.width = msize;
                    child.clear_render(); // layout 은 비멱등 (글리프 누적)
                    child.layout(cb, fonts, images);
                    cross[i] = child.dimensions.margin_box().height;
                    measured_at[i] = Some((main_pen, cross_probe));
                    main_pen += msize + gap + between_extra;
                }
                // 다음 줄의 잠정 cross 시작 (align-content 는 아래에서 다시 계산한다)
                cross_probe += line.iter().map(|&i| cross[i]).fold(0.0f32, f32::max) + cross_gap;
            }
        }

        // align-content: 다중 줄 flex 에서 cross 축 여유 공간을 줄 시작/사이에 분배 (§8.3).
        // 고정 cross(height/width)가 줄 cross 합보다 클 때만. justify_offsets 재사용.
        let align_content = self.flex_keyword("align-content");
        let (ac_start, ac_between) = if lines.len() > 1 && !align_content.is_empty() {
            if let Some(cc) = cont_cross {
                let total_lc: f32 = lines
                    .iter()
                    .map(|l| l.iter().map(|&i| cross[i]).fold(0.0f32, f32::max))
                    .sum::<f32>()
                    + cross_gap * (lines.len() as f32 - 1.0).max(0.0);
                justify_offsets(&align_content, (cc - total_lc).max(0.0), lines.len())
            } else {
                (0.0, 0.0)
            }
        } else {
            (0.0, 0.0)
        };

        // 4) 배치 (justify/align)
        let mut cross_pen = (if row { oy } else { ox }) + ac_start;
        for (li, line) in lines.iter().enumerate() {
            let cnt = line.len();
            let sizes = &all_sizes[li];
            let sum_basis: f32 = line.iter().map(|&i| basis[i]).sum();
            let sum_gap = gap * (cnt as f32 - 1.0).max(0.0);
            let free = if cont_main.is_finite() { cont_main - sum_basis - sum_gap } else { 0.0 };
            let total_grow: f32 = line.iter().map(|&i| grow[i]).sum();
            // justify: grow 가 free 를 소진 못했을 때만 남은 공간 분배
            let leftover = if total_grow > 0.0 { 0.0 } else { free.max(0.0) };
            let (start_off, between_extra) = justify_offsets(&justify, leftover, cnt);
            let mut main_pen = if row { ox } else { oy } + start_off;

            // 줄 cross 크기 (재측정된 cross 로)
            let line_cross_natural = line.iter().map(|&i| cross[i]).fold(0.0f32, f32::max);
            let line_cross = if lines.len() == 1 {
                cont_cross.unwrap_or(line_cross_natural).max(line_cross_natural)
            } else {
                line_cross_natural
            };

            for (k, &i) in line.iter().enumerate() {
                let msize = sizes[k];
                // align-self 가 있으면 컨테이너 align-items 를 덮는다 (auto = 상속)
                let self_align = match self.children[i].styled_node.value("align-self") {
                    Some(Value::Keyword(ref k)) if k != "auto" => k.clone(),
                    _ => align.clone(),
                };
                let stretch = (self_align.is_empty() || self_align == "stretch") && !cross_fixed[i];
                let item_cross = if stretch { line_cross } else { cross[i] };
                let cross_off = match self_align.as_str() {
                    "center" => (line_cross - item_cross) / 2.0,
                    "flex-end" | "end" => line_cross - item_cross,
                    _ => 0.0,
                };
                let child = &mut self.children[i];
                let mut cb: Dimensions = Default::default();
                if row {
                    cb.content.x = main_pen;
                    cb.content.y = cross_pen + cross_off;
                    cb.content.width = msize;
                } else {
                    cb.content.x = cross_pen + cross_off;
                    cb.content.y = main_pen;
                    cb.content.width = item_cross;
                }
                // 3b 에서 이미 **같은 자리, 같은 크기**로 레이아웃했으면 다시 하지 않는다.
                // (row 의 흔한 경우: 정렬 오프셋 0. 레이아웃은 비싸다 — react.dev 에서
                //  이 한 번을 아끼면 레이아웃 시간이 절반이 된다.)
                let already = measured_at[i]
                    .map(|(mx, my)| {
                        (mx - cb.content.x).abs() < 0.01 && (my - cb.content.y).abs() < 0.01
                    })
                    .unwrap_or(false);
                if !already {
                    child.clear_render(); // 측정 패스에서 쌓인 글리프 제거 (이중 렌더 방지)
                    child.layout(cb, fonts, images);
                }
                // flex main 크기를 강제 (fixed width/height 인 아이템이 grow/shrink 됐을 때).
                // calculate_width 가 CSS 크기로 덮으므로 여기서 flex 계산값으로 재지정.
                if main_fixed[i] {
                    if row {
                        let hextra =
                            child.dimensions.border_box().width - child.dimensions.content.width;
                        child.dimensions.content.width = (msize - hextra).max(0.0);
                    } else {
                        let vextra =
                            child.dimensions.border_box().height - child.dimensions.content.height;
                        child.dimensions.content.height = (msize - vextra).max(0.0);
                    }
                }
                // stretch: cross 크기를 줄 cross 로 강제 (내용보다 클 때만 늘림)
                if stretch {
                    if row {
                        let vextra =
                            child.dimensions.margin_box().height - child.dimensions.content.height;
                        let target = (line_cross - vextra).max(child.dimensions.content.height);
                        child.dimensions.content.height = target;
                    }
                    // column stretch 는 위에서 cb.width=item_cross 로 이미 폭이 늘어남
                }
                main_pen += msize + gap + between_extra;
            }
            cross_pen += line_cross + cross_gap + ac_between;
        }

        // 컨테이너 cross(=흐름) 크기 반영. calculate_height 가 명시 height 로 나중에 덮음.
        if row {
            self.dimensions.content.height = (cross_pen - oy - cross_gap).max(0.0);
        } else {
            self.dimensions.content.height = max_main.max(0.0);
        }
        // shrink-to-fit 부모용 내용 폭: row=내용 main 폭, column=가장 넓은 아이템 cross.
        self.used_width = if row {
            natural_main
        } else {
            cross.iter().cloned().fold(0.0f32, f32::max)
        };
    }
}

// justify-content 의 (시작 오프셋, 아이템 사이 추가 간격) 을 남는 공간 free 로부터 계산.
fn justify_offsets(justify: &str, free: f32, cnt: usize) -> (f32, f32) {
    let n = cnt as f32;
    match justify {
        "center" => (free / 2.0, 0.0),
        "flex-end" | "end" | "right" => (free, 0.0),
        "space-between" if cnt > 1 => (0.0, free / (n - 1.0)),
        "space-around" if cnt > 0 => (free / n / 2.0, free / n),
        "space-evenly" if cnt > 0 => (free / (n + 1.0), free / (n + 1.0)),
        _ => (0.0, 0.0), // flex-start / start / 기본
    }
}
