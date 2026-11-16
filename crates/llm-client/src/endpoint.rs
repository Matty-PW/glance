//! Where a model lives. Only loopback endpoints can be constructed.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use reqwest::Url;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EndpointError {
    #[error("`{0}` is not a valid http(s) base URL")]
    Invalid(String),
    #[error(
        "`{0}` is not a loopback address. Screen content may only be sent to 127.0.0.1, ::1 or localhost"
    )]
    NotLoopback(String),
    #[error("endpoint URLs must not contain credentials")]
    Credentials,
}

/// An OpenAI-compatible endpoint plus the model name to request from it.
#[derive(Debug, Clone)]
pub struct Endpoint {
    chat_url: Url,
    model: String,
    /// `localhost` is pinned to 127.0.0.1 so a hosts-file or DNS change cannot
    /// redirect it.
    pub(crate) pinned: Option<(String, SocketAddr)>,
}

impl Endpoint {
    /// `base_url` is the API root, for example `http://127.0.0.1:8080/v1`.
    pub fn loopback(base_url: &str, model: impl Into<String>) -> Result<Self, EndpointError> {
        let invalid = || EndpointError::Invalid(base_url.to_owned());
        let base = Url::parse(base_url).map_err(|_| invalid())?;
        if !matches!(base.scheme(), "http" | "https") {
            return Err(invalid());
        }
        if !base.username().is_empty() || base.password().is_some() {
            return Err(EndpointError::Credentials);
        }
        let host = base.host_str().ok_or_else(invalid)?;
        let port = base.port_or_known_default().ok_or_else(invalid)?;

        let bare = host.trim_start_matches('[').trim_end_matches(']');
        let pinned = match bare.parse::<IpAddr>() {
            Ok(ip) if ip.is_loopback() => None,
            Ok(_) => return Err(EndpointError::NotLoopback(host.to_owned())),
            Err(_) if host.eq_ignore_ascii_case("localhost") => Some((
                host.to_owned(),
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
            )),
            Err(_) => return Err(EndpointError::NotLoopback(host.to_owned())),
        };

        let joined = format!("{}/chat/completions", base.as_str().trim_end_matches('/'));
        let chat_url = Url::parse(&joined).map_err(|_| invalid())?;
        Ok(Self {
            chat_url,
            model: model.into(),
            pinned,
        })
    }

    pub fn chat_url(&self) -> &Url {
        &self.chat_url
    }

    pub fn model(&self) -> &str {
        &self.model
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_loopback_addresses() {
        for url in [
            "http://127.0.0.1:8080/v1",
            "http://127.0.0.1:8080/v1/",
            "http://127.8.9.10:1234/v1",
            "http://[::1]:8080/v1",
            "http://localhost:11434/v1",
        ] {
            let e = Endpoint::loopback(url, "m").unwrap_or_else(|e| panic!("{url}: {e}"));
            assert!(
                e.chat_url().as_str().ends_with("/v1/chat/completions"),
                "{url}"
            );
        }
    }

    #[test]
    fn localhost_is_pinned_to_127_0_0_1() {
        let e = Endpoint::loopback("http://localhost:11434/v1", "m").unwrap();
        let (host, addr) = e.pinned.unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(addr.to_string(), "127.0.0.1:11434");
    }

    #[test]
    fn refuses_anything_that_is_not_loopback() {
        for url in [
            "http://192.168.1.10:8080/v1",
            "http://10.0.0.1/v1",
            "http://0.0.0.0:8080/v1",
            "https://api.openai.com/v1",
            "http://localhost.example.com/v1",
            "http://127.0.0.1.example.com/v1",
            "http://[2001:db8::1]:8080/v1",
        ] {
            assert!(
                matches!(
                    Endpoint::loopback(url, "m"),
                    Err(EndpointError::NotLoopback(_))
                ),
                "{url} must be refused"
            );
        }
    }

    #[test]
    fn refuses_credentials_and_odd_schemes() {
        assert_eq!(
            Endpoint::loopback("http://user:pw@127.0.0.1:8080/v1", "m").unwrap_err(),
            EndpointError::Credentials
        );
        assert!(matches!(
            Endpoint::loopback("file:///tmp/x", "m"),
            Err(EndpointError::Invalid(_))
        ));
        assert!(matches!(
            Endpoint::loopback("not a url", "m"),
            Err(EndpointError::Invalid(_))
        ));
    }
}
