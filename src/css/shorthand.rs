use super::values::interpret_value;
use super::{Color, Declaration, Unit, Value};

// 수 값 파싱: 직접 파싱 실패 시 수학 함수(abs/sign/round/sqrt/…)를 interpret_value
// 로 평가한다. z-index/order/opacity/flex-* 처럼 수를 직접 파싱하던 프로퍼티가
// abs(1)/clamp(0,sign(1),1) 등을 받게 한다(수 반환 함수는 Length(_, Number)).
fn number_or_math(s: &str) -> Option<f32> {
    let s = s.trim();
    if let Ok(n) = s.parse::<f32>() {
        return Some(n);
    }
    // 순수 <number> 수학식(min/max/clamp/sign/round/abs/삼각/…)을 §CSS Values 4 §10
    // 대로 평가한다(부호있는 0·infinity·NaN 포함). 유한값만 수용(정수/수 프로퍼티에
    // inf/NaN 저장 방지). 길이·퍼센트가 섞이면 None 이라 아래 경로로 넘어간다.
    if let Some(v) = crate::css::eval_math_number(s) {
        if v.is_finite() {
            return Some(v as f32);
        }
    }
    match interpret_value(s) {
        Some(Value::Length(f, Unit::Number)) => Some(f),
        // calc(2 + 3)/calc(1 / 4) 등 수식은 eval_calc_number 로 수를 추출한다
        // (eval_calc 는 맨수를 Px 로 반환하지만 여기선 단위 무시하고 수만 쓴다).
        _ => super::eval_calc_number(s),
    }
}

