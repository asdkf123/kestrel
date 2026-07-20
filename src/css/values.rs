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
                    return parse_relative_color(f, &lower).map(|(c, s)| Value::ColorFn(c, s));
                }
            }
            break;
        }
    }
    if lower.starts_with("rgb(") || lower.starts_with("rgba(") {
        return parse_rgb_func(&lower).map(Value::Color);
    }
    if lower.starts_with("hsl(") || lower.starts_with("hsla(") {
        return parse_hsl_func(&lower).map(Value::Color);
    }
    // 모던 색 함수(CSS Color 4). lab/lch/oklab/oklch/color() 는 계산값에서 자기 형태를
    // 보존하므로 Value::ColorFn(sRGB 근사 + 캐논 직렬화). hwb 는 rgb() 로 계산된다.
    for name in ["oklch", "oklab", "lch", "lab"] {
        if lower.starts_with(name) && lower[name.len()..].starts_with('(') {
            return parse_lab_family(name, &lower).map(|(c, s)| Value::ColorFn(c, s));
        }
    }
    if lower.starts_with("hwb(") {
        return parse_hwb(&lower).map(Value::Color);
    }
    if lower.starts_with("color-mix(") {
        return parse_color_mix(&lower).map(|(c, s)| Value::ColorFn(c, s));
    }
    if lower.starts_with("color(") {
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
    let items: Vec<String> =
        split_top_commas(inner).iter().map(|it| normalize_image_set_item(it.trim())).collect();
    format!("image-set({})", items.join(", "))
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
    let is_position = |seg: &str| {
        let toks = split_top_level(seg);
        toks.len() == 1 && matches!(interpret_value(&toks[0]), Some(Value::Length(..)))
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
    // color-stop-list: 색 스톱과 color hint(위치 단독)가 번갈아 온다. hint 는 첫/끝에
    // 올 수 없고 연속될 수 없다. 색 스톱은 위치를 최대 2개까지(색 + pos1 [pos2]).
    let list = &segs[start..];
    let mut stops = 0;
    let mut prev_was_hint = false;
    for (i, seg) in list.iter().enumerate() {
        let seg = seg.trim();
        if is_color(seg) {
            stops += 1;
            prev_was_hint = false;
            if split_top_level(seg).len() > 3 {
                return false; // 색 + 위치 2개 초과
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
    // toks[0]="at". 나머지 위치.
    let mut pos: Vec<String> = toks[1..].to_vec();
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
    format!("at {} {}", x, y)
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
            Some(Value::Length(n, _)) if n.is_finite() => n,
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
    let resolve = |tok: &str, i: usize| -> Option<Comp> {
        let subbed = subst_channels(tok.trim(), &kv);
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
                Comp::Val(oalpha)
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
    if parts.len() != 3 {
        return None;
    }
    let spec = parts[0].trim().to_ascii_lowercase();
    let mut toks = spec.split_whitespace();
    if toks.next() != Some("in") {
        return None;
    }
    let space = toks.next()?.to_string();
    let hue_method = toks.next().unwrap_or("shorter").to_string();
    let (cs1, p1) = split_mix_part(&parts[1]);
    let (cs2, p2) = split_mix_part(&parts[2]);
    let (mut c1, ao1) = color_coords_none(&space, &cs1)?;
    let (mut c2, ao2) = color_coords_none(&space, &cs2)?;
    // 퍼센트 정규화(§CSS Color 5).
    let (w1, w2, alpha_mul) = match (p1, p2) {
        (None, None) => (0.5, 0.5, 1.0),
        (Some(a), None) => (a / 100.0, 1.0 - a / 100.0, 1.0),
        (None, Some(b)) => (1.0 - b / 100.0, b / 100.0, 1.0),
        (Some(a), Some(b)) => {
            let sum = a + b;
            if sum <= 0.0 {
                return None;
            }
            (a / sum, b / sum, if sum < 100.0 { sum / 100.0 } else { 1.0 })
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
        let h1 = if pw1 { co2[i] } else { co1[i] };
        let h2 = if pw2 { co1[i] } else { co2[i] };
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
        "srgb" | "hsl" | "hwb" => format!("color(srgb {} {} {}{})", nc(rr), nc(gg), nc(bb), ap),
        "srgb-linear" => format!("color(srgb-linear {} {} {}{})", n(mixed[0]), n(mixed[1]), n(mixed[2]), ap),
        "display-p3" => format!("color(display-p3 {} {} {}{})", n(mixed[0]), n(mixed[1]), n(mixed[2]), ap),
        "display-p3-linear" => format!("color(display-p3-linear {} {} {}{})", n(mixed[0]), n(mixed[1]), n(mixed[2]), ap),
        "xyz" | "xyz-d65" => format!("color(xyz-d65 {} {} {}{})", n(mixed[0]), n(mixed[1]), n(mixed[2]), ap),
        "xyz-d50" => format!("color(xyz-d50 {} {} {}{})", n(mixed[0]), n(mixed[1]), n(mixed[2]), ap),
        "oklab" => format!("oklab({} {} {}{})", n(mixed[0]), n(mixed[1]), n(mixed[2]), ap),
        "oklch" => format!("oklch({} {} {}{})", n(mixed[0]), n(mixed[1]), n(mixed[2]), ap),
        "lab" => format!("lab({} {} {}{})", n(mixed[0]), n(mixed[1]), n(mixed[2]), ap),
        "lch" => format!("lch({} {} {}{})", n(mixed[0]), n(mixed[1]), n(mixed[2]), ap),
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
