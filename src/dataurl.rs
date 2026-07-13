// data: URL 디코딩 (RFC 2397).
//
// 아이콘·플레이스홀더 이미지는 data: URI 로 인라인되는 일이 아주 흔하다.
// 예전엔 이걸 그냥 http::fetch 로 넘겨서 스킴 오류로 실패했다 — 이미지가 조용히
// 사라진다(박스는 잡히는데 안 그려진다).
//
// 형식: data:[<mediatype>][;base64],<data>
// base64 가 아니면 퍼센트 인코딩된 바이트로 본다.

pub fn is_data_url(s: &str) -> bool {
    s.trim_start().starts_with("data:")
}

// data: URL → 바이트. 형식이 깨졌으면 None.
pub fn decode(url: &str) -> Option<Vec<u8>> {
    let rest = url.trim_start().strip_prefix("data:")?;
    let (meta, payload) = rest.split_once(',')?;
    if meta.split(';').any(|t| t.trim().eq_ignore_ascii_case("base64")) {
        base64_decode(payload)
    } else {
        Some(percent_decode(payload))
    }
}

fn b64_val(c: u8) -> Option<u32> {
    Some(match c {
        b'A'..=b'Z' => (c - b'A') as u32,
        b'a'..=b'z' => (c - b'a') as u32 + 26,
        b'0'..=b'9' => (c - b'0') as u32 + 52,
        b'+' | b'-' => 62, // '-'/'_' 는 URL-safe 변형
        b'/' | b'_' => 63,
        _ => return None,
    })
}

pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for &c in s.as_bytes() {
        if c == b'=' {
            break;
        }
        if c.is_ascii_whitespace() {
            continue; // 줄바꿈 포함 허용
        }
        let v = b64_val(c)?;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = |c: u8| -> Option<u8> {
                Some(match c {
                    b'0'..=b'9' => c - b'0',
                    b'a'..=b'f' => c - b'a' + 10,
                    b'A'..=b'F' => c - b'A' + 10,
                    _ => return None,
                })
            };
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_base64_data_url() {
        // "Hi" 를 base64 로
        let u = "data:text/plain;base64,SGk=";
        assert_eq!(decode(u).unwrap(), b"Hi");
    }

    #[test]
    fn decodes_percent_encoded_data_url() {
        let u = "data:image/svg+xml,%3Csvg%3E";
        assert_eq!(decode(u).unwrap(), b"<svg>");
    }

    #[test]
    fn decodes_png_signature() {
        // 1x1 투명 PNG (실제 사이트가 플레이스홀더로 쓰는 그것)
        let u = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
        let bytes = decode(u).unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "PNG 시그니처");
    }

    #[test]
    fn rejects_non_data_url() {
        assert!(decode("https://x/y.png").is_none());
        assert!(!is_data_url("https://x/y.png"));
        assert!(is_data_url("data:image/png;base64,AAA"));
    }
}
