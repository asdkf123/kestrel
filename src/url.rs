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
