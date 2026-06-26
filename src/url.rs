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

    /// 상대/절대 href 를 이 URL 기준으로 해석한다.
    pub fn join(&self, href: &str) -> Option<Url> {
        if href.starts_with("http://") || href.starts_with("https://") {
            Url::parse(href).ok()
        } else if let Some(rest) = href.strip_prefix("//") {
            // 프로토콜 상대 (//host/path)
            Url::parse(&format!("{}://{}", self.scheme, rest)).ok()
        } else if href.starts_with('/') {
            Some(Url {
                scheme: self.scheme.clone(),
                host: self.host.clone(),
                port: self.port,
                path: href.split('#').next().unwrap_or("/").to_string(),
            })
        } else {
            // 현재 경로의 디렉터리 기준 상대
            let mut path = self.path.clone();
            match path.rfind('/') {
                Some(i) => path.truncate(i + 1),
                None => path = "/".to_string(),
            }
            path.push_str(href.split('#').next().unwrap_or(""));
            Some(Url {
                scheme: self.scheme.clone(),
                host: self.host.clone(),
                port: self.port,
                path,
            })
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
    fn round_trips_to_string() {
        assert_eq!(Url::parse("https://a.com/x").unwrap().as_string(), "https://a.com/x");
        assert_eq!(Url::parse("http://a.com:8080/y").unwrap().as_string(), "http://a.com:8080/y");
    }
}
