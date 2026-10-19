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

//! Response head parsing and body decoding.
//!
//! Framing precedence follows RFC 7230 §3.3.3 in the order Memento's
//! endpoints exercise it:
//!
//! 1. `1xx` / `204` / `304` (or a HEAD request) — no body at all;
//! 2. `Transfer-Encoding: chunked` — chunked decoding; chunked combined
//!    with `Content-Length`, or any other transfer coding, is rejected as
//!    a protocol error rather than "smuggled";
//! 3. a single consistent `Content-Length`;
//! 4. no framing declared — the body runs until the peer closes
//!    (`UntilClose`, which also forces `Connection: close` semantics).
//!
//! The decoder owns its connection, so a `Response` can be streamed
//! chunk-by-chunk (the byte-level hook used by `download` for progress
//! callbacks) and a partially consumed response is torn down with its
//! connection when dropped.

use std::io::{self, Read};

use crate::error::{HttpError, HttpResult};

use super::headers::{split_header, Headers};

/// Upper bound for a single status/header/chunk-size/trailer line before it
/// is treated as an attack rather than a header.
pub const MAX_LINE_BYTES: usize = 16 * 1024;

const TE_NOT_CHUNKED: &str = "unsupported transfer-encoding (only chunked is decoded)";
const TE_WITH_CL: &str = "both transfer-encoding and content-length present";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusLine
{
    /// HTTP minor version of `HTTP/1.{minor}`.
    pub minor: u8,
    pub code: u16,
    pub reason: String,
}

/// Parses `HTTP/1.1 200 OK`. The reason phrase may be empty. The version
/// token must be exactly HTTP/1.0 or HTTP/1.1 — other majors/dialects are
/// rejected rather than guessed at.
pub fn parse_status_line(line: &str) -> HttpResult<StatusLine>
{
    let bad = |why: &str| HttpError::Protocol(format!("malformed status line: {why}"));

    let (version, rest) = line
        .split_once(' ')
        .ok_or_else(|| bad("missing version/code"))?;
    let minor = version
        .strip_prefix("HTTP/1.")
        .ok_or_else(|| bad(&format!("unsupported version {version:?}")))?;
    if !matches!(minor, "0" | "1")
    {
        return Err(bad("unsupported version"));
    }
    let minor = minor.as_bytes()[0] - b'0';

    let (code, reason) = match rest.split_once(' ')
    {
        Some((code, reason)) => (code, reason),
        None => (rest, ""),
    };
    if code.len() != 3 || !code.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(bad("status code is not three digits"));
    }
    let code: u16 = code.parse().map_err(|_| bad("status code out of range"))?;
    if !(100..=599).contains(&code)
    {
        return Err(bad("status code out of range"));
    }

    Ok(StatusLine {
        minor,
        code,
        reason: reason.trim_end().to_owned(),
    })
}

fn head_block_not_ansi(why: &str) -> HttpError
{
    HttpError::Protocol(why.to_owned())
}

/// Maps body/header-phase `io::Error`s into the right `HttpError` arm:
/// framing/syntax problems are `Protocol`, everything else stays `Io`.
pub(crate) fn wrap_head_io(err: &io::Error) -> HttpError
{
    match err.kind()
    {
        io::ErrorKind::InvalidData => HttpError::Protocol(err.to_string()),
        _ => HttpError::Io(clone_io_error(err)),
    }
}

fn clone_io_error(err: &io::Error) -> io::Error
{
    io::Error::new(err.kind(), err.to_string())
}

/// Wrapper that keeps already-read bytes from the header phase so the body
/// decoder consumes them first.
pub(crate) struct PrefillReader<R>
{
    inner: R,
    buf: Vec<u8>,
    pos: usize,
}

impl<R: Read> PrefillReader<R>
{
    pub fn new(inner: R) -> Self
    {
        Self {
            inner,
            buf: Vec::new(),
            pos: 0,
        }
    }

    fn buffered(&self) -> &[u8]
    {
        &self.buf[self.pos..]
    }

    fn buffered_len(&self) -> usize
    {
        self.buf.len() - self.pos
    }

    fn drain_buffered(&mut self, count: usize) -> Vec<u8>
    {
        let bytes = self.buf[self.pos..self.pos + count].to_vec();
        self.pos += count;
        if self.pos == self.buf.len()
        {
            self.buf.clear();
            self.pos = 0;
        }
        bytes
    }

    fn fill(&mut self) -> io::Result<usize>
    {
        if self.pos == self.buf.len()
        {
            self.buf.clear();
            self.pos = 0;
        }
        let mut chunk = [0u8; 8 * 1024];
        let filled = self.inner.read(&mut chunk)?;
        self.buf.extend_from_slice(&chunk[..filled]);
        Ok(filled)
    }

