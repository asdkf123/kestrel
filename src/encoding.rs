// 문자 인코딩 감지와 디코딩.
//
// 예전엔 응답 바이트를 무조건 UTF-8 로 읽었다(String::from_utf8_lossy). EUC-KR 같은
// 레거시 인코딩 페이지는 조용히 깨진 글자로 렌더됐다 — "렌더는 됐는데 내용이 쓰레기"라
// 실패했다는 것조차 알기 어려웠다. 감지 → 디코딩을 표준 절차대로 한다.
//
// 감지 순서 (HTML Standard §13.2.3.1 요약):
//   1. BOM
//   2. HTTP Content-Type 의 charset 파라미터
//   3. 문서 앞부분의 <meta charset> / <meta http-equiv=Content-Type>
//   4. 기본값 UTF-8
//
// 디코딩: UTF-8, CP949(EUC-KR 확장), windows-1252/ISO-8859-1.
// 그 외 레거시 CJK(Shift_JIS/GBK/Big5)는 아직 테이블이 없다 — 감지되면 그대로 보고한다.

mod cp949 {
    include!("encoding_cp949.rs");
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Charset {
    Utf8,
    Cp949, // euc-kr / ks_c_5601 / cp949 — 한국 웹의 레거시 표준
    Windows1252,
    /// 감지는 됐지만 아직 디코더가 없는 인코딩 (이름 보존).
    Unsupported(&'static str),
}

// charset 이름 → Charset. 별칭 포함.
fn from_label(label: &str) -> Option<Charset> {
    let l = label.trim().trim_matches(|c| c == '"' || c == '\'').to_ascii_lowercase();
    Some(match l.as_str() {
        "utf-8" | "utf8" | "unicode-1-1-utf-8" => Charset::Utf8,
        "euc-kr" | "euckr" | "ks_c_5601-1987" | "ks_c_5601-1989" | "ksc5601" | "korean"
        | "cp949" | "windows-949" | "uhc" => Charset::Cp949,
        "windows-1252" | "cp1252" | "iso-8859-1" | "iso8859-1" | "latin1" | "ascii"
        | "us-ascii" => Charset::Windows1252,
        "shift_jis" | "sjis" | "shift-jis" | "windows-31j" => Charset::Unsupported("shift_jis"),
        "gbk" | "gb2312" | "gb18030" => Charset::Unsupported("gbk"),
        "big5" | "big5-hkscs" => Charset::Unsupported("big5"),
        _ => return None,
    })
}

// "text/html; charset=euc-kr" 같은 값에서 charset 파라미터를 뽑는다.
pub fn charset_from_content_type(ct: &str) -> Option<Charset> {
    let lower = ct.to_ascii_lowercase();
    let idx = lower.find("charset")?;
    let rest = &ct[idx + "charset".len()..];
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
    let end = rest.find(|c: char| c == ';' || c.is_whitespace()).unwrap_or(rest.len());
    from_label(&rest[..end])
}

// 문서 앞부분(최대 1024바이트)에서 <meta charset> / <meta http-equiv> 를 찾는다.
// 아직 디코딩 전이므로 ASCII 범위만 훑는다(표준도 동일한 전제).
fn charset_from_meta(bytes: &[u8]) -> Option<Charset> {
    let head = &bytes[..bytes.len().min(1024)];
    let text: String = head.iter().map(|&b| b as char).collect();
    let lower = text.to_ascii_lowercase();
    let mut pos = 0usize;
    while let Some(m) = lower[pos..].find("<meta") {
        let start = pos + m;
        let end = lower[start..].find('>').map(|e| start + e).unwrap_or(lower.len());
        let tag = &text[start..end];
        let tag_lower = &lower[start..end];
        // <meta charset="euc-kr">
        if let Some(ci) = tag_lower.find("charset") {
            let rest = tag[ci + "charset".len()..].trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                let rest = rest.trim_start();
                let e = rest
                    .find(|c: char| c == '"' && false)
                    .unwrap_or_else(|| {
                        rest.find(|c: char| c == ';' || c == '/' || c.is_whitespace())
                            .unwrap_or(rest.len())
                    });
                if let Some(cs) = from_label(&rest[..e]) {
                    return Some(cs);
                }
            }
        }
        pos = end.max(start + 1);
    }
    None
}

// BOM 검사. (바이트 오프셋, Charset)
fn charset_from_bom(bytes: &[u8]) -> Option<(usize, Charset)> {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return Some((3, Charset::Utf8));
    }
    None
}

// 응답 바이트 + Content-Type 헤더 → (디코딩된 문자열, 사용된 인코딩)
pub fn decode(bytes: &[u8], content_type: Option<&str>) -> (String, Charset) {
    let (skip, bom_cs) = charset_from_bom(bytes).map_or((0, None), |(s, c)| (s, Some(c)));
    let body = &bytes[skip..];
    let cs = bom_cs
        .or_else(|| content_type.and_then(charset_from_content_type))
        .or_else(|| charset_from_meta(body))
        .unwrap_or(Charset::Utf8);
    (decode_as(body, cs), cs)
}

