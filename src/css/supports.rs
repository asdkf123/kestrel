use super::shorthand::expand_declaration;

// @supports 조건 평가. not / and / or (괄호 밖) 와 (prop: value) 원자를 처리.
// 원자는 해당 선언이 파싱되면 지원으로 본다(관용적 — 미지원보단 포함 쪽으로).
// selector(...) 등 함수형 조건은 미지원(false)으로 간주.
pub(crate) fn supports_condition(cond: &str) -> bool {
    let c = cond.trim();
    if c.is_empty() {
        return false;
    }
    // not <cond>
    if let Some(rest) = strip_not(c) {
        return !supports_condition(rest);
    }
    // 최상위(괄호 밖) and / or
    if let Some(parts) = split_on_kw(c, "and") {
        return parts.iter().all(|p| supports_condition(p));
    }
    if let Some(parts) = split_on_kw(c, "or") {
        return parts.iter().any(|p| supports_condition(p));
    }
    // 괄호 묶음: 내부가 선언이면 검사, 아니면 하위 조건으로 재귀
    if c.starts_with('(') && c.ends_with(')') {
        let inner = c[1..c.len() - 1].trim();
        let nested = inner.starts_with('(')
            || strip_not(inner).is_some()
            || split_on_kw(inner, "and").is_some()
            || split_on_kw(inner, "or").is_some();
        if !nested && inner.contains(':') {
            return declaration_supported(inner);
        }
        return supports_condition(inner);
    }
    false // selector(...) / font-tech(...) 등 함수형 조건 미지원
}

fn strip_not(c: &str) -> Option<&str> {
    let lower = c.to_ascii_lowercase();
    if lower.starts_with("not ") || lower.starts_with("not(") {
        Some(c[3..].trim())
    } else {
        None
    }
}

// 엔진이 실제로 해석하는 longhand 프로퍼티 집합.
// @supports 는 이 집합으로만 참을 낸다 — 과다보고하면 사이트가 우리가 못 그리는
// 모던 레이아웃(container query/subgrid 등)을 내보내고 렌더가 깨진다.
// 과소보고는 안전하다(사이트가 폴백 CSS 를 준다). 새 프로퍼티를 구현하면 여기 추가.
pub(crate) const SUPPORTED: &[&str] = &[
    "align-content", "align-items", "align-self", "aspect-ratio", "backdrop-filter",
    "background-color", "background-image", "background-position", "background-repeat",
    "background-size", "border-bottom-width", "border-collapse",
    "border-color", "border-left-width", "border-radius",
    "border-right-width", "border-spacing", "border-style",
    "border-top-color", "border-top-width", "border-width", "bottom", "box-shadow",
    "box-sizing", "clear", "clip-path", "color", "column-count", "column-gap", "content",
    "direction", "display", "filter", "flex", "flex-basis", "flex-direction",
    "flex-grow", "flex-shrink", "flex-wrap", "float", "font-family", "font-size",
    "font-style", "font-weight", "gap", "grid-area", "grid-auto-rows", "grid-column",
    "grid-column-end", "grid-column-start", "grid-row", "grid-row-end", "grid-row-start",
    "grid-template-areas", "grid-template-columns", "grid-template-rows", "height",
    "justify-content", "justify-items", "justify-self", "left", "letter-spacing",
    "line-height", "list-style", "list-style-type", "margin-bottom", "margin-left",
    "margin-right", "margin-top", "max-height", "max-width", "min-height", "min-width",
    "mix-blend-mode", "object-fit", "object-position", "opacity", "order", "outline-color",
    "outline-offset", "outline-style", "outline-width", "overflow", "overflow-wrap",
    "overflow-x", "overflow-y", "padding-bottom", "padding-left", "padding-right",
    "padding-top", "position", "right", "row-gap", "text-align", "text-decoration-color",
    "text-decoration-line", "text-indent", "text-overflow", "text-transform", "top",
    "transform", "vertical-align", "visibility", "white-space", "width", "word-break",
    "word-spacing", "word-wrap", "z-index",
];