    /// Reads one CRLF-terminated line (bare LF tolerated) without the
    /// terminator. Errors on EOF and when the line exceeds `limit` bytes.
    pub fn read_line(&mut self, limit: usize) -> io::Result<Vec<u8>>
    {
        let mut line = Vec::new();
        loop
        {
            if let Some(newline) = self.buffered().iter().position(|b| *b == b'\n')
            {
                line.extend_from_slice(&self.drain_buffered(newline + 1));
                let terminator_len = if line.len() >= 2 && line[line.len() - 2] == b'\r'
                {
                    2
                }
                else
                {
                    1
                };
                if line.len() - terminator_len > limit
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "HTTP line exceeds length limit",
                    ));
                }
                break;
            }
            line.extend_from_slice(self.buffered());
            self.drain_buffered(self.buffered_len());
            if line.len() > limit
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "HTTP line exceeds length limit",
                ));
            }
            if self.fill()? == 0
            {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "connection closed inside a header line",
                ));
            }
        }
        if line.last() == Some(&b'\n')
        {
            line.pop();
            if line.last() == Some(&b'\r')
            {
                line.pop();
            }
        }
        Ok(line)
    }
}

impl<R: Read> Read for PrefillReader<R>
{
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize>
    {
        let buffered = self.buffered_len();
        if buffered > 0
        {
            let take = buffered.min(out.len());
            out[..take].copy_from_slice(&self.buf[self.pos..self.pos + take]);
            let _ = self.drain_buffered(take);
            return Ok(take);
        }
        self.inner.read(out)
    }
}


#[derive(Debug)]
enum ChunkStage
{
    Size,
    Data,
    DataEnd,
    Trailers,
    Done,
}

#[derive(Debug)]
struct ChunkState
{
    stage: ChunkStage,
    remaining: u64,
}

#[derive(Debug)]
enum BodyKind
{
    /// No body at all (1xx/204/304/HEAD).
    Complete,
    /// `Content-Length` framing; the counter runs down to zero.
    ContentLength(u64),
    /// `Transfer-Encoding: chunked`.
    Chunked(ChunkState),
    /// No framing declared; the body ends when the peer closes.
    UntilClose,
}

/// Incremental body decoder over the (already header-parsed) stream.
pub struct BodyReader<S: Read>
{
    source: PrefillReader<S>,
    kind: BodyKind,
    consumed: u64,
}

impl<S: Read> BodyReader<S>
{
    fn unexpected_eof() -> io::Error
    {
        io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "connection closed before the body was complete",
        )
    }

    pub fn consumed(&self) -> u64
    {
        self.consumed
    }

    /// Declared content length, when the response was framed with one.
    pub fn content_length(&self) -> Option<u64>
    {
        match &self.kind
        {
            BodyKind::ContentLength(remaining) => Some(*remaining + self.consumed),
            BodyKind::Complete => Some(0),
            _ => None,
        }
    }

    /// Reads the next piece of the body. For chunked responses this yields
    /// chunk data as it arrives (boundaries follow the sender's chunk sizes
    /// clipped to the caller's buffer), which is exactly the byte stream a
    /// progress callback wants.
    fn read_piece(&mut self, out: &mut [u8]) -> io::Result<usize>
    {
        if out.is_empty()
        {
            return Ok(0);
        }
        // Field-level split borrow: `read_chunked` is a free function so
        // `kind` and `source` borrow independently.
        let Self {
            source,
            kind,
            consumed,
        } = self;
        let taken = match kind
        {
            BodyKind::Complete => 0,
            BodyKind::UntilClose => source.read(out)?,
            BodyKind::ContentLength(remaining) => {
                if *remaining == 0
                {
                    return Ok(0);
                }
                let want = (*remaining).min(out.len() as u64) as usize;
                let n = source.read(&mut out[..want])?;
                if n == 0
                {
                    return Err(Self::unexpected_eof());
                }
                *remaining -= n as u64;
                n
            }
            BodyKind::Chunked(state) => read_chunked(source, state, out)?,
        };
        *consumed += taken as u64;
        Ok(taken)
    }
}

