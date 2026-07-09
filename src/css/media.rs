// 미디어 쿼리 매칭 (콤마 = OR). min-width/max-width(px) 와 print 타입만 평가.
// 그 외 특성(orientation, resolution 등)은 제약 없음으로 간주(관용적으로 매칭).
pub(crate) fn media_matches(query: &str, vw: f32) -> bool {
    let q = query.trim();
    if q.is_empty() {
        return true; // '@media { }' (타입 생략) → 매칭
    }
    q.split(',').any(|one| media_query_matches(one.trim(), vw))
}

fn media_query_matches(q: &str, vw: f32) -> bool {
    let ql = q.to_ascii_lowercase();
    let negate = ql.starts_with("not ");
    let body = ql.trim_start_matches("not ").trim();
    // print 전용은 화면에서 불일치
    if body.starts_with("print") {
        return negate;
    }
    let mut ok = true;
    if let Some(px) = media_feature_px(body, "min-width") {
        ok = ok && vw >= px;
    }
    if let Some(px) = media_feature_px(body, "max-width") {
        ok = ok && vw <= px;
    }
    if negate {
        !ok
    } else {
        ok
    }
}

// "(min-width: 768px)" 같은 특성에서 px 값을 추출. em 등 비-px 는 None.
fn media_feature_px(q: &str, feature: &str) -> Option<f32> {
    let idx = q.find(feature)?;
    let after = &q[idx + feature.len()..];
    let colon = after.find(':')?;
    let val = after[colon + 1..].trim_start();
    let num: String = val.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    if num.is_empty() {
        return None;
    }
    // px 단위이거나 단위 없을 때만 (em/rem/vw 등은 무시)
    let rest = val[num.len()..].trim_start();
    if rest.starts_with("px") || rest.starts_with(')') || rest.is_empty() {
        num.parse::<f32>().ok()
    } else {
        None
    }
}
