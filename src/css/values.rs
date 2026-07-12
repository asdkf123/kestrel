use super::{Color, Unit, Value};

// 단일 CSS 값 텍스트를 Value 로 해석. 색(#hex/rgb/이름), 길이(px/em/rem/%),
// url(), 키워드. 다중값/calc 등은 None.
pub(crate) fn interpret_value(text: &str) -> Option<Value> {
    if text.is_empty() {
        return None;
    }
    let bytes = text.as_bytes();
    if bytes[0] == b'#' {
        return parse_hex_color(text).map(Value::Color);
    }
    let lower = text.to_ascii_lowercase();
    if lower.starts_with("rgb(") || lower.starts_with("rgba(") {
        return parse_rgb_func(&lower).map(Value::Color);
    }
    if lower.starts_with("hsl(") || lower.starts_with("hsla(") {
        return parse_hsl_func(&lower).map(Value::Color);
    }
    if lower.starts_with("calc(") && text.ends_with(')') {
        return eval_calc(&text[5..text.len() - 1]);
    }
    if lower.starts_with("linear-gradient(") && text.ends_with(')') {
        return parse_linear_gradient(&text[16..text.len() - 1]).map(Value::Gradient);
    }
    if lower.starts_with("radial-gradient(") && text.ends_with(')') {
        return parse_radial_gradient(&text[16..text.len() - 1]).map(Value::Gradient);
    }
    if lower.starts_with("conic-gradient(") && text.ends_with(')') {
        return parse_conic_gradient(&text[15..text.len() - 1]).map(Value::Gradient);
    }
    // min()/max()/clamp() — 인자를 각각 해석해 MinMax 로 (계산은 style/layout).
    for (name, kind) in [
        ("min(", crate::css::MinMaxKind::Min),
        ("max(", crate::css::MinMaxKind::Max),
        ("clamp(", crate::css::MinMaxKind::Clamp),
    ] {
        if lower.starts_with(name) && text.ends_with(')') {
            let inner = &text[name.len()..text.len() - 1];
            let args: Vec<Value> = split_top_commas(inner)
                .iter()
                .filter_map(|a| interpret_value(a.trim()))
                .collect();
            if args.is_empty() {
                return None;
            }
            return Some(Value::MinMax(kind, args));
        }
    }
    // url(...) — 따옴표 유무 모두. URL 은 대소문자 보존을 위해 원본에서 추출.
    if lower.starts_with("url(") && text.ends_with(')') {
        let inner = text[4..text.len() - 1].trim().trim_matches(|c| c == '"' || c == '\'');
        if inner.is_empty() {
            return None;
        }
        return Some(Value::Url(inner.to_string()));
    }
    let numeric_start = bytes[0].is_ascii_digit()
        || bytes[0] == b'.'
        || (bytes[0] == b'-' && bytes.len() > 1 && (bytes[1].is_ascii_digit() || bytes[1] == b'.'));
    if numeric_start {
        let lower_num = text.to_ascii_lowercase();
        // 뷰포트 단위 — 절대 단위보다 먼저 (vmin 이 "in" 접미사에 먼저 걸리지 않도록).
        // 스타일 계산 시 뷰포트 크기로 px 확정.
        for (suffix, unit) in
            [("vmin", Unit::Vmin), ("vmax", Unit::Vmax), ("vw", Unit::Vw), ("vh", Unit::Vh)]
        {
            if let Some(num) = lower_num.strip_suffix(suffix) {
                return num.trim().parse::<f32>().ok().map(|f| Value::Length(f, unit));
            }
        }
        // 절대 단위 → px 즉시 변환 (문맥 불필요). 1px=1/96in, 1pt=1/72in, 1pc=12pt.
        for (suffix, factor) in [
            ("px", 1.0f32),
            ("pt", 96.0 / 72.0),
            ("pc", 16.0),
            ("in", 96.0),
            ("cm", 96.0 / 2.54),
            ("mm", 96.0 / 25.4),
            ("q", 96.0 / (25.4 * 4.0)),
        ] {
            if let Some(num) = lower_num.strip_suffix(suffix) {
                return num.trim().parse::<f32>().ok().map(|f| Value::Length(f * factor, Unit::Px));
            }
        }
        // 상대/문맥 단위. "rem" 을 "em" 보다 먼저. ch/ex 는 em 근사(0.5em).
        for (suffix, unit, scale) in [
            ("rem", Unit::Rem, 1.0f32),
            ("em", Unit::Em, 1.0),
            ("ch", Unit::Em, 0.5),
            ("ex", Unit::Em, 0.5),
            ("%", Unit::Percent, 1.0),
        ] {
            if let Some(num) = lower_num.strip_suffix(suffix) {
                return num.trim().parse::<f32>().ok().map(|f| Value::Length(f * scale, unit));
            }
        }
        // 단위 없는 0 은 유효한 길이 (예: margin: 0 auto)
        if let Ok(f) = text.parse::<f32>() {
            if f == 0.0 {
                return Some(Value::Length(0.0, Unit::Px));
            }
            // 단위 없는 수(column-count/z-index/order 등)는 Keyword 로 보존.
            // Length(px)로 두면 line-height:1.5 가 1.5px 가 되는 등 오작동하므로 Keyword.
            return Some(Value::Keyword(text.to_string()));
        }
        return None;
    }
    if text.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        if let Some(c) = named_color(&lower) {
            return Some(Value::Color(c));
        }
        return Some(Value::Keyword(text.to_string()));
    }
    None // calc()/다중값 등
}

