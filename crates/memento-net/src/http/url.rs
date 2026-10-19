////////////////////////////////////////////////////////////////////////////////
//
// Copyright (c) 2026 Ripose
//
// This file is part of Memento.
//
// Memento is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, version 2 of the License.
//
// Memento is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with Memento.  If not, see <https://www.gnu.org/licenses/>.
//
////////////////////////////////////////////////////////////////////////////////

//! Smallest URL subset Memento actually speaks: absolute `http`/`https`
//! URLs on AnkiConnect and the model-file hosts, plus relative resolution
//! for `Location` headers during redirects.
//!
//! No percent-decoding of the path happens — the path/query is forwarded to
//! the request target byte-for-byte, so callers must hand over encoded
//! URLs (same contract as `QUrl::toString()` inputs in the C++ code).

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

/// A parsed absolute URL. The host keeps the case handed to it (the C++
/// code never canonicalizes hosts either) and IPv6 literals are stored
/// without brackets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url
{
    pub scheme: Scheme,
    pub host: String,
    host_is_ipv6: bool,
    pub port: Option<u16>,
    /// Path plus query as it will appear in the request target
    /// (e.g. `/foo?bar=baz`). Always starts with `/`; `?`-only URLs are
    /// resolved to `original-path?query`.
    pub path_query: String,
}

impl Url
{
    pub fn parse(input: &str) -> HttpResult<Self>
    {
        let no_fragment = match input.find('#')
        {
            Some(idx) => &input[..idx],
            None => input,
        };

        let (scheme, rest) = if let Some(rest) = no_fragment.strip_prefix("http://")
        {
            (Scheme::Http, rest)
        }
        else if let Some(rest) = no_fragment.strip_prefix("https://")
        {
            (Scheme::Https, rest)
        }
        else
        {
            return Err(HttpError::Url(format!(
                "missing http/https scheme: {input:?}"
            )));
        };

        let authority_end = rest.find(['/', '?']).unwrap_or(rest.len());
        let (authority, target) = rest.split_at(authority_end);
        let host_port = Self::parse_authority(authority)?;

        let path_query = if target.is_empty() || target.starts_with('?')
        {
            // `http://host?x=1` and `http://host?x` have no explicit path;
            // the request target needs one.
            if target.is_empty()
            {
                "/".to_owned()
            }
            else
            {
                format!("/{}", target)
            }
        }
        else
        {
            target.to_owned()
        };
        if path_query.bytes().any(|b| b.is_ascii_control() || b == b' ')
        {
            return Err(HttpError::Url("URL contains spaces or control bytes".into()));
        }

        Ok(Self {
            scheme,
            host: host_port.0,
            host_is_ipv6: host_port.1,
            port: host_port.2,
            path_query,
        })
    }

    fn parse_authority(
        authority: &str,
    ) -> HttpResult<(String, bool, Option<u16>)>
    {
        if authority.is_empty()
        {
            return Err(HttpError::Url("empty host".into()));
        }
        if authority.contains('@')
        {
            return Err(HttpError::Url("userinfo in URL is not supported".into()));
        }

        let (host_part, port_part, ipv6) =
            if let Some(inner) = authority.strip_prefix('[')
            {
                let close = inner
                    .find(']')
                    .ok_or_else(|| HttpError::Url("unterminated IPv6 literal".into()))?;
                let host = &inner[..close];
                let rest = &inner[close + 1..];
                match rest.strip_prefix(':')
                {
                    Some(port) => (host, Some(port), true),
                    None if rest.is_empty() => (host, None, true),
                    None => {
                        return Err(HttpError::Url("junk after IPv6 literal".into()))
                    }
                }
            }
            else
            {
                match authority.rfind(':')
                {
                    Some(idx) => (
                        &authority[..idx],
                        Some(&authority[idx + 1..]),
                        false,
                    ),
                    None => (authority, None, false),
                }
            };

        if host_part.is_empty()
        {
            return Err(HttpError::Url("empty host".into()));
        }

        let port = match port_part
        {
            Some("") => None,
            Some(digits) if digits.bytes().all(|b| b.is_ascii_digit()) => {
                let parsed: u32 = digits
                    .parse()
                    .map_err(|_| HttpError::Url("bad port".into()))?;
                let port = u16::try_from(parsed)
                    .map_err(|_| HttpError::Url("port out of range".into()))?;
                Some(port)
            }
            Some(_) => return Err(HttpError::Url("bad port".into())),
            None => None,
        };

        Ok((host_part.to_owned(), ipv6, port))
    }

