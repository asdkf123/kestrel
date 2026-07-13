use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use crate::url::{Url, UrlError};

// 응답 없는 서버에 영원히 매달리지 않기 위한 소켓 타임아웃
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const IO_TIMEOUT: Duration = Duration::from_secs(15);

// User-Agent 는 능력 선언이기도 하다. 서버는 이걸 보고 무엇을 내려줄지 정한다.
// "Mozilla/5.0 …" 접두와 호환 토큰은 모든 브라우저가 붙이는 역사적 관습이고
// (Chrome/Safari/Firefox/Ladybird 전부), 서버는 이걸 보고 모던 브라우저용 마크업과
// 자원을 내려준다 — 우리가 그리려는 바로 그 콘텐츠다. 우리 자신은 끝의 Kestrel/0.1 로 밝힌다.
//
// 이 UA 를 켜면 구글폰트가 ttf 대신 woff2 를 내려준다. 그래서 woff2 디코더(brotli +
// glyf/loca 역변환)를 먼저 구현했다 — 없는 능력을 광고하면 대가를 치르기 때문이다
// (Accept 헤더에서 image/webp 를 뺀 것과 같은 원리).
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
AppleWebKit/537.36 (KHTML, like Gecko) Kestrel/0.1 Safari/537.36";

// Accept 는 우리가 실제로 디코드할 수 있는 것만 광고한다. 이건 협상이지 인사말이 아니다.
// 브라우저 UA 를 쓰면서 image/webp,image/avif 를 받는다고 하면 서버는 기꺼이 WebP 를
// 내려준다 — 그리고 우리는 그걸 못 읽어서 이미지가 전부 사라진다(실제로 그렇게 됐다.
// 위키백과 이미지 디코드가 3~5개에서 0개가 됐다). 없는 능력을 광고하면 그 대가를 치른다.
// PNG/JPEG 만 디코드하므로 그것만 우선순위로 밝히고, 나머지는 낮은 q 로 받는다.
const ACCEPT: &str = "text/html,application/xhtml+xml,application/xml;q=0.9,\
image/png,image/jpeg,*/*;q=0.8";

pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Debug)]
#[allow(dead_code)]
pub enum HttpError {
    Url(UrlError),
    Io(std::io::Error),
    Tls(String),
    BadResponse,
    TooManyRedirects,
    UnsupportedScheme,
}

trait Stream: Read + Write {}
impl<T: Read + Write> Stream for T {}

// 일시적 실패는 재시도한다 (GET 은 멱등). 브라우저도 그렇게 한다.
// 예전엔 한 번 실패하면 그대로 버려서 렌더가 비결정적이었다 — 같은 페이지를 두 번
// 그리면 이미지가 있다 없다 했다(위키미디어가 병렬 요청을 429 로 조인다).
const MAX_ATTEMPTS: u32 = 3;
const RETRY_BASE_MS: u64 = 250;

fn is_transient(status: u16) -> bool {
    matches!(status, 408 | 425 | 429 | 500 | 502 | 503 | 504)
}

pub fn fetch(url: &str) -> Result<Response, HttpError> {
    let mut last: Result<Response, HttpError> = Err(HttpError::BadResponse);
    for attempt in 0..MAX_ATTEMPTS {
        if attempt > 0 {
            // 지수 백오프. Retry-After(초)가 있으면 그걸 따르되 상한을 둔다.
            let wait = match &last {
                Ok(r) => header(&r.headers, "retry-after")
                    .and_then(|v| v.trim().parse::<u64>().ok())
                    .map(|s| (s * 1000).min(2000))
                    .unwrap_or(RETRY_BASE_MS << (attempt - 1)),
                Err(_) => RETRY_BASE_MS << (attempt - 1),
            };
            std::thread::sleep(Duration::from_millis(wait));
        }
        last = fetch_once(url);
        match &last {
            // 일시적 상태 코드가 아니면 그대로 반환 (2xx/4xx 등)
            Ok(r) if !is_transient(r.status) => return last,
            // 네트워크/프레이밍 오류와 일시적 상태 코드는 재시도
            _ => {}
        }
    }
    last
}

fn fetch_once(url: &str) -> Result<Response, HttpError> {
    let mut current = Url::parse(url).map_err(HttpError::Url)?;
    for _ in 0..6 {
        let raw = request(&current)?;
        let resp = parse_response(&raw)?;
        if (300..400).contains(&resp.status) {
            if let Some(loc) = header(&resp.headers, "location") {
                current = resolve(&current, &loc).map_err(HttpError::Url)?;
                continue;
            }
        }
        return Ok(resp);
    }
    Err(HttpError::TooManyRedirects)
}

fn request(url: &Url) -> Result<Vec<u8>, HttpError> {
    let addr = (url.host.as_str(), url.port)
        .to_socket_addrs()
        .map_err(HttpError::Io)?
        .next()
        .ok_or_else(|| {
            HttpError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "no address"))
        })?;
    let tcp = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).map_err(HttpError::Io)?;
    tcp.set_read_timeout(Some(IO_TIMEOUT)).map_err(HttpError::Io)?;
    tcp.set_write_timeout(Some(IO_TIMEOUT)).map_err(HttpError::Io)?;
    let mut stream: Box<dyn Stream> = match url.scheme.as_str() {
        "http" => Box::new(tcp),
        "https" => Box::new(tls_wrap(tcp, &url.host)?),
        _ => return Err(HttpError::UnsupportedScheme),
    };
    let req = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: {}\r\nAccept: {}\r\nAccept-Language: en-US,en;q=0.9\r\nAccept-Encoding: identity\r\nConnection: close\r\n\r\n",
        url.path, url.host, USER_AGENT, ACCEPT
    );
    stream.write_all(req.as_bytes()).map_err(HttpError::Io)?;
    let mut buf = Vec::new();
    match stream.read_to_end(&mut buf) {
        Ok(_) => {}
        // close_notify 없이 연결을 닫는 서버 관용 (구글 등 다수).
        // 절단 여부는 HTTP 프레이밍이 판단한다: parse_response 가
        // Content-Length 미달이면 BadResponse 로 거른다 (절단 공격 방어).
        // 모든 클라이언트(브라우저/curl)가 같은 판단을 한다.
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof && !buf.is_empty() => {}
        Err(e) => return Err(HttpError::Io(e)),
    }
    Ok(buf)
}