// calc() 평가 → (percent 계수, px 계수) 선형식. px 만이면 Length(px), 혼합이면
// Calc(pct, px). em/rem 등 미지원 단위나 단위 불일치 곱셈이면 None.
// 지원: + - * /, 괄호, px/%/단위없는 수.
#[derive(Clone, Copy)]
struct CalcVal {
    pct: f32,
    px: f32,
    num: f32,
    is_num: bool,
}

fn eval_calc(inner: &str) -> Option<Value> {
    let toks: Vec<char> = inner.chars().collect();
    let mut p = 0usize;
    let v = calc_expr(&toks, &mut p)?;
    skip_ws(&toks, &mut p);
    if p != toks.len() {
        return None;
    }
    if v.is_num {
        return Some(Value::Length(v.num, Unit::Px));
    }
    if v.pct == 0.0 {
        Some(Value::Length(v.px, Unit::Px))
    } else {
        Some(Value::Calc(v.pct, v.px))
    }
}

fn skip_ws(t: &[char], p: &mut usize) {
    while *p < t.len() && t[*p].is_whitespace() {
        *p += 1;
    }
}

// expr = term (('+'|'-') term)*
fn calc_expr(t: &[char], p: &mut usize) -> Option<CalcVal> {
    let mut acc = calc_term(t, p)?;
    loop {
        skip_ws(t, p);
        let op = match t.get(*p) {
            Some('+') => '+',
            Some('-') => '-',
            _ => break,
        };
        *p += 1;
        let rhs = calc_term(t, p)?;
        // 덧셈/뺄셈은 길이+길이 또는 수+수만
        if acc.is_num != rhs.is_num {
            return None;
        }
        let s = if op == '+' { 1.0 } else { -1.0 };
        acc = CalcVal {
            pct: acc.pct + s * rhs.pct,
            px: acc.px + s * rhs.px,
            num: acc.num + s * rhs.num,
            is_num: acc.is_num,
        };
    }
    Some(acc)
}

// term = factor (('*'|'/') factor)*
fn calc_term(t: &[char], p: &mut usize) -> Option<CalcVal> {
    let mut acc = calc_factor(t, p)?;
    loop {
        skip_ws(t, p);
        let op = match t.get(*p) {
            Some('*') => '*',
            Some('/') => '/',
            _ => break,
        };
        *p += 1;
        let rhs = calc_factor(t, p)?;
        acc = match op {
            '*' => {
                // 하나는 반드시 수(단위 없음)
                if acc.is_num {
                    CalcVal { pct: rhs.pct * acc.num, px: rhs.px * acc.num, num: rhs.num * acc.num, is_num: rhs.is_num }
                } else if rhs.is_num {
                    CalcVal { pct: acc.pct * rhs.num, px: acc.px * rhs.num, num: acc.num * rhs.num, is_num: false }
                } else {
                    return None;
                }
            }
            _ => {
                // 나눗셈: 우변은 수
                if !rhs.is_num || rhs.num == 0.0 {
                    return None;
                }
                CalcVal { pct: acc.pct / rhs.num, px: acc.px / rhs.num, num: acc.num / rhs.num, is_num: acc.is_num }
            }
        };
    }
    Some(acc)
}

