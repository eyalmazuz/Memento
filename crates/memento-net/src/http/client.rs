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

//! Blocking HTTP/1.1 client backed by `ureq` (with rustls + WebPKI roots),
//! enforcing redirect loop detection, HTTPS downgrade rejection, cross-origin
//! credential stripping, and SSRF redirect guards.

use std::io;
use std::net::{IpAddr, ToSocketAddrs};
use std::time::Duration;

use crate::error::{HttpError, HttpResult};

use super::headers::Headers;
use super::request::{Method, Request};
use super::response::Response;
use super::url::{Scheme, Url};

/// Default TCP/TLS connect timeout when none is specified.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Default transfer timeout when none is specified.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

fn effective_connect_timeout(
    connect_timeout: Option<Duration>,
    io_timeout: Option<Duration>,
) -> Option<Duration>
{
    match (connect_timeout, io_timeout)
    {
        (Some(connect), Some(io)) => Some(connect.min(io)),
        (Some(connect), None) => Some(connect),
        (None, io) => io,
    }
}

#[derive(Clone, Debug)]
pub struct HttpClientConfig
{
    /// Per-phase request/response/body timeout cap.
    pub timeout: Option<Duration>,
    pub connect_timeout: Option<Duration>,
    pub follow_redirects: bool,
    pub max_redirects: usize,
    /// Cap for a response's header block.
    pub max_header_bytes: usize,
}

impl Default for HttpClientConfig
{
    fn default() -> Self
    {
        Self {
            timeout: Some(DEFAULT_TIMEOUT),
            connect_timeout: Some(DEFAULT_CONNECT_TIMEOUT),
            follow_redirects: true,
            max_redirects: 5,
            max_header_bytes: 64 * 1024,
        }
    }
}

pub struct HttpClient
{
    config: HttpClientConfig,
    agent: ureq::Agent,
}

impl Default for HttpClient
{
    fn default() -> Self
    {
        Self::new(HttpClientConfig::default())
    }
}

impl HttpClient
{
    pub fn new(config: HttpClientConfig) -> Self
    {
        let timeout = config.timeout;
        Self::with_phase_timeouts(config, timeout, timeout)
    }

    pub(crate) fn with_phase_timeouts(
        config: HttpClientConfig,
        response_timeout: Option<Duration>,
        body_timeout: Option<Duration>,
    ) -> Self
    {
        let connect_timeout =
            effective_connect_timeout(config.connect_timeout, response_timeout.or(config.timeout));
        let ureq_config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .max_response_header_size(config.max_header_bytes)
            .max_idle_connections(0)
            .accept_encoding("identity")
            .user_agent(ureq::config::AutoHeaderValue::None)
            .accept(ureq::config::AutoHeaderValue::None)
            .timeout_connect(connect_timeout)
            .timeout_send_request(response_timeout)
            .timeout_send_body(body_timeout)
            .timeout_recv_response(response_timeout)
            .timeout_recv_body(body_timeout)
            .build();
        let agent = ureq::Agent::new_with_config(ureq_config);
        Self { config, agent }
    }

    pub fn config(&self) -> &HttpClientConfig
    {
        &self.config
    }

    /// Sends a request, following redirects (301/302 and non-HEAD 303 -> GET,
    /// 307/308 -> same method/body) within the configured limits. Loop
    /// detection includes the initial request target.
    pub fn send(&self, request: &Request) -> HttpResult<Response>
    {
        self.send_with_timeout(request, None)
    }

    /// Sends a request using an explicit per-request timeout override instead
    /// of the client's default transfer timeout.
    pub fn send_with_request_timeout(
        &self,
        request: &Request,
        timeout: Duration,
    ) -> HttpResult<Response>
    {
        self.send_with_timeout(request, Some(Some(timeout)))
    }

    pub(crate) fn send_with_timeout(
        &self,
        request: &Request,
        timeout_override: Option<Option<Duration>>,
    ) -> HttpResult<Response>
    {
        let mut current = request.clone();
        let started_remote = !is_local_or_loopback_url(&current.url);
        let mut visited: Vec<String> = vec![current.url.key()];
        let mut redirects = 0usize;

        loop
        {
            let response = self.send_once_with_timeout(&current, timeout_override)?;
            if !Self::is_redirect(response.status()) || !self.config.follow_redirects
            {
                return Ok(response);
            }

            redirects += 1;
            if redirects > self.config.max_redirects
            {
                return Err(HttpError::TooManyRedirects {
                    limit: self.config.max_redirects,
                });
            }

            let location = response
                .header("location")
                .ok_or_else(|| HttpError::Protocol("redirect without Location".into()))?
                .to_owned();
            let status = response.status();
            drop(response);

            current = prepare_redirect(&current, status, &location, started_remote, &mut visited)?;
        }
    }

