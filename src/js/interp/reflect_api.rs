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

// "min:max" (clamped range 반영의 invalid 필드 인코딩) → (min, max).
fn parse_range(s: &str) -> Option<(i64, i64)> {
    let (a, b) = s.split_once(':')?;
    Some((a.parse().ok()?, b.parse().ok()?))
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
            // nullable: 속성이 없으면 null (ARIA IDL 은 DOMString?)
            Reflect::NullableString => match raw {
                Some(s) => Value::Str(s),
                None => Value::Null,
            },
            Reflect::Url => {
                // 없거나 빈 값이면 빈 문자열(빈 값은 base 로 절대화하지 않는다, §HTML URL
                // 반영 getter), 있으면 문서 기준 URL 로 절대화.
                match raw {
                    Some(u) if !u.is_empty() => Value::Str(self.absolute_url(&u)),
                    _ => Value::Str(String::new()),
                }
            }
            Reflect::Enum => {
                // 열거형 (§2.6.2): 알려진 키워드면 그 정규형(소문자), 속성이 없으면
                // missing value default, 모르는 값이면 invalid value default.
                // 기본값이 명시되지 않았으면 빈 문자열.
                // <input>.type 이 "text" 로 나오는 게 바로 이 규칙이다 — 빈 문자열을
                // 주면 `if (input.type === 'text')` 같은 흔한 검사가 조용히 거짓이 된다.
                // nullable enum(DOMString?, 예: crossOrigin)은 missing 기본값이 없고
                // invalid 기본값이 있다 — 없으면 null. 비-nullable(dir 등)은 missing 없으면 "".
                let nullable = spec.missing.is_none() && spec.invalid.is_some();
                match raw {
                    None => match spec.missing {
                        Some(m) => Value::Str(m.to_string()),
                        None if nullable => Value::Null,
                        None => Value::Str(String::new()),
                    },
                    Some(s) => {
                        let lower = s.to_ascii_lowercase();
                        if spec.keywords.iter().any(|k| *k == lower) {
                            Value::Str(lower)
                        } else {
                            // 키워드 아님(빈 값 포함): invalid value default → missing → "".
                            Value::Str(spec.invalid.or(spec.missing).unwrap_or("").to_string())
                        }
                    }
                }
            }
            // 수치 반영: 기본값은 표의 missing(§HTML). 기본값이 음수면 "limited to only
            // non-negative numbers"(maxLength=-1) — 음수/무효는 기본값.
            Reflect::Long => {
                let default = spec.missing.and_then(parse_int).unwrap_or(0);
                let non_negative = default < 0;
                Value::Num(match raw.as_deref().and_then(parse_int) {
                    Some(v)
                        if (-2147483648..=2147483647).contains(&v)
                            && !(non_negative && v < 0) =>
                    {
                        v as f64
                    }
                    _ => default as f64,
                })
            }
            // unsigned long: invalid="min:max" 면 "clamped to the range"(colSpan [1,1000],
            // rowSpan [0,65534], span [1,1000]) — 파스 성공 시 [min,max]로 클램프,
            // 파스 실패는 기본값. 아니면 기본값 양수면 "limited to positive"(rows/cols/size),
            // 그 외 [0,2^31-1].
            Reflect::UnsignedLong => {
                let default = spec.missing.and_then(parse_int).unwrap_or(0);
                if let Some((lo, hi)) = spec.invalid.and_then(parse_range) {
                    let v = raw
                        .as_deref()
                        .and_then(parse_int)
                        .map(|n| n.clamp(lo, hi))
                        .unwrap_or(default);
                    Value::Num(v as f64)
                } else {
                    let limited_positive = default >= 1;
                    Value::Num(match raw.as_deref().and_then(parse_int) {
                        Some(v)
                            if (0..=2147483647).contains(&v)
                                && !(limited_positive && v < 1) =>
                        {
                            v as f64
                        }
                        _ => default as f64,
                    })
                }
            }
            // double: 기본값은 missing. 기본값이 양수면 "limited to numbers greater than
            // zero"(progress.max=1) — 0 이하는 기본값.
            Reflect::Double => {
                let default = spec.missing.and_then(parse_double).unwrap_or(0.0);
                let limited_positive = default > 0.0;
                Value::Num(match raw.as_deref().and_then(parse_double) {
                    Some(v) if v.is_finite() && !(limited_positive && v <= 0.0) => v,
                    _ => default,
                })
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
        let (attr, kind, missing, invalid) = {
            let dom = self.dom_arena()?;
            let crate::dom::NodeType::Element(e) = &dom.get(id).node_type else {
                return Ok(false);
            };
            let tag = e.tag_name.clone();
            let Some(spec) = lookup(&tag, key) else {
                return Ok(false);
            };
            (spec.attr, spec.kind, spec.missing, spec.invalid)
        };
        if matches!(kind, Reflect::TokenList) {
            return Ok(false);
        }
        // nullable enum(crossOrigin 등, missing 없고 invalid 있음)에 null/undefined 대입은
        // 콘텐츠 속성을 제거한다(§Web IDL DOMString? reflect).
        if matches!(kind, Reflect::Enum)
            && missing.is_none()
            && invalid.is_some()
            && matches!(value, Value::Null | Value::Undefined)
        {
            let dom = self.dom_arena()?;
            dom.remove_attr(id, attr);
            return Ok(true);
        }
        // 문자열/URL/열거·비-null NullableString 은 **먼저** ToString 강제변환한다(객체의
        // toString/valueOf 호출). dom 을 빌리기 전에 해야 &mut self 충돌이 없다.
        let str_val = match kind {
            Reflect::Bool | Reflect::Long | Reflect::UnsignedLong | Reflect::Double => None,
            Reflect::NullableString if matches!(value, Value::Null | Value::Undefined) => None,
            _ => Some(self.to_string_value(value)?),
        };
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
            // 수치 setter: unsigned long 은 WebIDL ToUint32 후 [0,2^31-1] 밖이면 기본값
            // (missing), long 은 유한하면 절단(§HTML 반영 setter).
            Reflect::Long | Reflect::UnsignedLong => {
                let raw_num = to_num(value);
                let out = if matches!(kind, Reflect::UnsignedLong) {
                    let default = missing.and_then(parse_int).unwrap_or(0);
                    if raw_num.is_finite() {
                        let u = (raw_num.trunc() as i64).rem_euclid(4294967296);
                        if u <= 2147483647 {
                            u
                        } else {
                            default
                        }
                    } else {
                        default
                    }
                } else if raw_num.is_finite() {
                    raw_num.trunc() as i64
                } else {
                    0
                };
                dom.set_attr(id, attr, format!("{}", out));
            }
            Reflect::Double => {
                let n = to_num(value);
                dom.set_attr(id, attr, crate::style::num_css(n as f32));
            }
            // nullable DOMString?: null/undefined 대입은 콘텐츠 속성을 제거(ARIA IDL).
            Reflect::NullableString => {
                if matches!(value, Value::Null | Value::Undefined) {
                    dom.remove_attr(id, attr);
                } else {
                    dom.set_attr(id, attr, str_val.unwrap());
                }
            }
            // 문자열/URL/열거: 그대로 문자열로 (URL 은 **절대화하지 않는다** — 표준은
            // 콘텐츠 속성에 준 값을 그대로 넣고, 읽을 때 절대화한다)
            _ => dom.set_attr(id, attr, str_val.unwrap()),
        }
        Ok(true)
    }
}
