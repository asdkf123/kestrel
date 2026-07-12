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
    ok = ok && prefers_ok(body);
    if negate {
        !ok
    } else {
        ok
    }
}

// prefers-color-scheme / prefers-contrast 평가. 헤드리스 렌더는 light + 표준 대비 기준.
// dark 스킴이나 more 대비를 요구하면 불일치. "not (…dark)" 형태는 일치(light).
fn prefers_ok(body: &str) -> bool {
    let mut ok = true;
    // 특성 앞에 "not (" 가 있으면 그 특성만 부정된 것으로 본다.
    let negated_at = |i: usize| {
        let pre = body[..i].trim_end();
        pre.ends_with("not (") || pre.ends_with("not(")
    };
    if let Some(i) = body.find("prefers-color-scheme") {
        let after = &body[i..];
        let neg = negated_at(i);
        if after.contains("dark") {
            ok = ok && neg; // dark 요구: 부정이면 light 라서 일치, 아니면 불일치
        } else if after.contains("light") {
            ok = ok && !neg; // light 요구: 일치
        }
    }
    if let Some(i) = body.find("prefers-contrast") {
        let after = &body[i..];
        let neg = negated_at(i);
        if after.contains("more") || after.contains("high") {
            ok = ok && neg;
        }
    }
    ok
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
