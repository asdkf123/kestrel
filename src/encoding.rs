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

use crate::encoding_cjk as cjk;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Charset {
    Utf8,
    Cp949, // euc-kr / ks_c_5601 / cp949 — 한국 웹의 레거시 표준
    Windows1252,
    ShiftJis, // 일본 레거시 웹
    Gbk,      // 중국 레거시 웹 (gbk / gb2312 / gb18030)
    Big5,     // 대만/홍콩 레거시 웹
    /// WHATWG 단일바이트 인코딩 (iso-8859-*, windows-125*, koi8-*, macintosh …).
    /// 표는 encoding.spec.whatwg.org 인덱스에서 기계 추출 (src/encoding_sbcs.rs).
    SingleByte(&'static [u16; 128]),
    /// 라벨은 읽었지만 우리가 못 읽는 인코딩. **조용히 UTF-8 로 읽지 않는다** —
    /// 그러면 글자가 전부 깨진 채로 "성공"한 것처럼 보인다.
    Unsupported,
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
        "shift_jis" | "sjis" | "shift-jis" | "windows-31j" | "ms_kanji" | "x-sjis"
        | "csshiftjis" => Charset::ShiftJis,
        "gbk" | "gb2312" | "gb18030" | "chinese" | "csgb2312" | "gb_2312" | "gb_2312-80"
        | "x-gbk" => Charset::Gbk,
        "big5" | "big5-hkscs" | "cn-big5" | "csbig5" | "x-x-big5" => Charset::Big5,
        // WHATWG 단일바이트 인코딩 전부 (라벨 별칭 포함)
        other => match crate::encoding_sbcs::table_for(other) {
            Some(t) => Charset::SingleByte(t),
            None => {
                // 이름은 있는데 우리가 못 읽는 인코딩 → 정직하게 표시한다.
                // (호출측이 UTF-8 로 폴백하되 경고를 남긴다)
                return Some(Charset::Unsupported);
            }
        },
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
        Charset::Utf8 => String::from_utf8_lossy(bytes).into_owned(),
        // 못 읽는 인코딩: UTF-8 로 시도하되 조용히 넘어가지 않는다 (글자가 깨진다).
        Charset::Unsupported => {
            println!("[encoding] 지원하지 않는 인코딩 — UTF-8 로 시도한다 (글자가 깨질 수 있음)");
            String::from_utf8_lossy(bytes).into_owned()
        }
        // 단일바이트: 0x00..0x7F 는 ASCII, 0x80..0xFF 는 표 (WHATWG §단일바이트 디코더)
        Charset::SingleByte(table) => bytes
            .iter()
            .map(|&b| {
                if b < 0x80 {
                    b as char
                } else {
                    char::from_u32(table[(b - 0x80) as usize] as u32).unwrap_or('\u{FFFD}')
                }
            })
            .collect(),
        Charset::Windows1252 => bytes.iter().map(|&b| w1252_char(b)).collect(),
        Charset::Cp949 => decode_cp949(bytes),
        Charset::ShiftJis => decode_shift_jis(bytes),
        Charset::Gbk => decode_gbk(bytes),
        Charset::Big5 => decode_big5(bytes),
    }
}

fn push_cp(out: &mut String, cp: u32) {
    match char::from_u32(cp) {
        Some(c) if cp != 0 => out.push(c),
        _ => out.push('\u{FFFD}'),
    }
}

// Shift_JIS 디코더 (WHATWG Encoding §12.3.1 그대로).
fn decode_shift_jis(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        i += 1;
        if b <= 0x80 {
            out.push(b as char); // ASCII (0x80 은 그대로 U+0080)
            continue;
        }
        if (0xA1..=0xDF).contains(&b) {
            // 반각 가타카나
            out.push(char::from_u32(0xFF61 - 0xA1 + b as u32).unwrap_or('\u{FFFD}'));
            continue;
        }
        if !((0x81..=0x9F).contains(&b) || (0xE0..=0xFC).contains(&b)) {
            out.push('\u{FFFD}');
            continue;
        }
        let Some(&t) = bytes.get(i) else {
            out.push('\u{FFFD}');
            break;
        };
        i += 1;
        let lead_off = if b < 0xA0 { 0x81u32 } else { 0xC1 };
        let trail_off = if t < 0x7F { 0x40u32 } else { 0x41 };
        if !((0x40..=0x7E).contains(&t) || (0x80..=0xFC).contains(&t)) {
            out.push('\u{FFFD}');
            i -= 1; // trail 은 다시 해석 (표준: prepend)
            continue;
        }
        let ptr = (b as u32 - lead_off) * 188 + (t as u32 - trail_off);
        if (8836..=10715).contains(&ptr) {
            // 사용자 정의 영역
            push_cp(&mut out, 0xE000 - 8836 + ptr);
            continue;
        }
        match cjk::JIS0208.get(ptr as usize).copied() {
            Some(cp) if cp != 0 => push_cp(&mut out, cp as u32),
            _ => out.push('\u{FFFD}'),
        }
    }
    out
}

