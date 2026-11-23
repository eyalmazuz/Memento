//! HTTP response wrapper over `ureq::http::Response<ureq::Body>`.

use std::io::{self, Read};

use crate::error::{HttpError, HttpResult};

use super::headers::Headers;

fn validate_content_encoding(headers: &Headers) -> HttpResult<()>
{
    for value in headers.get_all("content-encoding")
    {
        for coding in value.split(',')
        {
            let coding = coding.trim();
            if !coding.is_empty() && !coding.eq_ignore_ascii_case("identity")
            {
                return Err(HttpError::Protocol(format!(
                    "unsupported content-encoding: {coding}"
                )));
            }
        }
    }
    Ok(())
}

fn normalize_body_io_error(err: io::Error) -> io::Error
{
    if let Some(ureq_err) = err.get_ref().and_then(|e| e.downcast_ref::<ureq::Error>())
    {
        match ureq_err
        {
            ureq::Error::Timeout(_) => {
                return io::Error::new(io::ErrorKind::TimedOut, err.to_string());
            }
            ureq::Error::Protocol(_) => {
                return io::Error::new(io::ErrorKind::InvalidData, err.to_string());
            }
            _ => {}
        }
    }
    err
}

/// A parsed HTTP response with streaming body access.
pub struct Response
{
    minor: u8,
    status: u16,
    reason: String,
    headers: Headers,
    content_length: Option<u64>,
    keep_alive: bool,
    body: ureq::BodyReader<'static>,
    consumed: u64,
}

impl std::fmt::Debug for Response
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result
    {
        f.debug_struct("Response")
            .field("minor", &self.minor)
            .field("status", &self.status)
            .field("reason", &self.reason)
            .field("headers", &self.headers)
            .field("content_length", &self.content_length)
            .field("keep_alive", &self.keep_alive)
            .finish()
    }
}

impl Response
{
    pub(crate) fn from_ureq(
        raw: ureq::http::Response<ureq::Body>,
        is_head_request: bool,
    ) -> HttpResult<Self>
    {
        let status = raw.status().as_u16();
        let reason = raw.status().canonical_reason().unwrap_or("").to_owned();
        let minor = match raw.version()
        {
            ureq::http::Version::HTTP_10 => 0,
            _ => 1,
        };

        let mut headers = Headers::new();
        for (name, value) in raw.headers().iter()
        {
            let value_str = match value.to_str()
            {
                Ok(s) => s.to_owned(),
                Err(_) => String::from_utf8_lossy(value.as_bytes()).into_owned(),
            };
            headers.insert(name.as_str(), value_str);
        }

        if !is_head_request && status != 204 && status != 304
        {
            validate_content_encoding(&headers)?;
        }

        let no_body = is_head_request || (100..200).contains(&status) || status == 204 || status == 304;
        let content_length = if no_body
        {
            Some(0)
        }
        else if let Some(len) = raw.body().content_length()
        {
            Some(len)
        }
        else if !headers.contains("transfer-encoding")
        {
            headers
                .get("content-length")
                .and_then(|v| v.trim().parse::<u64>().ok())
        }
        else
        {
            None
        };

        let peer_close = headers.contains_token("connection", "close");
        let peer_keep = headers.contains_token("connection", "keep-alive");
        let is_close_delimited = content_length.is_none() && !headers.contains("transfer-encoding");
        let keep_alive = if is_close_delimited || peer_close
        {
            false
        }
        else if peer_keep
        {
            true
        }
        else
        {
            minor >= 1
        };

        let body = raw.into_body().into_reader();

        Ok(Self {
            minor,
            status,
            reason,
            headers,
            content_length,
            keep_alive,
            body,
            consumed: 0,
        })
    }

    pub fn status(&self) -> u16
    {
        self.status
    }

    pub fn reason(&self) -> &str
    {
        &self.reason
    }

    pub fn minor_version(&self) -> u8
    {
        self.minor
    }

    pub fn headers(&self) -> &Headers
    {
        &self.headers
    }

    /// First header value for `name`, case-insensitive.
    pub fn header(&self, name: &str) -> Option<&str>
    {
        self.headers.get(name)
    }

    /// Whether the server indicated the connection may be kept alive.
    pub fn keep_alive(&self) -> bool
    {
        self.keep_alive
    }

    pub fn content_length(&self) -> Option<u64>
    {
        self.content_length
    }

    /// Bytes of the body consumed so far.
    pub fn consumed(&self) -> u64
    {
        self.consumed
    }

    /// Streams the whole body through `sink`, invoking it once per decoded
    /// chunk. Returns the total number of bytes passed through.
    pub fn stream_chunks(
        &mut self,
        sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> io::Result<u64>
    {
        let mut buffer = vec![0u8; 64 * 1024];
        let mut total = 0u64;
        loop
        {
            let n = self.read(&mut buffer)?;
            if n == 0
            {
                return Ok(total);
            }
            sink(&buffer[..n])?;
            total += n as u64;
        }
    }

    /// Collects the whole body, bounded by `max_bytes`.
    pub fn read_all(&mut self, max_bytes: u64) -> io::Result<Vec<u8>>
    {
        let mut out = Vec::new();
        let mut buffer = vec![0u8; 16 * 1024];
        loop
        {
            let n = self.read(&mut buffer)?;
            if n == 0
            {
                return Ok(out);
            }
            if (out.len() as u64).saturating_add(n as u64) > max_bytes
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "response body exceeds the size limit",
                ));
            }
            out.extend_from_slice(&buffer[..n]);
        }
    }
}

impl Read for Response
{
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize>
    {
        let n = self.body.read(out).map_err(normalize_body_io_error)?;
        self.consumed = self.consumed.saturating_add(n as u64);
        Ok(n)
    }
}
