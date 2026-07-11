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

// "prop: value" 원자 지원 여부 — 선언이 longhand 로 확장되면 지원으로 본다.
fn declaration_supported(atom: &str) -> bool {
    let Some(colon) = atom.find(':') else { return false };
    let prop = atom[..colon].trim();
    let value = atom[colon + 1..].trim();
    if prop.is_empty() || value.is_empty() {
        return false;
    }
    !expand_declaration(prop, value).is_empty()
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
}
