use super::values::interpret_value;
use super::{Color, Declaration, Unit, Value};

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
        "z-index" => match value_text.trim().parse::<f32>() {
            Ok(n) => vec![Declaration { important: false, name: "z-index".to_string(), value: Value::Length(n, Unit::Number) }],
            _ => Vec::new(),
        },
        // font-weight 의 계산값은 수다(CSS Fonts §2.2). bold=700, normal=400.
        // 예전엔 "bold"/"normal" 키워드로 정규화해서 getComputedStyle 이 "bold" 를
        // 돌려줬다(표준은 "700"). 렌더는 600 이상을 굵게 그린다(폰트가 2종뿐).
        "font-weight" => {
            let v = value_text.trim().to_ascii_lowercase();
            let n = match v.as_str() {
                "bold" | "bolder" => 700.0,
                "normal" | "lighter" => 400.0,
                "initial" => 400.0,
                // inherit/unset/revert 는 선언을 남기지 않는다 → 상속이 적용된다.
                // 예전엔 이걸 "normal" 로 눌러버려서 `font-weight: inherit` 이 상속을
                // 끊었다(react.dev 의 리셋 CSS 가 실제로 이걸 쓴다).
                "inherit" | "unset" | "revert" => return Vec::new(),
                other => match other.parse::<f32>() {
                    Ok(n) if (1.0..=1000.0).contains(&n) => n,
                    _ => return Vec::new(),
                },
            };
            vec![Declaration {
                important: false,
                name: "font-weight".to_string(),
                value: Value::Length(n, Unit::Number),
            }]
        }
        // order: 정수(음수 가능). 단위 없는 수다.
        "order" => match value_text.trim().parse::<f32>() {
            Ok(n) => vec![Declaration { important: false, name: "order".to_string(), value: Value::Length(n, Unit::Number) }],
            _ => Vec::new(),
        },
        // flex-grow/flex-shrink: 단위 없는 수 (레이아웃이 to_px 로 스칼라를 읽는다)
        "flex-grow" | "flex-shrink" => match value_text.trim().parse::<f32>() {
            Ok(n) => vec![Declaration { important: false, name: name.to_string(), value: Value::Length(n, Unit::Number) }],
            _ => Vec::new(),
        },
        // flex 단축: <grow> [<shrink>] [<basis>]. 키워드: none=0 0 auto, auto=1 1 auto,
        // initial=0 1 auto. 숫자 하나(flex:1)=1 1 0% (등폭 핵심), 길이 하나=1 1 <len>.
        "flex" => {
            let v = value_text.trim();
            let (grow, shrink, basis): (f32, f32, Value) = match v {
                "none" => (0.0, 0.0, Value::Keyword("auto".to_string())),
                "auto" => (1.0, 1.0, Value::Keyword("auto".to_string())),
                "initial" | "" => (0.0, 1.0, Value::Keyword("auto".to_string())),
                _ => {
                    let mut grow: Option<f32> = None;
                    let mut shrink: Option<f32> = None;
                    let mut basis: Option<Value> = None;
                    for t in split_top_level(v) {
                        if is_flex_basis_token(t) {
                            if basis.is_none() {
                                basis = Some(parse_flex_basis(t));
                            }
                        } else if let Ok(num) = t.parse::<f32>() {
                            if grow.is_none() {
                                grow = Some(num);
                            } else if shrink.is_none() {
                                shrink = Some(num);
                            }
                        }
                    }
                    // basis 토큰 없이 숫자만 → basis 0% (flex:1 = 1 1 0%)
                    let basis = basis.unwrap_or(Value::Length(0.0, Unit::Percent));
                    (grow.unwrap_or(1.0), shrink.unwrap_or(1.0), basis)
                }
            };
            vec![
                Declaration { important: false, name: "flex-grow".to_string(), value: Value::Length(grow, Unit::Px) },
                Declaration { important: false, name: "flex-shrink".to_string(), value: Value::Length(shrink, Unit::Px) },
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
        "overflow" => {
            let toks = split_top_level(value_text);
            let Some(x) = toks.first().copied() else { return Vec::new() };
            match toks.get(1).copied() {
                None => vec![Declaration { important: false, name: "overflow".to_string(), value: Value::Keyword(x.to_string()) }],
                Some(y) => vec![
                    Declaration { important: false, name: "overflow-x".to_string(), value: Value::Keyword(x.to_string()) },
                    Declaration { important: false, name: "overflow-y".to_string(), value: Value::Keyword(y.to_string()) },
                ],
            }
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
            let mut lines: Vec<&str> = Vec::new();
            let mut color: Option<Value> = None;
            for t in split_top_level(value_text) {
                if matches!(t, "underline" | "overline" | "line-through") {
                    lines.push(t);
                } else if matches!(t, "solid" | "double" | "dotted" | "dashed" | "wavy" | "none") {
                    // style 키워드 / none 은 line 판정에 영향 없음 (none 은 lines 비우기)
                } else if let Some(v @ Value::Color(..)) = interpret_value(t) {
                    color = Some(v);
                }
            }
            let joined = lines.join(" ");
            let mut out = vec![Declaration { important: false,
                name: "text-decoration-line".to_string(),
                value: Value::Keyword(if joined.is_empty() { "none".to_string() } else { joined }),
            }];
            if let Some(c) = color {
                out.push(Declaration { important: false, name: "text-decoration-color".to_string(), value: c });
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
                v.parse::<f32>().ok()
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
        // @font-face src / font-family: 원문 보존(다중 url()·format() 포함). font-face 파서가 해석.
        "src" | "font-family" => {
            vec![Declaration { important: false, name: name.to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
        }
        // transform: 함수 목록(translate/scale/rotate/skew/matrix) 원문 보존.
        // 레이아웃이 2D 행렬로 파싱하고, 페인트가 서브트리를 그 행렬로 변환한다.
        "transform" | "-webkit-transform" => {
            vec![Declaration { important: false, name: "transform".to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
        }
        // transform-origin: "0 0", "left top", "50% 50%" 같은 다중 토큰 값이다.
        // 일반 값 파서는 다중 토큰을 파싱하지 못해 None 을 돌려주고, 그러면 선언이
        // 통째로 사라져서 **항상 중심 기준 회전**이 되어 버린다. 원문을 보존한다.
        "transform-origin" | "-webkit-transform-origin" => {
            vec![Declaration { important: false, name: "transform-origin".to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
        }
        // filter: 색 변환 함수 목록 원문 보존 (paint 가 grayscale/brightness/invert/sepia/contrast 적용).
        "filter" | "-webkit-filter" => {
            vec![Declaration { important: false, name: "filter".to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
        }
        // animation 단축: 이름 토큰만 추출 (정적 렌더가 @keyframes 최종 상태를 적용하기 위함).
        "animation" | "-webkit-animation" => {
            let name = split_top_level(value_text).into_iter().find(|t| is_animation_name(t));
            match name {
                Some(n) => vec![Declaration { important: false, name: "animation-name".to_string(), value: Value::Keyword(n.to_string()) }],
                None => Vec::new(),
            }
        }
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
            ]
        }
        // box-shadow: <dx> <dy> [blur] [spread] <color> (단일 그림자, outset 만)
        "box-shadow" => box_shadow_shorthand(value_text),
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
        "outline" => {
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
            if let Some(w) = width {
                out.push(Declaration { important: false, name: "outline-width".to_string(), value: w });
            }
            if let Some(s) = style {
                out.push(Declaration { important: false, name: "outline-style".to_string(), value: s });
            }
            if let Some(c) = color {
                out.push(Declaration { important: false, name: "outline-color".to_string(), value: c });
            }
            out
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
    // size 토큰: '/' 앞부분이 길이거나 크기 키워드인 첫 토큰
    let is_size = |t: &str| {
        let head = t.split('/').next().unwrap_or(t);
        matches!(interpret_value(head), Some(Value::Length(..))) || font_size_keyword(head).is_some()
    };
    let Some(si) = tokens.iter().position(|t| is_size(t)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    // size 앞: style/weight (variant/stretch 는 근사 무시)
    for t in &tokens[..si] {
        let tl = t.to_ascii_lowercase();
        match tl.as_str() {
            "italic" | "oblique" => out.push(Declaration {
                important: false,
                name: "font-style".to_string(),
                value: Value::Keyword("italic".to_string()),
            }),
            "bold" | "bolder" => out.push(Declaration {
                important: false,
                name: "font-weight".to_string(),
                value: Value::Length(700.0, Unit::Number),
            }),
            _ => {
                if let Ok(n) = tl.parse::<f32>() {
                    if (1.0..=1000.0).contains(&n) {
                        out.push(Declaration {
                            important: false,
                            name: "font-weight".to_string(),
                            value: Value::Length(n, Unit::Number),
                        });
                    }
                }
            }
        }
    }
    // size[/line-height]
    let mut sp = tokens[si].splitn(2, '/');
    let size = sp.next().unwrap_or(tokens[si]);
    let size_val = match interpret_value(size) {
        Some(v @ Value::Length(..)) => Some(v),
        _ => font_size_keyword(size).map(|px| Value::Length(px, Unit::Px)),
    };
    if let Some(sv) = size_val {
        out.push(Declaration { important: false, name: "font-size".to_string(), value: sv });
    }
    if let Some(lh) = sp.next() {
        out.extend(expand_declaration("line-height", lh)); // 무단위→factor, 길이→그대로
    }
    // family: size 뒤 나머지 전부
    if si + 1 < tokens.len() {
        out.push(Declaration {
            important: false,
            name: "font-family".to_string(),
            value: Value::Keyword(tokens[si + 1..].join(" ")),
        });
    }
    out
}

// 논리 양방향 속성(margin-inline 등) → 두 물리 속성. 1값=양쪽, 2값=start/end.
fn logical_pair(start: &str, end: &str, value_text: &str) -> Vec<Declaration> {
    let toks: Vec<&str> = split_top_level(value_text);
    let s = toks.first().copied().unwrap_or("");
    let e = toks.get(1).copied().unwrap_or(s);
    let mut out = expand_declaration(start, s);
    out.extend(expand_declaration(end, e));
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
        if t.starts_with("url(")
            || t.starts_with("linear-gradient(")
            || t.starts_with("radial-gradient(")
            || t.starts_with("conic-gradient(")
        {
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

// `box-shadow: [inset] <dx> <dy> [blur] [spread] <color>` 를 커스텀 longhand 로 확장.
// 다중 그림자는 첫 번째만. paint 가 이 longhand 를 읽는다.
fn box_shadow_shorthand(value_text: &str) -> Vec<Declaration> {
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
        // 한 값이면 overflow 그대로 (소비자가 세 이름을 다 본다)
        let d1 = expand_declaration("overflow", "hidden");
        assert!(matches!(find(&d1, "overflow"), Some(Value::Keyword(k)) if k == "hidden"));

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
        // flex:1 = 1 1 0% (등폭 핵심)
        let d = expand_declaration("flex", "1");
        assert_eq!(find(&d, "flex-grow"), Some(&Value::Length(1.0, Unit::Px)));
        assert_eq!(find(&d, "flex-shrink"), Some(&Value::Length(1.0, Unit::Px)));
        assert_eq!(find(&d, "flex-basis"), Some(&Value::Length(0.0, Unit::Percent)));
        // flex: 2 0 200px
        let d2 = expand_declaration("flex", "2 0 200px");
        assert_eq!(find(&d2, "flex-grow"), Some(&Value::Length(2.0, Unit::Px)));
        assert_eq!(find(&d2, "flex-shrink"), Some(&Value::Length(0.0, Unit::Px)));
        assert_eq!(find(&d2, "flex-basis"), Some(&Value::Length(200.0, Unit::Px)));
        // flex: 200px = 1 1 200px
        let d3 = expand_declaration("flex", "200px");
        assert_eq!(find(&d3, "flex-grow"), Some(&Value::Length(1.0, Unit::Px)));
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
