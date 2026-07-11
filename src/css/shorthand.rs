use super::values::interpret_value;
use super::{Color, Declaration, Unit, Value};

// 선언 하나를 (경우에 따라 여러) longhand 선언으로 확장한다.
pub(crate) fn expand_declaration(name: &str, value_text: &str) -> Vec<Declaration> {
    // 커스텀 프로퍼티(--*): 원문 보존, 사용 시점(var())에 해석.
    if name.starts_with("--") {
        return vec![Declaration {
            name: name.to_string(),
            value: Value::Keyword(value_text.to_string()),
        }];
    }
    // var() 참조: 원문을 Var 로 보존, 스타일 계산 시 치환·재파싱.
    if value_text.contains("var(") {
        return vec![Declaration { name: name.to_string(), value: Value::Var(value_text.to_string()) }];
    }
    match name {
        "margin" | "padding" => box_shorthand(name, "", value_text),
        "border-width" => box_shorthand("border", "-width", value_text),
        "border-color" => box_shorthand("border", "-color", value_text),
        "border-style" => box_shorthand("border", "-style", value_text),
        // border-radius: 첫 토큰만 균일 반경으로 (다중/타원 반경은 근사)
        "border-radius" => match value_text.split_whitespace().next().and_then(interpret_value) {
            Some(v @ Value::Length(..)) => {
                vec![Declaration { name: "border-radius".to_string(), value: v }]
            }
            _ => Vec::new(),
        },
        // z-index: 정수 → Length(n, Px) 로 보존 (paint 가 스택 레벨로 읽음). auto 는 드롭.
        "z-index" => match value_text.trim().parse::<f32>() {
            Ok(n) => vec![Declaration { name: "z-index".to_string(), value: Value::Length(n, Unit::Px) }],
            _ => Vec::new(),
        },
        // font-weight: bold/bolder/숫자>=600 → "bold", 그 외 → "normal" 로 정규화
        // (숫자 weight 는 interpret_value 로 안 살아남아 여기서 처리)
        "font-weight" => {
            let v = value_text.trim();
            let bold = v == "bold"
                || v == "bolder"
                || v.parse::<f32>().map(|n| n >= 600.0).unwrap_or(false);
            vec![Declaration {
                name: "font-weight".to_string(),
                value: Value::Keyword(if bold { "bold" } else { "normal" }.to_string()),
            }]
        }
        // flex-grow: 단위 없는 수 → Length(n, Px) 로 저장(레이아웃이 to_px 로 읽음)
        "flex-grow" => match value_text.trim().parse::<f32>() {
            Ok(n) => vec![Declaration { name: "flex-grow".to_string(), value: Value::Length(n, Unit::Px) }],
            _ => Vec::new(),
        },
        // flex 단축값: 첫 토큰의 grow 만 취함 (flex:1 → grow 1, none → 0, auto → 1)
        "flex" => {
            let first = value_text.split_whitespace().next().unwrap_or("");
            let grow = match first {
                "none" | "initial" => 0.0,
                "auto" => 1.0,
                _ => first.parse::<f32>().unwrap_or(0.0),
            };
            vec![Declaration { name: "flex-grow".to_string(), value: Value::Length(grow, Unit::Px) }]
        }
        // grid 트랙/영역 정의는 다중 토큰 → 원문을 Keyword 로 보존, 레이아웃이 파싱.
        "grid-template-columns" | "grid-template-rows" | "grid-template-areas" | "grid-area"
        | "grid-column" | "grid-row" => {
            vec![Declaration { name: name.to_string(), value: Value::Keyword(value_text.to_string()) }]
        }
        // grid-gap 은 gap 의 레거시 별칭
        "grid-gap" | "grid-column-gap" | "grid-row-gap" => {
            let mapped = name.strip_prefix("grid-").unwrap();
            expand_declaration(mapped, value_text)
        }
        // line-height: 단위 없는 수(1.5)와 퍼센트(150%)는 font-size 배수 → em 으로 저장
        // (스타일 계산 시 요소 font-size 기준 px 로 확정). normal/길이단위는 그대로.
        "line-height" => {
            let v = value_text.trim();
            if v == "normal" {
                return vec![Declaration { name: name.to_string(), value: Value::Keyword("normal".to_string()) }];
            }
            if let Some(pct) = v.strip_suffix('%') {
                if let Ok(n) = pct.trim().parse::<f32>() {
                    return vec![Declaration { name: name.to_string(), value: Value::Length(n / 100.0, Unit::Em) }];
                }
            }
            if let Ok(n) = v.parse::<f32>() {
                return vec![Declaration { name: name.to_string(), value: Value::Length(n, Unit::Em) }];
            }
            match interpret_value(v) {
                Some(value) => vec![Declaration { name: name.to_string(), value }],
                None => Vec::new(),
            }
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
                Some(op) => vec![Declaration {
                    name: "opacity".to_string(),
                    value: Value::Length(op.clamp(0.0, 1.0), Unit::Px),
                }],
                None => Vec::new(),
            }
        }
        // box-shadow: <dx> <dy> [blur] [spread] <color> (단일 그림자, outset 만)
        "box-shadow" => box_shadow_shorthand(value_text),
        // border: <width> <style> <color> (임의 순서) → 네 변 longhand 로
        "border" => border_shorthand(&["top", "right", "bottom", "left"], value_text),
        // background 단축: 색 → background-color, url() → background-image.
        // position/repeat/size/attachment/gradient 등은 근사(드롭).
        "background" => background_shorthand(value_text),
        "border-top" => border_shorthand(&["top"], value_text),
        "border-right" => border_shorthand(&["right"], value_text),
        "border-bottom" => border_shorthand(&["bottom"], value_text),
        "border-left" => border_shorthand(&["left"], value_text),
        _ => match interpret_value(value_text) {
            Some(value) => vec![Declaration { name: name.to_string(), value }],
            None => Vec::new(),
        },
    }
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