// factor = '(' expr ')' | number[unit]
fn calc_factor(t: &[char], p: &mut usize) -> Option<CalcVal> {
    skip_ws(t, p);
    if t.get(*p) == Some(&'(') {
        *p += 1;
        let v = calc_expr(t, p)?;
        skip_ws(t, p);
        if t.get(*p) != Some(&')') {
            return None;
        }
        *p += 1;
        return Some(v);
    }
    // 숫자 + 선택적 단위
    let start = *p;
    if t.get(*p) == Some(&'-') || t.get(*p) == Some(&'+') {
        *p += 1;
    }
    while *p < t.len() && (t[*p].is_ascii_digit() || t[*p] == '.') {
        *p += 1;
    }
    if *p == start || (*p == start + 1 && !t[start].is_ascii_digit()) {
        return None;
    }
    let num: f32 = t[start..*p].iter().collect::<String>().parse().ok()?;
    // 단위
    let ustart = *p;
    while *p < t.len() && (t[*p].is_ascii_alphabetic() || t[*p] == '%') {
        *p += 1;
    }
    let unit: String = t[ustart..*p].iter().collect::<String>().to_ascii_lowercase();
    match unit.as_str() {
        "" => Some(CalcVal { pct: 0.0, px: 0.0, num, is_num: true }),
        "px" => Some(CalcVal { pct: 0.0, px: num, num: 0.0, is_num: false }),
        "%" => Some(CalcVal { pct: num, px: 0.0, num: 0.0, is_num: false }),
        _ => None, // em/rem/vw 등 미지원
    }
}

// linear-gradient 인자 파싱: [<angle|to side>,] <color> [pos%], ...
fn parse_linear_gradient(inner: &str) -> Option<crate::css::Gradient> {
    // 최상위 콤마로 분리 (색함수 안 콤마 보존)
    let parts = split_top_commas(inner);
    if parts.is_empty() {
        return None;
    }
    let mut idx = 0;
    let mut angle = 180.0f32; // 기본: to bottom
    let first = parts[0].trim();
    let fl = first.to_ascii_lowercase();
    if let Some(deg) = fl.strip_suffix("deg") {
        if let Ok(a) = deg.trim().parse::<f32>() {
            angle = a;
            idx = 1;
        }
    } else if fl.starts_with("to ") {
        angle = match fl.trim() {
            "to top" => 0.0,
            "to right" => 90.0,
            "to bottom" => 180.0,
            "to left" => 270.0,
            "to top right" | "to right top" => 45.0,
            "to bottom right" | "to right bottom" => 135.0,
            "to bottom left" | "to left bottom" => 225.0,
            "to top left" | "to left top" => 315.0,
            _ => 180.0,
        };
        idx = 1;
    } else if fl.starts_with("turn") || fl.ends_with("turn") {
        if let Ok(t) = fl.trim_end_matches("turn").trim().parse::<f32>() {
            angle = t * 360.0;
            idx = 1;
        }
    }
    let stops = parse_color_stops(&parts[idx..])?;
    Some(crate::css::Gradient { angle_deg: angle, radial: false, conic: false, stops })
}

// radial-gradient([shape size at pos,]? stop, ...) — 모양/크기/위치는 근사(중심 방사,
// 박스 반경까지 채움)로 무시하고, 첫 파트가 색이 아니면 서술자로 보고 건너뛴다.
fn parse_radial_gradient(inner: &str) -> Option<crate::css::Gradient> {
    let parts = split_top_commas(inner);
    if parts.is_empty() {
        return None;
    }
    // 첫 파트의 첫 토큰이 색이면 서술자 없음, 아니면 서술자로 스킵
    let first_is_color = split_top_level(parts[0].trim())
        .first()
        .and_then(|t| interpret_value(t))
        .map(|v| matches!(v, Value::Color(_)))
        .unwrap_or(false);
    let idx = if first_is_color { 0 } else { 1 };
    if idx >= parts.len() {
        return None;
    }
    let stops = parse_color_stops(&parts[idx..])?;
    Some(crate::css::Gradient { angle_deg: 0.0, radial: true, conic: false, stops })
}