fn longhand_supported(prop: &str) -> bool {
    let p = prop.trim().to_ascii_lowercase();
    // 커스텀 프로퍼티(--x)는 var() 로 지원한다.
    if p.starts_with("--") {
        return true;
    }
    SUPPORTED.contains(&p.as_str())
}

// 엔진이 실제로 계산하는 값 함수 전부. 여기 없는 함수(color-mix/oklch/lab/env/attr/
// image-set …)는 파싱만 되고 무시되므로 지원한다고 하면 거짓말이다.
// 프로퍼티별로 나누지 않고 합집합으로 본다 — 과소보고는 안전, 과다보고만 위험하다.
const FUNCS: &[&str] = &[
    // 값 계산
    "var", "calc", "min", "max", "clamp",
    // 색
    "rgb", "rgba", "hsl", "hsla",
    // 이미지
    "url", "linear-gradient", "radial-gradient", "conic-gradient",
    // content
    "counter", "counters",
    // transform — 2D 함수 전부 (행렬로 합성해 서브트리를 실제로 변환한다)
    "translate", "translatex", "translatey", "scale", "scalex", "scaley",
    "rotate", "rotatez", "skew", "skewx", "skewy", "matrix",
    // filter / backdrop-filter
    "blur", "grayscale", "brightness", "invert", "contrast", "sepia", "saturate",
    "hue-rotate", "opacity",
    // clip-path (inset 만 그린다)
    "inset",
    // grid 트랙
    "repeat", "minmax", "fit-content",
];

// 값에 쓰인 함수 이름을 전부 뽑는다: 식별자 바로 뒤에 '(' 가 오는 형태.
fn value_functions(value: &str) -> Vec<String> {
    let b = value.as_bytes();
    let mut out = Vec::new();
    for (i, &c) in b.iter().enumerate() {
        if c != b'(' {
            continue;
        }
        let mut s = i;
        while s > 0 {
            let p = b[s - 1];
            if p.is_ascii_alphanumeric() || p == b'-' || p == b'_' {
                s -= 1;
            } else {
                break;
            }
        }
        if s < i {
            out.push(value[s..i].to_ascii_lowercase());
        }
    }
    out
}

// 열거형 프로퍼티: 엔진이 키워드를 하나씩 매칭하고 나머지는 조용히 기본값으로 떨어뜨린다.
// 그래서 값 검사 없이는 `@supports (position: sticky)` 가 참이 되고, 사이트는 스티키
// 헤더를 내보내지만 우리는 static 으로 그린다. 각 집합은 엔진 코드의 match 와 1:1 이다.
fn enum_values(prop: &str) -> Option<&'static [&'static str]> {
    Some(match prop {
        // style.rs StyledNode::display()
        "display" => &[
            "block", "inline", "inline-block", "flex", "inline-flex", "grid", "inline-grid",
            "none", "contents",
        ],
        // layout/mod.rs LayoutBox::position()
        "position" => &["static", "relative", "absolute", "fixed", "sticky"],
        "float" => &["left", "right", "none"],
        "clear" => &["left", "right", "both", "none"],
        _ => return None,
    })
}

// 하나의 longhand 선언(이름+값)이 실제로 구현돼 있는가.
fn longhand_decl_supported(prop: &str, value: &str) -> bool {
    if !longhand_supported(prop) {
        return false;
    }
    let p = prop.trim().to_ascii_lowercase();
    if p.starts_with("--") {
        return true; // 커스텀 프로퍼티의 값은 임의 토큰이다
    }
    let v = value.trim().to_ascii_lowercase();
    // 미구현 함수가 하나라도 있으면 거짓
    if value_functions(&v).iter().any(|f| !FUNCS.contains(&f.as_str())) {
        return false;
    }
    // 전역 키워드는 어디서나 유효
    if matches!(v.as_str(), "inherit" | "initial" | "unset" | "revert") {
        return true;
    }
    match enum_values(&p) {
        Some(allowed) => allowed.contains(&v.as_str()),
        None => true,
    }
}