// background 단축 → background-color / background-image longhand.
fn background_shorthand(value_text: &str) -> Vec<Declaration> {
    let mut out = Vec::new();
    for tok in split_top_level(value_text) {
        let t = tok.trim();
        if t.starts_with("url(") || t.starts_with("linear-gradient(") {
            if let Some(v) = interpret_value(t) {
                out.push(Declaration { name: "background-image".to_string(), value: v });
            }
        } else if let Some(v @ Value::Color(..)) = interpret_value(t) {
            out.push(Declaration { name: "background-color".to_string(), value: v });
        }
        // position/repeat/size/attachment/none/transparent → 무시
    }
    out
}

// `box-shadow: <dx> <dy> [blur] [spread] <color>` 를 커스텀 longhand 로 확장.
// 다중 그림자는 첫 번째만, inset 은 미지원(드롭). paint 가 이 longhand 를 읽는다.
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
    for tok in first.split_whitespace() {
        if tok == "inset" {
            return Vec::new(); // inset 그림자 미지원
        }
        match interpret_value(tok) {
            Some(Value::Length(v, Unit::Px)) => lens.push(v),
            Some(c @ Value::Color(..)) => color = Some(c),
            _ => {}
        }
    }
    if lens.len() < 2 {
        return Vec::new(); // dx, dy 필수
    }
    let color = color.unwrap_or(Value::Color(Color { r: 0, g: 0, b: 0, a: 128 }));
    let px = |v: f32| Value::Length(v, Unit::Px);
    vec![
        Declaration { name: "box-shadow-x".to_string(), value: px(lens[0]) },
        Declaration { name: "box-shadow-y".to_string(), value: px(lens[1]) },
        Declaration { name: "box-shadow-blur".to_string(), value: px(lens.get(2).copied().unwrap_or(0.0)) },
        Declaration { name: "box-shadow-spread".to_string(), value: px(lens.get(3).copied().unwrap_or(0.0)) },
        Declaration { name: "box-shadow-color".to_string(), value: color },
    ]
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
            out.push(Declaration { name: format!("border-{}-width", side), value: w.clone() });
        }
        if let Some(s) = &style {
            out.push(Declaration { name: format!("border-{}-style", side), value: s.clone() });
        }
        if let Some(c) = &color {
            out.push(Declaration { name: format!("border-{}-color", side), value: c.clone() });
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
    let mk = |side: &str, value: Value| Declaration {
        name: format!("{}-{}{}", prefix, side, suffix),
        value,
    };
    vec![mk("top", top), mk("right", right), mk("bottom", bottom), mk("left", left)]
}