// 선언 하나를 (경우에 따라 여러) longhand 선언으로 확장한다.
pub(crate) fn expand_declaration(name: &str, value_text: &str) -> Vec<Declaration> {
    // 커스텀 프로퍼티(--*): 원문 보존, 사용 시점(var())에 해석.
    if name.starts_with("--") {
        return vec![Declaration { important: false,
            name: name.to_string(),
            value: Value::Keyword(value_text.to_string()),
        }];
    }
    // var() 참조: 원문을 Var 로 보존, 스타일 계산 시 치환·재파싱.
    if value_text.contains("var(") {
        return vec![Declaration { important: false, name: name.to_string(), value: Value::Var(value_text.to_string()) }];
    }
    // @font-face 디스크립터: 값이 프로퍼티 문법이 아니다 (U+0-7F 는 색도 길이도 아니다).
    // 해석기에 넘기면 None → **선언이 통째로 버려진다**. 원문을 보존한다.
    // (unicode-range 를 잃으면 서브셋 폰트를 전부 받게 된다 — 1240개를 받고 있었다)
    if matches!(name, "unicode-range" | "src" | "font-display" | "size-adjust" | "ascent-override"
        | "descent-override" | "line-gap-override")
    {
        return vec![Declaration {
            important: false,
            name: name.to_string(),
            value: Value::Keyword(value_text.trim().to_string()),
        }];
    }
    // CSS 논리 속성 → 물리 속성 (LTR/가로쓰기 가정). 모던 CSS 에서 흔함.
    match name {
        // 크기
        "inline-size" => return expand_declaration("width", value_text),
        "block-size" => return expand_declaration("height", value_text),
        "min-inline-size" => return expand_declaration("min-width", value_text),
        "max-inline-size" => return expand_declaration("max-width", value_text),
        "min-block-size" => return expand_declaration("min-height", value_text),
        "max-block-size" => return expand_declaration("max-height", value_text),
        // 단일 논리 변 (start=left/top, end=right/bottom)
        "margin-inline-start" => return expand_declaration("margin-left", value_text),
        "margin-inline-end" => return expand_declaration("margin-right", value_text),
        "margin-block-start" => return expand_declaration("margin-top", value_text),
        "margin-block-end" => return expand_declaration("margin-bottom", value_text),
        "padding-inline-start" => return expand_declaration("padding-left", value_text),
        "padding-inline-end" => return expand_declaration("padding-right", value_text),
        "padding-block-start" => return expand_declaration("padding-top", value_text),
        "padding-block-end" => return expand_declaration("padding-bottom", value_text),
        "inset-inline-start" => return expand_declaration("left", value_text),
        "inset-inline-end" => return expand_declaration("right", value_text),
        "inset-block-start" => return expand_declaration("top", value_text),
        "inset-block-end" => return expand_declaration("bottom", value_text),
        // 양방향 논리 (1~2 값)
        "margin-inline" => return logical_pair("margin-left", "margin-right", value_text),
        "margin-block" => return logical_pair("margin-top", "margin-bottom", value_text),
        "padding-inline" => return logical_pair("padding-left", "padding-right", value_text),
        "padding-block" => return logical_pair("padding-top", "padding-bottom", value_text),
        "inset-inline" => return logical_pair("left", "right", value_text),
        "inset-block" => return logical_pair("top", "bottom", value_text),
        // scroll-margin/scroll-padding 논리 → 물리(수평 쓰기모드 기준).
        "scroll-margin-block-start" => return expand_declaration("scroll-margin-top", value_text),
        "scroll-margin-block-end" => return expand_declaration("scroll-margin-bottom", value_text),
        "scroll-margin-inline-start" => return expand_declaration("scroll-margin-left", value_text),
        "scroll-margin-inline-end" => return expand_declaration("scroll-margin-right", value_text),
        "scroll-margin-block" => {
            return logical_pair("scroll-margin-top", "scroll-margin-bottom", value_text)
        }
        "scroll-margin-inline" => {
            return logical_pair("scroll-margin-left", "scroll-margin-right", value_text)
        }
        "scroll-padding-block-start" => {
            return expand_declaration("scroll-padding-top", value_text)
        }
        "scroll-padding-block-end" => {
            return expand_declaration("scroll-padding-bottom", value_text)
        }
        "scroll-padding-inline-start" => {
            return expand_declaration("scroll-padding-left", value_text)
        }
        "scroll-padding-inline-end" => {
            return expand_declaration("scroll-padding-right", value_text)
        }
        "scroll-padding-block" => {
            return logical_pair("scroll-padding-top", "scroll-padding-bottom", value_text)
        }
        "scroll-padding-inline" => {
            return logical_pair("scroll-padding-left", "scroll-padding-right", value_text)
        }
        "scroll-margin-top" | "scroll-margin-right" | "scroll-margin-bottom"
        | "scroll-margin-left" => return scroll_side(name, value_text, false),
        "scroll-padding-top" | "scroll-padding-right" | "scroll-padding-bottom"
        | "scroll-padding-left" => return scroll_side(name, value_text, true),
        "scroll-margin" => return scroll_box("scroll-margin", value_text, false),
        "scroll-padding" => return scroll_box("scroll-padding", value_text, true),
        // scroll-snap-type(§CSS Scroll Snap): none | <axis> [strictness]?.
        "scroll-snap-type" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            if crate::css::scroll_snap_type_valid(value_text) {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(crate::css::scroll_snap_type_canonical(value_text)) }];
            }
            return Vec::new();
        }
        // top/right/bottom/left(§CSS Position): <length-percentage> | auto. 각도·단위없는
        // 비영·기타 키워드 거부. 유효값은 interpret_value 로 저장(레이아웃 불변). inset
        // 논리 프로퍼티가 여기로 매핑되므로 함께 검증된다.
        "top" | "right" | "bottom" | "left" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
            {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            let toks = split_top_level(value_text.trim());
            // anchor-size()/anchor()(§css-anchor-1): inset 프로퍼티에서 유효.
            if toks.len() == 1 {
                if let Some(canon) = crate::css::anchor_size_canonical(toks[0])
                    .or_else(|| crate::css::anchor_canonical(toks[0]))
                {
                    return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(canon) }];
                }
            }
            if toks.len() == 1 && crate::css::inset_length_valid(toks[0]) {
                let v = match interpret_value(toks[0]) {
                    // 단위없는 0 은 0px 로 캐논화(§CSSOM).
                    Some(Value::Length(n, Unit::Number)) if n == 0.0 => Value::Length(0.0, Unit::Px),
                    Some(other) => other,
                    None => Value::Keyword(low),
                };
                return vec![Declaration { important: false, name: name.to_string(), value: v }];
            }
            return Vec::new();
        }
        // inset 단축: top/right/bottom/left (margin 과 동일 규칙)
        "inset" => {
            let toks = split_top_level(value_text.trim());
            // anchor-size()/anchor() 토큰이 있으면 단일변 top arm(검증+캐논)을 per-token
            // 재사용해 TRBL 확장(box_shorthand 는 interpret_value 라 anchor 를 못 읽음).
            if !toks.is_empty() && toks.len() <= 4
                && toks.iter().any(|t| crate::css::anchor_size_canonical(t).is_some()
                    || crate::css::anchor_canonical(t).is_some())
            {
                let mut vals = Vec::new();
                for t in &toks {
                    let d = expand_declaration("top", t);
                    if d.len() != 1 {
                        return Vec::new();
                    }
                    vals.push(d[0].value.clone());
                }
                return trbl_decls(["top", "right", "bottom", "left"], &vals);
            }
            let sides = box_shorthand("", "", value_text); // "-top" 등 이름이 "-top" 형태
            return sides
                .into_iter()
                .map(|d| Declaration { important: false, name: d.name.trim_start_matches('-').to_string(), value: d.value })
                .collect();
        }
        _ => {}
    }
    match name {
        "margin" | "padding" => {
            let low = value_text.trim().to_ascii_lowercase();
            if !matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                let toks = split_top_level(value_text.trim());
                let is_margin = name == "margin";
                // anchor-size() 는 margin(단축)에서 유효(padding 은 아님). 토큰이 있으면
                // margin-top 단일변 arm 재사용해 TRBL 확장. (anchor() 는 margin 무효.)
                if is_margin && !toks.is_empty() && toks.len() <= 4
                    && toks.iter().any(|t| crate::css::anchor_size_canonical(t).is_some())
                {
                    let mut vals = Vec::new();
                    for t in &toks {
                        let d = expand_declaration("margin-top", t);
                        if d.len() != 1 {
                            return Vec::new();
                        }
                        vals.push(d[0].value.clone());
                    }
                    return trbl_decls(
                        ["margin-top", "margin-right", "margin-bottom", "margin-left"],
                        &vals,
                    );
                }
                let ok = !toks.is_empty() && toks.len() <= 4
                    && toks.iter().all(|t| if is_margin { crate::css::margin_value_valid(t) } else { crate::css::nonneg_lp_valid(t) });
                if !ok {
                    return Vec::new();
                }
                // 유효하나 box_shorthand 가 계산 못하는 calc 는 지정값 보존.
                let expanded = box_shorthand(name, "", value_text);
                if expanded.is_empty() {
                    return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }];
                }
                return expanded;
            }
            box_shorthand(name, "", value_text)
        }
        // border-width/style/color: 각각 <line-width>/<line-style>/<color> {1,4}.
        // 토큰 유형을 검증해 auto/none(width)/5값/음수/콤마를 거부한다.
        "border-width" => validated_box_shorthand("border", "-width", value_text, is_line_width_tok),
        "border-style" => validated_box_shorthand("border", "-style", value_text, is_line_style_tok),
        "border-color" => {
            validated_box_shorthand("border", "-color", value_text, |t| crate::css::single_color_valid(t))
        }
        // border-radius: 1~4 값 → 네 모서리 longhand. 슬래시 뒤 세로 반경은 근사로 무시.
        "border-radius" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                let d = |n: &str| Declaration { important: false, name: n.to_string(), value: Value::Keyword(low.clone()) };
                return vec![
                    d("border-top-left-radius"),
                    d("border-top-right-radius"),
                    d("border-bottom-right-radius"),
                    d("border-bottom-left-radius"),
                    d("border-radius"),
                ];
            }
            if !crate::css::border_radius_valid(value_text) {
                return Vec::new();
            }
            // h / v 두 축을 각각 4 코너로 확장(§CSS Backgrounds). v 축 없으면 h 를 그대로.
            let mut sp = value_text.splitn(2, '/');
            let hpart = sp.next().unwrap_or("");
            let vpart = sp.next();
            let parse4 = |part: &str| -> Option<[Value; 4]> {
                let t: Vec<Value> = split_top_level(part)
                    .into_iter()
                    .filter_map(interpret_value)
                    .filter(|v| matches!(v, Value::Length(..)))
                    .collect();
                if t.is_empty() {
                    return None;
                }
                Some(match t.len() {
                    1 => [t[0].clone(), t[0].clone(), t[0].clone(), t[0].clone()],
                    2 => [t[0].clone(), t[1].clone(), t[0].clone(), t[1].clone()],
                    3 => [t[0].clone(), t[1].clone(), t[2].clone(), t[1].clone()],
                    _ => [t[0].clone(), t[1].clone(), t[2].clone(), t[3].clone()],
                })
            };
            let Some(h4) = parse4(hpart) else {
                return Vec::new();
            };
            let v4 = match vpart {
                Some(vp) => match parse4(vp) {
                    Some(v) => v,
                    None => return Vec::new(),
                },
                None => h4.clone(),
            };
            // 코너 값: 가로==세로 면 단일 Length(레이아웃·계산 기존대로), 다르면 "h v" Keyword
            // (세로 반경 보존 — getComputedStyle 이 "17px 117px" 로 답하도록).
            let corner = |h: &Value, v: &Value| -> Value {
                let hs = crate::style::computed_value_string(h);
                let vs = crate::style::computed_value_string(v);
                if hs == vs {
                    h.clone()
                } else {
                    Value::Keyword(format!("{} {}", hs, vs))
                }
            };
            vec![
                Declaration { important: false, name: "border-top-left-radius".to_string(), value: corner(&h4[0], &v4[0]) },
                Declaration { important: false, name: "border-top-right-radius".to_string(), value: corner(&h4[1], &v4[1]) },
                Declaration { important: false, name: "border-bottom-right-radius".to_string(), value: corner(&h4[2], &v4[2]) },
                Declaration { important: false, name: "border-bottom-left-radius".to_string(), value: corner(&h4[3], &v4[3]) },
                // box-shadow 등 균일 근사용으로 border-radius 도 남긴다 (첫 코너의 가로).
                Declaration { important: false, name: "border-radius".to_string(), value: h4[0].clone() },
            ]
        }
        // border-image 단축(§CSS Backgrounds):
        //   <source> || <slice> [ / <width> | / <width>? / <outset> ]? || <repeat>
        "border-image" => {
            let low = value_text.trim().to_ascii_lowercase();
            let names = [
                "border-image-source",
                "border-image-slice",
                "border-image-width",
                "border-image-outset",
                "border-image-repeat",
            ];
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return names
                    .iter()
                    .map(|n| Declaration { important: false, name: n.to_string(), value: Value::Keyword(low.clone()) })
                    .collect();
            }
            expand_border_image(value_text.trim()).unwrap_or_default()
        }
        // grid 단축(§CSS Grid):
        //   <grid-template>
        //   | <rows> / [ auto-flow && dense? ] <auto-columns>?
        //   | [ auto-flow && dense? ] <auto-rows>? / <columns>
        "grid" => {
            let v = value_text.trim();
            let low = v.to_ascii_lowercase();
            let six = |tr: &str, tc: &str, ta: &str, ar: &str, ac: &str, af: &str| {
                vec![
                    Declaration { important: false, name: "grid-template-rows".to_string(), value: Value::Keyword(tr.to_string()) },
                    Declaration { important: false, name: "grid-template-columns".to_string(), value: Value::Keyword(tc.to_string()) },
                    Declaration { important: false, name: "grid-template-areas".to_string(), value: Value::Keyword(ta.to_string()) },
                    Declaration { important: false, name: "grid-auto-rows".to_string(), value: Value::Keyword(ar.to_string()) },
                    Declaration { important: false, name: "grid-auto-columns".to_string(), value: Value::Keyword(ac.to_string()) },
                    Declaration { important: false, name: "grid-auto-flow".to_string(), value: Value::Keyword(af.to_string()) },
                ]
            };
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return six(&low, &low, &low, &low, &low, &low);
            }
            // auto-flow 쪽 파싱: auto-flow 와 dense(순서 무관) 후 나머지는 auto 트랙.
            let parse_af = |s: &str| -> Option<(bool, String)> {
                let toks = split_top_level(s);
                let (mut has_af, mut dense, mut i) = (false, false, 0usize);
                while i < toks.len() {
                    match toks[i].to_ascii_lowercase().as_str() {
                        "auto-flow" if !has_af => has_af = true,
                        "dense" if !dense => dense = true,
                        _ => break,
                    }
                    i += 1;
                }
                if !has_af {
                    return None;
                }
                let rest = toks[i..].join(" ");
                let track = if rest.trim().is_empty() {
                    "auto".to_string()
                } else if crate::css::grid_auto_track_valid(rest.trim()) {
                    rest.trim().to_string()
                } else {
                    return None;
                };
                Some((dense, track))
            };
            let parts = split_top_slash_pub(v);
            let first_is_af = |s: &str| {
                s.split_whitespace()
                    .next()
                    .map(|t| t.eq_ignore_ascii_case("auto-flow") || t.eq_ignore_ascii_case("dense"))
                    .unwrap_or(false)
            };
            if parts.len() == 2 {
                let (l, r) = (parts[0].trim(), parts[1].trim());
                let (l_af, r_af) = (first_is_af(l), first_is_af(r));
                if !l_af && r_af {
                    // <rows> / auto-flow [dense]? <auto-columns>?  → 방향 column
                    if !l.is_empty() && crate::css::grid_template_track_valid(l) {
                        if let Some((dense, ac)) = parse_af(r) {
                            let af = if dense { "column dense" } else { "column" };
                            return six(&crate::css::grid_template_track_canonical(l), "none", "none", "auto", &ac, af);
                        }
                    }
                    return Vec::new();
                } else if l_af && !r_af {
                    // auto-flow [dense]? <auto-rows>? / <columns>  → 방향 row
                    if !r.is_empty() && crate::css::grid_template_track_valid(r) {
                        if let Some((dense, ar)) = parse_af(l) {
                            let af = if dense { "dense" } else { "row" };
                            return six("none", &crate::css::grid_template_track_canonical(r), "none", &ar, "auto", af);
                        }
                    }
                    return Vec::new();
                } else if l_af && r_af {
                    return Vec::new(); // 양쪽 auto-flow 는 무효
                }
            }
            // 그 외는 <grid-template> 형식(none | rows/cols | 문자열): auto-* 는 초기값.
            match expand_grid_template_value(v) {
                Some((r, c, a)) => six(&r, &c, &a, "auto", "auto", "row"),
                None => Vec::new(),
            }
        }
        // grid-template 단축(§CSS Grid):
        //   none | <rows> / <columns>
        //   | [ <line-names>? <string> <track-size>? <line-names>? ]+ [ / <explicit-track-list> ]?
        "grid-template" => {
            let v = value_text.trim();
            let low = v.to_ascii_lowercase();
            let three = |r: &str, c: &str, a: &str| {
                vec![
                    Declaration { important: false, name: "grid-template-rows".to_string(), value: Value::Keyword(r.to_string()) },
                    Declaration { important: false, name: "grid-template-columns".to_string(), value: Value::Keyword(c.to_string()) },
                    Declaration { important: false, name: "grid-template-areas".to_string(), value: Value::Keyword(a.to_string()) },
                ]
            };
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return three(&low, &low, &low);
            }
            match expand_grid_template_value(v) {
                Some((r, c, a)) => three(&r, &c, &a),
                None => Vec::new(),
            }
        }
        // z-index: 정수 → Length(n, Px) 로 보존 (paint 가 스택 레벨로 읽음). auto 는 드롭.
        // z-index: <integer> | auto. 직접 파싱 실패 시 수학 함수(abs/sign/round/…) 평가.
        "z-index" if value_text.trim().eq_ignore_ascii_case("auto") => {
            vec![Declaration { important: false, name: "z-index".to_string(), value: Value::Keyword("auto".to_string()) }]
        }
        // <integer> 만(소수 거부). 괄호가 있으면 calc 등 수학 함수로 평가.
        "z-index" if value_text.trim().parse::<i64>().is_ok() => {
            let n = value_text.trim().parse::<i64>().unwrap() as f32;
            vec![Declaration { important: false, name: "z-index".to_string(), value: Value::Length(n, Unit::Number) }]
        }
        "z-index" if value_text.contains('(') => match number_or_math(value_text) {
            Some(n) => vec![Declaration { important: false, name: "z-index".to_string(), value: Value::Length(n, Unit::Number) }],
            _ => Vec::new(),
        },
        "z-index" => Vec::new(),
        // font-weight 의 계산값은 수다(CSS Fonts §2.2). bold=700, normal=400.
        // 예전엔 "bold"/"normal" 키워드로 정규화해서 getComputedStyle 이 "bold" 를
        // 돌려줬다(표준은 "700"). 렌더는 600 이상을 굵게 그린다(폰트가 2종뿐).
        "font-weight" => {
            let v = value_text.trim().to_ascii_lowercase();
            // bolder/lighter 는 부모 계산 weight 기준 상대값 — 키워드로 보존하고
            // 스타일 계산(style.rs)이 부모 weight 로 해석한다(§CSS Fonts 2.2.1).
            if matches!(v.as_str(), "bolder" | "lighter") {
                return vec![Declaration { important: false, name: "font-weight".to_string(), value: Value::Keyword(v) }];
            }
            let n = match v.as_str() {
                "bold" => 700.0,
                "normal" => 400.0,
                "initial" => 400.0,
                // CSS-wide 키워드는 Keyword 로 보존 → 스타일 계산의 wide-keyword 해석이
                // 부모값으로 override 한다(요소가 자기 font-weight 를 가져도 inherit 이
                // 이긴다). 예전엔 Vec::new 라 자기 선언이 있으면 override 못 했다.
                "inherit" | "unset" | "revert" | "revert-layer" => {
                    return vec![Declaration {
                        important: false,
                        name: "font-weight".to_string(),
                        value: Value::Keyword(v.clone()),
                    }];
                }
                other => {
                    // 평수는 [1,1000] 이어야 유효. calc(…)는 interpret_value 로 평가해
                    // 범위 밖도 유효(used value 에서 클램프, §CSS Fonts).
                    if let Ok(n) = other.parse::<f32>() {
                        if (1.0..=1000.0).contains(&n) {
                            n
                        } else {
                            return Vec::new();
                        }
                    } else if other.ends_with(')') && other.contains('(') {
                        // 수학 함수 — 결과가 순수 <number> 여야(길이/%/각도 거부). 계산
                        // 불가여도 지정값 보존(used value 에서 클램프).
                        if super::values::math_number_only_valid(other) {
                            return vec![Declaration { important: false, name: "font-weight".to_string(), value: Value::Keyword(value_text.trim().to_string()) }];
                        } else {
                            return Vec::new();
                        }
                    } else {
                        return Vec::new();
                    }
                }
            };
            vec![Declaration {
                important: false,
                name: "font-weight".to_string(),
                value: Value::Length(n, Unit::Number),
            }]
        }
        // text-indent: <length-percentage> && hanging? && each-line?. 길이 토큰을 파싱해
        // Length 로(레이아웃이 읽음), hanging/each-line 키워드가 있어도 유효(계산값은 길이
        // 근사 — 키워드 별도 저장 안 함). 애니메이션 from/to 는 원문 문자열이라 interp 가
        // hanging 을 보존한다.
        // text-indent(§CSS Text): <length-percentage> && hanging? && each-line?.
        "text-indent" => {
            let vt = value_text.trim().to_ascii_lowercase();
            // CSS-wide 키워드는 모든 프로퍼티에서 유효 — Keyword 로 보존해 스타일 계산의
            // wide-keyword 해석(inherit→부모값 등)이 처리하게 한다(예전엔 거부→no-op 이라
            // text-indent: inherit 가 무시됐다).
            if matches!(vt.as_str(), "inherit" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: "text-indent".to_string(), value: Value::Keyword(vt) }];
            }
            if vt == "initial" {
                return vec![Declaration { important: false, name: "text-indent".to_string(), value: Value::Length(0.0, Unit::Px) }];
            }
            if !crate::css::text_indent_valid(value_text) {
                return Vec::new();
            }
            // 계산 가능한 길이는 Length(레이아웃), calc 등 계산 불가·키워드 조합은 지정값 보존.
            let toks: Vec<&str> = split_top_level(value_text.trim());
            let len = toks
                .iter()
                .find(|t| !matches!(t.to_ascii_lowercase().as_str(), "hanging" | "each-line"))
                .and_then(|t| interpret_value(t));
            match len {
                Some(v @ Value::Length(..)) if toks.len() == 1 => {
                    vec![Declaration { important: false, name: "text-indent".to_string(), value: v }]
                }
                _ => vec![Declaration { important: false, name: "text-indent".to_string(), value: Value::Keyword(value_text.trim().to_string()) }],
            }
        }
        // cursor(§CSS UI): [<url> [<x> <y>]?,]* <keyword>. 검증만(원문 보존). 무효
        // (잘못된 키워드/gradient 이미지/lengths 좌표 등) 거부. CSS-wide 통과.
        "cursor" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
            {
                return vec![Declaration { important: false, name: "cursor".to_string(), value: Value::Keyword(low) }];
            }
            if crate::css::cursor_valid(value_text) {
                vec![Declaration { important: false, name: "cursor".to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
            } else {
                Vec::new()
            }
        }
        // field-sizing(§CSS UI): fixed | content 만. 그 외 거부. CSS-wide 통과.
        "field-sizing" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(
                low.as_str(),
                "fixed" | "content" | "inherit" | "initial" | "unset" | "revert" | "revert-layer"
            ) {
                vec![Declaration { important: false, name: "field-sizing".to_string(), value: Value::Keyword(low) }]
            } else {
                Vec::new()
            }
        }
        // transition/animation-timing-function(§CSS Easing): <easing-function># 검증.
        // 무효(auto, cubic-bezier x 범위밖, steps 비정수 등) 거부. CSS-wide 통과.
        "transition-timing-function" | "animation-timing-function" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
            {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            if crate::css::timing_function_valid(value_text) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
            } else {
                Vec::new()
            }
        }
        // outline-offset(§CSS UI): <length> 단일(calc 포함). %·auto·단위없는 비영·
        // 두값·키워드 거부. CSS-wide 통과.
        "outline-offset" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
            {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            let toks = split_top_level(value_text.trim());
            if toks.len() == 1 {
                match interpret_value(toks[0]) {
                    Some(Value::Length(n, u)) => {
                        // % 는 <length> 아님, 단위없는 수는 0 만 허용(0→0px 로 캐논화).
                        if u == Unit::Number && n == 0.0 {
                            return vec![Declaration { important: false, name: name.to_string(), value: Value::Length(0.0, Unit::Px) }];
                        }
                        if u != Unit::Percent && u != Unit::Number {
                            return vec![Declaration { important: false, name: name.to_string(), value: Value::Length(n, u) }];
                        }
                    }
                    Some(v @ (Value::Calc(..) | Value::MinMax(..))) => {
                        return vec![Declaration { important: false, name: name.to_string(), value: v }];
                    }
                    _ => {}
                }
            }
            Vec::new()
        }
        // scrollbar-gutter(§CSS Overflow): auto | stable && both-edges?. 순서 무관 입력을
        // stable 먼저로 캐논화. auto+타값·force·both·길이 등 거부.
        "scrollbar-gutter" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
            {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            let toks = split_top_level(value_text.trim());
            let canon: Option<String> = match toks.as_slice() {
                [a] if a.eq_ignore_ascii_case("auto") => Some("auto".to_string()),
                [a] if a.eq_ignore_ascii_case("stable") => Some("stable".to_string()),
                [a, b]
                    if (a.eq_ignore_ascii_case("stable") && b.eq_ignore_ascii_case("both-edges"))
                        || (a.eq_ignore_ascii_case("both-edges") && b.eq_ignore_ascii_case("stable")) =>
                {
                    Some("stable both-edges".to_string())
                }
                _ => None,
            };
            match canon {
                Some(c) => vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(c) }],
                None => Vec::new(),
            }
        }
        // contain(§CSS Contain): none|strict|content | [[size|inline-size]||layout||
        // style||paint]. 혼합·중복·미인식 거부. 캐논 순서로 직렬화.
        "contain" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
            {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            if crate::css::contain_valid(value_text) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(crate::css::contain_canonical(value_text)) }]
            } else {
                Vec::new()
            }
        }
        // display(§CSS Display 3): 다값 문법 검증 + 캐논 직렬화(flow→block, 두값→레거시).
        // 저장은 캐논 형태(레이아웃이 레거시 단일 키워드를 이해). CSS-wide 통과.
        "display" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
            {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            if crate::css::display_valid(value_text) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(crate::css::display_canonical(value_text)) }]
            } else {
                Vec::new()
            }
        }
        // offset-path(§CSS Motion Path): none | <url> | <ray()> | [<basic-shape> || <coord-box>].
        "offset-path" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            if crate::css::offset_path_valid(value_text) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
            } else {
                Vec::new()
            }
        }
        // animation-range 단축/롱핸드(§scroll-animations): normal | <length-percentage> |
        // <range-name> <length-percentage>?. 단축은 start [end]. 시간·색·미지 이름 거부.
        "animation-range" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![
                    Declaration { important: false, name: "animation-range-start".to_string(), value: Value::Keyword(low.clone()) },
                    Declaration { important: false, name: "animation-range-end".to_string(), value: Value::Keyword(low) },
                ];
            }
            // 단축은 start/end 롱핸드로 전개(§CSSOM — 관련 롱핸드만 설정). 기본 오프셋
            // 캐논(start 0%, end 100% 생략).
            match crate::css::animation_range_expand(value_text) {
                Some((start, end)) => vec![
                    Declaration { important: false, name: "animation-range-start".to_string(), value: Value::Keyword(crate::css::animation_range_longhand_canonical(&start, false)) },
                    Declaration { important: false, name: "animation-range-end".to_string(), value: Value::Keyword(crate::css::animation_range_longhand_canonical(&end, true)) },
                ],
                None => Vec::new(),
            }
        }
        "animation-range-start" | "animation-range-end" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            if crate::css::animation_range_longhand_valid(value_text) {
                let is_end = name == "animation-range-end";
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(crate::css::animation_range_longhand_canonical(value_text, is_end)) }]
            } else {
                Vec::new()
            }
        }
        // offset-rotate(§CSS Motion Path): [ auto | reverse ] || <angle>. 키워드·각도 각
        // ≤1, 최소 하나. bare 0(단위 없음)·none·키워드 중복 거부. 캐논: 키워드 먼저.
        "offset-rotate" => match offset_rotate_canonical(value_text) {
            Some(canon) => vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(canon) }],
            None => Vec::new(),
        },
        // offset 단축(§CSS Motion Path): position? [path [distance || rotate]?]? [/ anchor]?.
        "offset" => return offset_shorthand(value_text),
        // offset-distance(§CSS Motion Path): <length-percentage>(부호 허용). 각도·키워드 거부.
        "offset-distance" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            let toks = split_top_level(value_text.trim());
            if toks.len() != 1
                || !(toks[0] == "0" || super::values::math_length_valid(&toks[0].to_ascii_lowercase(), true))
            {
                return Vec::new();
            }
            let v = match interpret_value(toks[0]) {
                Some(Value::Length(n, Unit::Number)) if n == 0.0 => Value::Length(0.0, Unit::Px),
                Some(other) => other,
                None => Value::Keyword(value_text.trim().to_string()),
            };
            return vec![Declaration { important: false, name: name.to_string(), value: v }];
        }
        // offset-position(auto|normal|<position>) / offset-anchor(auto|<position>).
        "offset-position" | "offset-anchor" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer" | "auto")
                || (name == "offset-position" && low == "normal")
            {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            if crate::css::position_valid(value_text.trim()) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
            } else {
                Vec::new()
            }
        }
        // interpolate-size(§CSS Values 5): numeric-only | allow-keywords.
        "interpolate-size" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(
                low.as_str(),
                "inherit" | "initial" | "unset" | "revert" | "revert-layer" | "numeric-only"
                    | "allow-keywords"
            ) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }]
            } else {
                Vec::new()
            }
        }
        // scroll-initial-target(§CSS Scroll Snap): none | nearest.
        "scroll-initial-target" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(
                low.as_str(),
                "inherit" | "initial" | "unset" | "revert" | "revert-layer" | "none" | "nearest"
            ) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }]
            } else {
                Vec::new()
            }
        }
        // overflow-anchor(§CSS Scroll Anchoring): auto | none 단일 키워드만.
        "overflow-anchor" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(
                low.as_str(),
                "inherit" | "initial" | "unset" | "revert" | "revert-layer" | "auto" | "none"
            ) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }]
            } else {
                Vec::new()
            }
        }
        // grid-auto-flow(§CSS Grid): [row | column] || dense. row(초기값) 생략 캐논.
        "grid-auto-flow" => match grid_auto_flow_canonical(value_text) {
            Some(canon) => vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(canon) }],
            None => Vec::new(),
        },
        // flex-line-count(§CSS Flexbox 자동 밸런싱): <integer [1,∞]>. auto·0·음수·
        // 소수·키워드 거부.
        "flex-line-count" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            if crate::css::positive_integer_valid(&low) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }]
            } else {
                Vec::new()
            }
        }
        // margin-trim(§CSS Box 4): none | <block축>? || <inline축>?. 각 축(block/
        // block-start/block-end 중 1, inline/inline-start/inline-end 중 1) 최대 하나.
        // 축 중복("block block", "block block-start")·미지 토큰·none 혼합 거부.
        "margin-trim" => match margin_trim_canonical(value_text) {
            Some(canon) => vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(canon) }],
            None => Vec::new(),
        },
        // content-visibility(§CSS Contain): visible | auto | hidden 단일 키워드만.
        "content-visibility" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(
                low.as_str(),
                "inherit" | "initial" | "unset" | "revert" | "revert-layer" | "visible" | "auto"
                    | "hidden"
            ) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }]
            } else {
                Vec::new()
            }
        }
        // accent-color(§CSS UI 4): auto | <color>. 다중값·무효 색 거부.
        "accent-color" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer" | "auto")
            {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            if crate::css::single_color_valid(value_text.trim()) {
                // 색은 Value 로 — serialize_decl 이 hex/함수 색을 rgb() 로 접고 명명·
                // currentcolor 는 키워드로 유지한다.
                let val = interpret_value(value_text.trim())
                    .unwrap_or_else(|| Value::Keyword(value_text.trim().to_string()));
                vec![Declaration { important: false, name: name.to_string(), value: val }]
            } else {
                Vec::new()
            }
        }
        // object-view-box(§CSS Images 5): none | <basic-shape-rect>.
        "object-view-box" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
            {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            if crate::css::object_view_box_valid(value_text.trim()) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
            } else {
                Vec::new()
            }
        }
        // 단일 키워드 enum UI 프로퍼티(§CSS UI 4 / §CSS Position). 정해진 키워드만 유효,
        // 그 외(미지 키워드·다중값·따옴표)는 거부. 계산값 = 지정 키워드.
        "caret-shape" | "input-security" | "overlay" | "caret-animation" => {
            let low = value_text.trim().to_ascii_lowercase();
            let wide = matches!(
                low.as_str(),
                "inherit" | "initial" | "unset" | "revert" | "revert-layer"
            );
            let ok = wide
                || match name {
                    "caret-shape" => matches!(low.as_str(), "auto" | "bar" | "block" | "underscore"),
                    "input-security" => matches!(low.as_str(), "auto" | "none"),
                    "overlay" => matches!(low.as_str(), "none" | "auto"),
                    "caret-animation" => matches!(low.as_str(), "auto" | "manual"),
                    _ => false,
                };
            if ok {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }]
            } else {
                Vec::new()
            }
        }
        // scrollbar-color(§CSS Scrollbars): auto | <color>{2}. 단일 색·혼합(auto+색)·
        // 3값은 무효. 직렬화(rgb 접기)는 serialize_decl 에서.
        "scrollbar-color" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer" | "auto")
            {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            let toks = crate::css::split_ws_depth0(value_text.trim());
            if toks.len() == 2 && toks.iter().all(|t| crate::css::single_color_valid(t.trim())) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
            } else {
                Vec::new()
            }
        }
        // scrollbar-width(§CSS Scrollbars): auto | thin | none 단일 키워드만.
        "scrollbar-width" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(
                low.as_str(),
                "inherit" | "initial" | "unset" | "revert" | "revert-layer" | "auto" | "thin"
                    | "none"
            ) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }]
            } else {
                Vec::new()
            }
        }
        // interactivity(§CSS UI 4): auto | inert 만.
        "interactivity" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(
                low.as_str(),
                "inherit" | "initial" | "unset" | "revert" | "revert-layer" | "auto" | "inert"
            ) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }]
            } else {
                Vec::new()
            }
        }
        // text-spacing-trim(§CSS Text 4): 단일 키워드. none/두값/allow-end/trim-auto 거부.
        "text-spacing-trim" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(
                low.as_str(),
                "inherit" | "initial" | "unset" | "revert" | "revert-layer" | "auto" | "normal"
                    | "space-all" | "space-first" | "trim-all" | "trim-both" | "trim-start"
            ) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }]
            } else {
                Vec::new()
            }
        }
        // word-break(§CSS Text): normal|keep-all|break-all|break-word|auto-phrase 단일.
        "word-break" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(
                low.as_str(),
                "inherit" | "initial" | "unset" | "revert" | "revert-layer" | "normal"
                    | "keep-all" | "break-all" | "break-word" | "auto-phrase" | "manual"
            ) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }]
            } else {
                Vec::new()
            }
        }
        // text-group-align(§CSS Text 4): none|start|end|left|right|center 단일.
        "text-group-align" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(
                low.as_str(),
                "inherit" | "initial" | "unset" | "revert" | "revert-layer" | "none" | "start"
                    | "end" | "left" | "right" | "center"
            ) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }]
            } else {
                Vec::new()
            }
        }
        // hanging-punctuation(§CSS Text): none | [first || [force-end|allow-end] || last].
        "hanging-punctuation" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
            {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            if crate::css::hanging_punctuation_valid(value_text) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
            } else {
                Vec::new()
            }
        }
        // text-autospace(§CSS Text 4): normal|auto|no-autospace | 스페이싱 그룹 || 삽입.
        "text-autospace" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
            {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            if crate::css::text_autospace_valid(value_text) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(crate::css::text_autospace_canonical(value_text)) }]
            } else {
                Vec::new()
            }
        }
        // text-spacing(§CSS Text 4): normal | none | auto | [<spacing-trim> || <autospace>]
        // 단축 → text-autospace + text-spacing-trim.
        "text-spacing" => match text_spacing_parse(value_text) {
            Some((a, t)) => vec![
                Declaration { important: false, name: "text-autospace".to_string(), value: Value::Keyword(a) },
                Declaration { important: false, name: "text-spacing-trim".to_string(), value: Value::Keyword(t) },
            ],
            None => Vec::new(),
        },
        // text-fit(§CSS Text 4): none | [grow|shrink] || [consistent|per-line|per-line-all]
        // || <percentage>. 계산값은 기본 consistent 생략. grow|shrink 필수(none 제외).
        "text-fit" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer" | "none") {
                return vec![Declaration { important: false, name: "text-fit".to_string(), value: Value::Keyword(low) }];
            }
            // 순차 문법: [grow|shrink] [consistent|per-line|per-line-all]? <percentage>?.
            let toks: Vec<&str> = low.split_whitespace().collect();
            let mut i = 0usize;
            let mode = match toks.first() {
                Some(&("grow" | "shrink")) => {
                    i += 1;
                    toks[0]
                }
                _ => return Vec::new(),
            };
            let mut line: Option<&str> = None;
            if matches!(toks.get(i), Some(&("consistent" | "per-line" | "per-line-all"))) {
                line = Some(toks[i]);
                i += 1;
            }
            let mut pct: Option<String> = None;
            if let Some(t) = toks.get(i) {
                match t.strip_suffix('%') {
                    Some(p) if p.parse::<f32>().is_ok() => {
                        pct = Some(t.to_string());
                        i += 1;
                    }
                    _ => {}
                }
            }
            if i != toks.len() {
                return Vec::new(); // 남은/잘못된 순서 토큰
            }
            let mut parts: Vec<String> = vec![mode.to_string()];
            if let Some(l) = line {
                if l != "consistent" {
                    parts.push(l.to_string());
                }
            }
            if let Some(p) = pct {
                parts.push(p);
            }
            vec![Declaration { important: false, name: "text-fit".to_string(), value: Value::Keyword(parts.join(" ")) }]
        }
        // overflow-clip-margin(§CSS Overflow 4): <visual-box> || <length [0,∞]>.
        "overflow-clip-margin" => match overflow_clip_margin_canonical(value_text) {
            Some(c) => vec![Declaration { important: false, name: "overflow-clip-margin".to_string(), value: Value::Keyword(c) }],
            None => Vec::new(),
        },
        // text-wrap-mode(§CSS Text 4): wrap | nowrap 만. 그 외(auto/normal/balance/
        // pretty/두값 등) 거부. CSS-wide 통과.
        "text-wrap-mode" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(
                low.as_str(),
                "inherit" | "initial" | "unset" | "revert" | "revert-layer" | "wrap" | "nowrap"
            ) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }]
            } else {
                Vec::new()
            }
        }
        // text-wrap-style(§CSS Text 4): auto | balance | stable | pretty 만.
        "text-wrap-style" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(
                low.as_str(),
                "inherit" | "initial" | "unset" | "revert" | "revert-layer" | "auto" | "balance"
                    | "stable" | "pretty"
            ) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }]
            } else {
                Vec::new()
            }
        }
        // text-transform(§CSS Text): none | [capitalize|uppercase|lowercase] || full-width
        // || full-size-kana, 또는 math-auto 단독. 카테고리 중복·none/math-auto 혼합 거부.
        "text-transform" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
            {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            if crate::css::text_transform_valid(value_text) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(crate::css::text_transform_canonical(value_text)) }]
            } else {
                Vec::new()
            }
        }
        // font-style(§CSS Fonts): normal | italic | oblique [<angle -90~90deg>]. italic+
        // 각도, 범위밖·단위없는·잘못된 각도, oblique 뒤 잉여 토큰 거부. CSS-wide 통과.
        "font-style" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
            {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            if crate::css::font_style_valid(value_text) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
            } else {
                Vec::new()
            }
        }
        // font-variant 단축(§CSS Fonts): 하위 카테고리 충돌·중복·미인식 토큰 거부.
        // normal/none 단독만 허용. 유효값은 원문 보존(현행 동작 유지). CSS-wide 통과.
        "font-variant" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
            {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            if crate::css::font_variant_valid(value_text) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
            } else {
                Vec::new()
            }
        }
        // transition-property(§CSS Transitions): none | <custom-ident>#. 항목별 유효
        // 식별자 검증(none/CSS-wide/default 항목 거부). CSS-wide 전체값은 통과.
        "transition-property" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
            {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            if crate::css::transition_property_valid(value_text) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
            } else {
                Vec::new()
            }
        }
        // transition-duration/delay(§CSS Transitions): <time>#. duration 은 음수 거부,
        // delay 는 음수 허용. 단위 없는 0/infinite/공백구분 목록 거부. CSS-wide 통과.
        "transition-duration" | "transition-delay" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
            {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            let allow_neg = name == "transition-delay";
            if crate::css::time_list_valid(value_text, allow_neg) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
            } else {
                Vec::new()
            }
        }
        // box-sizing(§CSS Sizing): content-box | border-box 만. 그 외(auto/fill-box/
        // margin-box/두값 등) 거부. CSS-wide 통과.
        "box-sizing" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(
                low.as_str(),
                "content-box"
                    | "border-box"
                    | "inherit"
                    | "initial"
                    | "unset"
                    | "revert"
                    | "revert-layer"
            ) {
                vec![Declaration { important: false, name: "box-sizing".to_string(), value: Value::Keyword(low) }]
            } else {
                Vec::new()
            }
        }
        // caret-color(§CSS UI): [ auto | <color> ]{1,2}. 무효 거부. 단일 <color>는
        // Color 로 저장(기존 색 보간 경로 유지 — 회귀 방지), 단일 auto/currentcolor 와
        // 두값 폼은 원문 Keyword(window 가 currentColor 해석). CSS-wide 통과.
        "caret-color" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
            {
                return vec![Declaration { important: false, name: "caret-color".to_string(), value: Value::Keyword(low) }];
            }
            if !crate::css::caret_color_valid(value_text) {
                return Vec::new();
            }
            let toks = split_top_level(value_text.trim());
            let value = if toks.len() == 1 {
                match interpret_value(toks[0]) {
                    Some(v @ (Value::Color(_) | Value::ColorFn(..))) => v,
                    _ => Value::Keyword(value_text.trim().to_string()),
                }
            } else {
                Value::Keyword(value_text.trim().to_string())
            };
            vec![Declaration { important: false, name: "caret-color".to_string(), value }]
        }
        // font-size-adjust(§CSS Fonts 5): none | [metric]? [from-font | <number>].
        // 검증·캐논(기본 ex-height 생략, calc 평가). 무효 거부. CSS-wide 통과.
        "font-size-adjust" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
            {
                return vec![Declaration { important: false, name: "font-size-adjust".to_string(), value: Value::Keyword(low) }];
            }
            match crate::css::normalize_font_size_adjust(value_text) {
                Some(norm) => vec![Declaration { important: false, name: "font-size-adjust".to_string(), value: Value::Keyword(norm) }],
                None => Vec::new(),
            }
        }
        // font-width 는 font-stretch 의 신명칭(§CSS Fonts 4) — 같은 값으로 파싱(별칭).
        // 계산값 노출은 window 가 font-stretch→font-width 미러링.
        "font-width" => expand_declaration("font-stretch", value_text),
        // font-stretch(§CSS Fonts 4): normal | <keyword> | <percentage 0+>. 검증 후 원문 보존.
        "font-stretch" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
                || crate::css::font_stretch_valid(value_text)
            {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
            } else {
                Vec::new()
            }
        }
        // font-variant-alternates(§CSS Fonts 4): 함수형. 검증 후 원문 보존
        // (custom-ident 대소문자 보존 위해 소문자화하지 않음).
        "font-variant-alternates" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }]
            } else if crate::css::font_variant_alternates_valid(value_text) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
            } else {
                Vec::new()
            }
        }
        // font-variant-numeric/east-asian(§CSS Fonts 4): 그룹형. 검증 후 원문 보존.
        "font-variant-numeric" | "font-variant-east-asian" => {
            let low = value_text.trim().to_ascii_lowercase();
            let ok = matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
                || (name == "font-variant-numeric" && crate::css::font_variant_numeric_valid(value_text))
                || (name == "font-variant-east-asian" && crate::css::font_variant_east_asian_valid(value_text));
            if ok {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }]
            } else {
                Vec::new()
            }
        }
        // font-synthesis 롱핸드(§CSS Fonts 4): weight/small-caps/position 은 auto|none,
        // style 은 auto|none|oblique-only. 단일 키워드.
        "font-synthesis-weight" | "font-synthesis-small-caps" | "font-synthesis-position"
        | "font-synthesis-style" => {
            let low = value_text.trim().to_ascii_lowercase();
            let ok = matches!(low.as_str(), "auto" | "none" | "inherit" | "initial" | "unset" | "revert" | "revert-layer")
                || (name == "font-synthesis-style" && low == "oblique-only");
            if ok {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }]
            } else {
                Vec::new()
            }
        }
        // font-synthesis(§CSS Fonts 4): none | [ weight || [style|oblique-only] ||
        // small-caps || position ] → 네 롱핸드로 전개.
        "font-synthesis" => {
            let low = value_text.trim().to_ascii_lowercase();
            let d = |n: &str, v: &str| Declaration { important: false, name: n.to_string(), value: Value::Keyword(v.to_string()) };
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![
                    d("font-synthesis-weight", &low),
                    d("font-synthesis-style", &low),
                    d("font-synthesis-small-caps", &low),
                    d("font-synthesis-position", &low),
                ];
            }
            if !crate::css::font_synthesis_valid(value_text) {
                return Vec::new();
            }
            let toks: Vec<&str> = low.split_whitespace().collect();
            let weight = if toks.contains(&"weight") { "auto" } else { "none" };
            let style = if toks.contains(&"style") {
                "auto"
            } else if toks.contains(&"oblique-only") {
                "oblique-only"
            } else {
                "none"
            };
            let small_caps = if toks.contains(&"small-caps") { "auto" } else { "none" };
            let position = if toks.contains(&"position") { "auto" } else { "none" };
            vec![
                d("font-synthesis-weight", weight),
                d("font-synthesis-style", style),
                d("font-synthesis-small-caps", small_caps),
                d("font-synthesis-position", position),
            ]
        }
        // font-variant-emoji(§CSS Fonts 4): normal | text | emoji | unicode.
        "font-variant-emoji" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
                || crate::css::font_variant_emoji_valid(value_text)
            {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }]
            } else {
                Vec::new()
            }
        }
        // font-variation-settings(§CSS Fonts 4): normal | [ <opentype-tag> <number> ]#.
        "font-variation-settings" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }]
            } else if crate::css::font_variation_settings_valid(value_text) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(crate::css::font_variation_settings_canonical(value_text)) }]
            } else {
                Vec::new()
            }
        }
        // text-wrap 단축(§CSS Text 4): text-wrap-mode || text-wrap-style. 캐논 직렬화로
        // 저장(무효값 거부). CSS-wide 키워드는 통과.
        "text-wrap" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(
                low.as_str(),
                "initial" | "inherit" | "unset" | "revert" | "revert-layer"
            ) {
                return vec![Declaration { important: false, name: "text-wrap".to_string(), value: Value::Keyword(low) }];
            }
            match crate::css::normalize_text_wrap(value_text) {
                Some(norm) => vec![Declaration { important: false, name: "text-wrap".to_string(), value: Value::Keyword(norm) }],
                None => Vec::new(),
            }
        }
        // white-space 단축(§CSS Text 4): white-space-collapse || text-wrap-mode.
        // normalize_white_space 로 검증·캐논화(무효값 balance 등은 거부→빈 선언).
        // CSS-wide 키워드는 통과(상속 처리는 스타일 계산이 담당).
        "white-space" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(
                low.as_str(),
                "initial" | "inherit" | "unset" | "revert" | "revert-layer"
            ) {
                return vec![Declaration { important: false, name: "white-space".to_string(), value: Value::Keyword(low) }];
            }
            match crate::css::normalize_white_space(value_text) {
                Some(norm) => vec![Declaration { important: false, name: "white-space".to_string(), value: Value::Keyword(norm) }],
                None => Vec::new(),
            }
        }
        // order: 정수(음수 가능). 단위 없는 수다.
        // order(§CSS Flexbox): <integer>. 비정수(123.45)·auto·다값 거부.
        "order" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: "order".to_string(), value: Value::Keyword(low) }];
            }
            match number_or_math(value_text) {
                Some(n) if n.fract() == 0.0 && n.is_finite() => vec![Declaration { important: false, name: "order".to_string(), value: Value::Length(n, Unit::Number) }],
                _ => Vec::new(),
            }
        }
        // flex-grow/flex-shrink: <number [0,∞]>(단위 없음). 음수·미인식 거부.
        "flex-grow" | "flex-shrink" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            // <number [0,∞]>: 엄격(단위·공백·후행점·이중부호·불량지수 거부).
            let v = value_text.trim();
            let vlow = v.to_ascii_lowercase();
            let is_math = vlow.ends_with(')')
                && ["calc(", "min(", "max(", "clamp(", "round(", "mod(", "rem(", "sign(", "abs("].iter().any(|p| vlow.starts_with(p));
            let strict_num = !v.contains(char::is_whitespace) && !v.ends_with('.') && v.parse::<f32>().is_ok();
            match number_or_math(value_text) {
                Some(n) if n >= 0.0 && (is_math || strict_num) => vec![Declaration { important: false, name: name.to_string(), value: Value::Length(n, Unit::Number) }],
                _ => Vec::new(),
            }
        }
        // contain-intrinsic-size(§CSS Sizing 4): [auto? [none|<length>]]{1,2} → width/height.
        "contain-intrinsic-size" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![
                    Declaration { important: false, name: "contain-intrinsic-width".to_string(), value: Value::Keyword(low.clone()) },
                    Declaration { important: false, name: "contain-intrinsic-height".to_string(), value: Value::Keyword(low) },
                ];
            }
            if !crate::css::contain_intrinsic_valid(value_text, 2) {
                return Vec::new();
            }
            // 그룹 분할: 각 그룹 = auto? (none|<length>).
            let toks: Vec<&str> = split_top_level(value_text.trim());
            let mut groups: Vec<String> = Vec::new();
            let mut i = 0;
            while i < toks.len() {
                let mut g = String::new();
                if toks[i].eq_ignore_ascii_case("auto") {
                    g.push_str("auto ");
                    i += 1;
                }
                g.push_str(toks[i]);
                i += 1;
                groups.push(g);
            }
            let w = groups[0].clone();
            let h = groups.get(1).cloned().unwrap_or_else(|| w.clone());
            return vec![
                Declaration { important: false, name: "contain-intrinsic-width".to_string(), value: Value::Keyword(w) },
                Declaration { important: false, name: "contain-intrinsic-height".to_string(), value: Value::Keyword(h) },
            ];
        }
        "contain-intrinsic-width" | "contain-intrinsic-height"
        | "contain-intrinsic-inline-size" | "contain-intrinsic-block-size" => {
            let phys = match name {
                "contain-intrinsic-inline-size" => "contain-intrinsic-width",
                "contain-intrinsic-block-size" => "contain-intrinsic-height",
                other => other,
            };
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: phys.to_string(), value: Value::Keyword(low) }];
            }
            if crate::css::contain_intrinsic_valid(value_text, 1) {
                return vec![Declaration { important: false, name: phys.to_string(), value: Value::Keyword(low) }];
            }
            return Vec::new();
        }
        // mask-position(§CSS Masking): <position>#(콤마 목록). 각 레이어를 검증.
        "mask-position" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            let layers = split_top_level_commas(value_text);
            if !layers.is_empty() && layers.iter().all(|l| crate::css::position_valid(l)) {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }];
            }
            return Vec::new();
        }
        // corner 단축 계열(§CSS Borders 4): 단일 코너 값을 "/"로 구분. corner/corners=4,
        // 변(top/bottom/left/right)·축(block/inline)=2.
        "corner"
        | "corner-top" | "corner-bottom" | "corner-left" | "corner-right"
        | "corner-block" | "corner-inline"
        | "corner-block-start" | "corner-block-end" | "corner-inline-start" | "corner-inline-end" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            let max = if name == "corner" { 4 } else { 2 };
            if crate::css::corner_slash_valid(value_text, max) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
            } else {
                Vec::new()
            }
        }
        // 단일 코너(§CSS Borders 4): normal | [<corner-shape> && <length-percentage>{1,2}].
        "corner-top-left" | "corner-top-right" | "corner-bottom-left" | "corner-bottom-right" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            if crate::css::corner_single_valid(value_text) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
            } else {
                Vec::new()
            }
        }
        // corner-shape 계열(§CSS Borders 4): [<corner-shape-value>]{1,N}. N=4(corner-shape),
        // 2(변/논리 축 단축), 1(단일 코너 롱핸드). 유효값 원문 보존.
        "corner-shape"
        | "corner-top-shape" | "corner-bottom-shape" | "corner-left-shape" | "corner-right-shape"
        | "corner-block-shape" | "corner-inline-shape"
        | "corner-top-left-shape" | "corner-top-right-shape" | "corner-bottom-left-shape"
        | "corner-bottom-right-shape" | "corner-block-start-shape" | "corner-block-end-shape"
        | "corner-inline-start-shape" | "corner-inline-end-shape" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            let max = match name {
                "corner-shape" => 4,
                "corner-top-left-shape" | "corner-top-right-shape" | "corner-bottom-left-shape"
                | "corner-bottom-right-shape" => 1,
                _ => 2, // 변(top/bottom/left/right)·논리 축·논리 모서리 단축은 2코너.
            };
            if crate::css::corner_shape_list_valid(value_text, max) {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(crate::css::corner_shape_canonical(value_text)) }];
            }
            return Vec::new();
        }
        // scale/translate(§CSS Transforms 2): 검증만, 유효값 원문 보존(캐논은 별개).
        "scale" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            if crate::css::scale_valid(value_text) {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }];
            }
            return Vec::new();
        }
        "translate" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            if crate::css::translate_valid(value_text) {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }];
            }
            return Vec::new();
        }
        // rotate(§CSS Transforms 2): none | <angle> | [x|y|z|<number>{3}] && <angle>.
        // 캐논 직렬화(축벡터 단순화)는 별개 — 검증만, 유효값 원문 보존.
        "rotate" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            if crate::css::rotate_valid(value_text) {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }];
            }
            return Vec::new();
        }
        // image-orientation(§CSS Images 3): from-image | none 만(각도/flip 형태는 폐기).
        "image-orientation" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(
                low.as_str(),
                "inherit" | "initial" | "unset" | "revert" | "revert-layer" | "from-image" | "none"
            ) {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            return Vec::new();
        }
        // aspect-ratio(§CSS Sizing): auto || <ratio>. 무효(auto/, 단위, 음수, 공백구분) 거부.
        "aspect-ratio" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            if crate::css::aspect_ratio_valid(value_text) {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(crate::css::aspect_ratio_canonical(value_text)) }];
            }
            return Vec::new();
        }
        // 크기 프로퍼티(§CSS Sizing): auto|none|<length-percentage 0+>|min/max/fit-content.
        // width/height/min-* 는 auto, max-* 는 none. 유효값은 interpret_value 저장(레이아웃 불변).
        // border-*-width 롱핸드(§CSS Backgrounds): <line-width>. thin/medium/thick 또는
        // <length>(% 불가). 무검증 catch-all 로 새던 max(0Hz)/max()/max(0%) 등을 거부.
        "border-top-width" | "border-right-width" | "border-bottom-width" | "border-left-width" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            let toks = split_top_level(value_text.trim());
            if toks.len() != 1 || !is_line_width_tok(toks[0]) {
                return Vec::new();
            }
            let v = interpret_value(toks[0]).unwrap_or(Value::Keyword(low));
            return vec![Declaration { important: false, name: name.to_string(), value: v }];
        }
        "width" | "height" | "min-width" | "min-height" | "max-width" | "max-height" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            let is_max = name.starts_with("max-");
            let toks = split_top_level(value_text.trim());
            // anchor-size()(§css-anchor-1): sizing 프로퍼티에서 유효. 캐논 직렬화로 저장.
            if toks.len() == 1 {
                if let Some(canon) = crate::css::anchor_size_canonical(toks[0]) {
                    return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(canon) }];
                }
            }
            if toks.len() != 1 || !crate::css::size_valid(toks[0], is_max, !is_max) {
                return Vec::new();
            }
            let v = match interpret_value(toks[0]) {
                Some(Value::Length(n, Unit::Number)) if n == 0.0 => Value::Length(0.0, Unit::Px),
                Some(other) => other,
                None => Value::Keyword(low),
            };
            return vec![Declaration { important: false, name: name.to_string(), value: v }];
        }
        // 정렬 프로퍼티(§CSS Box Alignment): 축(content/self)·auto·left·right·legacy 별 문법.
        "align-content" => return align_arm(name, value_text, true, false, false, false, true),
        "justify-content" => return align_arm(name, value_text, true, false, true, false, false),
        "align-items" => return align_arm(name, value_text, false, false, false, false, true),
        "justify-items" => return align_arm(name, value_text, false, false, true, true, true),
        "align-self" => return align_arm(name, value_text, false, true, false, false, true),
        "justify-self" => return align_arm(name, value_text, false, true, true, false, true),
        // flex-direction/flex-wrap(§CSS Flexbox): 단일 키워드. 미인식·두값 거부.
        "flex-direction" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(
                low.as_str(),
                "inherit" | "initial" | "unset" | "revert" | "revert-layer" | "row"
                    | "row-reverse" | "column" | "column-reverse"
            ) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }]
            } else {
                Vec::new()
            }
        }
        "flex-wrap" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(
                low.as_str(),
                "inherit" | "initial" | "unset" | "revert" | "revert-layer" | "nowrap" | "wrap"
                    | "wrap-reverse"
            ) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }]
            } else {
                Vec::new()
            }
        }
        // flex-basis(§CSS Flexbox): content | <'width'>(auto|<length-percentage 0+>|
        // min/max/fit-content). none·음수·두값·anchor-size 거부.
        "flex-basis" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            let toks = split_top_level(value_text.trim());
            if toks.len() == 1 && crate::css::flex_basis_valid(toks[0]) {
                return vec![Declaration { important: false, name: name.to_string(), value: parse_flex_basis(&low) }];
            }
            return Vec::new();
        }
        // flex 단축: <grow> [<shrink>] [<basis>]. 키워드: none=0 0 auto, auto=1 1 auto,
        // initial=0 1 auto. 숫자 하나(flex:1)=1 1 0% (등폭 핵심), 길이 하나=1 1 <len>.
        // flex 단축(§CSS Flexbox): none | [<'flex-grow'> <'flex-shrink'>? || <'flex-basis'>].
        // grow/shrink 는 <number>(단위 없음), basis 는 flex_basis_valid. 잘못된 구조·음수·
        // 3숫자·2 basis·none 혼합 거부. flex:1 = 1 1 0%.
        "flex" => {
            let v = value_text.trim();
            let low = v.to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return ["flex-grow", "flex-shrink", "flex-basis"]
                    .iter()
                    .map(|n| Declaration { important: false, name: n.to_string(), value: Value::Keyword(low.clone()) })
                    .collect();
            }
            let (grow, shrink, basis): (f32, f32, Value) = if low == "none" {
                (0.0, 0.0, Value::Keyword("auto".to_string()))
            } else {
                let mut nums: Vec<f32> = Vec::new();
                let mut basis: Option<Value> = None;
                for t in split_top_level(v) {
                    if let Ok(num) = t.parse::<f32>() {
                        if num < 0.0 || !num.is_finite() {
                            return Vec::new(); // grow/shrink 음수 불가
                        }
                        nums.push(num);
                    } else if crate::css::flex_basis_valid(t) {
                        if basis.is_some() {
                            return Vec::new(); // basis 는 하나만
                        }
                        basis = Some(parse_flex_basis(t));
                    } else {
                        return Vec::new(); // 미인식 토큰(none 혼합 포함)
                    }
                }
                if nums.len() > 2 || (nums.is_empty() && basis.is_none()) {
                    return Vec::new();
                }
                let grow = nums.first().copied().unwrap_or(1.0);
                let shrink = nums.get(1).copied().unwrap_or(1.0);
                // 숫자만 있고 basis 없으면 0%, 아무것도 없으면 auto(basis 만 있는 경우).
                let basis = basis.unwrap_or(Value::Length(0.0, Unit::Percent));
                (grow, shrink, basis)
            };
            vec![
                Declaration { important: false, name: "flex-grow".to_string(), value: Value::Length(grow, Unit::Number) },
                Declaration { important: false, name: "flex-shrink".to_string(), value: Value::Length(shrink, Unit::Number) },
                Declaration { important: false, name: "flex-basis".to_string(), value: basis },
            ]
        }
        // grid-auto-columns/rows(§CSS Grid): <track-size>+. 검증만, 유효 시 원문 보존.
        "grid-auto-columns" | "grid-auto-rows" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
                || crate::css::grid_auto_track_valid(value_text)
            {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
            } else {
                Vec::new()
            }
        }
        // grid-template-areas 는 <string>+ 문법 → 원문 보존, 레이아웃이 파싱.
        // grid-template-areas(§CSS Grid): none | <string>+. 모든 문자열은 같은(0 아닌)
        // 열 수, 각 셀은 <custom-ident> 또는 null(점만). 빈 문자열·열 불일치·none 혼합·
        // 비문자열 토큰 거부.
        "grid-template-areas" => match grid_template_areas_canonical(value_text) {
            Some(canon) => vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(canon) }],
            None => Vec::new(),
        },
        // grid-template-columns/rows(§CSS Grid): none | <track-list> | <auto-track-list>.
        // 검증만 하고 유효하면 원문 보존(레이아웃이 파싱). CSS-wide 는 통과.
        "grid-template-columns" | "grid-template-rows" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
                || crate::css::grid_template_track_valid(value_text)
            {
                let canon = if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                    low
                } else {
                    crate::css::grid_template_track_canonical(value_text)
                };
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(canon) }]
            } else {
                Vec::new()
            }
        }
        // grid-row/grid-column(§CSS Grid): <grid-line> [/ <grid-line>]?.
        // start = 첫 줄, end = 둘째 줄. 슬래시 없으면 첫 줄이 순수 custom-ident 면
        // 복사, 아니면 auto.
        "grid-row" | "grid-column" => {
            let axis = name.strip_prefix("grid-").unwrap();
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![
                    Declaration { important: false, name: format!("grid-{axis}-start"), value: Value::Keyword(low.clone()) },
                    Declaration { important: false, name: format!("grid-{axis}-end"), value: Value::Keyword(low) },
                ];
            }
            if !crate::css::grid_line_shorthand_valid(value_text) {
                return Vec::new();
            }
            let parts = split_top_slash_pub(value_text);
            let start = crate::css::grid_line_canonical(parts[0].trim());
            let end = match parts.get(1) {
                Some(p) => crate::css::grid_line_canonical(p.trim()),
                None if grid_line_is_bare_ident(&start) => start.clone(),
                None => "auto".to_string(),
            };
            let d = |n: String, v: String| Declaration { important: false, name: n, value: Value::Keyword(v) };
            vec![d(format!("grid-{axis}-start"), start), d(format!("grid-{axis}-end"), end)]
        }
        // grid-line 롱핸드(§CSS Grid): 단일 <grid-line> 검증 + 캐논.
        "grid-row-start" | "grid-row-end" | "grid-column-start" | "grid-column-end" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            if !crate::css::grid_line_valid(value_text.trim()) {
                return Vec::new();
            }
            vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(crate::css::grid_line_canonical(value_text.trim())) }]
        }
        // grid-area(§CSS Grid): <grid-line> [/ <grid-line>]{0,3} →
        // row-start / column-start / row-end / column-end.
        "grid-area" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                let d = |n: &str| Declaration { important: false, name: n.to_string(), value: Value::Keyword(low.clone()) };
                return vec![d("grid-row-start"), d("grid-column-start"), d("grid-row-end"), d("grid-column-end")];
            }
            if !crate::css::grid_area_valid(value_text) {
                return Vec::new();
            }
            let parts: Vec<String> = split_top_slash_pub(value_text)
                .iter()
                .map(|p| crate::css::grid_line_canonical(p.trim()))
                .collect();
            let row_start = parts[0].clone();
            let col_start = parts.get(1).cloned().unwrap_or_else(|| {
                if grid_line_is_bare_ident(&row_start) { row_start.clone() } else { "auto".to_string() }
            });
            let row_end = parts.get(2).cloned().unwrap_or_else(|| {
                if grid_line_is_bare_ident(&row_start) { row_start.clone() } else { "auto".to_string() }
            });
            let col_end = parts.get(3).cloned().unwrap_or_else(|| {
                if grid_line_is_bare_ident(&col_start) { col_start.clone() } else { "auto".to_string() }
            });
            let d = |n: &str, v: String| Declaration { important: false, name: n.to_string(), value: Value::Keyword(v) };
            vec![
                d("grid-row-start", row_start),
                d("grid-column-start", col_start),
                d("grid-row-end", row_end),
                d("grid-column-end", col_end),
            ]
        }
        // place-* 단축: <align> [<justify>] → align-*/justify-* longhand
        // place-items/place-content/place-self(§CSS Box Alignment): <align> <justify>?.
        "place-items" => place_shorthand("items", value_text, (false, false, false, false, true), (false, false, true, true, true)),
        "place-content" => place_shorthand("content", value_text, (true, false, false, false, true), (true, false, true, false, false)),
        "place-self" => place_shorthand("self", value_text, (false, true, false, false, true), (false, true, true, false, true)),
        // grid-gap 은 gap 의 레거시 별칭
        "grid-gap" | "grid-column-gap" | "grid-row-gap" => {
            let mapped = name.strip_prefix("grid-").unwrap();
            expand_declaration(mapped, value_text)
        }
        // gap: <row-gap> [<column-gap>]. 값이 둘이면 일반 값 파서가 None 을 돌려주고
        // 선언이 통째로 사라져서 간격이 0 이 됐다. longhand 로 쪼갠다.
        // 한 값이어도 longhand 를 함께 내보내 소비자가 어느 쪽을 읽든 맞게 한다.
        // gap(§CSS Box Alignment): <row-gap> [<column-gap>]. 각 normal|<length-percentage
        // 0+>. 음수·단위없는·none·max-content·3값·슬래시 거부.
        "gap" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![
                    Declaration { important: false, name: "row-gap".to_string(), value: Value::Keyword(low.clone()) },
                    Declaration { important: false, name: "column-gap".to_string(), value: Value::Keyword(low) },
                ];
            }
            let toks = split_top_level(value_text.trim());
            if toks.is_empty() || toks.len() > 2 || !toks.iter().all(|t| crate::css::gap_value_valid(t)) {
                return Vec::new();
            }
            let r = toks[0];
            let c = toks.get(1).copied().unwrap_or(r);
            let mut out = expand_declaration("row-gap", r);
            out.extend(expand_declaration("column-gap", c));
            out
        }
        // row-gap/column-gap(§CSS Box Alignment): normal | <length-percentage 0+>.
        "row-gap" | "column-gap" | "grid-row-gap" | "grid-column-gap" => {
            let phys = name.strip_prefix("grid-").unwrap_or(name);
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: phys.to_string(), value: Value::Keyword(low) }];
            }
            let toks = split_top_level(value_text.trim());
            if toks.len() != 1 || !crate::css::gap_value_valid(toks[0]) {
                return Vec::new();
            }
            if toks[0].eq_ignore_ascii_case("normal") {
                return vec![Declaration { important: false, name: phys.to_string(), value: Value::Keyword("normal".to_string()) }];
            }
            match interpret_value(toks[0]) {
                Some(v) => vec![Declaration { important: false, name: phys.to_string(), value: v }],
                None => Vec::new(),
            }
        }
        // overflow: <x> [<y>] (CSS Overflow §3). 두 값이면 선언이 사라져 visible 이 됐다.
        // overflow-x/overflow-y(§CSS Overflow): 단일 키워드만. 두값·미인식 거부.
        // overflow-block/inline 은 논리 프로퍼티 — 수평 쓰기모드 기준 물리축에 매핑.
        "overflow-x" | "overflow-y" | "overflow-block" | "overflow-inline" => {
            let low = value_text.trim().to_ascii_lowercase();
            let phys = match name {
                "overflow-block" => "overflow-y",
                "overflow-inline" => "overflow-x",
                other => other,
            };
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
            {
                return vec![Declaration { important: false, name: phys.to_string(), value: Value::Keyword(low) }];
            }
            if matches!(low.as_str(), "visible" | "hidden" | "clip" | "scroll" | "auto") {
                return vec![Declaration { important: false, name: phys.to_string(), value: Value::Keyword(low) }];
            }
            return Vec::new();
        }
        // overflow 단축(§CSS Overflow): overflow-x || overflow-y. 단일값은 양축에.
        // 유효 키워드(visible|hidden|clip|scroll|auto)만, 그 외·3값 이상 거부.
        "overflow" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
            {
                return vec![
                    Declaration { important: false, name: "overflow-x".to_string(), value: Value::Keyword(low.clone()) },
                    Declaration { important: false, name: "overflow-y".to_string(), value: Value::Keyword(low) },
                ];
            }
            let toks = split_top_level(value_text.trim());
            let valid =
                |t: &str| matches!(t.to_ascii_lowercase().as_str(), "visible" | "hidden" | "clip" | "scroll" | "auto");
            let (x, y) = match toks.as_slice() {
                [a] if valid(a) => (a.to_ascii_lowercase(), a.to_ascii_lowercase()),
                [a, b] if valid(a) && valid(b) => (a.to_ascii_lowercase(), b.to_ascii_lowercase()),
                _ => return Vec::new(),
            };
            vec![
                Declaration { important: false, name: "overflow-x".to_string(), value: Value::Keyword(x) },
                Declaration { important: false, name: "overflow-y".to_string(), value: Value::Keyword(y) },
            ]
        }
        // flex-flow: <flex-direction> || <flex-wrap> (순서 무관). 아예 미구현이었다.
        // flex-flow(§CSS Flexbox): <flex-direction> || <flex-wrap>. 각 최대 1회, 미인식·
        // 중복 거부. flex-direction/flex-wrap 롱핸드로 전개.
        "flex-flow" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![
                    Declaration { important: false, name: "flex-direction".to_string(), value: Value::Keyword(low.clone()) },
                    Declaration { important: false, name: "flex-wrap".to_string(), value: Value::Keyword(low) },
                ];
            }
            let (mut dir, mut wrap): (Option<String>, Option<String>) = (None, None);
            for t in split_top_level(value_text) {
                let lower = t.to_ascii_lowercase();
                match lower.as_str() {
                    "row" | "row-reverse" | "column" | "column-reverse" => {
                        if dir.is_some() {
                            return Vec::new();
                        }
                        dir = Some(lower);
                    }
                    "nowrap" | "wrap" | "wrap-reverse" => {
                        if wrap.is_some() {
                            return Vec::new();
                        }
                        wrap = Some(lower);
                    }
                    _ => return Vec::new(),
                }
            }
            if dir.is_none() && wrap.is_none() {
                return Vec::new();
            }
            let mut out = Vec::new();
            if let Some(d) = dir {
                out.push(Declaration { important: false, name: "flex-direction".to_string(), value: Value::Keyword(d) });
            }
            if let Some(w) = wrap {
                out.push(Declaration { important: false, name: "flex-wrap".to_string(), value: Value::Keyword(w) });
            }
            out
        }
        // border-spacing: <h> [<v>]. 두 값 원문 보존 (레이아웃이 이미 두 값을 읽는다).
        // background-size: cover | contain | [<length-percentage> | auto]{1,2}. 다중 토큰
        // 원문 보존 (페인트가 파싱). 예전엔 "50% 25%" 가 사라져 auto 로 그려졌다.
        // background-size/repeat/attachment: 문법 검증 후 원문 보존(레이어 리스트).
        // mask-repeat/mask-size 는 background 와 문법 동일(<repeat-style># / <bg-size>#).
        "background-size" | "background-repeat" | "background-attachment" | "mask-repeat" | "mask-size" => {
            let t = value_text.trim();
            let low = t.to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            let ok = match name {
                "background-size" | "mask-size" => crate::css::background_size_valid(t),
                "background-repeat" | "mask-repeat" => crate::css::background_repeat_valid(t),
                _ => crate::css::background_attachment_valid(t),
            };
            if ok {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(t.to_string()) }]
            } else {
                Vec::new()
            }
        }
        // text-fit(§CSS Text 4): none | [grow|shrink] [consistent|per-line|per-line-all]?
        // <percentage>?.
        "text-fit" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer" | "none") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            let toks: Vec<&str> = low.split_whitespace().collect();
            let ok = (|| {
                if toks.is_empty() || !matches!(toks[0], "grow" | "shrink") {
                    return false;
                }
                let mut i = 1;
                if i < toks.len() && matches!(toks[i], "consistent" | "per-line" | "per-line-all") {
                    i += 1;
                }
                if i < toks.len()
                    && toks[i].strip_suffix('%').map(|n| n.parse::<f64>().is_ok()).unwrap_or(false)
                {
                    i += 1;
                }
                i == toks.len()
            })();
            if ok {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }]
            } else {
                Vec::new()
            }
        }
        // scroll-axis-lock/scroll-target-group: auto|none. scroll-marker-group:
        // none|before|after(§CSS Overflow). 단일 키워드.
        "scroll-axis-lock" | "scroll-target-group" | "scroll-marker-group" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            let ok = match name {
                "scroll-marker-group" => matches!(low.as_str(), "none" | "before" | "after"),
                _ => matches!(low.as_str(), "auto" | "none"),
            };
            if ok {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }]
            } else {
                Vec::new()
            }
        }
        // mask-type: luminance|alpha. clip-rule/fill-rule: nonzero|evenodd. 단일 키워드.
        "mask-type" | "clip-rule" | "fill-rule" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            let ok = match name {
                "mask-type" => matches!(low.as_str(), "luminance" | "alpha"),
                _ => matches!(low.as_str(), "nonzero" | "evenodd"),
            };
            if ok {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }]
            } else {
                Vec::new()
            }
        }
        // line-height: 단위 없는 수(1.5)는 배수(Lh)로 저장해 상속 시 factor 그대로 —
        // 각 요소가 자기 font-size 를 곱한다(CSS2 §10.8). 퍼센트(150%)는 요소 font-size
        // 기준 길이로 확정돼 그 길이가 상속되므로 em 으로 저장. normal/길이단위는 그대로.
        // line-height(§CSS Inline): normal | none | <number [0,∞]> | <length-percentage [0,∞]>.
        "line-height" => {
            let v = value_text.trim();
            let low = v.to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            if matches!(low.as_str(), "normal" | "none") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            if split_top_level(v).len() != 1 {
                return Vec::new();
            }
            if let Some(pct) = v.strip_suffix('%') {
                return match pct.trim().parse::<f32>() {
                    Ok(n) if n >= 0.0 => vec![Declaration { important: false, name: name.to_string(), value: Value::Length(n / 100.0, Unit::Em) }],
                    _ => Vec::new(),
                };
            }
            if let Ok(n) = v.parse::<f32>() {
                return if n >= 0.0 {
                    vec![Declaration { important: false, name: name.to_string(), value: Value::Length(n, Unit::Lh) }]
                } else {
                    Vec::new()
                };
            }
            // 비음수 길이 또는 calc.
            if crate::css::nonneg_lp_valid(v) {
                let value = interpret_value(v).unwrap_or_else(|| Value::Keyword(v.to_string()));
                return vec![Declaration { important: false, name: name.to_string(), value }];
            }
            Vec::new()
        }
        // text-decoration[-line]: line 키워드 + 색 추출 (style/thickness 는 미사용).
        // none/키워드 없음 → "none". 인라인 레이아웃이 밑줄/취소선/윗줄로 그린다.
        // text-decoration-line(§CSS Text Decor): 검증 + 표준 순서 캐논.
        "text-decoration-line" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            if !crate::css::text_decoration_line_valid(value_text) {
                return Vec::new();
            }
            let canon = crate::css::normalize_text_decoration_line(value_text).unwrap_or(low);
            vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(canon) }]
        }
        "text-decoration" => {
            let is_shorthand = name == "text-decoration";
            let mut lines: Vec<String> = Vec::new();
            let mut line_idx: Vec<usize> = Vec::new();
            let mut style: Option<String> = None;
            let mut color: Option<Value> = None;
            let mut thickness: Option<String> = None;
            let (mut style_seen, mut color_seen, mut thick_seen, mut none_seen) = (0u32, 0u32, 0u32, 0u32);
            let mut invalid = false;
            for (i, &t) in split_top_level(value_text).iter().enumerate() {
                let tl = t.to_ascii_lowercase();
                if matches!(tl.as_str(), "underline" | "overline" | "line-through" | "blink") {
                    if lines.iter().any(|l| l == &tl) {
                        invalid = true; // 같은 line 키워드 중복
                    }
                    line_idx.push(i);
                    lines.push(tl);
                } else if matches!(tl.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                    // CSS-wide 키워드: 기존 동작 유지(무시) — 회귀 방지.
                } else if is_shorthand
                    && matches!(tl.as_str(), "solid" | "double" | "dotted" | "dashed" | "wavy")
                {
                    style_seen += 1;
                    style = Some(tl);
                } else if is_shorthand && matches!(tl.as_str(), "auto" | "from-font") {
                    thick_seen += 1;
                    thickness = Some(tl);
                } else if tl == "none" {
                    none_seen += 1; // none 은 line 비움(다른 line 과 배타)
                } else if is_shorthand && crate::css::single_color_valid(t) {
                    // currentcolor/transparent 는 interpret_value 가 Color 로 안 줄 수 있어
                    // single_color_valid 로 판정하고 키워드로 보존.
                    color_seen += 1;
                    color = Some(match interpret_value(t) {
                        Some(v @ Value::Color(..)) => v,
                        _ => Value::Keyword(tl.clone()),
                    });
                } else if is_shorthand {
                    match interpret_value(t) {
                        Some(Value::Length(..)) | Some(Value::Calc(_)) | Some(Value::MinMax(..)) => {
                            thick_seen += 1;
                            thickness = Some(t.to_string());
                        }
                        _ => invalid = true, // 알 수 없는 토큰
                    }
                } else {
                    invalid = true; // longhand line 에 비 line/none 토큰
                }
            }
            // 각 성분 ≤1, none 은 line 과 배타, line 키워드는 연속(중간에 다른 성분 불가).
            if style_seen > 1 || color_seen > 1 || thick_seen > 1 || none_seen > 1 {
                invalid = true;
            }
            if none_seen >= 1 && !lines.is_empty() {
                invalid = true;
            }
            if let (Some(&first), Some(&last)) = (line_idx.first(), line_idx.last()) {
                if last - first + 1 != line_idx.len() {
                    invalid = true; // line 키워드 비연속
                }
            }
            if invalid {
                return Vec::new();
            }
            let joined = lines.join(" ");
            let mut out = vec![Declaration { important: false,
                name: "text-decoration-line".to_string(),
                value: Value::Keyword(if joined.is_empty() { "none".to_string() } else { joined }),
            }];
            // 단축은 나머지 longhand 를 **항상** 출력(리셋). 미지정은 초기값.
            if is_shorthand {
                out.push(Declaration { important: false, name: "text-decoration-style".to_string(), value: Value::Keyword(style.unwrap_or_else(|| "solid".to_string())) });
                out.push(Declaration { important: false, name: "text-decoration-color".to_string(),
                    value: color.unwrap_or_else(|| Value::Keyword("currentcolor".to_string())) });
                out.push(Declaration { important: false, name: "text-decoration-thickness".to_string(),
                    value: Value::Keyword(thickness.unwrap_or_else(|| "auto".to_string())) });
            }
            out
        }
        // content (::before/::after 생성 콘텐츠): 따옴표 문자열은 벗기고 CSS 이스케이프
        // (\2022 등)를 해석. none/normal/attr()/counter() 는 원문 Keyword 로(생성 판단은 style).
        "content" => {
            let v = value_text.trim();
            let low = v.to_ascii_lowercase();
            // 검증: 무효면 지정 무시(마커/생성 콘텐츠 소비자는 유효값만 unquote 유지).
            if !matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
                && !crate::css::content_valid(v)
            {
                return Vec::new();
            }
            let unquoted = if v.len() >= 2
                && ((v.starts_with('"') && v.ends_with('"'))
                    || (v.starts_with('\'') && v.ends_with('\'')))
            {
                decode_css_escapes(&v[1..v.len() - 1])
            } else {
                v.to_string()
            };
            vec![Declaration { important: false, name: "content".to_string(), value: Value::Keyword(unquoted) }]
        }
        // opacity: 0..1 수 또는 퍼센트(50%). 단위 없는 수(Number)로 저장.
        // opacity(§CSS Color): <number> | <percentage>. 무효(auto/길이/다값) 거부,
        // 계산 불가하지만 문법 유효한 calc(%) 는 지정값 보존.
        "opacity" => {
            let v = value_text.trim();
            let low = v.to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: "opacity".to_string(), value: Value::Keyword(low) }];
            }
            // 순수 수(단위 없음) 또는 퍼센트만 계산. 단위 붙은 값(10px)은 거부.
            let n = if let Some(p) = v.strip_suffix('%') {
                p.trim().parse::<f32>().ok().map(|x| x / 100.0)
            } else {
                v.parse::<f32>().ok()
            };
            match n {
                Some(op) => vec![Declaration { important: false,
                    name: "opacity".to_string(),
                    value: Value::Length(op.clamp(0.0, 1.0), Unit::Number),
                }],
                // 순수 수·퍼센트·수학함수만 유효(단위·키워드·다값·malformed·타입 불일치
                // 거부). 수학함수는 결과가 <number>|<percentage> 인지 타입 검사.
                None if (super::values::math_function_valid(&low)
                    && super::values::math_number_valid(&low))
                    || v.strip_suffix('%').map(|p| p.trim().parse::<f64>().is_ok()).unwrap_or(false) => {
                    // 순수 <number> 수학식이면 계산값으로 접는다(calc(2/4)→0.5). %·문맥
                    // 단위가 섞여 못 접으면 원문 보존.
                    if let Some(x) = super::values::eval_math_number(&low) {
                        if x.is_finite() {
                            return vec![Declaration { important: false, name: "opacity".to_string(),
                                value: Value::Length((x as f32).clamp(0.0, 1.0), Unit::Number) }];
                        }
                    }
                    vec![Declaration { important: false, name: "opacity".to_string(), value: Value::Keyword(v.to_string()) }]
                }
                None => Vec::new(),
            }
        }
        // multicol(§CSS Multicol): column-count/width/rule-width 검증 + columns 전개.
        "column-count" | "column-width" | "column-rule-width" => {
            let low = value_text.trim().to_ascii_lowercase();
            let ok = matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
                || (name == "column-count" && crate::css::column_count_valid(value_text))
                || (name == "column-width" && crate::css::column_width_valid(value_text))
                || (name == "column-rule-width" && crate::css::column_rule_width_valid(value_text));
            if ok {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
            } else {
                Vec::new()
            }
        }
        // 단일 키워드 enum 프로퍼티: 정확한 값 집합으로 검증(무효·다값 거부).
        "float" | "clear" | "visibility" | "break-before" | "break-after" | "break-inside"
        | "box-decoration-break" => {
            let low = value_text.trim().to_ascii_lowercase();
            let ok = matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
                || match name {
                    "float" => matches!(low.as_str(), "none" | "left" | "right" | "inline-start" | "inline-end" | "top" | "bottom" | "start" | "end"),
                    "clear" => matches!(low.as_str(), "none" | "left" | "right" | "both" | "inline-start" | "inline-end"),
                    "visibility" => matches!(low.as_str(), "visible" | "hidden" | "collapse"),
                    "break-before" | "break-after" => matches!(low.as_str(), "auto" | "avoid" | "avoid-page" | "page" | "left" | "right" | "recto" | "verso" | "avoid-column" | "column" | "avoid-region" | "region"),
                    "break-inside" => matches!(low.as_str(), "auto" | "avoid" | "avoid-page" | "avoid-column" | "avoid-region"),
                    "box-decoration-break" => matches!(low.as_str(), "slice" | "clone"),
                    _ => false,
                };
            if ok {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }]
            } else {
                Vec::new()
            }
        }
        // border-*-radius 코너 롱핸드(§CSS Backgrounds): <lp [0,∞]>{1,2}. 검증 후 원문 보존.
        "border-top-left-radius" | "border-top-right-radius" | "border-bottom-left-radius"
        | "border-bottom-right-radius" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
                || crate::css::border_corner_radius_valid(value_text)
            {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
            } else {
                Vec::new()
            }
        }
        // background-position-x/y(§CSS Backgrounds 3): [center|<edge> <lp>?|<lp>]#.
        "background-position-x" | "background-position-y" => {
            let low = value_text.trim().to_ascii_lowercase();
            let edges: &[&str] = if name == "background-position-x" {
                &["left", "right", "x-start", "x-end"]
            } else {
                &["top", "bottom", "y-start", "y-end"]
            };
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
                || crate::css::bg_position_axis_valid(value_text, edges)
            {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
            } else {
                Vec::new()
            }
        }
        // background-clip/origin(§CSS Backgrounds): <box># 목록.
        "background-clip" | "background-origin" => {
            let low = value_text.trim().to_ascii_lowercase();
            let ok = matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
                || (name == "background-clip" && crate::css::background_clip_valid(value_text))
                || (name == "background-origin" && crate::css::box_list_valid(value_text, &["border-box", "padding-box", "content-box"]));
            if ok {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }]
            } else {
                Vec::new()
            }
        }
        // position-area(§css-anchor-2): none | <position-area>{1,2}. 축 호환성 문법 검증 +
        // 캐논(H/block/self-block 앞, span-all 드롭, center 슬롯). 예전엔 검증 없이 raw 수용.
        "position-area" => {
            match crate::css::position_area_canonical(value_text) {
                Some(canon) => vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(canon) }],
                None => Vec::new(),
            }
        }
        // list-style-type(§CSS Lists): <counter-style> | <string> | none.
        "list-style-type" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            if !crate::css::list_style_type_valid(value_text) {
                return Vec::new();
            }
            vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(crate::css::list_style_type_canonical(value_text)) }]
        }
        // position/list-style-position/shape-margin/shape-image-threshold/
        // list-style-image(§여러 스펙): 단순 검증.
        "position" | "list-style-position" | "shape-margin" | "shape-image-threshold"
        | "list-style-image" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            let ok = match name {
                "position" => matches!(low.as_str(), "static" | "relative" | "absolute" | "fixed" | "sticky"),
                "list-style-position" => matches!(low.as_str(), "inside" | "outside"),
                "shape-margin" => crate::css::nonneg_lp_valid(value_text),
                "shape-image-threshold" => crate::css::shape_image_threshold_valid(value_text),
                "list-style-image" => crate::css::list_style_image_valid(value_text),
                _ => false,
            };
            if !ok {
                return Vec::new();
            }
            // 이미지·길이 등은 원문 보존, 키워드는 소문자.
            let v = if matches!(name, "shape-margin" | "shape-image-threshold" | "list-style-image") {
                value_text.trim().to_string()
            } else {
                low
            };
            vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(v) }]
        }
        // css-overflow 계열: text-overflow/continue/max-lines/block-ellipsis/
        // -webkit-line-clamp 검증.
        "text-overflow" | "continue" | "max-lines" | "block-ellipsis" | "-webkit-line-clamp"
        | "line-clamp" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            let ok = match name {
                "text-overflow" => crate::css::text_overflow_valid(value_text),
                "continue" => matches!(low.as_str(), "normal" | "discard" | "collapse" | "-webkit-legacy"),
                "max-lines" => crate::css::max_lines_valid(value_text),
                "block-ellipsis" => crate::css::block_ellipsis_valid(value_text),
                "-webkit-line-clamp" => crate::css::webkit_line_clamp_valid(value_text),
                "line-clamp" => crate::css::line_clamp_valid(value_text),
                _ => false,
            };
            if !ok {
                return Vec::new();
            }
            // max-lines 는 정수 먼저 캐논, 나머지는 원문(문자열 대소문자 보존).
            let v = if name == "max-lines" {
                crate::css::max_lines_canonical(value_text)
            } else if matches!(name, "text-overflow" | "block-ellipsis" | "line-clamp") {
                value_text.trim().to_string()
            } else {
                low
            };
            vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(v) }]
        }
        // text-decoration-inset(§CSS Text Decor 4): auto | <length>{1,2}.
        "text-decoration-inset" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            if !crate::css::text_decoration_inset_valid(value_text) {
                return Vec::new();
            }
            vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(crate::css::text_decoration_inset_canonical(value_text)) }]
        }
        // text-decoration-style/color·text-emphasis-position(§CSS Text Decor) 검증.
        "text-decoration-style" | "text-decoration-color" | "text-emphasis-position" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            let ok = match name {
                "text-decoration-style" => matches!(low.as_str(), "solid" | "double" | "dotted" | "dashed" | "wavy"),
                "text-decoration-color" => crate::css::single_color_valid(value_text),
                "text-emphasis-position" => crate::css::text_emphasis_position_valid(value_text),
                _ => false,
            };
            if !ok {
                return Vec::new();
            }
            // 색은 Value 로 파싱(페인트 소비자 유지), position 은 캐논, style 은 소문자.
            let val = match name {
                "text-decoration-color" => interpret_value(value_text.trim())
                    .unwrap_or_else(|| Value::Keyword(value_text.trim().to_string())),
                "text-emphasis-position" => Value::Keyword(crate::css::text_emphasis_position_canonical(value_text)),
                _ => Value::Keyword(low),
            };
            vec![Declaration { important: false, name: name.to_string(), value: val }]
        }
        // text-underline-position(§CSS Text Decor): auto | [from-font|under] || [left|right].
        "text-underline-position" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
                || crate::css::text_underline_position_valid(value_text)
            {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }]
            } else {
                Vec::new()
            }
        }
        // outline 롱핸드·border-spacing·text-combine-upright(§CSS UI/Tables/Writing).
        "outline-width" | "outline-style" | "outline-color" | "border-spacing"
        | "text-combine-upright" => {
            let low = value_text.trim().to_ascii_lowercase();
            let ok = matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
                || (name == "outline-width" && crate::css::column_rule_width_valid(value_text))
                || (name == "outline-style" && crate::css::outline_style_valid(value_text))
                || (name == "outline-color" && crate::css::outline_color_valid(value_text))
                || (name == "border-spacing" && crate::css::border_spacing_valid(value_text))
                || (name == "text-combine-upright" && crate::css::text_combine_upright_valid(value_text));
            if ok {
                // 색은 Value 파싱, 그 외는 원문 보존.
                let val = if name == "outline-color" && !matches!(low.as_str(), "auto" | "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                    interpret_value(value_text.trim()).unwrap_or_else(|| Value::Keyword(low.clone()))
                } else {
                    Value::Keyword(value_text.trim().to_string())
                };
                vec![Declaration { important: false, name: name.to_string(), value: val }]
            } else {
                Vec::new()
            }
        }
        // size(§CSS Page): @page 서술자 — 요소 스타일 프로퍼티가 아니므로 모든 값 무효.
        "size" => Vec::new(),
        // page(§CSS Page): auto | <custom-ident>(default/CSS-wide 제외, 단일).
        "page" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            let toks: Vec<&str> = value_text.split_whitespace().collect();
            let ok = toks.len() == 1
                && (low == "auto" || (low != "default" && crate::css::is_css_ident(toks[0])));
            if ok {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
            } else {
                Vec::new()
            }
        }
        // mask-composite(§CSS Masking): [add|subtract|intersect|exclude]# 콤마 목록.
        "mask-composite" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
                || crate::css::mask_composite_valid(value_text)
            {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }]
            } else {
                Vec::new()
            }
        }
        // hyphenate-limit-chars(§CSS Text 4): [auto|<integer>]{1,3}. 축약 캐논.
        "hyphenate-limit-chars" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            if crate::css::hyphenate_limit_chars_valid(value_text) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(crate::css::hyphenate_limit_chars_canonical(value_text)) }]
            } else {
                Vec::new()
            }
        }
        // letter-spacing(§CSS Text): normal | <length>. tab-size: <number 0+> | <length 0+>.
        // hyphenate-character: auto | <string>.
        "letter-spacing" | "word-spacing" | "tab-size" | "hyphenate-character" => {
            let v = value_text.trim();
            let low = v.to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            let single = split_top_level(v).len() == 1;
            let ok = single && match name {
                // letter-spacing/word-spacing: normal | <length-percentage>(부호·calc 허용).
                "letter-spacing" | "word-spacing" => low == "normal" || crate::css::margin_value_valid(v) && low != "auto",
                "tab-size" => !low.contains('%') && (crate::css::nonneg_lp_valid(v) || v.parse::<f32>().map(|n| n >= 0.0).unwrap_or(false)),
                "hyphenate-character" => low == "auto" || (v.len() >= 2 && (v.starts_with('"') || v.starts_with('\'')) && v.ends_with(v.chars().next().unwrap())),
                _ => false,
            };
            if !ok {
                return Vec::new();
            }
            let value = if name == "hyphenate-character" || low == "normal" || low == "auto" {
                Value::Keyword(v.to_string())
            } else {
                interpret_value(v).unwrap_or_else(|| Value::Keyword(v.to_string()))
            };
            vec![Declaration { important: false, name: name.to_string(), value }]
        }
        // hyphens: none|manual|auto. text-justify: auto|none|inter-word|inter-character.
        // 단일 키워드만 — 다중 토큰/무효값 거부.
        "hyphens" | "text-justify" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            let ok = match name {
                "hyphens" => matches!(low.as_str(), "none" | "manual" | "auto"),
                // distribute 는 inter-character 의 레거시 별칭(§CSS Text 3) — 수용.
                _ => matches!(
                    low.as_str(),
                    "auto" | "none" | "inter-word" | "inter-character" | "distribute"
                ),
            };
            if ok {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }]
            } else {
                Vec::new()
            }
        }
        // word-space-transform(§CSS Text 4): none | [ space | ideographic-space ] || auto-phrase.
        "word-space-transform" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer" | "none") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            let toks: Vec<&str> = low.split_whitespace().collect();
            let (mut sp, mut auto) = (0u32, 0u32);
            let ok = !toks.is_empty() && toks.len() <= 2 && toks.iter().all(|t| {
                match *t {
                    "space" | "ideographic-space" => { sp += 1; true }
                    "auto-phrase" => { auto += 1; true }
                    _ => false,
                }
            }) && sp <= 1 && auto <= 1 && sp >= 1;
            if ok {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }]
            } else {
                Vec::new()
            }
        }
        // font-feature-settings·font-palette(§CSS Fonts 4) 검증.
        "font-feature-settings" | "font-palette" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            if name == "font-feature-settings" {
                if crate::css::font_feature_settings_valid(value_text) {
                    return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(crate::css::font_feature_settings_canonical(value_text)) }];
                }
                return Vec::new();
            }
            // font-palette: 대소문자 보존(dashed-ident).
            if crate::css::font_palette_valid(value_text) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
            } else {
                Vec::new()
            }
        }
        // font-variant-ligatures·font-language-override(§CSS Fonts 4) 검증.
        "font-variant-ligatures" | "font-language-override" => {
            let low = value_text.trim().to_ascii_lowercase();
            let ok = matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
                || (name == "font-variant-ligatures" && crate::css::font_variant_ligatures_valid(value_text))
                || (name == "font-language-override" && crate::css::font_language_override_valid(value_text));
            if ok {
                // 문자열은 대소문자 보존, 그 외 소문자.
                let v = if name == "font-language-override" && !matches!(low.as_str(), "normal" | "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                    value_text.trim().to_string()
                } else {
                    low
                };
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(v) }]
            } else {
                Vec::new()
            }
        }
        // font-size(§CSS Fonts): <absolute-size> | <relative-size> | <length-percentage [0,∞]>.
        "font-size" => {
            let v = value_text.trim();
            let low = v.to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            if matches!(low.as_str(), "xx-small" | "x-small" | "small" | "medium" | "large" | "x-large" | "xx-large" | "xxx-large" | "larger" | "smaller" | "math" | "-webkit-xxx-large") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            if split_top_level(v).len() == 1 && crate::css::nonneg_lp_valid(v) {
                let value = interpret_value(v).unwrap_or_else(|| Value::Keyword(v.to_string()));
                return vec![Declaration { important: false, name: name.to_string(), value }];
            }
            Vec::new()
        }
        // perspective(§CSS Transforms 2): none | <length [0,∞]>(퍼센트 불가).
        "perspective" => {
            let v = value_text.trim();
            let low = v.to_ascii_lowercase();
            let ok = matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer" | "none")
                || (!low.ends_with('%') && split_top_level(v).len() == 1 && crate::css::nonneg_lp_valid(v));
            if ok {
                let value = interpret_value(v).unwrap_or_else(|| Value::Keyword(low));
                vec![Declaration { important: false, name: name.to_string(), value }]
            } else {
                Vec::new()
            }
        }
        // block-step-size(§CSS Rhythm): none | <length [0,∞]>(퍼센트 불가).
        "block-step-size" => {
            let v = value_text.trim();
            let low = v.to_ascii_lowercase();
            let ok = matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer" | "none")
                || (!low.ends_with('%') && split_top_level(v).len() == 1 && crate::css::nonneg_lp_valid(v));
            if ok {
                let value = interpret_value(v).unwrap_or_else(|| Value::Keyword(low));
                vec![Declaration { important: false, name: name.to_string(), value }]
            } else {
                Vec::new()
            }
        }
        // color-scheme·overscroll-behavior·forced-color-adjust(§여러 스펙) 검증.
        "color-scheme" | "overscroll-behavior" | "overscroll-behavior-x"
        | "overscroll-behavior-y" | "overscroll-behavior-inline" | "overscroll-behavior-block"
        | "forced-color-adjust" => {
            let low = value_text.trim().to_ascii_lowercase();
            let ok = matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
                || (name == "color-scheme" && crate::css::color_scheme_valid(value_text))
                || (name == "overscroll-behavior" && crate::css::overscroll_valid(value_text, false))
                || (matches!(name, "overscroll-behavior-x" | "overscroll-behavior-y" | "overscroll-behavior-inline" | "overscroll-behavior-block") && crate::css::overscroll_valid(value_text, true))
                || (name == "forced-color-adjust" && matches!(low.as_str(), "auto" | "none" | "preserve-parent-color"));
            if ok {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }]
            } else {
                Vec::new()
            }
        }
        // baseline-shift(§CSS Inline/SVG): sub|super|top|center|bottom | <length-percentage>.
        "baseline-shift" => {
            let v = value_text.trim();
            let low = v.to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer" | "sub" | "super" | "top" | "center" | "bottom" | "baseline") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            if split_top_level(v).len() != 1 {
                return Vec::new();
            }
            // margin_value_valid = auto|calc|<length-percentage>; auto 제외.
            if crate::css::margin_value_valid(v) && low != "auto" {
                let value = interpret_value(v).unwrap_or_else(|| Value::Keyword(v.to_string()));
                vec![Declaration { important: false, name: name.to_string(), value }]
            } else {
                Vec::new()
            }
        }
        // 단일 키워드 enum(§여러 스펙): 정확한 값 집합으로 검증.
        "resize" | "user-select" | "caption-side" | "table-layout" | "empty-cells"
        | "border-collapse" | "writing-mode" | "unicode-bidi" | "text-orientation"
        | "direction" | "scroll-snap-stop" | "scroll-snap-align"
        | "alignment-baseline" | "dominant-baseline"
        | "ruby-position" | "ruby-align" | "ruby-overhang" | "ruby-merge"
        | "block-step-align" | "block-step-round" | "block-step-insert"
        | "text-align" | "text-align-last" | "text-align-all" | "line-break" | "word-wrap"
        | "overflow-wrap" | "white-space-collapse"
        | "font-kerning" | "font-variant-caps" | "font-variant-position" | "font-optical-sizing"
        | "transform-box" | "backface-visibility" => {
            let low = value_text.trim().to_ascii_lowercase();
            let snap_align_ok = || {
                let toks: Vec<&str> = low.split_whitespace().collect();
                !toks.is_empty() && toks.len() <= 2
                    && toks.iter().all(|t| matches!(*t, "none" | "start" | "end" | "center"))
            };
            let ok = matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
                || match name {
                    "scroll-snap-stop" => matches!(low.as_str(), "normal" | "always"),
                    "scroll-snap-align" => snap_align_ok(),
                    "resize" => matches!(low.as_str(), "none" | "both" | "horizontal" | "vertical" | "block" | "inline"),
                    "user-select" => matches!(low.as_str(), "auto" | "text" | "none" | "contain" | "all"),
                    "caption-side" => matches!(low.as_str(), "top" | "bottom"),
                    "table-layout" => matches!(low.as_str(), "auto" | "fixed"),
                    "empty-cells" => matches!(low.as_str(), "show" | "hide"),
                    "border-collapse" => matches!(low.as_str(), "separate" | "collapse"),
                    "writing-mode" => matches!(low.as_str(), "horizontal-tb" | "vertical-rl" | "vertical-lr" | "sideways-rl" | "sideways-lr"),
                    "unicode-bidi" => matches!(low.as_str(), "normal" | "embed" | "isolate" | "bidi-override" | "isolate-override" | "plaintext"),
                    "text-orientation" => matches!(low.as_str(), "mixed" | "upright" | "sideways"),
                    "direction" => matches!(low.as_str(), "ltr" | "rtl"),
                    "alignment-baseline" => matches!(low.as_str(), "baseline" | "text-bottom" | "alphabetic" | "ideographic" | "middle" | "central" | "mathematical" | "hanging" | "text-top"),
                    "dominant-baseline" => matches!(low.as_str(), "auto" | "text-bottom" | "alphabetic" | "ideographic" | "middle" | "central" | "mathematical" | "hanging" | "text-top"),
                    "ruby-position" => matches!(low.as_str(), "over" | "under" | "inter-character" | "alternate"),
                    "ruby-align" => matches!(low.as_str(), "start" | "center" | "space-between" | "space-around"),
                    "ruby-overhang" => matches!(low.as_str(), "auto" | "none" | "spaces"),
                    "ruby-merge" => matches!(low.as_str(), "separate" | "merge" | "auto"),
                    "block-step-align" => matches!(low.as_str(), "auto" | "center" | "start" | "end"),
                    "block-step-round" => matches!(low.as_str(), "up" | "down" | "nearest"),
                    "block-step-insert" => matches!(low.as_str(), "margin-box" | "padding-box" | "content-box"),
                    "font-kerning" => matches!(low.as_str(), "auto" | "normal" | "none"),
                    "font-variant-caps" => matches!(low.as_str(), "normal" | "small-caps" | "all-small-caps" | "petite-caps" | "all-petite-caps" | "unicase" | "titling-caps"),
                    "font-variant-position" => matches!(low.as_str(), "normal" | "sub" | "super"),
                    "font-optical-sizing" => matches!(low.as_str(), "auto" | "none"),
                    "transform-box" => matches!(low.as_str(), "content-box" | "border-box" | "fill-box" | "stroke-box" | "view-box"),
                    "backface-visibility" => matches!(low.as_str(), "visible" | "hidden"),
                    "text-align" => matches!(low.as_str(), "start" | "end" | "left" | "right" | "center" | "justify" | "match-parent" | "justify-all"),
                    "text-align-last" => matches!(low.as_str(), "auto" | "start" | "end" | "left" | "right" | "center" | "justify" | "match-parent"),
                    "text-align-all" => matches!(low.as_str(), "start" | "end" | "left" | "right" | "center" | "justify" | "match-parent"),
                    "line-break" => matches!(low.as_str(), "auto" | "loose" | "normal" | "strict" | "anywhere"),
                    "word-wrap" | "overflow-wrap" => matches!(low.as_str(), "normal" | "break-word" | "anywhere"),
                    "white-space-collapse" => matches!(low.as_str(), "collapse" | "preserve" | "preserve-breaks" | "preserve-spaces" | "break-spaces"),
                    _ => false,
                };
            if ok {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }]
            } else {
                Vec::new()
            }
        }
        // view-transition-name/class(§CSS View Transitions): custom-ident 검증.
        "view-transition-name" | "view-transition-class" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
                || (name == "view-transition-name" && crate::css::view_transition_name_valid(value_text))
                || (name == "view-transition-class" && crate::css::view_transition_class_valid(value_text))
            {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
            } else {
                Vec::new()
            }
        }
        // object-fit/image-rendering/image-resolution(§CSS Images) 검증.
        "object-fit" | "image-rendering" | "image-resolution" => {
            let low = value_text.trim().to_ascii_lowercase();
            // object-fit: fill | none | [ [contain|cover] || scale-down ].
            let object_fit_ok = || {
                let toks: Vec<&str> = low.split_whitespace().collect();
                match toks.as_slice() {
                    [a] => matches!(*a, "fill" | "none" | "contain" | "cover" | "scale-down"),
                    [a, b] => {
                        let cc = |t: &str| matches!(t, "contain" | "cover");
                        (cc(a) && *b == "scale-down") || (*a == "scale-down" && cc(b))
                    }
                    _ => false,
                }
            };
            let ok = matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
                || (name == "object-fit" && object_fit_ok())
                || (name == "image-rendering" && matches!(low.as_str(), "auto" | "smooth" | "high-quality" | "crisp-edges" | "pixelated"))
                || (name == "image-resolution" && crate::css::image_resolution_valid(value_text));
            if ok {
                let v = if name == "image-resolution" { value_text.trim().to_string() } else { low };
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(v) }]
            } else {
                Vec::new()
            }
        }
        // animation 롱핸드(§CSS Animations): 콤마 목록. 검증 후 원문 보존.
        "animation-name" | "animation-duration" | "animation-delay"
        | "animation-iteration-count" | "animation-direction" | "animation-fill-mode"
        | "animation-play-state" | "animation-range-start" | "animation-range-end" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
                || crate::css::animation_longhand_valid(name, value_text)
            {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
            } else {
                Vec::new()
            }
        }
        // text-decoration-skip-ink(§CSS Text Decor 4): auto | none | all.
        "text-decoration-skip-ink" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "auto" | "none" | "all" | "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }]
            } else {
                Vec::new()
            }
        }
        // text-decoration-skip-spaces(§CSS Text Decor 4): none | all | [ start || end ].
        "text-decoration-skip-spaces" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
                || crate::css::text_decoration_skip_spaces_valid(value_text)
            {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }]
            } else {
                Vec::new()
            }
        }
        // widows/orphans(§CSS Fragmentation): <integer [1,∞]>.
        "widows" | "orphans" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
                || crate::css::positive_integer_valid(value_text)
            {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
            } else {
                Vec::new()
            }
        }
        // will-change(§CSS Will Change): auto | [scroll-position|contents|<custom-ident>]#.
        "will-change" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
                || crate::css::will_change_valid(value_text)
            {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
            } else {
                Vec::new()
            }
        }
        // column-span/fill/rule-style/rule-color(§CSS Multicol): 단순 검증.
        "column-span" | "column-fill" | "column-rule-style" | "column-rule-color" => {
            let low = value_text.trim().to_ascii_lowercase();
            let ok = matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
                || (name == "column-span" && matches!(low.as_str(), "none" | "all"))
                || (name == "column-fill" && matches!(low.as_str(), "auto" | "balance" | "balance-all"))
                || (name == "column-rule-style" && crate::css::is_line_style(value_text))
                || (name == "column-rule-color" && crate::css::single_color_valid(value_text));
            if ok {
                // 색은 원문 보존(대소문자·함수), 그 외 키워드는 소문자.
                let v = if name == "column-rule-color" { value_text.trim().to_string() } else { low };
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(v) }]
            } else {
                Vec::new()
            }
        }
        // column-rule 단축(§CSS Multicol): <line-width> || <line-style> || <color> →
        // 세 롱핸드로 전개.
        "column-rule" => {
            let low = value_text.trim().to_ascii_lowercase();
            let d = |n: &str, v: String| Declaration { important: false, name: n.to_string(), value: Value::Keyword(v) };
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![d("column-rule-width", low.clone()), d("column-rule-style", low.clone()), d("column-rule-color", low)];
            }
            if !crate::css::column_rule_valid(value_text) {
                return Vec::new();
            }
            let (mut width, mut style, mut color) = (None, None, None);
            for tok in split_top_level(value_text) {
                if crate::css::is_line_style(&tok) {
                    style = Some(tok.to_ascii_lowercase());
                } else if crate::css::column_rule_width_valid(&tok) {
                    width = Some(tok.to_ascii_lowercase());
                } else {
                    color = Some(tok.to_string());
                }
            }
            vec![
                d("column-rule-width", width.unwrap_or_else(|| "medium".to_string())),
                d("column-rule-style", style.unwrap_or_else(|| "none".to_string())),
                d("column-rule-color", color.unwrap_or_else(|| "currentcolor".to_string())),
            ]
        }
        // columns 단축(§CSS Multicol): column-width/column-count 로 전개.
        "columns" => {
            let low = value_text.trim().to_ascii_lowercase();
            let d = |n: &str, v: &str| Declaration { important: false, name: n.to_string(), value: Value::Keyword(v.to_string()) };
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![d("column-width", &low), d("column-count", &low)];
            }
            match crate::css::columns_expand(value_text) {
                Some((w, c)) => vec![d("column-width", &w), d("column-count", &c)],
                None => Vec::new(),
            }
        }
        // counter-increment/reset/set(§CSS Lists 3): none | [ <custom-ident> <integer>? ]+.
        // counter-reset 만 reversed() 허용. 검증 + 기본 정수 추가 캐논(increment 1, 나머지 0).
        "counter-reset" | "counter-increment" | "counter-set" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            let allow_reversed = name == "counter-reset";
            if !crate::css::counter_list_valid(value_text, allow_reversed) {
                return Vec::new();
            }
            let default_int = if name == "counter-increment" { 1 } else { 0 };
            vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(crate::css::counter_list_canonical(value_text, default_int)) }]
        }
        // aspect-ratio: "w / h" 또는 단일 수 → 비율(w/h)을 Length(r, Px)로 저장.
        "aspect-ratio" => {
            let v = value_text.trim();
            let ratio = if let Some((a, b)) = v.split_once('/') {
                match (a.trim().parse::<f32>(), b.trim().parse::<f32>()) {
                    (Ok(a), Ok(b)) if b != 0.0 => Some(a / b),
                    _ => None,
                }
            } else {
                v.parse::<f32>().ok()
            };
            match ratio {
                Some(r) if r > 0.0 => vec![Declaration { important: false,
                    name: "aspect-ratio".to_string(),
                    value: Value::Length(r, Unit::Px),
                }],
                _ => Vec::new(),
            }
        }
        // @font-face src: 원문 보존(다중 url()·format() 포함). font-face 파서가 해석.
        "src" => {
            vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
        }
        // font-family: 유효성 검증(무효 식별자 0simple 등은 선언 거부). 원문 보존.
        "font-family" => {
            let v = value_text.trim();
            let low = v.to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
                || font_family_valid(v)
            {
                vec![Declaration { important: false, name: "font-family".to_string(), value: Value::Keyword(v.to_string()) }]
            } else {
                Vec::new()
            }
        }
        // transform: 함수 목록(translate/scale/rotate/skew/matrix) 원문 보존.
        // 레이아웃이 2D 행렬로 파싱하고, 페인트가 서브트리를 그 행렬로 변환한다.
        // transform(§CSS Transforms): none | <transform-function>+. 함수별 인자 개수·
        // 타입 검증(미지 함수는 관대). 유효하면 원문 보존(레이아웃이 파싱).
        "transform" | "-webkit-transform" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: "transform".to_string(), value: Value::Keyword(low) }];
            }
            if crate::css::transform_valid(value_text) {
                vec![Declaration { important: false, name: "transform".to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
            } else {
                Vec::new()
            }
        }
        // transition/animation 롱핸드: 원문 보존(애니메이션은 미구현이지만 계산값은
        // 정규화해 돌려준다 — collect_computed_styles 가 시간(ms→s)·목록 간격을 정규화).
        "transition-behavior"
        | "animation-composition" => {
            vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
        }
        // 키워드 값 프로퍼티(계산값 = 지정 키워드): UI/인터랙션/표/스크롤 등. 원문 보존.
        // 예전엔 interpret_value(키워드) 가 None 이라 선언이 통째로 드롭돼 getComputedStyle
        // 에 안 나왔다(cursor/user-select/appearance 등 실제 프로퍼티가 통째로 사라짐).
        // appearance(§CSS UI 4 + compat): 정해진 키워드만 유효(none/auto/textfield/
        // menulist-button/button/checkbox/listbox/menulist/meter/progress-bar/radio/
        // searchfield/textarea). bogus-button 등 미지 키워드는 거부.
        "appearance" | "-webkit-appearance" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(
                low.as_str(),
                "none" | "auto" | "textfield" | "menulist-button" | "button" | "checkbox"
                    | "listbox" | "menulist" | "meter" | "progress-bar" | "radio"
                    | "searchfield" | "textarea" | "inherit" | "initial" | "unset" | "revert"
                    | "revert-layer"
            ) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }]
            } else {
                Vec::new()
            }
        }
        "-webkit-user-select"
        | "pointer-events" | "touch-action"
        | "isolation"
        | "scroll-behavior"
        | "transform-style"
        | "background-blend-mode"
        | "text-rendering" | "print-color-adjust"
        // 2차 배치: text/font-variant/ruby/scrollbar/list 등 키워드 프로퍼티.
        | "text-emphasis-style"
        | "quotes"
        // 3차 배치: grid/break/column/bidi 등 키워드 프로퍼티.
        | "page-break-before" | "page-break-after" | "page-break-inside"
        // 4차 배치: mask, offset, scroll-snap-stop, place-self.
        | "mask-image"
        | "mask-origin" | "mask-clip" | "mask-mode"
        | "contain-intrinsic-width" | "contain-intrinsic-height"
        // 5차: SVG presentation 키워드/수/목록 프로퍼티(stroke-width/dashoffset 는 길이).
        | "fill-opacity" | "stroke-opacity" | "stroke-linecap" | "stroke-linejoin"
        | "stroke-dasharray" | "stroke-miterlimit"
        | "paint-order" | "vector-effect" | "text-anchor"
        | "shape-rendering" | "color-interpolation" | "color-interpolation-filters"
        | "marker-start" | "marker-mid" | "marker-end"
        // 6차: font/text/webkit-box/math/misc 키워드 프로퍼티(수/목록/함수 원문 보존).
        | "text-size-adjust"
        | "-webkit-text-size-adjust" | "-webkit-box-orient"
        | "-webkit-box-align" | "-webkit-box-pack" | "zoom"
        | "math-style" | "math-depth" | "math-shift"
        // 8차: 순수 키워드 롱핸드.
        | "anchor-name"
        // 9차: 개별 변환(translate 는 아래 arm 에서 검증; scale 도).
        // 개별 border-*-style(solid/dashed 등 키워드).
        | "border-top-style" | "border-right-style" | "border-bottom-style"
        | "border-left-style"
        // 12차: 흩어진 프로퍼티(위치/shape/키워드 원문 보존).
        | "text-box-trim"
        | "text-box-edge" | "text-box" | "white-space-trim" => {
            vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
        }
        // border-image 수치 롱핸드: 문법 검증 후 원문 보존.
        "border-image-repeat" | "border-image-outset" | "border-image-width" | "border-image-slice" => {
            let t = value_text.trim();
            let ok = match name {
                "border-image-repeat" => crate::css::border_image_repeat_valid(t),
                "border-image-outset" => crate::css::border_image_outset_valid(t),
                "border-image-width" => crate::css::border_image_width_valid(t),
                _ => crate::css::border_image_slice_valid(t),
            };
            if ok {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(t.to_string()) }]
            } else {
                Vec::new()
            }
        }
        // text-decoration-thickness: auto | from-font | <length-percentage>
        // text-underline-offset: auto | <length-percentage>
        // 길이/calc 는 Length/Calc 로 파싱해 계산값에서 em/rem→px, calc 축약이 되게 한다
        // (키워드로 보존하면 resolve_units 가 건너뛰어 "2em"/"calc(...)" 원문이 남는다).
        "text-decoration-thickness" | "text-underline-offset" => {
            let t = value_text.trim();
            let low = t.to_ascii_lowercase();
            let kw_ok = matches!(
                low.as_str(),
                "auto" | "inherit" | "initial" | "unset" | "revert" | "revert-layer"
            ) || (name == "text-decoration-thickness" && low == "from-font");
            if kw_ok {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            // <length-percentage>: 단위 있는 길이·퍼센트·calc 만. 단위없는 수는 0 만
            // 허용(§CSS unitless zero), 정크 키워드·다중값은 거부.
            match interpret_value(t) {
                Some(Value::Length(n, Unit::Number)) if n == 0.0 => {
                    vec![Declaration { important: false, name: name.to_string(), value: Value::Length(0.0, Unit::Px) }]
                }
                Some(Value::Length(_, Unit::Number)) => Vec::new(),
                Some(v @ (Value::Length(..) | Value::Calc(_) | Value::MinMax(..))) => {
                    vec![Declaration { important: false, name: name.to_string(), value: v }]
                }
                _ => Vec::new(),
            }
        }
        // SVG 페인트/색 프로퍼티: <color> 는 색으로(계산값 rgb()), none/url()/context-* 는
        // 키워드로 보존.
        "fill" | "stroke" | "stop-color" | "flood-color" | "lighting-color"
        | "text-emphasis-color"
        | "-webkit-text-fill-color" | "-webkit-text-stroke-color" => {
            let value = match interpret_value(value_text.trim()) {
                Some(v @ Value::Color(_)) => v,
                _ => Value::Keyword(value_text.trim().to_string()),
            };
            vec![Declaration { important: false, name: name.to_string(), value }]
        }
        // border-*-width/style 논리(§CSS Logical): 롱핸드 단일, -{block,inline}- 쌍은 {1,2}.
        "border-block-start-width" | "border-block-end-width" | "border-inline-start-width"
        | "border-inline-end-width" | "border-block-width" | "border-inline-width"
        | "border-block-start-style" | "border-block-end-style" | "border-inline-start-style"
        | "border-inline-end-style" | "border-block-style" | "border-inline-style" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            let is_style = name.ends_with("-style");
            let is_pair = name == "border-block-width" || name == "border-inline-width"
                || name == "border-block-style" || name == "border-inline-style";
            let toks = split_top_level(value_text.trim());
            let item_ok = |t: &str| if is_style { crate::css::is_line_style(t) } else { crate::css::column_rule_width_valid(t) };
            let ok = if is_pair {
                !toks.is_empty() && toks.len() <= 2 && toks.iter().all(|t| item_ok(t))
            } else {
                toks.len() == 1 && item_ok(toks[0])
            };
            if ok {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
            } else {
                Vec::new()
            }
        }
        // border-*-color 논리 롱핸드(§CSS Logical): 단일 <color>. border-{block,inline}
        // -color 는 <color>{1,2}. 검증 후 색은 Value, 무효는 거부.
        "border-block-start-color" | "border-block-end-color"
        | "border-inline-start-color" | "border-inline-end-color"
        | "border-block-color" | "border-inline-color" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            let is_pair = name == "border-block-color" || name == "border-inline-color";
            let toks = split_top_level(value_text);
            let ok = if is_pair {
                !toks.is_empty() && toks.len() <= 2 && toks.iter().all(|t| crate::css::single_color_valid(t))
            } else {
                crate::css::single_color_valid(value_text)
            };
            if !ok {
                return Vec::new();
            }
            // 단일 색은 Value 로, 쌍은 원문 보존.
            let value = if !is_pair {
                interpret_value(value_text.trim()).unwrap_or_else(|| Value::Keyword(value_text.trim().to_string()))
            } else {
                Value::Keyword(value_text.trim().to_string())
            };
            vec![Declaration { important: false, name: name.to_string(), value }]
        }
        // transform-origin: "0 0", "left top", "50% 50%" 같은 다중 토큰 값이다.
        // 일반 값 파서는 다중 토큰을 파싱하지 못해 None 을 돌려주고, 그러면 선언이
        // 통째로 사라져서 **항상 중심 기준 회전**이 되어 버린다. 원문을 보존한다.
        // transform-origin(§CSS Transforms): <position 2값> <length>?. 검증 후 원문 보존.
        "transform-origin" | "-webkit-transform-origin" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: "transform-origin".to_string(), value: Value::Keyword(low) }];
            }
            if !crate::css::transform_origin_valid(value_text) {
                return Vec::new();
            }
            vec![Declaration { important: false, name: "transform-origin".to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
        }
        // perspective-origin: 다중 토큰 원문 보존(계산값은 resolve_origin 이 px 로).
        // perspective-origin(§CSS Transforms): <position>. 검증 후 원문 보존(캐논은 직렬화).
        "perspective-origin" | "-webkit-perspective-origin" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: "perspective-origin".to_string(), value: Value::Keyword(low) }];
            }
            if !crate::css::position_valid(value_text) {
                return Vec::new();
            }
            vec![Declaration { important: false, name: "perspective-origin".to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
        }
        // filter: 색 변환 함수 목록 원문 보존 (paint 가 grayscale/brightness/invert/sepia/contrast 적용).
        "filter" | "-webkit-filter" => {
            vec![Declaration { important: false, name: "filter".to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
        }
        // animation 단축 → 롱핸드. 첫 시간=duration, 둘째=delay. 나머지는 키워드로 구분.
        "animation" | "-webkit-animation" => animation_shorthand(value_text),
        // text-shadow: <dx> <dy> [blur] <color> (단일 그림자). 상속 속성. paint 가 글리프 뒤에 그림.
        "text-shadow" => {
            let v = value_text.trim();
            let low = v.to_ascii_lowercase();
            if matches!(
                low.as_str(),
                "none" | "inherit" | "initial" | "unset" | "revert" | "revert-layer"
            ) {
                return vec![Declaration { important: false, name: "text-shadow".to_string(), value: Value::Keyword(low) }];
            }
            // §CSS Text Decor 문법 검증(inset·spread 없음, 길이 2~3, blur 음수·% 불가).
            // 무효면 거부. 유효값은 아래에서 항상 원문을 보존한다.
            if !text_shadow_valid(v) {
                return Vec::new();
            }
            // 첫 최상위 콤마까지가 첫 그림자 — paint 용 px 길이·색 추출(calc/em 은
            // 추출 실패해도 원문 보존으로 computed/보간이 처리).
            let mut depth = 0i32;
            let mut end = value_text.len();
            for (i, c) in value_text.char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    ',' if depth == 0 => {
                        end = i;
                        break;
                    }
                    _ => {}
                }
            }
            let mut lens: Vec<f32> = Vec::new();
            let mut color: Option<Value> = None;
            for tok in split_top_level(&value_text[..end]) {
                match interpret_value(tok) {
                    Some(Value::Length(v, Unit::Px)) => lens.push(v),
                    Some(c @ Value::Color(..)) => color = Some(c),
                    _ => {}
                }
            }
            let px = |v: f32| Value::Length(v, Unit::Px);
            let mut decls = Vec::new();
            if lens.len() >= 2 {
                let color = color.unwrap_or(Value::Color(Color { r: 0, g: 0, b: 0, a: 128 }));
                decls.push(Declaration { important: false, name: "text-shadow-x".to_string(), value: px(lens[0]) });
                decls.push(Declaration { important: false, name: "text-shadow-y".to_string(), value: px(lens[1]) });
                decls.push(Declaration { important: false, name: "text-shadow-color".to_string(), value: color });
            }
            // 전체 원문 보존 — getComputedStyle 이 캐논 직렬화(색 우선)하고 보간에 쓴다.
            decls.push(Declaration { important: false, name: "text-shadow".to_string(), value: Value::Keyword(v.to_string()) });
            decls
        }
        // box-shadow: <dx> <dy> [blur] [spread] <color> (단일 그림자, outset 만)
        "box-shadow" => box_shadow_shorthand(value_text),
        // transition: [<property> || <duration> || <timing-function> || <delay> ||
        // <behavior>]# — 첫 시간=duration, 둘째=delay. 롱핸드로 확장.
        "transition" => transition_shorthand(value_text),
        // border: <width> <style> <color> (임의 순서) → 네 변 longhand 로
        "border" => border_shorthand(&["top", "right", "bottom", "left"], value_text),
        // list-style 단축 → type/position/image. `list-style: none` 이 마커를 없앤다.
        "list-style" => {
            let low = value_text.trim().to_ascii_lowercase();
            if !matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
                && !crate::css::list_style_valid(value_text)
            {
                return Vec::new();
            }
            let mut out = Vec::new();
            for tok in split_top_level(value_text) {
                match tok {
                    "inside" | "outside" => out.push(Declaration { important: false,
                        name: "list-style-position".to_string(),
                        value: Value::Keyword(tok.to_string()),
                    }),
                    t if t.starts_with("url(") => {
                        if let Some(v) = interpret_value(t) {
                            out.push(Declaration { important: false, name: "list-style-image".to_string(), value: v });
                        }
                    }
                    // none 은 type/image 둘 다 될 수 있으나 마커 제거 목적상 type:none 로.
                    t => out.push(Declaration { important: false,
                        name: "list-style-type".to_string(),
                        value: Value::Keyword(t.to_string()),
                    }),
                }
            }
            out
        }
        // background 단축: 색 → background-color, url() → background-image.
        // position/repeat/size/attachment/gradient 등은 근사(드롭).
        "background" => background_shorthand(value_text),
        // background-position/object-position: 다중 토큰("center top" 등) 원문 보존,
        // paint 가 파싱. (position 계열은 축별 다값이라 interpret_value 로 못 담음)
        // object-position(§CSS Values): <position>(3값 불가). CSS-wide 통과.
        "object-position" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            if crate::css::position_valid(value_text) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
            } else {
                Vec::new()
            }
        }
        // background-position(§CSS Backgrounds): <bg-position>#(3값 허용, 콤마 목록).
        "background-position" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            let layers = split_top_level_commas(value_text);
            if !layers.is_empty() && layers.iter().all(|l| crate::css::bg_position_valid(l)) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
            } else {
                Vec::new()
            }
        }
        // clip-path/backdrop-filter: 함수 표기 원문 보존, paint 가 파싱.
        // clip-path: none | <url> | [ <basic-shape> || <geometry-box> ]. 검증 후 원문 보존.
        "clip-path" => {
            let t = value_text.trim();
            let low = t.to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            if crate::css::clip_path_valid(t) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(t.to_string()) }]
            } else {
                Vec::new()
            }
        }
        "backdrop-filter" => {
            vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
        }
        // all(§CSS Cascade): CSS 전역 키워드만(initial|inherit|unset|revert|revert-layer).
        "all" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "initial" | "inherit" | "unset" | "revert" | "revert-layer") {
                vec![Declaration { important: false, name: "all".to_string(), value: Value::Keyword(low) }]
            } else {
                Vec::new()
            }
        }
        // border-image-source: none | <image>. 단일 값(콤마 불가), auto 거부.
        "border-image-source" => {
            let t = value_text.trim();
            let low = t.to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            if crate::css::border_image_source_valid(t) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(t.to_string()) }]
            } else {
                Vec::new()
            }
        }
        // clip(레거시): auto | rect(...). 검증 후 원문 보존.
        "clip" => {
            let t = value_text.trim();
            let low = t.to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            if crate::css::clip_valid(t) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(t.to_string()) }]
            } else {
                Vec::new()
            }
        }
        // shape-outside: none | <image> | [ <basic-shape> || <shape-box> ]. 검증 후 원문 보존.
        "shape-outside" => {
            let t = value_text.trim();
            let low = t.to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            if crate::css::shape_outside_valid(t) {
                vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(t.to_string()) }]
            } else {
                Vec::new()
            }
        }
        // outline: <width> <style> <color> (균일 링, 레이아웃 영향 없음)
        // outline = <outline-width> || <outline-style> || <outline-color>. width 키워드
        // (thin/medium/thick)와 style 키워드(none/solid/…)를 구분하고 invert 는 색으로.
        // 단축은 모든 longhand 를 **리셋**(미지정은 초기값 medium/none/invert)한다.
        "outline" => {
            let low = value_text.trim().to_ascii_lowercase();
            if !matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
                && !crate::css::outline_valid(value_text)
            {
                return Vec::new();
            }
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
            {
                return vec![
                    Declaration { important: false, name: "outline-width".to_string(), value: Value::Keyword(low.clone()) },
                    Declaration { important: false, name: "outline-style".to_string(), value: Value::Keyword(low.clone()) },
                    Declaration { important: false, name: "outline-color".to_string(), value: Value::Keyword(low) },
                ];
            }
            let (mut width, mut style, mut color) = (None, None, None);
            for tok in split_top_level(value_text) {
                let tl = tok.to_ascii_lowercase();
                if matches!(tl.as_str(), "thin" | "medium" | "thick") {
                    width = Some(Value::Keyword(tl));
                } else if matches!(
                    tl.as_str(),
                    "none" | "solid" | "dotted" | "dashed" | "double" | "groove" | "ridge"
                        | "inset" | "outset" | "auto"
                ) {
                    style = Some(Value::Keyword(tl));
                } else if tl == "invert" {
                    color = Some(Value::Keyword("invert".to_string()));
                } else {
                    match interpret_value(tok) {
                        Some(v @ Value::Length(..)) => width = Some(v),
                        Some(v @ (Value::Color(..) | Value::ColorFn(..))) => color = Some(v),
                        _ => {}
                    }
                }
            }
            vec![
                Declaration { important: false, name: "outline-width".to_string(),
                    value: width.unwrap_or_else(|| Value::Keyword("medium".to_string())) },
                Declaration { important: false, name: "outline-style".to_string(),
                    value: style.unwrap_or_else(|| Value::Keyword("none".to_string())) },
                Declaration { important: false, name: "outline-color".to_string(),
                    value: color.unwrap_or_else(|| Value::Keyword("invert".to_string())) },
            ]
        }
        "border-top" => border_shorthand(&["top"], value_text),
        "border-right" => border_shorthand(&["right"], value_text),
        "border-bottom" => border_shorthand(&["bottom"], value_text),
        "border-left" => border_shorthand(&["left"], value_text),
        "font" => font_shorthand(value_text),
        // padding/margin 롱핸드(§CSS Box): padding 은 <length-percentage [0,∞]>,
        // margin 은 <length-percentage> | auto. 단일 값. 유효는 Value 저장(레이아웃 불변).
        "padding-top" | "padding-right" | "padding-bottom" | "padding-left"
        | "margin-top" | "margin-right" | "margin-bottom" | "margin-left" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            let is_margin = name.starts_with("margin");
            let toks = split_top_level(value_text.trim());
            // anchor-size()(§css-anchor-1): margin 에서 유효(padding 은 아님).
            if is_margin && toks.len() == 1 {
                if let Some(canon) = crate::css::anchor_size_canonical(toks[0]) {
                    return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(canon) }];
                }
            }
            let ok = toks.len() == 1
                && if is_margin { crate::css::margin_value_valid(toks[0]) } else { crate::css::nonneg_lp_valid(toks[0]) };
            if !ok {
                return Vec::new();
            }
            let v = interpret_value(toks[0]).unwrap_or_else(|| Value::Keyword(value_text.trim().to_string()));
            vec![Declaration { important: false, name: name.to_string(), value: v }]
        }
        // color(§CSS Color): <color>. CSS-wide 통과, 그 외는 실제 색만 수용(무효 명명/
        // 숫자/키워드 거부). 계산 불가하지만 문법 유효한 색 함수는 지정값 보존.
        // color / background-color: 단일 <color>. none/다중토큰/콤마 거부.
        "color" | "background-color" => {
            let low = value_text.trim().to_ascii_lowercase();
            if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
            }
            let v = value_text.trim();
            match interpret_value(v) {
                // 색(또는 색으로 해석되는 키워드)만 수용. Length/일반 Keyword 는 거부.
                Some(value @ (Value::Color(..) | Value::ColorFn(..))) => {
                    vec![Declaration { important: false, name: name.to_string(), value }]
                }
                Some(Value::Keyword(k)) if crate::css::single_color_valid(&k) => {
                    vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(k) }]
                }
                _ if crate::css::single_color_valid(v) || crate::css::color_syntax_valid(v) => {
                    vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(v.to_string()) }]
                }
                _ => Vec::new(),
            }
        }
        _ => match interpret_value(value_text) {
            Some(value) => vec![Declaration { important: false, name: name.to_string(), value }],
            None => Vec::new(),
        },
    }
}

