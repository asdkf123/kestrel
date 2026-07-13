// 미디어 쿼리 매칭 (콤마 = OR, "and" = AND). 헤드리스 데스크톱 기준:
// 뷰포트 vw×800, light 스킴, hover/fine 포인터, 표준 대비/모션, 1x, sRGB.
// 표준: 미지원/미인식 특성은 매칭 실패로 본다(관용적 true 아님).
const VH_DEFAULT: f32 = 800.0;
const ROOT_FS: f32 = 16.0; // @media 의 em/rem 은 초기 폰트크기 기준

pub(crate) fn media_matches(query: &str, vw: f32) -> bool {
    let q = query.trim();
    if q.is_empty() {
        return true; // '@media { }' (조건 생략) → 매칭
    }
    q.split(',').any(|one| one_query_matches(one.trim(), vw))
}

fn one_query_matches(q: &str, vw: f32) -> bool {
    let ql = q.to_ascii_lowercase();
    let (negate, body) = match ql.trim().strip_prefix("not ") {
        Some(rest) => (true, rest.trim().to_string()),
        None => (false, ql.trim().trim_start_matches("only ").trim().to_string()),
    };
    let mut ok = true;
    for cond in body.split(" and ") {
        let cond = cond.trim();
        if cond.is_empty() {
            continue;
        }
        let pass = if cond.starts_with('(') {
            // 괄호 한 겹만 벗긴다 ((not (…)) 같은 중첩 유지)
            let inner = cond.strip_prefix('(').and_then(|s| s.strip_suffix(')')).unwrap_or(cond);
            feature_matches(inner.trim(), vw)
        } else {
            matches!(cond, "screen" | "all") // print/기타 타입 불일치
        };
        ok = ok && pass;
        if !ok {
            break;
        }
    }
    ok != negate
}

#[derive(Clone, Copy)]
enum Bound {
    Min,
    Max,
    Exact,
}

// 괄호 안 특성 하나 평가.
fn feature_matches(feat: &str, vw: f32) -> bool {
    // 중첩 부정 (not (feature)) — Level 4
    if let Some(rest) = feat.strip_prefix("not ") {
        let inner = rest.trim().strip_prefix('(').and_then(|s| s.strip_suffix(')')).unwrap_or(rest);
        return !feature_matches(inner.trim(), vw);
    }
    let vh = VH_DEFAULT;
    // Level 4 범위형: "width >= 768px", "400px <= width <= 700px"
    if feat.contains('<') || feat.contains('>') {
        return range_feature_matches(feat, vw, vh);
    }
    let (name, value) = match feat.split_once(':') {
        Some((n, v)) => (n.trim(), Some(v.trim())),
        None => (feat, None),
    };
    let (bound, base) = if let Some(b) = name.strip_prefix("min-") {
        (Bound::Min, b)
    } else if let Some(b) = name.strip_prefix("max-") {
        (Bound::Max, b)
    } else {
        (Bound::Exact, name)
    };
    match base {
        "width" | "device-width" => cmp_len(bound, value, vw),
        "height" | "device-height" => cmp_len(bound, value, vh),
        "orientation" => match value {
            Some("portrait") => vw < vh,
            Some("landscape") => vw >= vh,
            _ => false,
        },
        "resolution" => cmp_resolution(bound, value),
        "color" => value.map_or(true, |v| v.parse::<f32>().map(|n| n > 0.0).unwrap_or(false)),
        "monochrome" => matches!(value, Some("0")),
        "hover" | "any-hover" => matches!(value, Some("hover")) || value.is_none(),
        "pointer" | "any-pointer" => matches!(value, Some("fine")) || value.is_none(),
        "prefers-color-scheme" => match value {
            Some("light") | None => true,
            _ => false, // dark 등 → 불일치 (헤드리스=light)
        },
        "prefers-contrast" => matches!(value, Some("no-preference") | None),
        "prefers-reduced-motion" => !matches!(value, Some("reduce")),
        "prefers-reduced-data" => !matches!(value, Some("reduce")),
        "prefers-reduced-transparency" => !matches!(value, Some("reduce")),
        "display-mode" => matches!(value, Some("browser")) || value.is_none(),
        "scripting" => matches!(value, Some("enabled")) || value.is_none(),
        "update" => matches!(value, Some("fast")) || value.is_none(),
        // 우리 캔버스는 sRGB — p3/rec2020 을 지원한다고 하면 거짓말이다.
        // (예전엔 값과 무관하게 항상 true 라 넓은 색역 전용 스타일이 잘못 적용됐다)
        "color-gamut" => matches!(value, Some("srgb") | None),
        // 실제 뷰포트 비율로 비교한다. 예전엔 항상 true 라
        // `@media (aspect-ratio: 16/9)` 같은 조건이 무조건 매칭됐다.
        "aspect-ratio" | "device-aspect-ratio" => match value {
            None => true, // 특성 존재 여부만 물음 → 있음
            Some(v) => match parse_ratio(v) {
                Some(r) => match bound {
                    Bound::Min => vw / vh >= r,
                    Bound::Max => vw / vh <= r,
                    Bound::Exact => (vw / vh - r).abs() < 0.001,
                },
                None => false,
            },
        },
        _ => false, // 미인식 특성 → 불일치 (표준)
    }
}

