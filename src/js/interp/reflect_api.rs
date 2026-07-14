// IDL 속성 반영 (HTML §2.6 "Reflecting content attributes in IDL attributes").
// 표(src/reflect.rs)는 표준 데이터에서 기계 추출한 357개다.
// 예전엔 id/className 등 몇 개만 손으로 처리하고 나머지는 **조용히 무시**했다:
// a.title 을 읽으면 undefined, img.width = 100 은 아무 일도 안 했다.
use super::*;
use crate::reflect::{Reflect, ReflectSpec, REFLECT};

// 이 태그의 이 IDL 이름이 반영 속성인가.
// 태그별 항목이 전역 항목보다 우선한다 (예: <a>.type 은 전역 속성이 아니다).
pub(super) fn lookup(tag: &str, idl: &str) -> Option<&'static ReflectSpec> {
    REFLECT
        .iter()
        .find(|s| s.tag == tag && s.idl == idl)
        .or_else(|| REFLECT.iter().find(|s| s.tag.is_empty() && s.idl == idl))
}

// HTML 표준의 정수 파싱 (§2.4.4.1 "Rules for parsing integers"):
// 선행 공백 허용, 부호 허용, 그 뒤 숫자. 실패하면 None.
fn parse_int(s: &str) -> Option<i64> {
    let t = s.trim_start_matches([' ', '\t', '\n', '\x0C', '\r']);
    let (neg, t) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let n = t.bytes().take_while(|b| b.is_ascii_digit()).count();
    if n == 0 {
        return None;
    }
    let v: i64 = t[..n].parse().ok()?;
    Some(if neg { -v } else { v })
}

// 부동소수 파싱 (§2.4.4.3)
fn parse_double(s: &str) -> Option<f64> {
    let t = s.trim_start_matches([' ', '\t', '\n', '\x0C', '\r']);
    let n = t
        .bytes()
        .take_while(|b| b.is_ascii_digit() || matches!(b, b'-' | b'+' | b'.' | b'e' | b'E'))
        .count();
    t[..n].parse().ok().filter(|v: &f64| v.is_finite())
}

impl Interp {
    // 반영 속성 읽기. 표에 없으면 None (호출부가 기존 처리로 넘어간다).
    pub(super) fn reflect_get(
        &mut self,
        id: crate::dom::NodeId,
        key: &str,
    ) -> Result<Option<Value>, String> {
        let dom = self.dom_arena()?;
        let crate::dom::NodeType::Element(e) = &dom.get(id).node_type else {
            return Ok(None);
        };
        let tag = e.tag_name.clone();
        let Some(spec) = lookup(&tag, key) else {
            return Ok(None);
        };
        let raw = {
            let dom = self.dom_arena()?;
            match &dom.get(id).node_type {
                crate::dom::NodeType::Element(e) => e.attributes.get(spec.attr).cloned(),
                _ => None,
            }
        };
        let v = match spec.kind {
            Reflect::Bool => Value::Bool(raw.is_some()),
            Reflect::String => Value::Str(raw.unwrap_or_default()),
            Reflect::Url => {
                // 없으면 빈 문자열, 있으면 문서 기준 URL 로 절대화 (표준)
                match raw {
                    None => Value::Str(String::new()),
                    Some(u) => Value::Str(self.absolute_url(&u)),
                }
            }
            Reflect::Enum => {
                // 열거형 (§2.6.2): 알려진 키워드면 그 정규형(소문자), 속성이 없으면
                // missing value default, 모르는 값이면 invalid value default.
                // 기본값이 명시되지 않았으면 빈 문자열.
                // <input>.type 이 "text" 로 나오는 게 바로 이 규칙이다 — 빈 문자열을
                // 주면 `if (input.type === 'text')` 같은 흔한 검사가 조용히 거짓이 된다.
                match raw {
                    None => Value::Str(spec.missing.unwrap_or("").to_string()),
                    Some(s) => {
                        let lower = s.to_ascii_lowercase();
                        if spec.keywords.iter().any(|k| *k == lower) {
                            Value::Str(lower)
                        } else if s.is_empty() {
                            // 빈 값도 "없음" 취급 (표준: missing value default)
                            Value::Str(spec.missing.unwrap_or("").to_string())
                        } else {
                            Value::Str(
                                spec.invalid.or(spec.missing).unwrap_or("").to_string(),
                            )
                        }
                    }
                }
            }
            Reflect::Long => Value::Num(match raw.as_deref().and_then(parse_int) {
                Some(v) if (-2147483648..=2147483647).contains(&v) => v as f64,
                _ => 0.0,
            }),
            Reflect::UnsignedLong => Value::Num(match raw.as_deref().and_then(parse_int) {
                Some(v) if (0..=2147483647).contains(&v) => v as f64,
                _ => 0.0,
            }),
            Reflect::Double => {
                Value::Num(raw.as_deref().and_then(parse_double).unwrap_or(0.0))
            }
            // classList/relList 등은 전용 뷰가 이미 있다 — 여기서 다루지 않는다
            Reflect::TokenList => return Ok(None),
        };
        Ok(Some(v))
    }

    // 반영 속성 쓰기. 처리했으면 true.
    pub(super) fn reflect_set(
        &mut self,
        id: crate::dom::NodeId,
        key: &str,
        value: &Value,
    ) -> Result<bool, String> {
        let dom = self.dom_arena()?;
        let crate::dom::NodeType::Element(e) = &dom.get(id).node_type else {
            return Ok(false);
        };
        let tag = e.tag_name.clone();
        let Some(spec) = lookup(&tag, key) else {
            return Ok(false);
        };
        let (attr, kind) = (spec.attr, spec.kind);
        if matches!(kind, Reflect::TokenList) {
            return Ok(false);
        }
        let dom = self.dom_arena()?;
        match kind {
            // 불리언: true → 빈 값으로 속성 추가, false → 제거 (표준)
            Reflect::Bool => {
                if to_bool(value) {
                    dom.set_attr(id, attr, String::new());
                } else {
                    dom.remove_attr(id, attr);
                }
            }
            // 수치: 표준의 직렬화 (정수는 정수로)
            Reflect::Long | Reflect::UnsignedLong => {
                let n = to_num(value);
                let n = if n.is_finite() { n.trunc() } else { 0.0 };
                dom.set_attr(id, attr, format!("{}", n as i64));
            }
            Reflect::Double => {
                let n = to_num(value);
                dom.set_attr(id, attr, crate::style::num_css(n as f32));
            }
            // 문자열/URL/열거: 그대로 문자열로 (URL 은 **절대화하지 않는다** — 표준은
            // 콘텐츠 속성에 준 값을 그대로 넣고, 읽을 때 절대화한다)
            _ => dom.set_attr(id, attr, to_display(value)),
        }
        Ok(true)
    }
}