// flex 단축에서 basis 토큰인가 (길이/%/키워드 — 순수 숫자 grow/shrink 와 구분).
fn is_flex_basis_token(t: &str) -> bool {
    if t.parse::<f32>().is_ok() {
        return false; // 단위 없는 순수 숫자(0, 2, 1.5)는 grow/shrink
    }
    matches!(t, "auto" | "content" | "max-content" | "min-content" | "fit-content")
        || t.ends_with('%')
        || matches!(interpret_value(t), Some(Value::Length(..)))
}

fn parse_flex_basis(t: &str) -> Value {
    if matches!(t, "auto" | "content" | "max-content" | "min-content" | "fit-content") {
        Value::Keyword(t.to_string())
    } else {
        interpret_value(t).unwrap_or(Value::Keyword("auto".to_string()))
    }
}

// 절대 크기 키워드 → px (medium=16 기준 스케일, CSS Fonts).
// text-spacing 단축(§CSS Text 4) 파싱 → (autospace, spacing-trim). normal/none/auto 특수,
// [<spacing-trim> || <autospace>] 2값. 채움 키워드(normal/auto)는 확정 슬롯 배정 후 남은
// 슬롯(autospace 우선)에. CSS-wide 키워드는 그대로 통과(계산에서 해석).
pub(crate) fn text_spacing_parse(raw: &str) -> Option<(String, String)> {
    let low = raw.trim().to_ascii_lowercase();
    match low.as_str() {
        "normal" | "initial" => return Some(("normal".into(), "normal".into())),
        "none" => return Some(("no-autospace".into(), "space-all".into())),
        "auto" => return Some(("auto".into(), "auto".into())),
        "inherit" | "unset" | "revert" | "revert-layer" => return Some((low.clone(), low)),
        _ => {}
    }
    let (mut trim, mut autospace): (Option<String>, Option<String>) = (None, None);
    let mut fills: Vec<String> = Vec::new();
    for tok in low.split_whitespace() {
        if matches!(tok, "space-all" | "space-first" | "trim-all" | "trim-both" | "trim-start") {
            if trim.is_some() {
                return None;
            }
            trim = Some(tok.to_string());
        } else if tok == "no-autospace" {
            if autospace.is_some() {
                return None;
            }
            autospace = Some(tok.to_string());
        } else if tok == "normal" {
            // 채움은 normal 만 — auto 는 2값 폼에서 무효(standalone 만).
            fills.push(tok.to_string());
        } else {
            return None;
        }
    }
    if trim.is_none() && autospace.is_none() && fills.is_empty() {
        return None;
    }
    for f in fills {
        if autospace.is_none() {
            autospace = Some(f);
        } else if trim.is_none() {
            trim = Some(f);
        } else {
            return None;
        }
    }
    Some((autospace.unwrap_or_else(|| "normal".into()), trim.unwrap_or_else(|| "normal".into())))
}