fn tls_wrap(tcp: TcpStream, host: &str) -> Result<impl Read + Write, HttpError> {
    use rustls::pki_types::ServerName;
    use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

    let roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = ClientConfig::builder_with_provider(rustls::crypto::ring::default_provider().into())
        .with_safe_default_protocol_versions()
        .map_err(|e| HttpError::Tls(e.to_string()))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let name = ServerName::try_from(host.to_string())
        .map_err(|_| HttpError::Tls("bad server name".into()))?;
    let conn = ClientConnection::new(Arc::new(config), name)
        .map_err(|e| HttpError::Tls(e.to_string()))?;
    Ok(StreamOwned::new(conn, tcp))
}

fn parse_response(raw: &[u8]) -> Result<Response, HttpError> {
    let sep = find_subslice(raw, b"\r\n\r\n").ok_or(HttpError::BadResponse)?;
    let head = String::from_utf8_lossy(&raw[..sep]);
    let body_raw = &raw[sep + 4..];

    let mut lines = head.split("\r\n");
    let status_line = lines.next().ok_or(HttpError::BadResponse)?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or(HttpError::BadResponse)?;

    let mut headers = Vec::new();
    for line in lines {
        if let Some(idx) = line.find(':') {
            headers.push((line[..idx].trim().to_string(), line[idx + 1..].trim().to_string()));
        }
    }

    let chunked = header(&headers, "transfer-encoding")
        .map_or(false, |v| v.to_ascii_lowercase().contains("chunked"));
    let body = if chunked { dechunk(body_raw)? } else { body_raw.to_vec() };

    // 절단 방어: Content-Length 선언보다 짧으면 불완전한 응답으로 거부.
    // (close_notify 관용의 안전핀 — 프레이밍 완결성 검증)
    if !chunked {
        if let Some(cl) = header(&headers, "content-length").and_then(|v| v.parse::<usize>().ok())
        {
            if body.len() < cl {
                return Err(HttpError::BadResponse);
            }
        }
    }

    Ok(Response { status, headers, body })
}

fn header(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.clone())
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn dechunk(mut data: &[u8]) -> Result<Vec<u8>, HttpError> {
    let mut out = Vec::new();
    loop {
        let line_end = find_subslice(data, b"\r\n").ok_or(HttpError::BadResponse)?;
        let size_str = std::str::from_utf8(&data[..line_end]).map_err(|_| HttpError::BadResponse)?;
        let size = usize::from_str_radix(size_str.split(';').next().unwrap().trim(), 16)
            .map_err(|_| HttpError::BadResponse)?;
        data = &data[line_end + 2..];
        if size == 0 {
            break;
        }
        if data.len() < size {
            return Err(HttpError::BadResponse);
        }
        out.extend_from_slice(&data[..size]);
        data = &data[size..];
        if data.len() >= 2 {
            data = &data[2..]; // 청크 뒤 \r\n
        }
    }
    Ok(out)
}

// 리다이렉트 Location 해석. URL 결합 규칙(RFC 3986: dot-segment 해소, 프로토콜 상대,
// 쿼리/프래그먼트만)은 Url::join 에 이미 구현돼 있다 — 여기서 중복 구현하지 않는다.
// (예전엔 자체 복사본이라 dot-segment 를 안 지워 `../x` 리다이렉트가 /a/b/../x 가 됐다)
fn resolve(base: &Url, location: &str) -> Result<Url, UrlError> {
    if let Some(u) = base.join(location) {
        Ok(u)
    } else {
        Url::parse(location)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_status_headers_body() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 5\r\n\r\nhello";
        let r = parse_response(raw).unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(header(&r.headers, "content-type").as_deref(), Some("text/html"));
        assert_eq!(r.body, b"hello");
    }

    #[test]
    fn decodes_chunked_body() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
        let r = parse_response(raw).unwrap();
        assert_eq!(r.body, b"hello world");
    }

    #[test]
    fn header_lookup_is_case_insensitive() {
        let h = vec![("Content-Type".to_string(), "x".to_string())];
        assert_eq!(header(&h, "content-type").as_deref(), Some("x"));
    }

    #[test]
    fn truncated_body_is_rejected() {
        // Content-Length 10 인데 5바이트만 → 절단으로 판단
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nhello";
        assert!(matches!(parse_response(raw), Err(HttpError::BadResponse)));
        // 정확히 도착하면 통과
        let ok = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello";
        assert_eq!(parse_response(ok).unwrap().body, b"hello");
    }
}

#[cfg(test)]
mod audit_tests {
    use super::*;

    #[test]
    fn redirect_resolve_normalizes_dot_segments() {
        // 리다이렉트 Location 이 상대경로일 때 RFC 3986 dot-segment 를 해소해야 한다.
        let base = Url::parse("https://site.com/a/b/page.html").unwrap();
        assert_eq!(resolve(&base, "../c.html").unwrap().path, "/a/c.html");
        assert_eq!(resolve(&base, "./d.html").unwrap().path, "/a/b/d.html");
        assert_eq!(resolve(&base, "/x/../y.html").unwrap().path, "/y.html");
    }
}