fn read_chunked<S: Read>(
    source: &mut PrefillReader<S>,
    state: &mut ChunkState,
    out: &mut [u8],
) -> io::Result<usize>
{
    loop
    {
        match state.stage
        {
            ChunkStage::Done => return Ok(0),
            ChunkStage::Size => {
                let line = source.read_line(MAX_LINE_BYTES)?;
                let line = String::from_utf8(line)
                    .map_err(|_| chunk_error("chunk size is not valid text"))?;
                let size_token = match line.split_once(';')
                {
                    Some((size, _extensions)) => size,
                    None => line.as_str(),
                };
                let size = u64::from_str_radix(size_token.trim(), 16)
                    .map_err(|_| chunk_error("malformed chunk size"))?;
                if size == 0
                {
                    state.stage = ChunkStage::Trailers;
                }
                else
                {
                    state.remaining = size;
                    state.stage = ChunkStage::Data;
                }
            }
            ChunkStage::Data => {
                let want = state.remaining.min(out.len() as u64) as usize;
                let n = source.read(&mut out[..want])?;
                if n == 0
                {
                    return Err(BodyReader::<PrefillReader<S>>::unexpected_eof());
                }
                state.remaining -= n as u64;
                if state.remaining == 0
                {
                    state.stage = ChunkStage::DataEnd;
                }
                return Ok(n);
            }
            ChunkStage::DataEnd => {
                let line = source.read_line(MAX_LINE_BYTES)?;
                if !line.is_empty()
                {
                    return Err(chunk_error("chunk data not terminated by CRLF"));
                }
                state.stage = ChunkStage::Size;
            }
            ChunkStage::Trailers => {
                let line = source.read_line(MAX_LINE_BYTES)?;
                if line.is_empty()
                {
                    state.stage = ChunkStage::Done;
                }
            }
        }
    }
}

fn chunk_error(why: &str) -> io::Error
{
    io::Error::new(io::ErrorKind::InvalidData, why.to_owned())
}

impl<S: Read> Read for BodyReader<S>
{
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize>
    {
        self.read_piece(out)
    }
}

/// A parsed response. Owns its connection (`S` = the transport stream), so
/// a drop closes it.
pub struct Response
{
    minor: u8,
    status: u16,
    reason: String,
    headers: Headers,
    body: BodyReader<Box<dyn Read>>,
    keep_alive: bool,
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
            .field("keep_alive", &self.keep_alive)
            .finish()
    }
}

fn parse_content_lengths(headers: &Headers) -> HttpResult<Option<u64>>
{
    let mut value: Option<u64> = None;
    for raw in headers.get_all("content-length")
    {
        let parsed: u64 = raw
            .parse()
            .map_err(|_| HttpError::Protocol("malformed content-length".into()))?;
        match value
        {
            Some(previous) if previous != parsed => {
                return Err(HttpError::Protocol("conflicting content-length".into()));
            }
            _ => value = Some(parsed),
        }
    }
    Ok(value)
}

fn has_only_chunked_transfer_encoding(headers: &Headers) -> bool
{
    let mut count = 0;
    for value in headers.get_all("transfer-encoding")
    {
        for coding in value.split(',')
        {
            if coding.trim().eq_ignore_ascii_case("chunked")
            {
                count += 1;
            }
            else
            {
                return false;
            }
        }
    }
    count == 1
}

/// Fully computes the framing decision for a response head.
fn body_framing(headers: &Headers, status: u16, minor: u8) -> HttpResult<BodyKind>
{
    let _ = minor;
    if (100..200).contains(&status) || status == 204 || status == 304
    {
        return Ok(BodyKind::Complete);
    }

    if headers.contains("transfer-encoding")
    {
        if !has_only_chunked_transfer_encoding(headers)
        {
            return Err(HttpError::Protocol(TE_NOT_CHUNKED.to_owned()));
        }
        if headers.contains("content-length")
        {
            return Err(HttpError::Protocol(TE_WITH_CL.to_owned()));
        }
        return Ok(BodyKind::Chunked(ChunkState {
            stage: ChunkStage::Size,
            remaining: 0,
        }));
    }

    if let Some(length) = parse_content_lengths(headers)?
    {
        return Ok(BodyKind::ContentLength(length));
    }

    Ok(BodyKind::UntilClose)
}

