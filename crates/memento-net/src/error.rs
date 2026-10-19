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

//! Structured errors for the HTTP/networking core.
//!
//! This is a hand-written stand-in for a `thiserror` derive: one enum, a
//! manual `Error`/`Display` impl, no extra dependencies.

use std::fmt;
use std::io;

/// Error raised by the blocking HTTP/1.1 client layer.
///
/// Body-level framing failures that happen while a `Response` is being read
/// surface as [`std::io::Error`] (kinds `UnexpectedEof` /
/// `InvalidData`) rather than this enum, because at that point the error is
/// indistinguishable from a problem on a caller-provided sink.
#[derive(Debug)]
#[non_exhaustive]
pub enum HttpError
{
    /// The URL did not parse or is outside the crate's supported subset
    /// (e.g. missing an `http`/`https` scheme, userinfo present, bad port).
    Url(String),

    /// A custom [`crate::http::Transport`] cannot connect to this HTTPS URL.
    TlsUnsupported,
    /// A socket-level failure: connection refused/reset, DNS resolution,
    /// read/write timeouts (`WouldBlock`/`TimedOut`), or EOF in the wrong
    /// place.
    Io(io::Error),

    /// The peer spoke something other than the HTTP/1.1 dialect this client
    /// accepts: malformed status line, bad header tokens, unsupported
    /// transfer-encoding, conflicting content-length, invalid chunk framing.
    Protocol(String),

    /// More redirects than the configured limit were requested.
    TooManyRedirects
    {
        /// The limit that was hit.
        limit: usize,
    },

    /// A redirect target that was already visited in this exchange
    /// (a redirect cycle).
    RedirectLoop,
}

impl HttpError
{
    /// True when the error corresponds to a read/write/connect timeout as it
    /// would surface through `QNetworkReply::TimeoutError` in the C++ code.
    pub fn is_timeout(&self) -> bool
    {
        match self
        {
            Self::Io(err) => {
                matches!(
                    err.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                )
            }
            _ => false,
        }
    }

    /// True when the error is transport-level (`Url`, `TlsUnsupported`, `Io`,
    /// too many redirects, redirect loop) rather than a protocol/shape error.
    pub fn is_transport(&self) -> bool
    {
        !matches!(self, Self::Protocol(_))
    }
}

impl fmt::Display for HttpError
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Url(why) => write!(f, "invalid URL: {why}"),
            Self::TlsUnsupported => write!(
                f,
                "https is not supported: no TLS transport configured"
            ),
            Self::Io(err) => write!(f, "network error: {err}"),
            Self::Protocol(why) => write!(f, "protocol error: {why}"),
            Self::TooManyRedirects { limit } => {
                write!(f, "redirected more than {limit} times")
            }
            Self::RedirectLoop => write!(f, "redirect cycle detected"),
        }
    }
}

impl std::error::Error for HttpError
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)>
    {
        match self
        {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for HttpError
{
    fn from(err: io::Error) -> Self
    {
        Self::Io(err)
    }
}

pub type HttpResult<T> = Result<T, HttpError>;

#[cfg(test)]
mod tests
{
    use super::*;

    #[test]
    fn timeout_classification()
    {
        let would_block = HttpError::Io(io::Error::new(
            io::ErrorKind::WouldBlock,
            "simulated read timeout",
        ));
        assert!(would_block.is_timeout());
        assert!(would_block.is_transport());

        let timed_out = HttpError::Io(io::Error::new(
            io::ErrorKind::TimedOut,
            "simulated connect timeout",
        ));
        assert!(timed_out.is_timeout());

        let refused = HttpError::Io(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            "no server",
        ));
        assert!(!refused.is_timeout());

        assert!(!HttpError::Protocol("bad chunk size".to_owned()).is_timeout());
        assert!(
            !HttpError::RedirectLoop.is_timeout(),
            "redirect bookkeeping errors are not timeouts"
        );
    }

    #[test]
    fn transport_classification_is_protocol_aware()
    {
        assert!(HttpError::Io(io::Error::new(
            io::ErrorKind::Other,
            "oops"
        ))
        .is_transport());
        assert!(HttpError::Url("missing scheme".into()).is_transport());
        assert!(HttpError::TlsUnsupported.is_transport());
        assert!(!HttpError::Protocol("malformed status line".to_owned()).is_transport());
    }

    #[test]
    fn display_is_stable()
    {
        assert_eq!(
            HttpError::TooManyRedirects { limit: 5 }.to_string(),
            "redirected more than 5 times"
        );
        assert_eq!(HttpError::RedirectLoop.to_string(), "redirect cycle detected");
        assert_eq!(
            HttpError::Protocol("bad chunk size".to_owned()).to_string(),
            "protocol error: bad chunk size"
        );
        assert_eq!(
            HttpError::TlsUnsupported.to_string(),
            "https is not supported: no TLS transport configured"
        );
    }
}
