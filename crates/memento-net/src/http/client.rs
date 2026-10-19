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

//! The blocking HTTP/1.1 client: fixed request canonicalization, redirect
//! following with loop detection, and the byte-level streaming hook.
//!
//! TLS is provided by rustls using the Mozilla WebPKI root set, with normal
//! certificate-chain and server-name validation. HTTPS is never retried as
//! plaintext. The default transport handles both HTTP and HTTPS; custom
//! [`Transport`] implementations remain available for other connection
//! policies.

use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

use crate::error::{HttpError, HttpResult};

use super::request::{Method, Request};
use super::response::Response;
use super::url::Url;

/// A duplex socket the HTTP codec runs over.
pub trait SingleStream: Read + Write {}
impl<T: Read + Write> SingleStream for T {}

/// Connection factory. Custom transports can replace the built-in TCP plus
/// rustls transport when a different network policy is required.
pub trait Transport
{
    /// Opens a stream to `url`'s authority. `io_timeout` (when set) applies
    /// to both the TCP connect and every subsequent read/write.
    fn connect(&self, url: &Url, io_timeout: Option<Duration>)
        -> HttpResult<Box<dyn SingleStream>>;
}

/// The default transport: TCP with rustls for HTTPS and validated WebPKI roots.
pub struct TcpTransport
{
    connect_timeout: Option<Duration>,
}

impl TcpTransport
{
    pub fn new(connect_timeout: Option<Duration>) -> Self
    {
        Self { connect_timeout }
    }
}

fn tls_config() -> Arc<ClientConfig> {
    static CLIENT_CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    Arc::clone(CLIENT_CONFIG.get_or_init(|| {
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        Arc::new(
            ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth(),
        )
    }))
}

fn server_name(host: &str) -> HttpResult<ServerName<'static>> {
    if let Ok(address) = host.parse::<IpAddr>() {
        return Ok(ServerName::IpAddress(address.into()));
    }
    ServerName::try_from(host.to_owned())
        .map_err(|err| HttpError::Url(format!("invalid TLS server name {host:?}: {err}")))
}

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

impl Transport for TcpTransport
{
    fn connect(&self, url: &Url, io_timeout: Option<Duration>) -> HttpResult<Box<dyn SingleStream>>
    {
        let port = url.port_or_default();
        let addrs: Vec<SocketAddr> = (url.host.as_str(), port)
            .to_socket_addrs()
            .map_err(HttpError::Io)?
            .collect();
        if addrs.is_empty()
        {
            return Err(HttpError::Io(std::io::Error::new(
                std::io::ErrorKind::AddrNotAvailable,
                format!("could not resolve {}:{}", url.host, port),
            )));
        }

        let mut last_err: Option<std::io::Error> = None;
        for addr in &addrs
        {
            let timeout = effective_connect_timeout(self.connect_timeout, io_timeout);
            let attempt = match timeout
            {
                Some(timeout) => TcpStream::connect_timeout(addr, timeout),
                None => TcpStream::connect(addr),
            };
            match attempt
            {
                Ok(stream) => {
                    let _ = stream.set_nodelay(true);
                    if let Some(timeout) = io_timeout
                    {
                        stream
                            .set_read_timeout(Some(timeout))
                            .map_err(HttpError::Io)?;
                        stream
                            .set_write_timeout(Some(timeout))
                            .map_err(HttpError::Io)?;
                    }
                    return match url.scheme
                    {
                        super::url::Scheme::Http => Ok(Box::new(stream)),
                        super::url::Scheme::Https => {
                            let server_name = server_name(&url.host)?;
                            let connection = ClientConnection::new(tls_config(), server_name)
                                .map_err(std::io::Error::other)
                                .map_err(HttpError::Io)?;
                            Ok(Box::new(StreamOwned::new(connection, stream)))
                        }
                    };
                }
                Err(err) => last_err = Some(err),
            }
        }
        Err(HttpError::Io(last_err.unwrap_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::Other, "connect failed")
        })))
    }
}

