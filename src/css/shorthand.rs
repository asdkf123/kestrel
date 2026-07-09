use super::values::interpret_value;
use super::{Color, Declaration, Unit, Value};

// 선언 하나를 (경우에 따라 여러) longhand 선언으로 확장한다.
pub(crate) fn expand_declaration(name: &str, value_text: &str) -> Vec<Declaration> {
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
        // grid 트랙 정의는 다중 토큰 → 원문을 Keyword 로 보존, 레이아웃이 파싱.
        "grid-template-columns" | "grid-template-rows" => {
            vec![Declaration { name: name.to_string(), value: Value::Keyword(value_text.to_string()) }]
        }
        // grid-gap 은 gap 의 레거시 별칭
        "grid-gap" | "grid-column-gap" | "grid-row-gap" => {
            let mapped = name.strip_prefix("grid-").unwrap();
            expand_declaration(mapped, value_text)
        }
        // box-shadow: <dx> <dy> [blur] [spread] <color> (단일 그림자, outset 만)
        "box-shadow" => box_shadow_shorthand(value_text),
        // border: <width> <style> <color> (임의 순서) → 네 변 longhand 로
        "border" => border_shorthand(&["top", "right", "bottom", "left"], value_text),
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