// GBK / GB18030 디코더 (WHATWG Encoding §10.2.1). 4바이트 시퀀스도 처리한다.
fn decode_gbk(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        i += 1;
        if b < 0x80 {
            out.push(b as char);
            continue;
        }
        if b == 0x80 {
            out.push('\u{20AC}'); // GBK 확장: 유로 기호
            continue;
        }
        if !(0x81..=0xFE).contains(&b) {
            out.push('\u{FFFD}');
            continue;
        }
        let Some(&t) = bytes.get(i) else {
            out.push('\u{FFFD}');
            break;
        };
        // 4바이트 시퀀스: lead 0x81..0xFE, 2번째 0x30..0x39
        if (0x30..=0x39).contains(&t) {
            if i + 2 < bytes.len() {
                let (b3, b4) = (bytes[i + 1], bytes[i + 2]);
                if (0x81..=0xFE).contains(&b3) && (0x30..=0x39).contains(&b4) {
                    let ptr = ((b as u32 - 0x81) * 10 + (t as u32 - 0x30)) * 1260
                        + (b3 as u32 - 0x81) * 10
                        + (b4 as u32 - 0x30);
                    i += 3;
                    push_cp(&mut out, gb18030_range_cp(ptr));
                    continue;
                }
            }
            out.push('\u{FFFD}');
            continue;
        }
        i += 1;
        if t == 0x7F || t < 0x40 {
            out.push('\u{FFFD}');
            continue;
        }
        let off = if t < 0x7F { 0x40u32 } else { 0x41 };
        let ptr = (b as u32 - 0x81) * 190 + (t as u32 - off);
        match cjk::GB18030.get(ptr as usize).copied() {
            Some(cp) if cp != 0 => push_cp(&mut out, cp as u32),
            _ => out.push('\u{FFFD}'),
        }
    }
    out
}

// 4바이트 GB18030 포인터 → 코드포인트 (범위 표 이분 탐색)
fn gb18030_range_cp(ptr: u32) -> u32 {
    if ptr > 39419 && ptr < 189000 || ptr > 1237575 {
        return 0; // 표준: 매핑 없음
    }
    if ptr == 7457 {
        return 0xE7C7;
    }
    let mut best = (0u32, 0u32);
    for &(p, cp) in cjk::GB18030_RANGES.iter() {
        if p <= ptr {
            best = (p, cp);
        } else {
            break;
        }
    }
    best.1 + (ptr - best.0)
}