    /// One request/response exchange, no redirects.
    pub fn send_once(&self, request: &Request) -> HttpResult<Response>
    {
        self.send_once_with_timeout(request, None)
    }

    fn send_once_with_timeout(
        &self,
        request: &Request,
        timeout_override: Option<Option<Duration>>,
    ) -> HttpResult<Response>
    {
        let mut builder = ureq::http::Request::builder()
            .method(request.method.to_http())
            .uri(request.url.as_str());

        for (name, value) in request.headers.iter()
        {
            if name.eq_ignore_ascii_case("host") && value != request.url.host_header()
            {
                return Err(HttpError::Protocol(format!(
                    "Host header {value:?} does not match URL host {:?}",
                    request.url.host_header()
                )));
            }
            if name.eq_ignore_ascii_case("content-length")
            {
                let declared: usize = value
                    .parse()
                    .map_err(|_| HttpError::Protocol("bad user Content-Length".into()))?;
                if Some(declared) != request.body.as_ref().map(Vec::len)
                {
                    return Err(HttpError::Protocol(
                        "user Content-Length does not match body length".into(),
                    ));
                }
            }
            builder = builder.header(name, value);
        }

        if !request.headers.contains("connection")
        {
            builder = builder.header("Connection", "keep-alive");
        }

        let is_head = request.method == Method::Head;
        match request.body.as_deref()
        {
            Some(bytes) => self.run_http_request(builder, bytes, timeout_override, is_head),
            None => self.run_http_request(builder, (), timeout_override, is_head),
        }
    }

    fn run_http_request(
        &self,
        builder: ureq::http::request::Builder,
        body: impl ureq::AsSendBody,
        timeout_override: Option<Option<Duration>>,
        is_head: bool,
    ) -> HttpResult<Response>
    {
        let mut http_req = builder
            .body(body)
            .map_err(|err| HttpError::Protocol(err.to_string()))?;

        if let Some(override_timeout) = timeout_override
        {
            let connect =
                effective_connect_timeout(self.config.connect_timeout, override_timeout);
            http_req = self
                .agent
                .configure_request(http_req)
                .timeout_connect(connect)
                .timeout_send_request(override_timeout)
                .timeout_send_body(override_timeout)
                .timeout_recv_response(override_timeout)
                .timeout_recv_body(override_timeout)
                .build();
        }

        let raw_resp = self
            .agent
            .run(http_req)
            .map_err(|err| self.map_ureq_error(err))?;

        Response::from_ureq(raw_resp, is_head)
    }

    fn map_ureq_error(&self, err: ureq::Error) -> HttpError
    {
        match err
        {
            ureq::Error::Io(io_err) => HttpError::Io(io_err),
            ureq::Error::Timeout(timeout) => HttpError::Io(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("request timed out ({timeout})"),
            )),
            ureq::Error::HostNotFound => HttpError::Io(io::Error::new(
                io::ErrorKind::NotFound,
                "host not found",
            )),
            ureq::Error::ConnectionFailed => HttpError::Io(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                "connection failed",
            )),
            ureq::Error::ConnectProxyFailed(msg) => {
                HttpError::Io(io::Error::other(format!("CONNECT proxy failed: {msg}")))
            }
            ureq::Error::Rustls(tls_err) => {
                HttpError::Io(io::Error::other(format!("TLS error: {tls_err}")))
            }
            ureq::Error::Tls(msg) => HttpError::Io(io::Error::other(format!("TLS error: {msg}"))),
            ureq::Error::Pem(pem_err) => {
                HttpError::Io(io::Error::other(format!("TLS PEM error: {pem_err:?}")))
            }
            ureq::Error::TlsRequired => HttpError::TlsUnsupported,
            ureq::Error::BadUri(msg) | ureq::Error::RequireHttpsOnly(msg) => HttpError::Url(msg),
            ureq::Error::InvalidProxyUrl => HttpError::Url("invalid proxy URL".to_owned()),
            ureq::Error::LargeResponseHeader(got, limit) => HttpError::Protocol(format!(
                "header block exceeds size limit ({got} > {limit})"
            )),
            ureq::Error::Protocol(proto) => {
                let msg = proto.to_string();
                if msg.contains("http parse fail")
                {
                    HttpError::Protocol(format!("malformed status line or headers ({msg})"))
                }
                else
                {
                    HttpError::Protocol(msg)
                }
            }
            ureq::Error::Http(http_err) => HttpError::Protocol(http_err.to_string()),
            ureq::Error::TooManyRedirects => HttpError::TooManyRedirects {
                limit: self.config.max_redirects,
            },
            ureq::Error::RedirectFailed => HttpError::Protocol("redirect failed".to_owned()),
            ureq::Error::BodyExceedsLimit(limit) => {
                HttpError::Protocol(format!("body exceeds size limit ({limit})"))
            }
            ureq::Error::StatusCode(code) => {
                HttpError::Protocol(format!("unexpected HTTP status {code}"))
            }
            other => HttpError::Io(io::Error::other(other.to_string())),
        }
    }

    const fn is_redirect(status: u16) -> bool
    {
        matches!(status, 301 | 302 | 303 | 307 | 308)
    }
}

