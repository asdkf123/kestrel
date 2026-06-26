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