// Big5 디코더 (WHATWG Encoding §11.2.1).
fn decode_big5(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        i += 1;
        if b < 0x80 {
            out.push(b as char);
            continue;
        }
        if !(0x81..=0xFE).contains(&b) {
            out.push('\u{FFFD}');
            continue;
        }
        let Some(&t) = bytes.get(i) else {
            out.push('\u{FFFD}');
            break;
        };
        if !((0x40..=0x7E).contains(&t) || (0xA1..=0xFE).contains(&t)) {
            out.push('\u{FFFD}');
            continue; // trail 이 아니면 lead 만 버린다
        }
        i += 1;
        let off = if t < 0x7F { 0x40u32 } else { 0x62 };
        let ptr = (b as u32 - 0x81) * 157 + (t as u32 - off);
        // 표준의 4개 예외: 두 코드포인트로 펼쳐진다
        let pair = match ptr {
            1133 => Some(['\u{00CA}', '\u{0304}']),
            1135 => Some(['\u{00CA}', '\u{030C}']),
            1164 => Some(['\u{00EA}', '\u{0304}']),
            1166 => Some(['\u{00EA}', '\u{030C}']),
            _ => None,
        };
        if let Some(p) = pair {
            out.push(p[0]);
            out.push(p[1]);
            continue;
        }
        match cjk::BIG5.get(ptr as usize).copied() {
            Some(cp) if cp != 0 => push_cp(&mut out, cp),
            _ => out.push('\u{FFFD}'),
        }
    }
    out
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
    fn decodes_whatwg_single_byte_encodings() {
        // 예전엔 아는 이름이 아니면 charset 을 통째로 무시하고 UTF-8 로 읽었다.
        // 글자가 전부 깨진 채로 "성공"한 것처럼 보인다 — 가장 나쁜 종류의 조용한 오류다.
        // 이제 WHATWG 단일바이트 인코딩 26종을 실제로 디코드한다.
        let cs = charset_from_content_type("text/html; charset=iso-8859-2").unwrap();
        assert!(matches!(cs, Charset::SingleByte(_)));
        // iso-8859-2 의 0xA1 = U+0104 (Ą), 0xE9 = U+00E9 (é)
        assert_eq!(decode_as(&[0xA1, 0xE9], cs), "Ąé");

        // koi8-r: 0xC1 = U+0430 (а)
        let koi = charset_from_content_type("text/plain; charset=koi8-r").unwrap();
        assert_eq!(decode_as(&[0xC1], koi), "а");

        // windows-1251 (키릴): 0xC0 = U+0410 (А)
        let cp1251 = charset_from_content_type("text/html;charset=windows-1251").unwrap();
        assert_eq!(decode_as(&[0xC0], cp1251), "А");

        // iso-8859-9(=windows-1254, 터키어): 0xFD = U+0131 (ı)
        let tr = charset_from_content_type("text/html; charset=iso-8859-9").unwrap();
        assert_eq!(decode_as(&[0xFD], tr), "ı");

        // 모르는 인코딩은 Unsupported 로 **표시**한다 (조용히 UTF-8 로 읽지 않는다)
        assert_eq!(
            charset_from_content_type("text/html; charset=x-made-up-9999"),
            Some(Charset::Unsupported)
        );
    }

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
    fn decodes_shift_jis() {
        // 바이트는 표준 코덱으로 인코딩한 실제 값이다 (지어낸 벡터가 아니다).
        let bytes: &[u8] = &[147, 250, 150, 123, 140, 234, 130, 204, 131, 101, 131, 88, 131, 103, 129, 65, 130, 208, 130, 231, 130, 170, 130, 200, 129, 66, 65, 66, 67, 49, 50, 51];
        let s = decode_as(bytes, Charset::ShiftJis);
        assert_eq!(s, "日本語のテスト、ひらがな。ABC123", "shift_jis 디코딩");
    }
    #[test]
    fn decodes_gbk() {
        // 바이트는 표준 코덱으로 인코딩한 실제 값이다 (지어낸 벡터가 아니다).
        let bytes: &[u8] = &[214, 208, 206, 196, 178, 226, 202, 212, 163, 172, 188, 242, 204, 229, 215, 214, 161, 163, 65, 66, 67, 49, 50, 51];
        let s = decode_as(bytes, Charset::Gbk);
        assert_eq!(s, "中文测试，简体字。ABC123", "gbk 디코딩");
    }
    #[test]
    fn decodes_big5() {
        // 바이트는 표준 코덱으로 인코딩한 실제 값이다 (지어낸 벡터가 아니다).
        let bytes: &[u8] = &[193, 99, 197, 233, 164, 164, 164, 229, 180, 250, 184, 213, 161, 65, 165, 191, 197, 233, 166, 114, 161, 67, 65, 66, 67, 49, 50, 51];
        let s = decode_as(bytes, Charset::Big5);
        assert_eq!(s, "繁體中文測試，正體字。ABC123", "big5 디코딩");
    }
    #[test]
    fn charset_detected_from_meta_for_cjk() {
        let body = "<meta charset=\"shift_jis\">".as_bytes();
        assert_eq!(decode(body, None).1, Charset::ShiftJis);
        let body = "<meta charset=\"big5\">".as_bytes();
        assert_eq!(decode(body, None).1, Charset::Big5);
        let body = "<meta charset=\"gb2312\">".as_bytes();
        assert_eq!(decode(body, None).1, Charset::Gbk);
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
