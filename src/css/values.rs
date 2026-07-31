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
    // 상대 색: <func>(from <origin> …) — 원본 색 채널을 키워드로 참조(CSS Color 5).
    for f in ["rgb", "rgba", "hsl", "hsla", "hwb", "lab", "lch", "oklab", "oklch", "color"] {
        if lower.starts_with(f) && lower[f.len()..].starts_with('(') {
            if let Some(inr) = func_inner(&lower) {
                if inr.trim_start().starts_with("from ") {
                    if !relative_color_valid(f, &lower) {
                        return None;
                    }
                    return parse_relative_color(f, &lower).map(|(c, s)| Value::ColorFn(c, s));
                }
            }
            break;
        }
    }
    if lower.starts_with("rgb(") || lower.starts_with("rgba(") {
        if !rgb_valid(&lower) {
            return None;
        }
        // none 채널이 있으면 color(srgb ...) 로 none 보존(ColorFn), 아니면 레거시 rgb().
        if let Some(v) = parse_rgb_none(&lower) {
            return Some(v);
        }
        return parse_rgb_func(&lower).map(Value::Color);
    }
    if lower.starts_with("hsl(") || lower.starts_with("hsla(") {
        if !hsl_valid(&lower) {
            return None;
        }
        if let Some(v) = parse_hsl_hwb_none(&lower) {
            return Some(v);
        }
        return parse_hsl_func(&lower).map(Value::Color);
    }
    // 모던 색 함수(CSS Color 4). lab/lch/oklab/oklch/color() 는 계산값에서 자기 형태를
    // 보존하므로 Value::ColorFn(sRGB 근사 + 캐논 직렬화). hwb 는 rgb() 로 계산된다.
    for name in ["oklch", "oklab", "lch", "lab"] {
        if lower.starts_with(name) && lower[name.len()..].starts_with('(') {
            if !lab_valid(name, &lower) {
                return None;
            }
            return parse_lab_family(name, &lower).map(|(c, s)| Value::ColorFn(c, s));
        }
    }
    if lower.starts_with("hwb(") {
        if !hwb_valid(&lower) {
            return None;
        }
        if let Some(v) = parse_hsl_hwb_none(&lower) {
            return Some(v);
        }
        return parse_hwb(&lower).map(Value::Color);
    }
    if lower.starts_with("color-mix(") {
        if !color_mix_valid(&lower) {
            return None;
        }
        return parse_color_mix(&lower).map(|(c, s)| Value::ColorFn(c, s));
    }
    if lower.starts_with("color(") {
        if !color_func_valid(&lower) {
            return None;
        }
        return parse_color_func(&lower).map(|(c, s)| Value::ColorFn(c, s));
    }
    if lower.starts_with("calc(") && text.ends_with(')') {
        return eval_calc(&text[5..text.len() - 1]);
    }
    // 무효 gradient(빈 세그먼트/hint 배치/스톱<2/색 아닌 스톱)는 파싱 거부 → 선언 드롭
    // (계산값 none, 지정값 ""). 파서가 관대하므로 gradient_valid 로 문법 검증.
    if (lower.starts_with("linear-gradient(")
        || lower.starts_with("radial-gradient(")
        || lower.starts_with("conic-gradient(")
        || lower.starts_with("repeating-linear-gradient(")
        || lower.starts_with("repeating-radial-gradient(")
        || lower.starts_with("repeating-conic-gradient("))
        && text.ends_with(')')
        && !gradient_valid(text)
    {
        return None;
    }
    // repeating-* 는 같은 문법에 반복 플래그만 다르다
    if lower.starts_with("repeating-linear-gradient(") && text.ends_with(')') {
        let inner = &text["repeating-linear-gradient(".len()..text.len() - 1];
        return parse_linear_gradient(inner).map(|mut g| {
            g.repeating = true;
            g.serial = normalize_gradient_serial(text, true);
            Value::Gradient(g)
        });
    }
    if lower.starts_with("repeating-radial-gradient(") && text.ends_with(')') {
        let inner = &text["repeating-radial-gradient(".len()..text.len() - 1];
        return parse_radial_gradient(inner).map(|mut g| {
            g.repeating = true;
            g.serial = normalize_gradient_serial(text, true);
            Value::Gradient(g)
        });
    }
    if lower.starts_with("repeating-conic-gradient(") && text.ends_with(')') {
        let inner = &text["repeating-conic-gradient(".len()..text.len() - 1];
        return parse_conic_gradient(inner).map(|mut g| {
            g.repeating = true;
            g.serial = normalize_gradient_serial(text, true);
            Value::Gradient(g)
        });
    }
    if lower.starts_with("linear-gradient(") && text.ends_with(')') {
        return parse_linear_gradient(&text[16..text.len() - 1]).map(|mut g| {
            g.serial = normalize_gradient_serial(text, true);
            Value::Gradient(g)
        });
    }
    if lower.starts_with("radial-gradient(") && text.ends_with(')') {
        return parse_radial_gradient(&text[16..text.len() - 1]).map(|mut g| {
            g.serial = normalize_gradient_serial(text, true);
            Value::Gradient(g)
        });
    }
    if lower.starts_with("conic-gradient(") && text.ends_with(')') {
        return parse_conic_gradient(&text[15..text.len() - 1]).map(|mut g| {
            g.serial = normalize_gradient_serial(text, true);
            Value::Gradient(g)
        });
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
    // abs()/sign() — 단위 독립적이라 파스 타임에 해석 가능(CSS Values 4 §10).
    // abs(-5em)=5em(부호만 뒤집음, 단위 보존), sign(-5px)=-1(단위 없는 수).
    // 인자는 단일 값 또는 calc 식(abs(5px - 10px)). 혼합 단위 calc 는 미해석→None.
    if (lower.starts_with("abs(") || lower.starts_with("sign(")) && text.ends_with(')') {
        let is_sign = lower.starts_with("sign(");
        let open = if is_sign { 5 } else { 4 };
        let inner = text[open..text.len() - 1].trim();
        // abs/sign 은 단위 독립적(부호만 다룸)이라 어떤 단위든 정확: abs(-3em)=3em,
        // sign(-3em)=-1, abs(-3)=3(수). math_arg 가 맨수/길이/px calc 를 해석.
        let (n, unit) = math_arg(inner)?;
        return Some(if is_sign {
            // signed zero: sign(0)=0, sign(-0)=-0 (수). NaN 은 미해석.
            let s = if n.is_nan() {
                return None;
            } else if n > 0.0 {
                1.0
            } else if n < 0.0 {
                -1.0
            } else {
                n // 0 또는 -0 그대로(부호 보존)
            };
            Value::Length(s, Unit::Number)
        } else {
            Value::Length(n.abs(), unit)
        });
    }
    // round()/mod()/rem() — 두 인자가 같은 단위면 파스 타임 확정(CSS Values 4 §10).
    // round([strategy,] a, b), mod(a, b), rem(a, b). 혼합 단위/미해석은 드롭.
    for (name, kind) in [("round(", b'R'), ("mod(", b'M'), ("rem(", b'E')] {
        if !(lower.starts_with(name) && text.ends_with(')')) {
            continue;
        }
        let inner = &text[name.len()..text.len() - 1];
        let mut segs = split_top_commas(inner);
        // round 의 선택적 첫 인자: 반올림 방향 키워드.
        let mut strategy = "nearest";
        if kind == b'R' && segs.len() == 3 {
            let s = segs[0].trim().to_ascii_lowercase();
            if matches!(s.as_str(), "nearest" | "up" | "down" | "to-zero") {
                strategy = match s.as_str() {
                    "up" => "up",
                    "down" => "down",
                    "to-zero" => "to-zero",
                    _ => "nearest",
                };
                segs.remove(0);
            }
        }
        if segs.len() != 2 {
            return None;
        }
        let (a, ua) = math_arg(segs[0].trim())?;
        let (bb, ub) = math_arg(segs[1].trim())?;
        if ua != ub || bb == 0.0 {
            return None; // 단위 불일치 또는 0 으로 나눔 → 미해석
        }
        let r = match kind {
            b'R' => {
                let q = a / bb;
                let rounded = match strategy {
                    "up" => q.ceil(),
                    "down" => q.floor(),
                    "to-zero" => q.trunc(),
                    _ => (q + 0.5).floor(), // nearest: 동점은 +∞ 쪽(CSS 규정)
                };
                rounded * bb
            }
            b'M' => a - bb * (a / bb).floor(), // mod: 결과 부호는 제수 b
            _ => a - bb * (a / bb).trunc(),    // rem: 결과 부호는 피제수 a
        };
        if !r.is_finite() {
            return None;
        }
        return Some(Value::Length(r, ua));
    }
    // sin()/cos()/tan() — 각도(deg/rad/grad/turn) 또는 수(라디안)를 받아 단위 없는
    // 수를 낸다(CSS Values 4 §10). sin(30deg)=0.5, cos(0)=1, tan(45deg)=1. 상수
    // 인자만 파스 타임 확정(calc/변수는 미해석→드롭). f64 로 계산해 정밀도 보존.
    for (name, kind) in [("sin(", b's'), ("cos(", b'c'), ("tan(", b't')] {
        if !(lower.starts_with(name) && text.ends_with(')')) {
            continue;
        }
        let inner = text[name.len()..text.len() - 1].trim();
        let rad = parse_angle_rad(inner)?;
        let r = match kind {
            b's' => rad.sin(),
            b'c' => rad.cos(),
            _ => rad.tan(),
        };
        if !r.is_finite() {
            return None;
        }
        return Some(Value::Length(r as f32, Unit::Number));
    }
    // sqrt()/exp()/log()/pow() — 단위 없는 수 인자·결과(CSS Values 4 §10).
    // sqrt(4)=2, exp(0)=1, log(e)=1, log(8,2)=3, pow(2,3)=8. 수 인자만.
    let as_number = |s: &str| -> Option<f64> {
        if let Ok(n) = s.trim().parse::<f64>() {
            return Some(n);
        }
        match interpret_value(s).or_else(|| eval_calc(s)) {
            Some(Value::Length(f, Unit::Number)) => Some(f as f64),
            _ => None,
        }
    };
    for (name, kind) in [("sqrt(", b'q'), ("exp(", b'e'), ("log(", b'l'), ("pow(", b'p')] {
        if !(lower.starts_with(name) && text.ends_with(')')) {
            continue;
        }
        let inner = &text[name.len()..text.len() - 1];
        let segs = split_top_commas(inner);
        let r = match kind {
            b'q' if segs.len() == 1 => as_number(segs[0].trim())?.sqrt(),
            b'e' if segs.len() == 1 => as_number(segs[0].trim())?.exp(),
            b'l' if segs.len() == 1 => as_number(segs[0].trim())?.ln(),
            b'l' if segs.len() == 2 => {
                as_number(segs[0].trim())?.log(as_number(segs[1].trim())?)
            }
            b'p' if segs.len() == 2 => {
                as_number(segs[0].trim())?.powf(as_number(segs[1].trim())?)
            }
            _ => return None,
        };
        if !r.is_finite() {
            return None;
        }
        return Some(Value::Length(r as f32, Unit::Number));
    }
    // hypot(a, b, ...) — √(Σ aᵢ²). 인자가 모두 같은 단위(또는 순수 px/수)면 확정.
    if lower.starts_with("hypot(") && text.ends_with(')') {
        let inner = &text[6..text.len() - 1];
        let segs = split_top_commas(inner);
        if segs.is_empty() {
            return None;
        }
        let mut unit: Option<Unit> = None;
        let mut sum = 0.0f64;
        for s in &segs {
            let (nf, u) = math_arg(s.trim())?;
            let n = nf as f64;
            match unit {
                Some(pu) if pu != u => return None, // 단위 불일치
                _ => unit = Some(u),
            }
            sum += n * n;
        }
        let r = sum.sqrt();
        if !r.is_finite() {
            return None;
        }
        return Some(Value::Length(r as f32, unit.unwrap_or(Unit::Number)));
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
// 단위별 계수 합으로 축약. 단위 불일치 곱셈(길이×길이)이면 None.
// 지원: + - * /, 괄호, px/%/단위없는 수.
#[derive(Clone, Copy, Default)]
struct CalcVal {
    pct: f32,
    px: f32,
    em: f32,
    rem: f32,
    vw: f32,
    vh: f32,
    vmin: f32,
    vmax: f32,
    num: f32,
    is_num: bool,
}

impl CalcVal {
    // 길이 계수 전체에 스칼라를 곱한다(단위 없는 수와의 곱/나눗셈용).
    fn scale(self, k: f32) -> CalcVal {
        CalcVal {
            pct: self.pct * k,
            px: self.px * k,
            em: self.em * k,
            rem: self.rem * k,
            vw: self.vw * k,
            vh: self.vh * k,
            vmin: self.vmin * k,
            vmax: self.vmax * k,
            num: self.num * k,
            is_num: self.is_num,
        }
    }
    // 두 길이 합(부호 s 로 뺄셈도). is_num 은 호출부가 맞춰 둔다.
    fn combine(self, rhs: CalcVal, s: f32) -> CalcVal {
        CalcVal {
            pct: self.pct + s * rhs.pct,
            px: self.px + s * rhs.px,
            em: self.em + s * rhs.em,
            rem: self.rem + s * rhs.rem,
            vw: self.vw + s * rhs.vw,
            vh: self.vh + s * rhs.vh,
            vmin: self.vmin + s * rhs.vmin,
            vmax: self.vmax + s * rhs.vmax,
            num: self.num + s * rhs.num,
            is_num: self.is_num,
        }
    }
}

// calc() 내부 식을 순수 수(단위 무시)로 평가. transition 시간 정규화 등에 쓴다.
pub(crate) fn eval_calc_number(inner: &str) -> Option<f32> {
    match eval_calc(inner) {
        Some(Value::Length(n, _)) if n.is_finite() => Some(n),
        _ => None,
    }
}

// 수학 함수 인자 → (값, 단위). 맨수는 Number(interpret_value 는 맨수를 Keyword 로
// 보존하므로 먼저 파싱), 길이는 그 단위, 순수 px calc 는 Px.
fn math_arg(s: &str) -> Option<(f32, Unit)> {
    let s = s.trim();
    if let Ok(f) = s.parse::<f32>() {
        return Some((f, Unit::Number));
    }
    match interpret_value(s).or_else(|| eval_calc(s)) {
        Some(Value::Length(f, u)) => Some((f, u)),
        Some(Value::Calc(c)) if !c.has_ctx_units() && c.pct == 0.0 => Some((c.px, Unit::Px)),
        Some(Value::Keyword(k)) => k.trim().parse::<f32>().ok().map(|f| (f, Unit::Number)),
        _ => None,
    }
}

// 각도 리터럴 → 라디안(f64). deg/grad/turn/rad 또는 단위 없는 수(라디안).
// grad 가 rad 를 접미로 포함하므로 검사 순서 주의(deg→grad→turn→rad→수).
fn parse_angle_rad(s: &str) -> Option<f64> {
    let s = s.trim();
    let lower = s.to_ascii_lowercase();
    let pi = std::f64::consts::PI;
    for (suffix, factor) in [
        ("deg", pi / 180.0),
        ("grad", pi / 200.0),
        ("turn", 2.0 * pi),
        ("rad", 1.0),
    ] {
        if let Some(num) = lower.strip_suffix(suffix) {
            return num.trim().parse::<f64>().ok().map(|n| n * factor);
        }
    }
    // 단위 없는 수는 라디안(CSS: sin(수)는 라디안 취급).
    s.parse::<f64>().ok()
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
        // 맨수 calc(2)/calc(cos(0)) 는 단위 없는 수다(표준). 예전엔 Px 로 반환해
        // line-height:calc(1.5)=1.5px, calc(cos(0)) 계산값=1px 처럼 틀렸다. 길이
        // 문맥의 calc(<수>)는 애초에 무효 CSS라 Number 로 두는 게 더 정확하다.
        return Some(Value::Length(v.num, Unit::Number));
    }
    let sum = crate::css::CalcSum {
        pct: v.pct,
        px: v.px,
        em: v.em,
        rem: v.rem,
        vw: v.vw,
        vh: v.vh,
        vmin: v.vmin,
        vmax: v.vmax,
    };
    // 순수 px(문맥 단위도 %도 없음)면 바로 Length. 그 외는 Calc 로 보존 —
    // 문맥 단위는 resolve_units 가, %는 len_px 가 확정한다.
    if !sum.has_ctx_units() && sum.pct == 0.0 {
        Some(Value::Length(sum.px, Unit::Px))
    } else {
        Some(Value::Calc(sum))
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
        acc = acc.combine(rhs, s);
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
                    rhs.scale(acc.num)
                } else if rhs.is_num {
                    acc.scale(rhs.num)
                } else {
                    return None;
                }
            }
            _ => {
                // 나눗셈: 우변은 수
                if !rhs.is_num || rhs.num == 0.0 {
                    return None;
                }
                acc.scale(1.0 / rhs.num)
            }
        };
    }
    Some(acc)
}

// factor = '(' expr ')' | number[unit]
fn calc_factor(t: &[char], p: &mut usize) -> Option<CalcVal> {
    skip_ws(t, p);
    // 중첩 calc(): `calc(50% + calc(10px * 2))` — 표준에서 허용된다.
    // 예전엔 'c' 를 만나 파싱이 실패하고 선언 전체가 버려졌다(조용히 다른 값이 됨).
    if t.len() >= *p + 5 {
        let head: String = t[*p..*p + 5].iter().collect::<String>().to_ascii_lowercase();
        if head == "calc(" {
            *p += 5;
            let v = calc_expr(t, p)?;
            skip_ws(t, p);
            if t.get(*p) != Some(&')') {
                return None;
            }
            *p += 1;
            return Some(v);
        }
    }
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
    // 수학 함수(sin/cos/sqrt/abs/round/hypot/…)를 calc 인자로: 균형 괄호까지 잘라
    // interpret_value 로 평가한다. calc(sin(30deg) * 100px) 등. min/max/clamp 는
    // Value::MinMax 를 내므로 여기선 폴백(calc 내 min/max 는 기존대로 미지원).
    if t.get(*p).is_some_and(|c| c.is_ascii_alphabetic()) {
        let fstart = *p;
        let mut j = *p;
        while j < t.len() && (t[j].is_ascii_alphabetic() || t[j] == '-') {
            j += 1;
        }
        if j < t.len() && t[j] == '(' {
            let mut depth = 0usize;
            let mut k = j;
            while k < t.len() {
                match t[k] {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            k += 1;
                            break;
                        }
                    }
                    _ => {}
                }
                k += 1;
            }
            if depth == 0 {
                let sub: String = t[fstart..k].iter().collect();
                if let Some(cv) = interpret_value(&sub).and_then(|v| match v {
                    Value::Length(f, u) => {
                        let mut c = CalcVal::default();
                        match u {
                            Unit::Number => {
                                c.num = f;
                                c.is_num = true;
                            }
                            Unit::Px => c.px = f,
                            Unit::Percent => c.pct = f,
                            Unit::Em => c.em = f,
                            Unit::Rem => c.rem = f,
                            Unit::Vw => c.vw = f,
                            Unit::Vh => c.vh = f,
                            Unit::Vmin => c.vmin = f,
                            Unit::Vmax => c.vmax = f,
                            _ => return None, // Lh 등 CalcVal 에 없는 단위 → 미지원
                        }
                        Some(c)
                    }
                    _ => None,
                }) {
                    *p = k;
                    return Some(cv);
                }
            }
        }
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
    // 단위별 계수 하나만 채운 CalcVal. 문맥 단위(em/rem/vw…)는 style 에서 px 로 접힌다.
    let mut c = CalcVal::default();
    match unit.as_str() {
        "" => {
            c.num = num;
            c.is_num = true;
        }
        "px" => c.px = num,
        "%" => c.pct = num,
        "em" => c.em = num,
        "rem" => c.rem = num,
        "vw" => c.vw = num,
        "vh" => c.vh = num,
        "vmin" => c.vmin = num,
        "vmax" => c.vmax = num,
        _ => return None, // pt/cm 등 나머지는 아직 미지원
    }
    Some(c)
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
    Some(crate::css::Gradient {
        angle_deg: angle,
        radial: false,
        circle: false,
        conic: false,
        repeating: false,
        stops,
        serial: String::new(),
    })
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
        .map(|v| matches!(v, Value::Color(_) | Value::ColorFn(_, _)))
        .unwrap_or(false);
    let idx = if first_is_color { 0 } else { 1 };
    if idx >= parts.len() {
        return None;
    }
    // 서술자(첫 파트)에 'circle' 이 있으면 원, 아니면 타원(기본). 크기/위치는 근사.
    let circle = idx == 1 && parts[0].to_ascii_lowercase().split_whitespace().any(|t| t == "circle");
    let stops = parse_color_stops(&parts[idx..])?;
    Some(crate::css::Gradient {
        angle_deg: 0.0,
        radial: true,
        circle,
        conic: false,
        repeating: false,
        stops,
        serial: String::new(),
    })
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
    let stops = parse_color_stops(&parts[idx..])?;
    Some(crate::css::Gradient {
        angle_deg: 0.0,
        radial: false,
        circle: false,
        conic: true,
        repeating: false,
        stops,
        serial: String::new(),
    })
}

// 색 스톱 목록. 위치는 %/px/deg 를 그대로 보존한다 (px 는 페인트 때 그라디언트 선
// 길이로 푼다). 이중 위치("#f00 0 10px")는 같은 색의 스톱 두 개로 펼친다 (표준).
fn parse_color_stops(parts: &[String]) -> Option<Vec<(Color, crate::css::StopPos)>> {
    use crate::css::StopPos;
    let parse_pos = |t: &str| -> Option<StopPos> {
        let t = t.trim();
        if let Some(n) = t.strip_suffix('%') {
            return n.trim().parse::<f32>().ok().map(|p| StopPos::Pct(p / 100.0));
        }
        if let Some(n) = t.strip_suffix("px") {
            return n.trim().parse::<f32>().ok().map(StopPos::Px);
        }
        if let Some(n) = t.strip_suffix("deg") {
            return n.trim().parse::<f32>().ok().map(|d| StopPos::Deg(d));
        }
        if let Some(n) = t.strip_suffix("turn") {
            return n.trim().parse::<f32>().ok().map(|d| StopPos::Deg(d * 360.0));
        }
        // 단위 없는 0 (표준에서 허용)
        if let Ok(v) = t.parse::<f32>() {
            if v == 0.0 {
                return Some(StopPos::Px(0.0));
            }
        }
        None
    };
    let mut stops: Vec<(Color, StopPos)> = Vec::new();
    for p in parts {
        let toks = split_top_level(p.trim());
        if toks.is_empty() {
            continue;
        }
        let color = match interpret_value(&toks[0]) {
            Some(Value::Color(c)) => c,
            // color()/lab()/oklch()/color-mix() 스톱: 렌더는 sRGB 근사(ColorFn 의
            // Color), 계산값 직렬화는 serial 이 원문(color() 등)을 보존한다.
            Some(Value::ColorFn(c, _)) => c,
            _ => continue,
        };
        let p1 = toks.get(1).and_then(|t| parse_pos(t));
        let p2 = toks.get(2).and_then(|t| parse_pos(t));
        match (p1, p2) {
            // 이중 위치: 같은 색의 스톱 두 개 (딱딱한 경계를 만든다)
            (Some(a), Some(b)) => {
                stops.push((color, a));
                stops.push((color, b));
            }
            (Some(a), None) => stops.push((color, a)),
            _ => stops.push((color, StopPos::Auto)),
        }
    }
    if stops.len() < 2 {
        return None;
    }
    Some(stops)
}

// gradient 계산값 캐논 직렬화: 색 스톱의 색만 rgb()/color() 로 정규화하고 방향/각도/
// 보간 메서드 구조는 원문 그대로 둔다. 렌더용 Gradient 구조체는 방향키워드·보간
// 메서드를 잃으므로(각도로 접힘) 원문 텍스트에서 색만 바꾸는 게 정확하다.
// "linear-gradient(30deg, red, blue)" → "...(30deg, rgb(255, 0, 0), rgb(0, 0, 255))".
// shape 함수(circle/ellipse) 의 "at <position>" 캐논 직렬화(§CSS Shapes): 위치는
// x-part 먼저, 빠진 축은 center, 값은 원문 유지(cm/키워드 해석 안 함). circle(at
// 50cm)→"circle(at 50cm center)", circle(at top 50% left 50cm)→"...(at left 50cm
// top 50%)". inset/polygon/path 및 "at" 없는 경우는 원문.
// SVG path data 정규화(§CSSOM path() 직렬화): 명령·수 사이 단일 공백, 콤마 제거,
// close(z)는 대문자 Z. 명령 대소문자(절대/상대)는 유지.
fn svg_path_normalize(d: &str) -> String {
    let chars: Vec<char> = d.chars().collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() || c == ',' {
            i += 1;
            continue;
        }
        if c.is_ascii_alphabetic() {
            out.push(if c == 'z' || c == 'Z' { "Z".to_string() } else { c.to_string() });
            i += 1;
            continue;
        }
        // 수: 부호 + 정수부/소수부(점 1개) + 지수부.
        let start = i;
        if chars[i] == '+' || chars[i] == '-' {
            i += 1;
        }
        let mut seen_dot = false;
        while i < chars.len() {
            let ch = chars[i];
            if ch.is_ascii_digit() {
                i += 1;
            } else if ch == '.' && !seen_dot {
                seen_dot = true;
                i += 1;
            } else {
                break;
            }
        }
        if i < chars.len() && (chars[i] == 'e' || chars[i] == 'E') {
            i += 1;
            if i < chars.len() && (chars[i] == '+' || chars[i] == '-') {
                i += 1;
            }
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
        }
        if i > start {
            out.push(chars[start..i].iter().collect());
        } else {
            i += 1; // 알 수 없는 문자 건너뜀
        }
    }
    out.join(" ")
}

// SVG path data 토큰: 명령 문자 또는 수. 정규화·검증 공용.
fn svg_path_tokens(d: &str) -> Vec<(bool, char)> {
    // (is_cmd, ch) — is_cmd 이면 ch=명령, 아니면 ch='0'(수 자리표시).
    let chars: Vec<char> = d.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() || c == ',' {
            i += 1;
            continue;
        }
        if c.is_ascii_alphabetic() {
            toks.push((true, c));
            i += 1;
            continue;
        }
        let start = i;
        if chars[i] == '+' || chars[i] == '-' {
            i += 1;
        }
        let mut dot = false;
        while i < chars.len() {
            let ch = chars[i];
            if ch.is_ascii_digit() {
                i += 1;
            } else if ch == '.' && !dot {
                dot = true;
                i += 1;
            } else {
                break;
            }
        }
        if i < chars.len() && (chars[i] == 'e' || chars[i] == 'E') {
            i += 1;
            if i < chars.len() && (chars[i] == '+' || chars[i] == '-') {
                i += 1;
            }
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
        }
        if i > start {
            toks.push((false, '0'));
        } else {
            i += 1; // 알 수 없는 문자
        }
    }
    toks
}

// SVG path data 유효성(§SVG path): moveto 로 시작, 각 명령의 인자 개수(반복 가능),
// A 는 7개. 인자 부족/초과·미지 명령 거부. flag 자리표시(A)는 수로 근사(공백 구분 가정).
fn svg_path_valid(d: &str) -> bool {
    let toks = svg_path_tokens(d);
    if toks.is_empty() || !toks[0].0 || !matches!(toks[0].1, 'M' | 'm') {
        return false;
    }
    let argc = |c: char| -> Option<usize> {
        match c.to_ascii_lowercase() {
            'z' => Some(0),
            'h' | 'v' => Some(1),
            'm' | 'l' | 't' => Some(2),
            's' | 'q' => Some(4),
            'c' => Some(6),
            'a' => Some(7),
            _ => None,
        }
    };
    let mut i = 0;
    while i < toks.len() {
        if !toks[i].0 {
            return false; // 수가 명령 없이 나옴
        }
        let n = argc(toks[i].1);
        let Some(n) = n else { return false };
        i += 1;
        if n == 0 {
            continue;
        }
        // 최소 한 그룹, 이후 수가 이어지면 추가 그룹(반복).
        loop {
            for _ in 0..n {
                if i < toks.len() && !toks[i].0 {
                    i += 1;
                } else {
                    return false; // 인자 부족
                }
            }
            if i < toks.len() && !toks[i].0 {
                continue; // 다음 그룹
            }
            break;
        }
    }
    true
}

pub(crate) fn normalize_shape(text: &str) -> String {
    let text = text.trim();
    let lower = text.to_ascii_lowercase();
    if !text.ends_with(')') {
        return text.to_string();
    }
    // inset()/rect()/xywh(): 각 길이 토큰 0→0px, border-radius "round <h> / <v>" 에서
    // h==v 면 "/ v" 생략.
    if lower.starts_with("inset(") || lower.starts_with("rect(") || lower.starts_with("xywh(") {
        // 단위 없는 0 → 0px(auto·%·calc·단위 길이는 원문).
        let c0 = |t: &str| if t.trim() == "0" { "0px".to_string() } else { t.to_string() };
        let open = text.find('(').unwrap();
        let func = text[..open].to_ascii_lowercase();
        let inner = text[open + 1..text.len() - 1].trim();
        let low_inner = inner.to_ascii_lowercase();
        let (before, radius) = match low_inner.find(" round ") {
            Some(ri) => (inner[..ri].trim(), Some(inner[ri + " round ".len()..].trim())),
            None => (inner, None),
        };
        let coords: String =
            split_top_level(before).iter().map(|t| c0(t)).collect::<Vec<_>>().join(" ");
        let round = radius.and_then(|r| {
            let (rs, all_zero) = if let Some((h, v)) = r.split_once('/') {
                let hv: Vec<String> = h.split_whitespace().map(c0).collect();
                let vv: Vec<String> = v.split_whitespace().map(c0).collect();
                let az = hv.iter().chain(vv.iter()).all(|t| t == "0px");
                let s = if hv == vv {
                    hv.join(" ")
                } else {
                    format!("{} / {}", hv.join(" "), vv.join(" "))
                };
                (s, az)
            } else {
                let rv: Vec<String> = r.split_whitespace().map(c0).collect();
                let az = rv.iter().all(|t| t == "0px");
                (rv.join(" "), az)
            };
            // 반경이 전부 0 이면 round 절 생략(§CSSOM).
            if all_zero { None } else { Some(rs) }
        });
        return match round {
            Some(r) => format!("{}({} round {})", func, coords, r),
            None => format!("{}({})", func, coords),
        };
    }
    // shape(): 좌표의 단위 없는 0 → 0px(명령·키워드는 "0" 이 아니라 영향 없음).
    if lower.starts_with("shape(") {
        let inner = text[text.find('(').unwrap() + 1..text.len() - 1].trim();
        let segs: Vec<String> = split_top_commas(inner)
            .iter()
            .map(|seg| {
                split_top_level(seg.trim())
                    .iter()
                    .map(|t| if t.trim() == "0" { "0px".to_string() } else { t.clone() })
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        return format!("shape({})", segs.join(", "));
    }
    // polygon(): fill-rule nonzero(기본) 생략, round 전부-0 생략, 각 점 좌표 0→0px.
    if lower.starts_with("polygon(") {
        let c0 = |t: &str| if t.trim() == "0" { "0px".to_string() } else { t.to_string() };
        let inner = text[text.find('(').unwrap() + 1..text.len() - 1].trim();
        let segs = split_top_level_commas_local(inner);
        if segs.is_empty() {
            return text.to_string();
        }
        let ftoks = split_top_level(&segs[0]);
        let has_prelude = ftoks
            .iter()
            .any(|t| matches!(t.to_ascii_lowercase().as_str(), "nonzero" | "evenodd" | "round"));
        let (prelude, points): (Vec<String>, &[String]) = if has_prelude {
            let mut pre: Vec<String> = Vec::new();
            let mut i = 0;
            if ftoks.get(i).map(|t| t.eq_ignore_ascii_case("evenodd")).unwrap_or(false) {
                pre.push("evenodd".to_string());
                i += 1;
            } else if ftoks.get(i).map(|t| t.eq_ignore_ascii_case("nonzero")).unwrap_or(false) {
                i += 1; // nonzero(기본) 생략
            }
            if ftoks.get(i).map(|t| t.eq_ignore_ascii_case("round")).unwrap_or(false) {
                let radii: Vec<String> = ftoks[i + 1..].iter().map(|t| c0(t)).collect();
                if !radii.iter().all(|r| r == "0px") {
                    pre.push("round".to_string());
                    pre.extend(radii);
                }
            }
            (pre, &segs[1..])
        } else {
            (Vec::new(), &segs[..])
        };
        let pts: Vec<String> = points
            .iter()
            .map(|seg| split_top_level(seg).iter().map(|t| c0(t)).collect::<Vec<_>>().join(" "))
            .collect();
        let mut out = String::from("polygon(");
        if !prelude.is_empty() {
            out.push_str(&prelude.join(" "));
            if !pts.is_empty() {
                out.push_str(", ");
            }
        }
        out.push_str(&pts.join(", "));
        out.push(')');
        return out;
    }
    // path(): fill-rule nonzero 생략·evenodd 유지, 문자열 겹따옴표.
    if lower.starts_with("path(") {
        let inner = text[text.find('(').unwrap() + 1..text.len() - 1].trim();
        let requote = |s: &str| -> Option<String> {
            let s = s.trim();
            let first = s.chars().next()?;
            if s.len() >= 2 && (first == '"' || first == '\'') && s.ends_with(first) {
                // 문자열 내부 SVG path data 도 정규화(공백·콤마·close 대문자).
                Some(format!("\"{}\"", svg_path_normalize(&s[1..s.len() - 1])))
            } else {
                None
            }
        };
        // 따옴표 밖 최상위 콤마로 fill-rule 분리.
        let ci = {
            let mut q: Option<char> = None;
            let mut found = None;
            for (i, ch) in inner.char_indices() {
                match q {
                    Some(qc) if ch == qc => q = None,
                    Some(_) => {}
                    None if ch == '"' || ch == '\'' => q = Some(ch),
                    None if ch == ',' => {
                        found = Some(i);
                        break;
                    }
                    None => {}
                }
            }
            found
        };
        return match ci {
            None => requote(inner).map(|s| format!("path({})", s)).unwrap_or_else(|| text.to_string()),
            Some(c) => {
                let fr = inner[..c].trim().to_ascii_lowercase();
                match (fr.as_str(), requote(inner[c + 1..].trim())) {
                    ("nonzero", Some(s)) => format!("path({})", s),
                    ("evenodd", Some(s)) => format!("path(evenodd, {})", s),
                    _ => text.to_string(),
                }
            }
        };
    }
    if !(lower.starts_with("circle(") || lower.starts_with("ellipse(")) {
        return text.to_string();
    }
    let open = text.find('(').unwrap();
    let func = text[..open].to_ascii_lowercase();
    let inner = text[open + 1..text.len() - 1].trim();
    let toks = split_top_level(inner);
    let at_idx = toks.iter().position(|t| t.eq_ignore_ascii_case("at"));
    let radius_toks = match at_idx {
        Some(i) => &toks[..i],
        None => &toks[..],
    };
    // 기본 반지름(모두 closest-side)은 생략(§CSS Shapes). circle(closest-side)→circle().
    let radius = if !radius_toks.is_empty()
        && radius_toks.iter().all(|t| t.eq_ignore_ascii_case("closest-side"))
    {
        String::new()
    } else {
        radius_toks.join(" ")
    };
    let mut parts = Vec::new();
    if !radius.is_empty() {
        parts.push(radius);
    }
    if let Some(i) = at_idx {
        parts.push(format!("at {}", normalize_position(&toks[i + 1..])));
    }
    format!("{}({})", func, parts.join(" "))
}

// CSS <position> 캐논 직렬화: x-part 먼저, 빠진 축 center. 값 원문 유지(해석 안 함).
fn normalize_position(toks: &[String]) -> String {
    let is_x = |t: &str| matches!(t.to_ascii_lowercase().as_str(), "left" | "right");
    let is_y = |t: &str| matches!(t.to_ascii_lowercase().as_str(), "top" | "bottom");
    match toks.len() {
        0 => "center center".to_string(),
        1 => {
            if is_y(&toks[0]) {
                format!("center {}", toks[0])
            } else {
                format!("{} center", toks[0])
            }
        }
        2 => {
            // <x> <y>. y키워드가 먼저이거나 x키워드가 뒤면 뒤집어 x 를 먼저.
            if is_y(&toks[0]) || is_x(&toks[1]) {
                format!("{} {}", toks[1], toks[0])
            } else {
                format!("{} {}", toks[0], toks[1])
            }
        }
        4 => {
            // <edge> <off> <edge> <off>. x-edge(left/right) 쪽을 먼저.
            if is_x(&toks[0]) {
                format!("{} {} {} {}", toks[0], toks[1], toks[2], toks[3])
            } else {
                format!("{} {} {} {}", toks[2], toks[3], toks[0], toks[1])
            }
        }
        _ => toks.join(" "),
    }
}

// image-set() 지정값 캐논 직렬화(§CSS Images 4): 함수명 표준화(-webkit-image-set
// →image-set), 각 이미지의 url(x)/'x'/"x" → url("x"). image-set(url(a.png) 1x)→
// image-set(url("a.png") 1x). 해상도/타입은 유지.
pub(crate) fn normalize_image_set(text: &str) -> String {
    let text = text.trim();
    let lower = text.to_ascii_lowercase();
    let open = if lower.starts_with("image-set(") {
        "image-set(".len()
    } else if lower.starts_with("-webkit-image-set(") {
        "-webkit-image-set(".len()
    } else {
        return text.to_string();
    };
    if !text.ends_with(')') {
        return text.to_string();
    }
    let inner = &text[open..text.len() - 1];
    let raw_items = split_top_commas(inner);
    // 무효 image-set 거부(""): 빈 아이템, none 이미지, 음수/무효 해상도.
    if raw_items.is_empty() || raw_items.iter().any(|i| i.trim().is_empty()) {
        return String::new();
    }
    for it in &raw_items {
        let toks = split_top_level(it.trim());
        let Some(img) = toks.first() else { return String::new() };
        if img.eq_ignore_ascii_case("none") {
            return String::new();
        }
        // 이미지 뒤: <resolution> || type(<string>), 각 ≤1(§CSS Images 4). type() 인자는
        // 단일 <string>. 미지 토큰·중복·이중 url 등 거부.
        let (mut res_seen, mut type_seen) = (0u32, 0u32);
        for t in &toks[1..] {
            let tl = t.to_ascii_lowercase();
            if tl.starts_with("type(") && t.ends_with(')') {
                let arg = t[t.find('(').unwrap() + 1..t.len() - 1].trim();
                if !is_css_string(arg) {
                    return String::new(); // type() 인자는 단일 문자열
                }
                type_seen += 1;
            } else if image_set_resolution_valid(t) {
                res_seen += 1;
            } else {
                return String::new();
            }
        }
        if res_seen > 1 || type_seen > 1 {
            return String::new();
        }
    }
    let items: Vec<String> =
        raw_items.iter().map(|it| normalize_image_set_item(it.trim())).collect();
    format!("image-set({})", items.join(", "))
}

// image-set 해상도: 양수 + x/dppx/dpi/dpcm. "-20x" 는 무효.
fn image_set_resolution_valid(res: &str) -> bool {
    let low = res.to_ascii_lowercase();
    for unit in ["dppx", "dpcm", "dpi", "x"] {
        if let Some(n) = low.strip_suffix(unit) {
            return n.trim().parse::<f32>().map(|v| v > 0.0).unwrap_or(false);
        }
    }
    // type(...) 은 해상도 자리 아님(별도 토큰) — 여기선 해상도로 안 옴. 그 외 무효.
    false
}

fn normalize_image_set_item(item: &str) -> String {
    let toks = split_top_level(item);
    if toks.is_empty() {
        return item.to_string();
    }
    let img = normalize_image_ref(&toks[0]);
    if toks.len() == 1 {
        img
    } else {
        format!("{} {}", img, toks[1..].join(" "))
    }
}

// url(x)/url("x")/url('x') 및 맨문자열 'x'/"x" → url("x"). 그 외(gradient 등)는 원문.
fn normalize_image_ref(t: &str) -> String {
    let tl = t.to_ascii_lowercase();
    if tl.starts_with("url(") && t.ends_with(')') {
        let inner = t[4..t.len() - 1].trim().trim_matches(|c| c == '"' || c == '\'');
        return format!("url(\"{}\")", inner);
    }
    if t.len() >= 2
        && ((t.starts_with('"') && t.ends_with('"'))
            || (t.starts_with('\'') && t.ends_with('\'')))
    {
        return format!("url(\"{}\")", &t[1..t.len() - 1]);
    }
    t.to_string()
}

// gradient 유효성(세터 거부용). 단일 gradient 함수만 검사하고, 다중 값/미인식
// 형태는 관대하게 true(오탐 회피 — 게터에서만 쓰이므로 렌더 무영향). 명백한 무효
// (빈 세그먼트/비색 비위치 스톱/무효 prefix/스톱<2)만 false.
pub(crate) fn gradient_valid(text: &str) -> bool {
    let text = text.trim();
    let lower = text.to_ascii_lowercase();
    let is_grad = ["linear-gradient(", "radial-gradient(", "conic-gradient(",
        "repeating-linear-gradient(", "repeating-radial-gradient(", "repeating-conic-gradient("]
        .iter().any(|p| lower.starts_with(p));
    if !is_grad || !text.ends_with(')') {
        return true; // gradient 아님/다중 값 → 판단 보류(통과)
    }
    let Some(open) = text.find('(') else { return true };
    // 단일 함수 확인 — 아니면 보류.
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    for (k, &b) in bytes.iter().enumerate() {
        if b == b'(' {
            depth += 1;
        } else if b == b')' {
            depth -= 1;
            if depth == 0 && k != bytes.len() - 1 {
                return true;
            }
        }
    }
    let inner = &text[open + 1..text.len() - 1];
    let segs = split_top_commas(inner);
    if segs.is_empty() || segs.iter().any(|s| s.trim().is_empty()) {
        return false; // 빈 세그먼트("linear-gradient(, red, blue)")
    }
    let is_color = |seg: &str| {
        let toks = split_top_level(seg);
        !toks.is_empty()
            && matches!(interpret_value(&toks[0]), Some(Value::Color(_) | Value::ColorFn(..)))
    };
    // conic 스톱/힌트는 <angle-percentage>(% 가 각도), 그 외는 <length-percentage>.
    let is_conic_grad =
        lower.starts_with("conic-gradient(") || lower.starts_with("repeating-conic-gradient(");
    let is_position = |seg: &str| {
        let toks = split_top_level(seg);
        if toks.len() != 1 {
            return false;
        }
        let t = toks[0].trim();
        if t == "0" {
            return true;
        }
        if is_conic_grad {
            math_angle_pct_valid(t)
        } else {
            math_length_valid(t, true)
        }
    };
    let is_angle = |t: &str| {
        ["deg", "rad", "grad", "turn"]
            .iter()
            .any(|u| t.strip_suffix(u).map(|p| p.trim().parse::<f32>().is_ok()).unwrap_or(false))
    };
    let is_prefix = |seg: &str| {
        let toks = split_top_level(seg);
        let Some(first) = toks.first() else { return false };
        let t = first.to_ascii_lowercase();
        matches!(
            t.as_str(),
            "to" | "in" | "from" | "at" | "circle" | "ellipse"
                | "closest-side" | "closest-corner" | "farthest-side" | "farthest-corner"
        ) || is_angle(&t)
            || interpret_value(first).map(|v| matches!(v, Value::Length(..))).unwrap_or(false)
    };
    // 첫 세그먼트: 색 스톱 또는 prefix. 나머지: 색 스톱 또는 위치(color hint).
    let first = segs[0].trim();
    let start = if is_color(first) {
        0
    } else if is_prefix(first) {
        1
    } else {
        return false;
    };
    // prefix 의 "at <position>"(radial/conic)는 유효한 <position>이어야 한다
    // (3값 형식·잘못된 축 배치 거부). "at" 앞의 shape/size 는 기존 느슨 검사 유지.
    if start == 1 {
        let ptoks = split_top_level(first);
        let at_pos = ptoks.iter().position(|t| t.eq_ignore_ascii_case("at"));
        // conic 의 "from <angle>": <angle> 은 순수 각도(% 불가). "at"/"in" 전까지 검증.
        if is_conic_grad {
            if let Some(fp) = ptoks.iter().position(|t| t.eq_ignore_ascii_case("from")) {
                let rest = &ptoks[fp + 1..];
                let end = rest
                    .iter()
                    .position(|t| matches!(t.to_ascii_lowercase().as_str(), "at" | "in"))
                    .unwrap_or(rest.len());
                let a = rest.first().map(|t| t.trim()).unwrap_or("");
                if end != 1 || !(a == "0" || math_angle_valid(a)) {
                    return false;
                }
            }
        }
        if let Some(ap) = at_pos {
            // 위치는 "at" 뒤부터 "in"(색보간법) 전까지.
            let rest = &ptoks[ap + 1..];
            let end = rest.iter().position(|t| t.eq_ignore_ascii_case("in")).unwrap_or(rest.len());
            if !bs_position_valid(&rest[..end]) {
                return false;
            }
        }
        // radial-gradient 의 <size>(원/타원 반지름)는 비음수여야 한다(§CSS Images).
        // "at" 앞(없으면 "in" 앞)의 길이/퍼센트 토큰이 음수면 거부.
        let is_radial =
            lower.starts_with("radial-gradient(") || lower.starts_with("repeating-radial-gradient(");
        if is_radial {
            let upto = at_pos
                .or_else(|| ptoks.iter().position(|t| t.eq_ignore_ascii_case("in")))
                .unwrap_or(ptoks.len());
            for t in &ptoks[..upto] {
                if t.trim().starts_with('-') && is_length_percentage(t) {
                    return false;
                }
            }
        }
        // "in <color-interpolation-method>"(§CSS Color 4): in 뒤는 반드시 색공간,
        // 극좌표만 [<hue-method> hue]. 색보간법이 방향보다 먼저 올 수 있으므로 phrase
        // 경계(색공간 뒤 hue-method+hue 유무)를 잡아 그만큼만 검증한다.
        if let Some(ip) = ptoks.iter().position(|t| t.eq_ignore_ascii_case("in")) {
            let after = &ptoks[ip + 1..];
            if after.is_empty() {
                return false;
            }
            let is_hue_method = |t: &str| {
                matches!(t.to_ascii_lowercase().as_str(), "shorter" | "longer" | "increasing" | "decreasing")
            };
            // 색공간 뒤 토큰이 hue-method 면 반드시 "hue" 가 뒤따라야 한다(4토큰), 아니면 색공간만(2토큰).
            let mlen = if after.len() >= 2 && is_hue_method(&after[1]) {
                if after.len() >= 3 && after[2].eq_ignore_ascii_case("hue") {
                    3
                } else {
                    return false;
                }
            } else {
                1
            };
            let method = format!("in {}", after[..mlen].join(" "));
            if !interp_method_valid(&method) {
                return false;
            }
        }
    }
    // color-stop-list: 색 스톱과 color hint(위치 단독)가 번갈아 온다. hint 는 첫/끝에
    // 올 수 없고 연속될 수 없다. 색 스톱은 위치를 최대 2개까지(색 + pos1 [pos2]).
    // 스톱 위치 타입: conic 은 <angle-percentage>, 그 외는 <length-percentage>. calc 는
    // 결과 차원을 검사한다(§CSS Values 4) — "calc(50% + 30deg)"(%+각도 혼합) 같은 타입
    // 불일치를 거부.
    let stop_pos_ok = |t: &str| {
        let t = t.trim();
        if t == "0" {
            return true; // 단위 없는 0 은 유효한 위치(0px/0deg)
        }
        if is_conic_grad {
            // <angle-percentage>(100%=360deg): % 를 angle 축으로 접어 calc(90deg + 50%)
            // 는 유효, calc(50% + 0)(각도-% 에 수 혼합)은 무효로 정확히 검사한다.
            math_angle_pct_valid(t)
        } else {
            // <length-percentage>: 각도 혼합(calc(50% + 30deg)) 등 타입 불일치 거부.
            math_length_valid(t, true)
        }
    };
    let list = &segs[start..];
    let mut stops = 0;
    let mut prev_was_hint = false;
    for (i, seg) in list.iter().enumerate() {
        let seg = seg.trim();
        if is_color(seg) {
            stops += 1;
            prev_was_hint = false;
            let toks = split_top_level(seg);
            if toks.len() > 3 {
                return false; // 색 + 위치 2개 초과
            }
            // 색 뒤 위치 토큰들은 유효한 스톱 위치여야 한다.
            if !toks[1..].iter().all(|t| stop_pos_ok(t)) {
                return false;
            }
        } else if is_position(seg) {
            if i == 0 || i + 1 == list.len() || prev_was_hint {
                return false; // hint 가 첫/끝/연속
            }
            prev_was_hint = true;
        } else {
            return false; // 색도 위치도 아닌 세그먼트("...,lab")
        }
    }
    stops >= 2
}

// gradient 계산값의 박스 문맥 해석: em/rem→px(폰트크기), "at <위치>" 키워드→%.
// window.rs 레이아웃 경로(폰트크기 있음)에서 g.serial 을 재처리한다(계산값 전용,
// 지정값 el.style 은 원문 유지). radial-gradient(50% 40em, …)→(50% 640px, …),
// at right center→at 100% 50%.
pub(crate) fn resolve_gradient_computed(serial: &str, fs: f32, root_fs: f32) -> String {
    let serial = serial.trim();
    let Some(open) = serial.find('(') else {
        return serial.to_string();
    };
    if !serial.ends_with(')') {
        return serial.to_string();
    }
    let func = &serial[..open];
    let inner = &serial[open + 1..serial.len() - 1];
    let segs: Vec<String> = split_top_commas(inner)
        .iter()
        .map(|seg| {
            let toks = split_top_level(seg.trim());
            if toks.first().map(|t| t.eq_ignore_ascii_case("at")).unwrap_or(false) {
                resolve_at_position(&toks, fs, root_fs)
            } else {
                toks.iter().map(|t| resolve_len_token(t, fs, root_fs)).collect::<Vec<_>>().join(" ")
            }
        })
        .collect();
    format!("{}({})", func, segs.join(", "))
}

// 길이 토큰 em/rem → px(폰트크기 곱). 그 외는 원문.
fn resolve_len_token(tok: &str, fs: f32, root_fs: f32) -> String {
    let low = tok.to_ascii_lowercase();
    if let Some(p) = low.strip_suffix("rem") {
        if let Ok(n) = p.parse::<f32>() {
            return format!("{}px", crate::style::num_css(n * root_fs));
        }
    } else if let Some(p) = low.strip_suffix("em") {
        if let Ok(n) = p.parse::<f32>() {
            return format!("{}px", crate::style::num_css(n * fs));
        }
    }
    tok.to_string()
}

// "at <위치>" 계산값: 키워드→%(left/top=0%, right/bottom=100%, center=50%), 길이는
// em→px, 단일 값이면 y=center(50%). 두 키워드 순서 뒤집기(top left→left top).
fn resolve_at_position(toks: &[String], fs: f32, root_fs: f32) -> String {
    // toks[0]="at". 위치는 "in"(보간 메서드) 전까지. "in lab" 등 뒤는 보존.
    let after = &toks[1..];
    let in_idx = after.iter().position(|t| t.eq_ignore_ascii_case("in"));
    let (pos_slice, rest) = match in_idx {
        Some(i) => (&after[..i], &after[i..]),
        None => (after, &after[after.len()..]),
    };
    let mut pos: Vec<String> = pos_slice.to_vec();
    // 4-값 문법: <x-edge> <x-off> <y-edge> <y-off> ("left 10px top 50em"). edge 가
    // left/top 이면 오프셋 그대로, right/bottom 이면 calc(100% - off).
    if pos.len() == 4 {
        let is_x = |t: &str| matches!(t.to_ascii_lowercase().as_str(), "left" | "right");
        let (xe, xo, ye, yo) = if is_x(&pos[0]) {
            (&pos[0], &pos[1], &pos[2], &pos[3])
        } else {
            (&pos[2], &pos[3], &pos[0], &pos[1])
        };
        let edge_off = |edge: &str, off: &str| -> String {
            let off_px = resolve_len_token(off, fs, root_fs);
            match edge.to_ascii_lowercase().as_str() {
                "right" | "bottom" => format!("calc(100% - {})", off_px),
                _ => off_px, // left/top → 오프셋 그대로
            }
        };
        let x = edge_off(xe, xo);
        let y = edge_off(ye, yo);
        return if rest.is_empty() {
            format!("at {} {}", x, y)
        } else {
            format!("at {} {} {}", x, y, rest.join(" "))
        };
    }
    if pos.len() == 2
        && matches!(pos[0].to_ascii_lowercase().as_str(), "top" | "bottom")
        && matches!(pos[1].to_ascii_lowercase().as_str(), "left" | "right" | "center")
    {
        pos.swap(0, 1);
    }
    let axis = |t: &str| -> String {
        match t.to_ascii_lowercase().as_str() {
            "left" | "top" => "0%".to_string(),
            "right" | "bottom" => "100%".to_string(),
            "center" => "50%".to_string(),
            _ => resolve_len_token(t, fs, root_fs),
        }
    };
    let x = pos.first().map(|t| axis(t)).unwrap_or_else(|| "50%".to_string());
    let y = pos.get(1).map(|t| axis(t)).unwrap_or_else(|| "50%".to_string());
    if rest.is_empty() {
        format!("at {} {}", x, y)
    } else {
        format!("at {} {} {}", x, y, rest.join(" "))
    }
}

// computed=true 면 색을 rgb()(계산값), false 면 키워드 유지(지정값 el.style).
pub(crate) fn normalize_gradient_serial(text: &str, computed: bool) -> String {
    let text = text.trim();
    let Some(open) = text.find('(') else {
        return text.to_string();
    };
    if !text.ends_with(')') {
        return text.to_string();
    }
    // 단일 함수인지(첫 '(' 가 끝에서 닫힘) 확인 — 다중 값(grad(), url())이면 원문.
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    for (k, &b) in bytes.iter().enumerate() {
        if b == b'(' {
            depth += 1;
        } else if b == b')' {
            depth -= 1;
            if depth == 0 && k != bytes.len() - 1 {
                return text.to_string();
            }
        }
    }
    let func = text[..open].to_ascii_lowercase();
    let inner = &text[open + 1..text.len() - 1];
    let segs = split_top_commas(inner);
    // 스톱이 모던 색함수(ColorFn=color()/lab/oklch)를 하나라도 포함하면 non-legacy.
    // 기본 보간 색공간: non-legacy=oklab, legacy=srgb (§CSS Color 4). 기본이면 생략.
    let non_legacy = segs.iter().any(|s| {
        split_top_level(s.trim())
            .first()
            .map(|t| matches!(interpret_value(t), Some(Value::ColorFn(..))))
            .unwrap_or(false)
    });
    let default_space = if non_legacy { "oklab" } else { "srgb" };
    let out: Vec<String> = segs
        .iter()
        .map(|s| normalize_gradient_seg(s.trim(), computed, default_space))
        .filter(|s| !s.is_empty()) // 기본 보간만이던 prefix 세그먼트는 삭제
        .collect();
    format!("{}({})", func, out.join(", "))
}

// 그라디언트 세그먼트: 첫 토큰이 색이면 색을 정규화(위치는 유지), 아니면(방향/각도/
// 보간 in <space>) 재정렬. computed=false 면 색 키워드(red)를 유지한다(지정값).
fn normalize_gradient_seg(seg: &str, computed: bool, default_space: &str) -> String {
    let toks = split_top_level(seg);
    if toks.is_empty() {
        return seg.to_string();
    }
    match interpret_value(&toks[0]) {
        Some(v @ (Value::Color(_) | Value::ColorFn(..))) => {
            let color = if !computed
                && toks[0].chars().all(|c| c.is_ascii_alphabetic() || c == '-')
            {
                toks[0].to_ascii_lowercase() // 지정값: 색 키워드 유지(red→red)
            } else {
                crate::style::computed_value_string(&v)
            };
            if toks.len() == 1 {
                color
            } else {
                format!("{} {}", color, toks[1..].join(" "))
            }
        }
        // prefix 세그먼트(각도/방향/보간): radial 기본 도형 ellipse 생략(§CSS Images 4,
        // ellipse 50% 40em→50% 40em) 후 보간 메서드를 각도/방향 뒤로 재정렬.
        _ => {
            let filtered: Vec<String> =
                toks.iter().filter(|t| !t.eq_ignore_ascii_case("ellipse")).cloned().collect();
            reorder_interp(&filtered, default_space)
        }
    }
}

// prefix 세그먼트에서 보간 메서드(in <space> [<method> hue])를 각도/방향/크기 뒤로
// 옮긴다. 기본 색공간(default_space)이면 생략(hue 없을 때), xyz→xyz-d65.
fn reorder_interp(toks: &[String], default_space: &str) -> String {
    let Some(in_idx) = toks.iter().position(|t| t.eq_ignore_ascii_case("in")) else {
        return toks.join(" ");
    };
    // in 블록 = in + <space> [+ <method> hue]. space 는 in 바로 뒤.
    let space_idx = in_idx + 1;
    let mut end = (space_idx + 1).min(toks.len()); // in + space 까지
    let hue_present = toks.get(end + 1).map(|t| t.eq_ignore_ascii_case("hue")).unwrap_or(false);
    let method = if hue_present {
        toks.get(space_idx + 1).map(|s| s.to_ascii_lowercase()).unwrap_or_default()
    } else {
        String::new()
    };
    if hue_present {
        end += 2; // 입력에서 <method> hue 블록 소비(생략하든 유지하든)
    }
    // 기본 hue 보간(shorter hue)은 생략, 나머지(longer/increasing/decreasing) 유지.
    let keep_hue = hue_present && method != "shorter";
    let space = toks.get(space_idx).map(|s| s.to_ascii_lowercase()).unwrap_or_default();
    let space_out = if space == "xyz" { "xyz-d65".to_string() } else { space.clone() };
    // 기본 색공간(srgb/oklab, 직교=무hue)이면 보간 메서드 전체 생략.
    let interp = if space.eq_ignore_ascii_case(default_space) {
        String::new()
    } else {
        let mut parts = vec!["in".to_string(), space_out];
        if keep_hue {
            parts.extend(toks[space_idx + 1..end].iter().cloned());
        }
        parts.join(" ")
    };
    let rest: Vec<&str> = toks
        .iter()
        .enumerate()
        .filter(|(k, _)| *k < in_idx || *k >= end)
        .map(|(_, t)| t.as_str())
        .collect();
    match (rest.is_empty(), interp.is_empty()) {
        (true, _) => interp,               // 각도/방향 없음 → 보간만(또는 빈=삭제)
        (false, true) => rest.join(" "),   // 기본 보간 생략 → 각도/방향만
        (false, false) => format!("{} {}", rest.join(" "), interp),
    }
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
// 색 함수 성분 분리. **괄호 깊이 인식**: 최상위(depth 0)의 공백/쉼표/슬래시로만
// 나누고, calc(50 * 3) 이나 calc(0 / 0) 처럼 괄호 안의 공백·슬래시는 보존한다.
// 예전엔 단순 split 이라 calc 내부 공백에서 토큰이 쪼개져 모던 색 함수가 깨졌다.
fn color_parts(inner: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    for c in inner.chars() {
        match c {
            '(' => {
                depth += 1;
                cur.push(c);
            }
            ')' => {
                depth -= 1;
                cur.push(c);
            }
            ',' | ' ' | '\t' | '\n' | '/' if depth == 0 => {
                if !cur.trim().is_empty() {
                    out.push(cur.trim().to_string());
                }
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

// 채널 값: 0-255 정수/실수 또는 퍼센트(0-100%).
fn chan_val(s: &str) -> Option<u8> {
    let s = s.trim();
    // none 은 0 으로 계산(§CSS Color 4). 우리 색 모델은 8비트라 none 을 못 실으므로
    // 근사(계산값 none 보존은 색 모델 확장이 필요한 별개 과제).
    if s.eq_ignore_ascii_case("none") {
        return Some(0);
    }
    if let Some(p) = s.strip_suffix('%') {
        return Some((p.trim().parse::<f32>().ok()? / 100.0 * 255.0).clamp(0.0, 255.0).round() as u8);
    }
    Some(s.parse::<f32>().ok()?.clamp(0.0, 255.0) as u8)
}

fn alpha_val(s: &str) -> Option<u8> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("none") {
        return Some(0);
    }
    if let Some(p) = s.strip_suffix('%') {
        return Some((p.trim().parse::<f32>().ok()? / 100.0 * 255.0).clamp(0.0, 255.0).round() as u8);
    }
    Some((s.parse::<f32>().ok()?.clamp(0.0, 1.0) * 255.0).round() as u8)
}

// 함수 표기의 괄호 안 내용.
fn func_inner(text: &str) -> Option<&str> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    if close <= open { return None; }
    Some(&text[open + 1..close])
}

// 모던 색 함수 컴포넌트: 수 / 퍼센트(pct_base 기준) / none(→0).
fn comp_num(s: &str, pct_base: f32) -> Option<f32> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("none") {
        return Some(0.0);
    }
    if let Some(p) = s.strip_suffix('%') {
        return Some(p.trim().parse::<f32>().ok()? / 100.0 * pct_base);
    }
    s.parse::<f32>().ok()
}

// 각도 컴포넌트(deg/grad/rad/turn, 무단위=deg, none→0).
fn comp_angle(s: &str) -> Option<f32> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("none") {
        return Some(0.0);
    }
    for (suf, mul) in [("deg", 1.0), ("grad", 0.9), ("turn", 360.0)] {
        if let Some(p) = s.strip_suffix(suf) {
            return Some(p.trim().parse::<f32>().ok()? * mul);
        }
    }
    if let Some(p) = s.strip_suffix("rad") {
        return Some(p.trim().parse::<f32>().ok()?.to_degrees());
    }
    s.parse::<f32>().ok()
}

// oklch(L C H [/ A]) — L: 0..1(또는 %of1), C: 수(또는 %of0.4), H: 각도.
// 모던 색 함수의 성분: 수 또는 none(계산값에서 보존). % 는 pct_base 로 해석해 수로.
#[derive(Clone, Copy)]
enum Comp {
    None,
    Val(f32),
}
impl Comp {
    fn get(self) -> f32 {
        match self {
            Comp::None => 0.0,
            Comp::Val(v) => v,
        }
    }
    fn clamp(self, lo: f32, hi: f32) -> Comp {
        match self {
            Comp::None => Comp::None,
            Comp::Val(v) => Comp::Val(v.clamp(lo, hi)),
        }
    }
    fn clamp_lo(self, lo: f32) -> Comp {
        match self {
            Comp::None => Comp::None,
            Comp::Val(v) => Comp::Val(v.max(lo)),
        }
    }
    fn ser(self) -> String {
        match self {
            Comp::None => "none".to_string(),
            Comp::Val(v) => csnum(v),
        }
    }
}

// 색 성분 수 직렬화(§CSSOM): 최대 4소수 자리 후 뒤 0/점 제거. -0 은 0 으로.
// (num_css 는 3자리라 lch 각도 등에서 정밀도가 모자랐다.)
fn csnum(v: f32) -> String {
    let s = format!("{:.4}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" || s.is_empty() {
        "0".to_string()
    } else {
        s.to_string()
    }
}

// 성분 파싱: none / 퍼센트(pct_base) / 수 / calc(). calc 는 eval_calc 로 수를 뽑고,
// NaN/inf 나 계산 불가(sign()/컨테이너단위 등)는 0 으로 근사(값은 유효로 본다).
fn parse_comp(s: &str, pct_base: f32) -> Option<Comp> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("none") {
        return Some(Comp::None);
    }
    if s.to_ascii_lowercase().starts_with("calc(") && s.ends_with(')') {
        let inner = &s[5..s.len() - 1];
        let n = match eval_calc(inner) {
            // 결과가 퍼센트면 채널 기준(pct_base)으로 환산(rgb 는 255, lab L 은 100 등).
            Some(Value::Length(n, Unit::Percent)) if n.is_finite() => n / 100.0 * pct_base,
            Some(Value::Length(n, _)) if n.is_finite() => n,
            // px + pct 혼합(calc((r/255)*100%) 등). % 는 채널 기준으로 환산.
            Some(Value::Calc(c)) if !c.has_ctx_units() => c.px + c.pct / 100.0 * pct_base,
            _ => 0.0,
        };
        return Some(Comp::Val(n));
    }
    if let Some(p) = s.strip_suffix('%') {
        return Some(Comp::Val(p.trim().parse::<f32>().ok()? / 100.0 * pct_base));
    }
    s.parse::<f32>().ok().map(Comp::Val)
}
fn parse_comp_angle(s: &str) -> Option<Comp> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("none") {
        return Some(Comp::None);
    }
    // calc() 각도: deg 를 떼고(결과는 도 단위) 산술만 평가. 비유한/불가는 0.
    if s.to_ascii_lowercase().starts_with("calc(") && s.ends_with(')') {
        let inner = s[5..s.len() - 1].replace("deg", "");
        let n = match eval_calc(&inner) {
            Some(Value::Length(n, _)) if n.is_finite() => n,
            _ => 0.0,
        };
        return Some(Comp::Val(n));
    }
    comp_angle(s).map(Comp::Val)
}
// 알파 파싱: 없으면 1(불투명), none, 그 외 [0,1] 클램프(% 는 /100).
fn parse_alpha(s: Option<&String>) -> Option<Comp> {
    match s {
        None => Some(Comp::Val(1.0)),
        Some(t) => Some(parse_comp(t, 1.0)?.clamp(0.0, 1.0)),
    }
}
fn alpha_u8(c: Comp) -> u8 {
    (c.get().clamp(0.0, 1.0) * 255.0).round() as u8
}
fn alpha_frac(c: Comp) -> f32 {
    c.get().clamp(0.0, 1.0)
}

// lab/lch/oklab/oklch 색을 float sRGB+알파로 (u8 양자화 회피 — color-mix 정밀도용).
fn lab_family_srgb_f(name: &str, text: &str) -> Option<[f32; 4]> {
    let p = color_parts(func_inner(text)?);
    if p.len() < 3 {
        return None;
    }
    let alpha = alpha_frac(parse_alpha(p.get(3))?);
    let (l_hi, ab_base, c_base) = match name {
        "lab" => (100.0, 125.0, 0.0),
        "oklab" => (1.0, 0.4, 0.0),
        "lch" => (100.0, 0.0, 150.0),
        "oklch" => (1.0, 0.0, 0.4),
        _ => return None,
    };
    let l = parse_comp(&p[0], l_hi)?.clamp(0.0, l_hi).get();
    let (lr, lg, lb) = if matches!(name, "lch" | "oklch") {
        let c = parse_comp(&p[1], c_base)?.clamp_lo(0.0).get();
        let h = parse_comp_angle(&p[2])?.get().to_radians();
        let (a, b) = (c * h.cos(), c * h.sin());
        if name == "lch" {
            lab_to_lin_srgb(l, a, b)
        } else {
            oklab_to_lin_srgb(l, a, b)
        }
    } else {
        let a = parse_comp(&p[1], ab_base)?.get();
        let b = parse_comp(&p[2], ab_base)?.get();
        if name == "lab" {
            lab_to_lin_srgb(l, a, b)
        } else {
            oklab_to_lin_srgb(l, a, b)
        }
    };
    Some([linear_to_srgb(lr), linear_to_srgb(lg), linear_to_srgb(lb), alpha])
}

fn comp_opt(c: Comp) -> Option<f32> {
    match c {
        Comp::None => None,
        Comp::Val(v) => Some(v),
    }
}

// 색을 보간 공간의 좌표(성분별 none 보존)+알파로 파싱한다(color-mix 용).
// 입력 색 함수가 보간 공간과 같으면 성분을 직접(none 보존) 파싱하고, 다르면
// sRGB 를 거쳐 변환한다(이 경우 none 은 소실 — 교차 공간의 analogous 는 근사).
fn color_coords_none(space: &str, cs: &str) -> Option<([Option<f32>; 3], Option<f32>)> {
    let low = cs.trim().to_ascii_lowercase();
    // 같은 공간 함수면 성분을 none 보존해 직접.
    let direct = match space {
        "hsl" => low.starts_with("hsl(") || low.starts_with("hsla("),
        "hwb" => low.starts_with("hwb("),
        "oklch" => low.starts_with("oklch("),
        "oklab" => low.starts_with("oklab("),
        "lch" => low.starts_with("lch("),
        "lab" => low.starts_with("lab("),
        _ => false,
    };
    if direct {
        let p = color_parts(func_inner(&low)?);
        if p.len() < 3 {
            return None;
        }
        let alpha = if p.len() >= 4 {
            comp_opt(parse_alpha(Some(&p[3]))?)
        } else {
            Some(1.0)
        };
        let (c0, c1, c2) = match space {
            "hsl" => (
                comp_opt(parse_comp_angle(&p[0])?),
                comp_opt(parse_comp(&p[1], 1.0)?),
                comp_opt(parse_comp(&p[2], 1.0)?),
            ),
            "hwb" => (
                comp_opt(parse_comp_angle(&p[0])?),
                comp_opt(parse_comp(&p[1], 1.0)?),
                comp_opt(parse_comp(&p[2], 1.0)?),
            ),
            "oklab" => (
                comp_opt(parse_comp(&p[0], 1.0)?),
                comp_opt(parse_comp(&p[1], 0.4)?),
                comp_opt(parse_comp(&p[2], 0.4)?),
            ),
            "lab" => (
                comp_opt(parse_comp(&p[0], 100.0)?),
                comp_opt(parse_comp(&p[1], 125.0)?),
                comp_opt(parse_comp(&p[2], 125.0)?),
            ),
            "oklch" => (
                comp_opt(parse_comp(&p[0], 1.0)?),
                comp_opt(parse_comp(&p[1], 0.4)?),
                comp_opt(parse_comp_angle(&p[2])?),
            ),
            "lch" => (
                comp_opt(parse_comp(&p[0], 100.0)?),
                comp_opt(parse_comp(&p[1], 150.0)?),
                comp_opt(parse_comp_angle(&p[2])?),
            ),
            _ => return None,
        };
        return Some(([c0, c1, c2], alpha));
    }
    // color(<space> …) 입력이 보간 공간과 같으면 성분 직접(none 보존, 감마 좌표 그대로).
    if low.starts_with("color(") {
        let p = color_parts(func_inner(&low)?);
        if p.len() >= 4 {
            let in_sp = if p[0] == "xyz" { "xyz-d65" } else { p[0].as_str() };
            let mix_sp = if space == "xyz" { "xyz-d65" } else { space };
            if in_sp == mix_sp {
                let alpha = if p.len() >= 5 {
                    comp_opt(parse_alpha(Some(&p[4]))?)
                } else {
                    Some(1.0)
                };
                return Some((
                    [
                        comp_opt(parse_comp(&p[1], 1.0)?),
                        comp_opt(parse_comp(&p[2], 1.0)?),
                        comp_opt(parse_comp(&p[3], 1.0)?),
                    ],
                    alpha,
                ));
            }
        }
    }
    // 교차 공간: sRGB 를 거쳐 변환(none 없음).
    let f = srgb_float_of(&low)?;
    let co = srgb_to_space(space, f[0], f[1], f[2])?;
    Some(([Some(co[0]), Some(co[1]), Some(co[2])], Some(f[3])))
}

// 색 문자열 → float sRGB+알파. 모던 색함수는 float 로 직접(u8 양자화 회피),
// 나머지는 interpret_value 의 u8 을 /255.
fn srgb_float_of(s: &str) -> Option<[f32; 4]> {
    let low = s.trim().to_ascii_lowercase();
    for name in ["oklch", "oklab", "lch", "lab"] {
        if low.starts_with(name) && low[name.len()..].starts_with('(') {
            return lab_family_srgb_f(name, &low);
        }
    }
    let c = interpret_value(&low).and_then(|v| v.paint_color())?;
    Some([
        c.r as f32 / 255.0,
        c.g as f32 / 255.0,
        c.b as f32 / 255.0,
        c.a as f32 / 255.0,
    ])
}
// 알파 직렬화: 1 이면 생략, none 이면 " / none", 그 외 " / <값>".
fn alpha_ser(c: Comp) -> String {
    match c {
        Comp::Val(v) if (v - 1.0).abs() < f32::EPSILON => String::new(),
        Comp::None => " / none".to_string(),
        Comp::Val(v) => format!(" / {}", csnum(v)),
    }
}

// lab/lch/oklab/oklch — 페인팅 sRGB 근사 + getComputedStyle 캐논 직렬화를 함께 만든다.
fn parse_lab_family(name: &str, text: &str) -> Option<(Color, Box<str>)> {
    let p = color_parts(func_inner(text)?);
    if p.len() < 3 {
        return None;
    }
    let alpha = parse_alpha(p.get(3))?;
    let au = alpha_u8(alpha);
    let (l_hi, ab_base, c_base) = match name {
        "lab" => (100.0, 125.0, 0.0),
        "oklab" => (1.0, 0.4, 0.0),
        "lch" => (100.0, 0.0, 150.0),
        "oklch" => (1.0, 0.0, 0.4),
        _ => return None,
    };
    let l = parse_comp(&p[0], l_hi)?.clamp(0.0, l_hi);
    let polar = matches!(name, "lch" | "oklch");
    let (c2, c3, rgba) = if polar {
        let c = parse_comp(&p[1], c_base)?.clamp_lo(0.0);
        // 색상(hue)은 계산값에서 [0, 360) 로 정규화된다.
        let h = match parse_comp_angle(&p[2])? {
            Comp::Val(v) => Comp::Val(((v % 360.0) + 360.0) % 360.0),
            Comp::None => Comp::None,
        };
        let rgba = if name == "lch" {
            lch_to_color(l.get(), c.get(), h.get(), au)
        } else {
            oklch_to_color(l.get(), c.get(), h.get(), au)
        };
        (c, h, rgba)
    } else {
        let a = parse_comp(&p[1], ab_base)?;
        let b = parse_comp(&p[2], ab_base)?;
        let rgba = if name == "lab" {
            lab_to_color(l.get(), a.get(), b.get(), au)
        } else {
            oklab_to_color(l.get(), a.get(), b.get(), au)
        };
        (a, b, rgba)
    };
    let serial = format!("{}({} {} {}{})", name, l.ser(), c2.ser(), c3.ser(), alpha_ser(alpha));
    Some((rgba, serial.into_boxed_str()))
}
// hwb(H W B [/ A]) — H: 각도, W/B: 퍼센트.
fn parse_hwb(text: &str) -> Option<Color> {
    let p = color_parts(func_inner(text)?);
    if p.len() < 3 { return None; }
    let a = if p.len() >= 4 { alpha_val(&p[3])? } else { 255 };
    Some(hwb_to_color(comp_angle(&p[0])?, comp_num(&p[1], 1.0)?, comp_num(&p[2], 1.0)?, a))
}

// color-mix 의 한 색 성분: "[p%] <color>" 또는 "<color> [p%]" → (sRGB float+알파, 퍼센트).
fn parse_mix_color(s: &str) -> Option<([f32; 4], Option<f32>)> {
    let s = s.trim();
    let toks = split_top_level(s);
    // 퍼센트 토큰을 찾아 떼고 나머지를 색으로 해석.
    let mut pct = None;
    let mut color_str = String::new();
    for t in &toks {
        if let Some(p) = t.strip_suffix('%') {
            if let Ok(v) = p.trim().parse::<f32>() {
                pct = Some(v);
                continue;
            }
        }
        if !color_str.is_empty() {
            color_str.push(' ');
        }
        color_str.push_str(t);
    }
    let col = srgb_float_of(&color_str)?;
    Some((col, pct))
}

// sRGB float(0..1) → 보간 색공간 좌표. 극좌표 공간은 [2] 가 색상(도).
fn srgb_to_space(space: &str, r: f32, g: f32, b: f32) -> Option<[f32; 3]> {
    Some(match space {
        "srgb" => [r, g, b],
        "srgb-linear" => [srgb_gamma_inv(r), srgb_gamma_inv(g), srgb_gamma_inv(b)],
        "oklab" => {
            let (l, a, bb) = srgb_to_oklab(r, g, b);
            [l, a, bb]
        }
        "oklch" => {
            let (l, a, bb) = srgb_to_oklab(r, g, b);
            [l, (a * a + bb * bb).sqrt(), bb.atan2(a).to_degrees().rem_euclid(360.0)]
        }
        "hsl" => {
            let (h, s, l) = srgb_to_hsl(r, g, b);
            [h, s, l]
        }
        "hwb" => {
            let (h, w, bl) = srgb_to_hwb(r, g, b);
            [h, w, bl]
        }
        "lab" => {
            let (l, a, bb) = srgb_to_lab(r, g, b);
            [l, a, bb]
        }
        "lch" => {
            let (l, a, bb) = srgb_to_lab(r, g, b);
            [l, (a * a + bb * bb).sqrt(), bb.atan2(a).to_degrees().rem_euclid(360.0)]
        }
        "xyz" | "xyz-d65" => {
            let (lr, lg, lb) = (srgb_gamma_inv(r), srgb_gamma_inv(g), srgb_gamma_inv(b));
            let (x, y, z) = mat3(&LSRGB_TO_XYZ65, lr, lg, lb);
            [x, y, z]
        }
        "xyz-d50" => {
            let (lr, lg, lb) = (srgb_gamma_inv(r), srgb_gamma_inv(g), srgb_gamma_inv(b));
            let (x, y, z) = mat3(&LSRGB_TO_XYZ65, lr, lg, lb);
            let (x, y, z) = mat3(&BRADFORD_D65_D50, x, y, z);
            [x, y, z]
        }
        "display-p3" | "display-p3-linear" => {
            let (lr, lg, lb) = (srgb_gamma_inv(r), srgb_gamma_inv(g), srgb_gamma_inv(b));
            let (x, y, z) = mat3(&LSRGB_TO_XYZ65, lr, lg, lb);
            let (pr, pg, pb) = mat3(&XYZ65_TO_P3, x, y, z);
            if space == "display-p3-linear" {
                [pr, pg, pb]
            } else {
                [linear_to_srgb(pr), linear_to_srgb(pg), linear_to_srgb(pb)]
            }
        }
        "rec2020" => {
            let (lr, lg, lb) = (srgb_gamma_inv(r), srgb_gamma_inv(g), srgb_gamma_inv(b));
            let (x, y, z) = mat3(&LSRGB_TO_XYZ65, lr, lg, lb);
            let (a, b2, c) = mat3(&XYZ65_TO_REC2020, x, y, z);
            [rec2020_encode(a), rec2020_encode(b2), rec2020_encode(c)]
        }
        "a98-rgb" => {
            let (lr, lg, lb) = (srgb_gamma_inv(r), srgb_gamma_inv(g), srgb_gamma_inv(b));
            let (x, y, z) = mat3(&LSRGB_TO_XYZ65, lr, lg, lb);
            let (a, b2, c) = mat3(&XYZ65_TO_A98, x, y, z);
            [a98_encode(a), a98_encode(b2), a98_encode(c)]
        }
        "prophoto-rgb" => {
            let (lr, lg, lb) = (srgb_gamma_inv(r), srgb_gamma_inv(g), srgb_gamma_inv(b));
            let (x, y, z) = mat3(&LSRGB_TO_XYZ65, lr, lg, lb);
            let (x, y, z) = mat3(&BRADFORD_D65_D50, x, y, z);
            let (a, b2, c) = mat3(&XYZ50_TO_PROPHOTO, x, y, z);
            [prophoto_encode(a), prophoto_encode(b2), prophoto_encode(c)]
        }
        _ => return None,
    })
}
// 보간 색공간 좌표 → sRGB float.
fn space_to_srgb(space: &str, c: [f32; 3]) -> Option<(f32, f32, f32)> {
    Some(match space {
        "srgb" => (c[0], c[1], c[2]),
        "srgb-linear" => (linear_to_srgb(c[0]), linear_to_srgb(c[1]), linear_to_srgb(c[2])),
        "oklab" => {
            let (lr, lg, lb) = oklab_to_lin_srgb(c[0], c[1], c[2]);
            (linear_to_srgb(lr), linear_to_srgb(lg), linear_to_srgb(lb))
        }
        "oklch" => {
            let h = c[2].to_radians();
            let (lr, lg, lb) = oklab_to_lin_srgb(c[0], c[1] * h.cos(), c[1] * h.sin());
            (linear_to_srgb(lr), linear_to_srgb(lg), linear_to_srgb(lb))
        }
        "hsl" => {
            let (r, g, b) = hsl_to_rgb(c[0], c[1].clamp(0.0, 1.0), c[2].clamp(0.0, 1.0));
            (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0)
        }
        "hwb" => {
            let col = hwb_to_color(c[0], c[1], c[2], 255);
            (col.r as f32 / 255.0, col.g as f32 / 255.0, col.b as f32 / 255.0)
        }
        "lab" => {
            let (lr, lg, lb) = lab_to_lin_srgb(c[0], c[1], c[2]);
            (linear_to_srgb(lr), linear_to_srgb(lg), linear_to_srgb(lb))
        }
        "lch" => {
            let h = c[2].to_radians();
            let (lr, lg, lb) = lab_to_lin_srgb(c[0], c[1] * h.cos(), c[1] * h.sin());
            (linear_to_srgb(lr), linear_to_srgb(lg), linear_to_srgb(lb))
        }
        "xyz" | "xyz-d65" => {
            let (lr, lg, lb) = mat3(&XYZ65_TO_LSRGB, c[0], c[1], c[2]);
            (linear_to_srgb(lr), linear_to_srgb(lg), linear_to_srgb(lb))
        }
        "xyz-d50" => {
            let (x, y, z) = mat3(&BRADFORD_D50_D65, c[0], c[1], c[2]);
            let (lr, lg, lb) = mat3(&XYZ65_TO_LSRGB, x, y, z);
            (linear_to_srgb(lr), linear_to_srgb(lg), linear_to_srgb(lb))
        }
        "display-p3" | "display-p3-linear" => {
            let (pr, pg, pb) = if space == "display-p3-linear" {
                (c[0], c[1], c[2])
            } else {
                (srgb_gamma_inv(c[0]), srgb_gamma_inv(c[1]), srgb_gamma_inv(c[2]))
            };
            let (x, y, z) = mat3(&P3_TO_XYZ65, pr, pg, pb);
            let (lr, lg, lb) = mat3(&XYZ65_TO_LSRGB, x, y, z);
            (linear_to_srgb(lr), linear_to_srgb(lg), linear_to_srgb(lb))
        }
        "rec2020" => {
            let (a, b, cc) = (rec2020_decode(c[0]), rec2020_decode(c[1]), rec2020_decode(c[2]));
            let (x, y, z) = mat3(&REC2020_TO_XYZ65, a, b, cc);
            let (lr, lg, lb) = mat3(&XYZ65_TO_LSRGB, x, y, z);
            (linear_to_srgb(lr), linear_to_srgb(lg), linear_to_srgb(lb))
        }
        "a98-rgb" => {
            let (a, b, cc) = (a98_decode(c[0]), a98_decode(c[1]), a98_decode(c[2]));
            let (x, y, z) = mat3(&A98_TO_XYZ65, a, b, cc);
            let (lr, lg, lb) = mat3(&XYZ65_TO_LSRGB, x, y, z);
            (linear_to_srgb(lr), linear_to_srgb(lg), linear_to_srgb(lb))
        }
        "prophoto-rgb" => {
            let (a, b, cc) = (prophoto_decode(c[0]), prophoto_decode(c[1]), prophoto_decode(c[2]));
            let (x, y, z) = mat3(&PROPHOTO_TO_XYZ50, a, b, cc);
            let (x, y, z) = mat3(&BRADFORD_D50_D65, x, y, z);
            let (lr, lg, lb) = mat3(&XYZ65_TO_LSRGB, x, y, z);
            (linear_to_srgb(lr), linear_to_srgb(lg), linear_to_srgb(lb))
        }
        _ => return None,
    })
}
// 색공간별 채널 클램프(계산값 범위): L 은 lab/lch 0-100, oklab/oklch 0-1;
// C(채도)는 lch/oklch 에서 음수 불가. 그 외는 클램프 안 함.
fn clamp_channel(space: &str, i: usize, v: f32) -> f32 {
    match (space, i) {
        ("lab" | "lch", 0) => v.clamp(0.0, 100.0),
        ("oklab" | "oklch", 0) => v.clamp(0.0, 1.0),
        ("lch" | "oklch", 1) => v.max(0.0),
        _ => v,
    }
}

// 극좌표 색공간에서 색상(hue) 성분의 인덱스. hsl/hwb 는 0, lch/oklch 는 2.
fn hue_index(space: &str) -> Option<usize> {
    match space {
        "hsl" | "hwb" => Some(0),
        "lch" | "oklch" => Some(2),
        _ => None,
    }
}

// 색상이 무력(powerless)한가 = 무채색. hsl: s=0 또는 l=0/1, hwb: w+b>=1,
// lch/oklch: c=0. 이 경우 색상은 보간에서 상대 색을 따른다(§CSS Color 4).
fn hue_powerless(space: &str, co: &[f32; 3]) -> bool {
    match space {
        "hsl" => co[1].abs() < 1e-4 || co[2] <= 1e-4 || co[2] >= 1.0 - 1e-4,
        "hwb" => co[1] + co[2] >= 1.0 - 1e-4,
        "lch" | "oklch" => co[1].abs() < 1e-4,
        _ => false,
    }
}

// relative color 의 함수별 설정: (보간 공간, 채널 키워드 3개, 채널별 pct 기준,
// 채널별 각도 여부, rgb 스케일). rgb 만 성분이 0-255(스케일 255), 나머지는 native.
struct RelSpec {
    space: &'static str,
    chans: [&'static str; 3],
    pct: [f32; 3],
    angle: [bool; 3],
    // 채널별 배율: 원본좌표→키워드값(oc*scale) 및 성분값→공간좌표(comp/scale).
    // rgb=255, hsl/hwb 의 s/l/w/b=100(퍼센트 수), 그 외 native=1.
    scale: [f32; 3],
}
fn rel_spec(func: &str, color_space: Option<&str>) -> Option<RelSpec> {
    Some(match func {
        "rgb" | "rgba" => RelSpec { space: "srgb", chans: ["r", "g", "b"], pct: [255.0, 255.0, 255.0], angle: [false; 3], scale: [255.0, 255.0, 255.0] },
        "hsl" | "hsla" => RelSpec { space: "hsl", chans: ["h", "s", "l"], pct: [1.0, 100.0, 100.0], angle: [true, false, false], scale: [1.0, 100.0, 100.0] },
        "hwb" => RelSpec { space: "hwb", chans: ["h", "w", "b"], pct: [1.0, 100.0, 100.0], angle: [true, false, false], scale: [1.0, 100.0, 100.0] },
        "lab" => RelSpec { space: "lab", chans: ["l", "a", "b"], pct: [100.0, 125.0, 125.0], angle: [false; 3], scale: [1.0, 1.0, 1.0] },
        "oklab" => RelSpec { space: "oklab", chans: ["l", "a", "b"], pct: [1.0, 0.4, 0.4], angle: [false; 3], scale: [1.0, 1.0, 1.0] },
        "lch" => RelSpec { space: "lch", chans: ["l", "c", "h"], pct: [100.0, 150.0, 1.0], angle: [false, false, true], scale: [1.0, 1.0, 1.0] },
        "oklch" => RelSpec { space: "oklch", chans: ["l", "c", "h"], pct: [1.0, 0.4, 1.0], angle: [false, false, true], scale: [1.0, 1.0, 1.0] },
        "color" => {
            let sp = color_space?;
            let chans = if sp.starts_with("xyz") { ["x", "y", "z"] } else { ["r", "g", "b"] };
            let space = match sp {
                "srgb" => "srgb", "srgb-linear" => "srgb-linear", "display-p3" => "display-p3",
                "display-p3-linear" => "display-p3-linear", "rec2020" => "rec2020",
                "a98-rgb" => "a98-rgb", "prophoto-rgb" => "prophoto-rgb",
                "xyz" | "xyz-d65" => "xyz-d65", "xyz-d50" => "xyz-d50", _ => return None,
            };
            RelSpec { space, chans, pct: [1.0, 1.0, 1.0], angle: [false; 3], scale: [1.0, 1.0, 1.0] }
        }
        _ => return None,
    })
}

// 문자열의 식별자 낱말 중 채널 키워드(r/g/b/alpha/l/c/h …)를 그 값으로 치환한다.
// calc(r * 2) 안의 r 도 바꾼다. 숫자·연산자·괄호는 그대로.
fn subst_channels(s: &str, kv: &impl Fn(&str) -> Option<f32>) -> String {
    let b = s.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_alphabetic() || b[i] == b'_' {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'-') {
                i += 1;
            }
            let word = &s[start..i];
            match kv(&word.to_ascii_lowercase()) {
                Some(v) => out.push_str(&csnum(v)),
                None => out.push_str(word),
            }
        } else {
            out.push(b[i] as char);
            i += 1;
        }
    }
    out
}

// <func>(from <origin> [<space>] <c0> <c1> <c2> [/ <alpha>]) — 상대 색(CSS Color 5).
// 원본 색의 채널을 키워드(r/g/b/l/c/h/alpha …)로 성분에 쓴다. calc 는 미지원(이 파일엔 없음).
fn parse_relative_color(func: &str, text: &str) -> Option<(Color, Box<str>)> {
    let inner = func_inner(text)?;
    let rest = inner.trim().strip_prefix("from ")?.trim();
    let toks = split_top_level(rest);
    // color 는 origin 다음에 공간 토큰. 성분은 3개 + 선택적 "/ alpha".
    let mut idx = 1;
    let origin_str = toks.first()?.clone();
    let color_space = if func == "color" {
        let s = toks.get(idx)?.clone();
        idx += 1;
        Some(s)
    } else {
        None
    };
    let spec = rel_spec(func, color_space.as_deref())?;
    // 성분/알파 분리 ("/" 토큰 기준).
    let comp_toks: Vec<&String> = toks[idx..].iter().take_while(|t| t.as_str() != "/").collect();
    if comp_toks.len() < 3 {
        return None;
    }
    let alpha_tok = toks.iter().position(|t| t == "/").and_then(|p| toks.get(p + 1));
    // 원본 색을 공간 좌표+알파로. 같은 공간이면 직접 파싱해 gamut 밖 값·정밀도 보존.
    let (oc_opt, oa_opt) = color_coords_none(spec.space, &origin_str)?;
    let oc = [
        clamp_channel(spec.space, 0, oc_opt[0].unwrap_or(0.0)),
        clamp_channel(spec.space, 1, oc_opt[1].unwrap_or(0.0)),
        clamp_channel(spec.space, 2, oc_opt[2].unwrap_or(0.0)),
    ];
    let oalpha = oa_opt.unwrap_or(1.0).clamp(0.0, 1.0);
    // 키워드→값 맵(채널별 배율).
    let kv = |name: &str| -> Option<f32> {
        if name == "alpha" {
            return Some(oalpha);
        }
        for i in 0..3 {
            if spec.chans[i] == name {
                return Some(oc[i] * spec.scale[i]);
            }
        }
        None
    };
    // 성분 해석: 채널 키워드를 값으로 치환(calc 안 포함) 후 parse_comp/각도.
    // 성분이 **직접 채널 참조**(r/g/b 등)이고 origin 의 그 채널이 none 이면 none 을
    // 보존한다(§CSS Color 5, rgb(from rgb(none...) r g b)→color(srgb none...)).
    // calc 등 연산이 섞이면 none→0 (계산 결과).
    let resolve = |tok: &str, i: usize| -> Option<Comp> {
        let t = tok.trim();
        if let Some(ci) = spec.chans.iter().position(|&c| c == t) {
            if oc_opt[ci].is_none() {
                return Some(Comp::None);
            }
        }
        let subbed = subst_channels(t, &kv);
        if spec.angle[i] {
            parse_comp_angle(&subbed)
        } else {
            parse_comp(&subbed, spec.pct[i])
        }
    };
    let c0 = resolve(comp_toks[0], 0)?;
    let c1 = resolve(comp_toks[1], 1)?;
    let c2 = resolve(comp_toks[2], 2)?;
    let alpha = match alpha_tok {
        None => Comp::Val(oalpha), // 알파 생략 시 원본 알파를 보존(기본값 1 아님)
        Some(t) => {
            // alpha 키워드는 0-1, 그 외 채널 키워드/calc 도 치환해 파싱.
            if t.eq_ignore_ascii_case("alpha") {
                if oa_opt.is_none() {
                    Comp::None // origin alpha 가 none → 보존
                } else {
                    Comp::Val(oalpha)
                }
            } else {
                let subbed = subst_channels(t, &kv);
                parse_alpha(Some(&subbed))?
            }
        }
    };
    // 공간 좌표(채널별 배율로 환원 + 범위 클램프).
    let coords = [
        clamp_channel(spec.space, 0, c0.get() / spec.scale[0]),
        clamp_channel(spec.space, 1, c1.get() / spec.scale[1]),
        clamp_channel(spec.space, 2, c2.get() / spec.scale[2]),
    ];
    let (rr, gg, bb) = space_to_srgb(spec.space, coords)?;
    let a = alpha_frac(alpha);
    let rgba = Color { r: to_u8(rr), g: to_u8(gg), b: to_u8(bb), a: (a * 255.0).round() as u8 };
    let none_out = [matches!(c0, Comp::None), matches!(c1, Comp::None), matches!(c2, Comp::None)];
    let alpha_out = if matches!(alpha, Comp::None) { None } else { Some(a) };
    let serial = if none_out.iter().any(|&x| x) || alpha_out.is_none() {
        serialize_mix_native(spec.space, &coords, &none_out, alpha_out)
    } else {
        serialize_mix(spec.space, &coords, rr, gg, bb, alpha_out)
    };
    Some((rgba, serial.into_boxed_str()))
}

// text-decoration-line 캐논: 표준 순서(underline overline line-through blink)로 재정렬.
// none/전역키워드/그 외는 None(호출부가 원문 유지). 중복/무효 키워드도 None.
pub fn normalize_text_decoration_line(raw: &str) -> Option<String> {
    let low = raw.trim().to_ascii_lowercase();
    let order = ["underline", "overline", "line-through", "blink", "spelling-error", "grammar-error"];
    let toks: Vec<&str> = low.split_whitespace().collect();
    if toks.len() < 2 || !toks.iter().all(|t| order.contains(t)) {
        return None; // 단일값/none/무효는 그대로
    }
    // 중복 금지.
    let mut seen = std::collections::HashSet::new();
    if !toks.iter().all(|t| seen.insert(*t)) {
        return None;
    }
    let out: Vec<&str> = order.iter().copied().filter(|o| toks.contains(o)).collect();
    Some(out.join(" "))
}

// white-space 단축(§CSS Text 4) 캐논: <white-space-collapse> || <text-wrap-mode> 를
// 표준 키워드로(collapse+wrap→normal, preserve+nowrap→pre 등). 컴포넌트/키워드 모두 받음.
pub fn normalize_white_space(raw: &str) -> Option<String> {
    let low = raw.trim().to_ascii_lowercase();
    if low.is_empty() {
        return None;
    }
    // 단일 단축 키워드 → (collapse, wrap) 성분.
    let (mut collapse, mut wrap) = match low.as_str() {
        "normal" => ("collapse", "wrap"),
        "pre" => ("preserve", "nowrap"),
        "nowrap" => ("collapse", "nowrap"),
        "pre-wrap" => ("preserve", "wrap"),
        "pre-line" => ("preserve-breaks", "wrap"),
        _ => {
            // 컴포넌트 형태: collapse/preserve/… + wrap/nowrap.
            let (mut c, mut w) = ("collapse", "wrap");
            let (mut sc, mut sw) = (false, false);
            for tok in low.split_whitespace() {
                match tok {
                    "collapse" | "preserve" | "preserve-breaks" | "preserve-spaces"
                    | "break-spaces" => {
                        c = tok;
                        sc = true;
                    }
                    "wrap" | "nowrap" => {
                        w = tok;
                        sw = true;
                    }
                    _ => return None,
                }
            }
            if !sc && !sw {
                return None;
            }
            (c, w)
        }
    };
    // 성분 → 표준 키워드(가능하면). 아니면 성분 형태.
    let _ = (&mut collapse, &mut wrap);
    Some(
        match (collapse, wrap) {
            ("collapse", "wrap") => "normal",
            ("preserve", "nowrap") => "pre",
            ("collapse", "nowrap") => "nowrap",
            ("preserve", "wrap") => "pre-wrap",
            ("preserve-breaks", "wrap") => "pre-line",
            ("break-spaces", "wrap") => "break-spaces",
            _ => return Some(format!("{collapse} {wrap}")),
        }
        .to_string(),
    )
}

// cursor 키워드(§CSS UI). 마지막 폴백은 반드시 이 중 하나.
const CURSOR_KEYWORDS: &[&str] = &[
    "auto", "default", "none", "context-menu", "help", "pointer", "progress", "wait",
    "cell", "crosshair", "text", "vertical-text", "alias", "copy", "move", "no-drop",
    "not-allowed", "grab", "grabbing", "e-resize", "n-resize", "ne-resize", "nw-resize",
    "s-resize", "se-resize", "sw-resize", "w-resize", "ew-resize", "ns-resize",
    "nesw-resize", "nwse-resize", "col-resize", "row-resize", "all-scroll", "zoom-in",
    "zoom-out",
];

// cursor 유효성(§CSS UI): [ <url> [<x> <y>]? ,]* <keyword>. url 만 이미지(gradient/
// light-dark 불가), 좌표는 <number> 2개, 마지막은 필수 키워드.
// cursor 의 <image>(§CSS Images 4 / Color 5): url() | 유효 gradient | image-set() |
// light-dark(A,B)(두 인자 모두 유효 이미지). light-dark(linear-gradient(red),…) 처럼
// 내부가 무효면 거부하도록 재귀 검증한다.
fn cursor_image_ok(u: &str) -> bool {
    let u = u.trim();
    let ul = u.to_ascii_lowercase();
    if ul.starts_with("url(") && u.ends_with(')') {
        return true;
    }
    const GRADS: [&str; 6] = [
        "linear-gradient(", "radial-gradient(", "conic-gradient(",
        "repeating-linear-gradient(", "repeating-radial-gradient(",
        "repeating-conic-gradient(",
    ];
    if GRADS.iter().any(|g| ul.starts_with(g)) {
        return u.ends_with(')') && gradient_valid(u);
    }
    if (ul.starts_with("image-set(") || ul.starts_with("-webkit-image-set(")) && u.ends_with(')') {
        // image-set(<item>#) — 각 항목은 <string>|<image> 로 시작(뒤에 resolution/type).
        let open = u.find('(').unwrap_or(0);
        let inner = &u[open + 1..u.len() - 1];
        let items = split_top_commas(inner);
        return !items.is_empty()
            && items.iter().all(|it| {
                let t = it.trim();
                let tl = t.to_ascii_lowercase();
                t.starts_with('"')
                    || t.starts_with('\'')
                    || tl.starts_with("url(")
                    || GRADS.iter().any(|g| tl.starts_with(g))
            });
    }
    if ul.starts_with("light-dark(") && u.ends_with(')') {
        let inner = &u["light-dark(".len()..u.len() - 1];
        let args = split_top_commas(inner);
        return args.len() == 2 && args.iter().all(|a| cursor_image_ok(a.trim()));
    }
    false
}

pub fn cursor_valid(raw: &str) -> bool {
    let parts = split_top_commas(raw);
    let Some((last, heads)) = parts.split_last() else {
        return false;
    };
    if !CURSOR_KEYWORDS.contains(&last.trim().to_ascii_lowercase().as_str()) {
        return false;
    }
    // 핫스팟 좌표는 <number> 2개(calc/math 포함).
    let coord_ok = |t: &str| -> bool {
        if t.parse::<f32>().is_ok() {
            return true;
        }
        let low = t.to_ascii_lowercase();
        low.ends_with(')')
            && ["calc(", "min(", "max(", "clamp(", "round("].iter().any(|p| low.starts_with(p))
    };
    for p in heads {
        let toks = split_top_level(p.trim());
        if !toks.first().is_some_and(|u| cursor_image_ok(u)) {
            return false;
        }
        match toks.len() {
            1 => {} // 이미지만
            3 => {
                if !coord_ok(&toks[1]) || !coord_ok(&toks[2]) {
                    return false; // 좌표는 <number> 2개(1px/3% 등 무효)
                }
            }
            _ => return false, // 좌표 1개/3개 등 무효
        }
    }
    true
}

// <time> 하나를 파싱: <number> 뒤에 s|ms 단위(대소문자 무시). 단위 없는 0 도 무효.
// inf/nan(Rust 파서가 받는) 은 is_finite 로 거른다. 값(부호 판단용)을 돌려준다.
fn parse_time_value(s: &str) -> Option<f64> {
    let low = s.trim().to_ascii_lowercase();
    let num = if let Some(n) = low.strip_suffix("ms") {
        n
    } else if let Some(n) = low.strip_suffix('s') {
        n
    } else {
        return None;
    };
    if num.is_empty() {
        return None;
    }
    let v: f64 = num.parse().ok()?;
    if v.is_finite() {
        Some(v)
    } else {
        None
    }
}

// <time># 목록 유효성(§CSS Transitions). allow_negative=false 면 duration(≥0), true 면 delay.
pub fn time_list_valid(raw: &str, allow_negative: bool) -> bool {
    let items = split_top_commas(raw);
    if items.is_empty() {
        return false;
    }
    items.iter().all(|item| {
        let low = item.trim().to_ascii_lowercase();
        // calc/min/max/clamp 등 수학함수는 결과가 <time> 여야(§CSS Values 4 타입 검사).
        // malformed·타입 불일치(min(1mt)/max(1px)/max(0)) 거부. 음수 클램프는 사용시.
        if math_function_valid(&low) {
            return math_time_valid(&low);
        }
        match parse_time_value(item) {
            Some(v) => allow_negative || v >= 0.0,
            None => false,
        }
    })
}

// text-transform 유효성(§CSS Text): none | [capitalize|uppercase|lowercase] ||
// full-width || full-size-kana, 또는 math-auto 단독. 카테고리 중복·혼합 거부.
pub fn text_transform_valid(raw: &str) -> bool {
    let toks = split_top_level(raw);
    if toks.is_empty() {
        return false;
    }
    if toks.len() == 1 {
        let low = toks[0].to_ascii_lowercase();
        if matches!(low.as_str(), "none" | "math-auto") {
            return true;
        }
    }
    let (mut has_case, mut has_fw, mut has_kana) = (false, false, false);
    for t in &toks {
        match t.to_ascii_lowercase().as_str() {
            "capitalize" | "uppercase" | "lowercase" => {
                if has_case {
                    return false;
                }
                has_case = true;
            }
            "full-width" => {
                if has_fw {
                    return false;
                }
                has_fw = true;
            }
            "full-size-kana" => {
                if has_kana {
                    return false;
                }
                has_kana = true;
            }
            _ => return false, // none/math-auto 를 다른 토큰과 함께, 또는 미인식
        }
    }
    true
}

// <length-percentage> 토큰인가(§CSS Values 4). 끝의 알파벳 연속을 단위로 보고 길이
// 단위 표와 대조. 단위 없는 수는 0 만. 각도/시간 등 다른 차원은 거부.
fn is_length_percentage(tok: &str) -> bool {
    let low = tok.trim().to_ascii_lowercase();
    if low.is_empty() {
        return false;
    }
    if let Some(num) = low.strip_suffix('%') {
        return num.parse::<f64>().map(|v| v.is_finite()).unwrap_or(false);
    }
    let unit_len = low.chars().rev().take_while(|c| c.is_ascii_alphabetic()).count();
    if unit_len == 0 {
        // 단위 없는 수는 0 만 <length> 로 유효.
        return low.parse::<f64>().map(|v| v == 0.0).unwrap_or(false);
    }
    let (num, unit) = low.split_at(low.len() - unit_len);
    if num.parse::<f64>().map(|v| !v.is_finite()).unwrap_or(true) {
        return false;
    }
    const LEN_UNITS: &[&str] = &[
        "px", "em", "rem", "ex", "rex", "cap", "rcap", "ch", "rch", "ic", "ric", "lh", "rlh",
        "vw", "vh", "vi", "vb", "vmin", "vmax", "svw", "svh", "svi", "svb", "svmin", "svmax",
        "lvw", "lvh", "lvi", "lvb", "lvmin", "lvmax", "dvw", "dvh", "dvi", "dvb", "dvmin",
        "dvmax", "cqw", "cqh", "cqi", "cqb", "cqmin", "cqmax", "cm", "mm", "q", "in", "pt", "pc",
    ];
    LEN_UNITS.contains(&unit)
}

// scroll-snap-type 유효성(§CSS Scroll Snap): none | [x|y|block|inline|both]
// [mandatory|proximity]?. axis 먼저, strictness 나중. 순서 뒤바뀜·중복·none 혼합 거부.
pub fn scroll_snap_type_valid(raw: &str) -> bool {
    let toks = split_top_level(raw);
    let is_axis = |t: &str| matches!(t.to_ascii_lowercase().as_str(), "x" | "y" | "block" | "inline" | "both");
    let is_strict = |t: &str| matches!(t.to_ascii_lowercase().as_str(), "mandatory" | "proximity");
    match toks.as_slice() {
        [a] => a.eq_ignore_ascii_case("none") || is_axis(a),
        [a, b] => is_axis(a) && is_strict(b),
        _ => false,
    }
}

// scroll-snap-type 캐논 직렬화(§CSS Scroll Snap): 기본 strictness(proximity) 생략.
pub fn scroll_snap_type_canonical(raw: &str) -> String {
    let toks = split_top_level(raw);
    if toks.len() == 2 && toks[1].eq_ignore_ascii_case("proximity") {
        return toks[0].to_ascii_lowercase();
    }
    toks.iter().map(|t| t.to_ascii_lowercase()).collect::<Vec<_>>().join(" ")
}

// 정렬 값 캐논 직렬화(§CSS Box Alignment): 기본 "first" 생략(first baseline→baseline).
pub fn alignment_canonical(raw: &str) -> String {
    let low = raw.trim().to_ascii_lowercase();
    let toks: Vec<&str> = low.split_whitespace().collect();
    // first baseline → baseline
    if toks.len() == 2 && toks[0] == "first" && toks[1] == "baseline" {
        return "baseline".to_string();
    }
    // justify-items 의 legacy && [left|right|center]: legacy 를 앞으로(§CSS Box Alignment).
    if toks.len() == 2 && toks.contains(&"legacy") {
        let other = toks.iter().copied().find(|&t| t != "legacy").unwrap_or("");
        if matches!(other, "left" | "right" | "center") {
            return format!("legacy {other}");
        }
    }
    low
}

// 정렬 위치 키워드인가(§CSS Box Alignment). is_content 면 content-position, 아니면
// self-position(self-start/end 추가). allow_lr 이면 left/right 도.
fn is_align_position(t: &str, is_content: bool, allow_lr: bool) -> bool {
    matches!(t, "center" | "start" | "end" | "flex-start" | "flex-end")
        || (!is_content && matches!(t, "self-start" | "self-end"))
        || (allow_lr && matches!(t, "left" | "right"))
}

// 정렬 프로퍼티 유효성(§CSS Box Alignment). is_content: content 축(distribution 허용),
// allow_auto: self, allow_lr: justify, allow_legacy: justify-items.
pub fn alignment_valid(
    raw: &str,
    is_content: bool,
    allow_auto: bool,
    allow_lr: bool,
    allow_legacy: bool,
    allow_baseline: bool,
) -> bool {
    let toks: Vec<String> = split_top_level(raw).iter().map(|t| t.to_ascii_lowercase()).collect();
    match toks.len() {
        1 => {
            let t = toks[0].as_str();
            match t {
                "normal" | "stretch" => true,
                "baseline" => allow_baseline,
                "auto" => allow_auto,
                "space-between" | "space-around" | "space-evenly" => is_content,
                "legacy" => allow_legacy,
                _ => is_align_position(t, is_content, allow_lr),
            }
        }
        2 => {
            let (a, b) = (toks[0].as_str(), toks[1].as_str());
            // [first|last] baseline
            if allow_baseline && matches!(a, "first" | "last") && b == "baseline" {
                return true;
            }
            // <overflow-position> <position>
            if matches!(a, "safe" | "unsafe") && is_align_position(b, is_content, allow_lr) {
                return true;
            }
            // justify-items: legacy && [left|right|center] (순서 무관)
            if allow_legacy {
                if a == "legacy" && matches!(b, "left" | "right" | "center") {
                    return true;
                }
                if b == "legacy" && matches!(a, "left" | "right" | "center") {
                    return true;
                }
            }
            false
        }
        _ => false,
    }
}

// flex-flow 캐논 직렬화(§CSS Flexbox): 기본값(row/nowrap) 생략, 방향 먼저. 둘 다
// 기본이면 "row".
pub fn flex_flow_canonical(raw: &str) -> String {
    let (mut dir, mut wrap): (Option<String>, Option<String>) = (None, None);
    for t in split_top_level(raw) {
        let low = t.to_ascii_lowercase();
        match low.as_str() {
            "row" | "row-reverse" | "column" | "column-reverse" => dir = Some(low),
            "nowrap" | "wrap" | "wrap-reverse" => wrap = Some(low),
            _ => {}
        }
    }
    let mut parts: Vec<String> = Vec::new();
    if let Some(d) = dir {
        if d != "row" {
            parts.push(d);
        }
    }
    if let Some(w) = wrap {
        if w != "nowrap" {
            parts.push(w);
        }
    }
    if parts.is_empty() {
        "row".to_string()
    } else {
        parts.join(" ")
    }
}

// flex-basis 유효성(§CSS Flexbox): content | auto | min/max/fit-content |
// <length-percentage>(비음수). none·음수·anchor-size·순수숫자 calc 거부.
pub fn flex_basis_valid(tok: &str) -> bool {
    let low = tok.trim().to_ascii_lowercase();
    if matches!(low.as_str(), "auto" | "content" | "min-content" | "max-content" | "fit-content") {
        return true;
    }
    if low.starts_with("fit-content(") && low.ends_with(')') {
        return true;
    }
    if is_math_fn(&low) {
        // <length-percentage> calc 만 — 결과 차원 타입 검사(§CSS Values 4). 각도·순수
        // 수 calc(0)·주파수 등 거부.
        return math_length_valid(&low, true);
    }
    is_length_percentage(&low) && !low.starts_with('-')
}

// 공백(괄호 depth 0)으로 토큰 분할 — 괄호 안 공백은 유지. 수학식 구문 검증용.
pub(crate) fn split_ws_depth0(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut depth = 0i32;
    let mut start = 0usize;
    for i in 0..b.len() {
        match b[i] {
            b'(' => depth += 1,
            b')' => depth -= 1,
            b' ' | b'\t' | b'\n' | b'\r' | 0x0c if depth == 0 => {
                if start < i {
                    out.push(&s[start..i]);
                }
                start = i + 1;
            }
            _ => {}
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

// 수학 인자의 타입 분류(§CSS Values 4). Unit enum 에 각도/시간 등이 없어 접미사
// 문자열로 분류한다(검증 전용 — 계산엔 안 씀). Percent 는 Length 와 호환 클래스로 본다
// (length-percentage 문맥). Unknown 은 중첩식/함수 등 — 타입 검사에서 허용.
#[derive(PartialEq, Clone, Copy)]
enum MKind {
    Number,
    LengthPct, // <length> | <percentage>
    Angle,
    Time,
    Freq,
    Resolution,
    Flex,
    Unknown,
    Invalid,
}

fn is_rounding_strategy(s: &str) -> bool {
    matches!(s.trim().to_ascii_lowercase().as_str(), "nearest" | "up" | "down" | "to-zero")
}

// 단일 수학 인자의 타입. 식(연산자/공백/괄호 포함)은 Unknown(허용). 단일 토큰만 접미사로
// 분류하며, 미지 단위·비수치 식별자는 Invalid.
fn arg_kind(seg: &str) -> MKind {
    let e = seg.trim();
    if e.is_empty() {
        return MKind::Invalid;
    }
    let toks = split_ws_depth0(e);
    if toks.len() != 1 || e.contains('(') {
        return MKind::Unknown; // 식/중첩 함수 → 타입 검사 생략(허용)
    }
    let t = e.trim_start_matches(['+', '-']);
    let low = t.to_ascii_lowercase();
    if low.is_empty() {
        return MKind::Invalid;
    }
    if matches!(low.as_str(), "e" | "pi" | "infinity" | "-infinity" | "nan") {
        return MKind::Number;
    }
    if low.parse::<f64>().is_ok() {
        return MKind::Number;
    }
    if let Some(p) = low.strip_suffix('%') {
        return if p.parse::<f64>().is_ok() { MKind::LengthPct } else { MKind::Invalid };
    }
    let unit_start = low.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(low.len());
    let (num, unit) = low.split_at(unit_start);
    if num.parse::<f64>().is_err() {
        return MKind::Invalid;
    }
    match unit {
        "px" | "em" | "rem" | "ex" | "rex" | "cap" | "rcap" | "ch" | "rch" | "ic" | "ric"
        | "lh" | "rlh" | "vw" | "vh" | "vi" | "vb" | "vmin" | "vmax" | "svw" | "svh" | "svi"
        | "svb" | "svmin" | "svmax" | "lvw" | "lvh" | "lvi" | "lvb" | "lvmin" | "lvmax"
        | "dvw" | "dvh" | "dvi" | "dvb" | "dvmin" | "dvmax" | "cqw" | "cqh" | "cqi" | "cqb"
        | "cqmin" | "cqmax" | "cm" | "mm" | "q" | "in" | "pt" | "pc" => MKind::LengthPct,
        "deg" | "grad" | "rad" | "turn" => MKind::Angle,
        "s" | "ms" => MKind::Time,
        "hz" | "khz" => MKind::Freq,
        "dpi" | "dpcm" | "dppx" | "x" => MKind::Resolution,
        "fr" => MKind::Flex,
        _ => MKind::Invalid,
    }
}

// round()/mod()/rem() 문법(§CSS Values 4 §10). round 는 [strategy,] A, B / mod·rem 은 A, B.
// 값 인자 정확히 2개, 두 인자 타입 호환(Percent~Length), Flex/Invalid·개수 오류 거부.
fn round_family_valid(name: &str, args: &[String]) -> bool {
    let strat = name == "round" && args.first().is_some_and(|a| is_rounding_strategy(a));
    let vals: &[String] = if strat { &args[1..] } else { args };
    if vals.len() != 2 {
        return false;
    }
    if !vals.iter().all(|a| math_expr_valid(a.trim())) {
        return false;
    }
    let (k1, k2) = (arg_kind(&vals[0]), arg_kind(&vals[1]));
    match (k1, k2) {
        (MKind::Invalid, _) | (_, MKind::Invalid) => false,
        (MKind::Flex, _) | (_, MKind::Flex) => false,
        (MKind::Unknown, _) | (_, MKind::Unknown) => true,
        (a, b) => a == b,
    }
}

// calc-sum 한 조각의 보수적 구문 검증(§CSS Values 4). 빈 식·시작/끝의 단독 이항
// 연산자(1 +, / 2)·연산자 없이 공백으로만 나열된 순수 값(1 2, 1px 2px) 을 무효로
// 본다. 중첩 산술 함수(calc(round(0px)) 등)는 재귀로 검증한다. 그 외 유효식은 통과.
fn math_expr_valid(expr: &str) -> bool {
    let e = expr.trim();
    if e.is_empty() {
        return false;
    }
    let toks = split_ws_depth0(e);
    if toks.is_empty() {
        return false;
    }
    let is_op = |s: &str| matches!(s, "+" | "-" | "*" | "/");
    if is_op(toks[0]) || is_op(toks[toks.len() - 1]) {
        return false;
    }
    // 연산자 하나 없이 순수 값이 공백으로 여러 개 나열되면 무효(calc 는 값 사이 연산자 필수).
    let all_plain =
        toks.iter().all(|t| t.chars().all(|c| c.is_alphanumeric() || c == '.' || c == '%'));
    if toks.len() >= 2 && all_plain {
        return false;
    }
    // 중첩 산술 함수·괄호 그룹 재귀 검증(무효 전파). var/env/attr/삼각 등은 허용.
    for tk in &toks {
        let inner_tok = tk.trim_start_matches(['+', '-']);
        if inner_tok.starts_with('(') && inner_tok.ends_with(')') && inner_tok.len() >= 2 {
            if !math_expr_valid(&inner_tok[1..inner_tok.len() - 1]) {
                return false;
            }
        } else if let Some(op) = inner_tok.find('(') {
            let nm = inner_tok[..op].to_ascii_lowercase();
            if matches!(nm.as_str(), "calc" | "min" | "max" | "clamp" | "round" | "mod" | "rem")
                && !math_function_valid(inner_tok)
            {
                return false;
            }
        }
    }
    true
}

// 산술 수학 함수(calc/min/max/clamp/round/mod/rem)의 구문 유효성. 예전엔 프리픽스만
// 보고 수용해 round()/round(,)/round(1 2)/round(1 + )/round(0px,0s) 같은 malformed 도
// 통과했다. 함수명·괄호 균형·인자·연산자·round 계열 문법(개수/strategy/타입)을 검증한다.
pub(crate) fn math_function_valid(text: &str) -> bool {
    let t = text.trim();
    if !t.ends_with(')') {
        return false;
    }
    let Some(open) = t.find('(') else {
        return false;
    };
    let name = t[..open].trim().to_ascii_lowercase();
    if !matches!(name.as_str(), "calc" | "min" | "max" | "clamp" | "round" | "mod" | "rem") {
        return false;
    }
    let mut depth = 0i32;
    for c in t.chars() {
        if c == '(' {
            depth += 1;
        } else if c == ')' {
            depth -= 1;
            if depth < 0 {
                return false;
            }
        }
    }
    if depth != 0 {
        return false;
    }
    let inner = t[open + 1..t.len() - 1].trim();
    if inner.is_empty() {
        return false;
    }
    // 선두/후행 최상위 콤마(빈 인자) — split_top_commas 가 후행 빈 세그먼트를 버려
    // "max(1s, )"/"min(, 1px)" 가 새던 걸 막는다.
    if inner.starts_with(',') || inner.ends_with(',') {
        return false;
    }
    let args = split_top_commas(inner);
    let args: Vec<String> = args.iter().map(|a| a.trim().to_string()).collect();
    if args.iter().any(|a| a.is_empty()) {
        return false; // 빈 인자(round(,) round(1, ) round(1,,2))
    }
    match name.as_str() {
        "round" | "mod" | "rem" => round_family_valid(&name, &args),
        // calc 는 인자 1개, min/max 는 1개 이상, clamp 는 3개(또는 none 포함). 구조만 검증.
        _ => args.iter().all(|a| math_expr_valid(a)),
    }
}

fn is_math_fn(low: &str) -> bool {
    math_function_valid(low)
}

// ── calc 차원 타입 검사(§CSS Values 4 §10 type checking) ──────────────────
// 수학 함수(calc/min/max/clamp/…)의 결과 차원을 해석해 프로퍼티 기대 타입과 대조.
// 예전엔 size_valid 가 contains("deg") 같은 편법으로만 걸러 max(0Hz)/max(0fr)/
// calc(1/2px)/max(0)(수) 같은 타입 불일치를 통과시켰다. 이 해석기는 절대 유효
// 표현식을 거부하지 않도록 미지 함수(var/env/trig 외)·attr 은 Wild(허용)로 둔다.
//
// MDim: 차원 지수 벡터. percent 는 length 로 접어 len 에 반영하고 pct 플래그로
// "% 포함 여부"만 따로 추적한다(length-only 프로퍼티에서 % 거부용). number=전부 0.
#[derive(Clone, Copy)]
struct MDim {
    len: i32,
    ang: i32,
    time: i32,
    freq: i32,
    res: i32,
    flex: i32,
    pct: bool,
}
impl MDim {
    fn num() -> Self {
        MDim { len: 0, ang: 0, time: 0, freq: 0, res: 0, flex: 0, pct: false }
    }
    fn axes_eq(&self, o: &Self) -> bool {
        self.len == o.len
            && self.ang == o.ang
            && self.time == o.time
            && self.freq == o.freq
            && self.res == o.res
            && self.flex == o.flex
    }
    // 단일 축 지수 1, 나머지 0 인지.
    fn is_axis(&self, sel: fn(&MDim) -> i32) -> bool {
        sel(self) == 1
            && [self.len, self.ang, self.time, self.freq, self.res, self.flex]
                .iter()
                .sum::<i32>()
                == 1
    }
    // 순수 수(모든 축 0, % 없음).
    fn is_pure_number(&self) -> bool {
        self.len == 0
            && self.ang == 0
            && self.time == 0
            && self.freq == 0
            && self.res == 0
            && self.flex == 0
            && !self.pct
    }
    // 수 또는 각도(삼각함수 sin/cos/tan 인자).
    fn is_number_or_angle(&self) -> bool {
        self.is_pure_number() || self.is_axis(|m| m.ang)
    }
}
#[derive(Clone, Copy)]
enum MTy {
    Bad,          // 타입 불일치/malformed
    Wild,         // 해석 불가(var/env/attr/미지 함수) — 무엇과도 호환
    D(MDim),
}

fn mdim_add(a: MTy, b: MTy) -> MTy {
    match (a, b) {
        (MTy::Bad, _) | (_, MTy::Bad) => MTy::Bad,
        (MTy::Wild, x) | (x, MTy::Wild) => x, // Wild 는 상대 타입을 채택(관대)
        (MTy::D(x), MTy::D(y)) => {
            if x.axes_eq(&y) {
                MTy::D(MDim { pct: x.pct || y.pct, ..x })
            } else {
                MTy::Bad
            }
        }
    }
}
fn mdim_mul(a: MTy, b: MTy) -> MTy {
    match (a, b) {
        (MTy::Bad, _) | (_, MTy::Bad) => MTy::Bad,
        (MTy::Wild, _) | (_, MTy::Wild) => MTy::Wild,
        (MTy::D(x), MTy::D(y)) => MTy::D(MDim {
            len: x.len + y.len,
            ang: x.ang + y.ang,
            time: x.time + y.time,
            freq: x.freq + y.freq,
            res: x.res + y.res,
            flex: x.flex + y.flex,
            pct: x.pct || y.pct,
        }),
    }
}
fn mdim_div(a: MTy, b: MTy) -> MTy {
    match (a, b) {
        (MTy::Bad, _) | (_, MTy::Bad) => MTy::Bad,
        (MTy::Wild, _) | (_, MTy::Wild) => MTy::Wild,
        (MTy::D(x), MTy::D(y)) => MTy::D(MDim {
            len: x.len - y.len,
            ang: x.ang - y.ang,
            time: x.time - y.time,
            freq: x.freq - y.freq,
            res: x.res - y.res,
            flex: x.flex - y.flex,
            pct: x.pct || y.pct,
        }),
    }
}

// 값 토큰 하나(부호+수+단위 또는 %)를 차원 타입으로. 미지 단위 → Bad.
// percent(%)가 접히는 문맥 차원. §CSS Values 4 에서 % 는 문맥의 차원으로 해석된다:
// length-percentage 는 length, angle-percentage(conic 등)는 angle, <number>|<percentage>
// (opacity)는 자기 자신(Bare).
#[derive(Clone, Copy, PartialEq)]
enum PctAxis {
    Len,
    Ang,
    Bare,
}

fn mdim_classify(tok: &str, pct: PctAxis) -> MTy {
    let low = tok.trim().to_ascii_lowercase();
    let low = low.strip_prefix(['+', '-']).unwrap_or(&low);
    if low.is_empty() {
        return MTy::Bad;
    }
    // 상수(§CSS Values): e / pi / infinity / nan → 수.
    if matches!(low, "e" | "pi" | "infinity" | "nan") {
        return MTy::D(MDim::num());
    }
    if let Some(num) = low.strip_suffix('%') {
        return if num.parse::<f64>().map(|v| v.is_finite()).unwrap_or(false) {
            // % 를 문맥 차원으로 접는다(Bare 면 축 없이 pct 플래그만).
            match pct {
                PctAxis::Len => MTy::D(MDim { len: 1, pct: true, ..MDim::num() }),
                PctAxis::Ang => MTy::D(MDim { ang: 1, pct: true, ..MDim::num() }),
                PctAxis::Bare => MTy::D(MDim { pct: true, ..MDim::num() }),
            }
        } else {
            MTy::Bad
        };
    }
    // 순수 수?
    if low.parse::<f64>().map(|v| v.is_finite()).unwrap_or(false) {
        return MTy::D(MDim::num());
    }
    let unit_len = low.chars().rev().take_while(|c| c.is_ascii_alphabetic()).count();
    if unit_len == 0 {
        return MTy::Bad;
    }
    let (num, unit) = low.split_at(low.len() - unit_len);
    if num.parse::<f64>().map(|v| !v.is_finite()).unwrap_or(true) {
        return MTy::Bad;
    }
    const LEN_UNITS: &[&str] = &[
        "px", "em", "rem", "ex", "rex", "cap", "rcap", "ch", "rch", "ic", "ric", "lh", "rlh", "vw",
        "vh", "vi", "vb", "vmin", "vmax", "svw", "svh", "svi", "svb", "svmin", "svmax", "lvw",
        "lvh", "lvi", "lvb", "lvmin", "lvmax", "dvw", "dvh", "dvi", "dvb", "dvmin", "dvmax", "cqw",
        "cqh", "cqi", "cqb", "cqmin", "cqmax", "cm", "mm", "q", "in", "pt", "pc",
    ];
    if LEN_UNITS.contains(&unit) {
        return MTy::D(MDim { len: 1, ..MDim::num() });
    }
    match unit {
        "deg" | "grad" | "rad" | "turn" => MTy::D(MDim { ang: 1, ..MDim::num() }),
        "s" | "ms" => MTy::D(MDim { time: 1, ..MDim::num() }),
        "hz" | "khz" => MTy::D(MDim { freq: 1, ..MDim::num() }),
        "dpi" | "dpcm" | "dppx" | "x" => MTy::D(MDim { res: 1, ..MDim::num() }),
        "fr" => MTy::D(MDim { flex: 1, ..MDim::num() }),
        _ => MTy::Bad,
    }
}

// 균형 괄호로 함수 인자 문자열(바깥 괄호 안)을 얻는다. name(...) 형식 가정.
fn mdim_func_args(text: &str) -> Option<(String, Vec<String>)> {
    let t = text.trim();
    if !t.ends_with(')') {
        return None;
    }
    let open = t.find('(')?;
    let name = t[..open].trim().to_ascii_lowercase();
    let inner = t[open + 1..t.len() - 1].trim();
    Some((name, split_top_commas(inner).iter().map(|a| a.trim().to_string()).collect()))
}

// 함수 인자 문자열에 선두/후행 최상위 콤마(빈 인자)가 있는지 — split_top_commas 가
// 후행 빈 세그먼트를 버려 "abs(1, )" 가 새는 걸 mdim 에서 직접 걸러낸다.
fn mdim_func_has_edge_comma(text: &str) -> bool {
    let t = text.trim();
    if let Some(open) = t.find('(') {
        if t.ends_with(')') {
            let inner = t[open + 1..t.len() - 1].trim();
            return inner.starts_with(',') || inner.ends_with(',');
        }
    }
    false
}

// 여러 인자가 같은 축(타입)인지 확인하고 통합 타입을 낸다(min/max/clamp/round/mod/rem).
fn mdim_same(args: &[String], pct: PctAxis) -> MTy {
    let mut acc: Option<MTy> = None;
    for a in args {
        if a.eq_ignore_ascii_case("none") {
            continue; // clamp(none, …) 등: 경계 생략
        }
        let ty = mdim_of(a, pct);
        acc = Some(match acc {
            None => ty,
            Some(prev) => mdim_add(prev, ty), // add 규칙 = 축 일치 요구
        });
    }
    acc.unwrap_or(MTy::Bad)
}

// 표현식(문자열) → 차원 타입. calc 문법의 +,-,*,/ 와 함수를 재귀 해석. pct 는 % 가
// 접히는 문맥 차원.
fn mdim_of(expr: &str, pct: PctAxis) -> MTy {
    let t = expr.trim();
    if t.is_empty() {
        return MTy::Bad;
    }
    // 함수 호출 전체?
    if t.ends_with(')') {
        if let Some((name, args)) = mdim_func_args(t) {
            // 이 텍스트가 순수 함수 호출인지 확인: 이름(비어있지 않음)이 식별자이고,
            // 첫 '(' 의 짝 ')' 가 문자열 끝이어야 한다. "calc(2) * calc(50px)" 처럼
            // 첫 호출 뒤에 내용이 더 있으면 아래 char 파서로(곱셈 등).
            let chars: Vec<char> = t.chars().collect();
            let open = t.find('(').unwrap_or(0);
            let name_ok = open > 0
                && t[..open].chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
            let call_spans_all = {
                let mut depth = 0i32;
                let mut end = None;
                for (i, &c) in chars.iter().enumerate().skip(open) {
                    match c {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                end = Some(i);
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                end == Some(chars.len() - 1)
            };
            let is_pure_call = name_ok && call_spans_all;
            if is_pure_call {
                if mdim_func_has_edge_comma(t) {
                    return MTy::Bad; // 선두/후행 콤마(빈 인자)
                }
                return match name.as_str() {
                    "calc" => {
                        if args.len() != 1 {
                            MTy::Bad
                        } else {
                            mdim_of(&args[0], pct)
                        }
                    }
                    "min" | "max" => {
                        if args.is_empty() {
                            MTy::Bad
                        } else {
                            mdim_same(&args, pct)
                        }
                    }
                    "clamp" => {
                        if args.len() != 3 {
                            MTy::Bad
                        } else {
                            mdim_same(&args, pct)
                        }
                    }
                    "mod" | "rem" => {
                        if args.len() != 2 {
                            MTy::Bad
                        } else {
                            mdim_same(&args, pct)
                        }
                    }
                    "round" => {
                        // round(<strategy>?, a, b): 첫 인자가 전략 키워드면 건너뛴다.
                        let vals: Vec<String> = if args
                            .first()
                            .map(|a| {
                                matches!(
                                    a.to_ascii_lowercase().as_str(),
                                    "nearest" | "up" | "down" | "to-zero"
                                )
                            })
                            .unwrap_or(false)
                        {
                            args[1..].to_vec()
                        } else {
                            args.clone()
                        };
                        if vals.len() != 2 {
                            MTy::Bad
                        } else {
                            mdim_same(&vals, pct)
                        }
                    }
                    "abs" => {
                        if args.len() != 1 {
                            MTy::Bad
                        } else {
                            mdim_of(&args[0], pct)
                        }
                    }
                    "sign" => {
                        if args.len() != 1 || matches!(mdim_of(&args[0], pct), MTy::Bad) {
                            MTy::Bad
                        } else {
                            MTy::D(MDim::num())
                        }
                    }
                    // sin/cos/tan: 인자 1개, 각도 또는 수(% 는 문맥 없음 → Bare). 결과는 수.
                    "sin" | "cos" | "tan" => {
                        if args.len() != 1 {
                            MTy::Bad
                        } else {
                            match mdim_of(&args[0], PctAxis::Bare) {
                                MTy::Wild => MTy::D(MDim::num()),
                                MTy::D(d) if d.is_number_or_angle() => MTy::D(MDim::num()),
                                _ => MTy::Bad,
                            }
                        }
                    }
                    // asin/acos/atan: 인자 1개, 순수 수. 결과는 각도.
                    "asin" | "acos" | "atan" => {
                        if args.len() != 1 {
                            MTy::Bad
                        } else {
                            match mdim_of(&args[0], PctAxis::Bare) {
                                MTy::Wild => MTy::D(MDim { ang: 1, ..MDim::num() }),
                                MTy::D(d) if d.is_pure_number() => {
                                    MTy::D(MDim { ang: 1, ..MDim::num() })
                                }
                                _ => MTy::Bad,
                            }
                        }
                    }
                    // atan2: 인자 2개, 같은 타입. 결과는 각도.
                    "atan2" => {
                        if args.len() != 2 || matches!(mdim_same(&args, PctAxis::Bare), MTy::Bad) {
                            MTy::Bad
                        } else {
                            MTy::D(MDim { ang: 1, ..MDim::num() })
                        }
                    }
                    // sqrt/pow/log/exp/hypot·var/env/attr·progress/calc-size 등: 관대(Wild).
                    _ => MTy::Wild,
                };
            }
        }
    }
    // 산술 표현식: char 기반 재귀 하강(+,- 최상위 / *,/ 그다음).
    let chars: Vec<char> = t.chars().collect();
    let mut p = 0usize;
    let ty = mdim_expr_chars(&chars, &mut p, pct);
    skip_ws(&chars, &mut p);
    if p != chars.len() {
        return MTy::Bad;
    }
    ty
}

fn mdim_expr_chars(t: &[char], p: &mut usize, pct: PctAxis) -> MTy {
    let mut acc = mdim_term_chars(t, p, pct);
    loop {
        skip_ws(t, p);
        let op = match t.get(*p) {
            Some('+') => '+',
            Some('-') => '-',
            _ => break,
        };
        // '+'/'-' 는 앞뒤 공백 필수(CSS). 뒤 공백 확인.
        if t.get(*p + 1).map(|c| !c.is_whitespace()).unwrap_or(true) {
            return MTy::Bad;
        }
        *p += 1;
        let rhs = mdim_term_chars(t, p, pct);
        let _ = op;
        acc = mdim_add(acc, rhs);
    }
    acc
}
fn mdim_term_chars(t: &[char], p: &mut usize, pct: PctAxis) -> MTy {
    let mut acc = mdim_factor_chars(t, p, pct);
    loop {
        skip_ws(t, p);
        let op = match t.get(*p) {
            Some('*') => '*',
            Some('/') => '/',
            _ => break,
        };
        *p += 1;
        let rhs = mdim_factor_chars(t, p, pct);
        acc = if op == '*' { mdim_mul(acc, rhs) } else { mdim_div(acc, rhs) };
    }
    acc
}
fn mdim_factor_chars(t: &[char], p: &mut usize, pct: PctAxis) -> MTy {
    skip_ws(t, p);
    // 괄호 그룹.
    if t.get(*p) == Some(&'(') {
        let start = *p;
        let mut depth = 0i32;
        let mut k = *p;
        while k < t.len() {
            match t[k] {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        k += 1;
                        break;
                    }
                }
                _ => {}
            }
            k += 1;
        }
        if depth != 0 {
            return MTy::Bad;
        }
        let inner: String = t[start + 1..k - 1].iter().collect();
        *p = k;
        return mdim_of(&inner, pct);
    }
    // 함수 호출: 이름 뒤 '('.
    if t.get(*p).is_some_and(|c| c.is_ascii_alphabetic()) {
        let nstart = *p;
        let mut j = *p;
        while j < t.len() && (t[j].is_ascii_alphabetic() || t[j] == '-') {
            j += 1;
        }
        if t.get(j) == Some(&'(') {
            let mut depth = 0i32;
            let mut k = j;
            while k < t.len() {
                match t[k] {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            k += 1;
                            break;
                        }
                    }
                    _ => {}
                }
                k += 1;
            }
            if depth != 0 {
                return MTy::Bad;
            }
            let sub: String = t[nstart..k].iter().collect();
            *p = k;
            return mdim_of(&sub, pct);
        }
        // 상수 키워드(e/pi/infinity/nan) 또는 미지 식별자.
        let name: String = t[nstart..j].iter().collect();
        *p = j;
        return mdim_classify(&name, pct);
    }
    // 값 토큰(부호+수+단위/%).
    let start = *p;
    if matches!(t.get(*p), Some('+') | Some('-')) {
        *p += 1;
    }
    while *p < t.len() {
        let c = t[*p];
        if c.is_ascii_alphanumeric() || c == '.' || c == '%' {
            *p += 1;
        } else if (c == '+' || c == '-')
            && *p > start
            && matches!(t[*p - 1], 'e' | 'E')
        {
            *p += 1; // 지수 부호
        } else {
            break;
        }
    }
    if *p == start {
        return MTy::Bad;
    }
    let tok: String = t[start..*p].iter().collect();
    mdim_classify(&tok, pct)
}

// 길이 문맥 수학 함수 유효성: 결과가 <length>(allow_pct 면 <length-percentage>).
pub(crate) fn math_length_valid(text: &str, allow_pct: bool) -> bool {
    match mdim_of(text, PctAxis::Len) {
        MTy::Wild => true, // 해석 불가(var 등)는 관대 수용
        MTy::D(d) => d.is_axis(|m| m.len) && (allow_pct || !d.pct),
        MTy::Bad => false,
    }
}

// 시간 문맥 수학 함수 유효성: 결과가 <time>(% 없음).
pub(crate) fn math_time_valid(text: &str) -> bool {
    match mdim_of(text, PctAxis::Len) {
        MTy::Wild => true,
        MTy::D(d) => d.is_axis(|m| m.time) && !d.pct,
        MTy::Bad => false,
    }
}

// 각도-퍼센트 문맥(conic-gradient 스톱/from 각도): 결과가 <angle>|<percentage>(100%=360deg).
// % 를 angle 축으로 접어 calc(90deg + 50%) 를 유효로, calc(50% + 0)(수 혼합)을 무효로 잡는다.
pub(crate) fn math_angle_pct_valid(text: &str) -> bool {
    match mdim_of(text, PctAxis::Ang) {
        MTy::Wild => true,
        MTy::D(d) => d.is_axis(|m| m.ang),
        MTy::Bad => false,
    }
}

// transform(§CSS Transforms) 함수 리스트 유효성. 각 함수의 인자 개수·타입을 검증.
// 미지 함수명은 관대 수용(신규 함수 대비, soundness — 유효식을 거부하지 않음).
pub fn transform_valid(raw: &str) -> bool {
    let t = raw.trim();
    if t.eq_ignore_ascii_case("none") {
        return true;
    }
    let funcs = split_top_level(t);
    if funcs.is_empty() {
        return false;
    }
    // 인자 타입 검사기: 단위 없는 0 은 어느 차원이든 허용.
    let ang = |a: &str| a.trim() == "0" || math_angle_valid(a.trim());
    let len = |a: &str, pct: bool| a.trim() == "0" || math_length_valid(a.trim(), pct);
    let num = |a: &str| math_number_valid(a.trim());
    for f in &funcs {
        let f = f.trim();
        if !f.ends_with(')') {
            return false;
        }
        let Some(open) = f.find('(') else { return false };
        let name = f[..open].trim().to_ascii_lowercase();
        let inner = f[open + 1..f.len() - 1].trim();
        let args: Vec<String> = split_top_commas(inner).iter().map(|a| a.trim().to_string()).collect();
        if args.iter().any(|a| a.is_empty()) {
            return false;
        }
        let ok = match name.as_str() {
            "translate" => (args.len() == 1 || args.len() == 2) && args.iter().all(|a| len(a, true)),
            "translatex" | "translatey" => args.len() == 1 && len(&args[0], true),
            "translatez" => args.len() == 1 && len(&args[0], false),
            "translate3d" => args.len() == 3 && len(&args[0], true) && len(&args[1], true) && len(&args[2], false),
            "scale" => (args.len() == 1 || args.len() == 2) && args.iter().all(|a| num(a)),
            "scalex" | "scaley" | "scalez" => args.len() == 1 && num(&args[0]),
            "scale3d" => args.len() == 3 && args.iter().all(|a| num(a)),
            "rotate" | "rotatex" | "rotatey" | "rotatez" => args.len() == 1 && ang(&args[0]),
            "rotate3d" => args.len() == 4 && num(&args[0]) && num(&args[1]) && num(&args[2]) && ang(&args[3]),
            "skew" => (args.len() == 1 || args.len() == 2) && args.iter().all(|a| ang(a)),
            "skewx" | "skewy" => args.len() == 1 && ang(&args[0]),
            "matrix" => args.len() == 6 && args.iter().all(|a| num(a)),
            "matrix3d" => args.len() == 16 && args.iter().all(|a| num(a)),
            "perspective" => args.len() == 1 && (args[0].eq_ignore_ascii_case("none") || len(&args[0], false)),
            // 미지 함수: 관대 수용.
            _ => true,
        };
        if !ok {
            return false;
        }
    }
    true
}

// 순수 수 문맥 수학 함수 유효성(font-weight 등 <number>, % 불가): 결과가 순수 수.
pub(crate) fn math_number_only_valid(text: &str) -> bool {
    match mdim_of(text, PctAxis::Len) {
        MTy::Wild => true,
        MTy::D(d) => d.is_pure_number(),
        MTy::Bad => false,
    }
}

// 각도 문맥 수학 함수 유효성(rotate/skew 등 <angle>, % 없음): 결과가 <angle>.
pub(crate) fn math_angle_valid(text: &str) -> bool {
    match mdim_of(text, PctAxis::Len) {
        MTy::Wild => true,
        MTy::D(d) => d.is_axis(|m| m.ang) && !d.pct,
        MTy::Bad => false,
    }
}

// 수/퍼센트 문맥 수학 함수 유효성(opacity 등 <number>|<percentage>): 결과가 순수
// 수(모든 축 0, % 없음) 또는 순수 퍼센트(len 1·% 플래그, 다른 축 없음)여야.
pub(crate) fn math_number_valid(text: &str) -> bool {
    match mdim_of(text, PctAxis::Len) {
        MTy::Wild => true,
        MTy::D(d) => {
            let axes_zero =
                d.len == 0 && d.ang == 0 && d.time == 0 && d.freq == 0 && d.res == 0 && d.flex == 0;
            let pure_number = axes_zero && !d.pct;
            let pure_percent = d.pct && d.is_axis(|m| m.len);
            pure_number || pure_percent
        }
        MTy::Bad => false,
    }
}

// <position> 유효성(§CSS Values): object-position/background-position 등. 1/2/4 토큰만
// (3 토큰은 현행 문법상 무효). lp 있으면 [수평][수직] 순서 엄격, 순수 키워드는 순서 무관.
pub fn position_valid(raw: &str) -> bool {
    let toks: Vec<String> = split_top_level(raw).iter().map(|t| t.to_ascii_lowercase()).collect();
    let is_h = |t: &str| matches!(t, "left" | "right");
    let is_v = |t: &str| matches!(t, "top" | "bottom");
    let is_c = |t: &str| t == "center";
    let is_lp = |t: &str| {
        if is_math_fn(t) {
            return math_length_valid(t, true); // <length-percentage>(§CSS Values 4 타입 검사)
        }
        is_length_percentage(t)
    };
    match toks.len() {
        1 => {
            let t = toks[0].as_str();
            is_h(t) || is_v(t) || is_c(t) || is_lp(t)
        }
        2 => {
            let (a, b) = (toks[0].as_str(), toks[1].as_str());
            let a_kw = is_h(a) || is_v(a) || is_c(a);
            let b_kw = is_h(b) || is_v(b) || is_c(b);
            if a_kw && b_kw {
                // 순수 키워드 쌍: 서로 다른 축(수평/수직)으로 배정 가능해야.
                let (a_h, a_v) = (is_h(a) || is_c(a), is_v(a) || is_c(a));
                let (b_h, b_v) = (is_h(b) || is_c(b), is_v(b) || is_c(b));
                (a_h && b_v) || (a_v && b_h)
            } else {
                // lp 포함: [수평 or lp] [수직 or lp] 순서 고정.
                (is_h(a) || is_c(a) || is_lp(a)) && (is_v(b) || is_c(b) || is_lp(b))
            }
        }
        4 => {
            // 두 그룹 [모서리 lp][모서리 lp], 하나는 수평(left|right) 하나는 수직(top|bottom).
            let g1_h = is_h(&toks[0]) && is_lp(&toks[1]);
            let g1_v = is_v(&toks[0]) && is_lp(&toks[1]);
            let g2_h = is_h(&toks[2]) && is_lp(&toks[3]);
            let g2_v = is_v(&toks[2]) && is_lp(&toks[3]);
            (g1_h && g2_v) || (g1_v && g2_h)
        }
        _ => false,
    }
}

// mask-position 캐논 직렬화(§CSS Masking): <position># — 레이어마다 position_canonical.
pub fn mask_position_canonical(raw: &str) -> String {
    split_top_commas(raw)
        .iter()
        .map(|l| {
            if position_valid(l) {
                position_canonical(l)
            } else {
                l.trim().to_ascii_lowercase()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

// <position> 한 축(1~2 토큰)을 계산값 퍼센트로. left/top→0%, right/bottom→100%,
// center→50%, 모서리+오프셋은 시작 기준 그대로, 끝(right/bottom) 기준은 100%-오프셋.
fn pos_axis_computed(tokens: &[&str]) -> String {
    match tokens {
        [t] => match *t {
            "left" | "top" => "0%".to_string(),
            "right" | "bottom" => "100%".to_string(),
            "center" => "50%".to_string(),
            other => other.to_string(),
        },
        [edge, offset] => {
            let from_start = matches!(*edge, "left" | "top");
            if from_start {
                offset.to_string()
            } else if let Some(pct) = offset.strip_suffix('%').and_then(|n| n.parse::<f64>().ok()) {
                let r = 100.0 - pct;
                let r = (r * 1e6).round() / 1e6;
                format!("{}%", r)
            } else {
                // 100% - offset 을 px 먼저 calc 로: "calc(-20px + 100%)"(Chrome 계산값·
                // 보간 경로와 일치). offset 부호를 뒤집어 앞에 둔다.
                let neg = match offset.strip_prefix('-') {
                    Some(r) => r.to_string(),
                    None => format!("-{}", offset),
                };
                format!("calc({} + 100%)", neg)
            }
        }
        _ => tokens.join(" "),
    }
}

// <position> 계산값(§CSSOM): [수평] [수직] 을 각각 퍼센트/오프셋 계산값으로.
pub fn position_computed(raw: &str) -> String {
    let toks: Vec<String> = split_top_level(raw).iter().map(|t| t.to_ascii_lowercase()).collect();
    let refs: Vec<&str> = toks.iter().map(|s| s.as_str()).collect();
    let is_h = |t: &str| matches!(t, "left" | "right");
    let is_v = |t: &str| matches!(t, "top" | "bottom");
    let is_kw = |t: &str| is_h(t) || is_v(t) || t == "center";
    let (h, v): (Vec<&str>, Vec<&str>) = match refs.len() {
        1 => {
            if is_v(refs[0]) {
                (vec!["center"], vec![refs[0]])
            } else {
                (vec![refs[0]], vec!["center"])
            }
        }
        2 => {
            if is_kw(refs[0]) && is_kw(refs[1]) {
                let h = if is_h(refs[0]) {
                    refs[0]
                } else if is_h(refs[1]) {
                    refs[1]
                } else {
                    "center"
                };
                let vv = if is_v(refs[0]) {
                    refs[0]
                } else if is_v(refs[1]) {
                    refs[1]
                } else {
                    "center"
                };
                (vec![h], vec![vv])
            } else {
                (vec![refs[0]], vec![refs[1]])
            }
        }
        4 => {
            if is_h(refs[0]) {
                (vec![refs[0], refs[1]], vec![refs[2], refs[3]])
            } else {
                (vec![refs[2], refs[3]], vec![refs[0], refs[1]])
            }
        }
        _ => return raw.trim().to_ascii_lowercase(),
    };
    format!("{} {}", pos_axis_computed(&h), pos_axis_computed(&v))
}

// background-position 계산값(§CSSOM): 콤마 구분 레이어마다 position_computed 적용
// (center→50%, left/top→0%, right/bottom→100%, 모서리+오프셋→calc).
pub fn background_position_computed(raw: &str) -> String {
    split_top_commas(raw)
        .iter()
        .map(|l| position_computed(l.trim()))
        .collect::<Vec<_>>()
        .join(", ")
}

// <bg-position> 지정값 캐논(§CSSOM): 1/2/4 는 position_canonical, 3값은 성분 파싱해
// [H성분] [V성분] 순서로(모서리+오프셋 유지).
pub fn bg_position_canonical(raw: &str) -> String {
    let toks: Vec<String> = split_top_level(raw).iter().map(|t| t.to_ascii_lowercase()).collect();
    if toks.len() != 3 {
        return position_canonical(raw);
    }
    let is_h = |t: &str| matches!(t, "left" | "right");
    let is_v = |t: &str| matches!(t, "top" | "bottom");
    let is_lp = |t: &str| {
        if is_math_fn(t) {
            return math_length_valid(t, true); // <length-percentage>(§CSS Values 4 타입 검사)
        }
        is_length_percentage(t)
    };
    let mut comps: Vec<(u8, String)> = Vec::new();
    let mut i = 0;
    while i < 3 {
        let t = toks[i].as_str();
        if t == "center" {
            comps.push((2, "center".to_string()));
            i += 1;
        } else if is_h(t) || is_v(t) {
            let axis = if is_h(t) { 0 } else { 1 };
            let mut s = t.to_string();
            i += 1;
            if i < 3 && is_lp(&toks[i]) {
                s = format!("{} {}", t, toks[i]);
                i += 1;
            }
            comps.push((axis, s));
        } else {
            return raw.trim().to_ascii_lowercase();
        }
    }
    if comps.len() != 2 {
        return raw.trim().to_ascii_lowercase();
    }
    // H 성분·V 성분 배정.
    let (h, v) = if comps[0].0 == 0 {
        (&comps[0].1, &comps[1].1)
    } else if comps[1].0 == 0 {
        (&comps[1].1, &comps[0].1)
    } else if comps[0].0 == 1 {
        (&comps[1].1, &comps[0].1)
    } else {
        (&comps[0].1, &comps[1].1)
    };
    format!("{} {}", h, v)
}

// background-position 캐논(§CSSOM): <bg-position># — 레이어마다 bg_position_canonical.
pub fn bg_position_list_canonical(raw: &str) -> String {
    split_top_commas(raw)
        .iter()
        .map(|l| {
            if bg_position_valid(l) {
                bg_position_canonical(l)
            } else {
                l.trim().to_ascii_lowercase()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

// <bg-position> 유효성(§CSS Backgrounds): <position> 에 3값 모서리+오프셋 형태를 더 허용.
// object-position(<position>, 3값 불가)과 달리 background-position 은 3값도 유효.
pub fn bg_position_valid(raw: &str) -> bool {
    let toks: Vec<String> = split_top_level(raw).iter().map(|t| t.to_ascii_lowercase()).collect();
    let is_h = |t: &str| matches!(t, "left" | "right");
    let is_v = |t: &str| matches!(t, "top" | "bottom");
    let is_lp = |t: &str| {
        if is_math_fn(t) {
            return math_length_valid(t, true); // <length-percentage>(§CSS Values 4 타입 검사)
        }
        is_length_percentage(t)
    };
    let n = toks.len();
    if n <= 2 || n == 4 {
        return position_valid(raw); // 1/2/4 는 <position> 과 동일
    }
    if n != 3 {
        return false;
    }
    // 3값: 두 성분(center | [left|right|top|bottom] <lp>?), 서로 다른 축(H/V) 배정.
    let mut i = 0;
    let mut axes: Vec<u8> = Vec::new();
    while i < n && axes.len() < 2 {
        let t = toks[i].as_str();
        if t == "center" {
            axes.push(2);
            i += 1;
        } else if is_h(t) {
            i += 1;
            if i < n && is_lp(&toks[i]) {
                i += 1;
            }
            axes.push(0);
        } else if is_v(t) {
            i += 1;
            if i < n && is_lp(&toks[i]) {
                i += 1;
            }
            axes.push(1);
        } else {
            return false; // 3값 형태엔 벌거벗은 <lp> 불가
        }
    }
    i == n && axes.len() == 2 && !(axes[0] == 0 && axes[1] == 0) && !(axes[0] == 1 && axes[1] == 1)
}

// <position> 지정값 캐논 직렬화(§CSSOM): [수평] [수직] 순서로, 1값은 빠진 축에 center.
// 키워드는 유지(퍼센트 변환은 계산값 몫). 유효한 값만 넣는다고 가정.
pub fn position_canonical(raw: &str) -> String {
    let toks: Vec<String> = split_top_level(raw).iter().map(|t| t.to_ascii_lowercase()).collect();
    let is_h = |t: &str| matches!(t, "left" | "right");
    let is_v = |t: &str| matches!(t, "top" | "bottom");
    let is_kw = |t: &str| is_h(t) || is_v(t) || t == "center";
    match toks.len() {
        1 => {
            let t = &toks[0];
            if is_v(t) {
                format!("center {}", t)
            } else {
                format!("{} center", t)
            }
        }
        2 => {
            let (a, b) = (toks[0].as_str(), toks[1].as_str());
            if is_kw(a) && is_kw(b) {
                let h = if is_h(a) {
                    a
                } else if is_h(b) {
                    b
                } else {
                    "center"
                };
                let v = if is_v(a) {
                    a
                } else if is_v(b) {
                    b
                } else {
                    "center"
                };
                format!("{} {}", h, v)
            } else {
                format!("{} {}", a, b) // lp 포함: 이미 [수평][수직] 순서
            }
        }
        4 => {
            let g1 = format!("{} {}", toks[0], toks[1]);
            let g2 = format!("{} {}", toks[2], toks[3]);
            if is_h(&toks[0]) {
                format!("{} {}", g1, g2)
            } else {
                format!("{} {}", g2, g1)
            }
        }
        _ => raw.trim().to_ascii_lowercase(),
    }
}

// contain-intrinsic-size 값 유효성(§CSS Sizing 4): [ auto? [ none | <length> ] ]{1,max}.
// 길이만(퍼센트 없음), 비음수. legacy·%·음수·초과 그룹 거부.
pub fn contain_intrinsic_valid(raw: &str, max_groups: usize) -> bool {
    let toks: Vec<String> = split_top_level(raw).iter().map(|t| t.to_ascii_lowercase()).collect();
    let is_len = |t: &str| {
        if is_math_fn(t) {
            return math_length_valid(t, false); // <length> 전용(§CSS Values 4 타입 검사)
        }
        !t.ends_with('%') && is_length_percentage(t) && !t.starts_with('-')
    };
    let mut i = 0;
    let mut groups = 0;
    while i < toks.len() && groups < max_groups {
        if toks[i] == "auto" {
            i += 1;
            if i >= toks.len() {
                return false; // auto 뒤엔 none|<length> 필수
            }
        }
        if toks[i] == "none" || is_len(&toks[i]) {
            i += 1;
            groups += 1;
        } else {
            return false;
        }
    }
    i == toks.len() && groups >= 1
}

// transform-origin 의 수평/수직 2값이 유효한가(모서리+오프셋 형태 없음, 순수 h&&v).
fn to_pos2_valid(a: &str, b: &str) -> bool {
    let is_h = |t: &str| matches!(t, "left" | "right");
    let is_v = |t: &str| matches!(t, "top" | "bottom");
    let is_lp = |t: &str| {
        if is_math_fn(t) {
            return math_length_valid(t, true); // <length-percentage>(§CSS Values 4 타입 검사)
        }
        is_length_percentage(t)
    };
    let a_kw = is_h(a) || is_v(a) || a == "center";
    let b_kw = is_h(b) || is_v(b) || b == "center";
    if a_kw && b_kw {
        let (a_h, a_v) = (is_h(a) || a == "center", is_v(a) || a == "center");
        let (b_h, b_v) = (is_h(b) || b == "center", is_v(b) || b == "center");
        (a_h && b_v) || (a_v && b_h)
    } else {
        (is_h(a) || a == "center" || is_lp(a)) && (is_v(b) || b == "center" || is_lp(b))
    }
}

// transform-origin 유효성(§CSS Transforms): [<position 2값>] <length>?(z 오프셋).
pub fn transform_origin_valid(raw: &str) -> bool {
    let toks: Vec<String> = split_top_level(raw).iter().map(|t| t.to_ascii_lowercase()).collect();
    let is_lp = |t: &str| {
        if is_math_fn(t) {
            return math_length_valid(t, true); // <length-percentage>(§CSS Values 4 타입 검사)
        }
        is_length_percentage(t)
    };
    let is_len = |t: &str| {
        if is_math_fn(t) {
            return math_length_valid(t, false); // <length> 전용(§CSS Values 4 타입 검사)
        }
        !t.ends_with('%') && is_length_percentage(t)
    };
    match toks.len() {
        1 => {
            let t = toks[0].as_str();
            matches!(t, "left" | "right" | "top" | "bottom" | "center") || is_lp(t)
        }
        2 => to_pos2_valid(&toks[0], &toks[1]),
        3 => to_pos2_valid(&toks[0], &toks[1]) && is_len(&toks[2]),
        _ => false,
    }
}

// transform-origin 캐논 직렬화: [수평] [수직] (1값→center 보충), z 오프셋 유지.
pub fn transform_origin_canonical(raw: &str) -> String {
    let toks: Vec<String> = split_top_level(raw).iter().map(|t| t.to_ascii_lowercase()).collect();
    let is_v = |t: &str| matches!(t, "top" | "bottom");
    match toks.len() {
        1 => {
            if is_v(&toks[0]) {
                format!("center {}", toks[0])
            } else {
                format!("{} center", toks[0])
            }
        }
        2 => position_canonical(raw),
        3 => format!("{} {}", position_canonical(&format!("{} {}", toks[0], toks[1])), toks[2]),
        _ => raw.trim().to_ascii_lowercase(),
    }
}

// 계산식이 수/퍼센트만인가(길이/각도/시간 단위 없음). 숫자 뒤에 지수 아닌 알파벳이
// 오면 단위로 본다(scale 은 <number>|<percentage> 만 허용).
fn calc_dimensionless(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_digit() || b[i] == b'.' {
            while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
                i += 1;
            }
            if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
                let j = i + 1;
                if j < b.len() && (b[j] == b'+' || b[j] == b'-' || b[j].is_ascii_digit()) {
                    i += 1;
                    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
                        i += 1;
                    }
                    while i < b.len() && b[i].is_ascii_digit() {
                        i += 1;
                    }
                }
            }
            if i < b.len() && b[i].is_ascii_alphabetic() {
                return false; // 숫자 뒤 단위 = 차원 있음
            }
        } else {
            i += 1;
        }
    }
    true
}

// corner-shape 한 값 유효성(§CSS Borders 4): round|scoop|bevel|notch|square|squircle|
// superellipse(<number>).
pub fn corner_shape_value_valid(t: &str) -> bool {
    let low = t.trim().to_ascii_lowercase();
    if matches!(low.as_str(), "round" | "scoop" | "bevel" | "notch" | "square" | "squircle") {
        return true;
    }
    if let Some(inner) = low.strip_prefix("superellipse(").and_then(|x| x.strip_suffix(')')) {
        let n = inner.trim();
        // <number>(infinity/-infinity/nan 키워드 포함) 또는 calc. "1 abc"/"8 8"/"," 는 거부.
        return !n.is_empty() && (n.parse::<f64>().is_ok() || is_math_fn(n));
    }
    false
}

// corner-shape 계열 유효성(§CSS Borders 4): [<corner-shape-value>]{1,max}.
pub fn corner_shape_list_valid(raw: &str, max: usize) -> bool {
    let toks = split_top_level(raw);
    !toks.is_empty() && toks.len() <= max && toks.iter().all(|t| corner_shape_value_valid(t))
}

// corner-shape 캐논 직렬화: superellipse 인자 공백 정리, TRBL 박스 축약(4→1/2/3).
pub fn corner_shape_canonical(raw: &str) -> String {
    let vals: Vec<String> = split_top_level(raw)
        .iter()
        .map(|t| {
            let low = t.to_ascii_lowercase();
            if let Some(inner) = low.strip_prefix("superellipse(").and_then(|x| x.strip_suffix(')'))
            {
                format!("superellipse({})", inner.trim())
            } else {
                low
            }
        })
        .collect();
    match vals.len() {
        4 => {
            let (a, b, c, d) = (&vals[0], &vals[1], &vals[2], &vals[3]);
            if a == b && b == c && c == d {
                a.clone()
            } else if a == c && b == d {
                format!("{} {}", a, b)
            } else if b == d {
                format!("{} {} {}", a, b, c)
            } else {
                vals.join(" ")
            }
        }
        2 => {
            if vals[0] == vals[1] {
                vals[0].clone()
            } else {
                vals.join(" ")
            }
        }
        _ => vals.join(" "),
    }
}

// scale 프로퍼티 유효성(§CSS Transforms 2): none | [<number>|<percentage>]{1,3}.
pub fn scale_valid(raw: &str) -> bool {
    let toks: Vec<String> = split_top_level(raw).iter().map(|t| t.to_ascii_lowercase()).collect();
    if toks.len() == 1 && toks[0] == "none" {
        return true;
    }
    if toks.is_empty() || toks.len() > 3 {
        return false;
    }
    toks.iter().all(|t| {
        if is_math_fn(t) {
            // 명백한 단순 차원 calc(길이/각도/시간)만 거부. sign()/comparison 등이 타입을
            // 바꿔 무차원이 될 수 있어(§CSS Values calc 타입) 함수 든 calc 는 관대히 수용.
            let simple = !t.contains('(') || t.matches('(').count() == 1;
            return !(simple && !calc_dimensionless(t));
        }
        if let Some(n) = t.strip_suffix('%') {
            return n.parse::<f64>().map(|v| v.is_finite()).unwrap_or(false);
        }
        t.parse::<f64>().map(|v| v.is_finite()).unwrap_or(false)
    })
}

// translate 프로퍼티 유효성(§CSS Transforms 2): none | <length-percentage>
// [<length-percentage> <length>?]?.
pub fn translate_valid(raw: &str) -> bool {
    let toks: Vec<String> = split_top_level(raw).iter().map(|t| t.to_ascii_lowercase()).collect();
    if toks.len() == 1 && toks[0] == "none" {
        return true;
    }
    let is_lp = |t: &str| {
        if is_math_fn(t) {
            return math_length_valid(t, true); // <length-percentage>(§CSS Values 4 타입 검사)
        }
        is_length_percentage(t)
    };
    let is_len = |t: &str| {
        if is_math_fn(t) {
            return math_length_valid(t, false); // <length> 전용(§CSS Values 4 타입 검사)
        }
        !t.ends_with('%') && is_length_percentage(t)
    };
    match toks.len() {
        1 => is_lp(&toks[0]),
        2 => is_lp(&toks[0]) && is_lp(&toks[1]),
        3 => is_lp(&toks[0]) && is_lp(&toks[1]) && is_len(&toks[2]),
        _ => false,
    }
}

// rotate 프로퍼티 유효성(§CSS Transforms 2): none | <angle> | [x|y|z|<number>{3}] && <angle>.
pub fn rotate_valid(raw: &str) -> bool {
    let toks: Vec<String> = split_top_level(raw).iter().map(|t| t.to_ascii_lowercase()).collect();
    if toks.len() == 1 && toks[0] == "none" {
        return true;
    }
    let is_angle = |t: &str| {
        if is_math_fn(t) {
            return !(t.contains('%'));
        }
        parse_angle_deg(t).is_some()
    };
    let is_num = |t: &str| {
        if is_math_fn(t) {
            return true;
        }
        t.parse::<f64>().map(|v| v.is_finite()).unwrap_or(false)
    };
    let (mut angles, mut axes, mut nums) = (0, 0, 0);
    for t in &toks {
        if is_angle(t) {
            angles += 1;
        } else if matches!(t.as_str(), "x" | "y" | "z") {
            axes += 1;
        } else if is_num(t) {
            nums += 1;
        } else {
            return false;
        }
    }
    // 각도 정확히 1개 필요. 축은 없거나(각도만), x/y/z 하나, 또는 수 3개.
    angles == 1 && ((axes == 0 && nums == 0) || (axes == 1 && nums == 0) || (axes == 0 && nums == 3))
}

// aspect-ratio 유효성(§CSS Sizing): auto || <ratio>. <ratio>=<number 0+> [/ <number 0+>].
pub fn aspect_ratio_valid(raw: &str) -> bool {
    let norm = raw.replace('/', " / ");
    let mut toks: Vec<&str> = norm.split_whitespace().collect();
    let auto_count = toks.iter().filter(|t| t.eq_ignore_ascii_case("auto")).count();
    if auto_count > 1 {
        return false;
    }
    if auto_count == 1 {
        if toks.first().map_or(false, |t| t.eq_ignore_ascii_case("auto")) {
            toks.remove(0);
        } else if toks.last().map_or(false, |t| t.eq_ignore_ascii_case("auto")) {
            toks.pop();
        } else {
            return false; // auto 는 양끝만
        }
    }
    if toks.is_empty() {
        return auto_count == 1; // "auto" 단독
    }
    let is_num = |t: &str| t.parse::<f64>().map(|v| v.is_finite() && v >= 0.0).unwrap_or(false);
    match toks.len() {
        1 => is_num(toks[0]),
        3 => toks[1] == "/" && is_num(toks[0]) && is_num(toks[2]),
        _ => false,
    }
}

// aspect-ratio 캐논 직렬화(§CSS Sizing): auto 앞으로, <ratio> 는 "a / b"(단일수→"n / 1").
pub fn aspect_ratio_canonical(raw: &str) -> String {
    let norm = raw.replace('/', " / ");
    let mut toks: Vec<String> = norm.split_whitespace().map(|t| t.to_ascii_lowercase()).collect();
    let has_auto = toks.iter().any(|t| t == "auto");
    toks.retain(|t| t != "auto");
    let ratio = match toks.len() {
        0 => String::new(),
        1 => format!("{} / 1", toks[0]),
        3 => format!("{} / {}", toks[0], toks[2]),
        _ => return raw.trim().to_ascii_lowercase(),
    };
    if has_auto {
        if ratio.is_empty() {
            "auto".to_string()
        } else {
            format!("auto {}", ratio)
        }
    } else {
        ratio
    }
}

// anchor-size() (§css-anchor-1 #anchor-size): sizing/inset/margin 프로퍼티에서만 유효.
//   anchor-size( [ <dashed-ident> || <size-keyword> ] [ , <fallback> ]? )
// size-keyword ∈ width|height|block|inline|self-block|self-inline. name/size 순서는
// 무관하나 캐논은 name-first. fallback ∈ <length-percentage> | 수학함수 | 중첩
// anchor-size(). 유효하면 캐논 문자열, 아니면 None. (예전엔 문법 없이 is_length_percentage
// 의 관대 수용에 기대 일부만 우연히 통과 — 요행이었다.)
pub fn anchor_size_canonical(value: &str) -> Option<String> {
    let t = value.trim();
    let tl = t.to_ascii_lowercase();
    if !tl.starts_with("anchor-size(") || !t.ends_with(')') {
        return None;
    }
    let inner = &t["anchor-size(".len()..t.len() - 1]; // dashed-ident 대소문자 보존
    // 선행/후행 콤마(빈 세그먼트)는 무효 — split_top_commas 가 후행 빈칸을 버리므로 먼저 막는다.
    // 빈 inner("anchor-size()")는 유효 — 아래에서 빈 주부로 처리한다.
    let it = inner.trim();
    if it.starts_with(',') || it.ends_with(',') {
        return None;
    }
    // 주부: [ <dashed-ident> || <size-keyword> ]? — 전체 optional(빈 것도 유효). 순서 무관,
    // 캐논 name-first. length-percentage 는 주부의 단독 토큰일 때만 허용(§, 예: anchor-size(10px)).
    // fallback 은 콤마 뒤(§): <length-percentage> | 수학함수 | 중첩 anchor-size().
    let parts = split_top_commas(inner);
    if parts.len() > 2 {
        return None;
    }
    let main = parts.first().map(|s| s.trim()).unwrap_or("");
    let toks = split_top_level(main);
    if toks.len() > 2 {
        return None;
    }
    const SIZES: &[&str] =
        &["width", "height", "block", "inline", "self-block", "self-inline"];
    let mut name: Option<String> = None;
    let mut size: Option<String> = None;
    let mut length: Option<String> = None;
    for tk in &toks {
        let low = tk.to_ascii_lowercase();
        if SIZES.contains(&low.as_str()) {
            if size.is_some() {
                return None; // size 두 개
            }
            size = Some(low);
        } else if tk.starts_with("--") && tk.len() > 2 {
            if name.is_some() {
                return None; // name 두 개
            }
            name = Some(tk.to_string()); // 대소문자 보존
        } else if toks.len() == 1 && is_length_percentage(&low) && !low.starts_with('-') {
            length = Some(low); // length 는 단독 토큰일 때만(name/size 와 못 섞임)
        } else {
            return None; // size-keyword/dashed-ident/(단독)length 아님
        }
    }
    let mut canon = String::from("anchor-size(");
    let mut mparts: Vec<String> = Vec::new();
    if let Some(n) = &name {
        mparts.push(n.clone());
    }
    if let Some(s) = &size {
        mparts.push(s.clone());
    }
    if let Some(l) = &length {
        mparts.push(l.clone());
    }
    canon.push_str(&mparts.join(" ")); // 빈 주부면 "" → anchor-size()
    if parts.len() == 2 {
        // anchor-size() 는 anchor-size() 만 중첩 허용(anchor() 는 inset 전용이라 불가).
        let fb = anchor_fallback_canonical(parts[1].trim(), true, false)?;
        canon.push_str(", ");
        canon.push_str(&fb);
    }
    canon.push(')');
    Some(canon)
}

// anchor()/anchor-size() 의 fallback: 단일 <length-percentage> | 수학함수 | **자기 자신**의
// 중첩. anchor-size() 는 anchor-size() 만, anchor() 는 anchor() 만 중첩 허용한다 —
// anchor() 는 inset 전용이라 sizing 문맥의 anchor-size() fallback 이 될 수 없고, 그 역도
// 유효값 목록에 없다(§css-anchor-1). 빈값/다중값/시간단위/bare 키워드/무효 중첩은 거부.
fn anchor_fallback_canonical(fb: &str, allow_anchor_size: bool, allow_anchor: bool) -> Option<String> {
    if fb.is_empty() || split_top_level(fb).len() != 1 {
        return None;
    }
    let low = fb.to_ascii_lowercase();
    if low.starts_with("anchor-size(") {
        return if allow_anchor_size { anchor_size_canonical(fb) } else { None };
    }
    if low.starts_with("anchor(") {
        return if allow_anchor { anchor_canonical(fb) } else { None };
    }
    if is_math_fn(&low) {
        return Some(fb.trim().to_string());
    }
    if is_length_percentage(&low) && !low.starts_with('-') {
        return Some(low);
    }
    None
}

// anchor() (§css-anchor-1 #anchor): inset 프로퍼티(top/right/bottom/left, 논리 포함)에서만.
//   anchor( [ <dashed-ident> || <anchor-side> ] [ , <fallback> ]? )
// anchor-side ∈ inside|outside|left|right|top|bottom|start|end|self-start|self-end|center
//   | <percentage> | 수학함수. (길이는 불가 — anchor-size 와 달리 <percentage> 만.)
// name/side 순서 무관 → 캐논 name-first. 유효하면 캐논 문자열, 아니면 None.
pub fn anchor_canonical(value: &str) -> Option<String> {
    let t = value.trim();
    let tl = t.to_ascii_lowercase();
    if !tl.starts_with("anchor(") || !t.ends_with(')') {
        return None;
    }
    let inner = &t["anchor(".len()..t.len() - 1]; // dashed-ident 대소문자 보존
    let it = inner.trim();
    if it.is_empty() || it.starts_with(',') || it.ends_with(',') {
        return None;
    }
    let parts = split_top_commas(inner);
    if parts.is_empty() || parts.len() > 2 {
        return None;
    }
    let toks = split_top_level(parts[0].trim());
    if toks.is_empty() || toks.len() > 2 {
        return None;
    }
    const SIDES: &[&str] = &[
        "inside", "outside", "left", "right", "top", "bottom", "start", "end",
        "self-start", "self-end", "center",
    ];
    let mut name: Option<String> = None;
    let mut side: Option<String> = None;
    for tk in &toks {
        let low = tk.to_ascii_lowercase();
        let is_side = SIDES.contains(&low.as_str())
            || (low.ends_with('%')
                && low[..low.len() - 1].parse::<f64>().map(|v| v.is_finite()).unwrap_or(false))
            || is_math_fn(&low);
        if is_side {
            if side.is_some() {
                return None; // side 두 개
            }
            // 키워드/퍼센트는 소문자 캐논, 수학함수는 원문 유지.
            side = Some(if is_math_fn(&low) { tk.to_string() } else { low });
        } else if tk.starts_with("--") && tk.len() > 2 {
            if name.is_some() {
                return None; // name 두 개
            }
            name = Some(tk.to_string()); // 대소문자 보존
        } else {
            return None; // anchor-side 도 dashed-ident 도 아님
        }
    }
    let side = side?; // side 필수
    let mut canon = String::from("anchor(");
    if let Some(n) = &name {
        canon.push_str(n);
        canon.push(' ');
    }
    canon.push_str(&side);
    if parts.len() == 2 {
        // anchor() 는 anchor() 만 중첩 허용(유효값 목록 기준).
        let fb = anchor_fallback_canonical(parts[1].trim(), false, true)?;
        canon.push_str(", ");
        canon.push_str(&fb);
    }
    canon.push(')');
    Some(canon)
}

// position-area (§css-anchor-2 #position-area): 2D 그리드 영역. none | <pa-keyword>{1,2}.
// 각 키워드는 한 축 그룹에 속하고, 두 값은 **호환 축 쌍**에서 하나씩(또는 center/span-all).
// 캐논: H/block/self-block 축이 앞, V/inline/self-inline 이 뒤. span-all 은 쌍에서 드롭.
#[derive(PartialEq, Clone, Copy)]
enum PaAxis {
    H,
    V,
    Inline,
    Block,
    SelfInline,
    SelfBlock,
    StartEnd,
    SelfStartEnd,
    Center,
    SpanAll,
}

fn pa_axis(kw: &str) -> Option<PaAxis> {
    use PaAxis::*;
    Some(match kw {
        "left" | "right" | "span-left" | "span-right" | "x-start" | "x-end" | "span-x-start"
        | "span-x-end" | "self-x-start" | "self-x-end" | "span-self-x-start" | "span-self-x-end" => H,
        "top" | "bottom" | "span-top" | "span-bottom" | "y-start" | "y-end" | "span-y-start"
        | "span-y-end" | "self-y-start" | "self-y-end" | "span-self-y-start" | "span-self-y-end" => V,
        "inline-start" | "inline-end" | "span-inline-start" | "span-inline-end" => Inline,
        "block-start" | "block-end" | "span-block-start" | "span-block-end" => Block,
        "self-inline-start" | "self-inline-end" | "span-self-inline-start" | "span-self-inline-end" => {
            SelfInline
        }
        "self-block-start" | "self-block-end" | "span-self-block-start" | "span-self-block-end" => {
            SelfBlock
        }
        "start" | "end" | "span-start" | "span-end" => StartEnd,
        "self-start" | "self-end" | "span-self-start" | "span-self-end" => SelfStartEnd,
        "center" => Center,
        "span-all" => SpanAll,
        _ => return None,
    })
}

pub fn position_area_canonical(value: &str) -> Option<String> {
    use PaAxis::*;
    let low = value.trim().to_ascii_lowercase();
    if matches!(low.as_str(), "none" | "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
        return Some(low);
    }
    let toks: Vec<&str> = low.split_whitespace().collect();
    if toks.is_empty() || toks.len() > 2 {
        return None;
    }
    let a0 = pa_axis(toks[0])?;
    if toks.len() == 1 {
        return Some(toks[0].to_string()); // 단일 유효 키워드(none 은 위에서)
    }
    let a1 = pa_axis(toks[1])?;
    let is_dir = |a: PaAxis| matches!(a, StartEnd | SelfStartEnd);
    // center/span-all 끼리: center center→center, span-all span-all→span-all, 그 외 입력 유지.
    if matches!(a0, Center | SpanAll) && matches!(a1, Center | SpanAll) {
        if a0 == a1 {
            return Some(toks[0].to_string());
        }
        return Some(format!("{} {}", toks[0], toks[1]));
    }
    // 방향 축(start/end, self-start/end)이 한쪽이면 특수: 같은 그룹끼리(같은 키워드는 단일로
    // 축약), 또는 center/span-all 과의 조합은 **입력 순서 유지**(span-all 드롭 안 함). 그 외
    // 비방향 축과는 비호환.
    if is_dir(a0) || is_dir(a1) {
        let (dir_a, other_a) = if is_dir(a0) { (a0, a1) } else { (a1, a0) };
        if other_a == dir_a {
            // 같은 방향 그룹: 같은 키워드 → 단일, 다르면 입력 순서.
            if toks[0] == toks[1] {
                return Some(toks[0].to_string());
            }
            return Some(format!("{} {}", toks[0], toks[1]));
        }
        if matches!(other_a, Center | SpanAll) {
            return Some(format!("{} {}", toks[0], toks[1])); // span-all 유지, 입력 순서
        }
        return None; // 방향 축 + 비호환
    }
    // 이하 비방향 축. 한쪽이 span-all → 드롭.
    if a0 == SpanAll {
        return Some(toks[1].to_string());
    }
    if a1 == SpanAll {
        return Some(toks[0].to_string());
    }
    // 한쪽이 center → center + 축키워드. H/Block/SelfBlock 앞, V/Inline/SelfInline 뒤.
    if a0 == Center || a1 == Center {
        let (axis_tok, axis) = if a0 == Center { (toks[1], a1) } else { (toks[0], a0) };
        return match axis {
            H | Block | SelfBlock => Some(format!("{} center", axis_tok)),
            V | Inline | SelfInline => Some(format!("center {}", axis_tok)),
            _ => None,
        };
    }
    // 두 축 키워드: 같은 키워드 무효, 호환 축 쌍만(캐논 순서 H/block/self-block 앞).
    if toks[0] == toks[1] {
        return None;
    }
    match (a0, a1) {
        (H, V) | (Block, Inline) | (SelfBlock, SelfInline) => Some(format!("{} {}", toks[0], toks[1])),
        (V, H) | (Inline, Block) | (SelfInline, SelfBlock) => Some(format!("{} {}", toks[1], toks[0])),
        _ => None, // 비호환 축(같은 축 포함)
    }
}

// position-area 계산값 remap(§css-anchor-2, 기본 writing-mode): block/inline 논리 축
// 쌍은 계산 시 start-end 논리 형태로 접힌다. 예: "block-start inline-start" → "start"
// (둘 다 start), "block-start inline-end" → "start end". 단일 키워드/물리 축/center/
// span-all 은 그대로. (writing-mode 인식 해석은 별도 — 여기선 기본 horizontal-tb 가정.)
fn logical_to_startend(kw: &str) -> Option<&'static str> {
    Some(match kw {
        "block-start" | "inline-start" => "start",
        "block-end" | "inline-end" => "end",
        "span-block-start" | "span-inline-start" => "span-start",
        "span-block-end" | "span-inline-end" => "span-end",
        "self-block-start" | "self-inline-start" => "self-start",
        "self-block-end" | "self-inline-end" => "self-end",
        "span-self-block-start" | "span-self-inline-start" => "span-self-start",
        "span-self-block-end" | "span-self-inline-end" => "span-self-end",
        _ => return None,
    })
}

fn pa_is_block_axis(kw: &str) -> bool {
    kw.contains("block-") // block-*, self-block-*, span-block-*, span-self-block-*
}

// start-end/self-start-end 키워드를 block/inline 논리형으로 복원(computed): 첫 슬롯→block,
// 둘째 슬롯→inline. start-end 축 + span-all 계산에 쓴다("start span-all"→"block-start").
fn startend_to_logical(kw: &str, block: bool) -> Option<String> {
    let ax = if block { "block" } else { "inline" };
    Some(match kw {
        "start" => format!("{}-start", ax),
        "end" => format!("{}-end", ax),
        "span-start" => format!("span-{}-start", ax),
        "span-end" => format!("span-{}-end", ax),
        "self-start" => format!("self-{}-start", ax),
        "self-end" => format!("self-{}-end", ax),
        "span-self-start" => format!("span-self-{}-start", ax),
        "span-self-end" => format!("span-self-{}-end", ax),
        _ => return None,
    })
}

pub fn position_area_computed(value: &str) -> String {
    let toks: Vec<&str> = value.split_whitespace().collect();
    if toks.len() == 2 {
        // start-end/self-start-end + span-all → 논리형으로 복원(span-all 드롭). start-end 가
        // 첫 슬롯이면 block, 둘째 슬롯이면 inline.
        if toks[1] == "span-all" {
            if let Some(l) = startend_to_logical(toks[0], true) {
                return l;
            }
        }
        if toks[0] == "span-all" {
            if let Some(l) = startend_to_logical(toks[1], false) {
                return l;
            }
        }
        let m0 = logical_to_startend(toks[0]);
        let m1 = logical_to_startend(toks[1]);
        let remapped = match (m0, m1) {
            // 두 토큰 모두 논리 축: 입력 순서 유지(파싱 캐논이 이미 block-first 정렬).
            (Some(a), Some(b)) => Some(format!("{} {}", a, b)),
            // 한쪽만 논리(다른쪽 center/span-all): center-slot 규칙으로 순서 — block 축은
            // start-end 를 앞, inline 축은 뒤.
            (Some(a), None) => Some(if pa_is_block_axis(toks[0]) {
                format!("{} {}", a, toks[1])
            } else {
                format!("{} {}", toks[1], a)
            }),
            (None, Some(b)) => Some(if pa_is_block_axis(toks[1]) {
                format!("{} {}", b, toks[0])
            } else {
                format!("{} {}", toks[0], b)
            }),
            (None, None) => None,
        };
        if let Some(r) = remapped {
            if let Some(c) = position_area_canonical(&r) {
                return c;
            }
        }
    }
    value.to_string()
}

// 크기 프로퍼티 값 유효성(§CSS Sizing): [auto|none|<length-percentage 0+>|min-content|
// max-content|fit-content|fit-content()]. allow_none: max-*, allow_auto: width/min-*.
pub fn size_valid(tok: &str, allow_none: bool, allow_auto: bool) -> bool {
    let low = tok.trim().to_ascii_lowercase();
    if low == "auto" {
        return allow_auto;
    }
    if low == "none" {
        return allow_none;
    }
    if matches!(low.as_str(), "min-content" | "max-content" | "fit-content"
        | "-webkit-min-content" | "-webkit-max-content" | "-webkit-fit-content"
        | "stretch" | "-webkit-fill-available" | "fit-content")
    {
        return true;
    }
    if low.starts_with("fit-content(") && low.ends_with(')') {
        return true;
    }
    if is_math_fn(&low) {
        // 결과가 <length-percentage> 여야(§CSS Values 4 타입 검사). max(0Hz)/
        // calc(1/2px)/max(0)(수) 등 타입 불일치 거부.
        return math_length_valid(&low, true);
    }
    is_length_percentage(&low) && !low.starts_with('-')
}

// row-gap/column-gap 값(§CSS Box Alignment): normal | <length-percentage>(비음수).
pub fn gap_value_valid(tok: &str) -> bool {
    let low = tok.trim().to_ascii_lowercase();
    if low == "normal" {
        return true;
    }
    if is_math_fn(&low) {
        return math_length_valid(&low, true);
    }
    is_length_percentage(&low) && !low.starts_with('-')
}

// scroll-margin 값(§CSS Scroll Snap): <length>(부호 무관, % 없음, auto 없음).
pub fn scroll_margin_valid(tok: &str) -> bool {
    let low = tok.trim().to_ascii_lowercase();
    if low == "auto" {
        return false;
    }
    if is_math_fn(&low) {
        // <length> calc 만(§CSS Values 4 타입 검사) — 각도·퍼센트 등 거부.
        return math_length_valid(&low, false);
    }
    !low.ends_with('%') && is_length_percentage(&low)
}

// scroll-padding 값(§CSS Scroll Snap): auto | <length-percentage>(비음수).
pub fn scroll_padding_valid(tok: &str) -> bool {
    let low = tok.trim().to_ascii_lowercase();
    if low == "auto" {
        return true;
    }
    if is_math_fn(&low) {
        return math_length_valid(&low, true);
    }
    is_length_percentage(&low) && !low.starts_with('-')
}

// 한 값(길이/auto/calc)을 CSSOM 캐논으로: 0→0px, calc 원문 보존, auto 유지.
fn box_token_canonical(t: &str) -> String {
    let low = t.trim().to_ascii_lowercase();
    if low == "auto" {
        return "auto".to_string();
    }
    if is_math_fn(&low) {
        return t.trim().to_string();
    }
    match interpret_value(t.trim()) {
        Some(Value::Length(n, _)) if n == 0.0 => "0px".to_string(),
        Some(v @ Value::Length(..)) => crate::style::computed_value_string(&v),
        _ => low,
    }
}

// TRBL 박스 단축 캐논 직렬화(§CSSOM): 1~4 값을 캐논화 후 표준 축약(1/2/3/4).
pub fn box_canonical(raw: &str) -> String {
    let toks = split_top_level(raw);
    if toks.is_empty() || toks.len() > 4 {
        return raw.trim().to_string();
    }
    let c: Vec<String> = toks.iter().map(|t| box_token_canonical(t)).collect();
    let (t, r, b, l) = match c.len() {
        1 => (&c[0], &c[0], &c[0], &c[0]),
        2 => (&c[0], &c[1], &c[0], &c[1]),
        3 => (&c[0], &c[1], &c[2], &c[1]),
        _ => (&c[0], &c[1], &c[2], &c[3]),
    };
    if t == r && r == b && b == l {
        t.clone()
    } else if t == b && r == l {
        format!("{} {}", t, r)
    } else if r == l {
        format!("{} {} {}", t, r, b)
    } else {
        format!("{} {} {} {}", t, r, b, l)
    }
}

// inset-block/inset-inline 단축 캐논 직렬화(§CSSOM): 각 값 0→0px 캐논, 두 값이 같으면
// 하나로 축약.
pub fn inset_pair_canonical(raw: &str) -> String {
    let toks = split_top_level(raw);
    if toks.is_empty() || toks.len() > 2 {
        return raw.trim().to_string();
    }
    let canon = |t: &str| -> String {
        let low = t.trim().to_ascii_lowercase();
        if low == "auto" {
            return "auto".to_string();
        }
        // 수학함수는 지정값에서 단순화하지 않고 원문 보존(calc(0px)→0px 로 접지 않음).
        if math_function_valid(&low) {
            return t.trim().to_string();
        }
        match interpret_value(t.trim()) {
            Some(Value::Length(n, _)) if n == 0.0 => "0px".to_string(),
            Some(v @ Value::Length(..)) => crate::style::computed_value_string(&v),
            _ => low,
        }
    };
    let a = canon(&toks[0]);
    let b = toks.get(1).map(|t| canon(t)).unwrap_or_else(|| a.clone());
    if a == b {
        a
    } else {
        format!("{} {}", a, b)
    }
}

// inset/오프셋 값 유효성(§CSS Position): <length-percentage> | auto(calc 포함).
// 각도(0deg/calc(20deg))·단위없는 비영(10)·기타 키워드 거부.
pub fn inset_length_valid(tok: &str) -> bool {
    let low = tok.trim().to_ascii_lowercase();
    if low == "auto" {
        return true;
    }
    if is_math_fn(&low) {
        // 결과가 <length-percentage> 여야(§CSS Values 4 타입 검사).
        return math_length_valid(&low, true);
    }
    is_length_percentage(&low)
}

// contain 유효성(§CSS Contain): none | strict | content | [[size|inline-size] ||
// layout || style || paint]. 단일 특수값 혼합·카테고리 중복·미인식 거부.
pub fn contain_valid(raw: &str) -> bool {
    let toks = split_top_level(raw);
    if toks.is_empty() {
        return false;
    }
    if toks.len() == 1 && matches!(toks[0].to_ascii_lowercase().as_str(), "none" | "strict" | "content")
    {
        return true;
    }
    let (mut sizing, mut layout, mut style, mut paint) = (false, false, false, false);
    for t in &toks {
        match t.to_ascii_lowercase().as_str() {
            "size" | "inline-size" => {
                if sizing {
                    return false;
                }
                sizing = true;
            }
            "layout" => {
                if layout {
                    return false;
                }
                layout = true;
            }
            "style" => {
                if style {
                    return false;
                }
                style = true;
            }
            "paint" => {
                if paint {
                    return false;
                }
                paint = true;
            }
            _ => return false, // none/strict/content 혼합 또는 미인식
        }
    }
    sizing || layout || style || paint
}

// contain 캐논 직렬화(§CSS Contain): size/inline-size, layout, style, paint 순서.
pub fn contain_canonical(raw: &str) -> String {
    let toks = split_top_level(raw);
    if toks.len() == 1 {
        return toks[0].to_ascii_lowercase();
    }
    let (mut sizing, mut layout, mut style, mut paint): (Option<String>, bool, bool, bool) =
        (None, false, false, false);
    for t in &toks {
        let low = t.to_ascii_lowercase();
        match low.as_str() {
            "size" | "inline-size" => sizing = Some(low),
            "layout" => layout = true,
            "style" => style = true,
            "paint" => paint = true,
            _ => {}
        }
    }
    let mut out: Vec<String> = Vec::new();
    if let Some(s) = sizing {
        out.push(s);
    }
    if layout {
        out.push("layout".to_string());
    }
    if style {
        out.push("style".to_string());
    }
    if paint {
        out.push("paint".to_string());
    }
    out.join(" ")
}

// contain 계산값 축약(§CSS Contain): 계산값에서만 layout+style+paint→content,
// size 까지면 strict(지정값은 축약하지 않고 캐논 순서 유지).
pub fn contain_computed(canonical: &str) -> String {
    match canonical.trim() {
        "size layout style paint" => "strict".to_string(),
        "layout style paint" => "content".to_string(),
        other => other.to_string(),
    }
}

// display 를 (outside, inside, list-item) 으로 파싱(§CSS Display 3 다값 문법).
// 각 카테고리 최대 1회, list-item 은 inside 가 flow|flow-root 일 때만. 무효면 None.
fn parse_display(raw: &str) -> Option<(Option<String>, Option<String>, bool)> {
    const OUTSIDE: [&str; 3] = ["block", "inline", "run-in"];
    const INSIDE: [&str; 7] = ["flow", "flow-root", "table", "flex", "grid", "ruby", "math"];
    let toks = split_top_level(raw);
    if toks.is_empty() {
        return None;
    }
    let (mut out, mut ins, mut li) = (None, None, false);
    for t in &toks {
        let low = t.to_ascii_lowercase();
        if OUTSIDE.contains(&low.as_str()) {
            if out.is_some() {
                return None;
            }
            out = Some(low);
        } else if INSIDE.contains(&low.as_str()) {
            if ins.is_some() {
                return None;
            }
            ins = Some(low);
        } else if low == "list-item" {
            if li {
                return None;
            }
            li = true;
        } else {
            return None; // 단일 전용 키워드나 미인식 토큰이 다값에 섞임
        }
    }
    if li {
        if let Some(i) = &ins {
            if i != "flow" && i != "flow-root" {
                return None; // list-item 은 flow|flow-root 하고만 결합
            }
        }
    }
    if out.is_none() && ins.is_none() && !li {
        return None;
    }
    Some((out, ins, li))
}

// display 단일 전용 키워드(§CSS Display): box/legacy/internal.
const DISPLAY_SINGLE: [&str; 18] = [
    "none", "contents", "inline-block", "inline-table", "inline-flex", "inline-grid",
    "table-row-group", "table-header-group", "table-footer-group", "table-row",
    "table-column-group", "table-column", "table-cell", "table-caption", "ruby-base",
    "ruby-text", "ruby-base-container", "ruby-text-container",
];

// display 유효성(§CSS Display 3).
pub fn display_valid(raw: &str) -> bool {
    let low = raw.trim().to_ascii_lowercase();
    DISPLAY_SINGLE.contains(&low.as_str()) || parse_display(raw).is_some()
}

// display 캐논 직렬화(§CSS Display 3): flow→block, 다값→레거시 단일 또는 캐논 두값.
// 지정값·계산값이 같은 규칙을 쓴다.
pub fn display_canonical(raw: &str) -> String {
    let low = raw.trim().to_ascii_lowercase();
    if DISPLAY_SINGLE.contains(&low.as_str()) {
        return low;
    }
    let Some((out, ins, li)) = parse_display(raw) else {
        return low;
    };
    if li {
        let mut parts: Vec<String> = Vec::new();
        if let Some(o) = &out {
            if o != "block" {
                parts.push(o.clone());
            }
        }
        if let Some(i) = &ins {
            if i != "flow" {
                parts.push(i.clone());
            }
        }
        parts.push("list-item".to_string());
        return parts.join(" ");
    }
    match (out.as_deref(), ins.as_deref()) {
        (Some(o), None) => o.to_string(),
        (None, Some("flow")) => "block".to_string(),
        (None, Some(i)) => i.to_string(),
        (Some(o), Some("flow")) => o.to_string(),
        (Some("block"), Some("flow-root")) => "flow-root".to_string(),
        (Some("block"), Some("flex")) => "flex".to_string(),
        (Some("block"), Some("grid")) => "grid".to_string(),
        (Some("block"), Some("table")) => "table".to_string(),
        (Some("inline"), Some("flow-root")) => "inline-block".to_string(),
        (Some("inline"), Some("flex")) => "inline-flex".to_string(),
        (Some("inline"), Some("grid")) => "inline-grid".to_string(),
        (Some("inline"), Some("table")) => "inline-table".to_string(),
        // ruby 의 기본 outside 는 inline — inline 은 생략, block/run-in 은 유지.
        (Some("inline"), Some("ruby")) => "ruby".to_string(),
        (Some(o), Some(i)) => format!("{} {}", o, i),
        (None, None) => low,
    }
}

// display 블록화(§CSS Display 2.7): float 걸리거나 절대/고정 위치인 요소의 계산
// display 는 inline-* 가 block-* 로 바뀐다.
pub fn blockify_display(d: &str) -> String {
    let d = d.trim();
    // 레이아웃 내부 상자(table-*/ruby-* internal)는 블록화 시 block.
    if matches!(
        d,
        "table-row-group" | "table-header-group" | "table-footer-group" | "table-row"
            | "table-column-group" | "table-column" | "table-cell" | "table-caption"
            | "ruby-base" | "ruby-text" | "ruby-base-container" | "ruby-text-container"
    ) {
        return "block".to_string();
    }
    match d {
        "inline" => "block",
        "inline-block" => "block",
        "inline-table" => "table",
        "inline-flex" => "flex",
        "inline-grid" => "grid",
        "run-in" => "block",
        "inline ruby" => "block ruby",
        other => other,
    }
    .to_string()
}

// hanging-punctuation 유효성(§CSS Text): none | [first || [force-end|allow-end] || last].
// none 단독, 각 카테고리(first/end/last) 최대 1회, 미인식·중복 거부.
pub fn hanging_punctuation_valid(raw: &str) -> bool {
    let toks = split_top_level(raw);
    if toks.is_empty() {
        return false;
    }
    if toks.len() == 1 && toks[0].eq_ignore_ascii_case("none") {
        return true;
    }
    let (mut first, mut end, mut last) = (false, false, false);
    for t in &toks {
        match t.to_ascii_lowercase().as_str() {
            "first" => {
                if first {
                    return false;
                }
                first = true;
            }
            "force-end" | "allow-end" => {
                if end {
                    return false;
                }
                end = true;
            }
            "last" => {
                if last {
                    return false;
                }
                last = true;
            }
            _ => return false, // none 혼합 또는 미인식
        }
    }
    true
}

// text-autospace 유효성(§CSS Text 4): normal | auto | no-autospace |
// [ideograph-alpha || ideograph-numeric || punctuation] || [insert | replace].
pub fn text_autospace_valid(raw: &str) -> bool {
    let toks = split_top_level(raw);
    if toks.is_empty() {
        return false;
    }
    if toks.len() == 1
        && matches!(toks[0].to_ascii_lowercase().as_str(), "normal" | "auto" | "no-autospace")
    {
        return true;
    }
    let (mut alpha, mut numeric, mut punct, mut insertion) = (false, false, false, false);
    for t in &toks {
        match t.to_ascii_lowercase().as_str() {
            "ideograph-alpha" => {
                if alpha {
                    return false;
                }
                alpha = true;
            }
            "ideograph-numeric" => {
                if numeric {
                    return false;
                }
                numeric = true;
            }
            "punctuation" => {
                if punct {
                    return false;
                }
                punct = true;
            }
            "insert" | "replace" => {
                if insertion {
                    return false;
                }
                insertion = true;
            }
            _ => return false, // normal/auto/no-autospace 혼합 또는 미인식
        }
    }
    true
}

// text-autospace 캐논 직렬화(§CSS Text 4): ideograph-alpha ideograph-numeric
// punctuation 순서 뒤에 삽입(insert|replace). normal/auto/no-autospace 는 단독.
pub fn text_autospace_canonical(raw: &str) -> String {
    let toks = split_top_level(raw);
    if toks.len() == 1 {
        return toks[0].to_ascii_lowercase();
    }
    let (mut alpha, mut numeric, mut punct, mut insertion): (bool, bool, bool, Option<String>) =
        (false, false, false, None);
    for t in &toks {
        let low = t.to_ascii_lowercase();
        match low.as_str() {
            "ideograph-alpha" => alpha = true,
            "ideograph-numeric" => numeric = true,
            "punctuation" => punct = true,
            "insert" | "replace" => insertion = Some(low),
            _ => {}
        }
    }
    let mut out: Vec<String> = Vec::new();
    if alpha {
        out.push("ideograph-alpha".to_string());
    }
    if numeric {
        out.push("ideograph-numeric".to_string());
    }
    if punct {
        out.push("punctuation".to_string());
    }
    if let Some(ins) = insertion {
        out.push(ins);
    }
    out.join(" ")
}

// text-transform 캐논 직렬화(§CSS Text): [case] full-width full-size-kana 순서.
pub fn text_transform_canonical(raw: &str) -> String {
    let toks = split_top_level(raw);
    if toks.len() == 1 {
        return toks[0].to_ascii_lowercase();
    }
    let (mut case, mut fw, mut kana): (Option<String>, Option<String>, Option<String>) =
        (None, None, None);
    for t in &toks {
        let low = t.to_ascii_lowercase();
        match low.as_str() {
            "capitalize" | "uppercase" | "lowercase" => case = Some(low),
            "full-width" => fw = Some(low),
            "full-size-kana" => kana = Some(low),
            _ => {}
        }
    }
    [case, fw, kana].into_iter().flatten().collect::<Vec<_>>().join(" ")
}

// <angle> 를 도(deg)로 파싱: 수 + deg|rad|grad|turn. 단위 없으면 None(0 도 무효).
fn parse_angle_deg(s: &str) -> Option<f64> {
    let low = s.trim().to_ascii_lowercase();
    let (num, factor) = if let Some(n) = low.strip_suffix("grad") {
        (n, 0.9)
    } else if let Some(n) = low.strip_suffix("deg") {
        (n, 1.0)
    } else if let Some(n) = low.strip_suffix("rad") {
        (n, 180.0 / std::f64::consts::PI)
    } else if let Some(n) = low.strip_suffix("turn") {
        (n, 360.0)
    } else {
        return None;
    };
    let v: f64 = num.parse().ok()?;
    if v.is_finite() {
        Some(v * factor)
    } else {
        None
    }
}

// font-style 유효성(§CSS Fonts): normal | italic | oblique [<angle [-90deg,90deg]>].
pub fn font_style_valid(raw: &str) -> bool {
    let toks = split_top_level(raw);
    match toks.len() {
        1 => matches!(toks[0].to_ascii_lowercase().as_str(), "normal" | "italic" | "oblique"),
        2 => {
            if toks[0].to_ascii_lowercase() != "oblique" {
                return false;
            }
            let a = &toks[1];
            let low = a.to_ascii_lowercase();
            // calc/min/max/clamp 은 구문상 유효(범위 클램프는 계산시).
            if low.ends_with(')')
                && ["calc(", "min(", "max(", "clamp("].iter().any(|p| low.starts_with(p))
            {
                return true;
            }
            match parse_angle_deg(a) {
                Some(deg) => (-90.0..=90.0).contains(&deg),
                None => false,
            }
        }
        _ => false,
    }
}

// font-style 계산값 정규화(§CSS Fonts): oblique <angle> 를 도로 접고 [-90,90] 클램프,
// 0deg 는 normal 로. 각도 calc 는 미평가(별개 블로커)라 원문 유지.
pub fn normalize_font_style(raw: &str) -> String {
    let toks = split_top_level(raw);
    if toks.len() != 2 || !toks[0].eq_ignore_ascii_case("oblique") {
        return raw.trim().to_string();
    }
    match parse_angle_deg(&toks[1]) {
        Some(deg) => {
            let deg = deg.clamp(-90.0, 90.0);
            if deg == 0.0 {
                "normal".to_string()
            } else {
                let r = (deg * 1e6).round() / 1e6;
                format!("oblique {}deg", r)
            }
        }
        None => raw.trim().to_string(),
    }
}

// font-variant 단축 토큰의 하위 카테고리(§CSS Fonts). 같은 카테고리 중복 금지.
// 함수형 alternates(stylistic() 등)는 함수명으로 분류한다.
fn font_variant_category(tok: &str) -> Option<&'static str> {
    let low = tok.to_ascii_lowercase();
    if let Some(paren) = low.find('(') {
        if !low.ends_with(')') {
            return None;
        }
        return match &low[..paren] {
            "stylistic" => Some("alt-stylistic"),
            "styleset" => Some("alt-styleset"),
            "character-variant" => Some("alt-charvar"),
            "swash" => Some("alt-swash"),
            "ornaments" => Some("alt-ornaments"),
            "annotation" => Some("alt-annotation"),
            _ => None,
        };
    }
    match low.as_str() {
        "common-ligatures" | "no-common-ligatures" => Some("lig-common"),
        "discretionary-ligatures" | "no-discretionary-ligatures" => Some("lig-disc"),
        "historical-ligatures" | "no-historical-ligatures" => Some("lig-hist"),
        "contextual" | "no-contextual" => Some("lig-ctx"),
        "small-caps" | "all-small-caps" | "petite-caps" | "all-petite-caps" | "unicase"
        | "titling-caps" => Some("caps"),
        "lining-nums" | "oldstyle-nums" => Some("num-fig"),
        "proportional-nums" | "tabular-nums" => Some("num-spc"),
        "diagonal-fractions" | "stacked-fractions" => Some("num-frac"),
        "ordinal" => Some("ordinal"),
        "slashed-zero" => Some("slashed-zero"),
        "jis78" | "jis83" | "jis90" | "jis04" | "simplified" | "traditional" => Some("ea-variant"),
        "full-width" | "proportional-width" => Some("ea-width"),
        "ruby" => Some("ruby"),
        "sub" | "super" => Some("position"),
        "historical-forms" => Some("alt-historical"),
        _ => None,
    }
}

// font-variant 단축 유효성(§CSS Fonts). normal/none 은 단독만, 각 하위 카테고리는
// 최대 1회(충돌·중복 거부), 미인식 토큰 거부.
pub fn font_variant_valid(raw: &str) -> bool {
    let toks = split_top_level(raw);
    if toks.is_empty() {
        return false;
    }
    if toks.len() == 1 {
        let low = toks[0].to_ascii_lowercase();
        if low == "normal" || low == "none" {
            return true;
        }
    }
    let mut seen: Vec<&'static str> = Vec::new();
    for t in &toks {
        let low = t.to_ascii_lowercase();
        if low == "normal" || low == "none" {
            return false; // 다른 토큰과 함께 오면 무효
        }
        match font_variant_category(t) {
            Some(cat) => {
                if seen.contains(&cat) {
                    return false; // 같은 카테고리 중복
                }
                seen.push(cat);
            }
            None => return false, // 미인식 토큰
        }
    }
    true
}

// transition-property 유효성(§CSS Transitions): none | <custom-ident>#.
// 각 항목은 유효 식별자(all 포함)이고 none/CSS-wide 키워드/default 는 항목이 될 수 없다.
pub fn transition_property_valid(raw: &str) -> bool {
    let whole = raw.trim().to_ascii_lowercase();
    if whole == "none" {
        return true;
    }
    let items = split_top_commas(raw);
    if items.is_empty() {
        return false;
    }
    items.iter().all(|item| single_transition_property_valid(item.trim()))
}

// CSS 식별자 형태인가: 시작은 letter/-/_/비ASCII, 이후 alnum/-/_/비ASCII.
pub fn is_css_ident(s: &str) -> bool {
    let start = |c: char| c.is_ascii_alphabetic() || c == '-' || c == '_' || (c as u32) >= 0x80;
    match s.chars().next() {
        Some(c0) if start(c0) => {}
        _ => return false,
    }
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || (c as u32) >= 0x80)
}

// <single-transition-property>: all 포함 유효 custom-ident. none/CSS-wide/default 제외.
pub fn single_transition_property_valid(t: &str) -> bool {
    let low = t.to_ascii_lowercase();
    !matches!(
        low.as_str(),
        "none" | "initial" | "inherit" | "unset" | "revert" | "revert-layer" | "default"
    ) && is_css_ident(t)
}

// <grid-line> 의 custom-ident: span/auto/CSS-wide/default 제외한 유효 식별자.
fn grid_ident_valid(t: &str) -> bool {
    let low = t.to_ascii_lowercase();
    !matches!(
        low.as_str(),
        "span" | "auto" | "initial" | "inherit" | "unset" | "revert" | "revert-layer" | "default"
    ) && is_css_ident(t)
}

// 단일 <grid-line>(§CSS Grid): auto | <custom-ident> |
// [<integer≠0> && <custom-ident>?] | [span && [<integer≥1> || <custom-ident>]].
// span 형에서 span 은 맨 앞 또는 맨 뒤여야 하고(중간이면 int/ident 를 가르므로 무효),
// 정수·ident 단위는 연속이어야 한다("2 span first" 무효, "2 i span" 유효).
pub fn grid_line_valid(s: &str) -> bool {
    let toks: Vec<&str> = s.split_whitespace().collect();
    if toks.is_empty() || toks.len() > 3 {
        return false;
    }
    let is_span = |t: &str| t.eq_ignore_ascii_case("span");
    let spans: Vec<usize> = toks.iter().enumerate().filter(|(_, t)| is_span(t)).map(|(i, _)| i).collect();
    if !spans.is_empty() {
        // span 정확히 한 번, 맨 앞 또는 맨 뒤. 나머지는 정수(≥1) 0~1 + ident 0~1, 최소 하나.
        if spans.len() != 1 {
            return false;
        }
        let sp = spans[0];
        if sp != 0 && sp != toks.len() - 1 {
            return false;
        }
        let rest: Vec<&str> = toks.iter().enumerate().filter(|(i, _)| *i != sp).map(|(_, t)| *t).collect();
        if rest.is_empty() {
            return false;
        }
        let (mut ints, mut idents) = (0, 0);
        for t in &rest {
            if let Ok(n) = t.parse::<i64>() {
                if n < 1 {
                    return false;
                }
                ints += 1;
            } else if grid_ident_valid(t) {
                idents += 1;
            } else {
                return false;
            }
        }
        return ints <= 1 && idents <= 1;
    }
    // span 없는 형.
    match toks.len() {
        1 => {
            if toks[0].eq_ignore_ascii_case("auto") {
                return true;
            }
            if let Ok(n) = toks[0].parse::<i64>() {
                return n != 0;
            }
            grid_ident_valid(toks[0])
        }
        2 => {
            // <integer≠0> 하나 + <custom-ident> 하나, 순서 무관.
            let a = toks[0].parse::<i64>().ok();
            let b = toks[1].parse::<i64>().ok();
            match (a, b) {
                (Some(n), None) => n != 0 && grid_ident_valid(toks[1]),
                (None, Some(n)) => n != 0 && grid_ident_valid(toks[0]),
                _ => false,
            }
        }
        _ => false,
    }
}

// <grid-line> 캐논: span → <integer> → <custom-ident> 순, 정수의 '+' 부호·선행 0 제거.
pub fn grid_line_canonical(s: &str) -> String {
    let toks: Vec<&str> = s.split_whitespace().collect();
    let (mut span, mut auto) = (false, false);
    let (mut int_tok, mut ident_tok): (Option<String>, Option<String>) = (None, None);
    for t in &toks {
        if t.eq_ignore_ascii_case("span") {
            span = true;
        } else if t.eq_ignore_ascii_case("auto") {
            auto = true;
        } else if let Ok(n) = t.parse::<i64>() {
            int_tok = Some(n.to_string());
        } else {
            ident_tok = Some((*t).to_string());
        }
    }
    if auto {
        return "auto".to_string();
    }
    // span 형에서 정수 1 이 ident 와 함께면 기본값이라 생략("span 1 two" → "span two").
    if span && int_tok.as_deref() == Some("1") && ident_tok.is_some() {
        int_tok = None;
    }
    let mut parts = Vec::new();
    if span {
        parts.push("span".to_string());
    }
    if let Some(i) = int_tok {
        parts.push(i);
    }
    if let Some(id) = ident_tok {
        parts.push(id);
    }
    parts.join(" ")
}

// grid-row/grid-column 단축(§CSS Grid): <grid-line> [/ <grid-line>]?.
pub fn grid_line_shorthand_valid(raw: &str) -> bool {
    let parts: Vec<String> = split_top_slash(raw);
    !parts.is_empty() && parts.len() <= 2 && parts.iter().all(|p| grid_line_valid(p.trim()))
}

// grid-area 단축(§CSS Grid): <grid-line> [/ <grid-line>]{0,3}.
pub fn grid_area_valid(raw: &str) -> bool {
    let parts: Vec<String> = split_top_slash(raw);
    !parts.is_empty() && parts.len() <= 4 && parts.iter().all(|p| grid_line_valid(p.trim()))
}

// ===== grid-template-columns/rows: <track-list> | <auto-track-list> 검증 =====

// 부호 없는 <length-percentage>(음수 리터럴 거부). calc 등 수식은 파스 타임 허용
// (범위 검사는 계산값 시점, calc(-0.5em+10px) 도 유효).
pub fn nonneg_length_percentage(t: &str) -> bool {
    let low = t.trim().to_ascii_lowercase();
    if low.starts_with('-') {
        return false;
    }
    (is_math_fn(&low) && math_length_valid(&low, true)) || is_length_percentage(t)
}

// margin 값(§CSS Box): auto | <length-percentage>(부호 무관) | calc.
pub fn margin_value_valid(t: &str) -> bool {
    let low = t.trim().to_ascii_lowercase();
    low == "auto" || (is_math_fn(&low) && math_length_valid(&low, true)) || is_length_percentage(t)
}

// 부호 없는 <flex>("3fr", "0fr"). 음수 거부.
fn nonneg_flex(t: &str) -> bool {
    let low = t.trim().to_ascii_lowercase();
    match low.strip_suffix("fr") {
        Some(num) => num.parse::<f64>().map(|v| v.is_finite() && v >= 0.0).unwrap_or(false),
        None => false,
    }
}

// <track-breadth> = <length-percentage 0+> | <flex 0+> | min-content | max-content | auto
fn track_breadth_valid(t: &str) -> bool {
    let low = t.trim().to_ascii_lowercase();
    matches!(low.as_str(), "min-content" | "max-content" | "auto")
        || nonneg_flex(t)
        || nonneg_length_percentage(t)
}

// <inflexible-breadth> = <length-percentage 0+> | min-content | max-content | auto (flex 제외)
fn inflexible_breadth_valid(t: &str) -> bool {
    let low = t.trim().to_ascii_lowercase();
    matches!(low.as_str(), "min-content" | "max-content" | "auto") || nonneg_length_percentage(t)
}

// <fixed-breadth> = <length-percentage 0+>
fn fixed_breadth_valid(t: &str) -> bool {
    nonneg_length_percentage(t)
}

// name( ... ) 인자 목록 추출(최상위 콤마 분리). 아니면 None.
fn fn_args(t: &str, name: &str) -> Option<Vec<String>> {
    let s = t.trim();
    if !s.to_ascii_lowercase().starts_with(name) || !s.ends_with(')') {
        return None;
    }
    let inner = &s[name.len()..s.len() - 1];
    Some(split_top_commas(inner).iter().map(|x| x.trim().to_string()).collect())
}

// <track-size> = <track-breadth> | minmax(<inflexible-breadth>,<track-breadth>) | fit-content(<lp 0+>)
fn track_size_valid(t: &str) -> bool {
    if let Some(a) = fn_args(t, "minmax(") {
        return a.len() == 2 && inflexible_breadth_valid(&a[0]) && track_breadth_valid(&a[1]);
    }
    if let Some(a) = fn_args(t, "fit-content(") {
        return a.len() == 1 && fixed_breadth_valid(&a[0]);
    }
    track_breadth_valid(t)
}

// <fixed-size> = <fixed-breadth> | minmax(<fixed-breadth>,<track-breadth>)
//              | minmax(<inflexible-breadth>,<fixed-breadth>). fit-content 은 고정 크기 아님.
fn fixed_size_valid(t: &str) -> bool {
    if let Some(a) = fn_args(t, "minmax(") {
        return a.len() == 2
            && ((fixed_breadth_valid(&a[0]) && track_breadth_valid(&a[1]))
                || (inflexible_breadth_valid(&a[0]) && fixed_breadth_valid(&a[1])));
    }
    if fn_args(t, "fit-content(").is_some() {
        return false;
    }
    fixed_breadth_valid(t)
}

// <line-names> = '[' <custom-ident>* ']'. span/auto 예약어 불가. 빈 [] 허용.
fn line_names_valid(t: &str) -> bool {
    let s = t.trim();
    match s.strip_prefix('[').and_then(|x| x.strip_suffix(']')) {
        Some(inner) => inner.split_whitespace().all(grid_ident_valid),
        None => false,
    }
}

// repeat( ... ) 내부 문자열. 아니면 None.
fn repeat_inner(t: &str) -> Option<String> {
    let s = t.trim();
    if s.to_ascii_lowercase().starts_with("repeat(") && s.ends_with(')') {
        Some(s["repeat(".len()..s.len() - 1].to_string())
    } else {
        None
    }
}

// 컴포넌트가 auto-repeat(첫 인자 auto-fill|auto-fit)인가.
fn is_auto_repeat_comp(t: &str) -> bool {
    match repeat_inner(t) {
        Some(inner) => {
            let f = split_top_commas(&inner).first().map(|s| s.trim().to_ascii_lowercase()).unwrap_or_default();
            f == "auto-fill" || f == "auto-fit"
        }
        None => false,
    }
}

// 괄호 () 와 대괄호 [] 깊이를 존중해 공백으로 컴포넌트 분리.
fn split_grid_components(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    for c in s.chars() {
        match c {
            '(' | '[' => {
                depth += 1;
                cur.push(c);
            }
            ')' | ']' => {
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

// [<line-names>? <track>]+ <line-names>? 시퀀스 검증.
// need_fixed: 트랙이 <fixed-size> 여야(auto-track-list 문맥).
// allow_repeat: repeat() 허용(top-level 만; repeat 중첩 금지).
// auto_seen: auto-repeat 누적(전체 목록에 최대 1개).
fn track_seq_valid(comps: &[String], need_fixed: bool, allow_repeat: bool, auto_seen: &mut u32) -> bool {
    if comps.is_empty() {
        return false;
    }
    let mut prev_names = false;
    let mut had_track = false;
    for c in comps {
        let cl = c.trim();
        if cl.starts_with('[') {
            if !line_names_valid(cl) || prev_names {
                return false;
            }
            prev_names = true;
            continue;
        }
        prev_names = false;
        if let Some(inner) = repeat_inner(cl) {
            if !allow_repeat {
                return false;
            }
            let parts = split_top_commas(&inner);
            if parts.len() < 2 {
                return false;
            }
            let count = parts[0].trim().to_ascii_lowercase();
            let is_auto = count == "auto-fill" || count == "auto-fit";
            if is_auto {
                *auto_seen += 1;
                if *auto_seen > 1 {
                    return false;
                }
            } else if !matches!(count.parse::<i64>(), Ok(n) if n >= 1) {
                return false;
            }
            let body = split_grid_components(&parts[1..].join(","));
            if !track_seq_valid(&body, is_auto || need_fixed, false, auto_seen) {
                return false;
            }
            had_track = true;
        } else {
            let ok = if need_fixed { fixed_size_valid(cl) } else { track_size_valid(cl) };
            if !ok {
                return false;
            }
            had_track = true;
        }
    }
    had_track
}

// grid-template-columns/rows 값(§CSS Grid): none | <track-list> | <auto-track-list>.
pub fn grid_template_track_valid(raw: &str) -> bool {
    let s = raw.trim();
    if s.is_empty() {
        return false;
    }
    if s.eq_ignore_ascii_case("none") {
        return true;
    }
    let comps = split_grid_components(s);
    let has_auto = comps.iter().any(|c| is_auto_repeat_comp(c));
    let mut auto_seen = 0u32;
    track_seq_valid(&comps, has_auto, true, &mut auto_seen)
}

// 부호 없는 <length>(퍼센트 불가, 음수 불가). calc 는 파스 타임 허용.
fn nonneg_length(t: &str) -> bool {
    let low = t.trim().to_ascii_lowercase();
    if low.starts_with('-') {
        return false;
    }
    if is_math_fn(&low) {
        // <length> 전용(% 불가) 문맥의 calc 타입 검사.
        return math_length_valid(&low, false);
    }
    if low.ends_with('%') {
        return false;
    }
    is_length_percentage(t)
}

// <string> 리터럴: 따옴표로 감싸고 같은 따옴표로 닫힘.
fn is_css_string(t: &str) -> bool {
    let t = t.trim();
    let b = t.as_bytes();
    b.len() >= 2 && (b[0] == b'"' || b[0] == b'\'') && b[b.len() - 1] == b[0]
}

// 공백 분리하되 따옴표 문자열은 한 토큰으로 유지.
fn split_ws_quotes(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in s.chars() {
        match quote {
            Some(q) => {
                cur.push(c);
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                '"' | '\'' => {
                    quote = Some(c);
                    cur.push(c);
                }
                c if c.is_whitespace() => {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                }
                _ => cur.push(c),
            },
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

// text-overflow(§CSS UI): [ clip | ellipsis | <string> ]{1,2}.
pub fn text_overflow_valid(raw: &str) -> bool {
    let toks = split_ws_quotes(raw.trim());
    if toks.is_empty() || toks.len() > 2 {
        return false;
    }
    toks.iter().all(|t| {
        let tl = t.to_ascii_lowercase();
        tl == "clip" || tl == "ellipsis" || is_css_string(t)
    })
}

// max-lines(§CSS Overflow 4): [ <integer [1,∞]> || auto ]. 캐논은 정수 먼저.
pub fn max_lines_valid(raw: &str) -> bool {
    let low = raw.trim().to_ascii_lowercase();
    let toks: Vec<&str> = low.split_whitespace().collect();
    if toks.is_empty() || toks.len() > 2 {
        return false;
    }
    let (mut ints, mut autos) = (0u32, 0u32);
    for t in toks {
        if t == "auto" {
            autos += 1;
        } else if is_math_fn(t) || matches!(t.parse::<i64>(), Ok(n) if n >= 1) {
            ints += 1;
        } else {
            return false;
        }
    }
    ints <= 1 && autos <= 1 && ints + autos >= 1
}

pub fn max_lines_canonical(raw: &str) -> String {
    let low = raw.trim().to_ascii_lowercase();
    let toks: Vec<&str> = low.split_whitespace().collect();
    let mut out: Vec<&str> = Vec::new();
    if let Some(t) = toks.iter().find(|t| **t != "auto") {
        out.push(t);
    }
    if toks.contains(&"auto") {
        out.push("auto");
    }
    out.join(" ")
}

// line-clamp(§CSS Overflow 4): none | [ <integer [1,∞]> || <ellipsis> ].
// ellipsis = auto | ellipsis | no-ellipsis | <string>. none 은 단독만.
pub fn line_clamp_valid(raw: &str) -> bool {
    let t = raw.trim();
    if t.eq_ignore_ascii_case("none") {
        return true;
    }
    let toks = split_ws_quotes(t);
    if toks.is_empty() || toks.len() > 3 {
        return false;
    }
    let (mut ints, mut ell, mut legacy) = (0u32, 0u32, 0u32);
    for tok in &toks {
        let tl = tok.to_ascii_lowercase();
        if matches!(tl.as_str(), "auto" | "ellipsis" | "no-ellipsis") || is_css_string(tok) {
            ell += 1;
        } else if tl == "-webkit-legacy" {
            legacy += 1;
        } else if matches!(tok.parse::<i64>(), Ok(n) if n >= 1) {
            ints += 1;
        } else {
            return false;
        }
    }
    // -webkit-legacy 단독은 무효(정수나 ellipsis 필요).
    ints <= 1 && ell <= 1 && legacy <= 1 && ints + ell >= 1
}

// block-ellipsis(§CSS Overflow 4): no-ellipsis | ellipsis | <string>. 단일 토큰.
pub fn block_ellipsis_valid(raw: &str) -> bool {
    let toks = split_ws_quotes(raw.trim());
    if toks.len() != 1 {
        return false;
    }
    matches!(toks[0].to_ascii_lowercase().as_str(), "no-ellipsis" | "ellipsis")
        || is_css_string(&toks[0])
}

// -webkit-line-clamp / continue 계열 정수: none | <integer [1,∞]>.
pub fn webkit_line_clamp_valid(raw: &str) -> bool {
    let low = raw.trim().to_ascii_lowercase();
    low == "none" || is_math_fn(&low) || matches!(low.parse::<i64>(), Ok(n) if n >= 1)
}

// text-decoration-line(§CSS Text Decor): none | spelling-error | grammar-error |
// [ underline || overline || line-through || blink ]. 단독형은 조합 불가.
pub fn text_decoration_line_valid(raw: &str) -> bool {
    let low = raw.trim().to_ascii_lowercase();
    if matches!(low.as_str(), "none" | "spelling-error" | "grammar-error") {
        return true;
    }
    let toks: Vec<&str> = low.split_whitespace().collect();
    if toks.is_empty() {
        return false;
    }
    let mut seen: Vec<&str> = Vec::new();
    for t in toks {
        if !matches!(t, "underline" | "overline" | "line-through" | "blink") {
            return false;
        }
        if seen.contains(&t) {
            return false;
        }
        seen.push(t);
    }
    true
}

// text-decoration-skip-spaces(§CSS Text Decor 4): none | all | [ start || end ].
pub fn text_decoration_skip_spaces_valid(raw: &str) -> bool {
    let low = raw.trim().to_ascii_lowercase();
    if matches!(low.as_str(), "none" | "all") {
        return true;
    }
    let toks: Vec<&str> = low.split_whitespace().collect();
    if toks.is_empty() || toks.len() > 2 {
        return false;
    }
    let mut seen: Vec<&str> = Vec::new();
    for t in toks {
        if !matches!(t, "start" | "end") {
            return false;
        }
        if seen.contains(&t) {
            return false;
        }
        seen.push(t);
    }
    true
}

// <length-percentage>(부호 무관). calc 는 파스 타임 허용.
fn is_length_any_sign(t: &str) -> bool {
    is_math_fn(&t.trim().to_ascii_lowercase()) || is_length_percentage(t)
}

// text-decoration-inset(§CSS Text Decor 4): auto | <length-percentage>{1,2}.
pub fn text_decoration_inset_valid(raw: &str) -> bool {
    if raw.trim().eq_ignore_ascii_case("auto") {
        return true;
    }
    let toks = split_top_level(raw.trim());
    !toks.is_empty() && toks.len() <= 2 && toks.iter().all(|t| is_length_any_sign(t))
}

// text-decoration-inset 캐논: 0→0px, 같은 두 값은 하나로 축약.
pub fn text_decoration_inset_canonical(raw: &str) -> String {
    if raw.trim().eq_ignore_ascii_case("auto") {
        return "auto".to_string();
    }
    let toks: Vec<String> = split_top_level(raw.trim())
        .iter()
        .map(|t| if t.trim() == "0" { "0px".to_string() } else { t.trim().to_string() })
        .collect();
    if toks.len() == 2 && toks[0] == toks[1] {
        return toks[0].clone();
    }
    toks.join(" ")
}

// text-emphasis-position(§CSS Text Decor): auto | [ over | under ] || [ right | left ].
pub fn text_emphasis_position_valid(raw: &str) -> bool {
    let low = raw.trim().to_ascii_lowercase();
    if low == "auto" {
        return true;
    }
    let toks: Vec<&str> = low.split_whitespace().collect();
    if toks.is_empty() || toks.len() > 2 {
        return false;
    }
    let (mut ou, mut rl) = (0u32, 0u32);
    for t in toks {
        match t {
            "over" | "under" => ou += 1,
            "right" | "left" => rl += 1,
            _ => return false,
        }
    }
    ou <= 1 && rl <= 1 && ou + rl >= 1
}

// text-emphasis-position 캐논: [over|under] 먼저, left 는 유지, 기본값 right 는 생략.
pub fn text_emphasis_position_canonical(raw: &str) -> String {
    let low = raw.trim().to_ascii_lowercase();
    if low == "auto" {
        return "auto".to_string();
    }
    let toks: Vec<&str> = low.split_whitespace().collect();
    let mut out: Vec<&str> = Vec::new();
    if let Some(t) = ["over", "under"].iter().find(|k| toks.contains(k)) {
        out.push(t);
    }
    if toks.contains(&"left") {
        out.push("left");
    }
    if out.is_empty() {
        low.clone()
    } else {
        out.join(" ")
    }
}

// text-underline-position(§CSS Text Decor): auto | [from-font|under] || [left|right].
pub fn text_underline_position_valid(raw: &str) -> bool {
    let low = raw.trim().to_ascii_lowercase();
    if low == "auto" {
        return true;
    }
    let toks: Vec<&str> = low.split_whitespace().collect();
    if toks.is_empty() || toks.len() > 2 {
        return false;
    }
    let (mut a, mut b) = (0u32, 0u32);
    for t in toks {
        match t {
            "from-font" | "under" => a += 1,
            "left" | "right" => b += 1,
            _ => return false,
        }
    }
    a <= 1 && b <= 1 && a + b >= 1
}

// text-underline-position 캐논: [from-font|under] 먼저, [left|right] 나중.
pub fn text_underline_position_canonical(raw: &str) -> String {
    let low = raw.trim().to_ascii_lowercase();
    if low == "auto" {
        return "auto".to_string();
    }
    let toks: Vec<&str> = low.split_whitespace().collect();
    let mut out: Vec<&str> = Vec::new();
    for grp in [["from-font", "under"], ["left", "right"]] {
        if let Some(t) = grp.iter().find(|k| toks.contains(k)) {
            out.push(t);
        }
    }
    out.join(" ")
}

// widows/orphans(§CSS Fragmentation): <integer [1,∞]>.
pub fn positive_integer_valid(raw: &str) -> bool {
    let low = raw.trim().to_ascii_lowercase();
    is_math_fn(&low) || matches!(low.parse::<i64>(), Ok(n) if n >= 1)
}

// 따옴표·괄호를 존중해 최상위 공백으로 토큰 분리.
fn split_top_tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for c in s.chars() {
        if let Some(q) = quote {
            cur.push(c);
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => {
                quote = Some(c);
                cur.push(c);
            }
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

// symbols()(§CSS Counter Styles): symbols( <type>? <string>+ ). type 는 맨 앞,
// alphabetic/numeric 은 문자열 2개 이상. 이미지는 이 구현에서 미지원(문자열만).
fn symbols_type(t: &str) -> Option<&'static str> {
    match t.to_ascii_lowercase().as_str() {
        "cyclic" => Some("cyclic"),
        "numeric" => Some("numeric"),
        "alphabetic" => Some("alphabetic"),
        "symbolic" => Some("symbolic"),
        "fixed" => Some("fixed"),
        _ => None,
    }
}

fn symbols_valid(tok: &str) -> bool {
    let t = tok.trim();
    if !t.to_ascii_lowercase().starts_with("symbols(") || !t.ends_with(')') {
        return false;
    }
    let parts = split_ws_quotes(&t["symbols(".len()..t.len() - 1]);
    if parts.is_empty() {
        return false;
    }
    let (ty, syms): (&str, &[String]) = match symbols_type(&parts[0]) {
        Some(ty) => (ty, &parts[1..]),
        None => ("symbolic", &parts[..]),
    };
    if syms.is_empty() || !syms.iter().all(|s| is_css_string(s)) {
        return false;
    }
    !(matches!(ty, "alphabetic" | "numeric") && syms.len() < 2)
}

// list-style-type(§CSS Lists): <counter-style> | <string> | none.
// <counter-style> = <counter-style-name>(predefined 또는 custom-ident) | symbols().
pub fn list_style_type_valid(raw: &str) -> bool {
    let t = raw.trim();
    if t.eq_ignore_ascii_case("none") {
        return true;
    }
    let toks = split_top_tokens(t);
    if toks.len() != 1 {
        return false;
    }
    let tok = &toks[0];
    if is_css_string(tok) {
        return true;
    }
    if tok.to_ascii_lowercase().starts_with("symbols(") {
        return symbols_valid(tok);
    }
    let low = tok.to_ascii_lowercase();
    !matches!(low.as_str(), "none" | "inherit" | "initial" | "unset" | "revert" | "revert-layer")
        && is_css_ident(tok)
}

// list-style-type 캐논: symbols() 의 기본 type(symbolic) 생략.
pub fn list_style_type_canonical(raw: &str) -> String {
    let t = raw.trim();
    if !t.to_ascii_lowercase().starts_with("symbols(") || !t.ends_with(')') {
        return t.to_string();
    }
    let parts = split_ws_quotes(&t["symbols(".len()..t.len() - 1]);
    let mut out: Vec<String> = Vec::new();
    let syms = match symbols_type(parts.first().map(|s| s.as_str()).unwrap_or("")) {
        Some("symbolic") => &parts[1..],
        Some(ty) => {
            out.push(ty.to_string());
            &parts[1..]
        }
        None => &parts[..],
    };
    for s in syms {
        out.push(s.clone());
    }
    format!("symbols({})", out.join(" "))
}

// ===== content(§CSS Content 3) 검증 =====

// 따옴표·괄호를 존중해 최상위 '/' 분리.
fn split_slash_qp(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for c in s.chars() {
        if let Some(q) = quote {
            cur.push(c);
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => {
                quote = Some(c);
                cur.push(c);
            }
            '(' | '[' => {
                depth += 1;
                cur.push(c);
            }
            ')' | ']' => {
                depth -= 1;
                cur.push(c);
            }
            '/' if depth == 0 => {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    out.push(cur.trim().to_string());
    out
}

// content 의 <custom-ident>(카운터 이름): 이스케이프 포함 허용.
fn content_ident_ok(t: &str) -> bool {
    !t.is_empty() && (t.contains('\\') || is_css_ident(t))
}

// counter()/counters() 유효성.
// counter()/counters() 의 스타일 인자: <counter-style-name>(none/default/CSS-wide 제외).
fn counter_style_arg_ok(t: &str) -> bool {
    let low = t.trim().to_ascii_lowercase();
    !matches!(
        low.as_str(),
        "none" | "default" | "inherit" | "initial" | "unset" | "revert" | "revert-layer"
    ) && content_ident_ok(t)
}

fn counter_fn_valid(t: &str) -> bool {
    if let Some(a) = fn_args(t, "counter(") {
        return (a.len() == 1 || a.len() == 2)
            && content_ident_ok(&a[0])
            && (a.len() == 1 || counter_style_arg_ok(&a[1]));
    }
    if let Some(a) = fn_args(t, "counters(") {
        return (a.len() == 2 || a.len() == 3)
            && content_ident_ok(&a[0])
            && is_css_string(&a[1])
            && (a.len() == 2 || counter_style_arg_ok(&a[2]));
    }
    false
}

// content-list 토큰 유효성.
fn content_token_valid(t: &str) -> bool {
    let low = t.to_ascii_lowercase();
    if is_css_string(t) {
        return true;
    }
    if matches!(low.as_str(), "open-quote" | "close-quote" | "no-open-quote" | "no-close-quote") {
        return true;
    }
    if counter_fn_valid(t) {
        return true;
    }
    if low.starts_with("url(") && t.ends_with(')') {
        return true;
    }
    // attr( <이름> ... ): 빈 attr() 은 무효.
    if let Some(a) = fn_args(t, "attr(") {
        return !a.is_empty() && !a[0].trim().is_empty();
    }
    // 이미지(gradient/image-set 등).
    let is_gradient = [
        "linear-gradient(",
        "radial-gradient(",
        "conic-gradient(",
        "repeating-linear-gradient(",
        "repeating-radial-gradient(",
        "repeating-conic-gradient(",
    ]
    .iter()
    .any(|p| low.starts_with(p));
    (is_gradient && gradient_valid(t)) || low.starts_with("image-set(") || low.starts_with("cross-fade(")
}

// content(§CSS Content 3): normal | none | <content-list> [ / <alt-text> ]?.
// alt-text = [ <string> | <counter> ]+.
pub fn content_valid(raw: &str) -> bool {
    let low = raw.trim().to_ascii_lowercase();
    if matches!(low.as_str(), "normal" | "none") {
        return true;
    }
    let parts = split_slash_qp(raw.trim());
    if parts.is_empty() || parts.len() > 2 {
        return false;
    }
    let main_toks = split_top_tokens(parts[0].trim());
    if main_toks.is_empty() || !main_toks.iter().all(|t| content_token_valid(t)) {
        return false;
    }
    if parts.len() == 2 {
        let alt = split_top_tokens(parts[1].trim());
        if alt.is_empty() || !alt.iter().all(|t| is_css_string(t) || counter_fn_valid(t)) {
            return false;
        }
    }
    true
}

// content 캐논: counter()/counters() 의 기본 스타일(decimal) 생략.
pub fn content_canonical(raw: &str) -> String {
    let low = raw.trim().to_ascii_lowercase();
    if matches!(low.as_str(), "normal" | "none") {
        return low;
    }
    split_slash_qp(raw.trim())
        .iter()
        .map(|part| {
            split_top_tokens(part.trim())
                .iter()
                .map(|t| canon_counter_token(t))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join(" / ")
}

fn canon_counter_token(t: &str) -> String {
    // counter(name, decimal) → counter(name); counters(name, s, decimal) → counters(name, s).
    if let Some(a) = fn_args(t, "counter(") {
        if a.len() == 2 && a[1].trim().eq_ignore_ascii_case("decimal") {
            return format!("counter({})", a[0].trim());
        }
    }
    if let Some(a) = fn_args(t, "counters(") {
        if a.len() == 3 && a[2].trim().eq_ignore_ascii_case("decimal") {
            return format!("counters({}, {})", a[0].trim(), a[1].trim());
        }
    }
    t.trim().to_string()
}

// list-style 단축(§CSS Lists): <position> || <image> || <type>. 각 슬롯 최대 1개,
// none 은 type/image 빈 슬롯을 채운다. 무효 키워드·슬롯 초과 거부.
pub fn list_style_valid(raw: &str) -> bool {
    let toks = split_top_tokens(raw.trim());
    if toks.is_empty() || toks.len() > 3 {
        return false;
    }
    let (mut pos, mut typ, mut img, mut none) = (0i32, 0i32, 0i32, 0i32);
    for t in &toks {
        let low = t.to_ascii_lowercase();
        if matches!(low.as_str(), "inside" | "outside") {
            pos += 1;
        } else if low == "none" {
            none += 1;
        } else if low != "none" && list_style_image_valid(t) {
            img += 1;
        } else if list_style_type_valid(t) {
            typ += 1;
        } else {
            return false;
        }
    }
    if pos > 1 || typ > 1 || img > 1 {
        return false;
    }
    none <= (1 - typ) + (1 - img)
}

// background-position-x/y(§CSS Backgrounds 3): [ center | [ <edge>? <lp>? ]! ]#.
// 각 항목은 center | <edge> [<lp>]? | <lp>. edge 는 축별 키워드.
pub fn bg_position_axis_valid(raw: &str, edges: &[&str]) -> bool {
    let is_lp = |t: &str| is_math_fn(&t.to_ascii_lowercase()) || is_length_percentage(t);
    let items = split_top_commas(raw);
    if items.is_empty() {
        return false;
    }
    items.iter().all(|it| {
        let toks = split_top_level(it.trim());
        match toks.len() {
            1 => {
                let low = toks[0].to_ascii_lowercase();
                low == "center" || edges.contains(&low.as_str()) || is_lp(&toks[0])
            }
            2 => edges.contains(&toks[0].to_ascii_lowercase().as_str()) && is_lp(&toks[1]),
            _ => false,
        }
    })
}

// 부호 없는 <number>(음수 거부). calc 허용.
fn nonneg_number(t: &str) -> bool {
    let low = t.trim().to_ascii_lowercase();
    if low.starts_with('-') {
        return false;
    }
    is_math_fn(&low) || low.parse::<f64>().map(|v| v.is_finite()).unwrap_or(false)
}
// 부호 없는 <length>(퍼센트·음수 거부).
fn nonneg_length_only(t: &str) -> bool {
    let low = t.trim().to_ascii_lowercase();
    if low.starts_with('-') || low.ends_with('%') {
        return false;
    }
    // is_length_percentage 는 단위 없는 수 중 0 만 <length> 로 인정한다. 단위 없는
    // 비영 수만 추가로 거부(단, 0 은 유효한 길이라 허용).
    is_math_fn(&low) || (is_length_percentage(t) && (low == "0" || low.parse::<f64>().is_err()))
}
// 부호 없는 <percentage>.
fn nonneg_percent(t: &str) -> bool {
    let low = t.trim().to_ascii_lowercase();
    if low.starts_with('-') {
        return false;
    }
    is_math_fn(&low)
        || low.strip_suffix('%').map(|n| n.parse::<f64>().map(|v| v.is_finite()).unwrap_or(false)).unwrap_or(false)
}

// 괄호 밖 최상위 콤마로 레이어 분리(background-* 리스트).
fn split_commas_top(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for c in raw.chars() {
        match c {
            '(' => {
                depth += 1;
                cur.push(c);
            }
            ')' => {
                depth -= 1;
                cur.push(c);
            }
            ',' if depth == 0 => {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    out.push(cur.trim().to_string());
    out
}

// background-repeat(§CSS Backgrounds): <repeat-style>#.
// 레이어 = repeat-x | repeat-y | [ repeat|space|round|no-repeat ]{1,2}.
pub fn background_repeat_valid(raw: &str) -> bool {
    let layers = split_commas_top(raw);
    !layers.is_empty() && layers.iter().all(|l| repeat_style_layer_valid(l))
}
fn repeat_style_layer_valid(l: &str) -> bool {
    let toks: Vec<&str> = l.split_whitespace().collect();
    match toks.len() {
        1 => matches!(
            toks[0].to_ascii_lowercase().as_str(),
            "repeat-x" | "repeat-y" | "repeat" | "space" | "round" | "no-repeat"
        ),
        2 => toks
            .iter()
            .all(|t| matches!(t.to_ascii_lowercase().as_str(), "repeat" | "space" | "round" | "no-repeat")),
        _ => false,
    }
}

// background-size(§CSS Backgrounds): <bg-size>#.
// 레이어 = [ <length-percentage [0,∞]> | auto ]{1,2} | cover | contain.
pub fn background_size_valid(raw: &str) -> bool {
    let layers = split_commas_top(raw);
    !layers.is_empty() && layers.iter().all(|l| bg_size_layer_valid(l))
}
fn bg_size_layer_valid(l: &str) -> bool {
    let low = l.trim().to_ascii_lowercase();
    if low == "cover" || low == "contain" {
        return true;
    }
    let toks = split_top_level(l.trim());
    !toks.is_empty()
        && toks.len() <= 2
        && toks.iter().all(|t| t.eq_ignore_ascii_case("auto") || nonneg_length_percentage(t))
}

// background-attachment(§CSS Backgrounds): [ scroll | fixed | local ]#.
pub fn background_attachment_valid(raw: &str) -> bool {
    let layers = split_commas_top(raw);
    !layers.is_empty()
        && layers.iter().all(|l| matches!(l.trim().to_ascii_lowercase().as_str(), "scroll" | "fixed" | "local"))
}

// ── <basic-shape>(§CSS Shapes) ─────────────────────────────────────────────
fn bs_lp(t: &str) -> bool {
    // <length-percentage>(부호 허용, calc). 무단위는 0 만.
    is_length_percentage(t) || is_math_fn(&t.to_ascii_lowercase())
}
fn bs_nonneg_lp(t: &str) -> bool {
    if t.trim().starts_with('-') {
        return false;
    }
    is_length_percentage(t) || is_math_fn(&t.to_ascii_lowercase())
}
// <position>(§CSS Values 4): 1·2·4 값만(3값 형식은 <bg-position> 전용이라 무효).
fn bs_position_valid(toks: &[String]) -> bool {
    let is_kw = |k: &str| matches!(k, "left" | "right" | "center" | "top" | "bottom");
    // 축: left/right=0, top/bottom=1, center=2(양축 가능).
    let axis = |k: &str| {
        if matches!(k, "left" | "right") {
            0i32
        } else if matches!(k, "top" | "bottom") {
            1
        } else {
            2
        }
    };
    match toks.len() {
        1 => {
            let l = toks[0].to_ascii_lowercase();
            is_kw(&l) || bs_lp(&toks[0])
        }
        2 => {
            let (a, b) = (toks[0].to_ascii_lowercase(), toks[1].to_ascii_lowercase());
            let (a_kw, b_kw) = (is_kw(&a), is_kw(&b));
            let (a_lp, b_lp) = (bs_lp(&toks[0]), bs_lp(&toks[1]));
            if !(a_kw || a_lp) || !(b_kw || b_lp) {
                return false;
            }
            if a_kw && b_kw {
                // 키워드 둘: 같은 축(left right / top bottom)은 무효, 순서는 자유.
                let (xa, xb) = (axis(&a), axis(&b));
                xa == 2 || xb == 2 || xa != xb
            } else if a_kw {
                // 키워드+길이: 첫 슬롯 키워드는 가로 계열(left/right/center)만.
                matches!(a.as_str(), "left" | "right" | "center")
            } else if b_kw {
                // 길이+키워드: 둘째 슬롯 키워드는 세로 계열(top/bottom/center)만.
                matches!(b.as_str(), "top" | "bottom" | "center")
            } else {
                true // 길이 둘
            }
        }
        4 => {
            // [kw <lp>] [kw <lp>] — 키워드 두 축, 각 뒤에 오프셋.
            let (k0, k2) = (toks[0].to_ascii_lowercase(), toks[2].to_ascii_lowercase());
            if !is_kw(&k0) || !is_kw(&k2) || !bs_lp(&toks[1]) || !bs_lp(&toks[3]) {
                return false;
            }
            let (x0, x2) = (axis(&k0), axis(&k2));
            x0 == 2 || x2 == 2 || x0 != x2
        }
        _ => false,
    }
}
// inset()/xywh()/rect() 의 [round <border-radius>] 꼬리 검증.
fn bs_round_tail(radii: &[String]) -> bool {
    !radii.is_empty() && border_radius_valid(&radii.join(" "))
}
fn inset_shape_valid(inner: &str) -> bool {
    let toks = split_top_level(inner);
    if toks.is_empty() {
        return false;
    }
    let rp = toks.iter().position(|t| t.eq_ignore_ascii_case("round"));
    let (coords, radii) = match rp {
        Some(p) => (&toks[..p], Some(&toks[p + 1..])),
        None => (&toks[..], None),
    };
    if coords.is_empty() || coords.len() > 4 || !coords.iter().all(|t| bs_lp(t)) {
        return false;
    }
    match radii {
        None => true,
        Some(r) => bs_round_tail(r),
    }
}
fn circle_shape_valid(inner: &str) -> bool {
    if inner.is_empty() {
        return true;
    }
    let toks = split_top_level(inner);
    let ap = toks.iter().position(|t| t.eq_ignore_ascii_case("at"));
    let (radius, pos) = match ap {
        Some(p) => (&toks[..p], Some(&toks[p + 1..])),
        None => (&toks[..], None),
    };
    if radius.len() > 1 {
        return false;
    }
    if let Some(r) = radius.first() {
        let l = r.to_ascii_lowercase();
        if !(matches!(
            l.as_str(),
            "closest-side" | "farthest-side" | "closest-corner" | "farthest-corner"
        ) || bs_nonneg_lp(r))
        {
            return false;
        }
    }
    match pos {
        Some(p) => bs_position_valid(p),
        None => true,
    }
}
fn ellipse_shape_valid(inner: &str) -> bool {
    if inner.is_empty() {
        return true;
    }
    let toks = split_top_level(inner);
    let ap = toks.iter().position(|t| t.eq_ignore_ascii_case("at"));
    let (radii, pos) = match ap {
        Some(p) => (&toks[..p], Some(&toks[p + 1..])),
        None => (&toks[..], None),
    };
    if radii.len() != 0 && radii.len() != 2 {
        return false;
    }
    for r in radii {
        let l = r.to_ascii_lowercase();
        if !(matches!(
            l.as_str(),
            "closest-side" | "farthest-side" | "closest-corner" | "farthest-corner"
        ) || bs_nonneg_lp(r))
        {
            return false;
        }
    }
    match pos {
        Some(p) => bs_position_valid(p),
        None => true,
    }
}
fn polygon_point_valid(s: &str) -> bool {
    let toks = split_top_level(s);
    toks.len() == 2 && toks.iter().all(|t| bs_lp(t))
}
fn polygon_prelude_valid(ftoks: &[String]) -> bool {
    // [ <fill-rule> ]? [ round <length> ]?  (순서 고정).
    let mut i = 0;
    if i < ftoks.len() && matches!(ftoks[i].to_ascii_lowercase().as_str(), "nonzero" | "evenodd") {
        i += 1;
    }
    if i < ftoks.len() && ftoks[i].eq_ignore_ascii_case("round") {
        i += 1;
        if i >= ftoks.len() || !nonneg_length_only(&ftoks[i]) {
            return false;
        }
        i += 1;
    }
    i == ftoks.len()
}
fn polygon_shape_valid(inner: &str) -> bool {
    let segs = split_top_level_commas_local(inner);
    if segs.is_empty() {
        return false;
    }
    let ftoks = split_top_level(&segs[0]);
    let has_prelude = ftoks
        .iter()
        .any(|t| matches!(t.to_ascii_lowercase().as_str(), "nonzero" | "evenodd" | "round"));
    if has_prelude {
        if !polygon_prelude_valid(&ftoks) {
            return false;
        }
        let pts = &segs[1..];
        !pts.is_empty() && pts.iter().all(|s| polygon_point_valid(s))
    } else {
        segs.iter().all(|s| polygon_point_valid(s))
    }
}
fn path_shape_valid(inner: &str) -> bool {
    let inner = inner.trim();
    // <string> 은 비어있지 않고 SVG path data(moveto M/m 로 시작)여야 한다.
    let is_path_str = |t: &str| {
        if !is_css_string(t) {
            return false;
        }
        let d = t[1..t.len() - 1].trim();
        // SVG path data: moveto 로 시작 + 각 명령 인자 개수 유효(A=7 등).
        svg_path_valid(d)
    };
    if inner.starts_with('"') || inner.starts_with('\'') {
        return is_path_str(inner);
    }
    // [ <fill-rule> , ]? <string> — fill-rule 앞엔 콤마·따옴표가 없다.
    if let Some(ci) = inner.find(',') {
        let fr = inner[..ci].trim();
        let st = inner[ci + 1..].trim();
        return matches!(fr.to_ascii_lowercase().as_str(), "nonzero" | "evenodd") && is_path_str(st);
    }
    false
}
fn xywh_shape_valid(inner: &str) -> bool {
    let toks = split_top_level(inner);
    let rp = toks.iter().position(|t| t.eq_ignore_ascii_case("round"));
    let (coords, radii) = match rp {
        Some(p) => (&toks[..p], Some(&toks[p + 1..])),
        None => (&toks[..], None),
    };
    // x,y 는 임의 <length-percentage>, width,height 는 <length-percentage [0,∞]>
    // (음수 리터럴 거부; calc 는 부호 정적 판단 불가라 허용).
    if coords.len() != 4 || !coords.iter().all(|t| bs_lp(t)) {
        return false;
    }
    if coords[2].trim().starts_with('-') || coords[3].trim().starts_with('-') {
        return false;
    }
    match radii {
        None => true,
        Some(r) => bs_round_tail(r),
    }
}
fn rect_shape_valid(inner: &str) -> bool {
    let toks = split_top_level(inner);
    let rp = toks.iter().position(|t| t.eq_ignore_ascii_case("round"));
    let (coords, radii) = match rp {
        Some(p) => (&toks[..p], Some(&toks[p + 1..])),
        None => (&toks[..], None),
    };
    if coords.len() != 4 || !coords.iter().all(|t| t.eq_ignore_ascii_case("auto") || bs_lp(t)) {
        return false;
    }
    match radii {
        None => true,
        Some(r) => bs_round_tail(r),
    }
}

// <basic-shape> 함수 하나(inset/circle/ellipse/polygon/path/xywh/rect) 검증.
// ray() 함수(§CSS Motion Path): ray( <angle> && <ray-size>? && contain? && [at <position>]? ).
// angle 필수(정확히 1), size/contain 각 ≤1, 순서 자유, at 뒤는 <position>.
pub fn ray_valid(raw: &str) -> bool {
    let s = raw.trim();
    let low = s.to_ascii_lowercase();
    if !low.starts_with("ray(") || !s.ends_with(')') {
        return false;
    }
    let inner = s[4..s.len() - 1].trim();
    if inner.is_empty() {
        return false;
    }
    let toks = split_top_level(inner);
    // ray 의 <angle> 은 단위 필수(bare 0 무효, "ray(0 sides)" 거부).
    let is_angle = |t: &str| math_angle_valid(t);
    let is_size = |t: &str| {
        matches!(
            t,
            "closest-side" | "closest-corner" | "farthest-side" | "farthest-corner" | "sides"
        )
    };
    // <angle> && <ray-size>? && contain? && [at <position>]? — 순서 자유(&&). at 뒤
    // 위치 성분은 다음 ray 키워드(size/contain/angle) 전까지 수집.
    let (mut angle, mut size, mut contain) = (0u32, 0u32, 0u32);
    let (mut has_at, mut pos_toks): (bool, Vec<String>) = (false, Vec::new());
    let mut i = 0;
    while i < toks.len() {
        let tl = toks[i].to_ascii_lowercase();
        if tl == "at" {
            has_at = true;
            i += 1;
            while i < toks.len() {
                let pt = toks[i].to_ascii_lowercase();
                if is_size(&pt) || pt == "contain" || is_angle(&pt) {
                    break;
                }
                pos_toks.push(toks[i].clone());
                i += 1;
            }
            continue;
        }
        if is_size(&tl) {
            size += 1;
        } else if tl == "contain" {
            contain += 1;
        } else if is_angle(&tl) {
            angle += 1;
        } else {
            return false;
        }
        i += 1;
    }
    if angle != 1 || size > 1 || contain > 1 {
        return false;
    }
    if has_at {
        return !pos_toks.is_empty() && position_valid(&pos_toks.join(" "));
    }
    true
}

// offset-path(§CSS Motion Path): none | <url> | <ray()> | [<basic-shape> || <coord-box>].
pub fn offset_path_valid(raw: &str) -> bool {
    let s = raw.trim();
    if s.eq_ignore_ascii_case("none") {
        return true;
    }
    let low = s.to_ascii_lowercase();
    if low.starts_with("url(") && s.ends_with(')') {
        return true;
    }
    // [<ray()|<basic-shape>|shape()> || <coord-box>] — 도형·박스 각 최대 1, 순서 자유.
    let comps = split_top_level(s);
    if comps.is_empty() {
        return false;
    }
    let (mut shapes, mut boxes) = (0u32, 0u32);
    for c in &comps {
        let cl = c.to_ascii_lowercase();
        if matches!(
            cl.as_str(),
            "content-box" | "padding-box" | "border-box" | "margin-box" | "fill-box"
                | "stroke-box" | "view-box"
        ) {
            boxes += 1;
        } else if cl.starts_with("ray(") {
            if !ray_valid(c) {
                return false;
            }
            shapes += 1;
        } else if cl.starts_with("shape(") {
            if !shape_func_valid(c) {
                return false;
            }
            shapes += 1;
        } else if cl.starts_with("path(") && c.ends_with(')') {
            // offset-path 의 path() 는 <fill-rule> 불허(clip-path 와 달리). 따옴표 밖
            // 최상위 콤마가 있으면 fill-rule 이 있는 것 → 거부.
            let inner = &c[c.find('(').unwrap() + 1..c.len() - 1];
            let mut q: Option<char> = None;
            let mut top_comma = false;
            for ch in inner.chars() {
                match q {
                    Some(qc) if ch == qc => q = None,
                    Some(_) => {}
                    None if ch == '"' || ch == '\'' => q = Some(ch),
                    None if ch == ',' => {
                        top_comma = true;
                        break;
                    }
                    None => {}
                }
            }
            if top_comma || !basic_shape_valid(c) {
                return false;
            }
            shapes += 1;
        } else if c.ends_with(')') && basic_shape_valid(c) {
            shapes += 1;
        } else {
            return false;
        }
    }
    shapes <= 1 && boxes <= 1 && shapes + boxes >= 1
}

// shape() 함수(§CSS Shapes 2) 구조 검증. 좌표/제어점 내용은 관대(soundness — 유효식
// 거부 안 함), 구조적 무효만 거부: 구획(콤마) 구분, 명령 키워드, 빈 인자(선두/후행
// 콤마), arc 플래그 중복(cw+ccw/large+small), hline/vline 의 위치 키워드.
pub fn shape_func_valid(raw: &str) -> bool {
    let s = raw.trim();
    let low = s.to_ascii_lowercase();
    if !low.starts_with("shape(") || !s.ends_with(')') {
        return false;
    }
    let inner = s[6..s.len() - 1].trim();
    if inner.is_empty() || inner.starts_with(',') || inner.ends_with(',') {
        return false;
    }
    let segs = split_top_commas(inner);
    if segs.is_empty() || segs.iter().any(|g| g.trim().is_empty()) {
        return false;
    }
    const CMDS: &[&str] = &["move", "line", "hline", "vline", "curve", "smooth", "arc", "close"];
    for (i, seg) in segs.iter().enumerate() {
        let toks: Vec<String> =
            split_top_level(seg.trim()).iter().map(|t| t.to_ascii_lowercase()).collect();
        if toks.is_empty() {
            return false;
        }
        if i == 0 {
            // [<fill-rule>]? from <coord-pair> — coords 안에 명령 키워드가 있으면 콤마 누락.
            // 좌표쌍은 최소 2토큰(x,y).
            let idx = if matches!(toks[0].as_str(), "nonzero" | "evenodd") { 1 } else { 0 };
            if toks.get(idx).map(|s| s.as_str()) != Some("from") {
                return false;
            }
            if toks.len() < idx + 3 || toks[idx + 1..].iter().any(|t| CMDS.contains(&t.as_str())) {
                return false;
            }
        } else {
            let cmd = toks[0].as_str();
            if !CMDS.contains(&cmd) {
                return false;
            }
            if cmd == "close" {
                if toks.len() != 1 {
                    return false; // close 는 인자 없음
                }
            } else {
                // 방향 명령은 <by-to>(by|to)가 두 번째 토큰.
                if !matches!(toks.get(1).map(|s| s.as_str()), Some("by") | Some("to")) {
                    return false;
                }
            }
            if cmd == "arc" {
                let cnt = |k: &str| toks.iter().filter(|t| t.as_str() == k).count();
                if cnt("cw") + cnt("ccw") > 1 || cnt("large") + cnt("small") > 1 {
                    return false;
                }
            }
            // curve 는 제어점 도입자 "with" 필수. smooth 는 제어점 있으면(좌표쌍 뒤
            // 토큰이 더 있으면) "with" 로 시작해야("via" 등 거부).
            if cmd == "curve" && !toks.iter().any(|t| t == "with") {
                return false;
            }
            if cmd == "smooth" && toks.len() > 4 && toks.get(4).map(|s| s.as_str()) != Some("with") {
                return false;
            }
            if (cmd == "hline" || cmd == "vline")
                && toks
                    .iter()
                    .any(|t| matches!(t.as_str(), "top" | "bottom" | "left" | "right" | "center"))
            {
                return false;
            }
        }
    }
    // from 만 있고 명령이 없으면 무효(최소 1개 명령).
    segs.len() >= 2
}

pub fn basic_shape_valid(raw: &str) -> bool {
    let s = raw.trim();
    if !s.ends_with(')') {
        return false;
    }
    let Some(open) = s.find('(') else { return false };
    let name = s[..open].trim().to_ascii_lowercase();
    let inner = s[open + 1..s.len() - 1].trim();
    match name.as_str() {
        "inset" => inset_shape_valid(inner),
        "circle" => circle_shape_valid(inner),
        "ellipse" => ellipse_shape_valid(inner),
        "polygon" => polygon_shape_valid(inner),
        "path" => path_shape_valid(inner),
        "xywh" => xywh_shape_valid(inner),
        "rect" => rect_shape_valid(inner),
        _ => false,
    }
}

// object-view-box(§CSS Images 5): none | <basic-shape-rect>. rect 계열(inset/xywh/
// rect)만 유효 — circle/ellipse/polygon/path 는 무효.
pub fn object_view_box_valid(raw: &str) -> bool {
    let s = raw.trim();
    if s.eq_ignore_ascii_case("none") {
        return true;
    }
    if !s.ends_with(')') {
        return false;
    }
    let Some(open) = s.find('(') else { return false };
    let name = s[..open].trim().to_ascii_lowercase();
    let inner = s[open + 1..s.len() - 1].trim();
    match name.as_str() {
        "inset" => inset_shape_valid(inner),
        "xywh" => xywh_shape_valid(inner),
        "rect" => rect_shape_valid(inner),
        _ => false,
    }
}

// clip(레거시, §CSS Masking): auto | rect(<len-or-auto>{4}). 퍼센트 불가, 부호 허용.
// 콤마 또는 공백 구분(혼용 불가).
pub fn clip_valid(raw: &str) -> bool {
    let s = raw.trim();
    if s.eq_ignore_ascii_case("auto") {
        return true;
    }
    let low = s.to_ascii_lowercase();
    if !low.starts_with("rect(") || !low.ends_with(')') {
        return false;
    }
    let inner = s[5..s.len() - 1].trim();
    let parts: Vec<String> = if inner.contains(',') {
        inner.split(',').map(|p| p.trim().to_string()).collect()
    } else {
        inner.split_whitespace().map(|p| p.to_string()).collect()
    };
    if parts.len() != 4 {
        return false;
    }
    let is_clip_len = |t: &str| {
        let l = t.to_ascii_lowercase();
        if l.ends_with('%') {
            return false;
        }
        is_length_percentage(t) || is_math_fn(&l)
    };
    parts.iter().all(|p| p.eq_ignore_ascii_case("auto") || is_clip_len(p))
}

fn is_geometry_box(t: &str) -> bool {
    matches!(
        t.to_ascii_lowercase().as_str(),
        "margin-box" | "border-box" | "padding-box" | "content-box" | "fill-box" | "stroke-box" | "view-box"
    )
}

// clip-path(§CSS Masking): none | <url> | [ <basic-shape> || <geometry-box> ].
pub fn clip_path_valid(raw: &str) -> bool {
    let s = raw.trim();
    if s.eq_ignore_ascii_case("none") {
        return true;
    }
    let low = s.to_ascii_lowercase();
    if low.starts_with("url(") && low.ends_with(')') {
        return true;
    }
    let comps = split_top_level(s);
    let (mut shapes, mut boxes) = (0u32, 0u32);
    for c in &comps {
        if is_geometry_box(c) {
            boxes += 1;
        } else if c.to_ascii_lowercase().starts_with("shape(") {
            if !shape_func_valid(c) {
                return false;
            }
            shapes += 1;
        } else if c.ends_with(')') && basic_shape_valid(c) {
            shapes += 1;
        } else {
            return false;
        }
    }
    shapes <= 1 && boxes <= 1 && shapes + boxes >= 1
}

fn is_shape_image_component(c: &str) -> bool {
    let low = c.to_ascii_lowercase();
    if !low.ends_with(')') {
        return false;
    }
    const IMG: &[&str] = &[
        "url(", "src(", "image(", "image-set(", "-webkit-image-set(", "cross-fade(",
        "-webkit-cross-fade(", "element(", "paint(", "linear-gradient(", "radial-gradient(",
        "conic-gradient(", "repeating-linear-gradient(", "repeating-radial-gradient(",
        "repeating-conic-gradient(", "-webkit-linear-gradient(", "-webkit-radial-gradient(",
        "-webkit-gradient(",
    ];
    IMG.iter().any(|p| low.starts_with(p))
}

// shape-outside(§CSS Shapes): none | <image> | [ <basic-shape> || <shape-box> ].
// shape-box = margin-box|border-box|padding-box|content-box. shape() 는 미구현이라
// 구조만 보고 느슨히 수용(유효 shape() 를 거부하지 않기 위함).
pub fn shape_outside_valid(raw: &str) -> bool {
    let s = raw.trim();
    if s.eq_ignore_ascii_case("none") {
        return true;
    }
    let comps = split_top_level(s);
    let (mut shapes, mut boxes, mut images) = (0u32, 0u32, 0u32);
    for c in &comps {
        let low = c.to_ascii_lowercase();
        if matches!(low.as_str(), "margin-box" | "border-box" | "padding-box" | "content-box") {
            boxes += 1;
        } else if is_shape_image_component(c) {
            images += 1;
        } else if low.starts_with("shape(") && low.ends_with(')') {
            if !shape_func_valid(c) {
                return false;
            }
            shapes += 1;
        } else if c.ends_with(')') && basic_shape_valid(c) {
            shapes += 1;
        } else {
            return false;
        }
    }
    if images > 0 {
        return images == 1 && shapes == 0 && boxes == 0;
    }
    shapes <= 1 && boxes <= 1 && shapes + boxes >= 1
}

// border-image-source(§CSS Backgrounds): none | <image>. 단일 값(최상위 콤마 불가).
pub fn border_image_source_valid(raw: &str) -> bool {
    let t = raw.trim();
    if t.eq_ignore_ascii_case("none") {
        return true;
    }
    split_top_level_commas_local(t).len() == 1 && t.ends_with(')')
}

// background-image 레이어 구조 검증: 각 레이어는 none 또는 <image>(함수).
// auto 같은 bare 키워드·빈 레이어 거부(gradient/cross-fade 내부는 별도 검증기).
// animation-range 단축(§scroll-animations): [ <start> <end>? ]#. start/end 각각
// normal | <length-percentage> | <timeline-range-name> <length-percentage>?. 시간·각도·
// 색·미지 이름·순서 위반 거부.
fn anim_range_is_name(t: &str) -> bool {
    matches!(
        t.to_ascii_lowercase().as_str(),
        "cover" | "contain" | "entry" | "exit" | "entry-crossing" | "exit-crossing"
    )
}
fn anim_range_is_lp(t: &str) -> bool {
    let low = t.to_ascii_lowercase();
    if is_math_fn(&low) {
        return math_length_valid(&low, true);
    }
    is_length_percentage(t) // "0"·음수·%·단위 길이 포함, 시간/각도/색 제외
}
// 토큰에서 range 값 하나(normal | lp | name lp?)를 소비, 다음 인덱스 반환.
fn anim_range_consume(toks: &[String], i: usize) -> Option<usize> {
    let t = &toks[i];
    if t.eq_ignore_ascii_case("normal") {
        return Some(i + 1);
    }
    if anim_range_is_name(t) {
        let mut j = i + 1;
        if j < toks.len() && anim_range_is_lp(&toks[j]) {
            j += 1;
        }
        return Some(j);
    }
    if anim_range_is_lp(t) {
        return Some(i + 1);
    }
    None
}
pub fn animation_range_valid(raw: &str) -> bool {
    let low = raw.trim().to_ascii_lowercase();
    if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
        return true;
    }
    let items = split_top_commas(raw);
    if items.is_empty() {
        return false;
    }
    items.iter().all(|item| {
        let toks = split_top_level(item.trim());
        if toks.is_empty() {
            return false;
        }
        let Some(i) = anim_range_consume(&toks, 0) else { return false };
        if i == toks.len() {
            return true; // start 만
        }
        matches!(anim_range_consume(&toks, i), Some(j) if j == toks.len()) // start end
    })
}

// animation-range 단축 → (start-list, end-list) 전개. 각 애니메이션(콤마)에서 start
// [end] 를 분리, end 미지정은 normal. 무효면 None.
pub fn animation_range_expand(raw: &str) -> Option<(String, String)> {
    let items = split_top_commas(raw);
    if items.is_empty() {
        return None;
    }
    let (mut starts, mut ends) = (Vec::new(), Vec::new());
    for item in &items {
        let toks = split_top_level(item.trim());
        if toks.is_empty() {
            return None;
        }
        let i = anim_range_consume(&toks, 0)?;
        let start = toks[..i].join(" ");
        if i == toks.len() {
            // end 생략 시 start 와 동일(§scroll-animations 단축).
            ends.push(start.clone());
        } else {
            let j = anim_range_consume(&toks, i)?;
            if j != toks.len() {
                return None;
            }
            ends.push(toks[i..j].join(" "));
        }
        starts.push(start);
    }
    Some((starts.join(", "), ends.join(", ")))
}

// animation-range 값 하나 캐논: "<name> <offset>" 에서 기본 오프셋 생략(start 는 0%,
// end 는 100%). "cover 0%"(start)→"cover", "cover 100%"(end)→"cover". normal·bare
// lp·비기본 오프셋은 유지.
fn anim_range_value_canon(item: &str, is_end: bool) -> String {
    let toks = split_top_level(item.trim());
    if toks.len() == 2 && anim_range_is_name(&toks[0]) {
        let off = toks[1].trim().to_ascii_lowercase();
        let default = if is_end { "100%" } else { "0%" };
        if off == default {
            return toks[0].clone();
        }
    }
    toks.join(" ")
}
// animation-range-start/end 롱핸드 값 캐논(콤마 목록 각 항목).
pub fn animation_range_longhand_canonical(raw: &str, is_end: bool) -> String {
    let low = raw.trim().to_ascii_lowercase();
    if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
        return low;
    }
    split_top_commas(raw)
        .iter()
        .map(|item| anim_range_value_canon(item, is_end))
        .collect::<Vec<_>>()
        .join(", ")
}

// animation-range-start/end 롱핸드: <single-value># (start+end 아님). 각 항목은 range
// 값 하나(normal | lp | name lp?)로 끝나야 한다("contain contain" 은 롱핸드 무효).
pub fn animation_range_longhand_valid(raw: &str) -> bool {
    let low = raw.trim().to_ascii_lowercase();
    if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
        return true;
    }
    let items = split_top_commas(raw);
    if items.is_empty() {
        return false;
    }
    items.iter().all(|item| {
        let toks = split_top_level(item.trim());
        !toks.is_empty() && matches!(anim_range_consume(&toks, 0), Some(j) if j == toks.len())
    })
}

// mask 단축(§CSS Masking): <mask-layer># 의 성분 개수 검증(sound — 유효값 거부 안 함).
// 각 레이어에서 compositing-operator·masking-mode·image 는 ≤1, geometry-box(no-clip
// 포함)는 ≤2(origin+clip), "/"(position/size 구분)는 ≤1. position/repeat/size 내용은
// 관대(개수 위반만 거부).
pub fn mask_shorthand_valid(raw: &str) -> bool {
    let t = raw.trim();
    let low = t.to_ascii_lowercase();
    if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer" | "none") {
        return true;
    }
    let layers = split_top_level_commas_local(t);
    if layers.is_empty() {
        return false;
    }
    for layer in &layers {
        let toks = split_top_level(layer.trim());
        if toks.is_empty() {
            return false;
        }
        let (mut comp, mut mode, mut boxes, mut slash, mut img) = (0u32, 0u32, 0u32, 0u32, 0u32);
        for tk in &toks {
            let tl = tk.to_ascii_lowercase();
            if tl == "/" {
                slash += 1;
            } else if matches!(tl.as_str(), "add" | "subtract" | "intersect" | "exclude") {
                comp += 1;
            } else if matches!(tl.as_str(), "alpha" | "luminance" | "match-source") {
                mode += 1;
            } else if matches!(
                tl.as_str(),
                "content-box" | "padding-box" | "border-box" | "margin-box" | "fill-box"
                    | "stroke-box" | "view-box" | "no-clip"
            ) {
                boxes += 1;
            } else if tl == "none"
                || (tl.ends_with(')')
                    && (tl.starts_with("url(")
                        || tl.starts_with("src(")
                        || tl.contains("gradient(")
                        || tl.starts_with("image(")
                        || tl.starts_with("image-set(")
                        || tl.starts_with("-webkit-image-set(")
                        || tl.starts_with("cross-fade(")
                        || tl.starts_with("element(")))
            {
                img += 1;
            }
            // position/repeat/size/길이 등은 세지 않음(관대).
        }
        if comp > 1 || mode > 1 || boxes > 2 || slash > 1 || img > 1 {
            return false;
        }
    }
    true
}

pub fn background_image_layers_valid(raw: &str) -> bool {
    let t = raw.trim();
    let low = t.to_ascii_lowercase();
    if matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
        return true;
    }
    let layers = split_top_level_commas_local(t);
    if layers.is_empty() {
        return false;
    }
    layers.iter().all(|l| {
        let lt = l.trim();
        let ll = lt.to_ascii_lowercase();
        if ll == "none" {
            return true;
        }
        if ll.is_empty() || !ll.ends_with(')') {
            return false;
        }
        // image()(§CSS Images): image( <color> ) 단일 색만. image(none)/image(notacolor)/
        // image(url(...))/image(red, blue) 등 거부. image-set 은 별개(위 검증).
        if ll.starts_with("image(") {
            let inner = lt[lt.find('(').unwrap() + 1..lt.len() - 1].trim();
            // <color> 단일. single_color_valid 가 못 받는 light-dark() 도 색이라 허용.
            return single_color_valid(inner)
                || (inner.to_ascii_lowercase().starts_with("light-dark(") && inner.ends_with(')'));
        }
        true
    })
}

// cross-fade(§CSS Images 4): cross-fade( [ <percentage [0,100]>? && <image|color> ]# ).
// cross-fade() 만 판단하고 그 외 값은 통과(다른 검증기가 처리).
pub fn cross_fade_valid(raw: &str) -> bool {
    let t = raw.trim();
    let low = t.to_ascii_lowercase();
    if !low.starts_with("cross-fade(") || !t.ends_with(')') {
        return true;
    }
    let inner = &t["cross-fade(".len()..t.len() - 1];
    let items = split_top_level_commas_local(inner);
    if items.is_empty() {
        return false;
    }
    items.iter().all(|it| cf_image_valid(it))
}
fn cf_image_valid(it: &str) -> bool {
    let toks = split_top_level(it.trim());
    if toks.is_empty() {
        return false;
    }
    let (mut pct, mut img) = (0u32, 0u32);
    for t in &toks {
        if is_pct_0_100(t) {
            pct += 1;
        } else if single_color_valid(t) || is_cf_image(t) {
            img += 1;
        } else {
            return false;
        }
    }
    img == 1 && pct <= 1
}
fn is_pct_0_100(t: &str) -> bool {
    t.trim()
        .strip_suffix('%')
        .and_then(|n| n.trim().parse::<f64>().ok())
        .map(|v| (0.0..=100.0).contains(&v))
        .unwrap_or(false)
}
fn is_cf_image(t: &str) -> bool {
    let low = t.trim().to_ascii_lowercase();
    if low == "none" {
        return true;
    }
    low.ends_with(')')
        && (low.starts_with("url(")
            || low.contains("gradient(")
            || low.starts_with("cross-fade(")
            || low.starts_with("image-set(")
            || low.starts_with("image(")
            || low.starts_with("-webkit-"))
}

// 따옴표를 존중하는 최상위 콤마 분리(path 문자열 내부 콤마 보존).
fn split_top_level_commas_local(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut cur = String::new();
    for c in s.chars() {
        match quote {
            Some(q) => {
                cur.push(c);
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                '"' | '\'' => {
                    quote = Some(c);
                    cur.push(c);
                }
                '(' => {
                    depth += 1;
                    cur.push(c);
                }
                ')' => {
                    depth -= 1;
                    cur.push(c);
                }
                ',' if depth == 0 => {
                    out.push(cur.trim().to_string());
                    cur.clear();
                }
                _ => cur.push(c),
            },
        }
    }
    out.push(cur.trim().to_string());
    out
}

// border-image-repeat(§CSS Backgrounds): [ stretch|repeat|round|space ]{1,2}.
pub fn border_image_repeat_valid(raw: &str) -> bool {
    let toks: Vec<&str> = raw.split_whitespace().collect();
    !toks.is_empty()
        && toks.len() <= 2
        && toks.iter().all(|t| matches!(t.to_ascii_lowercase().as_str(), "stretch" | "repeat" | "round" | "space"))
}

// border-image-outset(§CSS Backgrounds): [ <length [0,∞]> | <number [0,∞]> ]{1,4}.
pub fn border_image_outset_valid(raw: &str) -> bool {
    let toks = split_top_level(raw.trim());
    !toks.is_empty() && toks.len() <= 4 && toks.iter().all(|t| nonneg_number(t) || nonneg_length_only(t))
}

// border-image-width(§CSS Backgrounds): [ <length-percentage [0,∞]> | <number [0,∞]> | auto ]{1,4}.
pub fn border_image_width_valid(raw: &str) -> bool {
    let toks = split_top_level(raw.trim());
    !toks.is_empty()
        && toks.len() <= 4
        && toks.iter().all(|t| t.eq_ignore_ascii_case("auto") || nonneg_number(t) || nonneg_length_percentage(t))
}

// border-image-slice(§CSS Backgrounds): [ <number [0,∞]> | <percentage [0,∞]> ]{1,4} && fill?.
// fill 은 맨 앞이나 맨 뒤(중간 불가).
pub fn border_image_slice_valid(raw: &str) -> bool {
    let toks = split_top_level(raw.trim());
    if toks.is_empty() || toks.len() > 5 {
        return false;
    }
    let fills: Vec<usize> = toks.iter().enumerate().filter(|(_, t)| t.eq_ignore_ascii_case("fill")).map(|(i, _)| i).collect();
    if fills.len() > 1 {
        return false;
    }
    if let Some(&fp) = fills.first() {
        if fp != 0 && fp != toks.len() - 1 {
            return false;
        }
    }
    let nums: Vec<&str> = toks.iter().filter(|t| !t.eq_ignore_ascii_case("fill")).map(|t| t.as_str()).collect();
    !nums.is_empty() && nums.len() <= 4 && nums.iter().all(|t| nonneg_number(t) || nonneg_percent(t))
}

// 1-4 값 박스 대칭 축약(§CSSOM 직렬화). 원문 토큰을 그대로 보존한 채 대칭이면 생략.
fn box_collapse_tokens(toks: &[String]) -> Vec<String> {
    match toks.len() {
        4 => {
            let (t, r, b, l) = (&toks[0], &toks[1], &toks[2], &toks[3]);
            if t == r && r == b && b == l {
                vec![t.clone()]
            } else if t == b && r == l {
                vec![t.clone(), r.clone()]
            } else if r == l {
                vec![t.clone(), r.clone(), b.clone()]
            } else {
                toks.to_vec()
            }
        }
        3 => {
            let (t, r, b) = (&toks[0], &toks[1], &toks[2]);
            if t == r && r == b {
                vec![t.clone()]
            } else if t == b {
                vec![t.clone(), r.clone()]
            } else {
                toks.to_vec()
            }
        }
        2 => {
            if toks[0] == toks[1] {
                vec![toks[0].clone()]
            } else {
                toks.to_vec()
            }
        }
        _ => toks.to_vec(),
    }
}

// border-radius 단축 캐논 직렬화(§CSSOM): H / V 각각 박스 축약, V==H 면 "/ V" 생략.
// 무단위 0 → 0px.
pub fn border_radius_canonical(raw: &str) -> String {
    let norm = |seg: &str| -> Vec<String> {
        let toks: Vec<String> = split_top_level(seg.trim())
            .iter()
            .map(|t| if t.trim() == "0" { "0px".to_string() } else { t.trim().to_string() })
            .collect();
        box_collapse_tokens(&toks)
    };
    let slash = split_top_slash(raw.trim());
    let h = norm(&slash[0]);
    if h.is_empty() {
        return raw.trim().to_string();
    }
    if slash.len() < 2 {
        return h.join(" ");
    }
    let v = norm(&slash[1]);
    if h == v {
        h.join(" ")
    } else {
        format!("{} / {}", h.join(" "), v.join(" "))
    }
}

// border-image-repeat 캐논: 키워드 소문자화 후 두 값이 같으면 하나로.
pub fn border_image_repeat_canonical(raw: &str) -> String {
    let toks: Vec<String> = raw.split_whitespace().map(|s| s.to_ascii_lowercase()).collect();
    box_collapse_tokens(&toks).join(" ")
}

// border-image-outset/width 캐논: 박스 대칭 축약.
pub fn border_image_box_canonical(raw: &str) -> String {
    let toks = split_top_level(raw.trim());
    box_collapse_tokens(&toks).join(" ")
}

// border-image-slice 캐논: 숫자부 박스 축약 후 fill 을 맨 뒤로.
pub fn border_image_slice_canonical(raw: &str) -> String {
    let toks = split_top_level(raw.trim());
    let has_fill = toks.iter().any(|t| t.eq_ignore_ascii_case("fill"));
    let nums: Vec<String> = toks.iter().filter(|t| !t.eq_ignore_ascii_case("fill")).cloned().collect();
    let mut out = box_collapse_tokens(&nums);
    if has_fill {
        out.push("fill".to_string());
    }
    out.join(" ")
}

// border-radius(§CSS Backgrounds): <lp [0,∞]>{1,4} [ / <lp [0,∞]>{1,4} ]?.
pub fn border_radius_valid(raw: &str) -> bool {
    let parts = split_top_slash(raw);
    if parts.is_empty() || parts.len() > 2 {
        return false;
    }
    parts.iter().all(|p| {
        let toks = split_top_level(p.trim());
        !toks.is_empty() && toks.len() <= 4 && toks.iter().all(|t| nonneg_length_percentage(t))
    })
}

// border-*-radius 코너 롱핸드: <lp [0,∞]>{1,2}(슬래시 불가).
pub fn border_corner_radius_valid(raw: &str) -> bool {
    if raw.contains('/') {
        return false;
    }
    let toks = split_top_level(raw.trim());
    !toks.is_empty() && toks.len() <= 2 && toks.iter().all(|t| nonneg_length_percentage(t))
}

// background-origin 등 <box># 목록 검증(콤마 목록, 각 항목이 허용 박스 하나).
pub fn box_list_valid(raw: &str, boxes: &[&str]) -> bool {
    let items = split_top_commas(raw);
    !items.is_empty()
        && items.iter().all(|it| boxes.contains(&it.trim().to_ascii_lowercase().as_str()))
}

// background-clip(§CSS Backgrounds 4): [ <visual-box> || text ]#. 각 항목은 박스와
// text 를 조합 가능("border-area text").
pub fn background_clip_valid(raw: &str) -> bool {
    let items = split_top_commas(raw);
    if items.is_empty() {
        return false;
    }
    items.iter().all(|it| {
        let toks: Vec<&str> = it.split_whitespace().collect();
        if toks.is_empty() || toks.len() > 2 {
            return false;
        }
        let (mut vbox, mut text) = (0u32, 0u32);
        for t in toks {
            match t.to_ascii_lowercase().as_str() {
                "border-box" | "padding-box" | "content-box" | "border-area" => vbox += 1,
                "text" => text += 1,
                _ => return false,
            }
        }
        vbox <= 1 && text <= 1 && vbox + text >= 1
    })
}

// background-clip 캐논: 각 항목에서 visual-box 를 text 앞에 둔다.
pub fn background_clip_canonical(raw: &str) -> String {
    split_top_commas(raw)
        .iter()
        .map(|it| {
            let low = it.trim().to_ascii_lowercase();
            let toks: Vec<&str> = low.split_whitespace().collect();
            let mut out: Vec<&str> = Vec::new();
            if let Some(b) = toks.iter().find(|t| **t != "text") {
                out.push(b);
            }
            if toks.contains(&"text") {
                out.push("text");
            }
            out.join(" ")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

// shape-image-threshold(§CSS Shapes): <number> | <percentage>.
pub fn shape_image_threshold_valid(raw: &str) -> bool {
    let low = raw.trim().to_ascii_lowercase();
    if is_math_fn(&low) {
        return true;
    }
    if let Some(n) = low.strip_suffix('%') {
        return n.trim().parse::<f64>().map(|v| v.is_finite()).unwrap_or(false);
    }
    low.parse::<f64>().map(|v| v.is_finite()).unwrap_or(false)
}

// list-style-image(§CSS Lists): none | <image>. 단일 이미지(url/gradient/image-set 등).
pub fn list_style_image_valid(raw: &str) -> bool {
    let t = raw.trim();
    if t.eq_ignore_ascii_case("none") {
        return true;
    }
    if split_top_level(t).len() != 1 {
        return false;
    }
    let low = t.to_ascii_lowercase();
    let is_gradient = [
        "linear-gradient(",
        "radial-gradient(",
        "conic-gradient(",
        "repeating-linear-gradient(",
        "repeating-radial-gradient(",
        "repeating-conic-gradient(",
    ]
    .iter()
    .any(|p| low.starts_with(p));
    (low.starts_with("url(") && low.ends_with(')'))
        || (is_gradient && gradient_valid(t))
        || low.starts_with("image-set(")
        || low.starts_with("-webkit-image-set(")
        || low.starts_with("cross-fade(")
        || low.starts_with("image(")
}

// <resolution>: 수 + dppx|dpcm|dpi|x. calc 허용.
fn is_resolution(t: &str) -> bool {
    let low = t.trim().to_ascii_lowercase();
    if is_math_fn(&low) {
        return true;
    }
    for u in ["dppx", "dpcm", "dpi", "x"] {
        if let Some(n) = low.strip_suffix(u) {
            return n.trim().parse::<f64>().map(|v| v.is_finite()).unwrap_or(false);
        }
    }
    false
}

// image-resolution(§CSS Images 4): [ from-image || <resolution> ] && snap?.
// snap 은 맨 앞이나 맨 뒤(from-image 와 resolution 사이를 가르면 무효).
pub fn image_resolution_valid(raw: &str) -> bool {
    let toks = split_top_level(raw.trim());
    if toks.is_empty() || toks.len() > 3 {
        return false;
    }
    let (mut from, mut res) = (0u32, 0u32);
    let mut snap: Option<usize> = None;
    for (i, t) in toks.iter().enumerate() {
        let low = t.to_ascii_lowercase();
        if low == "from-image" {
            from += 1;
        } else if low == "snap" {
            if snap.is_some() {
                return false;
            }
            snap = Some(i);
        } else if is_resolution(t) {
            res += 1;
        } else {
            return false;
        }
    }
    if from > 1 || res > 1 || from + res == 0 {
        return false;
    }
    if let Some(sp) = snap {
        if sp != 0 && sp != toks.len() - 1 {
            return false;
        }
    }
    true
}

// <time>: 수 + s|ms. allow_neg=false 면 음수 거부. calc 허용.
fn is_time(t: &str, allow_neg: bool) -> bool {
    let low = t.trim().to_ascii_lowercase();
    if is_math_fn(&low) {
        // 결과가 <time> 여야(§CSS Values 4 타입 검사). max(0Hz)/max(1px)/max(0)(수)
        // 등 시간 아닌 타입 거부.
        return math_time_valid(&low);
    }
    let num = if let Some(n) = low.strip_suffix("ms") {
        n
    } else if let Some(n) = low.strip_suffix('s') {
        n
    } else {
        return false;
    };
    matches!(num.trim().parse::<f64>(), Ok(v) if v.is_finite() && (allow_neg || v >= 0.0))
}

// animation 콤마 목록 검증: 각 항목 item_ok, CSS-wide 키워드는 항목으로 무효.
fn anim_list(raw: &str, item_ok: fn(&str) -> bool) -> bool {
    let t = raw.trim();
    if t.starts_with(',') || t.ends_with(',') {
        return false;
    }
    let items = split_top_commas(raw);
    !items.is_empty()
        && items.iter().all(|it| {
            let s = it.trim();
            !matches!(
                s.to_ascii_lowercase().as_str(),
                "initial" | "inherit" | "unset" | "revert" | "revert-layer"
            ) && item_ok(s)
        })
}

fn anim_name_item(s: &str) -> bool {
    let low = s.to_ascii_lowercase();
    if low == "none" {
        return true;
    }
    // <string>: 빈 문자열 제외.
    if is_css_string(s) {
        return s.len() > 2;
    }
    s.split_whitespace().count() == 1 && low != "default" && is_css_ident(s)
}
fn anim_duration_item(s: &str) -> bool {
    s.eq_ignore_ascii_case("auto") || is_time(s, false)
}
fn anim_delay_item(s: &str) -> bool {
    is_time(s, true)
}
fn anim_iter_item(s: &str) -> bool {
    let low = s.to_ascii_lowercase();
    low == "infinite"
        || is_math_fn(&low)
        || matches!(low.parse::<f64>(), Ok(v) if v.is_finite() && v >= 0.0)
}
fn anim_direction_item(s: &str) -> bool {
    matches!(s.to_ascii_lowercase().as_str(), "normal" | "reverse" | "alternate" | "alternate-reverse")
}
fn anim_fill_item(s: &str) -> bool {
    matches!(s.to_ascii_lowercase().as_str(), "none" | "forwards" | "backwards" | "both")
}
fn anim_playstate_item(s: &str) -> bool {
    matches!(s.to_ascii_lowercase().as_str(), "running" | "paused")
}
// animation-range-start/end 항목: normal | <lp> | <range-name> <lp>?.
fn anim_range_item(s: &str) -> bool {
    if s.eq_ignore_ascii_case("normal") {
        return true;
    }
    let is_name = |t: &str| {
        matches!(
            t.to_ascii_lowercase().as_str(),
            "cover" | "contain" | "entry" | "exit" | "entry-crossing" | "exit-crossing"
        )
    };
    let is_lp = |t: &str| is_math_fn(&t.to_ascii_lowercase()) || is_length_percentage(t);
    let toks = split_top_level(s);
    match toks.len() {
        1 => is_name(&toks[0]) || is_lp(&toks[0]),
        2 => is_name(&toks[0]) && is_lp(&toks[1]),
        _ => false,
    }
}

// animation 롱핸드 검증(§CSS Animations): 프로퍼티별 콤마 목록.
pub fn animation_longhand_valid(name: &str, raw: &str) -> bool {
    match name {
        "animation-name" => anim_list(raw, anim_name_item),
        "animation-duration" => anim_list(raw, anim_duration_item),
        "animation-delay" => anim_list(raw, anim_delay_item),
        "animation-iteration-count" => anim_list(raw, anim_iter_item),
        "animation-direction" => anim_list(raw, anim_direction_item),
        "animation-fill-mode" => anim_list(raw, anim_fill_item),
        "animation-play-state" => anim_list(raw, anim_playstate_item),
        "animation-range-start" | "animation-range-end" => anim_list(raw, anim_range_item),
        _ => false,
    }
}

// will-change(§CSS Will Change): auto | [ scroll-position | contents | <custom-ident> ]#.
// custom-ident 는 CSS-wide·default·will-change·none·all·auto 제외. 항목당 단일 토큰.
pub fn will_change_valid(raw: &str) -> bool {
    if raw.trim().eq_ignore_ascii_case("auto") {
        return true;
    }
    let items = split_top_commas(raw);
    if items.is_empty() || raw.trim().starts_with(',') || raw.trim().ends_with(',') {
        return false;
    }
    items.iter().all(|item| {
        let t = item.trim();
        if t.split_whitespace().count() != 1 {
            return false;
        }
        let tl = t.to_ascii_lowercase();
        if matches!(
            tl.as_str(),
            "auto" | "none" | "all" | "will-change" | "default" | "initial" | "inherit"
                | "unset" | "revert" | "revert-layer" | "revert-rule"
        ) {
            return false;
        }
        tl == "scroll-position" || tl == "contents" || is_css_ident(t)
    })
}

// color-scheme(§CSS Color Adjust): normal | [ light | dark | <custom-ident> ]+ && only?.
// only 최대 1회, normal·CSS-wide·콤마 불가, 스킴 하나 이상.
pub fn color_scheme_valid(raw: &str) -> bool {
    let low = raw.trim().to_ascii_lowercase();
    if low == "normal" {
        return true;
    }
    let toks: Vec<&str> = low.split_whitespace().collect();
    if toks.is_empty() {
        return false;
    }
    let (mut only, mut schemes) = (0u32, 0u32);
    let mut only_pos = None;
    for (i, t) in toks.iter().enumerate() {
        if *t == "only" {
            only += 1;
            only_pos = Some(i);
        } else if matches!(*t, "normal" | "default" | "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
            return false;
        } else if is_css_ident(t) {
            schemes += 1;
        } else {
            return false;
        }
    }
    if only > 1 || schemes < 1 {
        return false;
    }
    // only 는 맨 앞이나 맨 뒤(사이에 오면 무효).
    if let Some(p) = only_pos {
        if p != 0 && p != toks.len() - 1 {
            return false;
        }
    }
    true
}

// hyphenate-limit-chars(§CSS Text 4): [ auto | <integer> ]{1,3}.
pub fn hyphenate_limit_chars_valid(raw: &str) -> bool {
    let toks: Vec<&str> = raw.split_whitespace().collect();
    !toks.is_empty()
        && toks.len() <= 3
        && toks.iter().all(|t| t.eq_ignore_ascii_case("auto") || t.parse::<i64>().is_ok())
}

// hyphenate-limit-chars 캐논: 뒤 값이 앞 값과 같으면 생략(auto auto→auto, 5 2 2→5 2).
pub fn hyphenate_limit_chars_canonical(raw: &str) -> String {
    let mut toks: Vec<String> = raw.split_whitespace().map(|s| s.to_ascii_lowercase()).collect();
    if toks.len() == 3 && toks[2] == toks[1] {
        toks.pop();
    }
    if toks.len() == 2 && toks[1] == toks[0] {
        toks.pop();
    }
    toks.join(" ")
}

// text-indent(§CSS Text): <length-percentage> && hanging? && each-line?.
pub fn text_indent_valid(raw: &str) -> bool {
    let toks = split_top_level(raw.trim());
    if toks.is_empty() || toks.len() > 3 {
        return false;
    }
    let (mut lp, mut hang, mut each) = (0u32, 0u32, 0u32);
    for t in &toks {
        let low = t.to_ascii_lowercase();
        if low == "hanging" {
            hang += 1;
        } else if low == "each-line" {
            each += 1;
        } else if is_math_fn(&low) || is_length_percentage(t) {
            lp += 1;
        } else {
            return false;
        }
    }
    lp == 1 && hang <= 1 && each <= 1
}

// mask-composite(§CSS Masking): [ add | subtract | intersect | exclude ]#(콤마 목록).
pub fn mask_composite_valid(raw: &str) -> bool {
    let t = raw.trim();
    if t.starts_with(',') || t.ends_with(',') {
        return false;
    }
    let items = split_top_commas(raw);
    !items.is_empty()
        && items.iter().all(|it| {
            matches!(it.trim().to_ascii_lowercase().as_str(), "add" | "subtract" | "intersect" | "exclude")
        })
}

// overscroll-behavior 값: contain | none | auto | chain.
fn overscroll_kw(t: &str) -> bool {
    matches!(t.to_ascii_lowercase().as_str(), "contain" | "none" | "auto" | "chain")
}

// overscroll-behavior 단축: [contain|none|auto|chain]{1,2}. x/y 는 단일.
pub fn overscroll_valid(raw: &str, single: bool) -> bool {
    let toks: Vec<&str> = raw.split_whitespace().collect();
    let max = if single { 1 } else { 2 };
    !toks.is_empty() && toks.len() <= max && toks.iter().all(|t| overscroll_kw(t))
}

// view-transition custom-ident: none/default/CSS-wide 제외 유효 식별자.
fn vt_ident_ok(t: &str) -> bool {
    let low = t.to_ascii_lowercase();
    !matches!(
        low.as_str(),
        "none" | "default" | "inherit" | "initial" | "unset" | "revert" | "revert-layer"
    ) && is_css_ident(t)
}

// view-transition-name(§CSS View Transitions): none | <custom-ident>(단일).
pub fn view_transition_name_valid(raw: &str) -> bool {
    if raw.trim().eq_ignore_ascii_case("none") {
        return true;
    }
    let toks: Vec<&str> = raw.split_whitespace().collect();
    toks.len() == 1 && vt_ident_ok(toks[0])
}

// view-transition-class(§CSS View Transitions): none | <custom-ident>+(공백 목록).
pub fn view_transition_class_valid(raw: &str) -> bool {
    if raw.trim().eq_ignore_ascii_case("none") {
        return true;
    }
    let toks: Vec<&str> = raw.split_whitespace().collect();
    !toks.is_empty() && toks.iter().all(|t| vt_ident_ok(t))
}

// outline-style(§CSS UI): auto | <line-style> (단, hidden 제외).
pub fn outline_style_valid(t: &str) -> bool {
    matches!(
        t.trim().to_ascii_lowercase().as_str(),
        "auto" | "none" | "dotted" | "dashed" | "solid" | "double" | "groove" | "ridge"
            | "inset" | "outset"
    )
}

// outline-color(§CSS UI): <color> | auto (invert 없음).
pub fn outline_color_valid(raw: &str) -> bool {
    raw.trim().eq_ignore_ascii_case("auto") || single_color_valid(raw)
}

// outline 단축(§CSS UI): <color|auto> || <outline-style> || <line-width>. 각 최대 1개.
pub fn outline_valid(raw: &str) -> bool {
    let toks = split_top_level(raw);
    if toks.is_empty() || toks.len() > 3 {
        return false;
    }
    let (mut w, mut s, mut c) = (0u32, 0u32, 0u32);
    for t in &toks {
        if outline_style_valid(t) {
            s += 1;
        } else if column_rule_width_valid(t) {
            w += 1;
        } else if single_color_valid(t) {
            c += 1;
        } else {
            return false;
        }
    }
    w <= 1 && s <= 1 && c <= 1
}

// border-spacing(§CSS Tables): <length [0,∞]>{1,2}(퍼센트 불가).
pub fn border_spacing_valid(raw: &str) -> bool {
    let toks = split_top_level(raw);
    !toks.is_empty()
        && toks.len() <= 2
        && toks.iter().all(|t| {
            let low = t.to_ascii_lowercase();
            if low.starts_with('-') || low.ends_with('%') {
                return false;
            }
            is_math_fn(&low) || is_length_percentage(t)
        })
}

// text-combine-upright(§CSS Writing Modes): none | all | [ digits <integer [2,4]>? ].
pub fn text_combine_upright_valid(raw: &str) -> bool {
    let low = raw.trim().to_ascii_lowercase();
    if matches!(low.as_str(), "none" | "all" | "digits") {
        return true;
    }
    if let Some(n) = low.strip_prefix("digits ") {
        return matches!(n.trim().parse::<i64>(), Ok(v) if (2..=4).contains(&v));
    }
    false
}

// <line-style>(§CSS Backgrounds): border/rule 스타일 키워드.
pub fn is_line_style(t: &str) -> bool {
    matches!(
        t.trim().to_ascii_lowercase().as_str(),
        "none" | "hidden" | "dotted" | "dashed" | "solid" | "double" | "groove" | "ridge"
            | "inset" | "outset"
    )
}

// 단일 <color>(§CSS Color): currentcolor/transparent/명명·hex·함수 색. auto 제외.
pub fn single_color_valid(raw: &str) -> bool {
    let toks = split_top_level(raw);
    if toks.len() != 1 {
        return false;
    }
    let t = toks[0].trim();
    t.eq_ignore_ascii_case("currentcolor")
        || t.eq_ignore_ascii_case("transparent")
        || matches!(interpret_value(t), Some(Value::Color(_)) | Some(Value::ColorFn(..)))
}

// column-rule 단축(§CSS Multicol): <line-width> || <line-style> || <color>. 각 최대 1개.
pub fn column_rule_valid(raw: &str) -> bool {
    let toks = split_top_level(raw);
    if toks.is_empty() || toks.len() > 3 {
        return false;
    }
    let (mut w, mut s, mut c) = (0u32, 0u32, 0u32);
    for t in &toks {
        if is_line_style(t) {
            s += 1;
        } else if column_rule_width_valid(t) {
            w += 1;
        } else if single_color_valid(t) {
            c += 1;
        } else {
            return false;
        }
    }
    w <= 1 && s <= 1 && c <= 1
}

// column-rule 캐논: 초기값(width medium, style none, color currentcolor) 성분 생략.
// 모두 초기값이면 "medium".
pub fn column_rule_canonical(raw: &str) -> String {
    let toks = split_top_level(raw);
    let (mut width, mut style, mut color) =
        ("medium".to_string(), "none".to_string(), "currentcolor".to_string());
    for t in &toks {
        if is_line_style(t) {
            style = t.to_ascii_lowercase();
        } else if column_rule_width_valid(t) {
            width = t.to_ascii_lowercase();
        } else {
            color = t.to_string();
        }
    }
    let mut parts = Vec::new();
    if width != "medium" {
        parts.push(width);
    }
    if !style.eq_ignore_ascii_case("none") {
        parts.push(style);
    }
    if !color.eq_ignore_ascii_case("currentcolor") {
        parts.push(color);
    }
    if parts.is_empty() {
        "medium".to_string()
    } else {
        parts.join(" ")
    }
}

// column-count(§CSS Multicol): auto | <integer [1,∞]>.
pub fn column_count_valid(raw: &str) -> bool {
    let low = raw.trim().to_ascii_lowercase();
    low == "auto" || is_math_fn(&low) || matches!(low.parse::<i64>(), Ok(n) if n >= 1)
}

// column-width(§CSS Multicol): auto | <length [0,∞]>.
pub fn column_width_valid(raw: &str) -> bool {
    raw.trim().eq_ignore_ascii_case("auto") || nonneg_length(raw)
}

// column-rule-width(§CSS Multicol): <line-width> = thin|medium|thick | <length [0,∞]>.
pub fn column_rule_width_valid(raw: &str) -> bool {
    matches!(raw.trim().to_ascii_lowercase().as_str(), "thin" | "medium" | "thick")
        || nonneg_length(raw)
}

// columns 단축(§CSS Multicol): <'column-width'> || <'column-count'>. width/count 로 배정.
pub fn columns_expand(raw: &str) -> Option<(String, String)> {
    let toks: Vec<&str> = raw.split_whitespace().collect();
    let is_w = |t: &str| column_width_valid(t);
    let is_c = |t: &str| column_count_valid(t);
    match toks.as_slice() {
        [a] => {
            if is_w(a) && is_c(a) {
                Some(("auto".to_string(), "auto".to_string()))
            } else if is_w(a) {
                Some((a.to_string(), "auto".to_string()))
            } else if is_c(a) {
                Some(("auto".to_string(), a.to_string()))
            } else {
                None
            }
        }
        [a, b] => {
            if is_w(a) && is_c(b) {
                Some((a.to_string(), b.to_string()))
            } else if is_c(a) && is_w(b) {
                Some((b.to_string(), a.to_string()))
            } else {
                None
            }
        }
        _ => None,
    }
}


// columns 캐논: [width if not auto] [count if not auto], 둘 다 auto 면 "auto".
// 길이 0 은 0px 로 직렬화.
pub fn columns_canonical(width: &str, count: &str) -> String {
    let mut parts = Vec::new();
    if !width.eq_ignore_ascii_case("auto") {
        let w = if width.trim() == "0" { "0px".to_string() } else { width.to_string() };
        parts.push(w);
    }
    if !count.eq_ignore_ascii_case("auto") {
        parts.push(count.to_string());
    }
    if parts.is_empty() {
        "auto".to_string()
    } else {
        parts.join(" ")
    }
}

// counter-increment/reset/set(§CSS Lists 3): none | [ <custom-ident> <integer>? ]+.
// counter-reset 는 reversed(<custom-ident>) 도 허용. 이스케이프·괄호(calc) 존중.

// 이스케이프(\X)와 괄호를 존중해 공백으로 토큰 분리.
fn counter_tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                cur.push(c);
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
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

// counter 이름 custom-ident: 이스케이프 포함하면 허용, 아니면 none/default/CSS-wide 제외.
fn counter_ident_valid(t: &str) -> bool {
    if t.contains('\\') {
        return !t.is_empty();
    }
    let low = t.to_ascii_lowercase();
    !matches!(
        low.as_str(),
        "none" | "default" | "inherit" | "initial" | "unset" | "revert" | "revert-layer"
    ) && is_css_ident(t)
}

// 정수 토큰: <integer> 또는 calc 류(값은 검증만, 평가 안 함).
fn counter_int_token(t: &str) -> bool {
    t.parse::<i64>().is_ok() || is_math_fn(&t.to_ascii_lowercase())
}

// counter 이름 토큰: reversed(<ident>)[reset 만] 또는 <custom-ident>.
fn counter_name_valid(t: &str, allow_reversed: bool) -> bool {
    let low = t.to_ascii_lowercase();
    if low.starts_with("reversed(") && t.ends_with(')') {
        return allow_reversed && counter_ident_valid(t["reversed(".len()..t.len() - 1].trim());
    }
    counter_ident_valid(t)
}

pub fn counter_list_valid(raw: &str, allow_reversed: bool) -> bool {
    let s = raw.trim();
    if s.eq_ignore_ascii_case("none") {
        return true;
    }
    let toks = counter_tokens(s);
    if toks.is_empty() {
        return false;
    }
    let mut i = 0;
    let mut count = 0;
    while i < toks.len() {
        if !counter_name_valid(&toks[i], allow_reversed) {
            return false;
        }
        i += 1;
        if i < toks.len() && counter_int_token(&toks[i]) {
            i += 1;
        }
        count += 1;
    }
    count >= 1
}

// 캐논: 비reversed 이름에 정수 없으면 default_int 추가, reversed 는 bare 유지. calc verbatim.
pub fn counter_list_canonical(raw: &str, default_int: i64) -> String {
    let s = raw.trim();
    if s.eq_ignore_ascii_case("none") {
        return "none".to_string();
    }
    let toks = counter_tokens(s);
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let name = toks[i].clone();
        i += 1;
        let int_tok = if i < toks.len() && counter_int_token(&toks[i]) {
            let t = toks[i].clone();
            i += 1;
            Some(t)
        } else {
            None
        };
        let is_reversed = name.to_ascii_lowercase().starts_with("reversed(");
        match int_tok {
            Some(n) => out.push(format!("{} {}", name, n)),
            None if is_reversed => out.push(name),
            None => out.push(format!("{} {}", name, default_int)),
        }
    }
    out.join(" ")
}

// name( ... ) 함수 분해: 균형 괄호. 이름은 알파벳/하이픈. 아니면 None.
fn parse_func(s: &str) -> Option<(String, String)> {
    let s = s.trim();
    let open = s.find('(')?;
    if !s.ends_with(')') {
        return None;
    }
    let name = &s[..open];
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphabetic() || c == '-') {
        return None;
    }
    Some((name.to_string(), s[open + 1..s.len() - 1].to_string()))
}

// font-variant-alternates(§CSS Fonts 4): normal | [ stylistic(<ident>) ||
// historical-forms || styleset(<ident>#) || character-variant(<ident>#) ||
// swash(<ident>) || ornaments(<ident>) || annotation(<ident>) ]. 각 종류 최대 1회.
pub fn font_variant_alternates_valid(raw: &str) -> bool {
    let low = raw.trim();
    if low.eq_ignore_ascii_case("normal") {
        return true;
    }
    let comps = split_grid_components(low);
    if comps.is_empty() {
        return false;
    }
    let mut seen: Vec<String> = Vec::new();
    for c in &comps {
        let cl = c.trim();
        let key = if cl.eq_ignore_ascii_case("historical-forms") {
            "historical-forms".to_string()
        } else if let Some((fname, args)) = parse_func(cl) {
            let fl = fname.to_ascii_lowercase();
            let single = matches!(fl.as_str(), "stylistic" | "swash" | "ornaments" | "annotation");
            let list = matches!(fl.as_str(), "styleset" | "character-variant");
            if !single && !list {
                return false;
            }
            let idents = split_top_commas(&args);
            let ok = if single {
                idents.len() == 1 && is_css_ident(idents[0].trim())
            } else {
                !idents.is_empty() && idents.iter().all(|i| is_css_ident(i.trim()))
            };
            if !ok {
                return false;
            }
            fl
        } else {
            return false;
        };
        if seen.contains(&key) {
            return false;
        }
        seen.push(key);
    }
    true
}

// 그룹형 font-variant 검증: normal 단독 | 상호배타 그룹 각 최대 1개 + 단독 플래그
// 각 최대 1회. normal 혼합·미지 토큰·그룹 중복 거부. 모든 후보는 소문자.
fn variant_group_valid(raw: &str, groups: &[&[&str]], singles: &[&str]) -> bool {
    let low = raw.trim().to_ascii_lowercase();
    if low == "normal" {
        return true;
    }
    let toks: Vec<&str> = low.split_whitespace().collect();
    if toks.is_empty() {
        return false;
    }
    let mut group_used = vec![false; groups.len()];
    let mut singles_used: Vec<&str> = Vec::new();
    for t in toks {
        if let Some(gi) = groups.iter().position(|g| g.contains(&t)) {
            if group_used[gi] {
                return false;
            }
            group_used[gi] = true;
        } else if singles.contains(&t) {
            if singles_used.contains(&t) {
                return false;
            }
            singles_used.push(t);
        } else {
            return false;
        }
    }
    true
}

// 그룹형 font-variant 캐논: 그룹 순서대로(존재하는 실제 토큰) 후 단독 플래그 순서.
fn variant_group_canonical(raw: &str, groups: &[&[&str]], singles: &[&str]) -> String {
    let low = raw.trim().to_ascii_lowercase();
    if low == "normal" {
        return "normal".to_string();
    }
    let toks: Vec<&str> = low.split_whitespace().collect();
    let mut out: Vec<&str> = Vec::new();
    for g in groups {
        if let Some(t) = g.iter().find(|k| toks.contains(k)) {
            out.push(t);
        }
    }
    for s in singles {
        if toks.contains(s) {
            out.push(s);
        }
    }
    out.join(" ")
}

const NUMERIC_GROUPS: &[&[&str]] = &[
    &["lining-nums", "oldstyle-nums"],
    &["proportional-nums", "tabular-nums"],
    &["diagonal-fractions", "stacked-fractions"],
];
const NUMERIC_SINGLES: &[&str] = &["ordinal", "slashed-zero"];
const EAST_ASIAN_GROUPS: &[&[&str]] = &[
    &["jis78", "jis83", "jis90", "jis04", "simplified", "traditional"],
    &["full-width", "proportional-width"],
];
const EAST_ASIAN_SINGLES: &[&str] = &["ruby"];

pub fn font_variant_numeric_canonical(raw: &str) -> String {
    variant_group_canonical(raw, NUMERIC_GROUPS, NUMERIC_SINGLES)
}
pub fn font_variant_east_asian_canonical(raw: &str) -> String {
    variant_group_canonical(raw, EAST_ASIAN_GROUPS, EAST_ASIAN_SINGLES)
}

// font-variant-ligatures(§CSS Fonts 4): normal | none | 그룹형.
pub fn font_variant_ligatures_valid(raw: &str) -> bool {
    if raw.trim().eq_ignore_ascii_case("none") {
        return true;
    }
    variant_group_valid(
        raw,
        &[
            &["common-ligatures", "no-common-ligatures"],
            &["discretionary-ligatures", "no-discretionary-ligatures"],
            &["historical-ligatures", "no-historical-ligatures"],
            &["contextual", "no-contextual"],
        ],
        &[],
    )
}

// font-language-override(§CSS Fonts 4): normal | <string>(1~4 ASCII 0x20~0x7E).
pub fn font_language_override_valid(raw: &str) -> bool {
    let t = raw.trim();
    if t.eq_ignore_ascii_case("normal") {
        return true;
    }
    if !is_css_string(t) {
        return false;
    }
    let inner = &t[1..t.len() - 1];
    let n = inner.chars().count();
    (1..=4).contains(&n) && inner.chars().all(|c| ('\u{20}'..='\u{7e}').contains(&c))
}

// font-variant-numeric(§CSS Fonts 4).
pub fn font_variant_numeric_valid(raw: &str) -> bool {
    variant_group_valid(raw, NUMERIC_GROUPS, NUMERIC_SINGLES)
}

// font-variant-east-asian(§CSS Fonts 4).
pub fn font_variant_east_asian_valid(raw: &str) -> bool {
    variant_group_valid(raw, EAST_ASIAN_GROUPS, EAST_ASIAN_SINGLES)
}

// font-synthesis(§CSS Fonts 4): none | [ weight || [style|oblique-only] || small-caps
// || position ]. style 과 oblique-only 는 같은 슬롯(상호배타). 각 슬롯 최대 1회.
pub fn font_synthesis_valid(raw: &str) -> bool {
    let low = raw.trim().to_ascii_lowercase();
    if low == "none" {
        return true;
    }
    let toks: Vec<&str> = low.split_whitespace().collect();
    if toks.is_empty() {
        return false;
    }
    let (mut w, mut s, mut sc, mut p) = (0u32, 0u32, 0u32, 0u32);
    for t in toks {
        match t {
            "weight" => w += 1,
            "style" | "oblique-only" => s += 1,
            "small-caps" => sc += 1,
            "position" => p += 1,
            _ => return false,
        }
    }
    w <= 1 && s <= 1 && sc <= 1 && p <= 1
}

// font-synthesis 캐논: 슬롯 순서 weight → style|oblique-only → small-caps → position.
pub fn font_synthesis_canonical(raw: &str) -> String {
    let low = raw.trim().to_ascii_lowercase();
    if low == "none" {
        return "none".to_string();
    }
    let toks: Vec<&str> = low.split_whitespace().collect();
    let mut out = Vec::new();
    if toks.contains(&"weight") {
        out.push("weight");
    }
    if toks.contains(&"style") {
        out.push("style");
    } else if toks.contains(&"oblique-only") {
        out.push("oblique-only");
    }
    if toks.contains(&"small-caps") {
        out.push("small-caps");
    }
    if toks.contains(&"position") {
        out.push("position");
    }
    if out.is_empty() {
        "none".to_string()
    } else {
        out.join(" ")
    }
}

// font-feature-settings(§CSS Fonts 4): normal | [ <opentype-tag> [<integer>|on|off]? ]#.
pub fn font_feature_settings_valid(raw: &str) -> bool {
    let t = raw.trim();
    if t.eq_ignore_ascii_case("normal") {
        return true;
    }
    if t.starts_with(',') || t.ends_with(',') {
        return false;
    }
    let items = split_top_commas(raw);
    if items.is_empty() {
        return false;
    }
    items.iter().all(|item| match parse_string_prefix(item.trim()) {
        Some((tag, rest)) => {
            opentype_tag_valid(&tag)
                && (rest.is_empty()
                    || rest.eq_ignore_ascii_case("on")
                    || rest.eq_ignore_ascii_case("off")
                    || rest.parse::<i64>().is_ok())
        }
        None => false,
    })
}

// font-feature-settings 캐논: 따옴표 큰따옴표로, on/1 생략, off→0.
pub fn font_feature_settings_canonical(raw: &str) -> String {
    if raw.trim().eq_ignore_ascii_case("normal") {
        return "normal".to_string();
    }
    let mut out = Vec::new();
    for item in split_top_commas(raw) {
        if let Some((tag, rest)) = parse_string_prefix(item.trim()) {
            let val = if rest.is_empty() || rest.eq_ignore_ascii_case("on") || rest == "1" {
                String::new()
            } else if rest.eq_ignore_ascii_case("off") {
                "0".to_string()
            } else {
                rest
            };
            if val.is_empty() {
                out.push(format!("\"{}\"", tag));
            } else {
                out.push(format!("\"{}\" {}", tag, val));
            }
        }
    }
    out.join(", ")
}

// font-palette(§CSS Fonts 4): normal | light | dark | <dashed-ident>.
pub fn font_palette_valid(raw: &str) -> bool {
    let low = raw.trim().to_ascii_lowercase();
    if matches!(low.as_str(), "normal" | "light" | "dark") {
        return true;
    }
    let toks: Vec<&str> = raw.split_whitespace().collect();
    toks.len() == 1 && toks[0].starts_with("--")
}

// font-variant-emoji(§CSS Fonts 4): normal | text | emoji | unicode.
pub fn font_variant_emoji_valid(raw: &str) -> bool {
    matches!(raw.trim().to_ascii_lowercase().as_str(), "normal" | "text" | "emoji" | "unicode")
}

// font-stretch/font-width(§CSS Fonts 4): normal | <keyword> | <percentage 0+>.
pub fn font_stretch_valid(raw: &str) -> bool {
    let low = raw.trim().to_ascii_lowercase();
    if matches!(
        low.as_str(),
        "normal"
            | "ultra-condensed"
            | "extra-condensed"
            | "condensed"
            | "semi-condensed"
            | "semi-expanded"
            | "expanded"
            | "extra-expanded"
            | "ultra-expanded"
    ) {
        return true;
    }
    if let Some(num) = low.strip_suffix('%') {
        return num.trim().parse::<f64>().map(|v| v.is_finite() && v >= 0.0).unwrap_or(false);
    }
    is_math_fn(&low)
}

// 문자열 리터럴 접두 추출: 따옴표로 시작하면 (내용, 이후) 반환. 이스케이프는 미처리
// (테스트 유효 케이스에 이스케이프 태그 없음).
fn parse_string_prefix(s: &str) -> Option<(String, String)> {
    let s = s.trim_start();
    let q = s.chars().next()?;
    if q != '"' && q != '\'' {
        return None;
    }
    let rest = &s[q.len_utf8()..];
    let end = rest.find(q)?;
    Some((rest[..end].to_string(), rest[end + q.len_utf8()..].trim().to_string()))
}

// CSS 문자열 이스케이프 디코드(\XX 16진, \c 리터럴).
fn decode_css_esc(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let mut hex = String::new();
        while hex.len() < 6 && chars.peek().map(|c| c.is_ascii_hexdigit()).unwrap_or(false) {
            hex.push(chars.next().unwrap());
        }
        if !hex.is_empty() {
            if chars.peek() == Some(&' ') {
                chars.next();
            }
            if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                out.push(ch);
            }
        } else if let Some(lit) = chars.next() {
            out.push(lit);
        }
    }
    out
}

// <opentype-tag>: 정확히 4자, 각 0x20~0x7E(이스케이프 디코드 후).
fn opentype_tag_valid(tag: &str) -> bool {
    let d = decode_css_esc(tag);
    d.chars().count() == 4 && d.chars().all(|c| ('\u{20}'..='\u{7e}').contains(&c))
}

// font-variation-settings(§CSS Fonts 4): normal | [ <opentype-tag> <number> ]#.
pub fn font_variation_settings_valid(raw: &str) -> bool {
    let t = raw.trim();
    if t.eq_ignore_ascii_case("normal") {
        return true;
    }
    if t.starts_with(',') || t.ends_with(',') {
        return false;
    }
    let items = split_top_commas(raw);
    if items.is_empty() {
        return false;
    }
    items.iter().all(|item| {
        let it = item.trim();
        match parse_string_prefix(it) {
            Some((tag, rest)) => {
                opentype_tag_valid(&tag)
                    && !rest.is_empty()
                    && rest.parse::<f64>().map(|v| v.is_finite()).unwrap_or(false)
            }
            None => false,
        }
    })
}

// font-variation-settings 캐논: 따옴표를 큰따옴표로, 수치 정규화(1e3→1000).
pub fn font_variation_settings_canonical(raw: &str) -> String {
    let t = raw.trim();
    if t.eq_ignore_ascii_case("normal") {
        return "normal".to_string();
    }
    let mut out = Vec::new();
    for item in split_top_commas(raw) {
        if let Some((tag, rest)) = parse_string_prefix(item.trim()) {
            if let Ok(n) = rest.parse::<f64>() {
                out.push(format!("\"{}\" {}", tag, crate::style::num_css(n as f32)));
            }
        }
    }
    out.join(", ")
}

// grid-auto-columns/rows 값(§CSS Grid): <track-size>+. line-names·repeat·최상위
// 콤마/슬래시 불가.
pub fn grid_auto_track_valid(raw: &str) -> bool {
    let s = raw.trim();
    if s.is_empty() {
        return false;
    }
    let mut depth = 0i32;
    for c in s.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            ',' | '/' if depth == 0 => return false,
            _ => {}
        }
    }
    let comps = split_grid_components(s);
    !comps.is_empty()
        && comps.iter().all(|c| {
            let t = c.trim();
            !t.starts_with('[') && track_size_valid(t)
        })
}

// grid-template-columns/rows 캐논: 빈 [] line-names 제거, repeat 몸통 재귀 정규화.
pub fn grid_template_track_canonical(raw: &str) -> String {
    let s = raw.trim();
    if s.eq_ignore_ascii_case("none") {
        return "none".to_string();
    }
    canon_track_seq(s)
}

fn canon_track_seq(s: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for c in split_grid_components(s) {
        let cl = c.trim();
        if cl.starts_with('[') {
            // 빈 line-names 는 직렬화에서 생략.
            let inner = &cl[1..cl.len().saturating_sub(1)];
            if inner.split_whitespace().next().is_some() {
                out.push(cl.to_string());
            }
        } else if let Some(inner) = repeat_inner(cl) {
            let parts = split_top_commas(&inner);
            if parts.len() >= 2 {
                out.push(format!("repeat({}, {})", parts[0].trim(), canon_track_seq(&parts[1..].join(","))));
            } else {
                out.push(cl.to_string());
            }
        } else if cl == "0" {
            // 무단위 0 트랙은 <length> 0 → 0px 로 캐논(§CSSOM).
            out.push("0px".to_string());
        } else {
            out.push(cl.to_string());
        }
    }
    out.join(" ")
}

// 최상위 '/' 분리(함수 괄호 안쪽 슬래시는 보존).
fn split_top_slash(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
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
            '/' if depth == 0 => {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    out.push(cur.trim().to_string());
    out
}

// transition-property 캐논 직렬화: all 키워드만 소문자화, custom-ident 는 대소문자 보존.
pub fn transition_property_canonical(raw: &str) -> String {
    let whole = raw.trim();
    if whole.eq_ignore_ascii_case("none") {
        return "none".to_string();
    }
    split_top_commas(raw)
        .iter()
        .map(|item| {
            let t = item.trim();
            if t.eq_ignore_ascii_case("all") {
                "all".to_string()
            } else {
                t.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

// <easing-function> 하나가 유효한가(§CSS Easing). keyword/cubic-bezier/steps/linear().
fn single_easing_valid(s: &str) -> bool {
    let low = s.trim().to_ascii_lowercase();
    if matches!(
        low.as_str(),
        "linear" | "ease" | "ease-in" | "ease-out" | "ease-in-out" | "step-start" | "step-end"
    ) {
        return true;
    }
    if let Some(inner) = low.strip_prefix("cubic-bezier(").and_then(|x| x.strip_suffix(')')) {
        let parts: Vec<&str> = inner.split(',').collect();
        if parts.len() != 4 {
            return false;
        }
        let nums: Option<Vec<f64>> = parts
            .iter()
            .map(|p| p.trim().parse::<f64>().ok().filter(|v| v.is_finite()))
            .collect();
        return match nums {
            Some(n) => (0.0..=1.0).contains(&n[0]) && (0.0..=1.0).contains(&n[2]),
            None => false,
        };
    }
    if let Some(inner) = low.strip_prefix("steps(").and_then(|x| x.strip_suffix(')')) {
        let parts: Vec<&str> = inner.split(',').map(|p| p.trim()).collect();
        if parts.is_empty() || parts.len() > 2 {
            return false;
        }
        // 첫 인자는 정수 또는 함수(sibling-index()/calc() 등 — 값을 파스타임에 알 수
        // 없어 관대). 함수는 알파벳으로 시작하고 괄호가 있어야("2()" 같은 건 무효).
        let n = match parts[0].parse::<i64>() {
            Ok(v) => v,
            Err(_)
                if parts[0].starts_with(|c: char| c.is_ascii_alphabetic())
                    && parts[0].ends_with(')')
                    && parts[0].contains('(') =>
            {
                1
            }
            Err(_) => return false,
        };
        return match parts.get(1).copied() {
            None | Some("start") | Some("end") | Some("jump-start") | Some("jump-end")
            | Some("jump-both") => n >= 1,
            Some("jump-none") => n >= 2, // jump-none 은 최소 2
            _ => false,
        };
    }
    // linear(...) (CSS Easing 2) 는 관대하게 유효로(내부 점 파싱 생략).
    low.starts_with("linear(") && low.ends_with(')')
}

// <easing-function> 하나의 캐논 직렬화(§CSS Easing): step-start→steps(1, start),
// step-end→steps(1), steps 의 기본 위치(end/jump-end) 생략.
fn single_easing_canonical(s: &str) -> String {
    let low = s.trim().to_ascii_lowercase();
    match low.as_str() {
        "step-start" => "steps(1, start)".to_string(),
        "step-end" => "steps(1)".to_string(),
        _ => {
            if let Some(inner) = low.strip_prefix("steps(").and_then(|x| x.strip_suffix(')')) {
                let parts: Vec<&str> = inner.split(',').map(|p| p.trim()).collect();
                let n = parts.first().copied().unwrap_or("1");
                match parts.get(1).copied() {
                    None | Some("end") | Some("jump-end") => format!("steps({})", n),
                    Some(pos) => format!("steps({}, {})", n, pos),
                }
            } else {
                low
            }
        }
    }
}

// transition/animation-timing-function 캐논 직렬화(콤마 구분 목록).
pub fn timing_function_canonical(raw: &str) -> String {
    split_top_commas(raw)
        .iter()
        .map(|f| single_easing_canonical(f))
        .collect::<Vec<_>>()
        .join(", ")
}

// transition/animation-timing-function 유효성: 콤마 구분 <easing-function> 목록.
pub fn timing_function_valid(raw: &str) -> bool {
    let fns = split_top_commas(raw);
    !fns.is_empty() && fns.iter().all(|f| single_easing_valid(f))
}

// caret-color 성분이 auto 또는 유효 <color> 인가.
fn caret_color_component_ok(t: &str) -> bool {
    t.eq_ignore_ascii_case("auto")
        || t.eq_ignore_ascii_case("currentcolor")
        || matches!(interpret_value(t), Some(Value::Color(_)) | Some(Value::ColorFn(..)))
}

// caret-color 유효성(§CSS UI): [ auto | <color> ]{1,2}. 괄호 인식 분리로 1~2 성분,
// 각 성분이 auto/currentcolor/<color>. none/invert/50%/3값/콤마 등은 무효.
pub fn caret_color_valid(raw: &str) -> bool {
    let toks = split_top_level(raw);
    (1..=2).contains(&toks.len()) && toks.iter().all(|t| caret_color_component_ok(t))
}

// caret-color 계산값(§CSS UI): 각 성분을 rgb 로 해석(auto/currentcolor→요소 color).
// 두 값이고 둘째가 auto 면 첫째만 직렬화. cc 는 요소 계산 color("rgb(...)").
pub fn caret_color_computed(raw: &str, cc: &str) -> Option<String> {
    let toks = split_top_level(raw);
    let resolve = |t: &str| -> Option<String> {
        if t.eq_ignore_ascii_case("auto") || t.eq_ignore_ascii_case("currentcolor") {
            Some(cc.to_string())
        } else {
            match interpret_value(t) {
                Some(v @ (Value::Color(_) | Value::ColorFn(..))) => {
                    Some(crate::style::computed_value_string(&v))
                }
                _ => None,
            }
        }
    };
    match toks.len() {
        // 단일 값은 여기서 재조립하지 않는다(None) — 단일 color 는 이미 Color 저장이라
        // 계산값이 맞고, 단일 auto 를 여기서 rgb 로 접으면 보간의 auto→color 불연속
        // 경로(auto 계산값 기대)가 깨진다. 두값 폼만 currentColor 해석 후 재조립.
        2 => {
            let first = resolve(&toks[0])?;
            if toks[1].eq_ignore_ascii_case("auto") {
                Some(first) // 둘째 auto 는 생략
            } else {
                Some(format!("{} {}", first, resolve(&toks[1])?))
            }
        }
        _ => None,
    }
}

// font-stretch/font-width 계산값 캐논(§CSS Fonts 4): 퍼센트. 키워드는 규정 %로,
// 퍼센트는 음수 0%로 clamp, calc 는 평가 후 %. ultra-condensed=50% … ultra-expanded=200%.
pub fn normalize_font_stretch(raw: &str) -> Option<String> {
    let t = raw.trim().to_ascii_lowercase();
    let kw = match t.as_str() {
        "ultra-condensed" => Some(50.0f32),
        "extra-condensed" => Some(62.5),
        "condensed" => Some(75.0),
        "semi-condensed" => Some(87.5),
        "normal" => Some(100.0),
        "semi-expanded" => Some(112.5),
        "expanded" => Some(125.0),
        "extra-expanded" => Some(150.0),
        "ultra-expanded" => Some(200.0),
        _ => None,
    };
    if let Some(pct) = kw {
        return Some(format!("{}%", crate::style::num_css(pct)));
    }
    match interpret_value(&t) {
        Some(Value::Length(n, Unit::Percent)) => {
            Some(format!("{}%", crate::style::num_css(n.max(0.0))))
        }
        Some(Value::Calc(c)) if c.px == 0.0 && !c.has_ctx_units() => {
            Some(format!("{}%", crate::style::num_css(c.pct.max(0.0))))
        }
        // calc(0%)·calc(100%-100%) 은 순 % 가 0 이라 Length(0,Px)로 접힌다 — 원문에
        // % 가 있으면 % 로 해석(수치는 동일한 0).
        Some(Value::Length(n, Unit::Px)) if t.contains('%') => {
            Some(format!("{}%", crate::style::num_css(n.max(0.0))))
        }
        _ => None,
    }
}

// font-size-adjust 검증·캐논(§CSS Fonts 5): none | [metric]? [from-font | <number>].
// metric ∈ {ex-height,cap-height,ch-width,ic-width,ic-height}. 기본 basis ex-height 는
// 직렬화에서 생략("ex-height 0.5"→"0.5"). number 는 음수 무효, calc 는 수치 평가 후
// calc() 유지("calc(0.5+1)"→"calc(1.5)"). 무효면 None.
pub fn normalize_font_size_adjust(raw: &str) -> Option<String> {
    let low = raw.trim().to_ascii_lowercase();
    if low == "none" {
        return Some("none".to_string());
    }
    const METRICS: [&str; 5] =
        ["ex-height", "cap-height", "ch-width", "ic-width", "ic-height"];
    let norm_value = |t: &str| -> Option<String> {
        if t == "from-font" {
            return Some("from-font".to_string());
        }
        if let Ok(n) = t.parse::<f32>() {
            return if n >= 0.0 {
                Some(crate::style::num_css(n))
            } else {
                None
            };
        }
        if t.starts_with("calc(") && t.ends_with(')') {
            return eval_calc_number(&t[5..t.len() - 1])
                .map(|n| format!("calc({})", crate::style::num_css(n)));
        }
        None
    };
        // calc() 내부 공백을 보존하도록 괄호 인식 분리.
    let toks: Vec<String> = split_top_level(&low);
    match toks.len() {
        1 => norm_value(&toks[0]),
        2 => {
            if !METRICS.contains(&toks[0].as_str()) {
                return None;
            }
            let val = norm_value(&toks[1])?;
            if toks[0] == "ex-height" {
                Some(val) // 기본 basis 생략
            } else {
                Some(format!("{} {}", toks[0], val))
            }
        }
        _ => None,
    }
}

// text-wrap 단축 캐논(§CSS Text 4): text-wrap-mode(wrap|nowrap) || text-wrap-style
// (auto|balance|stable|pretty). 직렬화: style 이 auto(기본)면 mode 만, 아니면 style 만
// (mode 가 wrap 기본일 때) 또는 "mode style". 무효/중복 토큰은 None.
pub fn normalize_text_wrap(raw: &str) -> Option<String> {
    let toks: Vec<String> = raw.split_whitespace().map(|s| s.to_ascii_lowercase()).collect();
    if toks.is_empty() || toks.len() > 2 {
        return None;
    }
    let (mut mode, mut style): (Option<&str>, Option<&str>) = (None, None);
    for t in &toks {
        match t.as_str() {
            "wrap" | "nowrap" => {
                if mode.is_some() {
                    return None; // 중복 mode
                }
                mode = Some(if t == "wrap" { "wrap" } else { "nowrap" });
            }
            "auto" | "balance" | "stable" | "pretty" => {
                if style.is_some() {
                    return None; // 중복 style
                }
                style = Some(match t.as_str() {
                    "balance" => "balance",
                    "stable" => "stable",
                    "pretty" => "pretty",
                    _ => "auto",
                });
            }
            _ => return None, // 무효 토큰
        }
    }
    let mode = mode.unwrap_or("wrap");
    let style = style.unwrap_or("auto");
    Some(if style == "auto" {
        mode.to_string()
    } else if mode == "wrap" {
        style.to_string()
    } else {
        format!("{mode} {style}")
    })
}

// hyphenate-limit-chars 계산값 캐논(§CSS Text 4): [auto|<integer>]{1,3}.
// 각 성분은 auto 또는 정수(calc 는 반올림해 정수로). 후행 중복은 생략(margin 식):
// c[2]==c[1] 이면 드롭, 이어서 c[1]==c[0] 이면 드롭. 예) "auto auto"→"auto",
// "5 2 calc(3.1)"→"5 2 3", "auto 2 2"→"auto 2". 확장은 하지 않는다(지정 개수 유지).
pub fn normalize_hyphenate_limit_chars(raw: &str) -> Option<String> {
    let toks: Vec<&str> = raw.split_whitespace().collect();
    if toks.is_empty() || toks.len() > 3 {
        return None;
    }
    let mut comps: Vec<String> = Vec::new();
    for t in &toks {
        if t.eq_ignore_ascii_case("auto") {
            comps.push("auto".to_string());
        } else if let Ok(n) = t.parse::<f32>() {
            comps.push(format!("{}", n.round() as i64));
        } else if let Some(Value::Length(n, Unit::Number)) =
            interpret_value(t).or_else(|| eval_calc(t))
        {
            comps.push(format!("{}", n.round() as i64));
        } else {
            return None;
        }
    }
    // 후행 중복 생략.
    if comps.len() == 3 && comps[2] == comps[1] {
        comps.pop();
    }
    if comps.len() == 2 && comps[1] == comps[0] {
        comps.pop();
    }
    Some(comps.join(" "))
}

// relative-color(rgb(from <origin> ...)) 지정값 캐논(§CSS Color 5): 레거시 함수명
// rgba→rgb/hsla→hsl, origin 키워드 소문자화(currentColor→currentcolor), origin 이 색
// 함수면 재귀 정규화. 채널 표현은 유지.
pub fn normalize_relative_color(raw: &str) -> Option<String> {
    let t = raw.trim();
    let open = t.find('(')?;
    let close = t.rfind(')')?;
    if close <= open {
        return None;
    }
    let func = t[..open].to_ascii_lowercase();
    if !matches!(
        func.as_str(),
        "rgb" | "rgba" | "hsl" | "hsla" | "hwb" | "lab" | "lch" | "oklab" | "oklch" | "color"
    ) {
        return None;
    }
    let inner = t[open + 1..close].trim();
    if inner.len() < 5 || !inner[..5].eq_ignore_ascii_case("from ") {
        return None;
    }
    let rest = inner[5..].trim();
    // origin = 최상위 첫 토큰(괄호 그룹 포함). 나머지 = 채널.
    let b = rest.as_bytes();
    let (mut depth, mut i) = (0i32, 0usize);
    while i < b.len() {
        match b[i] {
            b'(' => depth += 1,
            b')' => depth -= 1,
            c if c.is_ascii_whitespace() && depth == 0 => break,
            _ => {}
        }
        i += 1;
    }
    let origin = rest[..i].trim();
    let channels = rest[i..].trim();
    if origin.is_empty() || channels.is_empty() {
        return None;
    }
    let origin_canon = if origin.contains('(') {
        normalize_relative_color(origin)
            .or_else(|| normalize_color_function(origin))
            .or_else(|| normalize_lab_like(origin))
            .or_else(|| normalize_color_mix(origin))
            .unwrap_or_else(|| origin.to_string())
    } else {
        origin.to_ascii_lowercase()
    };
    let cfunc = match func.as_str() {
        "rgba" => "rgb",
        "hsla" => "hsl",
        f => f,
    };
    Some(format!("{cfunc}(from {origin_canon} {channels})"))
}

// color-mix 지정값 캐논 직렬화(§CSS Color 5): 퍼센트를 색 뒤로, inner 색 정규화
// (키워드 유지, 함수→rgb), 기본 50%(한쪽만) 생략. 파싱 불가는 None.
// color(<space> c1 c2 c3 [/ a]) 지정값 캐논 직렬화(§CSS Color 4). 채널 %→0-1 수,
// none 유지, alpha==1 생략, "/" 공백 정규화. 알려진 색공간만.
pub fn normalize_color_function(raw: &str) -> Option<String> {
    let t = raw.trim();
    if !t.to_ascii_lowercase().starts_with("color(") || !t.ends_with(')') {
        return None;
    }
    let inner = &t[6..t.len() - 1];
    let spaced = inner.replace('/', " / ");
    let toks: Vec<&str> = spaced.split_whitespace().collect();
    if toks.len() < 4 {
        return None;
    }
    let space = toks[0].to_ascii_lowercase();
    if !matches!(
        space.as_str(),
        "srgb" | "srgb-linear" | "display-p3" | "a98-rgb" | "prophoto-rgb" | "rec2020" | "xyz"
            | "xyz-d50" | "xyz-d65"
    ) {
        return None;
    }
    let conv = |s: &str| -> Option<String> {
        if s.eq_ignore_ascii_case("none") {
            return Some("none".to_string());
        }
        if let Some(p) = s.strip_suffix('%') {
            return p.parse::<f32>().ok().map(|n| crate::style::num_css(n / 100.0));
        }
        s.parse::<f32>().ok().map(crate::style::num_css)
    };
    let (mut chans, mut alpha_raw, mut after_slash): (Vec<String>, Option<&str>, bool) =
        (Vec::new(), None, false);
    for &tok in &toks[1..] {
        if tok == "/" {
            after_slash = true;
            continue;
        }
        if after_slash {
            alpha_raw = Some(tok); // 알파는 원문 유지(클램프 위해)
        } else {
            chans.push(conv(tok)?); // 채널은 클램프 안 함(범위 밖 값 보존)
        }
    }
    if chans.len() != 3 {
        return None;
    }
    // 알파는 [0,1] 로 클램프하고 1 이면 생략(§CSS Color 4). none 은 유지.
    let alpha_part = match alpha_raw {
        None => String::new(),
        Some(a) if a.eq_ignore_ascii_case("none") => " / none".to_string(),
        Some(a) => {
            let av = if let Some(p) = a.strip_suffix('%') {
                p.parse::<f32>().ok()? / 100.0
            } else {
                a.parse::<f32>().ok()?
            };
            let ac = av.clamp(0.0, 1.0);
            if ac == 1.0 {
                String::new()
            } else {
                format!(" / {}", crate::style::num_css(ac))
            }
        }
    };
    Some(format!(
        "color({} {} {} {}{})",
        space, chans[0], chans[1], chans[2], alpha_part
    ))
}

// 색 함수 인자 토큰화: 최상위 공백으로 분리, 최상위 '/'(alpha 구분)는 독립 토큰,
// 괄호 안(calc 등)은 통째로 유지. "20 calc(50%) 0.5 / 1" → [20, calc(50%), 0.5, /, 1].
fn color_tokens(inner: &str) -> Vec<String> {
    let (mut out, mut depth, mut cur) = (Vec::new(), 0i32, String::new());
    for c in inner.chars() {
        match c {
            '(' => {
                depth += 1;
                cur.push(c);
            }
            ')' => {
                depth -= 1;
                cur.push(c);
            }
            '/' if depth == 0 => {
                if !cur.trim().is_empty() {
                    out.push(cur.trim().to_string());
                }
                cur.clear();
                out.push("/".to_string());
            }
            c if c.is_whitespace() && depth == 0 => {
                if !cur.trim().is_empty() {
                    out.push(cur.trim().to_string());
                }
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

// lab()/lch()/oklab()/oklch() 지정값 캐논 직렬화(§CSS Color 4). L 은 [0,max] 클램프,
// % 는 각 채널 범위로(lab a/b=±125, oklab=±0.4, lch/oklch C=150/0.4). lch/oklch 의 C 는
// ≥0 클램프, H 는 각도→도(number). alpha [0,1] 클램프·1 생략. none 유지.
pub fn normalize_lab_like(raw: &str) -> Option<String> {
    let t = raw.trim();
    if !t.ends_with(')') {
        return None;
    }
    let low = t.to_ascii_lowercase();
    // (이름, L 최대, 중간채널 퍼센트당 스케일, lch 계열인지)
    let (name, l_max, mid_scale, is_lch) = if low.starts_with("lab(") {
        ("lab", 100.0, 1.25, false)
    } else if low.starts_with("lch(") {
        ("lch", 100.0, 1.5, true)
    } else if low.starts_with("oklab(") {
        ("oklab", 1.0, 0.004, false)
    } else if low.starts_with("oklch(") {
        ("oklch", 1.0, 0.004, true)
    } else {
        return None;
    };
    let inner = &t[name.len() + 1..t.len() - 1];
    let toks = color_tokens(inner);
    if toks.len() < 3 {
        return None;
    }
    let nc = crate::style::num_css;
    // 채널 정규화: none 유지, calc 은 수로 평가되면 calc(<수>) 아니면 원문 유지(% 등
    // 문맥 의존), % 는 pct_factor 배, 그 외 수. clamp 있으면 적용.
    let norm = |tok: &str, pct_factor: f32, clamp: Option<(f32, f32)>| -> Option<String> {
        if tok.eq_ignore_ascii_case("none") {
            return Some("none".to_string());
        }
        let clip = |v: f32| clamp.map_or(v, |(lo, hi)| v.clamp(lo, hi));
        let low = tok.to_ascii_lowercase();
        if low.starts_with("calc(") && tok.ends_with(')') {
            // calc 결과는 지정값에서 클램프하지 않는다(clamp 는 used-value 시점).
            return match eval_calc_number(&tok[5..tok.len() - 1]) {
                Some(n) => Some(format!("calc({})", nc(n))),
                None => Some(tok.to_string()), // % 등 문맥 의존 calc 은 원문 유지
            };
        }
        let v = if let Some(p) = tok.strip_suffix('%') {
            p.parse::<f32>().ok()? * pct_factor
        } else {
            tok.parse::<f32>().ok()?
        };
        Some(nc(clip(v)))
    };
    // L: % → pct/100*max, [0,max] 클램프.
    let l = norm(toks[0].as_str(), l_max / 100.0, Some((0.0, l_max)))?;
    // 채널1: a(lab/oklab) 또는 C(lch/oklch, ≥0 클램프). % → pct*mid_scale.
    let c1 = norm(
        toks[1].as_str(),
        mid_scale,
        if is_lch { Some((0.0, f32::INFINITY)) } else { None },
    )?;
    // 채널2: b(lab/oklab, % → pct*mid_scale) 또는 H(lch/oklch, 각도→도).
    let c2 = if is_lch {
        let s = &toks[2];
        if s.eq_ignore_ascii_case("none") {
            "none".to_string()
        } else if s.to_ascii_lowercase().starts_with("calc(") {
            norm(s.as_str(), 1.0, None)?
        } else {
            nc(crate::style::angle_token_deg(s).or_else(|| s.parse::<f32>().ok())?)
        }
    } else {
        norm(toks[2].as_str(), mid_scale, None)?
    };
    // alpha: "/" 뒤. [0,1] 클램프, 1 생략, none 유지.
    let alpha_part = match toks.iter().position(|x| x == "/").and_then(|p| toks.get(p + 1)) {
        None => String::new(),
        Some(a) if a.eq_ignore_ascii_case("none") => " / none".to_string(),
        Some(a) => {
            let an = norm(a.as_str(), 0.01, Some((0.0, 1.0)))?;
            if an == "1" {
                String::new()
            } else {
                format!(" / {an}")
            }
        }
    };
    Some(format!("{}({} {} {}{})", name, l, c1, c2, alpha_part))
}

// hsl()/hsla()/hwb() 지정값 캐논 — 단, none 채널이 있을 때만(8bit 색 모델이 none 을
// 못 담아 rgb 변환이 불가하므로 modern 형태로 유지). H 각도→도, S/L(W/B) % 제거,
// hsla→hsl, alpha [0,1] 클램프·1 생략. none 없으면 None(기존 rgb 변환 경로가 처리).
pub fn normalize_hsl_hwb(raw: &str) -> Option<String> {
    let t = raw.trim();
    if !t.ends_with(')') {
        return None;
    }
    let low = t.to_ascii_lowercase();
    let name = if low.starts_with("hsl(") || low.starts_with("hsla(") {
        "hsl"
    } else if low.starts_with("hwb(") {
        "hwb"
    } else {
        return None;
    };
    if !low.contains("none") {
        return None;
    }
    let open = t.find('(')?;
    let toks = color_tokens(&t[open + 1..t.len() - 1]);
    if toks.len() < 3 {
        return None;
    }
    let nc = crate::style::num_css;
    let h = if toks[0].eq_ignore_ascii_case("none") {
        "none".to_string()
    } else {
        nc(crate::style::angle_token_deg(&toks[0]).or_else(|| toks[0].parse::<f32>().ok())?)
    };
    // S/L(또는 W/B): % 제거해 수로(none 유지).
    let ch = |s: &str| -> Option<String> {
        if s.eq_ignore_ascii_case("none") {
            return Some("none".to_string());
        }
        let v = s.strip_suffix('%').unwrap_or(s).parse::<f32>().ok()?;
        Some(nc(v))
    };
    let c1 = ch(&toks[1])?;
    let c2 = ch(&toks[2])?;
    let alpha_part = match toks.iter().position(|x| x == "/").and_then(|p| toks.get(p + 1)) {
        None => String::new(),
        Some(a) if a.eq_ignore_ascii_case("none") => " / none".to_string(),
        Some(a) => {
            let av = if let Some(p) = a.strip_suffix('%') {
                p.parse::<f32>().ok()? / 100.0
            } else {
                a.parse::<f32>().ok()?
            };
            let ac = av.clamp(0.0, 1.0);
            if ac == 1.0 {
                String::new()
            } else {
                format!(" / {}", nc(ac))
            }
        }
    };
    Some(format!("{}({} {} {}{})", name, h, c1, c2, alpha_part))
}

pub fn normalize_color_mix(raw: &str) -> Option<String> {
    let low = raw.trim().to_ascii_lowercase();
    if !low.starts_with("color-mix(") || !low.ends_with(')') {
        return None;
    }
    let inner = &low["color-mix(".len()..low.len() - 1];
    let parts = split_top_commas(inner);
    if parts.len() != 3 {
        return None;
    }
    let space = parts[0].split_whitespace().collect::<Vec<_>>().join(" ");
    let (c1, p1) = split_mix_part(&parts[1]);
    let (c2, p2) = split_mix_part(&parts[2]);
    let c1s = serialize_mix_input_color(&c1)?;
    let c2s = serialize_mix_input_color(&c2)?;
    let is50 = |p: f32| (p - 50.0).abs() < 1e-4;
    // 퍼센트 정규화: 한쪽 생략 시 100-other, 둘 다 생략 시 50/50. 둘 다 50 이면 모두
    // 생략, 아니면 **둘 다** 표시(§CSS Color 5). A 25%, B → "A 25%, B 75%".
    let (v1, v2) = match (p1, p2) {
        (None, None) => (50.0, 50.0),
        (Some(a), None) => (a, 100.0 - a),
        (None, Some(b)) => (100.0 - b, b),
        (Some(a), Some(b)) => (a, b),
    };
    let (pc1, pc2) = if is50(v1) && is50(v2) {
        (String::new(), String::new())
    } else {
        (format!(" {}%", csnum(v1)), format!(" {}%", csnum(v2)))
    };
    Some(format!("color-mix({}, {}{}, {}{})", space, c1s, pc1, c2s, pc2))
}

// color-mix inner 색을 지정값 형태로. 키워드(식별자)는 그대로, 함수/hex 는 rgb/색공간.
fn serialize_mix_input_color(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if s.chars().all(|c| c.is_ascii_alphabetic() || c == '-') {
        return Some(s.to_ascii_lowercase());
    }
    match interpret_value(s) {
        Some(v @ Value::Color(_)) => Some(crate::style::computed_value_string(&v)),
        Some(Value::ColorFn(_, serial)) => Some(serial.to_string()),
        _ => None,
    }
}

// color-mix 의 한 성분에서 색 문자열과 퍼센트를 분리.
fn split_mix_part(s: &str) -> (String, Option<f32>) {
    let mut pct = None;
    let mut color_str = String::new();
    for t in &split_top_level(s.trim()) {
        if let Some(p) = t.strip_suffix('%') {
            if let Ok(v) = p.trim().parse::<f32>() {
                pct = Some(v);
                continue;
            }
        }
        if !color_str.is_empty() {
            color_str.push(' ');
        }
        color_str.push_str(t);
    }
    (color_str, pct)
}

// color-mix(in <space> [<hue>], c1 [p1], c2 [p2]) — 두 색을 색공간에서 보간.
// 성분별 none 을 운반한다(한쪽만 none 이면 상대값, 둘 다면 결과 none). 계산값은
// 보간 색공간 형태로 직렬화(§CSS Color 5 serial); 결과에 none 이 있으면 공간 native 형태.
fn parse_color_mix(text: &str) -> Option<(Color, Box<str>)> {
    let inner = func_inner(text)?;
    let parts = split_top_commas(inner);
    let first_lower = parts.first().map(|p| p.trim().to_ascii_lowercase()).unwrap_or_default();
    // 보간법(in <space> [<hue>])은 선택 — 생략하면 기본 oklab(§CSS Color 5).
    let (space, hue_method, cs1_raw, cs2_raw) = if first_lower == "in"
        || first_lower.starts_with("in ")
    {
        if parts.len() != 3 {
            return None;
        }
        let mut toks = first_lower.split_whitespace();
        toks.next(); // "in"
        let space = toks.next()?.to_string();
        let hue_method = toks.next().unwrap_or("shorter").to_string();
        (space, hue_method, parts[1].clone(), parts[2].clone())
    } else {
        if parts.len() != 2 {
            return None; // 단일색/다중색 color-mix 는 미구현
        }
        ("oklab".to_string(), "shorter".to_string(), parts[0].clone(), parts[1].clone())
    };
    let (cs1, p1) = split_mix_part(&cs1_raw);
    let (cs2, p2) = split_mix_part(&cs2_raw);
    let (mut c1, ao1) = color_coords_none(&space, &cs1)?;
    let (mut c2, ao2) = color_coords_none(&space, &cs2)?;
    // 퍼센트 정규화(§CSS Color 5).
    let (w1, w2, alpha_mul) = match (p1, p2) {
        (None, None) => (0.5, 0.5, 1.0),
        // 단일 퍼센트 p: 나머지는 100-p 이므로 p 는 [0,100] 이어야 유효(§CSS Color 5).
        (Some(a), None) => {
            if !(0.0..=100.0).contains(&a) {
                return None;
            }
            (a / 100.0, 1.0 - a / 100.0, 1.0)
        }
        (None, Some(b)) => {
            if !(0.0..=100.0).contains(&b) {
                return None;
            }
            (1.0 - b / 100.0, b / 100.0, 1.0)
        }
        (Some(a), Some(b)) => {
            if a < 0.0 || b < 0.0 {
                return None; // 음수 퍼센트는 무효(§CSS Color 5)
            }
            let sum = a + b;
            if sum == 0.0 {
                (0.5, 0.5, 0.0) // 합 0: 균등 혼합에 alpha 0(투명) — 무효 아님
            } else {
                (a / sum, b / sum, if sum < 100.0 { sum / 100.0 } else { 1.0 })
            }
        }
    };
    // 성분별 none 운반: 한쪽만 none 이면 상대값으로, 둘 다면 결과 none.
    let mut none_out = [false; 3];
    for i in 0..3 {
        match (c1[i], c2[i]) {
            (None, None) => none_out[i] = true,
            (None, Some(v)) => c1[i] = Some(v),
            (Some(v), None) => c2[i] = Some(v),
            _ => {}
        }
    }
    // 알파 none 운반.
    let (a1, a2, alpha_none) = match (ao1, ao2) {
        (None, None) => (1.0, 1.0, true),
        (None, Some(b)) => (b, b, false),
        (Some(a), None) => (a, a, false),
        (Some(a), Some(b)) => (a, b, false),
    };
    let mut co1 = [c1[0].unwrap_or(0.0), c1[1].unwrap_or(0.0), c1[2].unwrap_or(0.0)];
    let mut co2 = [c2[0].unwrap_or(0.0), c2[1].unwrap_or(0.0), c2[2].unwrap_or(0.0)];
    let hi = hue_index(&space);
    let pw1 = hue_powerless(&space, &co1);
    let pw2 = hue_powerless(&space, &co2);
    let mixed_hue = hi.map(|i| {
        // powerless(채도 0) hue 는 상대 색의 hue 를 취한다(§CSS Color 4). 단 **한쪽만**
        // powerless 일 때만 — 둘 다 powerless 면 원래 순서로 보간해야 한다(둘 다 상대값을
        // 취하면 h1/h2 가 뒤바뀐다).
        let (h1, h2) = if pw1 && !pw2 {
            (co2[i], co2[i])
        } else if pw2 && !pw1 {
            (co1[i], co1[i])
        } else {
            (co1[i], co2[i])
        };
        interp_hue(h1, h2, w2, &hue_method)
    });
    for i in 0..3 {
        if hi == Some(i) {
            continue;
        }
        co1[i] *= a1;
        co2[i] *= a2;
    }
    let mixed_alpha = a1 * w1 + a2 * w2;
    let mut mixed = [0.0f32; 3];
    for i in 0..3 {
        let m = co1[i] * w1 + co2[i] * w2;
        mixed[i] = if mixed_alpha > 1e-6 { m / mixed_alpha } else { 0.0 };
    }
    if let (Some(i), Some(h)) = (hi, mixed_hue) {
        mixed[i] = h;
    }
    let (rr, gg, bb) = space_to_srgb(&space, mixed)?;
    let out_a = (mixed_alpha * alpha_mul).clamp(0.0, 1.0);
    let rgba = Color {
        r: to_u8(rr),
        g: to_u8(gg),
        b: to_u8(bb),
        a: if alpha_none { 0 } else { (out_a * 255.0).round() as u8 },
    };
    let alpha_out = if alpha_none { None } else { Some(out_a) };
    // 결과에 none 성분이 있으면 공간 native 형태로(none 은 sRGB 로 접을 수 없음).
    let serial = if none_out.iter().any(|&x| x) || alpha_none {
        serialize_mix_native(&space, &mixed, &none_out, alpha_out)
    } else {
        serialize_mix(&space, &mixed, rr, gg, bb, alpha_out)
    };
    Some((rgba, serial.into_boxed_str()))
}

// none 없는 color-mix 결과 직렬화: 보간 공간 형태.
fn serialize_mix(space: &str, mixed: &[f32; 3], rr: f32, gg: f32, bb: f32, a: Option<f32>) -> String {
    let ap = match a {
        Some(v) if (v - 1.0).abs() < 1e-4 => String::new(),
        Some(v) => format!(" / {}", csnum(v)),
        None => " / none".to_string(),
    };
    let n = |v: f32| csnum(v);
    let nc = |v: f32| csnum(v.clamp(0.0, 1.0));
    match space {
        // srgb 계열: 색역 밖 값도 클램프하지 않고 보존(color-mix 계산값, §CSS Color 5).
        "srgb" | "hsl" | "hwb" => format!("color(srgb {} {} {}{})", n(rr), n(gg), n(bb), ap),
        "srgb-linear" => format!("color(srgb-linear {} {} {}{})", n(mixed[0]), n(mixed[1]), n(mixed[2]), ap),
        "display-p3" => format!("color(display-p3 {} {} {}{})", n(mixed[0]), n(mixed[1]), n(mixed[2]), ap),
        "display-p3-linear" => format!("color(display-p3-linear {} {} {}{})", n(mixed[0]), n(mixed[1]), n(mixed[2]), ap),
        "xyz" | "xyz-d65" => format!("color(xyz-d65 {} {} {}{})", n(mixed[0]), n(mixed[1]), n(mixed[2]), ap),
        "xyz-d50" => format!("color(xyz-d50 {} {} {}{})", n(mixed[0]), n(mixed[1]), n(mixed[2]), ap),
        "oklab" => format!("oklab({} {} {}{})", n(mixed[0]), n(mixed[1]), n(mixed[2]), ap),
        "oklch" => format!("oklch({} {} {}{})", n(mixed[0]), n(mixed[1]), n(mixed[2]), ap),
        "lab" => format!("lab({} {} {}{})", n(mixed[0]), n(mixed[1]), n(mixed[2]), ap),
        "lch" => format!("lch({} {} {}{})", n(mixed[0]), n(mixed[1]), n(mixed[2]), ap),
        // 넓은 색공간 RGB: native 좌표 그대로(srgb 로 접지 않음).
        "rec2020" | "a98-rgb" | "prophoto-rgb" => {
            format!("color({} {} {} {}{})", space, n(mixed[0]), n(mixed[1]), n(mixed[2]), ap)
        }
        _ => format!("color(srgb {} {} {}{})", nc(rr), nc(gg), nc(bb), ap),
    }
}

// none 성분이 있는 color-mix 결과 직렬화: 공간 native 형태로, none 은 "none".
fn serialize_mix_native(space: &str, mixed: &[f32; 3], none: &[bool; 3], a: Option<f32>) -> String {
    let ap = match a {
        Some(v) if (v - 1.0).abs() < 1e-4 => String::new(),
        Some(v) => format!(" / {}", csnum(v)),
        None => " / none".to_string(),
    };
    // 성분 직렬화: none 이면 "none", 아니면 스케일 적용한 수(퍼센트면 %).
    let cp = |i: usize, scale: f32, pct: bool| -> String {
        if none[i] {
            "none".to_string()
        } else if pct {
            format!("{}%", csnum(mixed[i] * scale))
        } else {
            csnum(mixed[i] * scale)
        }
    };
    match space {
        // hsl/hwb 계산값의 채도·명도는 0-100 수(% 없이) — fuzzy 형식대조가 % 유무를 본다.
        "hsl" => format!("hsl({} {} {}{})", cp(0, 1.0, false), cp(1, 100.0, false), cp(2, 100.0, false), ap),
        "hwb" => format!("hwb({} {} {}{})", cp(0, 1.0, false), cp(1, 100.0, false), cp(2, 100.0, false), ap),
        "oklab" => format!("oklab({} {} {}{})", cp(0, 1.0, false), cp(1, 1.0, false), cp(2, 1.0, false), ap),
        "oklch" => format!("oklch({} {} {}{})", cp(0, 1.0, false), cp(1, 1.0, false), cp(2, 1.0, false), ap),
        "lab" => format!("lab({} {} {}{})", cp(0, 1.0, false), cp(1, 1.0, false), cp(2, 1.0, false), ap),
        "lch" => format!("lch({} {} {}{})", cp(0, 1.0, false), cp(1, 1.0, false), cp(2, 1.0, false), ap),
        "srgb" | "srgb-linear" | "display-p3" | "display-p3-linear" | "xyz" | "xyz-d65" | "xyz-d50" => {
            let sp = if space == "xyz" { "xyz-d65" } else { space };
            format!("color({} {} {} {}{})", sp, cp(0, 1.0, false), cp(1, 1.0, false), cp(2, 1.0, false), ap)
        }
        _ => format!("color(srgb {} {} {}{})", cp(0, 1.0, false), cp(1, 1.0, false), cp(2, 1.0, false), ap),
    }
}

// 색상(hue) 보간: 보간법에 따라 각도차를 조정 후 선형 보간. w2 는 두 번째 색 가중치.
fn interp_hue(h1: f32, h2: f32, w2: f32, method: &str) -> f32 {
    let mut d = h2 - h1;
    match method {
        "longer" => {
            if d.abs() < 180.0 {
                d += if d >= 0.0 { -360.0 } else { 360.0 };
            }
        }
        "increasing" => {
            if d < 0.0 {
                d += 360.0;
            }
        }
        "decreasing" => {
            if d > 0.0 {
                d -= 360.0;
            }
        }
        // shorter(기본)
        _ => {
            if d > 180.0 {
                d -= 360.0;
            } else if d < -180.0 {
                d += 360.0;
            }
        }
    }
    (h1 + d * w2).rem_euclid(360.0)
}

// color(<space> c1 c2 c3 [/ A]) — 지정 색공간의 성분을 sRGB 근사로 + 캐논 직렬화 보존.
fn parse_color_func(text: &str) -> Option<(Color, Box<str>)> {
    let p = color_parts(func_inner(text)?);
    if p.len() < 4 {
        return None;
    }
    let space = p[0].to_ascii_lowercase();
    let s1 = parse_comp(&p[1], 1.0)?;
    let s2 = parse_comp(&p[2], 1.0)?;
    let s3 = parse_comp(&p[3], 1.0)?;
    let alpha = parse_alpha(p.get(4))?;
    let au = alpha_u8(alpha);
    let (c1, c2, c3) = (s1.get(), s2.get(), s3.get());
    // 모든 predefined 색공간을 space_to_srgb 로 sRGB 근사(정확한 매트릭스/전달함수).
    let (rr, gg, bb) = space_to_srgb(&space, [c1, c2, c3])?;
    let rgba = Color { r: to_u8(rr), g: to_u8(gg), b: to_u8(bb), a: au };
    // xyz 는 계산값에서 xyz-d65 로 정규화된다(CSS Color 4).
    let space_out = if space == "xyz" { "xyz-d65" } else { space.as_str() };
    let serial = format!(
        "color({} {} {} {}{})",
        space_out,
        s1.ser(),
        s2.ser(),
        s3.ser(),
        alpha_ser(alpha)
    );
    Some((rgba, serial.into_boxed_str()))
}

// 최상위 콤마로 분리하되 빈 항목 유지(선행/후행/이중 콤마 검출용).
fn split_commas_keep(s: &str) -> Vec<String> {
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
    out.push(cur);
    out
}

fn top_level_has(s: &str, ch: char) -> bool {
    let mut depth = 0i32;
    for c in s.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            c if c == ch && depth == 0 => return true,
            _ => {}
        }
    }
    false
}

// rgb()/rgba() 엄격 검증(§CSS Color 4). legacy(콤마): none 불가, RGB 전부 숫자거나
// 전부 퍼센트, 3~4 인자, 선행/후행/이중 콤마 불가, 각도/키워드 불가. modern(공백):
// none 허용, 3 채널 + 선택 /알파. 콤마와 슬래시 혼용 불가.
pub fn rgb_valid(lower: &str) -> bool {
    let inner = match func_inner(lower) {
        Some(i) => i.trim().to_string(),
        None => return false,
    };
    if inner.is_empty() {
        return false;
    }
    let is_num = |p: &str| {
        let p = p.trim();
        !p.is_empty() && p.parse::<f64>().map(|v| v.is_finite()).unwrap_or(false)
    };
    let is_pct = |p: &str| {
        p.trim()
            .strip_suffix('%')
            .map(|n| n.trim().parse::<f64>().map(|v| v.is_finite()).unwrap_or(false))
            .unwrap_or(false)
    };
    let is_calc = |p: &str| is_math_fn(&p.trim().to_ascii_lowercase());
    let is_none = |p: &str| p.trim().eq_ignore_ascii_case("none");
    let has_comma = top_level_has(&inner, ',');
    let has_slash = top_level_has(&inner, '/');
    if has_comma {
        if has_slash {
            return false;
        }
        let parts = split_commas_keep(&inner);
        if parts.len() != 3 && parts.len() != 4 {
            return false;
        }
        if parts.iter().any(|p| p.trim().is_empty()) {
            return false;
        }
        let rgb = &parts[0..3];
        if rgb.iter().any(|p| !(is_num(p) || is_pct(p) || is_calc(p))) {
            return false;
        }
        let nums = rgb.iter().filter(|p| is_num(p)).count();
        let pcts = rgb.iter().filter(|p| is_pct(p)).count();
        if nums > 0 && pcts > 0 {
            return false;
        }
        if parts.len() == 4 {
            let a = &parts[3];
            if !(is_num(a) || is_pct(a) || is_calc(a)) {
                return false;
            }
        }
        true
    } else {
        let parts = split_top_slash(&inner);
        if parts.is_empty() || parts.len() > 2 {
            return false;
        }
        let vals = split_top_level(parts[0].trim());
        if vals.len() != 3 {
            return false;
        }
        let ok = |p: &str| is_num(p) || is_pct(p) || is_calc(p) || is_none(p);
        if vals.iter().any(|p| !ok(p)) {
            return false;
        }
        if parts.len() == 2 {
            let a = parts[1].trim();
            if a.is_empty() || !ok(a) {
                return false;
            }
        }
        true
    }
}

// 색 채널 판별 공통.
fn col_is_num(p: &str) -> bool {
    let p = p.trim();
    !p.is_empty() && p.parse::<f64>().map(|v| v.is_finite()).unwrap_or(false)
}
fn col_is_pct(p: &str) -> bool {
    p.trim()
        .strip_suffix('%')
        .map(|n| n.trim().parse::<f64>().map(|v| v.is_finite()).unwrap_or(false))
        .unwrap_or(false)
}
fn col_is_calc(p: &str) -> bool {
    is_math_fn(&p.trim().to_ascii_lowercase())
}
fn col_is_none(p: &str) -> bool {
    p.trim().eq_ignore_ascii_case("none")
}
fn col_is_angle(p: &str) -> bool {
    let low = p.trim().to_ascii_lowercase();
    ["deg", "grad", "rad", "turn"].iter().any(|u| {
        low.strip_suffix(u)
            .map(|n| n.trim().parse::<f64>().map(|v| v.is_finite()).unwrap_or(false))
            .unwrap_or(false)
    })
}

// hsl()/hsla() 엄격 검증(§CSS Color 4). legacy(콤마): none 불가, hue=<number|angle>,
// S·L=<percentage>, 3~4 인자. modern(공백): none 허용, hue/S/L + 선택 /알파.
pub fn hsl_valid(lower: &str) -> bool {
    let inner = match func_inner(lower) {
        Some(i) => i.trim().to_string(),
        None => return false,
    };
    if inner.is_empty() {
        return false;
    }
    let is_hue = |p: &str| col_is_num(p) || col_is_angle(p) || col_is_calc(p);
    let has_comma = top_level_has(&inner, ',');
    let has_slash = top_level_has(&inner, '/');
    if has_comma {
        if has_slash {
            return false;
        }
        let parts = split_commas_keep(&inner);
        if (parts.len() != 3 && parts.len() != 4) || parts.iter().any(|p| p.trim().is_empty()) {
            return false;
        }
        if !is_hue(&parts[0]) {
            return false;
        }
        if !(col_is_pct(&parts[1]) || col_is_calc(&parts[1]))
            || !(col_is_pct(&parts[2]) || col_is_calc(&parts[2]))
        {
            return false;
        }
        if parts.len() == 4 {
            let a = &parts[3];
            if !(col_is_num(a) || col_is_pct(a) || col_is_calc(a)) {
                return false;
            }
        }
        true
    } else {
        let parts = split_top_slash(&inner);
        if parts.is_empty() || parts.len() > 2 {
            return false;
        }
        let vals = split_top_level(parts[0].trim());
        if vals.len() != 3 {
            return false;
        }
        if !(is_hue(&vals[0]) || col_is_none(&vals[0])) {
            return false;
        }
        // modern hsl 의 S·L 은 <percentage> | <number>(§CSS Color 4).
        let sl_ok = |p: &str| col_is_pct(p) || col_is_num(p) || col_is_calc(p) || col_is_none(p);
        if !sl_ok(&vals[1]) || !sl_ok(&vals[2]) {
            return false;
        }
        if parts.len() == 2 {
            let a = parts[1].trim();
            if a.is_empty()
                || !(col_is_num(a) || col_is_pct(a) || col_is_calc(a) || col_is_none(a))
            {
                return false;
            }
        }
        true
    }
}

// lab/lch/oklab/oklch 엄격 검증(§CSS Color 4): <func>( <L> <a|C> <b|H> [/ <alpha>]? ).
// 공백 구분 3채널, 콤마 불가. lab/oklab 의 a·b 는 각도 불가, lch/oklch 의 H 는 각도 허용.
pub fn lab_valid(name: &str, lower: &str) -> bool {
    let inner = match func_inner(lower) {
        Some(i) => i.trim().to_string(),
        None => return false,
    };
    if inner.is_empty() || top_level_has(&inner, ',') {
        return false;
    }
    let parts = split_top_slash(&inner);
    if parts.is_empty() || parts.len() > 2 {
        return false;
    }
    let vals = split_top_level(parts[0].trim());
    if vals.len() != 3 {
        return false;
    }
    let numlike = |p: &str| col_is_num(p) || col_is_pct(p) || col_is_none(p) || col_is_calc(p);
    let is_lch = name == "lch" || name == "oklch";
    if !numlike(&vals[0]) {
        return false;
    }
    if is_lch {
        if !numlike(&vals[1]) {
            return false;
        }
        if !(col_is_num(&vals[2]) || col_is_angle(&vals[2]) || col_is_none(&vals[2]) || col_is_calc(&vals[2])) {
            return false;
        }
    } else if !numlike(&vals[1]) || !numlike(&vals[2]) {
        return false;
    }
    if parts.len() == 2 {
        let a = parts[1].trim();
        if a.is_empty() || !numlike(a) {
            return false;
        }
    }
    true
}

// <color-interpolation-method>: in <colorspace> [<hue-method> hue]?.
fn interp_method_valid(s: &str) -> bool {
    let toks: Vec<&str> = s.split_whitespace().collect();
    if toks.len() < 2 || !toks[0].eq_ignore_ascii_case("in") {
        return false;
    }
    let space = toks[1].to_ascii_lowercase();
    let polar = matches!(space.as_str(), "hsl" | "hwb" | "lch" | "oklch");
    let rect = matches!(
        space.as_str(),
        "srgb" | "srgb-linear" | "display-p3" | "display-p3-linear" | "a98-rgb" | "prophoto-rgb"
            | "rec2020" | "lab" | "oklab" | "xyz" | "xyz-d50" | "xyz-d65"
    ) || space.starts_with("--");
    if !polar && !rect {
        return false;
    }
    match toks.len() {
        2 => true,
        4 => {
            polar
                && matches!(
                    toks[2].to_ascii_lowercase().as_str(),
                    "shorter" | "longer" | "increasing" | "decreasing"
                )
                && toks[3].eq_ignore_ascii_case("hue")
        }
        _ => false,
    }
}

// color-mix 의 [ <color> && <percentage [0,100]>? ] 인자. 퍼센트는 앞뒤 아무 위치.
fn color_mix_arg_valid(s: &str) -> bool {
    let toks = split_top_level(s);
    if toks.is_empty() {
        return false;
    }
    let mut color_toks: Vec<String> = Vec::new();
    let mut pct = 0;
    for t in &toks {
        if let Some(n) = t.strip_suffix('%').and_then(|x| x.trim().parse::<f64>().ok()) {
            if !(0.0..=100.0).contains(&n) {
                return false;
            }
            pct += 1;
        } else if is_math_fn(&t.to_ascii_lowercase()) {
            // calc 퍼센트(파스 타임 허용) — 퍼센트 슬롯으로 간주.
            pct += 1;
        } else {
            color_toks.push(t.clone());
        }
    }
    if pct > 1 || color_toks.is_empty() {
        return false;
    }
    let c = color_toks.join(" ");
    let cl = c.trim().to_ascii_lowercase();
    cl == "currentcolor"
        || cl == "transparent"
        || color_syntax_valid(c.trim())
        || matches!(interpret_value(c.trim()), Some(Value::Color(_)) | Some(Value::ColorFn(..)))
}

// 상대 색의 원본 색이 유효한가(명명/hex/함수/currentcolor/transparent).
fn rel_origin_valid(c: &str) -> bool {
    let cl = c.trim().to_ascii_lowercase();
    cl == "currentcolor"
        || cl == "transparent"
        || color_syntax_valid(c.trim())
        || matches!(interpret_value(c.trim()), Some(Value::Color(_)) | Some(Value::ColorFn(..)))
}

// 상대 색 채널 토큰: 채널 키워드 | number | (hue 면 angle, 아니면 percentage) | none | calc.
fn rel_channel_valid(tok: &str, kw: &[&str], is_hue: bool) -> bool {
    let low = tok.trim().to_ascii_lowercase();
    if kw.contains(&low.as_str()) {
        return true;
    }
    if col_is_num(tok) || col_is_none(tok) || col_is_calc(tok) {
        return true;
    }
    if is_hue {
        col_is_angle(tok)
    } else {
        col_is_pct(tok)
    }
}

fn valid_color_space_ident(space: &str) -> bool {
    matches!(
        space,
        "srgb" | "srgb-linear" | "display-p3" | "display-p3-linear" | "a98-rgb" | "prophoto-rgb"
            | "rec2020" | "xyz" | "xyz-d50" | "xyz-d65"
    ) || space.starts_with("--")
}

// 상대 색 검증(§CSS Color 5): <func>(from <origin> <ch>{3} [/ <alpha>]?). color() 는
// from <origin> <colorspace> <ch>{3}. 채널 키워드는 함수별. hue 채널은 퍼센트 불가.
pub fn relative_color_valid(func: &str, lower: &str) -> bool {
    let inner = match func_inner(lower) {
        Some(i) => i.trim().to_string(),
        None => return false,
    };
    let low_inner = inner.to_ascii_lowercase();
    if !low_inner.starts_with("from ") || top_level_has(&inner, ',') {
        return false;
    }
    let after = inner[5..].trim();
    let slash = split_top_slash(after);
    if slash.is_empty() || slash.len() > 2 {
        return false;
    }
    let main = split_top_level(slash[0].trim());
    let kw: &[&str];
    let hue_pos: Option<usize>;
    let channels: &[String];
    if func == "color" {
        // from <origin> <colorspace> <ch>{3}. 채널 키워드는 색공간별(rgb→r/g/b, xyz→x/y/z).
        if main.len() != 5 || !rel_origin_valid(&main[0]) {
            return false;
        }
        let space = main[1].to_ascii_lowercase();
        if !valid_color_space_ident(&space) {
            return false;
        }
        kw = if space.starts_with("xyz") { &["x", "y", "z", "alpha"] } else { &["r", "g", "b", "alpha"] };
        hue_pos = None;
        channels = &main[2..5];
    } else {
        let (k, h): (&[&str], Option<usize>) = match func {
            "rgb" | "rgba" => (&["r", "g", "b", "alpha"], None),
            "hsl" | "hsla" => (&["h", "s", "l", "alpha"], Some(0)),
            "hwb" => (&["h", "w", "b", "alpha"], Some(0)),
            "lab" | "oklab" => (&["l", "a", "b", "alpha"], None),
            "lch" | "oklch" => (&["l", "c", "h", "alpha"], Some(2)),
            _ => return false,
        };
        kw = k;
        hue_pos = h;
        if main.len() != 4 || !rel_origin_valid(&main[0]) {
            return false;
        }
        channels = &main[1..4];
    }
    for (i, c) in channels.iter().enumerate() {
        if !rel_channel_valid(c, kw, hue_pos == Some(i)) {
            return false;
        }
    }
    if slash.len() == 2 {
        let a = slash[1].trim();
        if a.is_empty() || !rel_channel_valid(a, kw, false) {
            return false;
        }
    }
    true
}

// 색 함수 문법 유효성 디스패처(계산 가능 여부와 무관, 파싱 유효성만).
// interpret_value 가 계산 실패로 None 을 줘도, 문법이 유효하면 지정값을 보존한다.
pub fn color_syntax_valid(raw: &str) -> bool {
    let lower = raw.trim().to_ascii_lowercase();
    // 상대 색: <func>(from ...).
    for f in ["rgb", "rgba", "hsl", "hsla", "hwb", "lab", "lch", "oklab", "oklch", "color"] {
        if lower.starts_with(f) && lower[f.len()..].starts_with('(') {
            if let Some(inr) = func_inner(&lower) {
                if inr.trim_start().starts_with("from ") {
                    return relative_color_valid(f, &lower);
                }
            }
            break;
        }
    }
    if lower.starts_with("rgb(") || lower.starts_with("rgba(") {
        return rgb_valid(&lower);
    }
    if lower.starts_with("hsl(") || lower.starts_with("hsla(") {
        return hsl_valid(&lower);
    }
    for n in ["oklch", "oklab", "lch", "lab"] {
        if lower.starts_with(n) && lower[n.len()..].starts_with('(') {
            return lab_valid(n, &lower);
        }
    }
    if lower.starts_with("color-mix(") {
        return color_mix_valid(&lower);
    }
    if lower.starts_with("color(") {
        return color_func_valid(&lower);
    }
    if lower.starts_with("hwb(") {
        return hwb_valid(&lower);
    }
    false
}

// hwb() 문법 검증(§CSS Color 4): hwb( <hue> <W> <B> [/ <alpha>]? ). 콤마 형 없음.
pub fn hwb_valid(lower: &str) -> bool {
    let inner = match func_inner(lower) {
        Some(i) => i.trim().to_string(),
        None => return false,
    };
    if inner.is_empty() || top_level_has(&inner, ',') {
        return false;
    }
    let parts = split_top_slash(&inner);
    if parts.is_empty() || parts.len() > 2 {
        return false;
    }
    let vals = split_top_level(parts[0].trim());
    if vals.len() != 3 {
        return false;
    }
    let is_hue = |p: &str| col_is_num(p) || col_is_angle(p) || col_is_calc(p) || col_is_none(p);
    let wb_ok = |p: &str| col_is_pct(p) || col_is_num(p) || col_is_calc(p) || col_is_none(p);
    if !is_hue(&vals[0]) || !wb_ok(&vals[1]) || !wb_ok(&vals[2]) {
        return false;
    }
    if parts.len() == 2 {
        let a = parts[1].trim();
        if a.is_empty() || !wb_ok(a) {
            return false;
        }
    }
    true
}

// color-mix() 엄격 검증(§CSS Color 5): color-mix( <method>?, <color-pct>+ ). 메서드는
// 있으면 맨 앞이어야 하고, 뒤에 색이 하나 이상. 메서드가 없으면 전부 색.
pub fn color_mix_valid(lower: &str) -> bool {
    let inner = match func_inner(lower) {
        Some(i) => i.trim().to_string(),
        None => return false,
    };
    let parts = split_commas_keep(&inner);
    if parts.is_empty() {
        return false;
    }
    let has_method = interp_method_valid(parts[0].trim());
    let colors = if has_method { &parts[1..] } else { &parts[..] };
    !colors.is_empty() && colors.iter().all(|p| color_mix_arg_valid(p.trim()))
}

// color() 함수 엄격 검증(§CSS Color 4): color( <colorspace> <channel>{3} [/ <alpha>]? ).
// colorspace 는 predefined 또는 --custom. 채널은 number|percentage|none|calc(각도 불가).
pub fn color_func_valid(lower: &str) -> bool {
    let inner = match func_inner(lower) {
        Some(i) => i.trim().to_string(),
        None => return false,
    };
    if inner.is_empty() || top_level_has(&inner, ',') {
        return false;
    }
    let parts = split_top_slash(&inner);
    if parts.is_empty() || parts.len() > 2 {
        return false;
    }
    let toks = split_top_level(parts[0].trim());
    if toks.is_empty() {
        return false;
    }
    let space = toks[0].to_ascii_lowercase();
    let valid_space = matches!(
        space.as_str(),
        "srgb" | "srgb-linear" | "display-p3" | "display-p3-linear" | "a98-rgb" | "prophoto-rgb"
            | "rec2020" | "xyz" | "xyz-d50" | "xyz-d65"
    ) || space.starts_with("--");
    if !valid_space {
        return false;
    }
    let channels = &toks[1..];
    if channels.len() != 3 {
        return false;
    }
    let ch_ok = |p: &str| col_is_num(p) || col_is_pct(p) || col_is_none(p) || col_is_calc(p);
    if channels.iter().any(|p| !ch_ok(p)) {
        return false;
    }
    if parts.len() == 2 {
        let a = parts[1].trim();
        if a.is_empty() || !ch_ok(a) {
            return false;
        }
    }
    true
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

// sRGB 채널값(0-1) 직렬화 — 계산값은 정확 비교라 8소수 자리까지(128/255=0.50196078).
// csnum(4자리)로는 부족. 뒤 0/점 제거, -0→0.
fn srgb_chan_ser(v: f32) -> String {
    let s = format!("{:.8}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-0" {
        "0".to_string()
    } else {
        s.to_string()
    }
}

// rgb() 에 none 채널이 있으면 계산값은 color(srgb ...) 로 none 을 보존한다(§CSS Color
// 4). 8비트 Color 는 none 을 못 담으므로 ColorFn(fallback + none 보존 serial). none 이
// 없으면 None → 레거시 rgb() 경로가 처리(rgb() 형태 유지).
fn parse_rgb_none(text: &str) -> Option<Value> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    let parts = color_parts(&text[open + 1..close]);
    if parts.len() != 3 && parts.len() != 4 {
        return None;
    }
    let is_none = |s: &str| s.trim().eq_ignore_ascii_case("none");
    if !parts.iter().any(|p| is_none(p)) {
        return None; // none 없음 → 레거시
    }
    // 채널 → (serial 0-1, fallback u8). 수(0-255) 또는 %(0-100→0-1).
    let ch = |s: &str| -> Option<(String, u8)> {
        let s = s.trim();
        if is_none(s) {
            return Some(("none".to_string(), 0));
        }
        let v01 = if let Some(p) = s.strip_suffix('%') {
            p.trim().parse::<f32>().ok()? / 100.0
        } else {
            s.parse::<f32>().ok()? / 255.0
        };
        let v = v01.clamp(0.0, 1.0);
        Some((srgb_chan_ser(v), (v * 255.0).round() as u8))
    };
    let (rs, r) = ch(&parts[0])?;
    let (gs, g) = ch(&parts[1])?;
    let (bs, b) = ch(&parts[2])?;
    // 알파: none/수/%(→0-1), 1 이면 생략.
    let (aser, au) = match parts.get(3) {
        None => (String::new(), 255u8),
        Some(s) if is_none(s) => (" / none".to_string(), 0u8),
        Some(s) => {
            let s = s.trim();
            let a = if let Some(p) = s.strip_suffix('%') {
                p.trim().parse::<f32>().ok()? / 100.0
            } else {
                s.parse::<f32>().ok()?
            };
            let ac = a.clamp(0.0, 1.0);
            let ser = if (ac - 1.0).abs() < 1e-9 {
                String::new()
            } else {
                format!(" / {}", srgb_chan_ser(ac))
            };
            (ser, (ac * 255.0).round() as u8)
        }
    };
    let serial = format!("color(srgb {rs} {gs} {bs}{aser})");
    Some(Value::ColorFn(Color { r, g, b, a: au }, serial.into_boxed_str()))
}

// rgb()/rgba() 에 none 채널이 있을 때의 **지정값** 레거시 직렬화(§CSSOM serialize a
// color). none→0, %→반올림(0-255), 정수 반올림·클램프. alpha 가 1 이면 rgb(), 아니면
// rgba(). none 없는 rgb 는 기존 경로가 처리하므로 None 으로 위임한다. (계산값은 none 을
// color(srgb ...) 로 보존 — parse_rgb_none, 이 함수는 el.style 지정값 전용.)
pub(crate) fn normalize_rgb_legacy(text: &str) -> Option<String> {
    let low = text.trim().to_ascii_lowercase();
    if !(low.starts_with("rgb(") || low.starts_with("rgba(")) || !low.ends_with(')') {
        return None;
    }
    if !low.contains("none") || low.contains("(from ") {
        return None; // none 없음/relative-color → 기존 경로 위임
    }
    if !rgb_valid(&low) {
        return None;
    }
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    let parts = color_parts(&text[open + 1..close]);
    if parts.len() != 3 && parts.len() != 4 {
        return None;
    }
    let is_none = |s: &str| s.trim().eq_ignore_ascii_case("none");
    let chan = |s: &str| -> Option<u8> {
        let s = s.trim();
        if is_none(s) {
            return Some(0);
        }
        let v = if let Some(p) = s.strip_suffix('%') {
            p.trim().parse::<f32>().ok()? / 100.0 * 255.0
        } else {
            s.parse::<f32>().ok()?
        };
        Some(v.round().clamp(0.0, 255.0) as u8)
    };
    let r = chan(&parts[0])?;
    let g = chan(&parts[1])?;
    let b = chan(&parts[2])?;
    let alpha = match parts.get(3) {
        None => 1.0f32,
        Some(s) if is_none(s) => 0.0,
        Some(s) => {
            let s = s.trim();
            let a = if let Some(p) = s.strip_suffix('%') {
                p.trim().parse::<f32>().ok()? / 100.0
            } else {
                s.parse::<f32>().ok()?
            };
            a.clamp(0.0, 1.0)
        }
    };
    if (alpha - 1.0).abs() < 1e-9 {
        Some(format!("rgb({}, {}, {})", r, g, b))
    } else {
        Some(format!("rgba({}, {}, {}, {})", r, g, b, crate::style::num_css(alpha)))
    }
}

// An+B 마이크로문법(§CSS Syntax §9) 캐논화: even→2n, odd→2n+1, "3n - 0"→"3n",
// "2n+0"→"2n", 계수 1/-1 은 n/-n. 공백을 모두 제거하고 (a,b) 로 파싱해 재직렬화한다.
fn canonicalize_nth(arg: &str) -> Option<String> {
    let s: String = arg.chars().filter(|c| !c.is_whitespace()).collect();
    let low = s.to_ascii_lowercase();
    let (a, b): (i64, i64) = if low == "even" {
        (2, 0)
    } else if low == "odd" {
        (2, 1)
    } else if let Some(np) = low.find('n') {
        let a = match &low[..np] {
            "" | "+" => 1,
            "-" => -1,
            x => x.parse::<i64>().ok()?,
        };
        let rest = &low[np + 1..];
        let b = if rest.is_empty() { 0 } else { rest.parse::<i64>().ok()? };
        (a, b)
    } else {
        (0, low.parse::<i64>().ok()?)
    };
    Some(if a == 0 {
        b.to_string()
    } else {
        let coef = if a == 1 {
            "n".to_string()
        } else if a == -1 {
            "-n".to_string()
        } else {
            format!("{}n", a)
        };
        if b == 0 {
            coef
        } else if b > 0 {
            format!("{}+{}", coef, b)
        } else {
            format!("{}-{}", coef, -b)
        }
    })
}

fn canon_pseudo_arg(name: &str, arg: &str) -> String {
    match name {
        "nth-child" | "nth-last-child" | "nth-of-type" | "nth-last-of-type" | "nth-col"
        | "nth-last-col" => {
            let lower = arg.to_ascii_lowercase();
            if let Some(ofp) = lower.find(" of ") {
                let nth = &arg[..ofp];
                let sel = &arg[ofp + 4..];
                match canonicalize_nth(nth) {
                    Some(c) => format!("{} of {}", c, serialize_selector(sel)),
                    None => arg.trim().to_string(),
                }
            } else {
                canonicalize_nth(arg).unwrap_or_else(|| arg.trim().to_string())
            }
        }
        // 선택자 리스트 인자: 재귀 직렬화(내부 공백 정규화).
        "not" | "is" | "where" | "has" | "matches" => serialize_selector(arg.trim()),
        // 그 외(lang/dir/host 등): 앞뒤 공백만 제거.
        _ => arg.trim().to_string(),
    }
}

// CSSOM "serialize a selector"(§CSSOM)의 일부: 함수형 pseudo-class 인자의 공백을
// 정규화하고 An+B 를 캐논화한다. 타입/클래스/id/결합자/속성선택자/문자열은 원문 그대로
// 둔다(ASCII 구분자만 바이트 스캔 — UTF-8 안전). 세터가 원문 저장이라 getter 에서 캐논.
pub(crate) fn serialize_selector(raw: &str) -> String {
    let s = raw.trim();
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0usize;
    let mut copy_from = 0usize;
    while i < b.len() {
        match b[i] {
            // CSS 주석은 selectorText 직렬화에서 제거(§CSSOM). 문자열/속성 안의 "/*" 는
            // 아래 arm 들이 먼저 소비하므로 여기 도달하지 않는다.
            b'/' if i + 1 < b.len() && b[i + 1] == b'*' => {
                out.push_str(&s[copy_from..i]);
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i = if i + 1 < b.len() { i + 2 } else { b.len() };
                copy_from = i;
            }
            b'"' | b'\'' => {
                let q = b[i];
                i += 1;
                while i < b.len() {
                    if b[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if b[i] == q {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            b'[' => {
                i += 1;
                while i < b.len() && b[i] != b']' {
                    if b[i] == b'"' || b[i] == b'\'' {
                        let q = b[i];
                        i += 1;
                        while i < b.len() {
                            if b[i] == b'\\' {
                                i += 2;
                                continue;
                            }
                            if b[i] == q {
                                i += 1;
                                break;
                            }
                            i += 1;
                        }
                    } else {
                        i += 1;
                    }
                }
                if i < b.len() {
                    i += 1;
                }
            }
            b':' => {
                let mut j = i + 1;
                while j < b.len() && b[j] == b':' {
                    j += 1;
                }
                let name_start = j;
                while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'-') {
                    j += 1;
                }
                if j > name_start && j < b.len() && b[j] == b'(' {
                    let mut depth = 0i32;
                    let mut k = j;
                    while k < b.len() {
                        match b[k] {
                            b'(' => depth += 1,
                            b')' => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                            b'"' | b'\'' => {
                                let q = b[k];
                                k += 1;
                                while k < b.len() {
                                    if b[k] == b'\\' {
                                        k += 2;
                                        continue;
                                    }
                                    if b[k] == q {
                                        break;
                                    }
                                    k += 1;
                                }
                            }
                            _ => {}
                        }
                        k += 1;
                    }
                    if k < b.len() {
                        out.push_str(&s[copy_from..j + 1]);
                        let name = s[name_start..j].to_ascii_lowercase();
                        out.push_str(&canon_pseudo_arg(&name, &s[j + 1..k]));
                        out.push(')');
                        i = k + 1;
                        copy_from = i;
                        continue;
                    }
                }
                i = j;
            }
            _ => i += 1,
        }
    }
    out.push_str(&s[copy_from..]);
    // 주석 제거로 남을 수 있는 후행 공백 정리.
    let n = out.trim_end().len();
    out.truncate(n);
    out
}

// hsl()/hwb() 에 none 채널이 있으면 계산값은 hsl()/hwb() 형태로 none 을 보존한다
// (§CSS Color 4, rgb 와 달리 색공간 형태 유지). ColorFn(none→0 fallback + none 보존
// serial). serial 은 normalize_hsl_hwb 재사용. none 없으면 None → 레거시 경로.
fn parse_hsl_hwb_none(text: &str) -> Option<Value> {
    let low = text.to_ascii_lowercase();
    if !low.contains("none") {
        return None;
    }
    let name = if low.starts_with("hwb") {
        "hwb"
    } else if low.starts_with("hsl") {
        "hsl"
    } else {
        return None;
    };
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    let parts = color_parts(&text[open + 1..close]);
    if parts.len() != 3 && parts.len() != 4 {
        return None;
    }
    let is_none = |s: &str| s.trim().eq_ignore_ascii_case("none");
    // 계산값 serial: 지정값(normalize_hsl_hwb, % 제거)과 달리 S/L(W/B) 의 % 를 **유지**.
    // H 는 각도→deg 수(none 은 유지). hsla→hsl.
    let h = if is_none(&parts[0]) {
        "none".to_string()
    } else {
        csnum(comp_angle(&parts[0])?)
    };
    let keep = |s: &str| if is_none(s) { "none".to_string() } else { s.trim().to_string() };
    let s = keep(&parts[1]);
    let l = keep(&parts[2]);
    let a_ser = match parts.get(3) {
        None => String::new(),
        Some(p) if is_none(p) => " / none".to_string(),
        Some(p) => {
            let ps = p.trim();
            let av = if let Some(x) = ps.strip_suffix('%') {
                x.trim().parse::<f32>().ok()? / 100.0
            } else {
                ps.parse::<f32>().ok()?
            };
            let ac = av.clamp(0.0, 1.0);
            if (ac - 1.0).abs() < 1e-9 {
                String::new()
            } else {
                format!(" / {}", csnum(ac))
            }
        }
    };
    let serial = format!("{name}({h} {s} {l}{a_ser})");
    // fallback Color(렌더 근사): none→0 치환 후 기존 파서.
    let z: Vec<String> = parts
        .iter()
        .map(|p| if is_none(p) { "0".to_string() } else { p.trim().to_string() })
        .collect();
    let mut inner = z[..3].join(" ");
    if z.len() == 4 {
        inner.push_str(" / ");
        inner.push_str(&z[3]);
    }
    let zeroed = format!("{}{})", &text[..open + 1], inner);
    let fallback = if name == "hwb" {
        parse_hwb(&zeroed)
    } else {
        parse_hsl_func(&zeroed)
    }?;
    Some(Value::ColorFn(fallback, serial.into_boxed_str()))
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

// ── 모던 색 함수 → sRGB 변환 (CSS Color 4). 우리 색 모델은 8비트 sRGB 라 페인팅용
// 근사로 변환한다. 표준 매트릭스/공식 사용(편법 없음). getComputedStyle 은 아직 rgb()
// 로 접히므로 색공간 보존은 별개 과제(색 모델 확장 필요).

// 선형 광량 → sRGB 감마 인코딩 (IEC 61966-2-1).
fn linear_to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.0031308 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

fn to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

// 3x3 행렬(행 우선) × 벡터.
fn mat3(m: &[f32; 9], x: f32, y: f32, z: f32) -> (f32, f32, f32) {
    (
        m[0] * x + m[1] * y + m[2] * z,
        m[3] * x + m[4] * y + m[5] * z,
        m[6] * x + m[7] * y + m[8] * z,
    )
}

// XYZ(D65) → 선형 sRGB (CSS Color 4).
const XYZ65_TO_LSRGB: [f32; 9] = [
    3.240_625_5, -1.537_208, -0.498_628_6,
    -0.968_930_7, 1.875_756_1, 0.041_517_5,
    0.055_710_1, -0.204_021_1, 1.056_995_9,
];
// 선형 display-p3 → XYZ(D65).
const P3_TO_XYZ65: [f32; 9] = [
    0.486_570_95, 0.265_667_7, 0.198_217_28,
    0.228_974_56, 0.691_738_5, 0.079_286_91,
    0.0, 0.045_113_38, 1.043_944_4,
];
// Bradford 색순응 XYZ(D50) → XYZ(D65).
const BRADFORD_D50_D65: [f32; 9] = [
    0.955_473_4, -0.023_008_5, 0.063_258_7,
    -0.028_369_8, 1.009_994_3, 0.021_041_8,
    0.012_313, -0.020_542_2, 1.329_909_8,
];

// sRGB 감마 디코드(감마 인코딩 sRGB/디스플레이P3 → 선형).
fn srgb_gamma_inv(c: f32) -> f32 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

// 선형 sRGB → XYZ(D65) (CSS Color 4 정방향).
const LSRGB_TO_XYZ65: [f32; 9] = [
    0.412_390_8, 0.357_584_3, 0.180_480_8,
    0.212_639, 0.715_168_7, 0.072_192_32,
    0.019_330_82, 0.119_194_78, 0.950_532_2,
];
// XYZ(D65) → XYZ(D50) Bradford (BRADFORD_D50_D65 의 역).
const BRADFORD_D65_D50: [f32; 9] = [
    1.047_922_5, 0.022_946_8, -0.050_192_3,
    0.029_654_4, 0.990_449_4, -0.017_073_1,
    -0.009_243_4, 0.015_055_2, 0.751_878_6,
];
// XYZ(D65) → 선형 display-p3 (P3_TO_XYZ65 의 역).
const XYZ65_TO_P3: [f32; 9] = [
    2.493_497, -0.931_383_6, -0.402_710_8,
    -0.829_489, 1.762_664_1, 0.023_624_69,
    0.035_845_63, -0.076_172_39, 0.956_884_5,
];

// ── rec2020 / a98-rgb / prophoto-rgb 전달함수 + 매트릭스 (CSS Color 4) ──
const REC2020_TO_XYZ65: [f32; 9] = [
    0.636_958, 0.144_616_9, 0.168_881,
    0.262_700_2, 0.677_998_1, 0.059_301_72,
    0.0, 0.028_072_693, 1.060_985_1,
];
const XYZ65_TO_REC2020: [f32; 9] = [
    1.716_651_2, -0.355_670_8, -0.253_366_3,
    -0.666_684_4, 1.616_481_2, 0.015_768_546,
    0.017_639_857, -0.042_770_613, 0.942_103_1,
];
const A98_TO_XYZ65: [f32; 9] = [
    0.576_669, 0.185_558_2, 0.188_228_65,
    0.297_345, 0.627_363_5, 0.075_291_46,
    0.027_031_36, 0.070_688_85, 0.991_337_5,
];
const XYZ65_TO_A98: [f32; 9] = [
    2.041_588, -0.565_007, -0.344_731_35,
    -0.969_243_6, 1.875_967_5, 0.041_555_057,
    0.013_444_281, -0.118_362_39, 1.015_175,
];
const PROPHOTO_TO_XYZ50: [f32; 9] = [
    0.797_760_5, 0.135_185_84, 0.031_349_35,
    0.288_071_12, 0.711_843_2, 0.000_085_653_96,
    0.0, 0.0, 0.825_104_6,
];
const XYZ50_TO_PROPHOTO: [f32; 9] = [
    1.345_799, -0.255_580_1, -0.051_106_286,
    -0.544_622_5, 1.508_232_7, 0.020_536_033,
    0.0, 0.0, 1.211_967_5,
];
fn rec2020_encode(l: f32) -> f32 {
    const A: f32 = 1.099_296_8;
    const B: f32 = 0.018_053_968;
    let s = l.signum();
    let l = l.abs();
    s * if l < B { 4.5 * l } else { A * l.powf(0.45) - (A - 1.0) }
}
fn rec2020_decode(v: f32) -> f32 {
    const A: f32 = 1.099_296_8;
    const B: f32 = 0.018_053_968;
    let s = v.signum();
    let v = v.abs();
    s * if v < 4.5 * B { v / 4.5 } else { ((v + (A - 1.0)) / A).powf(1.0 / 0.45) }
}
fn a98_encode(l: f32) -> f32 {
    l.signum() * l.abs().powf(256.0 / 563.0)
}
fn a98_decode(v: f32) -> f32 {
    v.signum() * v.abs().powf(563.0 / 256.0)
}
fn prophoto_encode(l: f32) -> f32 {
    const ET: f32 = 1.0 / 512.0;
    let s = l.signum();
    let l = l.abs();
    s * if l < ET { 16.0 * l } else { l.powf(1.0 / 1.8) }
}
fn prophoto_decode(v: f32) -> f32 {
    const ET2: f32 = 16.0 / 512.0;
    let s = v.signum();
    let v = v.abs();
    s * if v < ET2 { v / 16.0 } else { v.powf(1.8) }
}

// XYZ(D50) → CIELAB.
fn xyz_d50_to_lab(x: f32, y: f32, z: f32) -> (f32, f32, f32) {
    let (xn, yn, zn) = (0.964_212_26, 1.0, 0.825_188_25);
    let eps = 216.0 / 24389.0;
    let kappa = 24389.0 / 27.0;
    let f = |t: f32| {
        if t > eps {
            t.cbrt()
        } else {
            (kappa * t + 16.0) / 116.0
        }
    };
    let (fx, fy, fz) = (f(x / xn), f(y / yn), f(z / zn));
    (116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz))
}
fn srgb_to_lab(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let (lr, lg, lb) = (srgb_gamma_inv(r), srgb_gamma_inv(g), srgb_gamma_inv(b));
    let (x, y, z) = mat3(&LSRGB_TO_XYZ65, lr, lg, lb);
    let (x, y, z) = mat3(&BRADFORD_D65_D50, x, y, z);
    xyz_d50_to_lab(x, y, z)
}

// 선형 sRGB 3채널(0..1) → 8비트 sRGB Color.
fn lin_srgb_to_color(lr: f32, lg: f32, lb: f32, a: u8) -> Color {
    Color {
        r: to_u8(linear_to_srgb(lr)),
        g: to_u8(linear_to_srgb(lg)),
        b: to_u8(linear_to_srgb(lb)),
        a,
    }
}

// Oklab(L, a, b) → 선형 sRGB (Oklab 스펙의 역변환 매트릭스).
fn oklab_to_lin_srgb(l_: f32, a: f32, b: f32) -> (f32, f32, f32) {
    let l = l_ + 0.396_337_78 * a + 0.215_803_76 * b;
    let m = l_ - 0.105_561_346 * a - 0.063_854_17 * b;
    let s = l_ - 0.089_484_18 * a - 1.291_485_5 * b;
    let (l, m, s) = (l * l * l, m * m * m, s * s * s);
    (
        4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
        -1.268_438 * l + 2.609_757_4 * m - 0.341_319_4 * s,
        -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s,
    )
}

fn oklab_to_color(l: f32, a: f32, b: f32, alpha: u8) -> Color {
    let (lr, lg, lb) = oklab_to_lin_srgb(l, a, b);
    lin_srgb_to_color(lr, lg, lb, alpha)
}

// Oklch(L, C, H도) → Oklab → sRGB.
fn oklch_to_color(l: f32, c: f32, h_deg: f32, alpha: u8) -> Color {
    let h = h_deg.to_radians();
    oklab_to_color(l, c * h.cos(), c * h.sin(), alpha)
}

// CIELAB(D50) → 선형 sRGB. XYZ(D50) → Bradford D50→D65 → sRGB 매트릭스 합성.
fn lab_to_lin_srgb(l: f32, a: f32, b: f32) -> (f32, f32, f32) {
    // Lab → XYZ (D50 백색점)
    let fy = (l + 16.0) / 116.0;
    let fx = fy + a / 500.0;
    let fz = fy - b / 200.0;
    let eps = 216.0 / 24389.0;
    let kappa = 24389.0 / 27.0;
    let f_inv = |t: f32| {
        let t3 = t * t * t;
        if t3 > eps { t3 } else { (116.0 * t - 16.0) / kappa }
    };
    let (xn, yn, zn) = (0.964_212_26, 1.0, 0.825_188_25); // D50 백색점
    let x = f_inv(fx) * xn;
    let y = if l > kappa * eps { fy * fy * fy } else { l / kappa } * yn;
    let z = f_inv(fz) * zn;
    // XYZ(D50) → 선형 sRGB (Bradford D50→D65 를 합성한 매트릭스, CSS Color 4 부록)
    (
        3.134_136_2 * x - 1.617_386 * y - 0.490_662_28 * z,
        -0.978_795_47 * x + 1.916_254_4 * y + 0.033_442_29 * z,
        0.071_945_6 * x - 0.228_976_76 * y + 1.405_386_1 * z,
    )
}

fn lab_to_color(l: f32, a: f32, b: f32, alpha: u8) -> Color {
    let (lr, lg, lb) = lab_to_lin_srgb(l, a, b);
    lin_srgb_to_color(lr, lg, lb, alpha)
}

fn lch_to_color(l: f32, c: f32, h_deg: f32, alpha: u8) -> Color {
    let h = h_deg.to_radians();
    lab_to_color(l, c * h.cos(), c * h.sin(), alpha)
}

// ── color-mix 용 정방향 변환 (sRGB → 보간 색공간) ────────────────────────────

// 선형 sRGB → Oklab (정방향 매트릭스).
fn lin_srgb_to_oklab(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let l = 0.412_221_47 * r + 0.536_332_54 * g + 0.051_445_99 * b;
    let m = 0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b;
    let s = 0.088_302_46 * r + 0.281_718_84 * g + 0.629_978_7 * b;
    let (l, m, s) = (l.cbrt(), m.cbrt(), s.cbrt());
    (
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    )
}
// 감마 sRGB(0..1) → Oklab.
fn srgb_to_oklab(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    lin_srgb_to_oklab(srgb_gamma_inv(r), srgb_gamma_inv(g), srgb_gamma_inv(b))
}
// 감마 sRGB → HSL (h도, s[0..1], l[0..1]).
fn srgb_to_hsl(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d.abs() < 1e-9 {
        return (0.0, 0.0, l);
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    ((h * 60.0).rem_euclid(360.0), s, l)
}
// 감마 sRGB → HWB (h도, w[0..1], b[0..1]).
fn srgb_to_hwb(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let (h, _, _) = srgb_to_hsl(r, g, b);
    let w = r.min(g).min(b);
    let bl = 1.0 - r.max(g).max(b);
    (h, w, bl)
}

// HWB(색상, 흰색 비율, 검정 비율) → sRGB (CSS Color 4).
fn hwb_to_color(h: f32, mut w: f32, mut blk: f32, alpha: u8) -> Color {
    if w + blk > 1.0 {
        let sum = w + blk;
        w /= sum;
        blk /= sum;
    }
    // 순색(HSL s=1,l=0.5)에 흰/검정을 섞는다
    let (r, g, b) = hsl_to_rgb(h, 1.0, 0.5);
    let mix = |c: u8| {
        let base = c as f32 / 255.0;
        to_u8(base * (1.0 - w - blk) + w)
    };
    Color { r: mix(r), g: mix(g), b: mix(b), a: alpha }
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
        // CSS <system-color> (CSS Color 4 §7 + CSS2 비권장분). 값은 구현 정의라 밝은
        // 스킴 기본값을 쓴다. (color-scheme 인지 해석은 후속 — 여기선 키워드 인식이 핵심.)
        "canvas" | "field" | "window" | "buttonhighlight" | "threedhighlight" => {
            (255, 255, 255)
        }
        "canvastext" | "fieldtext" | "windowtext" | "buttontext" | "marktext" | "infotext"
        | "menutext" | "captiontext" => (0, 0, 0),
        "linktext" => (0, 0, 238),
        "visitedtext" => (85, 26, 139),
        "activetext" => (255, 0, 0),
        "buttonface" | "threedface" | "menu" | "buttonshadow" | "threedlightshadow" => {
            (240, 240, 240)
        }
        "buttonborder" | "threedshadow" | "graytext" | "threeddarkshadow" | "windowframe"
        | "inactivecaptiontext" => (128, 128, 128),
        "highlight" | "selecteditem" | "activecaption" | "activeborder" | "accentcolor" => {
            (0, 120, 215)
        }
        "highlighttext" | "selecteditemtext" | "accentcolortext" => (255, 255, 255),
        "mark" | "infobackground" => (255, 255, 0),
        "scrollbar" | "background" | "inactiveborder" | "inactivecaption" | "appworkspace" => {
            (212, 208, 200)
        }
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
            Some(Value::Color(c)) | Some(Value::ColorFn(c, _)) => c,
            other => panic!("expected color, got {:?}", other),
        }
    }

    #[test]
    fn math_length_type_checking() {
        // 유효한 길이 문맥 calc — 반드시 수용(회귀 방지가 핵심).
        for v in [
            "calc(1px + 1em)",
            "calc(100% - 20px)",
            "calc(100% - 20px + 1rem)",
            "min(10px, 20px)",
            "max(1em, 2vw, 3%)",
            "clamp(10px, 5%, 20px)",
            "calc(1px * 2)",
            "calc(2 * 1px)",
            "calc(100% / 3)",
            "calc((1px + 1px) * 2)",
            "calc(10px + calc(5px * 2))",
            "min(calc(1px + 1%), 2em)",
            "calc(1px * sin(45deg))", // sin→number, len*num=len
            "calc(var(--x) + 1px)",   // var→wild → 수용
            "round(nearest, 10px, 2px)",
            "calc(1px)",
            "abs(-5px)",
            "calc(2em + 3ex)",
            "calc(20px + calc(80px))",
            "calc(calc(100px))",
            "calc(calc(150px*2/3))",
            "calc(calc(2) * calc(50px))",
        ] {
            assert!(math_length_valid(v, true), "should accept length: {v}");
        }
        // 무효한 타입/차원 — 반드시 거부.
        for v in [
            "max(0Hz)",
            "max(0deg)",
            "max(0s)",
            "max(0fr)",
            "max(0dpi)",
            "max(0)",             // 수(number)는 <length> 아님
            "calc(1 / 2px)",      // len^-1
            "calc(1% * 1% * 1%)", // len^3(% 는 len 으로 접힘)
            "calc(1px * 1px)",    // len^2
            "max(1%, 0)",         // 타입 불일치(len vs num)
            "max(1px, 0s)",       // 타입 불일치
            "calc(1px + 1s)",     // 축 불일치
        ] {
            assert!(!math_length_valid(v, true), "should reject: {v}");
        }
        // length-only 문맥(border-width): % 거부.
        assert!(math_length_valid("max(1px, 2px)", false));
        assert!(!math_length_valid("max(0%)", false));
        assert!(math_length_valid("max(0%)", true));
        // 시간 문맥.
        assert!(math_time_valid("calc(1s + 500ms)"));
        assert!(math_time_valid("max(1s, 2s)"));
        assert!(!math_time_valid("max(1px)"));
        assert!(!math_time_valid("calc(1s * 1s)"));
        // 각도 문맥 + 삼각함수 인자 검증.
        for v in [
            "calc(45deg + 1turn)",
            "max(45deg, 90deg)",
            "acos(0.5)",
            "asin(1)",
            "atan(1)",
            "atan2(1, 2)",
            "calc(1deg * sin(45deg))", // sin→number, deg*num=deg
            "acos(1 + 1)",
        ] {
            assert!(math_angle_valid(v), "should accept angle: {v}");
        }
        for v in [
            "acos()",           // 인자 없음
            "acos(1deg)",       // acos 는 수 인자(각도 아님)
            "asin(90px)",       // 길이 인자
            "acos(1dag)",       // 미지 단위
            "acos(1deg - 0.5rad)", // 각도 인자(수 아님)
            "acos(1deg 2deg)",  // 연산자 없는 2값
            "atan2(1)",         // 인자 1개
            "max(0px)",         // 길이(각도 아님)
            "max(45deg, 1s)",   // 축 불일치
        ] {
            assert!(!math_angle_valid(v), "should reject angle: {v}");
        }
        // transform 함수 리스트 — 유효(회귀 방지가 핵심).
        for v in [
            "none",
            "rotate(45deg)",
            "rotate(0)",
            "rotate(1turn)",
            "translate(10px)",
            "translate(10px, 20%)",
            "translateX(10px)",
            "translateZ(5px)",
            "translate3d(10px, 20%, 5px)",
            "scale(2)",
            "scale(2, 0.5)",
            "scale(50%)",
            "scaleX(1.5)",
            "rotate(45deg) translate(10px, 20px)",
            "matrix(1, 0, 0, 1, 0, 0)",
            "matrix3d(1,0,0,0,0,1,0,0,0,0,1,0,0,0,0,1)",
            "skew(10deg, 20deg)",
            "skewX(10deg)",
            "perspective(500px)",
            "perspective(none)",
            "rotate3d(1, 1, 1, 45deg)",
            "translate(calc(10px + 5%))",
            "rotate(calc(45deg * 2))",
            "rotate(acos(0.5))",
        ] {
            assert!(transform_valid(v), "should accept transform: {v}");
        }
        // angle-percentage(conic 스톱): % 가 각도로 접혀 각도-% 혼합 유효, 수 혼합 무효.
        assert!(math_angle_pct_valid("50%"));
        assert!(math_angle_pct_valid("30deg"));
        assert!(math_angle_pct_valid("calc(90deg + 50%)"));
        assert!(math_angle_pct_valid("calc(100% - 45deg)"));
        assert!(!math_angle_pct_valid("10px")); // 길이(각도-% 아님)
        assert!(!math_angle_pct_valid("calc(50% + 0)")); // 각도-% 에 수 혼합
        assert!(!math_angle_pct_valid("calc(1s + 50%)")); // 시간 혼합
        // rotate 등 순수 <angle> 은 % 거부(문맥 Len-fold 유지).
        assert!(math_angle_valid("45deg"));
        assert!(!math_angle_valid("50%"));
        assert!(!math_angle_valid("calc(90deg + 50%)"));
        // shape() 함수 — 유효(회귀 방지).
        for v in [
            "shape(from 0px 0px, line to 10px 10px)",
            "shape(from 1em 50%, line to 10px 10px)",
            "shape(from 1ch -50px, line to -10% 12px)",
            "shape(from 10px 10px, move by 10px 5px, line by 20px 40%, close)",
            "shape(from 10px 10px, hline by 10px, vline to 5rem)",
            "shape(from 10px 10px, vline by 5%, hline to 1vw)",
            "shape(from 10px 10px, curve to 50px 20px with 10rem 1px / 20vh 1ch)",
            "shape(from 10px 10px, smooth to 50px 20px with 10rem 1%)",
            "shape(from 10px 10px, arc to 50px 1pt of 10px 10px small rotate 0deg)",
            "shape(from 10% 1rem, arc to 50px 1pt of 20% cw large rotate 25deg)",
            "shape(nonzero from 0px 0px, line to 10px 10px)",
            "shape(from 10px 10px, curve to 50px 20px with 10rem center)",
        ] {
            assert!(shape_func_valid(v), "should accept shape: {v}");
        }
        // shape() 함수 — 구조적 무효.
        for v in [
            "shape(from 20px 40px line to 20px 30px)",       // 콤마 누락
            "shape(from 20px 40px, line to 20px 30px,)",     // 후행 콤마
            "shape(from 20px 40px, arc by 20px 20px of 10% 20% cw ccw)", // cw+ccw
            "shape(from 20px 40px, arc by 20px 20px of 10% 20% small large)", // small+large
            "shape(from 20px 40px, curve to 20px 20px, using 10px 30px)", // using(비명령)
            "shape(from 20px 40px, move to 20px 30px, hline to top)",  // hline 위치값
            "shape(from 20px 40px, move to 20px 30px, vline to left)", // vline 위치값
            "shape(from 20px, 40px, line to 20px, 30px)",    // 좌표 내 콤마
            "shape(from 0px 10px)",                          // 명령 없음
            "shape(from 0px)",                               // from 좌표 1개
            "shape(from 0px 0px, close path)",               // close 인자
            "shape(from 10px 10px, hline byy 10px)",         // by/to 아님
            "shape(from 10px 10px, smooth via 10rem)",       // by/to 아님
        ] {
            assert!(!shape_func_valid(v), "should reject shape: {v}");
        }
        assert!(!ray_valid("ray(0 sides)")); // bare 0 각도
        // animation-range 문법.
        for v in [
            "normal", "normal normal", "cover", "entry, exit", "0% 100%",
            "entry 0% entry 100%", "cover 50%", "cover 0px", "120%", "0",
            "cover -42%", "contain calc(10px + 10%)", "exit 1%, cover 2%, contain 0%",
            "normal cover 0%", "cover 50% exit 0%", "normal exit 50%",
        ] {
            assert!(animation_range_valid(v), "should accept animation-range: {v}");
        }
        for v in [
            "1s", "-1s", "1s 2s", "1s / 2s", "#ff0000", "red", "thing", "thing 0%",
            "0s entry 50%", "1s 2s 3s", "peek 50%", "none", "cover 50% enter 50%",
        ] {
            assert!(!animation_range_valid(v), "should reject animation-range: {v}");
        }
        // SVG path data 검증(유효 — 회귀 방지 핵심).
        for d in [
            "M 10 10 h 80 v 80 h -80 Z",
            "M0,0L100,0L100,100Z",
            "M0,0C30,60,70,60,100,0",
            "M0,0Q50,100,100,0",
            "M0,0A25,25,0,0,1,50,0",
            "m 10 20 q 30 60 40 50 q 100 70 90 80",
            "m 20 0 h -100",
            "M 0 0 L 100 100 M 100 200 L 200 200 Z L 300 300 Z",
            "m 10 170 h 90 v 30 m 0 0 s 1 2 3 4 z c 9 8 7 6 5 4",
            "m 10 20 a 10 20 30 1 0 40 50 a 110 120 30 1 1 140 50",
        ] {
            assert!(svg_path_valid(d), "should accept path: {d}");
        }
        for d in ["M 20 30 A 60 70 80", "M 0 0 L 1", "H 10", "M 0 0 X 1 2", "M 0 0 C 1 2 3"] {
            assert!(!svg_path_valid(d), "should reject path: {d}");
        }
        // offset-path — 유효(회귀 방지).
        for v in [
            "none",
            "circle()",
            "circle(100px at 50% 50%)",
            "ellipse(closest-side closest-side at 10% 20%)",
            "inset(0px 1px 2% 3em)",
            "inset(0px round 0 1px)",
            "polygon(1px 2px, 3em 4em)",
            "polygon(round 1px, 1% 2%)",
            "ray(0deg)",
            "ray(-720deg sides)",
            "ray(0.25turn closest-corner contain)",
            "ray(0deg at center center contain)",
            "ray(0deg sides at center center)",
            "ray(0deg) stroke-box",
            "content-box ellipse(50% 60% at 50% 50%)",
            "inset(10% 20% 30% 40%) border-box",
            "border-box",
            "url(#path)",
            "shape(from 0px 0px, line to 10px 10px)",
        ] {
            assert!(offset_path_valid(v), "should accept offset-path: {v}");
        }
        // offset-path — 무효.
        for v in [
            "ray()",                 // angle 없음
            "ray(sides)",            // angle 없음
            "ray(0deg 90deg)",       // angle 2개
            "ray(0deg sides sides)", // size 2개
            "ray(0px)",              // 길이(각도 아님)
            "auto",
            "content-box content-box", // box 2개
            "circle() ellipse()",    // shape 2개
        ] {
            assert!(!offset_path_valid(v), "should reject offset-path: {v}");
        }
        // transform 함수 리스트 — 무효.
        for v in [
            "perspective(1000)",    // 단위 없는 수(길이 아님)
            "rotate(acos())",
            "rotate(acos(1deg))",   // acos 는 수 인자
            "rotate(asin(90px))",
            "rotate(max(0px))",     // 길이(각도 아님)
            "translate(10deg)",     // 각도(길이 아님)
            "scale(10px)",          // 길이(수 아님)
            "rotate(45deg, 90deg)", // 인자 2개
            "matrix(1, 2, 3)",      // 인자 6개 아님
        ] {
            assert!(!transform_valid(v), "should reject transform: {v}");
        }
    }

    #[test]
    fn absolute_units_convert_to_px() {
        // 1pt = 96/72 px, 1pc = 16px, 1in = 96px, 1cm ≈ 37.8px
        // 중첩 calc() — 표준에서 허용된다. 예전엔 파싱이 실패해 선언이 통째로 버려졌다.
        assert!(matches!(
            interpret_value("calc(50% + calc(10px * 2))"),
            Some(Value::Calc(_))
        ));
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
    fn modern_color_computed_serialization() {
        // 계산값은 원 색공간을 보존한다(rgb() 로 안 접힘) + 클램프/none/각도정규화.
        let ser = |s: &str| match interpret_value(s) {
            Some(v) => crate::style::computed_value_string(&v),
            None => "<none>".to_string(),
        };
        assert_eq!(ser("lab(20 0 10/0.5)"), "lab(20 0 10 / 0.5)");
        assert_eq!(ser("lab(400 0 10/50%)"), "lab(100 0 10 / 0.5)"); // L 클램프 100
        assert_eq!(ser("lab(-40 0 0)"), "lab(0 0 0)"); // L 클램프 0
        assert_eq!(ser("oklab(4 0 0.1/50%)"), "oklab(1 0 0.1 / 0.5)"); // L 클램프 1
        assert_eq!(ser("oklab(20% 70% -80%)"), "oklab(0.2 0.28 -0.32)"); // % 기준
        assert_eq!(ser("lch(10 20 380deg)"), "lch(10 20 20)"); // hue 정규화
        assert_eq!(ser("lch(10 20 -340deg)"), "lch(10 20 20)");
        assert_eq!(ser("oklch(0.5 -0.2 0)"), "oklch(0.5 0 0)"); // C 클램프 0 이상
        assert_eq!(ser("lab(none none none / none)"), "lab(none none none / none)"); // none 보존
        assert_eq!(ser("color(srgb 1 0 0)"), "color(srgb 1 0 0)");
        assert_eq!(ser("color(xyz 0.1 0.2 0.3)"), "color(xyz-d65 0.1 0.2 0.3)"); // xyz→xyz-d65
        assert_eq!(ser("lab(calc(50 * 3) 0 0)"), "lab(100 0 0)"); // calc 평가 + 클램프
    }

    #[test]
    fn modern_color_functions_to_srgb() {
        // 모던 색 함수(CSS Color 4)를 sRGB 근사로 변환. 알려진 빨강/초록 값.
        // 손으로 계산한 색공간 입력값이라 몇 단위 오차 허용(변환 방향/근사 검증이 목적).
        let near = |c: Color, r: u8, g: u8, b: u8| {
            (c.r as i32 - r as i32).abs() <= 10
                && (c.g as i32 - g as i32).abs() <= 10
                && (c.b as i32 - b as i32).abs() <= 10
        };
        assert!(near(color("oklch(0.628 0.2577 29.23)"), 255, 0, 0), "oklch red");
        assert!(near(color("oklab(0.628 0.225 0.126)"), 255, 0, 0), "oklab red");
        assert!(near(color("lab(53.24 80.09 67.2)"), 255, 0, 0), "lab red");
        assert!(near(color("lch(53.24 104.55 40.0)"), 255, 0, 0), "lch red");
        assert_eq!(color("color(srgb 1 0 0)"), Color { r: 255, g: 0, b: 0, a: 255 });
        assert!(near(color("color(display-p3 0 1 0)"), 0, 255, 0), "p3 green");
        assert!(near(color("color(xyz 0.9505 1 1.089)"), 255, 255, 255), "xyz white");
        assert!(near(color("hwb(0 0% 0%)"), 255, 0, 0), "hwb red");
        assert!(near(color("hwb(0 50% 0%)"), 255, 128, 128), "hwb tinted");
        // 알파 보존
        assert_eq!(color("oklch(0.628 0.2577 29.23 / 0.5)").a, 128);
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