// text-spacing 계산·지정값 직렬화(autospace, trim → 캐논). 순서는 <spacing-trim> 먼저.
pub(crate) fn text_spacing_from_parts(a: &str, t: &str) -> String {
    if a == "normal" && t == "normal" {
        "normal".to_string()
    } else if a == "no-autospace" && t == "space-all" {
        "none".to_string()
    } else if a == "auto" && t == "auto" {
        "auto".to_string()
    } else {
        let mut parts: Vec<String> = Vec::new();
        if t != "normal" {
            parts.push(t.to_string());
        }
        if a != "normal" {
            parts.push(a.to_string());
        }
        if parts.is_empty() {
            "normal".to_string()
        } else {
            parts.join(" ")
        }
    }
}

// overflow-clip-margin(§CSS Overflow 4): <visual-box> || <length [0,∞]> 캐논.
// 기본 box=padding-box(생략), length=0px. box first, box 있으면 0 length 생략,
// box 없으면 length. calc/min/max 원문 보존. 무효면 None.
pub(crate) fn overflow_clip_margin_canonical(raw: &str) -> Option<String> {
    let low = raw.trim().to_ascii_lowercase();
    if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
        return Some(low);
    }
    let (mut boxv, mut len): (Option<String>, Option<String>) = (None, None);
    for tok in split_top_level(&low) {
        let t = tok.trim();
        if matches!(t, "content-box" | "padding-box" | "border-box") {
            if boxv.is_some() {
                return None;
            }
            boxv = Some(t.to_string());
        } else {
            if len.is_some() {
                return None;
            }
            match interpret_value(t) {
                Some(v @ Value::Length(n, u)) if !matches!(u, Unit::Percent) && n >= 0.0 => {
                    len = Some(crate::style::computed_value_string(&v));
                }
                // calc 는 % 없어야(overflow-clip-margin 은 <length>, % 무효).
                Some(Value::Calc(c)) if c.pct == 0.0 => {
                    len = Some(t.to_string());
                }
                Some(Value::MinMax(..)) => {
                    len = Some(t.to_string());
                }
                _ => return None,
            }
        }
    }
    if boxv.is_none() && len.is_none() {
        return None;
    }
    let b = boxv.unwrap_or_else(|| "padding-box".to_string());
    let l = len.unwrap_or_else(|| "0px".to_string());
    Some(if b == "padding-box" {
        l
    } else if l == "0px" {
        b
    } else {
        format!("{} {}", b, l)
    })
}