    pub fn port_or_default(&self) -> u16
    {
        self.port.unwrap_or_else(|| self.scheme.default_port())
    }

    /// Value of the `Host` header for this URL: IPv6 literals regain their
    /// brackets, and a non-default port is written explicitly (matching
    /// what Qt puts into the `Host` header for `http://host:8765`).
    pub fn host_header(&self) -> String
    {
        let mut value = if self.host_is_ipv6 {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        let default = self.scheme.default_port();
        if let Some(port) = self.port
        {
            if port != default
            {
                value.push(':');
                value.push_str(&port.to_string());
            }
        }
        value
    }

    /// Stable identity used for redirect loop detection and key comparisons.
    pub fn key(&self) -> String
    {
        format!(
            "{}://{}:{}{}",
            self.scheme_str(),
            self.host_header(),
            self.port_or_default(),
            self.path_query
        )
    }

    pub const fn scheme_str(&self) -> &'static str
    {
        match self.scheme
        {
            Scheme::Http => "http",
            Scheme::Https => "https",
        }
    }

    /// Resolves a `Location` header value against this URL, accepting the
    /// forms Memento's model-file hosts emit in practice: absolute URLs,
    /// scheme-relative `//host/path`, absolute paths `/x`, query-only
    /// `?x`, and relative `x`/`../x` references.
    pub fn resolve_relative(&self, location: &str) -> HttpResult<Self>
    {
        let location = location.trim();

        if location.starts_with("http://") || location.starts_with("https://")
        {
            return Self::parse(location);
        }

        if let Some(rest) = location.strip_prefix("//")
        {
            let rebuilt = format!("{}://{}", self.scheme_str(), rest);
            return Self::parse(&rebuilt);
        }

        let rebuilt = if location.starts_with('/')
        {
            format!("{}://{}{}", self.scheme_str(), self.authority_url(), location)
        }
        else if location.starts_with('?')
        {
            let path = self.path_query.split('?').next().unwrap_or("/");
            format!("{}://{}{}{}", self.scheme_str(), self.authority_url(), path, location)
        }
        else
        {
            // Relative reference: merge against the base directory (the
            // path without its last segment), then fold dot segments.
            let base = if let Some(idx) = self.path_query.rfind('/')
            {
                &self.path_query[..=idx]
            }
            else
            {
                "/"
            };
            let merged = remove_dot_segments(&format!("{base}{location}"));
            format!("{}://{}{}", self.scheme_str(), self.authority_url(), merged)
        };

        Self::parse(&rebuilt)
    }

    fn authority_url(&self) -> String
    {
        let mut value = if self.host_is_ipv6 {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        value.push(':');
        value.push_str(&self.port_or_default().to_string());
        value
    }
}

impl fmt::Display for Url
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        write!(
            f,
            "{}://{}{}",
            self.scheme_str(),
            self.authority_url(),
            self.path_query
        )
    }
}

/// Folds `.` and `..` path segments the way RFC 3986 `remove_dot_segments`
/// does (empty segments and slashes are collapsed, `..` beyond the root
/// stops at the root).
fn remove_dot_segments(path: &str) -> String
{
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/')
    {
        match segment
        {
            "." => {}
            ".." => {
                if segments.len() > 1
                {
                    segments.pop();
                }
            }
            other => segments.push(other),
        }
    }
    if segments.is_empty()
    {
        return "/".to_owned();
    }
    segments.join("/")
}