pub fn decode_as(bytes: &[u8], cs: Charset) -> String {
    match cs {
        Charset::Utf8 | Charset::Unsupported(_) => String::from_utf8_lossy(bytes).into_owned(),
        Charset::Windows1252 => bytes.iter().map(|&b| w1252_char(b)).collect(),
        Charset::Cp949 => decode_cp949(bytes),
    }
}

// windows-1252: 0x80..0x9F 만 ISO-8859-1 과 다르다(특수 문자 구간).
fn w1252_char(b: u8) -> char {
    const HIGH: [u16; 32] = [
        0x20AC, 0x0081, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160,
        0x2039, 0x0152, 0x008D, 0x017D, 0x008F, 0x0090, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022,
        0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x009D, 0x017E, 0x0178,
    ];
    if (0x80..=0x9F).contains(&b) {
        char::from_u32(HIGH[(b - 0x80) as usize] as u32).unwrap_or('\u{FFFD}')
    } else {
        b as char // ISO-8859-1 은 코드포인트 = 바이트값
    }
}

// CP949(EUC-KR 확장): ASCII 는 1바이트, 0x81..0xFE 는 2바이트.
fn decode_cp949(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b < 0x80 {
            out.push(b as char);
            i += 1;
            continue;
        }
        if b >= cp949::CP949_LEAD_LO && i + 1 < bytes.len() {
            let trail = bytes[i + 1];
            if let Some(ti) = cp949_trail_index(trail) {
                let idx = (b - cp949::CP949_LEAD_LO) as usize * cp949::CP949_COLS + ti;
                let u = cp949::CP949.get(idx).copied().unwrap_or(0);
                if u != 0 {
                    if let Some(c) = char::from_u32(u as u32) {
                        out.push(c);
                        i += 2;
                        continue;
                    }
                }
            }
        }
        out.push('\u{FFFD}'); // 매핑 없음 — 표준대로 대체 문자
        i += 1;
    }
    out
}

fn cp949_trail_index(t: u8) -> Option<usize> {
    match t {
        0x41..=0x5A => Some((t - 0x41) as usize),
        0x61..=0x7A => Some((t - 0x61) as usize + 26),
        0x81..=0xFE => Some((t - 0x81) as usize + 52),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_charset_from_content_type() {
        assert_eq!(
            charset_from_content_type("text/html; charset=euc-kr"),
            Some(Charset::Cp949)
        );
        assert_eq!(
            charset_from_content_type("text/html;charset=UTF-8"),
            Some(Charset::Utf8)
        );
        assert_eq!(charset_from_content_type("text/html"), None);
    }

    #[test]
    fn decodes_euc_kr_page() {
        // "한글 테스트" 를 CP949 로 인코딩한 바이트
        let body = b"<meta charset=\"euc-kr\"><h1>\xC7\xD1\xB1\xDB \xC5\xD7\xBD\xBA\xC6\xAE</h1>";
        let (text, cs) = decode(body, None);
        assert_eq!(cs, Charset::Cp949, "meta 에서 euc-kr 감지");
        assert!(text.contains("한글 테스트"), "실제로 디코딩됐다: {}", text);
    }

    #[test]
    fn http_header_beats_meta() {
        // 헤더가 UTF-8 이면 meta 의 euc-kr 보다 우선 (표준 우선순위)
        let body = "<meta charset=\"euc-kr\"><p>ok</p>".as_bytes();
        let (_, cs) = decode(body, Some("text/html; charset=utf-8"));
        assert_eq!(cs, Charset::Utf8);
    }

    #[test]
    fn decodes_windows_1252() {
        // "Café" in windows-1252: C=0x43 a=0x61 f=0x66 é=0xE9
        let body = b"<meta charset=\"windows-1252\">Caf\xE9";
        let (text, cs) = decode(body, None);
        assert_eq!(cs, Charset::Windows1252);
        assert!(text.contains("Café"), "{}", text);
    }

    #[test]
    fn utf8_is_default_and_unchanged() {
        let body = "한글 UTF-8".as_bytes();
        let (text, cs) = decode(body, None);
        assert_eq!(cs, Charset::Utf8);
        assert_eq!(text, "한글 UTF-8");
    }

    #[test]
    fn bom_wins() {
        let mut body = vec![0xEF, 0xBB, 0xBF];
        body.extend_from_slice("한글".as_bytes());
        let (text, cs) = decode(&body, Some("text/html; charset=euc-kr"));
        assert_eq!(cs, Charset::Utf8, "BOM 이 헤더보다 우선");
        assert_eq!(text, "한글");
    }
}