pub(crate) fn font_size_keyword(k: &str) -> Option<f32> {
    Some(match k {
        "xx-small" => 9.6,
        "x-small" => 12.0,
        "small" => 13.3,
        "medium" => 16.0,
        "large" => 18.0,
        "x-large" => 24.0,
        "xx-large" => 32.0,
        _ => return None,
    })
}

// font 단축: [style|variant|weight|stretch]* size[/line-height] family
// 시스템 폰트 키워드(caption 등)와 global 키워드는 no-op. size 토큰을 못 찾으면 드롭.
// CSS 식별자(custom-ident) 유효성(§CSS Syntax). name-start(문자/_/비ASCII) 또는
// -name-start 로 시작, 이후 name char(문자/숫자/-/_/비ASCII). 숫자·특수문자 시작은 무효.
// 이스케이프(\)가 있으면 관대하게 유효로 본다(정확한 토큰화는 생략).
fn is_css_ident(tok: &str) -> bool {
    if tok.is_empty() {
        return false;
    }
    if tok.contains('\\') {
        return true; // 이스케이프 — 관대 처리
    }
    let name_start = |c: char| c.is_ascii_alphabetic() || c == '_' || (c as u32) >= 0x80;
    let mut chars = tok.chars();
    let c0 = chars.next().unwrap();
    if c0 == '-' {
        match chars.next() {
            Some(c) if name_start(c) || c == '-' => {}
            _ => return false, // "-" 단독, "-3" 등 무효
        }
    } else if !name_start(c0) {
        return false; // 숫자/특수문자 시작 무효
    }
    tok.chars()
        .skip(1)
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || (c as u32) >= 0x80)
}

// 따옴표 문자열이 하나로 닫히고 그 뒤에 다른 토큰이 없는가('times' new roman 은 무효).
fn quoted_string_complete(s: &str, q: char) -> bool {
    let mut it = s.char_indices();
    it.next(); // 여는 따옴표
    let mut escaped = false;
    for (i, c) in it {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
            continue;
        }
        if c == q {
            return s[i + c.len_utf8()..].trim().is_empty();
        }
    }
    false // 안 닫힘
}

// 콤마 구분 패밀리 하나의 유효성: 단일 따옴표 문자열이거나 유효 식별자 시퀀스.
// 예약어(CSS-wide 키워드·default)는 **단일 토큰** 패밀리일 때만 무효 — 다중 토큰
// ("default bongo")에서는 일반 식별자로 유효(§CSS Fonts <family-name>).
fn single_family_valid(fam: &str) -> bool {
    let fam = fam.trim();
    let Some(first) = fam.chars().next() else {
        return false;
    };
    if first == '"' || first == '\'' {
        return quoted_string_complete(fam, first);
    }
    let toks: Vec<&str> = fam.split_whitespace().collect();
    if toks.len() == 1
        && matches!(
            toks[0].to_ascii_lowercase().as_str(),
            "default" | "initial" | "inherit" | "unset" | "revert" | "revert-layer"
        )
    {
        return false;
    }
    !toks.is_empty() && toks.iter().all(|t| is_css_ident(t))
}

// font-family 값 유효성(§CSS Fonts). 각 콤마 구분 패밀리가 유효해야 한다.
// 무효면 선언 전체가 파싱 실패(font: 16px 0simple 도 통째로 무효).
pub(crate) fn font_family_valid(value: &str) -> bool {
    let fams = split_top_level_commas(value);
    !fams.is_empty() && fams.iter().all(|f| single_family_valid(f))
}

fn font_shorthand(value_text: &str) -> Vec<Declaration> {
    let v = value_text.trim();
    if matches!(
        v,
        "caption" | "icon" | "menu" | "message-box" | "small-caption" | "status-bar"
            | "inherit" | "initial" | "unset"
    ) {
        return Vec::new();
    }
    let tokens: Vec<&str> = split_top_level(v);
    // size 토큰: '/' 앞부분이 길이거나 크기 키워드(larger/smaller 포함)인 첫 토큰
    // font-size 는 <length-percentage>(calc/min/max/clamp 포함) | 절대·상대 키워드.
    let is_size = |t: &str| {
        let head = t.split('/').next().unwrap_or(t);
        matches!(
            interpret_value(head),
            Some(Value::Length(..)) | Some(Value::Calc(..)) | Some(Value::MinMax(..))
        ) || font_size_keyword(head).is_some()
            || matches!(head.to_ascii_lowercase().as_str(), "larger" | "smaller")
    };
    let Some(si) = tokens.iter().position(|t| is_size(t)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    // size 앞 접두(§CSS Fonts font 단축): style || variant(small-caps) || weight ||
    // stretch, 각 성분 최대 1회, normal 은 채움값. 미인식 토큰·중복 성분·접두 5개 이상은
    // 단축 전체를 무효로 한다. font 단축은 지정 안 된 성분을 초기값으로 리셋하므로
    // (§CSS Fonts §font-prop) 계산값 round-trip 을 위해 모든 성분을 명시 방출한다.
    let is_weight = |l: &str| {
        matches!(l, "bold" | "bolder" | "lighter")
            || l.parse::<f32>().map(|n| (1.0..=1000.0).contains(&n)).unwrap_or(false)
    };
    let is_stretch = |l: &str| {
        matches!(
            l,
            "ultra-condensed" | "extra-condensed" | "condensed" | "semi-condensed"
                | "semi-expanded" | "expanded" | "extra-expanded" | "ultra-expanded"
        )
    };
    if tokens[..si].len() > 4 {
        return Vec::new();
    }
    let mut style_v: Option<Value> = None;
    let mut variant_scaps = false;
    let mut weight_v: Option<Value> = None;
    let mut stretch_v: Option<String> = None;
    for t in &tokens[..si] {
        let tl = t.to_ascii_lowercase();
        if tl == "normal" {
            continue; // 채움값 — 카테고리 안 잡음
        } else if tl == "italic" || tl == "oblique" {
            if style_v.is_some() {
                return Vec::new();
            }
            style_v = Some(Value::Keyword("italic".to_string()));
        } else if tl == "small-caps" {
            if variant_scaps {
                return Vec::new();
            }
            variant_scaps = true;
        } else if is_weight(&tl) {
            if weight_v.is_some() {
                return Vec::new();
            }
            weight_v = Some(if tl == "bold" {
                Value::Length(700.0, Unit::Number)
            } else if tl == "bolder" || tl == "lighter" {
                Value::Keyword(tl.clone())
            } else {
                Value::Length(tl.parse::<f32>().unwrap_or(400.0), Unit::Number)
            });
        } else if is_stretch(&tl) {
            if stretch_v.is_some() {
                return Vec::new();
            }
            stretch_v = Some(tl.clone());
        } else {
            return Vec::new(); // 미인식 토큰(oldstyle-nums 등) → 무효
        }
    }
    // size[/line-height] — 슬래시 주변 공백 허용(계산값 round-trip "16px / 1.2").
    let (size_str, lh_str, family_start): (String, Option<String>, usize) =
        if let Some(slash) = tokens[si].find('/') {
            let (a, b) = tokens[si].split_at(slash);
            let after = &b[1..];
            if after.is_empty() {
                (a.to_string(), tokens.get(si + 1).map(|s| s.to_string()), si + 2)
            } else {
                (a.to_string(), Some(after.to_string()), si + 1)
            }
        } else if tokens.get(si + 1).map_or(false, |t| t.starts_with('/')) {
            let rest = &tokens[si + 1][1..];
            if rest.is_empty() {
                (tokens[si].to_string(), tokens.get(si + 2).map(|s| s.to_string()), si + 3)
            } else {
                (tokens[si].to_string(), Some(rest.to_string()), si + 2)
            }
        } else {
            (tokens[si].to_string(), None, si + 1)
        };
    let size_val = match interpret_value(&size_str) {
        Some(v @ (Value::Length(..) | Value::Calc(..) | Value::MinMax(..))) => Some(v),
        _ => {
            let sl = size_str.to_ascii_lowercase();
            if matches!(sl.as_str(), "larger" | "smaller") {
                Some(Value::Keyword(sl)) // 상대 크기 키워드 보존
            } else {
                font_size_keyword(&size_str).map(|px| Value::Length(px, Unit::Px))
            }
        }
    };
    let Some(size_val) = size_val else {
        return Vec::new();
    };
    // family 필수(§CSS Fonts): size(/line-height) 뒤 나머지 전부. 없거나 무효면 무효.
    if family_start >= tokens.len() {
        return Vec::new();
    }
    let family = tokens[family_start..].join(" ");
    if !font_family_valid(&family) {
        return Vec::new();
    }
    // line-height 있으면 유효성 검사(무효면 단축 전체 무효).
    let lh_decls = match &lh_str {
        Some(lh) => {
            let d = expand_declaration("line-height", lh); // 무단위→factor, 길이→그대로
            if d.is_empty() {
                return Vec::new();
            }
            Some(d)
        }
        None => None,
    };
    // 전체 리셋 방출: 지정 안 된 성분은 초기값(normal → weight 400 등). 계산값
    // 재조립(getComputedStyle().font)이 개별 longhand 설정과 일치하도록 한다.
    let kw = |s: &str| Value::Keyword(s.to_string());
    out.push(Declaration {
        important: false,
        name: "font-style".to_string(),
        value: style_v.unwrap_or_else(|| kw("normal")),
    });
    out.push(Declaration {
        important: false,
        name: "font-variant-caps".to_string(),
        value: kw(if variant_scaps { "small-caps" } else { "normal" }),
    });
    out.push(Declaration {
        important: false,
        name: "font-weight".to_string(),
        // 미지정 시 초기값 400(수치) — normal 키워드로 두면 계산값이 "normal" 로 새어
        // font-weight-computed 가 깨진다(계산 font-weight 는 수치, §CSS Fonts).
        value: weight_v.unwrap_or_else(|| Value::Length(400.0, Unit::Number)),
    });
    out.push(Declaration {
        important: false,
        name: "font-stretch".to_string(),
        value: kw(stretch_v.as_deref().unwrap_or("normal")),
    });
    out.push(Declaration { important: false, name: "font-size".to_string(), value: size_val });
    match lh_decls {
        Some(d) => out.extend(d),
        None => out.push(Declaration {
            important: false,
            name: "line-height".to_string(),
            value: kw("normal"),
        }),
    }
    out.push(Declaration {
        important: false,
        name: "font-family".to_string(),
        value: Value::Keyword(family),
    });
    out
}

// 논리 양방향 속성(margin-inline 등) → 두 물리 속성. 1값=양쪽, 2값=start/end.
// place-items/place-content/place-self 단축(§CSS Box Alignment): <align> <justify>?.
// 각 절반이 1~2 토큰 정렬값이라 align 을 길게(2토큰) 먼저 시도해 greedy 분할한다.
// align/justify 파라미터: (is_content, allow_auto, allow_lr, allow_legacy, allow_baseline).
type AlignParams = (bool, bool, bool, bool, bool);
fn place_shorthand(axis: &str, value_text: &str, ap: AlignParams, jp: AlignParams) -> Vec<Declaration> {
    let low = value_text.trim().to_ascii_lowercase();
    if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
        return vec![
            Declaration { important: false, name: format!("align-{}", axis), value: Value::Keyword(low.clone()) },
            Declaration { important: false, name: format!("justify-{}", axis), value: Value::Keyword(low) },
        ];
    }
    let toks: Vec<&str> = split_top_level(value_text.trim());
    let n = toks.len();
    if n == 0 || n > 4 {
        return Vec::new();
    }
    let valid = |slice: &[&str], p: AlignParams| {
        crate::css::alignment_valid(&slice.join(" "), p.0, p.1, p.2, p.3, p.4)
    };
    for align_len in [2usize, 1] {
        if align_len > n {
            continue;
        }
        let align = &toks[..align_len];
        if !valid(align, ap) {
            continue;
        }
        let rest = &toks[align_len..];
        let (a_val, j_val) = if rest.is_empty() {
            // 단일값은 양축에 적용. 단 justify 축에서 무효인 값(place-content 의 baseline)은
            // start 로 대체된다(§CSS Box Alignment §7).
            let j = if valid(align, jp) { align.join(" ") } else { "start".to_string() };
            (align.join(" "), j)
        } else if valid(rest, jp) {
            (align.join(" "), rest.join(" "))
        } else {
            continue;
        };
        return vec![
            Declaration { important: false, name: format!("align-{}", axis), value: Value::Keyword(crate::css::alignment_canonical(&a_val)) },
            Declaration { important: false, name: format!("justify-{}", axis), value: Value::Keyword(crate::css::alignment_canonical(&j_val)) },
        ];
    }
    Vec::new()
}

// 정렬 프로퍼티 arm(§CSS Box Alignment): CSS-wide 통과, 문법 검증 후 원문(소문자) 보존.
fn align_arm(
    name: &str,
    value_text: &str,
    is_content: bool,
    allow_auto: bool,
    allow_lr: bool,
    allow_legacy: bool,
    allow_baseline: bool,
) -> Vec<Declaration> {
    let low = value_text.trim().to_ascii_lowercase();
    if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
        return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
    }
    if crate::css::alignment_valid(value_text, is_content, allow_auto, allow_lr, allow_legacy, allow_baseline) {
        vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(crate::css::alignment_canonical(&low)) }]
    } else {
        Vec::new()
    }
}

// scroll-margin/scroll-padding 한 변(§CSS Scroll Snap). is_padding 이면 auto|
// <length-percentage> 비음수, 아니면 <length>. 무효면 빈 선언.
fn scroll_side(name: &str, value_text: &str, is_padding: bool) -> Vec<Declaration> {
    let low = value_text.trim().to_ascii_lowercase();
    if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
        return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(low) }];
    }
    let toks = split_top_level(value_text.trim());
    if toks.len() != 1 {
        return Vec::new();
    }
    let ok = if is_padding {
        crate::css::scroll_padding_valid(toks[0])
    } else {
        crate::css::scroll_margin_valid(toks[0])
    };
    if !ok {
        return Vec::new();
    }
    let v = if toks[0].eq_ignore_ascii_case("auto") {
        Value::Keyword("auto".to_string())
    } else {
        match interpret_value(toks[0]) {
            Some(Value::Length(n, Unit::Number)) if n == 0.0 => Value::Length(0.0, Unit::Px),
            Some(other) => other,
            None => Value::Keyword(low),
        }
    };
    vec![Declaration { important: false, name: name.to_string(), value: v }]
}