// conic-gradient([from Ndeg] [at pos,]? stop, ...) — from/at 서술자는 근사로 무시.
// 색 스톱 위치는 각도(0-360deg 또는 %)를 0-1 로 정규화.
fn parse_conic_gradient(inner: &str) -> Option<crate::css::Gradient> {
    let parts = split_top_commas(inner);
    if parts.is_empty() {
        return None;
    }
    let first = parts[0].trim().to_ascii_lowercase();
    let idx = if first.starts_with("from") || first.starts_with("at") { 1 } else { 0 };
    if idx >= parts.len() {
        return None;
    }
    // 각도 위치(Ndeg)를 % 로 바꿔 parse_color_stops 가 처리하도록 전처리
    let mut stops: Vec<(Color, Option<f32>)> = Vec::new();
    for p in &parts[idx..] {
        let toks = split_top_level(p.trim());
        if toks.is_empty() {
            continue;
        }
        let color = match interpret_value(&toks[0]) {
            Some(Value::Color(c)) => c,
            _ => continue,
        };
        let pos = toks.get(1).and_then(|t| {
            if let Some(d) = t.strip_suffix("deg") {
                d.trim().parse::<f32>().ok().map(|v| v / 360.0)
            } else {
                t.strip_suffix('%').and_then(|n| n.trim().parse::<f32>().ok()).map(|v| v / 100.0)
            }
        });
        stops.push((color, pos));
    }
    if stops.len() < 2 {
        return None;
    }
    let n = stops.len();
    let resolved: Vec<(Color, f32)> = stops
        .iter()
        .enumerate()
        .map(|(i, (c, p))| (*c, p.unwrap_or(i as f32 / (n as f32 - 1.0))))
        .collect();
    Some(crate::css::Gradient { angle_deg: 0.0, radial: false, conic: true, stops: resolved })
}

// 색 스톱 목록 파싱. 위치 미지정 스톱은 균등 분배. 스톱 2개 미만이면 None.
fn parse_color_stops(parts: &[String]) -> Option<Vec<(Color, f32)>> {
    let mut stops: Vec<(Color, Option<f32>)> = Vec::new();
    for p in parts {
        let toks = split_top_level(p.trim());
        if toks.is_empty() {
            continue;
        }
        let color = match interpret_value(&toks[0]) {
            Some(Value::Color(c)) => c,
            _ => continue,
        };
        let pos = toks
            .get(1)
            .and_then(|t| t.strip_suffix('%'))
            .and_then(|n| n.trim().parse::<f32>().ok())
            .map(|p| p / 100.0);
        stops.push((color, pos));
    }
    if stops.len() < 2 {
        return None;
    }
    let n = stops.len();
    Some(
        stops
            .iter()
            .enumerate()
            .map(|(i, (c, p))| (*c, p.unwrap_or(i as f32 / (n as f32 - 1.0))))
            .collect(),
    )
}

