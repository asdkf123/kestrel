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
const SUPPORTED: &[&str] = &[
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
    let expanded = expand_declaration(prop, value);
    if expanded.is_empty() {
        return false; // 값이 파싱 안 됨
    }
    // 확장된 longhand 가 전부 구현돼 있어야 한다.
    expanded.iter().all(|d| longhand_supported(&d.name))
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
}
