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
            let sides = box_shorthand("", "", value_text); // "-top" 등 이름이 "-top" 형태
            return sides
                .into_iter()
                .map(|d| Declaration { important: false, name: d.name.trim_start_matches('-').to_string(), value: d.value })
                .collect();
        }
        _ => {}
    }
    match name {
        "margin" | "padding" => box_shorthand(name, "", value_text),
        "border-width" => box_shorthand("border", "-width", value_text),
        "border-color" => box_shorthand("border", "-color", value_text),
        "border-style" => box_shorthand("border", "-style", value_text),
        // border-radius: 1~4 값 → 네 모서리 longhand. 슬래시 뒤 세로 반경은 근사로 무시.
        "border-radius" => {
            let hpart = value_text.split('/').next().unwrap_or(value_text);
            let toks: Vec<Value> = split_top_level(hpart)
                .into_iter()
                .filter_map(interpret_value)
                .filter(|v| matches!(v, Value::Length(..)))
                .collect();
            if toks.is_empty() {
                return Vec::new();
            }
            let (tl, tr, br, bl) = match toks.len() {
                1 => (toks[0].clone(), toks[0].clone(), toks[0].clone(), toks[0].clone()),
                2 => (toks[0].clone(), toks[1].clone(), toks[0].clone(), toks[1].clone()),
                3 => (toks[0].clone(), toks[1].clone(), toks[2].clone(), toks[1].clone()),
                _ => (toks[0].clone(), toks[1].clone(), toks[2].clone(), toks[3].clone()),
            };
            vec![
                Declaration { important: false, name: "border-top-left-radius".to_string(), value: tl.clone() },
                Declaration { important: false, name: "border-top-right-radius".to_string(), value: tr },
                Declaration { important: false, name: "border-bottom-right-radius".to_string(), value: br },
                Declaration { important: false, name: "border-bottom-left-radius".to_string(), value: bl },
                // box-shadow 등 균일 근사용으로 border-radius 도 남긴다 (첫 값).
                Declaration { important: false, name: "border-radius".to_string(), value: tl },
            ]
        }
        // z-index: 정수 → Length(n, Px) 로 보존 (paint 가 스택 레벨로 읽음). auto 는 드롭.
        // z-index: <integer> | auto. 직접 파싱 실패 시 수학 함수(abs/sign/round/…) 평가.
        "z-index" if value_text.trim().eq_ignore_ascii_case("auto") => {
            vec![Declaration { important: false, name: "z-index".to_string(), value: Value::Keyword("auto".to_string()) }]
        }
        "z-index" => match number_or_math(value_text) {
            Some(n) => vec![Declaration { important: false, name: "z-index".to_string(), value: Value::Length(n, Unit::Number) }],
            _ => Vec::new(),
        },
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
                // inherit/unset/revert 는 선언을 남기지 않는다 → 상속이 적용된다.
                // 예전엔 이걸 "normal" 로 눌러버려서 `font-weight: inherit` 이 상속을
                // 끊었다(react.dev 의 리셋 CSS 가 실제로 이걸 쓴다).
                "inherit" | "unset" | "revert" => return Vec::new(),
                other => {
                    // 평수는 [1,1000] 이어야 유효. calc(…)는 interpret_value 로 평가해
                    // 범위 밖도 유효(used value 에서 클램프, §CSS Fonts).
                    if let Ok(n) = other.parse::<f32>() {
                        if (1.0..=1000.0).contains(&n) {
                            n
                        } else {
                            return Vec::new();
                        }
                    } else if let Some(Value::Length(n, _)) = interpret_value(other) {
                        n.clamp(1.0, 1000.0)
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
        "text-indent" => {
            let vt = value_text.trim().to_ascii_lowercase();
            // CSS 전역 키워드: inherit/unset/revert 는 선언 없음(상속 적용), initial=0px.
            if matches!(vt.as_str(), "inherit" | "unset" | "revert" | "revert-layer") {
                return Vec::new();
            }
            if vt == "initial" {
                return vec![Declaration {
                    important: false,
                    name: "text-indent".to_string(),
                    value: Value::Length(0.0, Unit::Px),
                }];
            }
            let toks: Vec<&str> = split_top_level(value_text.trim());
            let kw_ok = toks.iter().all(|t| {
                matches!(*t, "hanging" | "each-line")
                    || matches!(interpret_value(t), Some(Value::Length(..)))
            });
            let len = toks
                .iter()
                .find(|t| !matches!(**t, "hanging" | "each-line"))
                .and_then(|t| interpret_value(t));
            match len {
                Some(v @ Value::Length(..)) if kw_ok => {
                    vec![Declaration { important: false, name: "text-indent".to_string(), value: v }]
                }
                _ => Vec::new(),
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
        "order" => match number_or_math(value_text) {
            Some(n) => vec![Declaration { important: false, name: "order".to_string(), value: Value::Length(n, Unit::Number) }],
            _ => Vec::new(),
        },
        // flex-grow/flex-shrink: <number [0,∞]>(단위 없음). 음수·미인식 거부.
        "flex-grow" | "flex-shrink" => match number_or_math(value_text) {
            Some(n) if n >= 0.0 => vec![Declaration { important: false, name: name.to_string(), value: Value::Length(n, Unit::Number) }],
            _ => Vec::new(),
        },
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
        // grid 트랙/영역 정의는 다중 토큰 → 원문을 Keyword 로 보존, 레이아웃이 파싱.
        "grid-template-columns" | "grid-template-rows" | "grid-template-areas" | "grid-area"
        | "grid-column" | "grid-row" => {
            vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.to_string()) }]
        }
        // place-* 단축: <align> [<justify>] → align-*/justify-* longhand
        "place-items" | "place-content" | "place-self" => {
            let axis = name.strip_prefix("place-").unwrap();
            let toks: Vec<&str> = split_top_level(value_text);
            let a = toks.first().copied().unwrap_or("");
            let j = toks.get(1).copied().unwrap_or(a);
            if a.is_empty() {
                return Vec::new();
            }
            vec![
                Declaration { important: false, name: format!("align-{}", axis), value: Value::Keyword(a.to_string()) },
                Declaration { important: false, name: format!("justify-{}", axis), value: Value::Keyword(j.to_string()) },
            ]
        }
        // grid-gap 은 gap 의 레거시 별칭
        "grid-gap" | "grid-column-gap" | "grid-row-gap" => {
            let mapped = name.strip_prefix("grid-").unwrap();
            expand_declaration(mapped, value_text)
        }
        // gap: <row-gap> [<column-gap>]. 값이 둘이면 일반 값 파서가 None 을 돌려주고
        // 선언이 통째로 사라져서 간격이 0 이 됐다. longhand 로 쪼갠다.
        // 한 값이어도 longhand 를 함께 내보내 소비자가 어느 쪽을 읽든 맞게 한다.
        "gap" => {
            let toks = split_top_level(value_text);
            let Some(r) = toks.first().copied() else { return Vec::new() };
            let c = toks.get(1).copied().unwrap_or(r);
            let mut out = expand_declaration("row-gap", r);
            out.extend(expand_declaration("column-gap", c));
            out
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
        "flex-flow" => {
            let mut out = Vec::new();
            for t in split_top_level(value_text) {
                let lower = t.to_ascii_lowercase();
                match lower.as_str() {
                    "row" | "row-reverse" | "column" | "column-reverse" => out.push(Declaration {
                        important: false, name: "flex-direction".to_string(), value: Value::Keyword(lower) }),
                    "nowrap" | "wrap" | "wrap-reverse" => out.push(Declaration {
                        important: false, name: "flex-wrap".to_string(), value: Value::Keyword(lower) }),
                    _ => {}
                }
            }
            out
        }
        // border-spacing: <h> [<v>]. 두 값 원문 보존 (레이아웃이 이미 두 값을 읽는다).
        "border-spacing" => {
            let toks = split_top_level(value_text);
            if toks.len() >= 2 {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }];
            }
            interpret_value(value_text)
                .map(|v| vec![Declaration { important: false, name: name.to_string(), value: v }])
                .unwrap_or_default()
        }
        // background-size: cover | contain | [<length-percentage> | auto]{1,2}. 다중 토큰
        // 원문 보존 (페인트가 파싱). 예전엔 "50% 25%" 가 사라져 auto 로 그려졌다.
        "background-size" => {
            vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
        }
        // line-height: 단위 없는 수(1.5)는 배수(Lh)로 저장해 상속 시 factor 그대로 —
        // 각 요소가 자기 font-size 를 곱한다(CSS2 §10.8). 퍼센트(150%)는 요소 font-size
        // 기준 길이로 확정돼 그 길이가 상속되므로 em 으로 저장. normal/길이단위는 그대로.
        "line-height" => {
            let v = value_text.trim();
            if v == "normal" {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword("normal".to_string()) }];
            }
            if let Some(pct) = v.strip_suffix('%') {
                if let Ok(n) = pct.trim().parse::<f32>() {
                    return vec![Declaration { important: false, name: name.to_string(), value: Value::Length(n / 100.0, Unit::Em) }];
                }
            }
            if let Ok(n) = v.parse::<f32>() {
                return vec![Declaration { important: false, name: name.to_string(), value: Value::Length(n, Unit::Lh) }];
            }
            match interpret_value(v) {
                Some(value) => vec![Declaration { important: false, name: name.to_string(), value }],
                None => Vec::new(),
            }
        }
        // text-decoration[-line]: line 키워드 + 색 추출 (style/thickness 는 미사용).
        // none/키워드 없음 → "none". 인라인 레이아웃이 밑줄/취소선/윗줄로 그린다.
        "text-decoration" | "text-decoration-line" => {
            let is_shorthand = name == "text-decoration";
            let mut lines: Vec<&str> = Vec::new();
            let mut style: Option<&str> = None;
            let mut color: Option<Value> = None;
            let mut thickness: Option<String> = None;
            for t in split_top_level(value_text) {
                if matches!(t, "underline" | "overline" | "line-through" | "blink") {
                    lines.push(t);
                } else if is_shorthand
                    && matches!(t, "solid" | "double" | "dotted" | "dashed" | "wavy")
                {
                    style = Some(t);
                } else if is_shorthand && matches!(t, "auto" | "from-font") {
                    thickness = Some(t.to_string());
                } else if t == "none" {
                    // none 은 line 비움
                } else if is_shorthand {
                    match interpret_value(t) {
                        Some(v @ Value::Color(..)) => color = Some(v),
                        Some(Value::Length(..)) => thickness = Some(t.to_string()),
                        _ => {}
                    }
                }
            }
            let joined = lines.join(" ");
            let mut out = vec![Declaration { important: false,
                name: "text-decoration-line".to_string(),
                value: Value::Keyword(if joined.is_empty() { "none".to_string() } else { joined }),
            }];
            // 단축은 나머지 longhand 를 **항상** 출력(리셋). 미지정은 초기값.
            if is_shorthand {
                out.push(Declaration { important: false, name: "text-decoration-style".to_string(), value: Value::Keyword(style.unwrap_or("solid").to_string()) });
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
        "opacity" => {
            let v = value_text.trim();
            let n = if let Some(p) = v.strip_suffix('%') {
                p.trim().parse::<f32>().ok().map(|x| x / 100.0)
            } else {
                // 직접 파싱 실패 시 수학 함수(clamp(0,sign(1),1) 등) 평가.
                number_or_math(v)
            };
            match n {
                Some(op) => vec![Declaration { important: false,
                    name: "opacity".to_string(),
                    value: Value::Length(op.clamp(0.0, 1.0), Unit::Number),
                }],
                None => Vec::new(),
            }
        }
        // counter-reset/counter-increment: 원문 보존 ("name [n] ..."). 카운터 처리기가 파싱.
        "counter-reset" | "counter-increment" | "counter-set" => {
            vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
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
        "transform" | "-webkit-transform" => {
            vec![Declaration { important: false, name: "transform".to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
        }
        // transition/animation 롱핸드: 원문 보존(애니메이션은 미구현이지만 계산값은
        // 정규화해 돌려준다 — collect_computed_styles 가 시간(ms→s)·목록 간격을 정규화).
        "transition-behavior"
        | "animation-name" | "animation-duration" | "animation-delay"
        | "animation-iteration-count"
        | "animation-direction" | "animation-fill-mode" | "animation-play-state"
        | "animation-composition" => {
            vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
        }
        // 키워드 값 프로퍼티(계산값 = 지정 키워드): UI/인터랙션/표/스크롤 등. 원문 보존.
        // 예전엔 interpret_value(키워드) 가 None 이라 선언이 통째로 드롭돼 getComputedStyle
        // 에 안 나왔다(cursor/user-select/appearance 등 실제 프로퍼티가 통째로 사라짐).
        "appearance" | "-webkit-appearance" | "user-select" | "-webkit-user-select"
        | "resize" | "pointer-events" | "touch-action" | "hyphens" | "writing-mode"
        | "text-orientation" | "image-rendering" | "isolation" | "box-decoration-break"
        | "caption-side" | "empty-cells" | "table-layout" | "background-attachment"
        | "background-clip" | "background-origin" | "overflow-anchor" | "scroll-behavior"
        | "text-decoration-style" | "text-underline-position" | "will-change"
        | "content-visibility" | "backface-visibility" | "transform-style" | "transform-box"
        | "text-align-last" | "overscroll-behavior" | "overscroll-behavior-x"
        | "overscroll-behavior-y" | "scroll-snap-align"
        | "background-blend-mode" | "font-kerning" | "font-variant-caps"
        | "text-rendering" | "color-scheme" | "forced-color-adjust" | "print-color-adjust"
        // 2차 배치: text/font-variant/ruby/scrollbar/list 등 키워드 프로퍼티.
        | "text-emphasis-style" | "text-emphasis-position" | "text-combine-upright"
        | "text-decoration-skip-ink" | "line-break"
        | "ruby-position" | "ruby-align"
        | "white-space-collapse" | "font-optical-sizing" | "font-synthesis"
        | "font-synthesis-weight" | "font-synthesis-style" | "font-synthesis-small-caps"
        | "font-synthesis-position"
        | "font-variant-ligatures" | "font-variant-numeric" | "font-variant-east-asian"
        | "font-variant-position" | "font-variant-alternates" | "font-language-override"
        | "list-style-position" | "quotes" | "scrollbar-width" | "scrollbar-color"
        | "mask-type" | "hyphenate-character" | "text-justify"
        // 3차 배치: grid/break/column/bidi 등 키워드 프로퍼티.
        | "grid-auto-flow" | "grid-auto-columns" | "break-before" | "break-after"
        | "break-inside" | "page-break-before" | "page-break-after" | "page-break-inside"
        | "column-span" | "column-fill" | "column-rule-style" | "caret-shape"
        | "unicode-bidi" | "border-image-repeat"
        | "column-width"
        // 4차 배치: logical border-style, mask, offset, scroll-snap-stop, place-self.
        | "border-block-start-style" | "border-block-end-style" | "border-inline-start-style"
        | "border-inline-end-style" | "mask-image" | "mask-repeat" | "mask-position"
        | "mask-size" | "mask-origin" | "mask-clip" | "mask-composite" | "mask-mode"
        | "offset-path" | "offset-rotate" | "offset-anchor" | "offset-position" | "scroll-snap-stop"
        | "contain-intrinsic-width" | "contain-intrinsic-height"
        // 5차: SVG presentation 키워드/수/목록 프로퍼티(stroke-width/dashoffset 는 길이).
        | "fill-opacity" | "stroke-opacity" | "stroke-linecap" | "stroke-linejoin"
        | "stroke-dasharray" | "stroke-miterlimit" | "clip-rule" | "fill-rule"
        | "paint-order" | "vector-effect" | "dominant-baseline" | "text-anchor"
        | "shape-rendering" | "color-interpolation" | "color-interpolation-filters"
        | "marker-start" | "marker-mid" | "marker-end" | "baseline-shift"
        // 6차: font/text/webkit-box/math/misc 키워드 프로퍼티(수/목록/함수 원문 보존).
        | "font-feature-settings" | "font-variation-settings" | "font-stretch"
        | "font-palette"
        | "text-size-adjust"
        | "-webkit-text-size-adjust" | "-webkit-box-orient" | "-webkit-line-clamp"
        | "line-clamp" | "-webkit-box-align" | "-webkit-box-pack" | "zoom"
        | "image-orientation" | "math-style" | "math-depth" | "math-shift"
        // 8차: 순수 키워드 롱핸드.
        | "view-transition-name" | "anchor-name"
        // 9차: 개별 변환 프로퍼티(값 원문 보존 — scale: 1.5 2, rotate: 45deg 등).
        | "scale" | "rotate" | "translate"
        // 10차: corner-shape(round/scoop/bevel/superellipse() 등 — 원문 보존).
        | "corner-shape" | "corner-top-left-shape" | "corner-top-right-shape"
        | "corner-bottom-left-shape" | "corner-bottom-right-shape"
        | "corner-block-start-shape" | "corner-block-end-shape"
        | "corner-inline-start-shape" | "corner-inline-end-shape" | "corner-top-shape"
        | "corner-bottom-shape" | "corner-left-shape" | "corner-right-shape"
        | "corner-block-shape" | "corner-inline-shape"
        // 11차: border-image 롱핸드(원문 보존 — none/url/gradient/수치 목록).
        | "border-image-source" | "border-image-slice" | "border-image-width"
        | "border-image-outset"
        // 개별 border-*-style(solid/dashed 등 키워드).
        | "border-top-style" | "border-right-style" | "border-bottom-style"
        | "border-left-style"
        // 12차: 흩어진 프로퍼티(위치/shape/키워드 원문 보존).
        // border-*-radius 코너 롱핸드는 <length-percentage>{1,2}(수평 [수직]) — 원문 보존해
        // 2값(타원 코너)도 유효하게. CSS.supports/합성이 이를 필요로 한다.
        | "border-top-left-radius" | "border-top-right-radius" | "border-bottom-left-radius"
        | "border-bottom-right-radius"
        | "background-position-x" | "background-position-y" | "shape-outside"
        // hyphenate-limit-chars/character 원문 보존.
        | "hyphenate-limit-chars" | "hyphenate-character"
        | "word-space-transform" | "view-transition-class" | "text-box-trim"
        | "text-box-edge" | "text-box" | "white-space-trim" => {
            vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
        }
        // text-decoration-thickness: auto | from-font | <length-percentage>
        // text-underline-offset: auto | <length-percentage>
        // 길이/calc 는 Length/Calc 로 파싱해 계산값에서 em/rem→px, calc 축약이 되게 한다
        // (키워드로 보존하면 resolve_units 가 건너뛰어 "2em"/"calc(...)" 원문이 남는다).
        "text-decoration-thickness" | "text-underline-offset" => {
            let t = value_text.trim();
            let value = match interpret_value(t) {
                Some(v @ (Value::Length(..) | Value::Calc(_) | Value::MinMax(..))) => v,
                _ => Value::Keyword(t.to_string()), // auto/from-font/CSS-wide 키워드
            };
            vec![Declaration { important: false, name: name.to_string(), value }]
        }
        // SVG 페인트/색 프로퍼티: <color> 는 색으로(계산값 rgb()), none/url()/context-* 는
        // 키워드로 보존. column-rule-color 도 색.
        "fill" | "stroke" | "stop-color" | "flood-color" | "lighting-color"
        | "column-rule-color" | "text-emphasis-color"
        | "-webkit-text-fill-color" | "-webkit-text-stroke-color"
        | "border-block-start-color" | "border-block-end-color"
        | "border-inline-start-color" | "border-inline-end-color" => {
            let value = match interpret_value(value_text.trim()) {
                Some(v @ Value::Color(_)) => v,
                _ => Value::Keyword(value_text.trim().to_string()),
            };
            vec![Declaration { important: false, name: name.to_string(), value }]
        }
        // transform-origin: "0 0", "left top", "50% 50%" 같은 다중 토큰 값이다.
        // 일반 값 파서는 다중 토큰을 파싱하지 못해 None 을 돌려주고, 그러면 선언이
        // 통째로 사라져서 **항상 중심 기준 회전**이 되어 버린다. 원문을 보존한다.
        "transform-origin" | "-webkit-transform-origin" => {
            vec![Declaration { important: false, name: "transform-origin".to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
        }
        // perspective-origin: 다중 토큰 원문 보존(계산값은 resolve_origin 이 px 로).
        "perspective-origin" | "-webkit-perspective-origin" => {
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
            if value_text.trim() == "none" {
                return Vec::new();
            }
            // 첫 최상위 콤마까지가 첫 그림자
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
            if lens.len() < 2 {
                return Vec::new();
            }
            let color = color.unwrap_or(Value::Color(Color { r: 0, g: 0, b: 0, a: 128 }));
            let px = |v: f32| Value::Length(v, Unit::Px);
            vec![
                Declaration { important: false, name: "text-shadow-x".to_string(), value: px(lens[0]) },
                Declaration { important: false, name: "text-shadow-y".to_string(), value: px(lens[1]) },
                Declaration { important: false, name: "text-shadow-color".to_string(), value: color },
                // 전체 원문 보존 — getComputedStyle 이 캐논 직렬화(색 우선)하고 보간에 쓴다.
                Declaration { important: false,
                    name: "text-shadow".to_string(),
                    value: Value::Keyword(value_text.trim().to_string()),
                },
            ]
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
        "background-position" | "object-position" => {
            vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
        }
        // clip-path/backdrop-filter: 함수 표기 원문 보존, paint 가 파싱.
        "clip-path" | "backdrop-filter" => {
            vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
        }
        // outline: <width> <style> <color> (균일 링, 레이아웃 영향 없음)
        // outline = <outline-width> || <outline-style> || <outline-color>. width 키워드
        // (thin/medium/thick)와 style 키워드(none/solid/…)를 구분하고 invert 는 색으로.
        // 단축은 모든 longhand 를 **리셋**(미지정은 초기값 medium/none/invert)한다.
        "outline" => {
            let low = value_text.trim().to_ascii_lowercase();
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
fn font_size_keyword(k: &str) -> Option<f32> {
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
    // 단축 전체를 무효로 한다. variant/stretch 는 근사상 선언은 생략하되 검증은 한다.
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
    let (mut has_style, mut has_variant, mut has_weight, mut has_stretch) =
        (false, false, false, false);
    for t in &tokens[..si] {
        let tl = t.to_ascii_lowercase();
        if tl == "normal" {
            continue; // 채움값 — 카테고리 안 잡음
        } else if tl == "italic" || tl == "oblique" {
            if has_style {
                return Vec::new();
            }
            has_style = true;
            out.push(Declaration {
                important: false,
                name: "font-style".to_string(),
                value: Value::Keyword("italic".to_string()),
            });
        } else if tl == "small-caps" {
            if has_variant {
                return Vec::new();
            }
            has_variant = true;
        } else if is_weight(&tl) {
            if has_weight {
                return Vec::new();
            }
            has_weight = true;
            let wv = if tl == "bold" {
                Value::Length(700.0, Unit::Number)
            } else if tl == "bolder" || tl == "lighter" {
                Value::Keyword(tl.clone())
            } else {
                Value::Length(tl.parse::<f32>().unwrap_or(400.0), Unit::Number)
            };
            out.push(Declaration { important: false, name: "font-weight".to_string(), value: wv });
        } else if is_stretch(&tl) {
            if has_stretch {
                return Vec::new();
            }
            has_stretch = true;
        } else {
            return Vec::new(); // 미인식 토큰(oldstyle-nums 등) → 무효
        }
    }
    // size[/line-height]
    let mut sp = tokens[si].splitn(2, '/');
    let size = sp.next().unwrap_or(tokens[si]);
    let size_val = match interpret_value(size) {
        Some(v @ (Value::Length(..) | Value::Calc(..) | Value::MinMax(..))) => Some(v),
        _ => {
            let sl = size.to_ascii_lowercase();
            if matches!(sl.as_str(), "larger" | "smaller") {
                Some(Value::Keyword(sl)) // 상대 크기 키워드 보존
            } else {
                font_size_keyword(size).map(|px| Value::Length(px, Unit::Px))
            }
        }
    };
    if let Some(sv) = size_val {
        out.push(Declaration { important: false, name: "font-size".to_string(), value: sv });
    }
    if let Some(lh) = sp.next() {
        out.extend(expand_declaration("line-height", lh)); // 무단위→factor, 길이→그대로
    }
    // family 필수(§CSS Fonts): size 뒤 나머지 전부. 없거나 무효면 단축 전체가 무효.
    if si + 1 >= tokens.len() {
        return Vec::new();
    }
    let family = tokens[si + 1..].join(" ");
    if !font_family_valid(&family) {
        return Vec::new();
    }
    out.push(Declaration {
        important: false,
        name: "font-family".to_string(),
        value: Value::Keyword(family),
    });
    out
}

// 논리 양방향 속성(margin-inline 등) → 두 물리 속성. 1값=양쪽, 2값=start/end.
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
        for tok in split_top_level(part.trim()) {
            let tl = tok.to_ascii_lowercase();
            if is_time(&tl) {
                times.push(tok.to_string());
            } else if is_timing(&tl) {
                tf = Some(tok.to_string());
            } else if tl == "infinite" || tl.parse::<f32>().is_ok() {
                iter = Some(tok.to_string());
            } else if matches!(tl.as_str(), "normal" | "reverse" | "alternate" | "alternate-reverse") {
                dir = Some(tl.clone());
            } else if matches!(tl.as_str(), "forwards" | "backwards" | "both") {
                fill = Some(tl.clone());
            } else if matches!(tl.as_str(), "running" | "paused") {
                state = Some(tl.clone());
            } else {
                name = Some(tok.to_string());
            }
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
    let mut size_tokens: Vec<String> = Vec::new();
    let mut pos_tokens: Vec<String> = Vec::new();

    // 레이어는 괄호 밖 콤마로만 나뉜다. 예전엔 그냥 split(',') 이라
    // `background: rgb(1,2,3)` 이 "rgb(1" 로 잘려 배경색이 통째로 사라졌다.
    // CSS 문법상 색은 마지막 레이어에만 올 수 있으므로, 이미지/반복/크기는 첫 레이어,
    // 색은 마지막 레이어에서 찾는다. (우리는 레이어 1장만 그린다)
    let layers = split_top_level_commas(value_text);
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
        } else if matches!(
            t,
            "scroll" | "fixed" | "local" | "border-box" | "padding-box" | "content-box" | "none"
        ) {
            // attachment/origin 키워드 → 무시
        } else if let Some(v @ Value::Color(..)) = interpret_value(t) {
            color = Some(v);
        }
    }
    if let Some(v) = image {
        out.push(Declaration { important: false, name: "background-image".to_string(), value: v });
    }
    if let Some(v) = color {
        out.push(Declaration { important: false, name: "background-color".to_string(), value: v });
    }
    if let Some(v) = repeat {
        out.push(Declaration { important: false, name: "background-repeat".to_string(), value: v });
    }
    if !size_tokens.is_empty() {
        out.push(Declaration { important: false,
            name: "background-size".to_string(),
            value: Value::Keyword(size_tokens.join(" ")),
        });
    }
    if !pos_tokens.is_empty() {
        out.push(Declaration { important: false,
            name: "background-position".to_string(),
            value: Value::Keyword(pos_tokens.join(" ")),
        });
    }
    out
}

// box-shadow 성분이 유효 <length> 인가(px/em 등, 0, calc-무-%). %·단위없는 수(0 제외)는 무효.
fn is_shadow_length(t: &str) -> bool {
    if t.trim() == "0" {
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
fn border_shorthand(sides: &[&str], value_text: &str) -> Vec<Declaration> {
    let (mut width, mut style, mut color) = (None, None, None);
    for tok in split_top_level(value_text) {
        match interpret_value(tok) {
            Some(v @ Value::Length(..)) => width = Some(v),
            Some(v @ Value::Color(..)) => color = Some(v),
            Some(Value::Keyword(k)) => style = Some(Value::Keyword(k)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for &side in sides {
        if let Some(w) = &width {
            out.push(Declaration { important: false, name: format!("border-{}-width", side), value: w.clone() });
        }
        if let Some(s) = &style {
            out.push(Declaration { important: false, name: format!("border-{}-style", side), value: s.clone() });
        }
        if let Some(c) = &color {
            out.push(Declaration { important: false, name: format!("border-{}-color", side), value: c.clone() });
        }
    }
    out
}

// CSS 박스 단축값(1~4개)을 top/right/bottom/left longhand 로 확장.
// prefix="margin", suffix=""  → margin-top ...
// prefix="border", suffix="-width" → border-top-width ...
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
        assert!(find(&d, "background-size").is_none(), "size 없음");
    }
}