fn prepare_redirect(
    current: &Request,
    status: u16,
    location: &str,
    started_remote: bool,
    visited: &mut Vec<String>,
) -> HttpResult<Request>
{
    let next_url = current.url.resolve_relative(location)?;

    if current.url.scheme() == Scheme::Https && next_url.scheme() == Scheme::Http
    {
        return Err(HttpError::Protocol(
            "refusing HTTPS to HTTP downgrade redirect".into(),
        ));
    }
    if is_link_local_url(&next_url)
        || ((started_remote || !is_local_or_loopback_url(&current.url))
            && is_restricted_redirect_target(&next_url))
    {
        return Err(HttpError::Protocol(format!(
            "refusing redirect to restricted host {:?}",
            next_url.host_str()
        )));
    }

    let next_key = next_url.key();
    if visited.contains(&next_key)
    {
        return Err(HttpError::RedirectLoop);
    }
    visited.push(next_key);

    let method = if status == 303 && current.method != Method::Head
    {
        Method::Get
    }
    else if (current.method == Method::Post || current.method == Method::Put)
        && (status == 301 || status == 302)
    {
        Method::Get
    }
    else
    {
        current.method
    };

    let body = if method == current.method
    {
        current.body.clone()
    }
    else
    {
        None
    };
    let dropped_body = current.body.is_some() && body.is_none();
    let same_origin = same_origin(&current.url, &next_url);

    let mut next_headers = Headers::new();
    for (name, value) in current.headers.iter()
    {
        if name.eq_ignore_ascii_case("host")
        {
            continue;
        }
        if !same_origin
            && (name.eq_ignore_ascii_case("authorization")
                || name.eq_ignore_ascii_case("cookie"))
        {
            continue;
        }
        if dropped_body
            && (name.eq_ignore_ascii_case("content-length")
                || name.eq_ignore_ascii_case("transfer-encoding")
                || name.eq_ignore_ascii_case("content-type"))
        {
            continue;
        }
        next_headers.insert(name, value);
    }

    Ok(Request {
        method,
        url: next_url,
        headers: next_headers,
        body,
    })
}

fn same_origin(left: &Url, right: &Url) -> bool
{
    left.scheme() == right.scheme()
        && left.host_str().eq_ignore_ascii_case(right.host_str())
        && left.port_or_default() == right.port_or_default()
}

fn is_localhost_name(host: &str) -> bool
{
    let normalized = host.trim_end_matches('.').to_ascii_lowercase();
    normalized == "localhost" || normalized.ends_with(".localhost")
}

fn ip_is_loopback_or_unspecified(ip: IpAddr) -> bool
{
    match ip
    {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_unspecified(),
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6
                    .to_ipv4_mapped()
                    .is_some_and(|v4| v4.is_loopback() || v4.is_unspecified())
        }
    }
}

fn ip_is_link_local(ip: IpAddr) -> bool
{
    match ip
    {
        IpAddr::V4(v4) => v4.is_link_local(),
        IpAddr::V6(v6) => {
            (v6.segments()[0] & 0xffc0) == 0xfe80
                || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_link_local())
        }
    }
}

fn is_local_or_loopback_url(url: &Url) -> bool
{
    match url.host()
    {
        ::url::Host::Domain(domain) => is_localhost_name(domain),
        ::url::Host::Ipv4(v4) => ip_is_loopback_or_unspecified(IpAddr::V4(v4)),
        ::url::Host::Ipv6(v6) => ip_is_loopback_or_unspecified(IpAddr::V6(v6)),
    }
}