#[derive(Clone, Debug)]
pub struct HttpClientConfig
{
    /// Per-operation read/write/connect cap. `None` (the default) blocks
    /// forever, mirroring the un-timed download path in
    /// `DownloadManager::download`; the AnkiConnect constant `5000 ms` from
    /// `AnkiClient` lives in the `anki` module, not here.
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
            timeout: None,
            connect_timeout: None,
            follow_redirects: true,
            max_redirects: 5,
            max_header_bytes: 64 * 1024,
        }
    }
}

pub struct HttpClient
{
    config: HttpClientConfig,
    transport: Box<dyn Transport>,
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
        let transport = TcpTransport::new(config.connect_timeout);
        Self::with_transport(config, Box::new(transport))
    }

    /// Installs a custom transport, for example when an application needs
    /// a proxy or a platform-specific connection policy.
    pub fn with_transport(config: HttpClientConfig, transport: Box<dyn Transport>) -> Self
    {
        Self { config, transport }
    }

    pub fn config(&self) -> &HttpClientConfig
    {
        &self.config
    }

    pub fn transport(&self) -> &dyn Transport
    {
        self.transport.as_ref()
    }

    /// Sends a request, following redirects (301/302 and non-HEAD 303 → GET,
    /// 307/308 → same method/body) within the configured limits. Loop
    /// detection includes the initial request target.
    pub fn send(&self, request: &Request) -> HttpResult<Response>
    {
        let mut current = request.clone();
        let mut visited: Vec<String> = vec![current.url.key()];
        let mut redirects = 0usize;

        loop
        {
            let response = self.send_once(&current)?;
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
            // The response owns the previous connection; drop it before the
            // next exchange.
            let next_url = current.url.resolve_relative(&location)?;
            let next_key = next_url.key();
            if visited.contains(&next_key)
            {
                return Err(HttpError::RedirectLoop);
            }
            visited.push(next_key);

            let method = if response.status() == 303 && current.method != Method::Head
            {
                Method::Get
            }
            else if (current.method == Method::Post || current.method == Method::Put)
                && (response.status() == 301 || response.status() == 302)
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
            drop(response);

            current = Request {
                method,
                url: next_url,
                headers: current.headers.clone(),
                body,
            };
        }
    }

    /// One request/response exchange, no redirects.
    pub fn send_once(&self, request: &Request) -> HttpResult<Response>
    {
        let wire = request.canonical_wire()?;
        let mut stream = self
            .transport
            .connect(&request.url, self.config.timeout)?;
        stream.write_all(&wire).map_err(HttpError::Io)?;
        stream.flush().map_err(HttpError::Io)?;
        let is_head = request.method == Method::Head;
        Response::decode(
            Box::new(Conn(stream)),
            self.config.max_header_bytes,
            is_head,
        )
    }

    const fn is_redirect(status: u16) -> bool
    {
        matches!(status, 301 | 302 | 303 | 307 | 308)
    }
}

/// Adapts the transport's boxed stream into the reader `Response::decode`
/// consumes. Needed because two different trait objects
/// (`Box<dyn SingleStream>` → `Box<dyn Read>`) do not coerce; this is the
/// delegation, no unsafe.
struct Conn(Box<dyn SingleStream>);

impl Read for Conn
{
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize>
    {
        self.0.read(out)
    }
}

#[cfg(test)]
mod tests
{
    use super::*;

    #[test]
    fn defaults_are_conservative()
    {
        let config = HttpClientConfig::default();
        assert!(config.follow_redirects);
        assert_eq!(config.max_redirects, 5);
        assert!(config.connect_timeout.is_none());
        assert!(config.timeout.is_none());
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
        assert_eq!(effective_connect_timeout(Some(long), Some(short)), Some(short));
        assert_eq!(effective_connect_timeout(Some(short), Some(long)), Some(short));
        assert_eq!(effective_connect_timeout(Some(long), None), Some(long));
        assert_eq!(effective_connect_timeout(None, Some(short)), Some(short));
    }
}
