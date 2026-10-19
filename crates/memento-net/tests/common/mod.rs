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

//! Shared test harness: ephemeral HTTP servers over `std::net::TcpListener`
//! bound to `127.0.0.1:0`. Everything in-process; tests never reach an
//! external host.

#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

/// What a scripted exchange wants the server to do after the request.
pub enum Action
{
    /// Write these exact bytes, then close write side.
    Respond(Vec<u8>),
    /// Close without responding (aborted socket).
    Drop,
    /// Stay silent (exercises the client's read timeout).
    Stall,
    /// Wait, then respond.
    StallThen(Duration, Vec<u8>),
}

/// A parsed view of the request the server received.
#[derive(Clone, Debug)]
pub struct CapturedRequest
{
    pub method: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl CapturedRequest
{
    pub fn header(&self, name: &str) -> Option<&str>
    {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

pub struct TestServer
{
    listener: TcpListener,
    addr: SocketAddr,
    /// Kept so tests with soft-deadline timeouts do not race a stalled
    /// handler into a leaked log line; dropping is fine either way.
    #[allow(dead_code)]
    workers: std::sync::Mutex<Vec<JoinHandle<()>>>,
}

/// Convenience builder for a full response head.
pub fn response_head(status_line: &str, headers: &str) -> String
{
    format!("{status_line}\r\n{headers}\r\n\r\n")
}

impl TestServer
{
    /// Binds a fresh listener; the OS picks the port so tests never clash.
    pub fn start() -> Self
    {
        let listener =
            TcpListener::bind("127.0.0.1:0").expect("binding 127.0.0.1:0 must work in tests");
        let addr = listener.local_addr().unwrap();
        Self {
            listener,
            addr,
            workers: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn port(&self) -> u16
    {
        self.addr.port()
    }

    /// URL of the form `http://127.0.0.1:{port}` (plus `path`).
    pub fn url(&self, path: &str) -> String
    {
        format!("http://{}:{}{}", self.addr.ip(), self.addr.port(), path)
    }

    /// Spawns the accept loop. `handler` is called once per connection with
    /// the connection index (0-based) and the captured request.
    pub fn spawn<F>(&self, handler: F) -> &Self
    where
        F: Fn(usize, CapturedRequest) -> Action + Send + Sync + 'static,
    {
        let listener = self.listener.try_clone().expect("listener clone");
        let handler = Arc::new(handler);
        let mut workers = self.workers.lock().unwrap();
        workers.push(std::thread::spawn(move || {
            for (index, conn) in listener.incoming().enumerate()
            {
                match conn
                {
                    Ok(mut stream) => {
                        let _ =
                            stream.set_read_timeout(Some(Duration::from_secs(5)));
                        let _ =
                            stream.set_write_timeout(Some(Duration::from_secs(5)));
                        let handler = Arc::clone(&handler);
                        std::thread::spawn(move || {
                            let Ok(Some(request)) = read_request(&mut stream)
                            else
                            {
                                // includes read timeouts — client gave up first
                                return;
                            };
                            match handler(index, request)
                            {
                                Action::Respond(bytes) => {
                                    let _ = stream.write_all(&bytes);
                                    let _ = stream.flush();
                                    let _ =
                                        stream.shutdown(std::net::Shutdown::Write);
                                    // drain for a graceful close
                                    let mut sink = [0u8; 512];
                                    let _ = stream.read(&mut sink);
                                }
                                Action::StallThen(delay, bytes) => {
                                    std::thread::sleep(delay);
                                    let _ = stream.write_all(&bytes);
                                }
                                Action::Stall => {
                                    // hold the socket open longer than any
                                    // test timeout so the client sees a
                                    // read timeout, not a close
                                    std::thread::sleep(Duration::from_secs(2));
                                }
                                Action::Drop => { /* close immediately */ }
                            }
                        });
                    }
                    Err(_) => return,
                }
            }
        }));
        self
    }
}

/// Reads one request (head + Content-Length body). Returns `Ok(None)` when
/// the peer disconnects or times out before a full head arrives.
fn read_request(stream: &mut std::net::TcpStream) -> std::io::Result<Option<CapturedRequest>>
{
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4 * 1024];
    loop
    {
        if let Some(head_end) = find_head_end(&buffer)
        {
            let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
            let mut lines = head.split("\r\n");
            let request_line = lines.next().unwrap_or_default();
            let mut parts = request_line.split(' ');
            let method = parts.next().unwrap_or_default().to_owned();
            let target = parts.next().unwrap_or_default().to_owned();

            let mut headers = Vec::new();
            for line in lines
            {
                if line.is_empty()
                {
                    continue;
                }
                if let Some((name, value)) = line.split_once(':')
                {
                    headers
                        .push((name.trim().to_owned(), value.trim().to_owned()));
                }
            }

            let content_length: usize = headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, value)| value.parse().ok())
                .unwrap_or(0);

            let body_start = head_end + 4;
            while buffer.len() < body_start + content_length
            {
                let n = stream.read(&mut chunk)?;
                if n == 0
                {
                    break;
                }
                buffer.extend_from_slice(&chunk[..n]);
            }
            let body_start = body_start.min(buffer.len());
            let body_end = body_start + content_length;
            let body = buffer
                [body_start..body_end.min(buffer.len())]
                .to_vec();

            return Ok(Some(CapturedRequest {
                method,
                target,
                headers,
                body,
            }));
        }

        let n = stream.read(&mut chunk)?;
        if n == 0
        {
            return Ok(None);
        }
        buffer.extend_from_slice(&chunk[..n]);
        if buffer.len() > 128 * 1024
        {
            return Ok(None);
        }
    }
}

fn find_head_end(buffer: &[u8]) -> Option<usize>
{
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}