// "prop: value" 원자 지원 여부.
// 선언을 longhand 로 확장한 뒤, 확장 결과가 전부 우리가 실제로 해석하는 프로퍼티여야
// 참이다. 예전엔 "파싱만 되면 지원" 이라 container-type/subgrid 같은 미구현 기능도
// 지원한다고 거짓말했다(과다보고 → 사이트가 못 그리는 레이아웃을 보냄).
fn declaration_supported(atom: &str) -> bool {
    let Some(colon) = atom.find(':') else { return false };
    let prop = atom[..colon].trim();
    let value = atom[colon + 1..].trim();
    if prop.is_empty() || value.is_empty() {
        return false;
    }
    // 원문에 우리가 계산하지 않는 함수가 있으면 거짓 (color-mix/oklch/env/attr…).
    // 파싱 뒤에는 원문이 남지 않는 값도 있어서 확장 전에 본다.
    if value_functions(&value.to_ascii_lowercase()).iter().any(|f| !FUNCS.contains(&f.as_str())) {
        return false;
    }
    let expanded = expand_declaration(prop, value);
    if expanded.is_empty() {
        return false; // 값이 파싱 안 됨
    }
    // 확장된 longhand 가 전부 구현돼 있고, 값도 엔진이 실제로 해석하는 값이어야 한다.
    expanded
        .iter()
        .all(|d| longhand_decl_supported(&d.name, &crate::style::computed_value_string(&d.value)))
}

