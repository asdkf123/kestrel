# Kestrel M3a — 네트워킹 코어 (URL + HTTP 클라이언트): 설계 문서

- 날짜: 2026-06-26
- 상태: 승인됨 (브레인스토밍 완료)
- 범위: M3a 만. (M3b = HTML 파서 견고화, M3c = fetch→render 통합은 별도)

## 1. 맥락

M1/M2로 HTML+CSS+텍스트를 렌더하는 엔진을 만들었지만 입력은 로컬 파일뿐이다. M3는 **인터넷에서 페이지를 가져온다**. 전부 직접 짜되, **TLS만 rustls에 위임**(직접 구현은 보안상 금물 — 결정 기록 참조).

M3 분할:
- **M3a (이 문서)**: URL 파싱 + HTTP/1.1 클라이언트(http=TCP, https=rustls). 결과물: URL → 응답 바이트.
- **M3b**: HTML 파서 견고화(doctype/주석/void/script/style/엔티티, 패닉 금지).
- **M3c**: fetch → parse → render 통합 (`kestrel <url>`).

## 2. 목표 (M3a, 한 줄)

URL 문자열을 주면 HTTP/HTTPS로 가져와 상태코드·헤더·바디 바이트를 돌려준다. 리다이렉트와 청크 인코딩 처리 포함.

## 3. 아키텍처

새 모듈 두 개. 기존 렌더 파이프라인은 건드리지 않는다(통합은 M3c).

```
"https://example.com/path" → [url] → Url{scheme,host,port,path}
                                        │
                                        ▼
                              [http] fetch(&Url) →  TCP 연결 (+ https면 rustls)
                                        │           GET 요청 작성/전송
                                        ▼           응답 수신/파싱
                              Response { status, headers, body }
```

| 모듈 | 입력 → 출력 | 책임 |
|------|------------|------|
| `url.rs` | `&str` → `Url` | scheme/host/port/path 파싱 |
| `http.rs` | `&Url` → `Response` | TCP/TLS 연결, GET 요청, 응답 파싱, 리다이렉트 |

### 3.1 계약 타입

```
// url.rs
pub struct Url { pub scheme: String, pub host: String, pub port: u16, pub path: String }
impl Url {
    pub fn parse(input: &str) -> Result<Url, UrlError>;   // http=80, https=443 기본 포트
}

// http.rs
pub struct Response { pub status: u16, pub headers: Vec<(String, String)>, pub body: Vec<u8> }
pub enum HttpError { Url(UrlError), Io(std::io::Error), Tls(String), BadResponse, TooManyRedirects, UnsupportedScheme }
pub fn fetch(url: &str) -> Result<Response, HttpError>;   // 리다이렉트 자동 추적(상한 있음)
```

## 4. `url.rs` 상세

- 형식: `scheme://host[:port][/path][?query][#frag]`. 최소 파싱: scheme, host, port(없으면 기본), path(없으면 "/"). query는 path에 포함, fragment는 버림.
- 지원 scheme: `http`(80), `https`(443). 그 외 `UrlError`.
- host에 포트가 붙으면 분리. IPv6 대괄호는 비범위(YAGNI).

## 5. `http.rs` 상세

1. **연결**: `TcpStream::connect((host, port))`. https면 그 위에 rustls로 감싼 스트림. 두 경우를 동일 `Read + Write` 인터페이스로 추상화(enum 또는 boxed trait object).
2. **TLS(rustls)**: `webpki-roots`의 루트 CA로 `ClientConfig` 구성, `ServerName`=host, `ClientConnection` + `StreamOwned`. 인증서 검증은 rustls가 수행.
3. **요청**: `GET {path} HTTP/1.1` + 헤더 `Host`, `User-Agent: Kestrel/0.1`, `Accept-Encoding: identity`(압축 안 받음), `Connection: close`(EOF까지 읽기 단순화).
4. **응답 수신**: 소켓을 EOF까지 모두 읽음. `\r\n\r\n`으로 헤더/바디 분리.
5. **파싱**: 상태줄(`HTTP/1.1 200 OK` → status u16), 헤더(`Name: Value`), 바디. `Transfer-Encoding: chunked`면 디청크. (`Content-Length`는 Connection: close라 보조적.)
6. **리다이렉트**: status 3xx + `Location`이면 그 URL로 재요청. 상대 Location은 현재 URL 기준 해석. 상한(예: 5회) 초과 시 `TooManyRedirects`.

## 6. 의존성

- 추가: `rustls`(TLS), `webpki-roots`(루트 CA). 이 둘만 외부.
- TCP는 std(`std::net::TcpStream`). URL 파싱·HTTP 파싱·디청크는 직접.

## 7. 범위 / 비범위

**범위**: GET, http/https, 리다이렉트, chunked 디코딩, 상태/헤더/바디 파싱.

**비범위**: POST/기타 메서드, 쿠키, gzip/br 압축 해제, 커넥션 재사용(keep-alive), HTTP/2·3, 캐시, 프록시, IPv6 리터럴. (후속)

## 8. 에러 처리

- 연결/IO 실패 → `HttpError::Io`. TLS 실패 → `HttpError::Tls`. 잘못된 응답 → `BadResponse`. 패닉하지 않는다.
- 알 수 없는 scheme → `UnsupportedScheme`.

## 9. 테스트 / 검증

- **헤르메틱 단위 테스트(네트워크 불필요)**:
  - `url.rs`: `Url::parse("https://example.com/a/b")` → scheme/host/port(443)/path 정확. 포트 명시(`http://h:8080/`) 처리. 잘못된 입력 → 에러.
  - `http.rs`: 응답 파서를 **합성 바이트 버퍼**로 테스트 — 상태줄/헤더/Content-Length 바디 파싱, chunked 디코딩(`"5\r\nhello\r\n0\r\n\r\n"` → `"hello"`). (소켓 없이 파싱 함수 단위로.)
- **수동 통합 검증(네트워크 필요)**: `kestrel --fetch https://example.com` 모드를 추가해 status 200 + HTML 바이트 길이를 출력. (네트워크 의존이라 `cargo test`에는 넣지 않음 — 기존 PPM 덤프처럼 수동 실행.)

## 10. 완료 기준 (M3a Definition of Done)

1. `Url::parse`가 http/https URL을 정확히 분해.
2. 응답 파서가 상태/헤더/Content-Length·chunked 바디를 합성 입력에서 올바르게 파싱(단위 테스트 통과).
3. `--fetch` 모드로 실제 `https://example.com`에서 200 + HTML을 받아온다(수동 검증).
4. 기존 M1/M2 테스트 모두 통과.

## 11. 결정 기록

| 질문 | 결정 |
|------|------|
| TLS | rustls에 위임 (직접 구현 안 함 — 보안). HTTP/URL/디청크는 직접 |
| M3 분할 | M3a(네트워킹) / M3b(파서 견고화) / M3c(통합) |
| 메서드 | GET만 |
| 연결 | Connection: close (keep-alive 비범위) |
| 압축 | Accept-Encoding: identity (gzip 비범위) |
| 네트워크 테스트 | 단위 테스트는 헤르메틱, 실제 fetch는 수동 검증 |