// 최상위(괄호 밖) 콤마로 분리
fn split_top_commas(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    for c in s.chars() {
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
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

// 공백으로 최상위 토큰 분리 (색함수 괄호 보존)
fn split_top_level(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    for c in s.chars() {
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

fn parse_hex_color(text: &str) -> Option<Color> {
    let hex = &text[1..];
    match hex.len() {
        3 => {
            let r = u8::from_str_radix(&hex[0..1], 16).ok()?;
            let g = u8::from_str_radix(&hex[1..2], 16).ok()?;
            let b = u8::from_str_radix(&hex[2..3], 16).ok()?;
            // 0xN → 0xNN (N*17)
            Some(Color { r: r * 17, g: g * 17, b: b * 17, a: 255 })
        }
        4 => {
            // #rgba — 각 니블 ×17, 알파 포함
            let r = u8::from_str_radix(&hex[0..1], 16).ok()?;
            let g = u8::from_str_radix(&hex[1..2], 16).ok()?;
            let b = u8::from_str_radix(&hex[2..3], 16).ok()?;
            let a = u8::from_str_radix(&hex[3..4], 16).ok()?;
            Some(Color { r: r * 17, g: g * 17, b: b * 17, a: a * 17 })
        }
        6 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            Some(Color { r, g, b, a: 255 })
        }
        8 => {
            // #rrggbbaa
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            let a = u8::from_str_radix(&hex[6..8], 16).ok()?;
            Some(Color { r, g, b, a })
        }
        _ => None,
    }
}

// 콤마 또는 공백 구분(모던 문법), '/' 알파 모두 수용.
fn color_parts(inner: &str) -> Vec<String> {
    inner
        .replace('/', " ")
        .split(|c| c == ',' || c == ' ' || c == '\t')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

// 채널 값: 0-255 정수/실수 또는 퍼센트(0-100%).
fn chan_val(s: &str) -> Option<u8> {
    if let Some(p) = s.strip_suffix('%') {
        return Some((p.trim().parse::<f32>().ok()? / 100.0 * 255.0).clamp(0.0, 255.0).round() as u8);
    }
    Some(s.parse::<f32>().ok()?.clamp(0.0, 255.0) as u8)
}

fn alpha_val(s: &str) -> Option<u8> {
    if let Some(p) = s.strip_suffix('%') {
        return Some((p.trim().parse::<f32>().ok()? / 100.0 * 255.0).clamp(0.0, 255.0).round() as u8);
    }
    Some((s.parse::<f32>().ok()?.clamp(0.0, 1.0) * 255.0).round() as u8)
}

fn parse_rgb_func(text: &str) -> Option<Color> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    let parts = color_parts(&text[open + 1..close]);
    if parts.len() != 3 && parts.len() != 4 {
        return None;
    }
    let r = chan_val(&parts[0])?;
    let g = chan_val(&parts[1])?;
    let b = chan_val(&parts[2])?;
    let a = if parts.len() == 4 { alpha_val(&parts[3])? } else { 255 };
    Some(Color { r, g, b, a })
}

fn parse_hsl_func(text: &str) -> Option<Color> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    let parts = color_parts(&text[open + 1..close]);
    if parts.len() != 3 && parts.len() != 4 {
        return None;
    }
    let h = parts[0].trim_end_matches("deg").trim().parse::<f32>().ok()?;
    let s = parts[1].trim_end_matches('%').trim().parse::<f32>().ok()? / 100.0;
    let l = parts[2].trim_end_matches('%').trim().parse::<f32>().ok()? / 100.0;
    let a = if parts.len() == 4 { alpha_val(&parts[3])? } else { 255 };
    let (r, g, b) = hsl_to_rgb(h, s.clamp(0.0, 1.0), l.clamp(0.0, 1.0));
    Some(Color { r, g, b, a })
}

// HSL(각도, 채도[0-1], 명도[0-1]) → RGB. 표준 변환.
fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let h = ((h % 360.0) + 360.0) % 360.0;
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - (((h / 60.0) % 2.0) - 1.0).abs());
    let m = l - c / 2.0;
    let (r1, g1, b1) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let to = |v: f32| ((v + m).clamp(0.0, 1.0) * 255.0).round() as u8;
    (to(r1), to(g1), to(b1))
}