// scroll-margin/scroll-padding 단축(§CSS Scroll Snap): 1~4 값 → 네 변.
fn scroll_box(prefix: &str, value_text: &str, is_padding: bool) -> Vec<Declaration> {
    let low = value_text.trim().to_ascii_lowercase();
    if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
        return ["top", "right", "bottom", "left"]
            .iter()
            .map(|s| Declaration { important: false, name: format!("{}-{}", prefix, s), value: Value::Keyword(low.clone()) })
            .collect();
    }
    let toks = split_top_level(value_text.trim());
    if toks.is_empty() || toks.len() > 4 {
        return Vec::new();
    }
    let sides: [&str; 4] = match toks.len() {
        1 => [toks[0], toks[0], toks[0], toks[0]],
        2 => [toks[0], toks[1], toks[0], toks[1]],
        3 => [toks[0], toks[1], toks[2], toks[1]],
        _ => [toks[0], toks[1], toks[2], toks[3]],
    };
    let mut out = Vec::new();
    for (name_side, val) in ["top", "right", "bottom", "left"].iter().zip(sides.iter()) {
        let decls = scroll_side(&format!("{}-{}", prefix, name_side), val, is_padding);
        if decls.is_empty() {
            return Vec::new(); // 한 변이라도 무효면 단축 전체 무효
        }
        out.extend(decls);
    }
    out
}

fn logical_pair(start: &str, end: &str, value_text: &str) -> Vec<Declaration> {
    let toks: Vec<&str> = split_top_level(value_text);
    // 1~2 값만. CSS-wide 키워드는 단독만(두 값과 혼합 불가). 각 값의 물리 확장이
    // 비면(무효 값) 단축 전체 무효.
    if toks.is_empty() || toks.len() > 2 {
        return Vec::new();
    }
    let is_csswide = |t: &str| {
        matches!(
            t.to_ascii_lowercase().as_str(),
            "inherit" | "initial" | "unset" | "revert" | "revert-layer"
        )
    };
    if toks.len() == 2 && (is_csswide(toks[0]) || is_csswide(toks[1])) {
        return Vec::new();
    }
    let s = toks[0];
    let e = toks.get(1).copied().unwrap_or(s);
    let start_exp = expand_declaration(start, s);
    let end_exp = expand_declaration(end, e);
    if start_exp.is_empty() || end_exp.is_empty() {
        return Vec::new();
    }
    let mut out = start_exp;
    out.extend(end_exp);
    out
}

// animation 단축 토큰이 애니메이션 이름인지 (시간·타이밍·방향·반복 등 키워드 제외).
fn is_animation_name(t: &str) -> bool {
    if t.ends_with("ms") || t.ends_with('s') && t[..t.len() - 1].chars().all(|c| c.is_ascii_digit() || c == '.') {
        return false; // 시간
    }
    if t.parse::<f32>().is_ok() {
        return false; // 반복 횟수
    }
    !matches!(
        t,
        "ease" | "linear" | "ease-in" | "ease-out" | "ease-in-out" | "step-start" | "step-end"
            | "infinite" | "normal" | "reverse" | "alternate" | "alternate-reverse" | "none"
            | "forwards" | "backwards" | "both" | "running" | "paused" | "initial" | "inherit"
    ) && t.chars().next().map(|c| c.is_ascii_alphabetic() || c == '-' || c == '_').unwrap_or(false)
}

// CSS 문자열 이스케이프 해석: \XXXX(최대 6자리 16진 코드포인트, 뒤 공백 1개 흡수)와
// \c(리터럴). 아이콘 폰트 content: "\f001" 등에 필요.
fn decode_css_escapes(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        // 16진 이스케이프
        let mut hex = String::new();
        while hex.len() < 6 && chars.peek().map(|c| c.is_ascii_hexdigit()).unwrap_or(false) {
            hex.push(chars.next().unwrap());
        }
        if !hex.is_empty() {
            // 이스케이프 뒤 공백 1개는 구분자로 흡수
            if chars.peek() == Some(&' ') {
                chars.next();
            }
            if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                out.push(ch);
            }
        } else if let Some(lit) = chars.next() {
            out.push(lit); // \" \\ 등 리터럴 이스케이프
        }
    }
    out
}

// 괄호 깊이를 고려해 공백으로 최상위 토큰 분리 (rgb(1, 2, 3) 는 한 토큰).
// 값 토큰화의 유일한 규칙: 괄호 안(함수 인자)의 공백·콤마는 구분자가 아니다.
// 예전엔 단축 프로퍼티들이 split_whitespace()/split(',') 를 그대로 써서
// `border: 1px solid rgba(0, 0, 0, .1)` 의 색이 통째로 사라지고
// `background: rgb(1,2,3)` 은 배경이 아예 안 칠해졌다 (아주 흔한 표기다).
// 괄호·따옴표 **밖**의 '/' 만 공백으로 감싼다. url(a/b) 안의 슬래시는 건드리지 않는다.
fn space_top_level_slashes(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    for c in text.chars() {
        match c {
            '\'' | '"' if quote.is_none() => {
                quote = Some(c);
                out.push(c);
            }
            q if Some(q) == quote => {
                quote = None;
                out.push(q);
            }
            '(' if quote.is_none() => {
                depth += 1;
                out.push(c);
            }
            ')' if quote.is_none() => {
                depth = depth.saturating_sub(1);
                out.push(c);
            }
            '/' if quote.is_none() && depth == 0 => {
                out.push(' ');
                out.push('/');
                out.push(' ');
            }
            _ => out.push(c),
        }
    }
    out
}

// animation 단축 → 여덟 롱핸드. 첫 시간=duration, 둘째=delay. 나머지는 키워드/수로.
fn animation_shorthand(value_text: &str) -> Vec<Declaration> {
    let kw = |name: &str, val: String| Declaration {
        important: false,
        name: name.to_string(),
        value: Value::Keyword(val),
    };
    let is_time = |s: &str| -> bool {
        s.strip_suffix("ms").map(|n| n.parse::<f32>().is_ok()).unwrap_or(false)
            || s.strip_suffix('s').map(|n| n.parse::<f32>().is_ok()).unwrap_or(false)
    };
    let is_timing = |s: &str| -> bool {
        matches!(s, "ease" | "linear" | "ease-in" | "ease-out" | "ease-in-out" | "step-start" | "step-end")
            || s.starts_with("cubic-bezier(")
            || s.starts_with("steps(")
    };
    let (mut names, mut durs, mut tfs, mut delays, mut iters, mut dirs, mut fills, mut states) =
        (vec![], vec![], vec![], vec![], vec![], vec![], vec![], vec![]);
    for part in split_top_level_commas(value_text.trim()) {
        let (mut name, mut times, mut tf, mut iter, mut dir, mut fill, mut state): (
            Option<String>, Vec<String>, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>,
        ) = (None, vec![], None, None, None, None, None);
        // 각 컴포넌트는 레이어당 최대 1회 — 중복이면 선언 전체 무효(§CSS Animations).
        for tok in split_top_level(part.trim()) {
            let tl = tok.to_ascii_lowercase();
            if is_time(&tl) {
                if times.len() >= 2 {
                    return Vec::new(); // 시간값은 duration/delay 둘까지
                }
                times.push(tok.to_string());
            } else if is_timing(&tl) {
                if tf.is_some() {
                    return Vec::new();
                }
                tf = Some(tok.to_string());
            } else if tl == "infinite" || tl.parse::<f32>().is_ok() {
                if iter.is_some() {
                    return Vec::new();
                }
                iter = Some(tok.to_string());
            } else if matches!(tl.as_str(), "normal" | "reverse" | "alternate" | "alternate-reverse") {
                if dir.is_some() {
                    return Vec::new();
                }
                dir = Some(tl.clone());
            } else if matches!(tl.as_str(), "forwards" | "backwards" | "both") {
                if fill.is_some() {
                    return Vec::new();
                }
                fill = Some(tl.clone());
            } else if matches!(tl.as_str(), "running" | "paused") {
                if state.is_some() {
                    return Vec::new();
                }
                state = Some(tl.clone());
            } else {
                if name.is_some() {
                    return Vec::new(); // 이름(custom-ident)은 레이어당 하나
                }
                name = Some(tok.to_string());
            }
        }
        // animation-duration(첫 시간값)은 음수 불가(delay 는 음수 허용).
        if times.first().map(|d| d.trim().starts_with('-')).unwrap_or(false) {
            return Vec::new();
        }
        names.push(name.unwrap_or_else(|| "none".to_string()));
        // animation-duration 초기값은 auto(§CSS Animations 2, 스크롤 구동).
        durs.push(times.first().cloned().unwrap_or_else(|| "auto".to_string()));
        delays.push(times.get(1).cloned().unwrap_or_else(|| "0s".to_string()));
        tfs.push(tf.unwrap_or_else(|| "ease".to_string()));
        iters.push(iter.unwrap_or_else(|| "1".to_string()));
        dirs.push(dir.unwrap_or_else(|| "normal".to_string()));
        fills.push(fill.unwrap_or_else(|| "none".to_string()));
        states.push(state.unwrap_or_else(|| "running".to_string()));
    }
    vec![
        kw("animation-name", names.join(", ")),
        kw("animation-duration", durs.join(", ")),
        kw("animation-timing-function", tfs.join(", ")),
        kw("animation-delay", delays.join(", ")),
        kw("animation-iteration-count", iters.join(", ")),
        kw("animation-direction", dirs.join(", ")),
        kw("animation-fill-mode", fills.join(", ")),
        kw("animation-play-state", states.join(", ")),
        // animation 단축은 스크롤 구동 롱핸드도 초기값으로 리셋한다(§CSS Animations 2).
        kw("animation-timeline", "auto".to_string()),
        kw("animation-range-start", "normal".to_string()),
        kw("animation-range-end", "normal".to_string()),
        kw("animation", value_text.trim().to_string()),
    ]
}

// transition 단축 → 다섯 롱핸드. 각 롱핸드는 콤마 목록. 첫 시간값=duration,
// 둘째=delay. timing-function/behavior/property 는 키워드로 구분.
fn transition_shorthand(value_text: &str) -> Vec<Declaration> {
    let kw = |name: &str, val: String| Declaration {
        important: false,
        name: name.to_string(),
        value: Value::Keyword(val),
    };
    let vt = value_text.trim();
    // CSS-wide 키워드는 단축 전체에 적용 — 다섯 롱핸드에 그대로 전파.
    let low = vt.to_ascii_lowercase();
    if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
        return vec![
            kw("transition-property", low.clone()),
            kw("transition-duration", low.clone()),
            kw("transition-timing-function", low.clone()),
            kw("transition-delay", low.clone()),
            kw("transition-behavior", low.clone()),
            kw("transition", low),
        ];
    }
    let is_time = |s: &str| -> bool {
        if let Some(n) = s.strip_suffix("ms") {
            n.parse::<f32>().is_ok()
        } else if let Some(n) = s.strip_suffix('s') {
            n.parse::<f32>().is_ok()
        } else {
            false
        }
    };
    let is_timing = |s: &str| -> bool {
        matches!(
            s,
            "ease" | "linear" | "ease-in" | "ease-out" | "ease-in-out" | "step-start" | "step-end"
        ) || s.starts_with("cubic-bezier(")
            || s.starts_with("steps(")
    };
    if vt.eq_ignore_ascii_case("none") {
        return vec![
            kw("transition-property", "none".to_string()),
            kw("transition-duration", "0s".to_string()),
            kw("transition-timing-function", "ease".to_string()),
            kw("transition-delay", "0s".to_string()),
            kw("transition-behavior", "normal".to_string()),
            kw("transition", "none".to_string()),
        ];
    }
    let (mut props, mut durs, mut tfs, mut delays, mut behs) =
        (vec![], vec![], vec![], vec![], vec![]);
    for part in split_top_level_commas(vt) {
        let (mut prop, mut times, mut tf, mut beh): (
            Option<String>,
            Vec<String>,
            Option<String>,
            Option<String>,
        ) = (None, vec![], None, None);
        // 레이어당 각 성분은 최대 1회(시간은 최대 2회: duration, delay). 초과·미인식
        // 토큰·잘못된 프로퍼티 ident 는 단축 전체를 무효로 만든다(§CSS Transitions).
        for tok in split_top_level(part.trim()) {
            let tl = tok.to_ascii_lowercase();
            if is_time(&tl) {
                if times.len() >= 2 {
                    return Vec::new();
                }
                times.push(tok.to_string());
            } else if is_timing(&tl) {
                if tf.is_some() {
                    return Vec::new();
                }
                tf = Some(tok.to_string());
            } else if tl == "allow-discrete" || tl == "normal" {
                if beh.is_some() {
                    return Vec::new();
                }
                beh = Some(tl);
            } else {
                // 프로퍼티는 레이어당 하나, 유효 <single-transition-property> 여야 한다.
                if prop.is_some() || !crate::css::single_transition_property_valid(tok) {
                    return Vec::new();
                }
                prop = Some(tok.to_string());
            }
        }
        // 첫 시간은 duration(음수 불가), 둘째는 delay(음수 허용).
        if let Some(d) = times.first() {
            if d.trim().starts_with('-') {
                return Vec::new();
            }
        }
        props.push(prop.unwrap_or_else(|| "all".to_string()));
        durs.push(times.first().cloned().unwrap_or_else(|| "0s".to_string()));
        delays.push(times.get(1).cloned().unwrap_or_else(|| "0s".to_string()));
        tfs.push(tf.unwrap_or_else(|| "ease".to_string()));
        behs.push(beh.unwrap_or_else(|| "normal".to_string()));
    }
    vec![
        kw("transition-property", props.join(", ")),
        kw("transition-duration", durs.join(", ")),
        kw("transition-timing-function", tfs.join(", ")),
        kw("transition-delay", delays.join(", ")),
        kw("transition-behavior", behs.join(", ")),
        kw("transition", vt.to_string()),
    ]
}

fn split_top_level(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start: Option<usize> = None;
    for (i, c) in text.char_indices() {
        match c {
            '(' => {
                depth += 1;
                start.get_or_insert(i);
            }
            ')' => {
                depth -= 1;
                start.get_or_insert(i);
            }
            c if c.is_whitespace() && depth == 0 => {
                if let Some(st) = start.take() {
                    out.push(&text[st..i]);
                }
            }
            _ => {
                start.get_or_insert(i);
            }
        }
    }
    if let Some(st) = start {
        out.push(&text[st..]);
    }
    out
}