// 괄호 깊이 0 에서 공백으로 둘러싸인 키워드(and/or)로 분리. 없으면 None.
fn split_on_kw(c: &str, kw: &str) -> Option<Vec<String>> {
    let lower = c.to_ascii_lowercase();
    let bytes = c.as_bytes();
    let klen = kw.len();
    let mut depth = 0i32;
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < c.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => depth -= 1,
            _ => {}
        }
        if depth == 0
            && i > 0
            && bytes[i - 1].is_ascii_whitespace()
            && i + klen < c.len()
            && bytes[i + klen].is_ascii_whitespace()
            && lower.get(i..i + klen) == Some(kw)
        {
            parts.push(c[start..i].trim().to_string());
            i += klen;
            start = i;
            continue;
        }
        i += 1;
    }
    if parts.is_empty() {
        return None;
    }
    parts.push(c[start..].trim().to_string());
    Some(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supports_at_rule_gates_rules_in_stylesheet() {
        // 지원되는 조건 → 내부 규칙 포함
        let ss = crate::css::parse(
            "@supports (display: grid) { .a { color: #ff0000; } }".to_string(),
        );
        assert_eq!(ss.rules.len(), 1, "지원 조건이면 규칙 포함");
        // 지원 안 되는 조건 → 내부 규칙 제외
        let ss2 = crate::css::parse(
            "@supports not (display: grid) { .a { color: #ff0000; } }".to_string(),
        );
        assert_eq!(ss2.rules.len(), 0, "미지원 조건이면 규칙 제외");
    }

    #[test]
    fn supports_basic_feature_queries() {
        assert!(supports_condition("(display: grid)"));
        assert!(supports_condition("(display: flex)"));
        // 지원하는 두 조건의 and
        assert!(supports_condition("(display: grid) and (gap: 1rem)"));
        // not 지원되는 것 → false
        assert!(!supports_condition("not (display: grid)"));
        // or: 하나라도 지원되면 true
        assert!(supports_condition("(display: -webkit-box) or (display: flex)"));
    }

    #[test]
    fn supports_does_not_overreport_unimplemented_features() {
        // 예전엔 "선언이 파싱되면 지원" 이라 미구현 기능도 참이었다(거짓말).
        // 사이트가 우리가 못 그리는 모던 레이아웃을 내보내 렌더가 깨진다.
        assert!(!supports_condition("(container-type: inline-size)"));
        // 엔진이 해석하지 않는 프로퍼티 → 거짓
        assert!(!supports_condition("(border-left-color: red)"));
        assert!(!supports_condition("(unknown-prop: 1px)"));
        // and 중 하나라도 미지원이면 거짓
        assert!(!supports_condition("(display: grid) and (container-type: inline-size)"));
        // or 는 지원되는 쪽이 있으면 참
        assert!(supports_condition("(container-type: inline-size) or (display: grid)"));
        // 커스텀 프로퍼티는 지원(var())
        assert!(supports_condition("(--x: 1px)"));
        // not 은 뒤집는다
        assert!(supports_condition("not (container-type: inline-size)"));
    }

    #[test]
    fn type_selectors_are_case_insensitive() {
        // HTML 의 타입 선택자는 ASCII 대소문자 구분이 없다(선택자 표준 §6.1).
        // 예전엔 `DIV { … }` 이 조용히 아무것도 매칭하지 않았다.
        let ss = crate::css::parse("DIV SPAN { color: #ff0000; }".to_string());
        match &ss.rules[0].selectors[0] {
            crate::css::Selector::Complex(parts) => {
                assert_eq!(parts[0].1.tag_name.as_deref(), Some("div"), "소문자로 정규화");
                assert_eq!(parts[1].1.tag_name.as_deref(), Some("span"));
            }
            other => panic!("복합 선택자를 기대: {:?}", other),
        }
    }

    #[test]
    fn supports_checks_values_not_just_property_names() {
        // 프로퍼티 이름만 보면 열거형의 미구현 값이 전부 지원으로 보고된다.
        // sticky 는 이제 실제로 구현했으므로 참이다 (구현하기 전엔 거짓이었다 —
        // 못 그리는 걸 지원한다고 하면 사이트가 폴백을 줄 기회를 스스로 없앤다).
        assert!(supports_condition("(position: sticky)"));
        assert!(supports_condition("(position: absolute)"));
        assert!(supports_condition("(position: fixed)"));
        assert!(!supports_condition("(position: running)"), "미구현 값은 거짓");

        // display: 우리가 실제로 레이아웃하는 값만 참
        assert!(supports_condition("(display: contents)"));
        assert!(supports_condition("(display: grid)"));
        assert!(!supports_condition("(display: table-cell)"));
        assert!(!supports_condition("(display: flow-root)"));
        assert!(!supports_condition("(display: list-item)"));

        // 미구현 값 함수 → 거짓 (파싱은 되지만 계산하지 않는다)
        assert!(!supports_condition("(color: color-mix(in srgb, red, blue))"));
        assert!(!supports_condition("(color: oklch(0.7 0.1 200))"));
        assert!(!supports_condition("(width: env(safe-area-inset-left))"));
        // transform: 2D 함수는 전부 행렬로 합성해 실제로 변환한다
        assert!(supports_condition("(transform: rotate(45deg))"));
        assert!(supports_condition("(transform: translateX(10px))"));
        assert!(supports_condition("(transform: matrix(1, 0, 0, 1, 5, 5))"));
        // 3D 는 아직 미구현 → 거짓 (지원한다고 하면 사이트가 2D 폴백을 줄 기회를 잃는다)
        assert!(!supports_condition("(transform: rotate3d(0, 1, 0, 45deg))"));
        assert!(!supports_condition("(transform: perspective(500px))"));

        // 구현된 함수는 참
        assert!(supports_condition("(width: calc(100% - 10px))"));
        assert!(supports_condition("(width: min(100%, 40rem))"));
        assert!(supports_condition("(color: rgb(1 2 3))"));

        // 전역 키워드는 어디서나 유효
        assert!(supports_condition("(display: inherit)"));
    }
}
