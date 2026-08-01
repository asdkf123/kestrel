// 미디어 쿼리 매칭 (콤마 = OR, "and" = AND). 헤드리스 데스크톱 기준:
// 뷰포트 vw×800, light 스킴, hover/fine 포인터, 표준 대비/모션, 1x, sRGB.
// 표준: 미지원/미인식 특성은 매칭 실패로 본다(관용적 true 아님).
const VH_DEFAULT: f32 = 800.0;
const ROOT_FS: f32 = 16.0; // @media 의 em/rem 은 초기 폰트크기 기준

// @container 조건 평가. 컨테이너의 인라인/블록 크기를 뷰포트처럼 넣고 미디어 특성
// 평가기를 그대로 쓴다 (min-width/max-width/width 범위 문법이 동일하다).
// inline-size/block-size 는 width/height 로 정규화한다.
pub(crate) fn container_matches(cond: &str, cw: f32, ch: f32) -> bool {
    let c = cond.trim();
    if c.is_empty() {
        return true; // 조건 없는 @container (이름만) → 컨테이너가 있으면 매칭
    }
    let normalized = c.replace("inline-size", "width").replace("block-size", "height");
    media_matches_vp(&normalized, cw, ch)
}

pub(crate) fn media_matches(query: &str, vw: f32) -> bool {
    media_matches_vp(query, vw, VH_DEFAULT)
}

// 뷰포트 높이까지 지정하는 변형. window.matchMedia 가 실제 뷰포트로 평가할 때 쓴다
// (CSS 의 @media 와 JS 의 matchMedia 가 같은 답을 내야 한다 — 예전엔 JS 쪽이 늘 false).
pub(crate) fn media_matches_vp(query: &str, vw: f32, vh: f32) -> bool {
    let q = query.trim();
    if q.is_empty() {
        return true; // '@media { }' (조건 생략) → 매칭
    }
    q.split(',').any(|one| one_query_matches(one.trim(), vw, vh))
}


