//! Thin `http`/`https` URL wrapper backed by the `url` crate.

use std::fmt;

pub use crate::error::{HttpError, HttpResult};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme
{
    Http,
    Https,
}

impl Scheme
{
    pub const fn default_port(self) -> u16
    {
        match self
        {
            Self::Http => 80,
            Self::Https => 443,
        }
    }
}

/// A parsed absolute `http` or `https` URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url
{
    inner: ::url::Url,
    scheme: Scheme,
}

impl Url
{
    pub fn parse(input: &str) -> HttpResult<Self>
    {
        // Reject raw control bytes / whitespace before WHATWG parsing, which
        // would otherwise silently strip ASCII tabs and newlines.
        if input.chars().any(|c| c.is_control() || c.is_whitespace())
        {
            return Err(HttpError::Url(
                "URL contains whitespace or control bytes".into(),
            ));
        }

        let parsed = ::url::Url::parse(input)
            .map_err(|err| HttpError::Url(format!("{err}: {input:?}")))?;
        Self::from_parsed(parsed)
    }

    fn from_parsed(mut parsed: ::url::Url) -> HttpResult<Self>
    {
        let scheme = match parsed.scheme()
        {
            "http" => Scheme::Http,
            "https" => Scheme::Https,
            other => {
                return Err(HttpError::Url(format!(
                    "unsupported scheme {other:?} (expected http or https)"
                )));
            }
        };

        if !parsed.username().is_empty() || parsed.password().is_some()
        {
            return Err(HttpError::Url("userinfo in URL is not supported".into()));
        }
        if parsed.host().is_none()
        {
            return Err(HttpError::Url("empty host".into()));
        }

        parsed.set_fragment(None);
        Ok(Self {
            inner: parsed,
            scheme,
        })
    }

    pub const fn scheme(&self) -> Scheme
    {
        self.scheme
    }

    pub const fn scheme_str(&self) -> &'static str
    {
        match self.scheme
        {
            Scheme::Http => "http",
            Scheme::Https => "https",
        }
    }

    pub fn as_str(&self) -> &str
    {
        self.inner.as_str()
    }

    pub fn as_url(&self) -> &::url::Url
    {
        &self.inner
    }

    pub fn host(&self) -> ::url::Host<&str>
    {
        self.inner
            .host()
            .expect("validated http/https URL always has a host")
    }

    pub fn host_str(&self) -> &str
    {
        self.inner
            .host_str()
            .expect("validated http/https URL always has a host")
    }

    pub fn port(&self) -> Option<u16>
    {
        self.inner.port()
    }

    pub fn port_or_default(&self) -> u16
    {
        self.inner
            .port_or_known_default()
            .unwrap_or_else(|| self.scheme.default_port())
    }

    /// Value of the `Host` header for this URL: IPv6 literals include brackets
    /// and non-default ports are written explicitly.
    pub fn host_header(&self) -> String
    {
        let host = self.host_str();
        match self.inner.port()
        {
            Some(port) if port != self.scheme.default_port() => format!("{host}:{port}"),
            _ => host.to_owned(),
        }
    }

    pub fn path_query(&self) -> String
    {
        match self.inner.query()
        {
            Some(q) => format!("{}?{q}", self.inner.path()),
            None => self.inner.path().to_owned(),
        }
    }

    /// Stable identity used for redirect loop detection and key comparisons.
    pub fn key(&self) -> String
    {
        format!(
            "{}://{}:{}{}",
            self.scheme_str(),
            self.host_str().to_ascii_lowercase(),
            self.port_or_default(),
            self.path_query(),
        )
    }

    /// Resolves a `Location` header value against this URL (absolute URLs,
    /// scheme-relative `//host/path`, absolute paths `/x`, query-only `?x`,
    /// and relative `x`/`../x` references).
    pub fn resolve_relative(&self, location: &str) -> HttpResult<Self>
    {
        if location.chars().any(|c| c.is_control())
        {
            return Err(HttpError::Url(
                "redirect location contains control bytes".into(),
            ));
        }
        let location = location.trim();
        if location.chars().any(|c| c.is_whitespace())
        {
            return Err(HttpError::Url(
                "redirect location contains whitespace".into(),
            ));
        }
        let joined = self
            .inner
            .join(location)
            .map_err(|err| HttpError::Url(format!("invalid redirect location {location:?}: {err}")))?;
        Self::from_parsed(joined)
    }
}

impl fmt::Display for Url
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        f.write_str(self.inner.as_str())
    }
}

#[cfg(test)]
mod tests
{
    use super::*;

    #[test]
    fn authority_rejects_control_chars_nul_and_whitespace()
    {
        for bad in [
            "http://example.com\r\nX-Injected: yes/path",
            "https://example.com\n/path",
            "http://example.com\0/path",
            "http://example.com\t/path",
            "http://example .com/path",
            "http://[::1\r\n]/path",
            "http://[not-ipv6]/path",
        ]
        {
            assert!(
                Url::parse(bad).is_err(),
                "expected authority rejection for {bad:?}"
            );
        }
    }

    #[test]
    fn resolve_relative_strips_base_query_before_finding_parent_dir()
    {
        let base = Url::parse("https://example.com/dir/page?next=/other/sub").unwrap();
        let resolved = base.resolve_relative("rel.html").unwrap();
        assert_eq!(resolved.path_query(), "/dir/rel.html");

        let resolved_with_query = base.resolve_relative("../item.bin?from=/a/b").unwrap();
        assert_eq!(resolved_with_query.path_query(), "/item.bin?from=/a/b");
    }

    #[test]
    fn rejects_userinfo_and_non_http_schemes()
    {
        assert!(Url::parse("http://user:pass@example.com/").is_err());
        assert!(Url::parse("ftp://example.com/file").is_err());
        assert!(Url::parse("example.com/no-scheme").is_err());
    }
}
