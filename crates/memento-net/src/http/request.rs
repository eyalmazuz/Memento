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

//! Request model and fixed byte-level canonicalization.
//!
//! Canonical wire shape (deterministic, asserted by tests):
//!
//! ```text
//! {METHOD} {request-target} HTTP/1.1\r\n
//! Host: {host[:port]}\r\n
//! {user headers, insertion order}\r\n
//! Content-Length: {n}\r\n     (only when a body is set and the user did
//!                              not bring their own Content-Length header)
//! Connection: keep-alive\r\n  (only when the user did not set Connection)
//! Accept-Encoding: identity\r\n (only when the user did not set it — this
//!                              crate cannot decompress anything)
//! \r\n
//! {body}
//! ```

use super::headers::Headers;
use super::url::Url;
use crate::error::{HttpError, HttpResult};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Method
{
    Get,
    Head,
    Post,
    Put,
    Delete,
    Patch,
}

impl Method
{
    pub const fn as_str(self) -> &'static str
    {
        match self
        {
            Self::Get => "GET",
            Self::Head => "HEAD",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
            Self::Patch => "PATCH",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request
{
    pub method: Method,
    pub url: Url,
    pub headers: Headers,
    pub body: Option<Vec<u8>>,
}

impl Request
{
    pub fn new(method: Method, url: Url) -> Self
    {
        Self {
            method,
            url,
            headers: Headers::new(),
            body: None,
        }
    }

    pub fn get(url: Url) -> Self
    {
        Self::new(Method::Get, url)
    }

    pub fn post(url: Url, body: Vec<u8>) -> Self
    {
        Self {
            method: Method::Post,
            url,
            headers: Headers::new(),
            body: Some(body),
        }
    }

    /// Adds a user header (validated later, at canonicalization time).
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self
    {
        self.headers.insert(name, value);
        self
    }

    pub fn with_body(mut self, body: Vec<u8>) -> Self
    {
        self.body = Some(body);
        self
    }

    /// The exact bytes to write to the socket. Validation of user headers
    /// happens here so every `HttpClient` (and every test) sees the same
    /// rules.
    pub fn canonical_wire(&self) -> HttpResult<Vec<u8>>
    {
        for (name, value) in self.headers.iter()
        {
            if !super::headers::is_token(name)
            {
                return Err(HttpError::Protocol(format!(
                    "invalid header field name {name:?}"
                )));
            }
            if !super::headers::valid_header_value(value)
            {
                return Err(HttpError::Protocol(format!(
                    "header value for {name} contains forbidden bytes"
                )));
            }
            if name.eq_ignore_ascii_case("host")
            {
                if value != self.url.host_header()
                {
                    return Err(HttpError::Protocol(format!(
                        "Host header {value:?} does not match URL host {:?}",
                        self.url.host_header()
                    )));
                }
            }
            if name.eq_ignore_ascii_case("content-length")
            {
                let declared: usize = value
                    .parse()
                    .map_err(|_| HttpError::Protocol("bad user Content-Length".into()))?;
                if Some(declared) != self.body.as_ref().map(Vec::len)
                {
                    return Err(HttpError::Protocol(
                        "user Content-Length does not match body length".into(),
                    ));
                }
            }
        }

        let target = if self.url.path_query.is_empty()
        {
            "/"
        }
        else
        {
            self.url.path_query.as_str()
        };

        let mut wire = Vec::new();
        wire.extend_from_slice(self.method.as_str().as_bytes());
        wire.push(b' ');
        wire.extend_from_slice(target.as_bytes());
        wire.extend_from_slice(b" HTTP/1.1\r\n");

        let push_header = |wire: &mut Vec<u8>, name: &str, value: &str| {
            wire.extend_from_slice(name.as_bytes());
            wire.extend_from_slice(b": ");
            wire.extend_from_slice(value.as_bytes());
            wire.extend_from_slice(b"\r\n");
        };

        push_header(&mut wire, "Host", &self.url.host_header());
        for (name, value) in self.headers.iter()
        {
            push_header(&mut wire, name, value);
        }

        if self.body.is_some() && !self.headers.contains("content-length")
        {
            push_header(
                &mut wire,
                "Content-Length",
                &self.body.as_ref().unwrap().len().to_string(),
            );
        }
        if !self.headers.contains("connection")
        {
            push_header(&mut wire, "Connection", "keep-alive");
        }
        if !self.headers.contains("accept-encoding")
        {
            push_header(&mut wire, "Accept-Encoding", "identity");
        }

        wire.extend_from_slice(b"\r\n");
        if let Some(body) = &self.body
        {
            wire.extend_from_slice(body);
        }

        Ok(wire)
    }
}

#[cfg(test)]
mod tests
{
    use super::*;

    fn url_for(raw: &str) -> Url
    {
        Url::parse(raw).expect("fixture URL must parse")
    }

    #[test]
    fn canonical_get_has_fixed_shape()
    {
        let request = Request::get(url_for("http://127.0.0.1:8765"));
        let wire = String::from_utf8(request.canonical_wire().unwrap()).unwrap();
        assert_eq!(
            wire,
            "GET / HTTP/1.1\r\n\
             Host: 127.0.0.1:8765\r\n\
             Connection: keep-alive\r\n\
             Accept-Encoding: identity\r\n\
             \r\n"
        );
    }

    #[test]
    fn canonical_post_includes_content_length_and_user_headers_in_order()
    {
        let request = Request::post(url_for("http://127.0.0.1:8765/"), b"{}".to_vec())
            .header("Content-Type", "application/json")
            .header("X-Test", "a");
        let wire = String::from_utf8(request.canonical_wire().unwrap()).unwrap();
        assert_eq!(
            wire,
            "POST / HTTP/1.1\r\n\
             Host: 127.0.0.1:8765\r\n\
             Content-Type: application/json\r\n\
             X-Test: a\r\n\
             Content-Length: 2\r\n\
             Connection: keep-alive\r\n\
             Accept-Encoding: identity\r\n\
             \r\n\
             {}"
        );
    }

    #[test]
    fn canonicalization_preserves_explicit_paths_and_queries()
    {
        let request = Request::get(url_for("http://localhost:8765/sub/dir?p=1"));
        let wire = String::from_utf8(request.canonical_wire().unwrap()).unwrap();
        assert!(
            wire.starts_with("GET /sub/dir?p=1 HTTP/1.1\r\n"),
            "request target must keep path and query verbatim: {wire}"
        );
    }

    #[test]
    fn header_value_injection_is_rejected()
    {
        let request =
            Request::get(url_for("http://127.0.0.1:8765/")).header("X-Evil", "one\r\nX: y");
        assert!(matches!(
            request.canonical_wire(),
            Err(HttpError::Protocol(_))
        ));
    }

    #[test]
    fn non_token_header_name_is_rejected()
    {
        let request = Request::get(url_for("http://127.0.0.1:8765/")).header("Bad Header", "v");
        assert!(matches!(
            request.canonical_wire(),
            Err(HttpError::Protocol(_))
        ));
    }

    #[test]
    fn user_connection_header_wins()
    {
        let request = Request::get(url_for("http://127.0.0.1:8765/"))
            .header("Connection", "close");
        let wire = String::from_utf8(request.canonical_wire().unwrap()).unwrap();
        assert!(wire.contains("Connection: close\r\n"));
        assert!(!wire.contains("Connection: keep-alive"));
    }

    #[test]
    fn mismatched_user_host_is_rejected()
    {
        let request = Request::get(url_for("http://127.0.0.1:8765/"))
            .header("Host", "evil.example.com");
        assert!(matches!(
            request.canonical_wire(),
            Err(HttpError::Protocol(_))
        ));
    }
}
