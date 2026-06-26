# Kestrel M3a — 네트워킹 코어 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** URL을 주면 HTTP/HTTPS로 가져와 상태코드·헤더·바디 바이트를 돌려주는 클라이언트를 만든다(리다이렉트·청크 포함).

**Architecture:** `url.rs`(URL 파싱)와 `http.rs`(연결·요청·응답 파싱). HTTP/URL/디청크는 직접, TLS는 rustls에 위임. 응답 파서는 합성 입력으로 단위 테스트, 실제 fetch는 수동 검증.

**Tech Stack:** Rust(edition 2021), `std::net::TcpStream`, `rustls`(TLS), `webpki-roots`(루트 CA).

## Global Constraints

- 프로젝트 위치: `~/Documents/Projects/kestrel/`. 다른 저장소 건드리지 않는다.
- TLS는 rustls에 위임(직접 구현 금지). HTTP/URL/디청크는 직접.
- 외부 의존성은 `rustls`, `webpki-roots`만 추가.
- GET만. http(80)/https(443). keep-alive·gzip·POST·쿠키 비범위.
- 어떤 입력에도 패닉하지 않고 `HttpError`/`UrlError` 반환.
- 네트워크 의존 코드는 `cargo test`에 넣지 않는다(수동 검증). 단위 테스트는 헤르메틱.
- 기존 렌더 파이프라인(M1/M2)은 건드리지 않는다(통합은 M3c).
- 계약 타입은 스펙 3.1을 따른다.
- 커밋 메시지 끝에: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`

---

### Task 1: `url.rs` — URL 파싱

**Files:**
- Create: `src/url.rs`
- Modify: `src/main.rs` (`mod url;` 추가)

**Interfaces:**
- Consumes: 없음
- Produces:
  - `pub struct Url { pub scheme: String, pub host: String, pub port: u16, pub path: String }`
  - `pub enum UrlError { NoScheme, UnsupportedScheme, NoHost }`
  - `impl Url { pub fn parse(input: &str) -> Result<Url, UrlError> }`

- [ ] **Step 1: 실패 테스트 먼저 작성**

`src/url.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_https_with_path() {
        let u = Url::parse("https://example.com/a/b").unwrap();
        assert_eq!(u.scheme, "https");
        assert_eq!(u.host, "example.com");
        assert_eq!(u.port, 443);
        assert_eq!(u.path, "/a/b");
    }

    #[test]
    fn defaults_path_and_http_port() {
        let u = Url::parse("http://example.com").unwrap();
        assert_eq!(u.port, 80);
        assert_eq!(u.path, "/");
    }

    #[test]
    fn explicit_port() {
        let u = Url::parse("http://h.local:8080/x").unwrap();
        assert_eq!(u.host, "h.local");
        assert_eq!(u.port, 8080);
        assert_eq!(u.path, "/x");
    }

    #[test]
    fn rejects_unknown_scheme() {
        assert!(Url::parse("ftp://h/").is_err());
        assert!(Url::parse("noscheme").is_err());
    }
}
```

- [ ] **Step 2: 테스트 실패 확인**

Run: `source ~/.cargo/env && cargo test url`
Expected: 컴파일 실패 — `Url` 미정의.

- [ ] **Step 3: 구현**

`src/url.rs` 테스트 위쪽에:

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct Url {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub path: String,
}

#[derive(Debug, PartialEq)]
pub enum UrlError {
    NoScheme,
    UnsupportedScheme,
    NoHost,
}

impl Url {
    pub fn parse(input: &str) -> Result<Url, UrlError> {
        let (scheme, rest) = input.split_once("://").ok_or(UrlError::NoScheme)?;
        let scheme = scheme.to_ascii_lowercase();
        let default_port = match scheme.as_str() {
            "http" => 80,
            "https" => 443,
            _ => return Err(UrlError::UnsupportedScheme),
        };
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], rest[i..].to_string()),
            None => (rest, "/".to_string()),
        };
        let path = path.split('#').next().unwrap_or("/").to_string();
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) => {
                let port = p.parse::<u16>().map_err(|_| UrlError::NoHost)?;
                (h.to_string(), port)
            }
            None => (authority.to_string(), default_port),
        };
        if host.is_empty() {
            return Err(UrlError::NoHost);
        }
        Ok(Url { scheme, host, port, path })
    }
}
```