impl Response
{
    /// Parses the full head off `stream` and returns the response with a
    /// lazy body decoder.
    pub(crate) fn decode(
        stream: Box<dyn Read>,
        max_header_bytes: usize,
        is_head_request: bool,
    ) -> HttpResult<Self>
    {
        let mut reader = PrefillReader::new(stream);

        let status_bytes = reader
            .read_line(MAX_LINE_BYTES)
            .map_err(|err| wrap_head_io(&err))?;
        let status_line = std::str::from_utf8(&status_bytes)
            .map_err(|_| head_block_not_ansi("status line is not valid text"))?;
        let StatusLine { minor, code, reason } = parse_status_line(status_line)?;

        let mut headers = Headers::new();
        let mut head_size = status_bytes.len();
        loop
        {
            let raw = reader.read_line(MAX_LINE_BYTES).map_err(|err| wrap_head_io(&err))?;
            head_size += raw.len();
            if head_size > max_header_bytes
            {
                return Err(HttpError::Protocol(
                    "header block exceeds size limit".to_owned(),
                ));
            }
            let line = std::str::from_utf8(&raw)
                .map_err(|_| head_block_not_ansi("header line is not valid text"))?;
            if line.is_empty()
            {
                break;
            }
            let (name, value) = split_header(line)?;
            headers.insert(name, value);
        }

        let kind = if is_head_request
        {
            BodyKind::Complete
        }
        else
        {
            body_framing(&headers, code, minor)?
        };

        let peer_close = headers.contains_token("connection", "close");
        let peer_keep = headers.contains_token("connection", "keep-alive");
        let keep_alive = if matches!(kind, BodyKind::UntilClose)
        {
            false
        }
        else if peer_close
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

        Ok(Self {
            minor,
            status: code,
            reason,
            headers,
            body: BodyReader {
                source: reader,
                kind,
                consumed: 0,
            },
            keep_alive,
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

    /// Whether the connection may be reused after this response.
    pub fn keep_alive(&self) -> bool
    {
        self.keep_alive
    }

    pub fn content_length(&self) -> Option<u64>
    {
        self.body.content_length()
    }

    /// Bytes of the body consumed so far (used by the progress tracking).
    pub fn consumed(&self) -> u64
    {
        self.body.consumed()
    }

    /// Streams the whole body through `sink`, invoking it once per decoded
    /// chunk. Returns the total number of bytes passed through. Aborting is
    /// simply dropping the response (and with it the connection).
    pub fn stream_chunks(
        &mut self,
        sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> io::Result<u64>
    {
        let mut buffer = vec![0u8; 64 * 1024];
        let mut total = 0u64;
        loop
        {
            let n = self.body.read_piece(&mut buffer)?;
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
            let n = self.body.read_piece(&mut buffer)?;
            if n == 0
            {
                return Ok(out);
            }
            out.extend_from_slice(&buffer[..n]);
            if out.len() as u64 > max_bytes
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "response body exceeds the size limit",
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests
{
    use super::*;
    use std::io::Cursor;

    fn decode(raw: &[u8]) -> HttpResult<Response>
    {
        Response::decode(Box::new(Cursor::new(raw.to_vec())), 64 * 1024, false)
    }

    #[test]
    fn status_line_variants()
    {
        let head = parse_status_line("HTTP/1.1 200 OK").unwrap();
        assert_eq!(head.minor, 1);
        assert_eq!(head.code, 200);
        assert_eq!(head.reason, "OK");

        let no_reason = parse_status_line("HTTP/1.1 404").unwrap();
        assert_eq!(no_reason.code, 404);
        assert_eq!(no_reason.reason, "");

        assert!(parse_status_line("HTTP/2 200 OK").is_err());
        assert!(parse_status_line("HTTP/1.0 9xx OK").is_err());
        assert!(parse_status_line("HTTP/1.0 20").is_err());
        assert!(parse_status_line("HTTP/1.0 20X OK").is_err());
        assert!(parse_status_line("200 OK").is_err());
        assert!(parse_status_line("HTTP/1.1 600 OK").is_err());
        assert!(parse_status_line("garbage HTTP/1.1").is_err());
    }

    #[test]
    fn content_length_body_decodes_and_reports_length()
    {
        let mut response = decode(
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
        )
        .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.reason(), "OK");
        assert_eq!(response.keep_alive(), false);
        assert_eq!(response.read_all(1024).unwrap(), b"hello");
        assert_eq!(response.content_length(), Some(5));
        assert_eq!(response.consumed(), 5);
    }

    #[test]
    fn keep_alive_defaults_per_version()
    {
        let http11 = decode(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").unwrap();
        assert!(http11.keep_alive());

        let http10 = decode(b"HTTP/1.0 200 OK\r\nContent-Length: 0\r\n\r\n").unwrap();
        assert!(!http10.keep_alive());

        let http10_keep =
            decode(b"HTTP/1.0 200 OK\r\nContent-Length: 0\r\nKeep-Alive: timeout=5\r\nConnection: keep-alive\r\n\r\n")
                .unwrap();
        assert!(http10_keep.keep_alive());

        let explicit_close =
            decode(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .unwrap();
        assert!(!explicit_close.keep_alive());
    }

    #[test]
    fn chunked_body_and_trailers_decode()
    {
        let mut response = decode(
            concat!(
                "HTTP/1.1 200 OK\r\n",
                "Transfer-Encoding: chunked\r\n\r\n",
                "3\r\nhel\r\n",
                "2;x-ext=1\r\nlo\r\n",
                "0\r\nX-Trailer: yes\r\n\r\n"
            )
            .as_bytes(),
        )
        .unwrap();
        assert!(response.header("transfer-encoding").is_some());
        assert_eq!(response.read_all(1024).unwrap(), b"hello");
        assert_eq!(response.consumed(), 5);
    }

    #[test]
    fn chunked_piece_boundaries_follow_the_sender()
    {
        let mut response = decode(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nhel\r\n2\r\nlo\r\n0\r\n\r\n",
        )
        .unwrap();
        let mut pieces = Vec::new();
        response
            .stream_chunks(&mut |chunk| {
                pieces.push(chunk.to_vec());
                Ok(())
            })
            .unwrap();
        assert_eq!(
            pieces.iter().map(Vec::as_slice).collect::<Vec<_>>(),
            vec![&b"hel"[..], &b"lo"[..]]
        );
    }

    #[test]
    fn chunked_size_must_be_hex()
    {
        let broken = decode(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n\r\n",
        );
        match broken {
            Ok(mut response) => {
                let err = response.read_all(1024).unwrap_err();
                assert_eq!(err.kind(), io::ErrorKind::InvalidData);
            }
            Err(_) => {} // rejected at head parse time is equally acceptable
        }
    }

    #[test]
    fn no_body_statuses_have_complete_bodies()
    {
        let mut response = decode(b"HTTP/1.1 204 No Content\r\n\r\n").unwrap();
        assert_eq!(response.read_all(1024).unwrap().len(), 0);
        assert!(
            response.keep_alive(),
            "bodyless 204 over HTTP/1.1 stays reusable"
        );
    }

    #[test]
    fn content_length_conflicts_are_rejected()
    {
        let response = decode(
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 3\r\n\r\nab",
        );
        assert!(matches!(response, Err(HttpError::Protocol(_))));
    }

    #[test]
    fn transfer_encoding_conflicting_content_length_is_rejected()
    {
        let response = decode(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Length: 2\r\n\r\n0\r\n\r\n",
        );
        assert!(matches!(response, Err(HttpError::Protocol(_))));
    }

    #[test]
    fn unsupported_transfer_encoding_is_rejected()
    {
        let response = decode(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\n\r\n",
        );
        assert!(matches!(response, Err(HttpError::Protocol(_))));
    }

    #[test]
    fn chunked_coding_cannot_follow_or_precede_unsupported_transfer_codings()
    {
        for headers in [
            "Transfer-Encoding: gzip, chunked",
            "Transfer-Encoding: chunked, gzip",
            "Transfer-Encoding: chunked\r\nTransfer-Encoding: chunked",
        ]
        {
            let response = decode(
                format!("HTTP/1.1 200 OK\r\n{headers}\r\n\r\n").as_bytes(),
            );
            assert!(matches!(response, Err(HttpError::Protocol(_))));
        }
    }

    #[test]
    fn completed_line_still_obeys_the_length_limit()
    {
        let mut bytes = vec![b'a'; MAX_LINE_BYTES + 1];
        bytes.push(b'\n');
        let mut reader = PrefillReader::new(Cursor::new(bytes));
        let err = reader.read_line(MAX_LINE_BYTES).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn header_block_enforces_size_limit()
    {
        let mut raw = b"HTTP/1.1 200 OK\r\n".to_vec();
        raw.extend_from_slice(b"X-Fill: ");
        raw.extend(vec![b'a'; 8 * 1024]);
        raw.extend_from_slice(b"\r\nX-Fill2: ");
        raw.extend(vec![b'a'; 8 * 1024]);
        raw.extend_from_slice(b"\r\nX-Fill3: ");
        raw.extend(vec![b'a'; 8 * 1024]);
        raw.extend_from_slice(b"\r\nX-Fill4: ");
        raw.extend(vec![b'a'; 8 * 1024]);
        raw.extend_from_slice(b"\r\nX-Fill5: ");
        raw.extend(vec![b'a'; 8 * 1024]);
        raw.extend_from_slice(b"\r\n\r\n");
        let response =
            Response::decode(Box::new(Cursor::new(raw)), 32 * 1024, false);
        assert!(matches!(response, Err(HttpError::Protocol(_))));
    }

    #[test]
    fn truncated_status_line_fails_cleanly()
    {
        let response = decode(b"HTTP/1.1 20");
        assert!(matches!(response, Err(HttpError::Io(_))));
    }
}