fn one_query_matches(q: &str, vw: f32, vh: f32) -> bool {
    let ql = q.to_ascii_lowercase();
    let ql = ql.trim();
    if ql.is_empty() {
        return false; // 빈 쿼리(콤마 사이)는 "not all" — 불일치.
    }
    let (negate, body) = match ql.strip_prefix("not ") {
        Some(rest) => (true, rest.trim().to_string()),
        None => (false, ql.trim_start_matches("only ").trim().to_string()),
    };
    let mut ok = true;
    for cond in body.split(" and ") {
        let cond = cond.trim();
        if cond.is_empty() {
            continue;
        }
        // 조건 평가는 Option: None = 무효(미지 특성/무효 값) → 쿼리 전체가 "not all"
        // 이 되어 부정(not)으로도 참이 될 수 없다(§Media Queries).
        let pass: Option<bool> = if cond.starts_with('(') {
            let inner = cond.strip_prefix('(').and_then(|s| s.strip_suffix(')')).unwrap_or(cond);
            feature_matches(inner.trim(), vw, vh)
        } else {
            // 미디어 타입: screen/all 매칭, 그 외(print 등·미지 타입)는 유효하나 불일치.
            Some(matches!(cond, "screen" | "all"))
        };
        match pass {
            None => return false, // 무효 → "not all", 부정 무시.
            Some(false) => {
                ok = false;
                break;
            }
            Some(true) => {}
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

// 괄호 안 특성 하나 평가. None = 무효(미지 특성/무효 값/이산형에 min-max) →
// 쿼리 "not all". Some(b) = 유효, 헤드리스 기본과 비교한 참/거짓.
fn feature_matches(feat: &str, vw: f32, vh: f32) -> Option<bool> {
    // 중첩 부정 (not (feature)) — Level 4. 무효의 부정은 여전히 무효(None).
    if let Some(rest) = feat.strip_prefix("not ") {
        let inner = rest.trim().strip_prefix('(').and_then(|s| s.strip_suffix(')')).unwrap_or(rest);
        return feature_matches(inner.trim(), vw, vh).map(|b| !b);
    }
    // Level 4 범위형: "width >= 768px", "400px <= width <= 700px"
    if feat.contains('<') || feat.contains('>') {
        return range_feature_matches(feat, vw, vh);
    }
    let (name, value) = match feat.split_once(':') {
        Some((n, v)) => (n.trim(), Some(v.trim())),
        None => (feat.trim(), None),
    };
    if name.is_empty() {
        return None;
    }
    let (bound, base) = if let Some(b) = name.strip_prefix("min-") {
        (Bound::Min, b)
    } else if let Some(b) = name.strip_prefix("max-") {
        (Bound::Max, b)
    } else {
        (Bound::Exact, name)
    };
    // min-/max- 는 범위형 특성만 허용. 이산형에 붙으면 무효.
    let is_range = matches!(
        base,
        "width" | "height" | "device-width" | "device-height" | "inline-size" | "block-size"
            | "resolution" | "color" | "color-index" | "monochrome"
            | "aspect-ratio" | "device-aspect-ratio"
    );
    if !matches!(bound, Bound::Exact) && !is_range {
        return None;
    }
    // 이산형(키워드 집합) 특성: (유효 값들, 헤드리스에서 매칭되는 값).
    let discrete: Option<(&[&str], &str)> = match base {
        "orientation" => {
            Some((&["portrait", "landscape"], if vw < vh { "portrait" } else { "landscape" }))
        }
        "scan" => Some((&["interlace", "progressive"], "progressive")),
        "hover" | "any-hover" => Some((&["none", "hover"], "hover")),
        "pointer" | "any-pointer" => Some((&["none", "coarse", "fine"], "fine")),
        "prefers-color-scheme" => Some((&["light", "dark"], "light")),
        "prefers-contrast" => {
            Some((&["no-preference", "more", "less", "custom"], "no-preference"))
        }
        "prefers-reduced-motion" => Some((&["no-preference", "reduce"], "no-preference")),
        "prefers-reduced-data" => Some((&["no-preference", "reduce"], "no-preference")),
        "prefers-reduced-transparency" => Some((&["no-preference", "reduce"], "no-preference")),
        "forced-colors" => Some((&["none", "active"], "none")),
        "dynamic-range" => Some((&["standard", "high"], "standard")),
        "video-dynamic-range" => Some((&["standard", "high"], "standard")),
        "color-gamut" => Some((&["srgb", "p3", "rec2020"], "srgb")),
        "inverted-colors" => Some((&["none", "inverted"], "none")),
        "display-mode" => Some((
            &[
                "fullscreen",
                "standalone",
                "minimal-ui",
                "browser",
                "window-controls-overlay",
                "picture-in-picture",
            ],
            "browser",
        )),
        "update" => Some((&["none", "slow", "fast"], "fast")),
        "scripting" => Some((&["none", "initial-only", "enabled"], "enabled")),
        "overflow-block" => Some((&["none", "scroll", "paged", "optional-paged"], "scroll")),
        "overflow-inline" => Some((&["none", "scroll"], "scroll")),
        _ => None,
    };
    if let Some((valid, want)) = discrete {
        return match value {
            // 부울 컨텍스트: 특성의 사용값이 "false 등가"(none/no-preference/standard)면
            // 거짓, 아니면 참(§Media Queries boolean context).
            None => Some(!matches!(want, "none" | "no-preference" | "standard")),
            Some(v) => {
                if !valid.contains(&v) {
                    None // 무효 값 → 쿼리 무효.
                } else {
                    Some(v == want)
                }
            }
        };
    }
    // 범위형/수치형.
    match base {
        "width" | "device-width" | "inline-size" => match value {
            None => Some(vw > 0.0),
            Some(v) => parse_len(v).map(|len| cmp_num(bound, vw, len)),
        },
        "height" | "device-height" | "block-size" => match value {
            None => Some(vh > 0.0),
            Some(v) => parse_len(v).map(|len| cmp_num(bound, vh, len)),
        },
        "resolution" => match value {
            None => Some(true),
            Some(v) => cmp_resolution_opt(bound, v),
        },
        "grid" => match value {
            None => Some(false), // 비격자 화면 → 부울 false
            Some("0") => Some(true),
            Some("1") => Some(false),
            Some(_) => None,
        },
        // 컬러 화면: 8bit/채널 가정. color-index 는 팔레트 없음 → 0.
        "color" => match value {
            None => Some(true),
            Some(v) => v.parse::<i64>().ok().filter(|n| *n >= 0).map(|n| cmp_num(bound, 8.0, n as f32)),
        },
        "color-index" => match value {
            None => Some(false),
            Some(v) => v.parse::<i64>().ok().filter(|n| *n >= 0).map(|n| cmp_num(bound, 0.0, n as f32)),
        },
        "monochrome" => match value {
            None => Some(false),
            Some(v) => v.parse::<i64>().ok().filter(|n| *n >= 0).map(|n| cmp_num(bound, 0.0, n as f32)),
        },
        "aspect-ratio" | "device-aspect-ratio" => match value {
            None => Some(true),
            Some(v) => parse_ratio(v).map(|r| match bound {
                Bound::Min => vw / vh >= r,
                Bound::Max => vw / vh <= r,
                Bound::Exact => (vw / vh - r).abs() < 0.001,
            }),
        },
        _ => None, // 미인식 특성 → 무효.
    }
}

// 수치 비교: actual op requested. Min → actual>=req, Max → actual<=req, Exact → 근사동일.
fn cmp_num(bound: Bound, actual: f32, requested: f32) -> bool {
    match bound {
        Bound::Min => actual >= requested,
        Bound::Max => actual <= requested,
        Bound::Exact => (actual - requested).abs() < 0.5,
    }
}

// resolution 비교(값 무효면 None). 헤드리스 = 1dppx(96dpi).
fn cmp_resolution_opt(bound: Bound, v: &str) -> Option<bool> {
    let num: String = v.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    let n = num.parse::<f32>().ok()?;
    let dppx = match v[num.len()..].trim() {
        "dppx" | "x" => n,
        "dpi" => n / 96.0,
        "dpcm" => n / 37.795,
        _ => return None,
    };
    Some(match bound {
        Bound::Min => 1.0 >= dppx,
        Bound::Max => 1.0 <= dppx,
        Bound::Exact => (1.0 - dppx).abs() < 0.01,
    })
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

// "width >= 768px", "400px <= width <= 700px" 등 범위 문법. width/height 아니면
// 무효(None).
fn range_feature_matches(feat: &str, vw: f32, vh: f32) -> Option<bool> {
    let toks: Vec<&str> = feat.split_whitespace().collect();
    let np = toks
        .iter()
        .position(|t| matches!(*t, "width" | "height" | "device-width" | "device-height"))?;
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
    Some(ok)
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