`src/main.rs`의 `mod` 목록에 `mod url;` 추가.

- [ ] **Step 4: 테스트 통과 확인**

Run: `source ~/.cargo/env && cargo test url`
Expected: 4개 PASS.

- [ ] **Step 5: 커밋**

```bash
cd ~/Documents/Projects/kestrel && source ~/.cargo/env
git add src/url.rs src/main.rs
git commit -m "$(printf 'feat(url): URL 파싱 (scheme/host/port/path)\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>')"
```

---

### Task 2: `http.rs` — 의존성 + 연결 + 응답 파싱

**Files:**
- Modify: `Cargo.toml` (rustls, webpki-roots)
- Create: `src/http.rs`
- Modify: `src/main.rs` (`mod http;` 추가)

**Interfaces:**
- Consumes: `crate::url::{Url, UrlError}`
- Produces:
  - `pub struct Response { pub status: u16, pub headers: Vec<(String, String)>, pub body: Vec<u8> }`
  - `pub enum HttpError { Url(UrlError), Io(std::io::Error), Tls(String), BadResponse, TooManyRedirects, UnsupportedScheme }`
  - `pub fn fetch(url: &str) -> Result<Response, HttpError>`
  - (모듈 내부) `parse_response(&[u8]) -> Result<Response, HttpError>`, `dechunk(&[u8]) -> Result<Vec<u8>, HttpError>`, `header(&[(String,String)], &str) -> Option<String>`

- [ ] **Step 1: 의존성 추가 + 빌드 확인**

`Cargo.toml`의 `[dependencies]`에 추가:

```toml
rustls = { version = "0.23", default-features = false, features = ["ring", "std", "tls12", "logging"] }
webpki-roots = "0.26"
```

Run: `source ~/.cargo/env && cargo build`
Expected: rustls/ring/webpki-roots 컴파일 성공. (ring 빌드에 C 컴파일러·perl 필요 — macOS 기본 제공. 실패 시 build-essential류 설치.)

- [ ] **Step 2: 실패 테스트 먼저 작성 (헤르메틱 — 네트워크 없음)**

`src/http.rs` 맨 아래:

```rust
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
}
```

- [ ] **Step 3: 테스트 실패 확인**

Run: `source ~/.cargo/env && cargo test http`
Expected: 컴파일 실패 — `parse_response` 미정의.

- [ ] **Step 4: 구현**

`src/http.rs` 테스트 위쪽에:

```rust
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

use crate::url::{Url, UrlError};

pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Debug)]
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

pub fn fetch(url: &str) -> Result<Response, HttpError> {
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
    let tcp = TcpStream::connect((url.host.as_str(), url.port)).map_err(HttpError::Io)?;
    let mut stream: Box<dyn Stream> = match url.scheme.as_str() {
        "http" => Box::new(tcp),
        "https" => Box::new(tls_wrap(tcp, &url.host)?),
        _ => return Err(HttpError::UnsupportedScheme),
    };
    let req = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: Kestrel/0.1\r\nAccept-Encoding: identity\r\nConnection: close\r\n\r\n",
        url.path, url.host
    );
    stream.write_all(req.as_bytes()).map_err(HttpError::Io)?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).map_err(HttpError::Io)?;
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

fn resolve(base: &Url, location: &str) -> Result<Url, UrlError> {
    if location.starts_with("http://") || location.starts_with("https://") {
        Url::parse(location)
    } else if location.starts_with('/') {
        Ok(Url {
            scheme: base.scheme.clone(),
            host: base.host.clone(),
            port: base.port,
            path: location.to_string(),
        })
    } else {
        let mut path = base.path.clone();
        if let Some(idx) = path.rfind('/') {
            path.truncate(idx + 1);
        }
        path.push_str(location);
        Ok(Url {
            scheme: base.scheme.clone(),
            host: base.host.clone(),
            port: base.port,
            path,
        })
    }
}
```