// CSS <named-color> 전체 (CSS Color Level 4) + transparent.
fn named_color(name: &str) -> Option<Color> {
    if name == "transparent" {
        return Some(Color { r: 0, g: 0, b: 0, a: 0 });
    }
    let rgb = match name {
        "aliceblue" => (240, 248, 255),
        "antiquewhite" => (250, 235, 215),
        "aqua" | "cyan" => (0, 255, 255),
        "aquamarine" => (127, 255, 212),
        "azure" => (240, 255, 255),
        "beige" => (245, 245, 220),
        "bisque" => (255, 228, 196),
        "black" => (0, 0, 0),
        "blanchedalmond" => (255, 235, 205),
        "blue" => (0, 0, 255),
        "blueviolet" => (138, 43, 226),
        "brown" => (165, 42, 42),
        "burlywood" => (222, 184, 135),
        "cadetblue" => (95, 158, 160),
        "chartreuse" => (127, 255, 0),
        "chocolate" => (210, 105, 30),
        "coral" => (255, 127, 80),
        "cornflowerblue" => (100, 149, 237),
        "cornsilk" => (255, 248, 220),
        "crimson" => (220, 20, 60),
        "darkblue" => (0, 0, 139),
        "darkcyan" => (0, 139, 139),
        "darkgoldenrod" => (184, 134, 11),
        "darkgray" | "darkgrey" => (169, 169, 169),
        "darkgreen" => (0, 100, 0),
        "darkkhaki" => (189, 183, 107),
        "darkmagenta" => (139, 0, 139),
        "darkolivegreen" => (85, 107, 47),
        "darkorange" => (255, 140, 0),
        "darkorchid" => (153, 50, 204),
        "darkred" => (139, 0, 0),
        "darksalmon" => (233, 150, 122),
        "darkseagreen" => (143, 188, 143),
        "darkslateblue" => (72, 61, 139),
        "darkslategray" | "darkslategrey" => (47, 79, 79),
        "darkturquoise" => (0, 206, 209),
        "darkviolet" => (148, 0, 211),
        "deeppink" => (255, 20, 147),
        "deepskyblue" => (0, 191, 255),
        "dimgray" | "dimgrey" => (105, 105, 105),
        "dodgerblue" => (30, 144, 255),
        "firebrick" => (178, 34, 34),
        "floralwhite" => (255, 250, 240),
        "forestgreen" => (34, 139, 34),
        "fuchsia" | "magenta" => (255, 0, 255),
        "gainsboro" => (220, 220, 220),
        "ghostwhite" => (248, 248, 255),
        "gold" => (255, 215, 0),
        "goldenrod" => (218, 165, 32),
        "gray" | "grey" => (128, 128, 128),
        "green" => (0, 128, 0),
        "greenyellow" => (173, 255, 47),
        "honeydew" => (240, 255, 240),
        "hotpink" => (255, 105, 180),
        "indianred" => (205, 92, 92),
        "indigo" => (75, 0, 130),
        "ivory" => (255, 255, 240),
        "khaki" => (240, 230, 140),
        "lavender" => (230, 230, 250),
        "lavenderblush" => (255, 240, 245),
        "lawngreen" => (124, 252, 0),
        "lemonchiffon" => (255, 250, 205),
        "lightblue" => (173, 216, 230),
        "lightcoral" => (240, 128, 128),
        "lightcyan" => (224, 255, 255),
        "lightgoldenrodyellow" => (250, 250, 210),
        "lightgray" | "lightgrey" => (211, 211, 211),
        "lightgreen" => (144, 238, 144),
        "lightpink" => (255, 182, 193),
        "lightsalmon" => (255, 160, 122),
        "lightseagreen" => (32, 178, 170),
        "lightskyblue" => (135, 206, 250),
        "lightslategray" | "lightslategrey" => (119, 136, 153),
        "lightsteelblue" => (176, 196, 222),
        "lightyellow" => (255, 255, 224),
        "lime" => (0, 255, 0),
        "limegreen" => (50, 205, 50),
        "linen" => (250, 240, 230),
        "maroon" => (128, 0, 0),
        "mediumaquamarine" => (102, 205, 170),
        "mediumblue" => (0, 0, 205),
        "mediumorchid" => (186, 85, 211),
        "mediumpurple" => (147, 112, 219),
        "mediumseagreen" => (60, 179, 113),
        "mediumslateblue" => (123, 104, 238),
        "mediumspringgreen" => (0, 250, 154),
        "mediumturquoise" => (72, 209, 204),
        "mediumvioletred" => (199, 21, 133),
        "midnightblue" => (25, 25, 112),
        "mintcream" => (245, 255, 250),
        "mistyrose" => (255, 228, 225),
        "moccasin" => (255, 228, 181),
        "navajowhite" => (255, 222, 173),
        "navy" => (0, 0, 128),
        "oldlace" => (253, 245, 230),
        "olive" => (128, 128, 0),
        "olivedrab" => (107, 142, 35),
        "orange" => (255, 165, 0),
        "orangered" => (255, 69, 0),
        "orchid" => (218, 112, 214),
        "palegoldenrod" => (238, 232, 170),
        "palegreen" => (152, 251, 152),
        "paleturquoise" => (175, 238, 238),
        "palevioletred" => (219, 112, 147),
        "papayawhip" => (255, 239, 213),
        "peachpuff" => (255, 218, 185),
        "peru" => (205, 133, 63),
        "pink" => (255, 192, 203),
        "plum" => (221, 160, 221),
        "powderblue" => (176, 224, 230),
        "purple" => (128, 0, 128),
        "rebeccapurple" => (102, 51, 153),
        "red" => (255, 0, 0),
        "rosybrown" => (188, 143, 143),
        "royalblue" => (65, 105, 225),
        "saddlebrown" => (139, 69, 19),
        "salmon" => (250, 128, 114),
        "sandybrown" => (244, 164, 96),
        "seagreen" => (46, 139, 87),
        "seashell" => (255, 245, 238),
        "sienna" => (160, 82, 45),
        "silver" => (192, 192, 192),
        "skyblue" => (135, 206, 235),
        "slateblue" => (106, 90, 205),
        "slategray" | "slategrey" => (112, 128, 144),
        "snow" => (255, 250, 250),
        "springgreen" => (0, 255, 127),
        "steelblue" => (70, 130, 180),
        "tan" => (210, 180, 140),
        "teal" => (0, 128, 128),
        "thistle" => (216, 191, 216),
        "tomato" => (255, 99, 71),
        "turquoise" => (64, 224, 208),
        "violet" => (238, 130, 238),
        "wheat" => (245, 222, 179),
        "white" => (255, 255, 255),
        "whitesmoke" => (245, 245, 245),
        "yellow" => (255, 255, 0),
        "yellowgreen" => (154, 205, 50),
        _ => return None,
    };
    Some(Color { r: rgb.0, g: rgb.1, b: rgb.2, a: 255 })
}