// min-/max-/정확 길이 비교. value 없거나 파싱 실패면 불일치.
fn cmp_len(bound: Bound, value: Option<&str>, actual: f32) -> bool {
    let Some(len) = value.and_then(parse_len) else {
        return false;
    };
    match bound {
        Bound::Min => actual >= len,
        Bound::Max => actual <= len,
        Bound::Exact => (actual - len).abs() < 0.5,
    }
}

// 길이 → px. px/단위없음, em/rem(초기 16px 기준), pt 지원. 그 외 None.
fn parse_len(s: &str) -> Option<f32> {
    let s = s.trim();
    let num: String =
        s.chars().take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-').collect();
    let n = num.parse::<f32>().ok()?;
    match s[num.len()..].trim() {
        "px" | "" => Some(n),
        "em" | "rem" => Some(n * ROOT_FS),
        "pt" => Some(n * 96.0 / 72.0),
        _ => None,
    }
}

// resolution: 헤드리스 = 96dpi(1x). dppx/dpi/dpcm 를 dppx 로 환산해 비교.
fn cmp_resolution(bound: Bound, value: Option<&str>) -> bool {
    let Some(v) = value else {
        return false;
    };
    let num: String = v.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    let Ok(n) = num.parse::<f32>() else {
        return false;
    };
    let dppx = match v[num.len()..].trim() {
        "dppx" | "x" => n,
        "dpi" => n / 96.0,
        "dpcm" => n / 37.795,
        _ => return false,
    };
    match bound {
        Bound::Min => 1.0 >= dppx,
        Bound::Max => 1.0 <= dppx,
        Bound::Exact => (1.0 - dppx).abs() < 0.01,
    }
}

// 종횡비 값: "16/9" 또는 "1.5" (단일 수). 파싱 실패면 None.
fn parse_ratio(v: &str) -> Option<f32> {
    let v = v.trim();
    match v.split_once('/') {
        Some((w, h)) => {
            let w: f32 = w.trim().parse().ok()?;
            let h: f32 = h.trim().parse().ok()?;
            if h == 0.0 {
                return None;
            }
            Some(w / h)
        }
        None => v.parse().ok(),
    }
}

// "width >= 768px", "400px <= width <= 700px" 등 범위 문법.
fn range_feature_matches(feat: &str, vw: f32, vh: f32) -> bool {
    let toks: Vec<&str> = feat.split_whitespace().collect();
    let Some(np) = toks
        .iter()
        .position(|t| matches!(*t, "width" | "height" | "device-width" | "device-height"))
    else {
        return false;
    };
    let actual = if toks[np].contains("height") { vh } else { vw };
    let mut ok = true;
    // 왼쪽 경계: len op name  → len op actual
    if np >= 2 {
        if let Some(len) = parse_len(toks[np - 2]) {
            ok = ok && eval_cmp(len, toks[np - 1], actual);
        }
    }
    // 오른쪽 경계: name op len → actual op len
    if toks.len() >= np + 3 {
        if let Some(len) = parse_len(toks[np + 2]) {
            ok = ok && eval_cmp(actual, toks[np + 1], len);
        }
    }
    ok
}

fn eval_cmp(a: f32, op: &str, b: f32) -> bool {
    match op {
        ">=" => a >= b,
        "<=" => a <= b,
        ">" => a > b,
        "<" => a < b,
        "=" => (a - b).abs() < 0.5,
        _ => false,
    }
}