`src/main.rs`의 `mod` 목록에 `mod http;` 추가.

- [ ] **Step 5: 테스트 통과 확인**

Run: `source ~/.cargo/env && cargo test http`
Expected: 3개 PASS.

- [ ] **Step 6: 커밋**

```bash
cd ~/Documents/Projects/kestrel && source ~/.cargo/env
git add Cargo.toml Cargo.lock src/http.rs src/main.rs
git commit -m "$(printf 'feat(http): HTTP/1.1 클라이언트 (rustls TLS) + 응답 파싱/디청크\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>')"
```

---

### Task 3: `--fetch` 모드 + 실제 fetch 수동 검증

**Files:**
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `crate::http::fetch`
- Produces: CLI `--fetch <url>` 모드

- [ ] **Step 1: `--fetch` 모드 추가**

`src/main.rs`의 `main()` 시작부(다른 분기보다 먼저)에 추가:

```rust
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 3 && args[1] == "--fetch" {
        match http::fetch(&args[2]) {
            Ok(resp) => {
                println!("status {} | {} headers | {} body bytes", resp.status, resp.headers.len(), resp.body.len());
                let preview: String = String::from_utf8_lossy(&resp.body).chars().take(200).collect();
                println!("--- first 200 chars ---\n{}", preview);
            }
            Err(e) => println!("fetch error: {:?}", e),
        }
        return;
    }
```

- [ ] **Step 2: 빌드 + 전체 테스트**

Run: `source ~/.cargo/env && cargo build`
Expected: 성공.

Run: `source ~/.cargo/env && cargo test`
Expected: 기존 + url(4) + http(3) 모두 PASS.

- [ ] **Step 3: 실제 fetch 수동 검증 (네트워크 필요)**

```bash
cd ~/Documents/Projects/kestrel && source ~/.cargo/env
cargo run -- --fetch https://example.com
```
Expected: `status 200 | N headers | M body bytes` 출력 + 본문 앞부분에 `<!doctype html>`/`<html>` 등 example.com HTML이 보임. (TLS 핸드셰이크 + 인증서 검증이 rustls로 수행됨.)

추가 확인(리다이렉트): `cargo run -- --fetch http://example.com` 도 200으로 끝나야 함(http→https 리다이렉트 추적, 단 example.com이 http로 응답하면 그대로).

- [ ] **Step 4: 커밋**

```bash
cd ~/Documents/Projects/kestrel && source ~/.cargo/env
git add src/main.rs
git commit -m "$(printf 'feat: --fetch CLI 모드 + 실제 HTTPS fetch 검증 (M3a 완성)\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>')"
```

---

## Self-Review

**1. Spec coverage:** URL 파싱 → Task 1. HTTP 연결/요청/응답 파싱/디청크/리다이렉트 → Task 2. rustls TLS → Task 2 `tls_wrap`. 의존성(rustls/webpki-roots) → Task 2. 실제 fetch 수동 검증 → Task 3. 헤르메틱 단위 테스트(url, 응답 파서, 디청크) → Task 1/2. 스펙 항목 전부 커버.

**2. Placeholder scan:** 없음. 모든 코드 완전. 단 rustls 0.23 API는 버전에 민감 — 빌드 에러 시 해당 버전 시그니처로 조정(주석 명시).

**3. Type consistency:**
- `Url{scheme,host:String,port:u16,path:String}`, `Url::parse(&str)->Result<Url,UrlError>` — Task 1 정의, Task 2 `request`/`resolve`에서 동일 사용. ✓
- `Response{status:u16,headers:Vec<(String,String)>,body:Vec<u8>}`, `fetch(&str)->Result<Response,HttpError>` — Task 2 정의, Task 3 사용. ✓
- `header`/`parse_response`/`dechunk` 내부 함수 — Task 2 정의/테스트. ✓
- `HttpError` 변형들 일관. ✓

불일치 없음.
