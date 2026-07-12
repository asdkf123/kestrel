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
            let toks: Vec<Value> = hpart
                .split_whitespace()
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
            Ok(n) => vec![Declaration { important: false, name: "z-index".to_string(), value: Value::Length(n, Unit::Px) }],
            _ => Vec::new(),
        },
        // font-weight: bold/bolder/숫자>=600 → "bold", 그 외 → "normal" 로 정규화
        // (숫자 weight 는 interpret_value 로 안 살아남아 여기서 처리)
        "font-weight" => {
            let v = value_text.trim();
            let bold = v == "bold"
                || v == "bolder"
                || v.parse::<f32>().map(|n| n >= 600.0).unwrap_or(false);
            vec![Declaration { important: false,
                name: "font-weight".to_string(),
                value: Value::Keyword(if bold { "bold" } else { "normal" }.to_string()),
            }]
        }
        // order: 정수(음수 가능) → Length(n, Px) (flex 아이템 재정렬용)
        "order" => match value_text.trim().parse::<f32>() {
            Ok(n) => vec![Declaration { important: false, name: "order".to_string(), value: Value::Length(n, Unit::Px) }],
            _ => Vec::new(),
        },
        // flex-grow/flex-shrink: 단위 없는 수 → Length(n, Px) (레이아웃이 to_px 로 읽음)
        "flex-grow" | "flex-shrink" => match value_text.trim().parse::<f32>() {
            Ok(n) => vec![Declaration { important: false, name: name.to_string(), value: Value::Length(n, Unit::Px) }],
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
                    for t in v.split_whitespace() {
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
            let toks: Vec<&str> = value_text.split_whitespace().collect();
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
            for t in value_text.split_whitespace() {
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
        // opacity: 0..1 수 또는 퍼센트(50%). 스칼라를 Length(op, Px)로 실어 paint 가 읽음.
        // (미지원 단위 아님 — 파서가 0 아닌 단위없는 수를 드롭하므로 여기서 처리.)
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
                    value: Value::Length(op.clamp(0.0, 1.0), Unit::Px),
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
        // transform: 함수 목록(translate/scale/rotate...) 원문 보존, 레이아웃이 파싱.
        // (translate 만 시각 오프셋으로 적용, 나머지는 근사/무시)
        "transform" => {
            vec![Declaration { important: false, name: "transform".to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
        }
        // filter: 색 변환 함수 목록 원문 보존 (paint 가 grayscale/brightness/invert/sepia/contrast 적용).
        "filter" | "-webkit-filter" => {
            vec![Declaration { important: false, name: "filter".to_string(), value: Value::Keyword(value_text.trim().to_string()) }]
        }
        // animation 단축: 이름 토큰만 추출 (정적 렌더가 @keyframes 최종 상태를 적용하기 위함).
        "animation" | "-webkit-animation" => {
            let name = value_text.split_whitespace().find(|t| is_animation_name(t));
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
            for tok in value_text[..end].split_whitespace() {
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
            for tok in value_text.split_whitespace() {
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
            for tok in value_text.split_whitespace() {
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
    let tokens: Vec<&str> = v.split_whitespace().collect();
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
                value: Value::Keyword("bold".to_string()),
            }),
            _ => {
                if tl.parse::<f32>().map(|n| n >= 600.0).unwrap_or(false) {
                    out.push(Declaration {
                        important: false,
                        name: "font-weight".to_string(),
                        value: Value::Keyword("bold".to_string()),
                    });
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
    let toks: Vec<&str> = value_text.split_whitespace().collect();
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
fn split_top_level(text: &str) -> Vec<String> {
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

// background 단축 → background-color/image/repeat/size longhand.
// `background: #fff url(x) no-repeat center / cover` 처럼 repeat/size 도 추출해
// 기존 background-repeat/background-size 렌더 경로를 활성화한다. (position 은 렌더
// 미구현이라 아직 무시.)
fn background_shorthand(value_text: &str) -> Vec<Declaration> {
    let mut out = Vec::new();
    let mut image = None;
    let mut color = None;
    let mut repeat = None;
    let mut size = None;
    let mut pos_tokens: Vec<String> = Vec::new();

    let has_gradient = value_text.contains("gradient(");
    // 그라디언트가 없으면 다중 레이어(콤마) 중 첫 레이어만. gradient 안 콤마 보호 위해
    // gradient 있을 땐 전체를 그대로 쓴다 (split_top_level 이 괄호를 존중).
    let layer: String = if has_gradient {
        value_text.to_string()
    } else {
        value_text.split(',').next().unwrap_or("").to_string()
    };
    // "center/cover" 처럼 붙은 슬래시를 토큰화하기 위해 공백 삽입 (gradient 없을 때만).
    let normalized = if has_gradient { layer.clone() } else { layer.replace('/', " / ") };

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
            // background-size 자리
            match t {
                "cover" | "contain" | "auto" => size = Some(Value::Keyword(t.to_string())),
                _ => {
                    if size.is_none() {
                        if let Some(v) = interpret_value(t) {
                            size = Some(v);
                        }
                    }
                }
            }
            continue;
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
    if let Some(v) = size {
        out.push(Declaration { important: false, name: "background-size".to_string(), value: v });
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
    for tok in first.split_whitespace() {
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
    for tok in value_text.split_whitespace() {
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
    let tokens: Vec<Value> = value_text.split_whitespace().filter_map(interpret_value).collect();
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
        assert!(matches!(find(&d, "font-weight"), Some(Value::Keyword(k)) if k == "bold"));
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
    fn background_shorthand_repeat_x_only() {
        let d = expand_declaration("background", "url(a.png) repeat-x");
        assert!(
            matches!(find(&d, "background-repeat"), Some(Value::Keyword(k)) if k == "repeat-x"),
            "repeat-x"
        );
        assert!(find(&d, "background-size").is_none(), "size 없음");
    }
}