fn is_link_local_url(url: &Url) -> bool
{
    match url.host()
    {
        ::url::Host::Domain(_) => false,
        ::url::Host::Ipv4(v4) => ip_is_link_local(IpAddr::V4(v4)),
        ::url::Host::Ipv6(v6) => ip_is_link_local(IpAddr::V6(v6)),
    }
}

fn is_restricted_redirect_target(url: &Url) -> bool
{
    if is_local_or_loopback_url(url) || is_link_local_url(url)
    {
        return true;
    }
    if let ::url::Host::Domain(domain) = url.host()
    {
        if let Ok(addrs) = (domain, url.port_or_default()).to_socket_addrs()
        {
            for addr in addrs
            {
                let ip = addr.ip();
                if ip_is_loopback_or_unspecified(ip) || ip_is_link_local(ip)
                {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests
{
    use super::*;

    #[test]
    fn defaults_are_bounded_and_conservative()
    {
        let config = HttpClientConfig::default();
        assert!(config.follow_redirects);
        assert_eq!(config.max_redirects, 5);
        assert_eq!(config.connect_timeout, Some(DEFAULT_CONNECT_TIMEOUT));
        assert_eq!(config.timeout, Some(DEFAULT_TIMEOUT));
        assert_eq!(config.max_header_bytes, 64 * 1024);
    }

    #[test]
    fn redirect_statuses_are_method_sensitive()
    {
        assert!(HttpClient::is_redirect(301));
        assert!(HttpClient::is_redirect(308));
        assert!(!HttpClient::is_redirect(200));
        assert!(!HttpClient::is_redirect(304));
    }

    #[test]
    fn transfer_timeout_caps_tcp_connect_timeout()
    {
        let short = Duration::from_secs(5);
        let long = Duration::from_secs(10);
        assert_eq!(
            effective_connect_timeout(Some(long), Some(short)),
            Some(short)
        );
        assert_eq!(
            effective_connect_timeout(Some(short), Some(long)),
            Some(short)
        );
        assert_eq!(effective_connect_timeout(Some(long), None), Some(long));
        assert_eq!(effective_connect_timeout(None, Some(short)), Some(short));
    }

    #[test]
    fn blocks_https_to_http_downgrade_redirect()
    {
        let req = Request::get(Url::parse("https://example.test/start").unwrap());
        let mut visited = vec![req.url.key()];
        let err = prepare_redirect(
            &req,
            302,
            "http://example.test/insecure",
            true,
            &mut visited,
        )
        .unwrap_err();
        assert!(matches!(err, HttpError::Protocol(_)));
    }

    #[test]
    fn blocks_remote_redirect_to_loopback_and_link_local()
    {
        for target in [
            "http://127.0.0.1:8765/",
            "http://[::1]:8765/",
            "http://[::ffff:127.0.0.1]:8765/",
            "http://localhost:8765/",
            "http://169.254.169.254/latest/meta-data/",
        ]
        {
            let req = Request::get(Url::parse("http://remote.example.test/file").unwrap());
            let mut visited = vec![req.url.key()];
            let err = prepare_redirect(&req, 302, target, true, &mut visited).unwrap_err();
            assert!(
                matches!(err, HttpError::Protocol(_)),
                "expected SSRF redirect block for {target}, got {err:?}"
            );
        }
    }

    #[test]
    fn strips_auth_and_cookie_on_cross_origin_redirect_and_clears_host_and_content_length()
    {
        let req = Request::post(
            Url::parse("https://auth.example.test/submit").unwrap(),
            b"{}".to_vec(),
        )
        .header("Host", "auth.example.test")
        .header("Content-Length", "2")
        .header("Authorization", "Bearer secret")
        .header("Cookie", "session=xyz")
        .header("X-Custom", "keep");

        let mut visited = vec![req.url.key()];
        let next = prepare_redirect(
            &req,
            303,
            "https://other.example.test/done",
            true,
            &mut visited,
        )
        .unwrap();

        assert_eq!(next.method, Method::Get);
        assert!(next.body.is_none());
        assert_eq!(next.url.as_str(), "https://other.example.test/done");
        assert!(!next.headers.contains("Host"));
        assert!(!next.headers.contains("Authorization"));
        assert!(!next.headers.contains("Cookie"));
        assert!(!next.headers.contains("Content-Length"));
        assert_eq!(next.headers.get("X-Custom"), Some("keep"));
    }
}