pub(crate) fn valid_identifier_char(c: char) -> bool {
    matches!(c, 'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::Color;

    fn color(s: &str) -> Color {
        match interpret_value(s) {
            Some(Value::Color(c)) => c,
            other => panic!("expected color, got {:?}", other),
        }
    }

    #[test]
    fn absolute_units_convert_to_px() {
        // 1pt = 96/72 px, 1pc = 16px, 1in = 96px, 1cm ≈ 37.8px
        assert_eq!(interpret_value("72pt"), Some(Value::Length(96.0, Unit::Px)));
        assert_eq!(interpret_value("1pc"), Some(Value::Length(16.0, Unit::Px)));
        assert_eq!(interpret_value("1in"), Some(Value::Length(96.0, Unit::Px)));
        let cm = match interpret_value("2.54cm") {
            Some(Value::Length(v, Unit::Px)) => v,
            other => panic!("expected px, got {:?}", other),
        };
        assert!((cm - 96.0).abs() < 0.01, "2.54cm ≈ 96px, 실제 {}", cm);
        // ch/ex 는 0.5em 근사로 저장
        assert_eq!(interpret_value("2ch"), Some(Value::Length(1.0, Unit::Em)));
    }

    #[test]
    fn hex4_and_hex8_alpha() {
        // #rgba / #rrggbbaa (CSS Color 4) — 이전엔 드롭됐음
        assert_eq!(color("#ff000080"), Color { r: 255, g: 0, b: 0, a: 128 });
        assert_eq!(color("#f008"), Color { r: 255, g: 0, b: 0, a: 136 });
    }

    #[test]
    fn extended_named_colors() {
        // CSS Level 4 확장 이름 색 (이전엔 미지원)
        assert_eq!(color("tomato"), Color { r: 255, g: 99, b: 71, a: 255 });
        assert_eq!(color("steelblue"), Color { r: 70, g: 130, b: 180, a: 255 });
        assert_eq!(color("rebeccapurple"), Color { r: 102, g: 51, b: 153, a: 255 });
        assert_eq!(color("crimson"), Color { r: 220, g: 20, b: 60, a: 255 });
        assert_eq!(color("dodgerblue"), Color { r: 30, g: 144, b: 255, a: 255 });
        // 대소문자 무시
        assert_eq!(color("ForestGreen"), Color { r: 34, g: 139, b: 34, a: 255 });
    }

    #[test]
    fn hsl_and_modern_color_syntax() {
        // hsl: 빨강(0도, 100%, 50%)
        assert_eq!(color("hsl(0, 100%, 50%)"), Color { r: 255, g: 0, b: 0, a: 255 });
        // hsl 초록(120도)
        assert_eq!(color("hsl(120, 100%, 50%)"), Color { r: 0, g: 255, b: 0, a: 255 });
        // hsla 알파
        assert_eq!(color("hsla(240, 100%, 50%, 0.5)").b, 255);
        assert_eq!(color("hsla(240, 100%, 50%, 0.5)").a, 128);
        // 공백 구분(모던) rgb
        assert_eq!(color("rgb(10 20 30)"), Color { r: 10, g: 20, b: 30, a: 255 });
        // 퍼센트 채널 + / 알파
        assert_eq!(color("rgb(100% 0% 0% / 0.5)"), Color { r: 255, g: 0, b: 0, a: 128 });
    }
}