// grid-template 토큰화: [..] · ".." · (..) 는 한 토큰으로 유지, 그 외 공백 분리.
fn tokenize_grid_template(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    let mut bracket = false;
    let mut quote: Option<char> = None;
    for c in s.chars() {
        if let Some(q) = quote {
            cur.push(c);
            if c == q {
                quote = None;
                out.push(std::mem::take(&mut cur));
            }
            continue;
        }
        match c {
            '"' | '\'' => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                quote = Some(c);
                cur.push(c);
            }
            '[' => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                bracket = true;
                cur.push(c);
            }
            ']' => {
                cur.push(c);
                bracket = false;
                out.push(std::mem::take(&mut cur));
            }
            '(' => {
                depth += 1;
                cur.push(c);
            }
            ')' => {
                depth -= 1;
                cur.push(c);
            }
            c if c.is_whitespace() && depth == 0 && !bracket => {
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

// "..." 문자열이면 내부 셀 개수(공백 분리)를 돌려준다. 셀이 0이면 None.
// 각 셀은 <custom-ident> 또는 하나 이상의 '.'(null 셀).
fn grid_area_string_cells(tok: &str) -> Option<usize> {
    let t = tok.trim();
    let b = t.as_bytes();
    if b.len() < 2 || (b[0] != b'"' && b[0] != b'\'') || b[b.len() - 1] != b[0] {
        return None;
    }
    let inner = &t[1..t.len() - 1];
    let cells: Vec<&str> = inner.split_whitespace().collect();
    if cells.is_empty() {
        return None;
    }
    for cell in &cells {
        let is_null = cell.chars().all(|c| c == '.');
        let is_ident = !cell.is_empty() && !is_null && crate::css::is_css_ident(cell);
        if !is_null && !is_ident {
            return None;
        }
    }
    Some(cells.len())
}

// "..." 문자열 캐논: 셀을 공백 하나로 재결합.
fn grid_area_string_canonical(tok: &str) -> String {
    let t = tok.trim();
    let inner = &t[1..t.len() - 1];
    let cells: Vec<&str> = inner.split_whitespace().collect();
    format!("\"{}\"", cells.join(" "))
}

// [..] 이면 내부 문자열(대괄호 제거)을 돌려준다.
fn grid_bracket_inner(tok: &str) -> Option<&str> {
    let t = tok.trim();
    if t.starts_with('[') && t.ends_with(']') {
        Some(&t[1..t.len() - 1])
    } else {
        None
    }
}

// grid-template 문자열 템플릿 형식 파싱(§CSS Grid):
//   [ <line-names>? <string> <track-size>? <line-names>? ]+ [ / <explicit-track-list> ]?
// (rows, columns, areas) 캐논을 돌려준다. 문법 위반 시 None.
fn expand_grid_template_strings(template_part: &str, columns_part: Option<&str>) -> Option<(String, String, String)> {
    let tokens = tokenize_grid_template(template_part);
    let mut slot_names: Vec<Vec<String>> = vec![Vec::new()];
    let mut slot_groups: Vec<usize> = vec![0];
    let mut strings: Vec<String> = Vec::new();
    let mut tracks: Vec<Option<String>> = Vec::new();
    let mut just_after_string = false;
    let mut ncols: Option<usize> = None;
    for tok in &tokens {
        if let Some(inner) = grid_bracket_inner(tok) {
            let idents: Vec<String> = inner.split_whitespace().map(|s| s.to_string()).collect();
            for id in &idents {
                if !crate::css::is_css_ident(id) {
                    return None;
                }
            }
            let cur = slot_names.len() - 1;
            slot_names[cur].extend(idents);
            slot_groups[cur] += 1;
            just_after_string = false;
        } else if tok.starts_with('"') || tok.starts_with('\'') {
            let cells = grid_area_string_cells(tok)?;
            match ncols {
                Some(n) if n != cells => return None,
                None => ncols = Some(cells),
                _ => {}
            }
            strings.push(grid_area_string_canonical(tok));
            tracks.push(None);
            slot_names.push(Vec::new());
            slot_groups.push(0);
            just_after_string = true;
        } else {
            // track-size(문자열 뒤에서만, 행마다 하나). none 은 track-size 가 아니다.
            if !just_after_string {
                return None;
            }
            if tok.eq_ignore_ascii_case("none") || !crate::css::grid_template_track_valid(tok) {
                return None;
            }
            let last = tracks.len() - 1;
            if tracks[last].is_some() {
                return None;
            }
            tracks[last] = Some(crate::css::grid_template_track_canonical(tok));
            just_after_string = false;
        }
    }
    if strings.is_empty() {
        return None;
    }
    // line-names 그룹 수 제한: 경계(맨 앞/뒤) ≤1, 문자열 사이 ≤2.
    let n = slot_names.len();
    for (i, &gc) in slot_groups.iter().enumerate() {
        let limit = if i == 0 || i == n - 1 { 1 } else { 2 };
        if gc > limit {
            return None;
        }
    }
    // rows 직렬화: L0 T0 L1 T1 ... T(k-1) L(k). 빈 line-names 생략, track 기본 auto.
    let mut parts: Vec<String> = Vec::new();
    for i in 0..strings.len() {
        if !slot_names[i].is_empty() {
            parts.push(format!("[{}]", slot_names[i].join(" ")));
        }
        parts.push(tracks[i].clone().unwrap_or_else(|| "auto".to_string()));
    }
    if !slot_names[strings.len()].is_empty() {
        parts.push(format!("[{}]", slot_names[strings.len()].join(" ")));
    }
    let rows = parts.join(" ");
    let areas = strings.join(" ");
    let columns = match columns_part {
        None => "none".to_string(),
        Some(c) => {
            // 슬래시 뒤 columns 는 <explicit-track-list> — none 불가.
            let c = c.trim();
            if c.is_empty() || c.eq_ignore_ascii_case("none") || !crate::css::grid_template_track_valid(c) {
                return None;
            }
            crate::css::grid_template_track_canonical(c)
        }
    };
    Some((rows, columns, areas))
}

// grid-template 값(none | rows/cols | 문자열 템플릿) → (rows, columns, areas) 캐논.
fn expand_grid_template_value(v: &str) -> Option<(String, String, String)> {
    let low = v.trim().to_ascii_lowercase();
    if low == "none" {
        return Some(("none".to_string(), "none".to_string(), "none".to_string()));
    }
    let has_string = v.contains('"') || v.contains('\'');
    let parts = split_top_slash_pub(v);
    if !has_string {
        // <rows> / <columns>
        if parts.len() == 2 {
            let (r, c) = (parts[0].trim(), parts[1].trim());
            if !r.is_empty()
                && !c.is_empty()
                && crate::css::grid_template_track_valid(r)
                && crate::css::grid_template_track_valid(c)
            {
                return Some((
                    crate::css::grid_template_track_canonical(r),
                    crate::css::grid_template_track_canonical(c),
                    "none".to_string(),
                ));
            }
        }
        return None;
    }
    // 문자열 템플릿(슬래시로 columns 분리 가능, 최대 1개).
    match parts.len() {
        1 => expand_grid_template_strings(v, None),
        2 => expand_grid_template_strings(parts[0].trim(), Some(parts[1].trim())),
        _ => None,
    }
}

// grid-template 단축의 캐논 직렬화(§CSSOM). 지정값을 최소 형태로:
//  none / none → none, 빈 [] 생략, 문자열 형식은 기본 auto 트랙 생략.
pub(crate) fn grid_template_canonical(raw: &str) -> String {
    let (rows, cols, areas) = match expand_grid_template_value(raw.trim()) {
        Some(t) => t,
        None => return raw.trim().to_string(),
    };
    if areas == "none" {
        if rows == "none" && cols == "none" {
            return "none".to_string();
        }
        return format!("{rows} / {cols}");
    }
    // 문자열 형식: rows 의 라인네임/트랙 사이에 area 문자열을 끼워 넣는다.
    let strings: Vec<String> = tokenize_grid_template(&areas)
        .into_iter()
        .filter(|t| t.starts_with('"') || t.starts_with('\''))
        .collect();
    let mut out: Vec<String> = Vec::new();
    let mut si = 0usize;
    for tok in tokenize_grid_template(&rows) {
        if tok.starts_with('[') {
            out.push(tok);
        } else {
            // 트랙 앞에 해당 행의 문자열을 넣고, 기본 auto 트랙은 생략.
            if si < strings.len() {
                out.push(strings[si].clone());
                si += 1;
            }
            if !tok.eq_ignore_ascii_case("auto") {
                out.push(tok);
            }
        }
    }
    let mut s = out.join(" ");
    if cols != "none" {
        s = format!("{s} / {cols}");
    }
    s
}

// grid 단축의 캐논 직렬화(§CSSOM). auto-flow 형식은 auto-flow 를 dense 앞으로
// 정렬하고, 그 외(template 형식)는 grid_template_canonical 에 위임한다.
pub(crate) fn grid_canonical(raw: &str) -> String {
    let v = raw.trim();
    let parts = split_top_slash_pub(v);
    if parts.len() == 2 {
        let first_af = |s: &str| {
            s.split_whitespace()
                .next()
                .map(|t| t.eq_ignore_ascii_case("auto-flow") || t.eq_ignore_ascii_case("dense"))
                .unwrap_or(false)
        };
        let (l, r) = (parts[0].trim(), parts[1].trim());
        let parse_af = |s: &str| -> Option<(bool, String)> {
            let toks = split_top_level(s);
            let (mut has_af, mut dense, mut i) = (false, false, 0usize);
            while i < toks.len() {
                match toks[i].to_ascii_lowercase().as_str() {
                    "auto-flow" if !has_af => has_af = true,
                    "dense" if !dense => dense = true,
                    _ => break,
                }
                i += 1;
            }
            if !has_af {
                return None;
            }
            Some((dense, toks[i..].join(" ")))
        };
        let af_str = |dense: bool, track: &str| {
            let d = if dense { " dense" } else { "" };
            let t = if track.trim().is_empty() { String::new() } else { format!(" {}", track.trim()) };
            format!("auto-flow{d}{t}")
        };
        if first_af(l) && !first_af(r) {
            if let Some((dense, ar)) = parse_af(l) {
                return format!("{} / {}", af_str(dense, &ar), crate::css::grid_template_track_canonical(r));
            }
        } else if first_af(r) && !first_af(l) {
            if let Some((dense, ac)) = parse_af(r) {
                return format!("{} / {}", crate::css::grid_template_track_canonical(l), af_str(dense, &ac));
            }
        }
    }
    grid_template_canonical(v)
}

// border-image-source 토큰인가: none 또는 <image>(url()/그래디언트/image-set 등).
fn is_bi_image_token(t: &str) -> bool {
    let low = t.to_ascii_lowercase();
    if low == "none" {
        return true;
    }
    if !low.ends_with(')') {
        return false;
    }
    const IMG_FNS: &[&str] = &[
        "url(", "src(", "image(", "image-set(", "-webkit-image-set(", "cross-fade(",
        "-webkit-cross-fade(", "element(", "paint(", "linear-gradient(", "radial-gradient(",
        "conic-gradient(", "repeating-linear-gradient(", "repeating-radial-gradient(",
        "repeating-conic-gradient(", "-webkit-linear-gradient(", "-webkit-radial-gradient(",
        "-webkit-repeating-linear-gradient(", "-webkit-repeating-radial-gradient(",
        "-webkit-gradient(", "-moz-linear-gradient(", "-o-linear-gradient(",
    ];
    IMG_FNS.iter().any(|f| low.starts_with(f))
}

fn is_bi_repeat_kw(t: &str) -> bool {
    matches!(t.to_ascii_lowercase().as_str(), "stretch" | "repeat" | "round" | "space")
}

// border-image 단축을 5개 롱핸드로 전개(§CSS Backgrounds). 문법 위반 시 None.
// source/repeat 는 B(slice-group) 바깥에서 앞뒤로만 등장하고 각각 한 번씩 연속.
fn expand_border_image(value: &str) -> Option<Vec<Declaration>> {
    let segs = split_top_slash_pub(value);
    let ns = segs.len();
    if ns == 0 || ns > 3 {
        return None;
    }

    let mut source: Option<String> = None;
    let mut repeat_toks: Vec<String> = Vec::new();
    let mut repeat_side: Option<bool> = None; // false=앞, true=뒤

    // seg0 앞쪽에서 source/repeat 벗겨내기.
    let mut s0: Vec<String> = split_top_level(&segs[0]).iter().map(|s| s.to_string()).collect();
    while let Some(first) = s0.first().cloned() {
        if source.is_none() && is_bi_image_token(&first) {
            source = Some(first);
            s0.remove(0);
        } else if is_bi_repeat_kw(&first) && repeat_toks.len() < 2 {
            if repeat_side == Some(true) {
                return None; // 뒤에서 이미 집음 → 비연속
            }
            repeat_side = Some(false);
            repeat_toks.push(first);
            s0.remove(0);
        } else {
            break;
        }
    }
    // 슬래시가 없으면(ns==1) seg0 뒤쪽에서도 벗겨낸다.
    if ns == 1 {
        while let Some(last) = s0.last().cloned() {
            if source.is_none() && is_bi_image_token(&last) {
                source = Some(last);
                s0.pop();
            } else if is_bi_repeat_kw(&last) && repeat_toks.len() < 2 {
                if repeat_side == Some(false) && !repeat_toks.is_empty() {
                    return None; // 앞에서 이미 집음 → 비연속
                }
                repeat_side = Some(true);
                repeat_toks.push(last);
                s0.pop();
            } else {
                break;
            }
        }
    }
    let slice_toks = s0;

    // width/outset 세그먼트 확정, 마지막 세그먼트 뒤쪽에서 source/repeat 벗겨내기.
    let mut width_seg: Option<Vec<String>> = None;
    let mut outset_seg: Option<Vec<String>> = None;
    if ns >= 2 {
        let mut last: Vec<String> = split_top_level(&segs[ns - 1]).iter().map(|s| s.to_string()).collect();
        while let Some(lt) = last.last().cloned() {
            if source.is_none() && is_bi_image_token(&lt) {
                source = Some(lt);
                last.pop();
            } else if is_bi_repeat_kw(&lt) && repeat_toks.len() < 2 {
                if repeat_side == Some(false) && !repeat_toks.is_empty() {
                    return None;
                }
                repeat_side = Some(true);
                repeat_toks.push(lt);
                last.pop();
            } else {
                break;
            }
        }
        if ns == 2 {
            width_seg = Some(last);
        } else {
            outset_seg = Some(last);
            width_seg = Some(split_top_level(&segs[1]).iter().map(|s| s.to_string()).collect());
        }
    }

    // 최소 한 컴포넌트는 있어야 한다.
    if source.is_none() && repeat_toks.is_empty() && slice_toks.is_empty() && ns == 1 {
        return None;
    }

    // slice.
    let slice_str = if slice_toks.is_empty() {
        if ns > 1 {
            return None; // '/ width' 앞에 slice 필수
        }
        "100%".to_string()
    } else {
        let joined = slice_toks.join(" ");
        if !crate::css::border_image_slice_valid(&joined) {
            return None;
        }
        crate::css::border_image_slice_canonical(&joined)
    };
    // width.
    let width_str = match &width_seg {
        None => "1".to_string(),
        Some(w) if w.is_empty() => "1".to_string(),
        Some(w) => {
            let joined = w.join(" ");
            if !crate::css::border_image_width_valid(&joined) {
                return None;
            }
            crate::css::border_image_box_canonical(&joined)
        }
    };
    // outset.
    let outset_str = match &outset_seg {
        None => "0".to_string(),
        Some(o) if o.is_empty() => return None, // 둘째 슬래시 뒤 outset 필수
        Some(o) => {
            let joined = o.join(" ");
            if !crate::css::border_image_outset_valid(&joined) {
                return None;
            }
            crate::css::border_image_box_canonical(&joined)
        }
    };
    // repeat.
    let repeat_str = if repeat_toks.is_empty() {
        "stretch".to_string()
    } else {
        if repeat_side == Some(true) {
            repeat_toks.reverse();
        }
        let joined = repeat_toks.join(" ");
        if !crate::css::border_image_repeat_valid(&joined) {
            return None;
        }
        crate::css::border_image_repeat_canonical(&joined)
    };
    let source_str = source.unwrap_or_else(|| "none".to_string());

    Some(vec![
        Declaration { important: false, name: "border-image-source".to_string(), value: Value::Keyword(source_str) },
        Declaration { important: false, name: "border-image-slice".to_string(), value: Value::Keyword(slice_str) },
        Declaration { important: false, name: "border-image-width".to_string(), value: Value::Keyword(width_str) },
        Declaration { important: false, name: "border-image-outset".to_string(), value: Value::Keyword(outset_str) },
        Declaration { important: false, name: "border-image-repeat".to_string(), value: Value::Keyword(repeat_str) },
    ])
}

// 괄호 밖 '/' 로 분리 (grid-row/column/area 의 grid-line 구분).
// offset 단축(§CSS Motion Path) → 5개 롱핸드 전개.
// [ <offset-position>? [ <offset-path> [ <offset-distance> || <offset-rotate> ]? ]? ]!
// [ / <offset-anchor> ]?. 기본값: position normal, path none, distance 0px, rotate auto,
// anchor auto.
fn offset_shorthand(value_text: &str) -> Vec<Declaration> {
    let mk = |n: &str, v: String| Declaration { important: false, name: n.to_string(), value: Value::Keyword(v) };
    let low = value_text.trim().to_ascii_lowercase();
    if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
        return ["offset-position", "offset-path", "offset-distance", "offset-rotate", "offset-anchor"]
            .iter().map(|n| mk(n, low.clone())).collect();
    }
    let parts = split_top_slash_pub(value_text);
    if parts.is_empty() || parts.len() > 2 {
        return Vec::new();
    }
    // anchor(/ 뒤): auto | <position>.
    let anchor = if parts.len() == 2 {
        let a = parts[1].trim();
        if a.eq_ignore_ascii_case("auto") {
            "auto".to_string()
        } else if crate::css::position_valid(a) {
            a.to_string()
        } else {
            return Vec::new();
        }
    } else {
        "auto".to_string()
    };
    let toks = split_top_level(parts[0].trim());
    if toks.is_empty() {
        return Vec::new();
    }
    let is_path_ind = |t: &str| {
        let tl = t.to_ascii_lowercase();
        tl == "none"
            || (tl.ends_with(')')
                && ["ray(", "path(", "url(", "circle(", "ellipse(", "inset(", "polygon(",
                    "xywh(", "rect(", "shape(", "src("]
                    .iter().any(|p| tl.starts_with(p)))
    };
    let is_pos_comp = |t: &str| {
        matches!(
            t.to_ascii_lowercase().as_str(),
            "left" | "right" | "top" | "bottom" | "center" | "auto" | "normal"
        ) || crate::css::offset_len_ok(t)
    };
    // position: 선두 위치 성분(경로 지시자 전까지). offset-position 은 auto|normal|<position>.
    let mut i = 0;
    while i < toks.len() && !is_path_ind(toks[i]) && is_pos_comp(toks[i]) {
        i += 1;
    }
    let position = if i == 0 {
        "normal".to_string()
    } else {
        let p = toks[..i].join(" ");
        if !(p.eq_ignore_ascii_case("auto") || p.eq_ignore_ascii_case("normal") || crate::css::position_valid(&p)) {
            return Vec::new();
        }
        p
    };
    let (mut path, mut dist, mut rot) = ("none".to_string(), "0px".to_string(), "auto".to_string());
    if i < toks.len() {
        if !is_path_ind(toks[i]) {
            return Vec::new();
        }
        path = toks[i].to_string();
        i += 1;
        if !path.eq_ignore_ascii_case("none") && !crate::css::offset_path_valid(&path) {
            return Vec::new();
        }
        // distance || rotate (순서 자유).
        let (mut nd, mut nkw, mut nang) = (0u32, 0u32, 0u32);
        let mut rot_kw: Option<String> = None;
        let mut rot_ang: Option<String> = None;
        while i < toks.len() {
            let t = &toks[i];
            let tl = t.to_ascii_lowercase();
            if matches!(tl.as_str(), "auto" | "reverse") {
                nkw += 1;
                rot_kw = Some(tl);
            } else if super::values::math_angle_valid(t) {
                nang += 1;
                rot_ang = Some(t.to_string());
            } else if crate::css::offset_len_ok(t) {
                nd += 1;
                dist = t.to_string();
            } else {
                return Vec::new();
            }
            i += 1;
        }
        if nd > 1 || nkw > 1 || nang > 1 {
            return Vec::new();
        }
        if nkw + nang > 0 {
            let mut parts: Vec<String> = Vec::new();
            if let Some(k) = rot_kw {
                parts.push(k);
            }
            if let Some(a) = rot_ang {
                parts.push(a);
            }
            rot = parts.join(" ");
        }
    }
    vec![
        mk("offset-position", position),
        mk("offset-path", path),
        mk("offset-distance", dist),
        mk("offset-rotate", rot),
        mk("offset-anchor", anchor),
    ]
}

fn split_top_slash_pub(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for c in text.chars() {
        match c {
            '(' => { depth += 1; cur.push(c); }
            ')' => { depth -= 1; cur.push(c); }
            '/' if depth == 0 => { out.push(cur.trim().to_string()); cur.clear(); }
            _ => cur.push(c),
        }
    }
    out.push(cur.trim().to_string());
    out
}

// grid-line 이 순수 <custom-ident> 인가(정수·span·auto 아님).
// 단축에서 슬래시 생략 시 반대편으로 복사할지 판단.
fn grid_line_is_bare_ident(line: &str) -> bool {
    let toks: Vec<&str> = line.split_whitespace().collect();
    toks.len() == 1
        && toks[0].parse::<i64>().is_err()
        && !toks[0].eq_ignore_ascii_case("auto")
        && !toks[0].eq_ignore_ascii_case("span")
}

// 괄호 밖 콤마로만 분리 (background 의 레이어, font-family 목록 등).
fn split_top_level_commas(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    for c in text.chars() {
        match c {
            '(' => {
                depth += 1;
                cur.push(c);
            }
            ')' => {
                depth -= 1;
                cur.push(c);
            }
            ',' if depth == 0 => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

// background 단축 → background-color/image/repeat/size longhand.
// `background: #fff url(x) no-repeat center / cover` 처럼 repeat/size 도 추출해
// 기존 background-repeat/background-size 렌더 경로를 활성화한다. (position 은 렌더
// 미구현이라 아직 무시.)
fn background_shorthand(value_text: &str) -> Vec<Declaration> {
    let mut out = Vec::new();
    let mut image = None;
    let mut color = None;
    let mut repeat = None;
    let mut attachment: Option<String> = None;
    let mut boxes: Vec<String> = Vec::new();
    let mut size_tokens: Vec<String> = Vec::new();
    let mut pos_tokens: Vec<String> = Vec::new();

    // 레이어는 괄호 밖 콤마로만 나뉜다. 예전엔 그냥 split(',') 이라
    // `background: rgb(1,2,3)` 이 "rgb(1" 로 잘려 배경색이 통째로 사라졌다.
    // CSS 문법상 색은 마지막 레이어에만 올 수 있으므로, 이미지/반복/크기는 첫 레이어,
    // 색은 마지막 레이어에서 찾는다. (우리는 레이어 1장만 그린다)
    let layers = split_top_level_commas(value_text);
    // background-color 는 마지막 레이어에만 올 수 있다(§CSS Backgrounds). 그 외 레이어에
    // 색 토큰이 있으면 무효.
    if layers.len() > 1 {
        for lyr in &layers[..layers.len() - 1] {
            if split_top_level(lyr).iter().any(|t| matches!(interpret_value(t.trim()), Some(Value::Color(_)))) {
                return Vec::new();
            }
        }
    }
    let first = layers.first().cloned().unwrap_or_default();
    let last = layers.last().cloned().unwrap_or_default();
    let has_gradient = value_text.contains("gradient(");
    let layer: String = if has_gradient { value_text.to_string() } else { first };
    // "center/cover" 처럼 붙은 슬래시를 토큰화하기 위해 공백 삽입 (gradient 없을 때만).
    // "center/cover" 처럼 붙은 슬래시를 토큰화하려면 공백이 필요하다. 하지만 문자열을
    // 통째로 replace 하면 **url() 안의 경로 슬래시까지** 벌어져서
    // url(../tpl/images/x.gif) 가 url(.. / tpl / images / x.gif) 가 된다 (실제로 400/404 가 났다).
    // 괄호/따옴표 밖의 슬래시만 벌린다.
    let normalized = if has_gradient { layer.clone() } else { space_top_level_slashes(&layer) };
    // 마지막 레이어의 색 (첫 레이어에 색이 있으면 아래 루프가 덮어쓴다)
    if !has_gradient && layers.len() > 1 {
        for tok in split_top_level(&last) {
            if let Some(v @ Value::Color(_)) = interpret_value(tok.trim()) {
                color = Some(v);
            }
        }
    }

    let mut after_slash = false;
    for tok in split_top_level(&normalized) {
        let t = tok.trim();
        if t.is_empty() {
            continue;
        }
        if t == "/" {
            after_slash = true;
            continue;
        }
        if after_slash {
            // background-size 자리: cover|contain|auto|<length-percentage> 최대 2개.
            // 예전엔 슬래시 뒤 토큰을 **끝까지** 크기로 먹어서, `center / 60% url(x) red`
            // 처럼 크기 뒤에 이미지나 색이 오면 둘 다 통째로 사라졌다.
            let is_size = matches!(t, "cover" | "contain" | "auto")
                || t.ends_with('%')
                || crate::css::parse_len_px(t).is_some();
            if is_size && size_tokens.len() < 2 {
                size_tokens.push(t.to_string());
                continue;
            }
            after_slash = false; // 크기 끝 — 이 토큰부터 다시 일반 규칙
        }
        // 이름을 하나씩 나열하면 repeating-* 처럼 새 함수가 생길 때마다 조용히 빠진다.
        // "이미지 값으로 해석되는가" 로 판정한다.
        let tl = t.to_ascii_lowercase();
        if tl.starts_with("url(") || tl.contains("gradient(") {
            if let Some(v) = interpret_value(t) {
                image = Some(v);
            }
        } else if matches!(t, "repeat" | "no-repeat" | "repeat-x" | "repeat-y" | "space" | "round") {
            repeat = Some(Value::Keyword(t.to_string()));
        } else if matches!(t, "left" | "right" | "top" | "bottom" | "center") {
            pos_tokens.push(t.to_string());
        } else if t.ends_with('%') || t.trim_end_matches("px").parse::<f32>().is_ok() {
            pos_tokens.push(t.to_string()); // 길이/퍼센트 위치
        } else if matches!(t, "scroll" | "fixed" | "local") {
            attachment = Some(t.to_string());
        } else if matches!(t, "border-box" | "padding-box" | "content-box") {
            boxes.push(t.to_string());
        } else if t == "none" {
            // image none — 명시적 none(기본값이라 별도 저장 불필요).
        } else if let Some(v @ Value::Color(..)) = interpret_value(t) {
            color = Some(v);
        }
    }
    // 단축은 모든 롱핸드를 리셋한다 — 생략된 것은 초기값으로 채운다(§CSS Backgrounds).
    // origin/clip: 박스 하나면 둘 다, 둘이면 첫째=origin 둘째=clip. 없으면 각 초기값.
    let kw = |name: &str, v: String| Declaration { important: false, name: name.to_string(), value: Value::Keyword(v) };
    out.push(Declaration {
        important: false,
        name: "background-image".to_string(),
        value: image.unwrap_or_else(|| Value::Keyword("none".to_string())),
    });
    out.push(kw("background-position", if pos_tokens.is_empty() { "0% 0%".to_string() } else { pos_tokens.join(" ") }));
    out.push(kw("background-size", if size_tokens.is_empty() { "auto".to_string() } else { size_tokens.join(" ") }));
    out.push(Declaration {
        important: false,
        name: "background-repeat".to_string(),
        value: repeat.unwrap_or_else(|| Value::Keyword("repeat".to_string())),
    });
    out.push(kw("background-attachment", attachment.unwrap_or_else(|| "scroll".to_string())));
    out.push(kw("background-origin", boxes.first().cloned().unwrap_or_else(|| "padding-box".to_string())));
    out.push(kw(
        "background-clip",
        boxes.get(1).or_else(|| boxes.first()).cloned().unwrap_or_else(|| "border-box".to_string()),
    ));
    out.push(Declaration {
        important: false,
        name: "background-color".to_string(),
        value: color.unwrap_or(Value::Color(Color { r: 0, g: 0, b: 0, a: 0 })),
    });
    out
}

// box-shadow 성분이 유효 <length> 인가(px/em 등, 0, calc-무-%). %·단위없는 수(0 제외)는 무효.
fn is_shadow_length(t: &str) -> bool {
    let t = t.trim();
    if t == "0" {
        return true;
    }
    // 수학함수(calc/min/max/clamp/…)는 구문상 유효 — interpret_value 가 중첩 max·나눗셈
    // (calc(max(10em,20px)/2))을 평가 못 해도 <length> 문맥이면 수용한다(값은 사용시 결정).
    // shadow 는 <length> 라 % 가 있으면 무효.
    if !t.contains('%') && super::values::math_function_valid(t) {
        return true;
    }
    match interpret_value(t) {
        Some(Value::Length(_, u)) => !matches!(u, Unit::Percent | Unit::Number),
        Some(Value::Calc(c)) => c.pct == 0.0, // calc 는 % 없어야
        Some(Value::MinMax(..)) => true,
        _ => false,
    }
}
fn is_shadow_color(t: &str) -> bool {
    t.eq_ignore_ascii_case("currentcolor")
        || matches!(interpret_value(t), Some(Value::Color(_)) | Some(Value::ColorFn(..)))
}

// 그림자 길이의 부호 계수(음수 판정용). calc/함수는 부호 불명(사용값에서 clamp) → None.
fn shadow_length_val(t: &str) -> Option<f32> {
    let tl = t.trim();
    if tl == "0" {
        return Some(0.0);
    }
    if tl.contains('(') {
        return None; // calc(-1px) 등은 파스타임 음수여도 유효(사용값 clamp)
    }
    match interpret_value(t) {
        Some(Value::Length(v, _)) => Some(v),
        _ => None,
    }
}

// box-shadow 그림자 하나: inset? && <length>{2,4} && <color>?. 길이는 **연속** 2~4개,
// inset/color 는 각 1개 이하, blur(3번째)는 음수 불가(§CSS Backgrounds 3).
fn single_shadow_valid(s: &str) -> bool {
    let (mut inset, mut color) = (0u32, 0u32);
    let mut runs: Vec<Vec<Option<f32>>> = Vec::new();
    let mut cur: Vec<Option<f32>> = Vec::new();
    for tok in split_top_level(s) {
        if tok.eq_ignore_ascii_case("inset") {
            inset += 1;
            if !cur.is_empty() {
                runs.push(std::mem::take(&mut cur));
            }
        } else if is_shadow_length(tok) {
            cur.push(shadow_length_val(tok));
        } else if is_shadow_color(tok) {
            color += 1;
            if !cur.is_empty() {
                runs.push(std::mem::take(&mut cur));
            }
        } else {
            return false; // 알 수 없는 토큰
        }
    }
    if !cur.is_empty() {
        runs.push(cur);
    }
    if !(inset <= 1 && color <= 1 && runs.len() == 1) {
        return false;
    }
    let run = &runs[0];
    if !(2..=4).contains(&run.len()) {
        return false;
    }
    // blur(index 2)는 음수 불가.
    !matches!(run.get(2), Some(Some(b)) if *b < 0.0)
}

// 단일 text-shadow 유효성: <color>? && <length>{2,3}. box-shadow 와 달리 inset·
// spread(4번째 길이) 없음, 길이는 정확히 2~3개(연속), blur(3번째) 음수 불가, % 불가.
fn single_text_shadow_valid(s: &str) -> bool {
    let mut color = 0u32;
    let mut runs: Vec<Vec<Option<f32>>> = Vec::new();
    let mut cur: Vec<Option<f32>> = Vec::new();
    for tok in split_top_level(s) {
        if tok.eq_ignore_ascii_case("inset") {
            return false; // text-shadow 는 inset 없음
        } else if is_shadow_length(tok) {
            cur.push(shadow_length_val(tok));
        } else if is_shadow_color(tok) {
            color += 1;
            if !cur.is_empty() {
                runs.push(std::mem::take(&mut cur));
            }
        } else {
            return false; // 알 수 없는 토큰
        }
    }
    if !cur.is_empty() {
        runs.push(cur);
    }
    if !(color <= 1 && runs.len() == 1) {
        return false;
    }
    let run = &runs[0];
    if !(2..=3).contains(&run.len()) {
        return false;
    }
    // blur(index 2)는 음수 불가.
    !matches!(run.get(2), Some(Some(b)) if *b < 0.0)
}

// grid-template-areas(§CSS Grid) 유효성 + 캐논 직렬화. none | <string>+. 모든 행은
// 같은(0 아닌) 열 수, 각 셀은 custom-ident 또는 null(점만). 캐논: 셀 공백 단일화,
// 점 시퀀스→".", 행마다 큰따옴표.
pub(crate) fn grid_template_areas_canonical(raw: &str) -> Option<String> {
    let low = raw.trim().to_ascii_lowercase();
    if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer" | "none") {
        return Some(low);
    }
    let chars: Vec<char> = raw.trim().chars().collect();
    let mut rows: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }
        if chars[i] == '"' || chars[i] == '\'' {
            let quote = chars[i];
            i += 1;
            let start = i;
            while i < chars.len() && chars[i] != quote {
                i += 1;
            }
            if i >= chars.len() {
                return None; // 미종료 문자열
            }
            rows.push(chars[start..i].iter().collect());
            i += 1;
        } else {
            return None; // 맨 토큰(none/auto 등)
        }
    }
    if rows.is_empty() {
        return None;
    }
    let mut cols: Option<usize> = None;
    let mut canon_rows: Vec<String> = Vec::new();
    for r in &rows {
        let mut cells: Vec<String> = Vec::new();
        for c in r.split_whitespace() {
            // 셀: null(점만) 또는 custom-ident(점 불포함).
            if c.chars().all(|ch| ch == '.') {
                cells.push(".".to_string());
            } else if !c.contains('.') {
                cells.push(c.to_string());
            } else {
                return None; // ".a" 등 혼합
            }
        }
        if cells.is_empty() {
            return None; // 빈 문자열
        }
        match cols {
            None => cols = Some(cells.len()),
            Some(n) if n != cells.len() => return None,
            _ => {}
        }
        canon_rows.push(format!("\"{}\"", cells.join(" ")));
    }
    Some(canon_rows.join(" "))
}

// offset-rotate(§CSS Motion Path) 유효성 + 캐논. [auto|reverse] || <angle>. 키워드·
// 각도 각 ≤1, 최소 하나. 캐논: 키워드 먼저, 각도 나중.
pub(crate) fn offset_rotate_canonical(raw: &str) -> Option<String> {
    let low = raw.trim().to_ascii_lowercase();
    if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
        return Some(low);
    }
    let toks = split_top_level(raw.trim());
    let (mut kw, mut ang) = (None::<String>, None::<String>);
    let (mut nkw, mut nang) = (0u32, 0u32);
    for t in &toks {
        let tl = t.to_ascii_lowercase();
        if matches!(tl.as_str(), "auto" | "reverse") {
            nkw += 1;
            kw = Some(tl);
        } else if super::values::math_angle_valid(t) {
            nang += 1;
            ang = Some(t.to_string());
        } else {
            return None;
        }
    }
    if toks.is_empty() || nkw > 1 || nang > 1 {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    if let Some(k) = kw {
        parts.push(k);
    }
    if let Some(a) = ang {
        parts.push(a);
    }
    Some(parts.join(" "))
}

// grid-auto-flow(§CSS Grid) 유효성 + 캐논 직렬화. [row|column] || dense. row 는
// 초기값이라 dense 와 함께면 생략, direction 먼저.
pub(crate) fn grid_auto_flow_canonical(raw: &str) -> Option<String> {
    let low = raw.trim().to_ascii_lowercase();
    if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
        return Some(low);
    }
    let toks = split_top_level(&low);
    let (mut dir, mut dense) = (0u32, 0u32);
    let (mut has_row, mut has_col, mut has_dense) = (false, false, false);
    for t in &toks {
        match *t {
            "row" => {
                dir += 1;
                has_row = true;
            }
            "column" => {
                dir += 1;
                has_col = true;
            }
            "dense" => {
                dense += 1;
                has_dense = true;
            }
            _ => return None,
        }
    }
    if toks.is_empty() || dir > 1 || dense > 1 {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    if has_col {
        parts.push("column");
        if has_dense {
            parts.push("dense");
        }
    } else if has_dense {
        parts.push("dense"); // row(초기값) 생략
    } else if has_row {
        parts.push("row");
    }
    Some(parts.join(" "))
}

// margin-trim(§CSS Box 4) 유효성 + 캐논 직렬화. none | (block||inline) |
// (block-start||block-end||inline-start||inline-end). 축약/엣지 혼합·중복·미지 거부.
// CSSOM 캐논: block-start+block-end→block, inline 짝→inline, block 성분 먼저.
pub(crate) fn margin_trim_canonical(raw: &str) -> Option<String> {
    let low = raw.trim().to_ascii_lowercase();
    if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer" | "none") {
        return Some(low);
    }
    let toks = split_top_level(&low);
    let (mut b, mut bs, mut be, mut i, mut is, mut ie) =
        (false, false, false, false, false, false);
    let dup = |flag: &mut bool| -> bool {
        if *flag {
            false
        } else {
            *flag = true;
            true
        }
    };
    for t in &toks {
        let good = match *t {
            "block" => dup(&mut b),
            "block-start" => dup(&mut bs),
            "block-end" => dup(&mut be),
            "inline" => dup(&mut i),
            "inline-start" => dup(&mut is),
            "inline-end" => dup(&mut ie),
            _ => false,
        };
        if !good {
            return None;
        }
    }
    let has_sh = b || i;
    let has_edge = bs || be || is || ie;
    if toks.is_empty() || (has_sh && has_edge) {
        return None;
    }
    // 축약형(block/inline) 입력은 입력 순서 보존.
    if has_sh {
        return Some(toks.join(" "));
    }
    // 엣지형: 존재하는 모든 축이 완전(양끝)일 때만 축약 키워드로 접고 block 먼저.
    // 하나라도 부분 축이면 입력 순서 그대로.
    let block_present = bs || be;
    let inline_present = is || ie;
    let block_full = bs && be;
    let inline_full = is && ie;
    let collapsible = (!block_present || block_full) && (!inline_present || inline_full);
    if collapsible {
        let mut parts: Vec<&str> = Vec::new();
        if block_present {
            parts.push("block");
        }
        if inline_present {
            parts.push("inline");
        }
        Some(parts.join(" "))
    } else {
        Some(toks.join(" "))
    }
}

// text-decoration 단축 지정값 캐논 직렬화(§CSSOM serialize a shorthand): 초기값
// (line none·thickness auto·style solid·color currentcolor)은 생략, 순서는 line →
// thickness → style → color, line 키워드는 캐논 순(underline overline line-through
// blink)으로 재정렬. 전부 초기값이면 "none". 색 이름은 유지(hex→rgb).
pub(crate) fn text_decoration_canonical(raw: &str) -> Option<String> {
    let v = raw.trim();
    let low_all = v.to_ascii_lowercase();
    if matches!(low_all.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
        return Some(low_all);
    }
    let mut lines: Vec<String> = Vec::new();
    let mut style: Option<String> = None;
    let mut color: Option<String> = None;
    let mut thickness: Option<String> = None;
    let (mut style_seen, mut color_seen, mut thick_seen, mut none_seen) = (0u32, 0u32, 0u32, 0u32);
    let mut line_idx: Vec<usize> = Vec::new();
    for (i, &t) in split_top_level(v).iter().enumerate() {
        let tl = t.to_ascii_lowercase();
        if matches!(tl.as_str(), "underline" | "overline" | "line-through" | "blink") {
            if lines.iter().any(|l| l == &tl) {
                return None;
            }
            line_idx.push(i);
            lines.push(tl);
        } else if matches!(tl.as_str(), "solid" | "double" | "dotted" | "dashed" | "wavy") {
            style_seen += 1;
            style = Some(tl);
        } else if matches!(tl.as_str(), "auto" | "from-font") {
            thick_seen += 1;
            thickness = Some(tl);
        } else if tl == "none" {
            none_seen += 1;
        } else if crate::css::single_color_valid(t) {
            color_seen += 1;
            // 이름 색은 유지, hex/함수는 rgb()/rgba() 로 접는다.
            let canon = if t.chars().all(|c| c.is_ascii_alphabetic()) {
                tl.clone()
            } else {
                interpret_value(t)
                    .map(|val| crate::style::computed_value_string(&val))
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| t.to_string())
            };
            color = Some(canon);
        } else {
            match interpret_value(t) {
                Some(Value::Length(..)) | Some(Value::Calc(_)) | Some(Value::MinMax(..)) => {
                    thick_seen += 1;
                    thickness = Some(t.to_string());
                }
                _ => return None,
            }
        }
    }
    if style_seen > 1 || color_seen > 1 || thick_seen > 1 || none_seen > 1 {
        return None;
    }
    if none_seen >= 1 && !lines.is_empty() {
        return None;
    }
    if let (Some(&first), Some(&last)) = (line_idx.first(), line_idx.last()) {
        if last - first + 1 != line_idx.len() {
            return None;
        }
    }
    const ORDER: [&str; 4] = ["underline", "overline", "line-through", "blink"];
    let lines_sorted: Vec<&str> = ORDER
        .iter()
        .copied()
        .filter(|o| lines.iter().any(|l| l == o))
        .collect();
    let has_other = style.as_deref().is_some_and(|s| s != "solid")
        || color.as_deref().is_some_and(|c| c != "currentcolor")
        || thickness.as_deref().is_some_and(|t| t != "auto");
    let mut parts: Vec<String> = Vec::new();
    if !lines_sorted.is_empty() {
        parts.push(lines_sorted.join(" "));
    } else if !has_other {
        return Some("none".to_string());
    }
    if let Some(t) = thickness {
        if t != "auto" {
            parts.push(t);
        }
    }
    if let Some(s) = style {
        if s != "solid" {
            parts.push(s);
        }
    }
    if let Some(c) = color {
        if c != "currentcolor" {
            parts.push(c);
        }
    }
    if parts.is_empty() {
        return Some("none".to_string());
    }
    Some(parts.join(" "))
}

// text-shadow 지정값 캐논 직렬화: 그림자마다 <color> <lengths> 순(색 먼저). 길이는
// 0→0px 정규화, 색 키워드 유지. box-shadow 와 달리 inset 없음.
pub(crate) fn text_shadow_canonical(value: &str) -> Option<String> {
    let v = value.trim();
    if v.eq_ignore_ascii_case("none") {
        return Some("none".to_string());
    }
    if !text_shadow_valid(v) {
        return None;
    }
    let mut shadows = Vec::new();
    for shadow in split_top_level_commas(v) {
        let mut color = None;
        let mut lengths = Vec::new();
        for tok in split_top_level(shadow.trim()) {
            if is_shadow_length(tok) {
                let norm = if tok.contains('(') {
                    tok.to_string()
                } else {
                    interpret_value(tok)
                        .map(|val| crate::style::computed_value_string(&val))
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| tok.to_string())
                };
                lengths.push(norm);
            } else if is_shadow_color(tok) {
                color = Some(tok.to_string());
            }
        }
        let mut parts = Vec::new();
        if let Some(c) = color {
            parts.push(c);
        }
        parts.extend(lengths);
        shadows.push(parts.join(" "));
    }
    Some(shadows.join(", "))
}

// text-shadow 값 유효성: none | <shadow-t>#. 무효면 선언 거부.
pub(crate) fn text_shadow_valid(value: &str) -> bool {
    let v = value.trim();
    if v.eq_ignore_ascii_case("none") {
        return true;
    }
    let shadows = split_top_level_commas(v);
    !shadows.is_empty() && shadows.iter().all(|s| single_text_shadow_valid(s.trim()))
}

// box-shadow 지정값 캐논 직렬화: 그림자마다 <color> <lengths> <inset> 순(color 먼저,
// inset 끝). 길이는 0→0px 등 정규화, 색 키워드는 유지(§CSS Backgrounds 직렬화).
pub(crate) fn box_shadow_canonical(value: &str) -> Option<String> {
    let v = value.trim();
    if v.eq_ignore_ascii_case("none") {
        return Some("none".to_string());
    }
    if !box_shadow_valid(v) {
        return None;
    }
    let mut shadows = Vec::new();
    for shadow in split_top_level_commas(v) {
        let (mut color, mut inset) = (None, false);
        let mut lengths = Vec::new();
        for tok in split_top_level(shadow.trim()) {
            if tok.eq_ignore_ascii_case("inset") {
                inset = true;
            } else if is_shadow_length(tok) {
                // calc/함수는 원문 유지(재직렬화가 항 순서를 바꿈). 단순 길이만 정규화(0→0px).
                let norm = if tok.contains('(') {
                    tok.to_string()
                } else {
                    interpret_value(tok)
                        .map(|val| crate::style::computed_value_string(&val))
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| tok.to_string())
                };
                lengths.push(norm);
            } else if is_shadow_color(tok) {
                color = Some(tok.to_string());
            }
        }
        let mut parts = Vec::new();
        if let Some(c) = color {
            parts.push(c);
        }
        parts.extend(lengths);
        if inset {
            parts.push("inset".to_string());
        }
        shadows.push(parts.join(" "));
    }
    Some(shadows.join(", "))
}

// box-shadow 값 유효성: none | <shadow>#. 무효면 선언 거부.
pub(crate) fn box_shadow_valid(value: &str) -> bool {
    let v = value.trim();
    if v.eq_ignore_ascii_case("none") {
        return true;
    }
    let shadows = split_top_level_commas(v);
    !shadows.is_empty() && shadows.iter().all(|s| single_shadow_valid(s.trim()))
}

// `box-shadow: [inset] <dx> <dy> [blur] [spread] <color>` 를 커스텀 longhand 로 확장.
// 다중 그림자는 첫 번째만. paint 가 이 longhand 를 읽는다.
fn box_shadow_shorthand(value_text: &str) -> Vec<Declaration> {
    // 무효값 거부(§CSS Backgrounds). none 은 아래에서 빈 longhand + 원문 보존.
    let low = value_text.trim().to_ascii_lowercase();
    if !matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
        && !box_shadow_valid(value_text)
    {
        return Vec::new();
    }
    // 최상위(괄호 밖) 첫 콤마까지가 첫 그림자 — rgba(...) 안의 콤마는 보존.
    let mut depth = 0i32;
    let mut end = value_text.len();
    for (i, c) in value_text.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                end = i;
                break;
            }
            _ => {}
        }
    }
    let first = value_text[..end].trim();
    let mut lens: Vec<f32> = Vec::new();
    let mut color: Option<Value> = None;
    let mut inset = false;
    for tok in split_top_level(first) {
        if tok == "inset" {
            inset = true;
            continue;
        }
        match interpret_value(tok) {
            Some(Value::Length(v, Unit::Px)) => lens.push(v),
            Some(c @ Value::Color(..)) => color = Some(c),
            _ => {}
        }
    }
    let color = color.unwrap_or(Value::Color(Color { r: 0, g: 0, b: 0, a: 128 }));
    let px = |v: f32| Value::Length(v, Unit::Px);
    // 첫 그림자 longhand (inner-shadow 경로가 읽음) — dx,dy 있을 때만.
    let mut out = if lens.len() >= 2 {
        vec![
            Declaration { important: false, name: "box-shadow-x".to_string(), value: px(lens[0]) },
            Declaration { important: false, name: "box-shadow-y".to_string(), value: px(lens[1]) },
            Declaration { important: false, name: "box-shadow-blur".to_string(), value: px(lens.get(2).copied().unwrap_or(0.0)) },
            Declaration { important: false, name: "box-shadow-spread".to_string(), value: px(lens.get(3).copied().unwrap_or(0.0)) },
            Declaration { important: false, name: "box-shadow-color".to_string(), value: color },
            Declaration { important: false,
                name: "box-shadow-inset".to_string(),
                value: Value::Keyword(if inset { "inset" } else { "outset" }.to_string()),
            },
        ]
    } else {
        Vec::new()
    };
    // 전체 원문 보존 — paint 가 다중(콤마) 그림자를 모두 파싱해 발행한다.
    out.push(Declaration { important: false,
        name: "box-shadow".to_string(),
        value: Value::Keyword(value_text.trim().to_string()),
    });
    out
}

