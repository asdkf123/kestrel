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

/// RFC 3986 §5.2.4 remove_dot_segments — 경로의 "." / ".." 세그먼트를 해소한다.
/// 예: "/a/b/../c.css" → "/a/c.css", "/dir/." → "/dir/", "/a/.." → "/".
/// 쿼리(?…)/프래그먼트는 경로가 아니므로 호출측이 떼고 넘긴다.
pub fn remove_dot_segments(path: &str) -> String {
    let absolute = path.starts_with('/');
    // ".." 는 상위로, "." 와 빈 세그먼트는 버린다. 루트 위로는 못 올라간다.
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    // 원본이 디렉터리로 끝나면(슬래시/./..) 결과도 슬래시로 끝나야 한다.
    let dir_like = path.ends_with('/')
        || path.ends_with("/.")
        || path.ends_with("/..")
        || path == "."
        || path == "..";
    let mut s = String::new();
    if absolute {
        s.push('/');
    }
    s.push_str(&out.join("/"));
    if dir_like && !s.ends_with('/') {
        s.push('/');
    }
    s
}

// 경로에 붙은 쿼리는 보존하고 경로 부분만 dot 세그먼트를 해소한다.
fn normalize_with_query(path_and_query: &str) -> String {
    match path_and_query.split_once('?') {
        Some((p, q)) => format!("{}?{}", remove_dot_segments(p), q),
        None => remove_dot_segments(path_and_query),
    }
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

    /// 상대/절대 href 를 이 URL 기준으로 해석한다.
    pub fn join(&self, href: &str) -> Option<Url> {
        if href.starts_with("http://") || href.starts_with("https://") {
            Url::parse(href).ok()
        } else if let Some(rest) = href.strip_prefix("//") {
            // 프로토콜 상대 (//host/path)
            Url::parse(&format!("{}://{}", self.scheme, rest)).ok()
        } else if href.starts_with('/') {
            let p = href.split('#').next().unwrap_or("/");
            Some(self.with_path(&normalize_with_query(p)))
        } else {
            // 프래그먼트만(#x) → 기준 URL 그대로 (RFC 3986 §5.3)
            let no_frag = href.split('#').next().unwrap_or("");
            if no_frag.is_empty() {
                return Some(self.clone());
            }
            // 쿼리만(?x) → 기준 경로 + 새 쿼리
            if let Some(q) = no_frag.strip_prefix('?') {
                let base_path = self.path.split('?').next().unwrap_or("/");
                return Some(self.with_path(&format!("{}?{}", base_path, q)));
            }
            // 현재 경로의 디렉터리 기준 상대 → dot 세그먼트 해소
            let base_path = self.path.split('?').next().unwrap_or("/");
            let mut dir = base_path.to_string();
            match dir.rfind('/') {
                Some(i) => dir.truncate(i + 1),
                None => dir = "/".to_string(),
            }
            dir.push_str(no_frag);
            Some(self.with_path(&normalize_with_query(&dir)))
        }
    }

    fn with_path(&self, path: &str) -> Url {
        Url {
            scheme: self.scheme.clone(),
            host: self.host.clone(),
            port: self.port,
            path: path.to_string(),
        }
    }

    pub fn as_string(&self) -> String {
        let default_port = (self.scheme == "http" && self.port == 80)
            || (self.scheme == "https" && self.port == 443);
        if default_port {
            format!("{}://{}{}", self.scheme, self.host, self.path)
        } else {
            format!("{}://{}:{}{}", self.scheme, self.host, self.port, self.path)
        }
    }
}

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

    #[test]
    fn joins_relative_and_absolute() {
        let base = Url::parse("https://site.com/dir/page.html").unwrap();
        // 절대
        assert_eq!(base.join("https://o.com/x").unwrap().host, "o.com");
        // 루트 상대
        let r = base.join("/style.css").unwrap();
        assert_eq!(r.host, "site.com");
        assert_eq!(r.path, "/style.css");
        // 디렉터리 상대
        let d = base.join("a.css").unwrap();
        assert_eq!(d.path, "/dir/a.css");
        // 프로토콜 상대
        assert_eq!(base.join("//cdn.com/s.css").unwrap().host, "cdn.com");
    }

    #[test]
    fn removes_dot_segments_rfc3986() {
        // §5.2.4 — 상위 이동/현재 디렉터리 해소
        assert_eq!(remove_dot_segments("/a/b/../c.css"), "/a/c.css");
        assert_eq!(remove_dot_segments("/a/b/./c"), "/a/b/c");
        assert_eq!(remove_dot_segments("/a/b/.."), "/a/");
        assert_eq!(remove_dot_segments("/a/.."), "/");
        assert_eq!(remove_dot_segments("/a/b/"), "/a/b/");
        assert_eq!(remove_dot_segments("/a/b/."), "/a/b/");
        // 루트 위로는 못 올라간다
        assert_eq!(remove_dot_segments("/../x"), "/x");
        assert_eq!(remove_dot_segments("/a/../../x"), "/x");
        // 다중 상위
        assert_eq!(remove_dot_segments("/a/b/c/../../d"), "/a/d");
    }

    #[test]
    fn joins_dot_relative_paths() {
        let base = Url::parse("https://site.com/a/b/page.html").unwrap();
        // 상위 디렉터리 상대 (실사이트 CSS/JS/이미지에 흔함)
        assert_eq!(base.join("../c.css").unwrap().path, "/a/c.css");
        assert_eq!(base.join("../../top.css").unwrap().path, "/top.css");
        assert_eq!(base.join("./same.css").unwrap().path, "/a/b/same.css");
        // "." → 현재 디렉터리
        assert_eq!(base.join(".").unwrap().path, "/a/b/");
        assert_eq!(base.join("..").unwrap().path, "/a/");
        // 루트 상대도 정규화
        assert_eq!(base.join("/x/../y.css").unwrap().path, "/y.css");
        // 쿼리는 보존
        assert_eq!(base.join("../c.css?v=2").unwrap().path, "/a/c.css?v=2");
        // 쿼리만 / 프래그먼트만 (RFC 3986 §5.3)
        assert_eq!(base.join("?q=1").unwrap().path, "/a/b/page.html?q=1");
        assert_eq!(base.join("#frag").unwrap().path, "/a/b/page.html");
    }

    #[test]
    fn round_trips_to_string() {
        assert_eq!(Url::parse("https://a.com/x").unwrap().as_string(), "https://a.com/x");
        assert_eq!(Url::parse("http://a.com:8080/y").unwrap().as_string(), "http://a.com:8080/y");
    }
}