// `border[-side]: <width> <style> <color>` 단축값(임의 순서, 일부 생략 가능)을
// 지정한 변들의 width/style/color longhand 로 확장한다.
// <line-width>: thin|medium|thick | <length [0,∞]>.
fn is_line_width_tok(t: &str) -> bool {
    let low = t.trim().to_ascii_lowercase();
    if matches!(low.as_str(), "thin" | "medium" | "thick" | "hairline") {
        return true;
    }
    // <line-width> = <length>(% 불가). 수학 함수는 결과가 <length> 인지 타입 검사.
    if super::values::math_function_valid(&low) {
        return super::values::math_length_valid(&low, false);
    }
    match interpret_value(t) {
        Some(Value::Length(n, u)) => n >= 0.0 && u != Unit::Percent,
        _ => false,
    }
}

// <line-style>(§CSS Backgrounds): none|hidden|dotted|dashed|solid|double|groove|ridge|inset|outset.
fn is_line_style_tok(t: &str) -> bool {
    matches!(
        t.trim().to_ascii_lowercase().as_str(),
        "none" | "hidden" | "dotted" | "dashed" | "solid" | "double" | "groove" | "ridge"
            | "inset" | "outset"
    )
}

// border 단축(§CSS Backgrounds): <line-width> || <line-style> || <color>.
// 각 컴포넌트 최대 1회, 분류 불가/중복 토큰이 있으면 선언 전체 거부.
// 생략된 컴포넌트는 초기값(width medium, style none, color currentcolor)으로 채운다.
// 전체 border(4면)는 border-image 롱핸드도 초기값으로 리셋한다.
fn border_shorthand(sides: &[&str], value_text: &str) -> Vec<Declaration> {
    let kw = |name: &str, v: &str| Declaration {
        important: false,
        name: name.to_string(),
        value: Value::Keyword(v.to_string()),
    };
    let low = value_text.trim().to_ascii_lowercase();
    let reset_bi = |out: &mut Vec<Declaration>, v: &str| {
        if sides.len() == 4 {
            let bi: [(&str, &str); 5] = [
                ("border-image-source", if v.is_empty() { "none" } else { v }),
                ("border-image-slice", if v.is_empty() { "100%" } else { v }),
                ("border-image-width", if v.is_empty() { "1" } else { v }),
                ("border-image-outset", if v.is_empty() { "0" } else { v }),
                ("border-image-repeat", if v.is_empty() { "stretch" } else { v }),
            ];
            for (p, val) in bi {
                out.push(kw(p, val));
            }
        }
    };
    // CSS 전역 키워드: 모든 롱핸드를 그대로 편다(border-image 포함).
    if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
        let mut out = Vec::new();
        for &side in sides {
            out.push(kw(&format!("border-{}-width", side), &low));
            out.push(kw(&format!("border-{}-style", side), &low));
            out.push(kw(&format!("border-{}-color", side), &low));
        }
        reset_bi(&mut out, &low);
        return out;
    }

    let (mut width, mut style, mut color): (Option<String>, Option<String>, Option<String>) =
        (None, None, None);
    for tok in split_top_level(value_text) {
        if width.is_none() && is_line_width_tok(tok) {
            width = Some(tok.to_string());
        } else if style.is_none() && is_line_style_tok(tok) {
            style = Some(tok.to_string());
        } else if color.is_none() && crate::css::single_color_valid(tok) {
            color = Some(tok.to_string());
        } else {
            return Vec::new(); // 분류 불가/중복 → 선언 무효
        }
    }
    if width.is_none() && style.is_none() && color.is_none() {
        return Vec::new();
    }
    let wv = width.as_deref().unwrap_or("medium");
    let sv = style.as_deref().unwrap_or("none");
    let cv = color.as_deref().unwrap_or("currentcolor");
    // 레이아웃/페인트가 읽는 계산값 경로는 타입된 Value 를 요구한다:
    //  - width: <length> 는 Length(레이아웃이 폭으로 읽음), thin/medium/thick 은 Keyword.
    //  - color: 실제 색은 Color/ColorFn(페인트 get_color), currentcolor 는 Keyword(후단 치환).
    let width_val = |v: &str| -> Value {
        if matches!(v.to_ascii_lowercase().as_str(), "thin" | "medium" | "thick") {
            Value::Keyword(v.to_string())
        } else {
            interpret_value(v).unwrap_or_else(|| Value::Keyword(v.to_string()))
        }
    };
    let color_val = |v: &str| -> Value {
        match interpret_value(v) {
            Some(c @ (Value::Color(_) | Value::ColorFn(..))) => c,
            _ => Value::Keyword(v.to_string()),
        }
    };
    let mut out = Vec::new();
    for &side in sides {
        out.push(Declaration { important: false, name: format!("border-{}-width", side), value: width_val(wv) });
        out.push(kw(&format!("border-{}-style", side), sv));
        out.push(Declaration { important: false, name: format!("border-{}-color", side), value: color_val(cv) });
    }
    reset_bi(&mut out, "");
    out
}

// 유형 검증이 붙은 박스 단축(border-width/style/color): 토큰이 1~4개이고
// 전부 valid 면 box_shorthand 로 확장, 아니면 선언 무효(Vec::new()).
// CSS 전역 키워드는 그대로 4면에 편다.
fn validated_box_shorthand(
    prefix: &str,
    suffix: &str,
    value_text: &str,
    valid: impl Fn(&str) -> bool,
) -> Vec<Declaration> {
    let low = value_text.trim().to_ascii_lowercase();
    if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
        return box_shorthand(prefix, suffix, value_text);
    }
    let toks = split_top_level(value_text.trim());
    if toks.is_empty() || toks.len() > 4 || !toks.iter().all(|t| valid(t)) {
        return Vec::new();
    }
    box_shorthand(prefix, suffix, value_text)
}

// CSS 박스 단축값(1~4개)을 top/right/bottom/left longhand 로 확장.
// prefix="margin", suffix=""  → margin-top ...
// prefix="border", suffix="-width" → border-top-width ...
// 미리 캐논화된 값 리스트를 TRBL 규칙(1→모두, 2→TB/LR, 3→T/LR/B, 4→TRBL)으로
// 네 side 선언에 배분한다. anchor-size()/anchor() 처럼 interpret_value 로 못 읽는 값을
// 단일변 arm 에서 이미 검증·캐논한 Value 로 배분할 때 쓴다.
fn trbl_decls(names: [&str; 4], vals: &[Value]) -> Vec<Declaration> {
    let idx: [usize; 4] = match vals.len() {
        1 => [0, 0, 0, 0],
        2 => [0, 1, 0, 1],
        3 => [0, 1, 2, 1],
        4 => [0, 1, 2, 3],
        _ => return Vec::new(),
    };
    names
        .iter()
        .zip(idx)
        .map(|(n, i)| Declaration { important: false, name: n.to_string(), value: vals[i].clone() })
        .collect()
}

fn box_shorthand(prefix: &str, suffix: &str, value_text: &str) -> Vec<Declaration> {
    let tokens: Vec<Value> =
        split_top_level(value_text).into_iter().filter_map(interpret_value).collect();
    let (top, right, bottom, left) = match tokens.len() {
        1 => (tokens[0].clone(), tokens[0].clone(), tokens[0].clone(), tokens[0].clone()),
        2 => (tokens[0].clone(), tokens[1].clone(), tokens[0].clone(), tokens[1].clone()),
        3 => (tokens[0].clone(), tokens[1].clone(), tokens[2].clone(), tokens[1].clone()),
        4 => (tokens[0].clone(), tokens[1].clone(), tokens[2].clone(), tokens[3].clone()),
        _ => return Vec::new(),
    };
    let mk = |side: &str, value: Value| Declaration { important: false,
        name: format!("{}-{}{}", prefix, side, suffix),
        value,
    };
    vec![mk("top", top), mk("right", right), mk("bottom", bottom), mk("left", left)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find<'a>(decls: &'a [Declaration], name: &str) -> Option<&'a Value> {
        decls.iter().find(|d| d.name == name).map(|d| &d.value)
    }

    #[test]
    fn multi_token_values_are_not_dropped() {
        // 일반 값 파서는 다중 토큰을 못 읽어 None 을 돌려주고, 그러면 선언이 통째로
        // 사라진다. 아래 프로퍼티들은 그래서 조용히 무시되고 있었다 (요행).
        // overflow: <x> [<y>]
        let d = expand_declaration("overflow", "hidden auto");
        assert!(matches!(find(&d, "overflow-x"), Some(Value::Keyword(k)) if k == "hidden"));
        assert!(matches!(find(&d, "overflow-y"), Some(Value::Keyword(k)) if k == "auto"));
        // 한 값이면 양축(overflow-x/overflow-y)에 펼친다(§CSS Overflow 단축).
        let d1 = expand_declaration("overflow", "hidden");
        assert!(matches!(find(&d1, "overflow-x"), Some(Value::Keyword(k)) if k == "hidden"));
        assert!(matches!(find(&d1, "overflow-y"), Some(Value::Keyword(k)) if k == "hidden"));

        // gap: <row> [<column>]
        let g = expand_declaration("gap", "10px 20px");
        assert_eq!(find(&g, "row-gap"), Some(&Value::Length(10.0, Unit::Px)));
        assert_eq!(find(&g, "column-gap"), Some(&Value::Length(20.0, Unit::Px)));
        let g1 = expand_declaration("gap", "8px");
        assert_eq!(find(&g1, "row-gap"), Some(&Value::Length(8.0, Unit::Px)));
        assert_eq!(find(&g1, "column-gap"), Some(&Value::Length(8.0, Unit::Px)));

        // flex-flow: <direction> || <wrap> (순서 무관, 아예 미구현이었다)
        let f = expand_declaration("flex-flow", "column wrap");
        assert!(matches!(find(&f, "flex-direction"), Some(Value::Keyword(k)) if k == "column"));
        assert!(matches!(find(&f, "flex-wrap"), Some(Value::Keyword(k)) if k == "wrap"));
        let f2 = expand_declaration("flex-flow", "wrap-reverse row-reverse");
        assert!(matches!(find(&f2, "flex-direction"), Some(Value::Keyword(k)) if k == "row-reverse"));
        assert!(matches!(find(&f2, "flex-wrap"), Some(Value::Keyword(k)) if k == "wrap-reverse"));

        // border-spacing: <h> [<v>] — 레이아웃은 이미 두 값을 읽고 있었지만 선언이 없었다
        let b = expand_declaration("border-spacing", "2px 4px");
        assert!(matches!(find(&b, "border-spacing"), Some(Value::Keyword(k)) if k == "2px 4px"));

        // background-size: 다중 토큰 원문 보존
        let z = expand_declaration("background-size", "50% 25%");
        assert!(matches!(find(&z, "background-size"), Some(Value::Keyword(k)) if k == "50% 25%"));

        // transform-origin: 다중 토큰 원문 보존
        let t = expand_declaration("transform-origin", "left top");
        assert!(matches!(find(&t, "transform-origin"), Some(Value::Keyword(k)) if k == "left top"));
    }

    #[test]
    fn flex_shorthand_emits_basis() {
        // flex:1 = 1 1 0% (등폭 핵심). grow/shrink 는 <number>(단위 없음).
        let d = expand_declaration("flex", "1");
        assert_eq!(find(&d, "flex-grow"), Some(&Value::Length(1.0, Unit::Number)));
        assert_eq!(find(&d, "flex-shrink"), Some(&Value::Length(1.0, Unit::Number)));
        assert_eq!(find(&d, "flex-basis"), Some(&Value::Length(0.0, Unit::Percent)));
        // flex: 2 0 200px
        let d2 = expand_declaration("flex", "2 0 200px");
        assert_eq!(find(&d2, "flex-grow"), Some(&Value::Length(2.0, Unit::Number)));
        assert_eq!(find(&d2, "flex-shrink"), Some(&Value::Length(0.0, Unit::Number)));
        assert_eq!(find(&d2, "flex-basis"), Some(&Value::Length(200.0, Unit::Px)));
        // flex: 200px = 1 1 200px
        let d3 = expand_declaration("flex", "200px");
        assert_eq!(find(&d3, "flex-grow"), Some(&Value::Length(1.0, Unit::Number)));
        assert_eq!(find(&d3, "flex-basis"), Some(&Value::Length(200.0, Unit::Px)));
    }

    #[test]
    fn font_shorthand_expands_all_parts() {
        let d = expand_declaration("font", "italic bold 14px/1.5 Arial, sans-serif");
        assert!(matches!(find(&d, "font-style"), Some(Value::Keyword(k)) if k == "italic"));
        // font-weight 의 계산값은 수다 (bold = 700, CSS Fonts §2.2)
        assert_eq!(find(&d, "font-weight"), Some(&Value::Length(700.0, Unit::Number)));
        assert_eq!(find(&d, "font-size"), Some(&Value::Length(14.0, Unit::Px)));
        // 단위 없는 배수는 Lh(상속 시 factor 유지, 요소별 font-size 곱)로 저장
        assert_eq!(find(&d, "line-height"), Some(&Value::Length(1.5, Unit::Lh)));
        assert!(matches!(find(&d, "font-family"), Some(Value::Keyword(k)) if k.contains("Arial")));
    }

    #[test]
    fn font_shorthand_minimal_and_keyword_size() {
        let d = expand_declaration("font", "16px sans-serif");
        assert_eq!(find(&d, "font-size"), Some(&Value::Length(16.0, Unit::Px)));
        assert!(matches!(find(&d, "font-family"), Some(Value::Keyword(k)) if k == "sans-serif"));
        // 크기 키워드
        let d2 = expand_declaration("font", "large serif");
        assert_eq!(find(&d2, "font-size"), Some(&Value::Length(18.0, Unit::Px)));
        // 시스템 폰트 키워드는 no-op
        assert!(expand_declaration("font", "caption").is_empty());
    }

    #[test]
    fn unitless_numbers_are_numbers_not_lengths() {
        // 예전엔 opacity/z-index/order/flex-grow 를 Length(n, Px) 로 실었다. 동작은 했지만
        // getComputedStyle 이 "0.5px"/"5px"/"1px" 같은 거짓 값을 돌려줬다 — 길이가 아니라 수다.
        assert_eq!(
            find(&expand_declaration("opacity", "0.5"), "opacity"),
            Some(&Value::Length(0.5, Unit::Number))
        );
        assert_eq!(
            find(&expand_declaration("z-index", "5"), "z-index"),
            Some(&Value::Length(5.0, Unit::Number))
        );
        assert_eq!(
            find(&expand_declaration("flex-grow", "2"), "flex-grow"),
            Some(&Value::Length(2.0, Unit::Number))
        );
        // font-weight 의 계산값도 수다 (bold = 700)
        assert_eq!(
            find(&expand_declaration("font-weight", "bold"), "font-weight"),
            Some(&Value::Length(700.0, Unit::Number))
        );
        assert_eq!(
            find(&expand_declaration("font-weight", "700"), "font-weight"),
            Some(&Value::Length(700.0, Unit::Number))
        );
        // 레이아웃은 여전히 to_px 로 스칼라를 읽는다
        assert_eq!(
            find(&expand_declaration("flex-grow", "3"), "flex-grow").unwrap().to_px(),
            3.0
        );
    }

    #[test]
    fn shorthands_respect_parentheses_in_function_values() {
        // 예전엔 단축 파서들이 괄호를 무시하고 공백·콤마로 잘라서,
        // `background: rgb(1,2,3)` 은 "rgb(1" 로 잘려 배경이 아예 안 칠해지고
        // `border: 1px solid rgba(0, 0, 0, .1)` 은 색이 통째로 사라졌다.
        // rgba(…, .1) 표기는 실제 사이트에서 압도적으로 흔하다.
        let d = expand_declaration("background", "rgb(1,2,3)");
        assert!(
            matches!(find(&d, "background-color"), Some(Value::Color(c)) if c.r == 1 && c.g == 2 && c.b == 3),
            "콤마 있는 rgb() 배경색: {:?}",
            d
        );
        let d = expand_declaration("background", "rgba(1, 2, 4, 1) url(x.png) no-repeat");
        assert!(
            matches!(find(&d, "background-color"), Some(Value::Color(c)) if c.b == 4),
            "콤마+공백 rgba() + url: {:?}",
            d
        );
        assert!(matches!(find(&d, "background-image"), Some(Value::Url(_))));

        let d = expand_declaration("border", "2px solid rgba(1, 2, 6, 0.5)");
        assert!(
            matches!(find(&d, "border-top-color"), Some(Value::Color(c)) if c.b == 6),
            "테두리 색이 살아있다: {:?}",
            d
        );
        assert!(matches!(find(&d, "border-top-width"), Some(Value::Length(w, _)) if *w == 2.0));

        let d = expand_declaration("outline", "2px solid rgb(1, 2, 7)");
        assert!(matches!(find(&d, "outline-color"), Some(Value::Color(c)) if c.b == 7));

        // 다중 레이어: 색은 마지막 레이어에만 올 수 있다 (CSS 문법)
        let d = expand_declaration("background", "url(a.png) no-repeat, rgb(9, 8, 7)");
        assert!(
            matches!(find(&d, "background-color"), Some(Value::Color(c)) if c.r == 9),
            "마지막 레이어의 색: {:?}",
            d
        );
    }

    #[test]
    fn background_shorthand_extracts_repeat_and_size() {
        // url + no-repeat + position/size(cover) 모두 longhand 로
        let d = expand_declaration("background", "#ffffff url(x.png) no-repeat center / cover");
        assert!(matches!(find(&d, "background-image"), Some(Value::Url(_))), "이미지");
        assert!(matches!(find(&d, "background-color"), Some(Value::Color(_))), "색");
        assert!(
            matches!(find(&d, "background-repeat"), Some(Value::Keyword(k)) if k == "no-repeat"),
            "repeat"
        );
        assert!(
            matches!(find(&d, "background-size"), Some(Value::Keyword(k)) if k == "cover"),
            "size cover"
        );
    }

    #[test]
    fn background_shorthand_extracts_position() {
        let d = expand_declaration("background", "url(a.png) no-repeat center");
        assert!(
            matches!(find(&d, "background-position"), Some(Value::Keyword(k)) if k == "center"),
            "position center"
        );
    }

    #[test]
    fn text_decoration_extracts_line_and_color() {
        let d = expand_declaration("text-decoration", "underline wavy red");
        assert!(
            matches!(find(&d, "text-decoration-line"), Some(Value::Keyword(k)) if k == "underline"),
            "line"
        );
        assert!(matches!(find(&d, "text-decoration-color"), Some(Value::Color(_))), "color 추출");
    }

    #[test]
    fn border_radius_expands_to_four_corners() {
        // "8px 4px 2px 1px" → TL/TR/BR/BL
        let d = expand_declaration("border-radius", "8px 4px 2px 1px");
        let px = |name: &str| match find(&d, name) {
            Some(Value::Length(v, _)) => *v,
            _ => -1.0,
        };
        assert_eq!(px("border-top-left-radius"), 8.0);
        assert_eq!(px("border-top-right-radius"), 4.0);
        assert_eq!(px("border-bottom-right-radius"), 2.0);
        assert_eq!(px("border-bottom-left-radius"), 1.0);
        // 2값: TL/BR = 첫째, TR/BL = 둘째
        let d2 = expand_declaration("border-radius", "10px 20px");
        let px2 = |name: &str| match find(&d2, name) {
            Some(Value::Length(v, _)) => *v,
            _ => -1.0,
        };
        assert_eq!(px2("border-top-left-radius"), 10.0);
        assert_eq!(px2("border-top-right-radius"), 20.0);
        assert_eq!(px2("border-bottom-right-radius"), 10.0);
        assert_eq!(px2("border-bottom-left-radius"), 20.0);
    }

    #[test]
    fn position_longhands_preserve_raw_multivalue() {
        let d = expand_declaration("object-position", "right bottom");
        assert!(matches!(d.first().map(|x| &x.value), Some(Value::Keyword(k)) if k == "right bottom"));
        let d2 = expand_declaration("background-position", "center top");
        assert!(matches!(d2.first().map(|x| &x.value), Some(Value::Keyword(k)) if k == "center top"));
    }

    #[test]
    fn background_shorthand_size_does_not_swallow_rest() {
        // `/ <size>` 뒤에 이미지나 색이 와도 삼키지 않는다 (예전엔 둘 다 사라졌다).
        let d = expand_declaration("background", "no-repeat center / 60% url(x.png) red");
        assert!(matches!(find(&d, "background-image"), Some(Value::Url(_))), "이미지가 살아야");
        assert!(matches!(find(&d, "background-color"), Some(Value::Color(_))), "색이 살아야");
        assert!(matches!(find(&d, "background-size"), Some(Value::Keyword(k)) if k == "60%"), "size 60%");
        // 두 값 크기
        let d2 = expand_declaration("background", "url(x.png) center / 50% 25% no-repeat");
        assert!(matches!(find(&d2, "background-size"), Some(Value::Keyword(k)) if k == "50% 25%"));
        assert!(matches!(find(&d2, "background-repeat"), Some(Value::Keyword(k)) if k == "no-repeat"));
    }

    #[test]
    fn background_shorthand_repeat_x_only() {
        let d = expand_declaration("background", "url(a.png) repeat-x");
        assert!(
            matches!(find(&d, "background-repeat"), Some(Value::Keyword(k)) if k == "repeat-x"),
            "repeat-x"
        );
        // 단축은 생략된 롱핸드도 초기값으로 리셋한다(§CSS Backgrounds): size=auto.
        assert!(matches!(find(&d, "background-size"), Some(Value::Keyword(k)) if k == "auto"), "size 초기값 auto");
    }

    // anchor-size()(§css-anchor-1): sizing 프로퍼티에서 문법 검증 + name-first 캐논.
    // 예전엔 is_length_percentage 관대 수용으로 일부만 우연히 통과(요행)했다.
    #[test]
    fn anchor_size_grammar_on_sizing() {
        let canon = |v: &str| match find(&expand_declaration("width", v), "width") {
            Some(Value::Keyword(k)) => k.clone(),
            _ => String::new(),
        };
        // 유효: name 선택, size 필수, 순서 무관(캐논은 name-first), fallback.
        assert_eq!(canon("anchor-size(width)"), "anchor-size(width)");
        assert_eq!(canon("anchor-size(--foo width)"), "anchor-size(--foo width)");
        assert_eq!(canon("anchor-size(width --foo)"), "anchor-size(--foo width)"); // 순서 뒤집기→캐논
        assert_eq!(canon("anchor-size(--foo width, 1px)"), "anchor-size(--foo width, 1px)");
        assert_eq!(canon("anchor-size(self-block)"), "anchor-size(self-block)");
        // name/size 전체가 optional (§): 빈 것/이름만/길이 단독/이름+fallback.
        assert_eq!(canon("anchor-size()"), "anchor-size()");
        assert_eq!(canon("anchor-size(--foo)"), "anchor-size(--foo)");
        assert_eq!(canon("anchor-size(10px)"), "anchor-size(10px)"); // length 단독
        assert_eq!(canon("anchor-size(--foo, 10px)"), "anchor-size(--foo, 10px)"); // size 없이 fallback
        assert_eq!(canon("anchor-size(--foo width, anchor-size(--bar inline))"),
                   "anchor-size(--foo width, anchor-size(--bar inline))"); // 중첩 fallback
        // 무효: 거부(빈 문자열).
        for bad in [
            "anchor-size(--foo, width)",     // 콤마가 name/size 사이
            "anchor-size(--foo width,)",     // 후행 콤마
            "anchor-size(--foo width height)", // size 두 개
            "anchor-size(foo width)",        // dashed-ident 아님
            "anchor-size(--foo top)",        // size-keyword 아님
            "anchor-size(--foo 50%)",        // 첫부분에 길이 불가
            "anchor-size(--foo width, 1)",   // fallback 단위없는 1
            "anchor-size(--foo width, 100s)", // 시간 단위
            "anchor-size(--foo width, height)", // bare 키워드 fallback
            "anchor-size(, 10px)",           // 선행 콤마
            "anchor-size(--foo width, anchor-size(bar width))", // 무효 중첩(bar)
            "anchor-size(--foo width, anchor(--bar top))", // anchor() 는 fallback 불가(inset 전용)
        ] {
            assert_eq!(canon(bad), "", "무효여야: {}", bad);
        }
        // 자기 자신 중첩만 허용.
        assert_eq!(canon("anchor-size(--foo width, anchor-size(--bar block))"),
                   "anchor-size(--foo width, anchor-size(--bar block))");
        // anchor-size 는 sizing 뿐 아니라 inset/margin 에서도 유효, padding 은 불가(§).
        let kw = |p: &str, v: &str| match find(&expand_declaration(p, v), p) {
            Some(Value::Keyword(k)) => k.clone(),
            _ => String::new(),
        };
        assert_eq!(kw("top", "anchor-size(width)"), "anchor-size(width)");
        assert_eq!(kw("left", "anchor-size(--foo width)"), "anchor-size(--foo width)");
        assert_eq!(kw("margin-left", "anchor-size(width --foo)"), "anchor-size(--foo width)");
        assert_eq!(kw("margin-top", "anchor-size(height)"), "anchor-size(height)");
        // padding 은 anchor-size 거부(§: sizing/inset/margin 만).
        assert_eq!(kw("padding-left", "anchor-size(--foo width)"), "");
    }

    // position-area(§css-anchor-2): 2D 축 호환성 문법 + 캐논. 예전엔 검증 전무(raw 수용).
    #[test]
    fn position_area_grammar() {
        let c = |v: &str| match find(&expand_declaration("position-area", v), "position-area") {
            Some(Value::Keyword(k)) => k.clone(),
            _ => String::new(),
        };
        // 단일 + none
        assert_eq!(c("none"), "none");
        assert_eq!(c("center"), "center");
        assert_eq!(c("span-all"), "span-all");
        assert_eq!(c("top"), "top");
        // 호환 축 쌍 캐논(H/block/self-block 앞)
        assert_eq!(c("top left"), "left top"); // V H → H first
        assert_eq!(c("left top"), "left top");
        assert_eq!(c("inline-start block-start"), "block-start inline-start"); // inline block → block first
        // span-all 드롭(비방향), center 슬롯
        assert_eq!(c("top span-all"), "top");
        assert_eq!(c("span-all left"), "left");
        assert_eq!(c("top center"), "center top"); // V → center 앞
        assert_eq!(c("left center"), "left center"); // H → 앞
        assert_eq!(c("center center"), "center");
        assert_eq!(c("span-all span-all"), "span-all");
        // 방향 축(start/end): 같은 그룹 유효, 같은 키워드 단일 축약, span-all/center 유지
        assert_eq!(c("start end"), "start end");
        assert_eq!(c("start start"), "start");
        assert_eq!(c("start span-all"), "start span-all"); // 드롭 안 함
        assert_eq!(c("span-all start"), "span-all start");
        assert_eq!(c("start center"), "start center"); // 입력 순서
        // 무효: 같은 축(비방향)/중복/비호환 축/미인식 키워드
        for bad in ["top top", "top bottom", "left right", "top inline-start",
                    "vertical", "start top", "foobar", "start foobar", "none none"] {
            assert_eq!(c(bad), "", "무효여야: {}", bad);
        }
        // 계산값 remap(§css-anchor-2, 기본 writing-mode): block/inline 논리 → start-end 접힘.
        let cc = |v: &str| crate::css::position_area_computed(v);
        assert_eq!(cc("block-start inline-start"), "start");     // 둘 다 start
        assert_eq!(cc("block-start inline-end"), "start end");
        assert_eq!(cc("block-start center"), "start center");    // block → 앞
        assert_eq!(cc("center inline-start"), "center start");   // inline → 뒤
        assert_eq!(cc("start span-all"), "block-start");         // 역: 첫 슬롯 → block
        assert_eq!(cc("span-all start"), "inline-start");        // 역: 둘째 슬롯 → inline
        assert_eq!(cc("self-block-end self-inline-start"), "self-end self-start");
        assert_eq!(cc("left top"), "left top");                  // 물리 축은 그대로
        assert_eq!(cc("top"), "top");                            // 단일은 그대로
    }

    // anchor()(§css-anchor-1): inset 프로퍼티 전용. anchor-side ∈ 키워드|<percentage>|math.
    #[test]
    fn anchor_grammar_on_inset() {
        let kw = |p: &str, v: &str| match find(&expand_declaration(p, v), p) {
            Some(Value::Keyword(k)) => k.clone(),
            _ => String::new(),
        };
        // 유효: side 필수(키워드/퍼센트/math), name 선택·순서무관→name-first, fallback.
        assert_eq!(kw("top", "anchor(top)"), "anchor(top)");
        assert_eq!(kw("left", "anchor(--foo left)"), "anchor(--foo left)");
        assert_eq!(kw("bottom", "anchor(right --foo)"), "anchor(--foo right)"); // 순서 뒤집기
        assert_eq!(kw("top", "anchor(50%)"), "anchor(50%)");
        assert_eq!(kw("top", "anchor(--foo left, 1px)"), "anchor(--foo left, 1px)");
        assert_eq!(kw("top", "anchor(--foo left, anchor(--bar right))"),
                   "anchor(--foo left, anchor(--bar right))"); // 중첩 fallback
        // inset-only: margin/sizing/padding 에선 거부.
        assert_eq!(kw("margin-top", "anchor(top)"), "");
        assert_eq!(kw("height", "anchor(top)"), "");
        // 무효: 거부.
        for bad in [
            "anchor(--foo, top)",       // 콤마가 name/side 사이
            "anchor(--foo top,)",       // 후행 콤마
            "anchor(--foo top bottom)", // side 두 개
            "anchor(foo top)",          // dashed-ident 아님
            "anchor(--foo height)",     // side 아님(height 는 size)
            "anchor(--foo 10em)",       // 길이 불가(퍼센트만)
            "anchor(--foo 100s)",       // 시간
            "anchor(2 * 20%)",          // 다중 토큰
            "anchor(--foo top, 1)",     // fallback 단위없는 1
            "anchor(--foo top, bottom)", // bare 키워드 fallback
            "anchor(--foo top, anchor-size(--bar width))", // anchor-size() 는 fallback 불가
        ] {
            assert_eq!(kw("top", bad), "", "무효여야: {}", bad);
        }
        // 자기 자신 중첩만 허용.
        assert_eq!(kw("top", "anchor(--foo top, anchor(--bar bottom))"),
                   "anchor(--foo top, anchor(--bar bottom))");
    }
}
