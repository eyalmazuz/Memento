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

//! End-to-end HTTP suites against ephemeral `127.0.0.1` servers: success,
//! chunked framing, redirects (absolute, relative, scheme-relative, loop
//! detection, depth limit), aborted sockets, read timeouts, oversized
//! headers, malformed status lines, keep-alive flagging, and HTTPS transport.
//! Every server binds `127.0.0.1:0`; no external hosts.

mod common;

use std::io;
use std::time::Duration;

use common::{response_head, Action, CapturedRequest, TestServer};

use memento_net::error::HttpError;
use memento_net::http::{HttpClient, HttpClientConfig, Request, Url};

fn client() -> HttpClient
{
    HttpClient::new(HttpClientConfig::default())
}

fn client_with_config(config: HttpClientConfig) -> HttpClient
{
    HttpClient::new(config)
}

fn ok_200(body: &[u8]) -> Vec<u8>
{
    let mut wire =
        response_head(
            "HTTP/1.1 200 OK",
            &format!("Content-Length: {}\r\nConnection: close", body.len()),
        )
        .into_bytes();
    wire.extend_from_slice(body);
    wire
}

// ---- success paths ------------------------------------------------------

#[test]
fn plain_success_round_trip_captures_the_request_canonical_form()
{
    let server = TestServer::start();
    let port = server.port();
    server.spawn(move |_, request: CapturedRequest| {
        assert_eq!(request.method, "GET");
        assert_eq!(request.target, "/");
        // The canonical client contract: host header, explicit identity
        // encoding, keep-alive default.
        assert_eq!(request.header("host").unwrap(), format!("127.0.0.1:{port}"));
        assert_eq!(request.header("accept-encoding").unwrap(), "identity");
        assert_eq!(request.header("connection").unwrap(), "keep-alive");
        Action::Respond(ok_200(b"payload"))
    });

    let url = server.url("/");
    let mut response =
        client().send(&Request::get(Url::parse(&url).unwrap())).unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.reason(), "OK");
    assert_eq!(response.read_all(1024).unwrap(), b"payload");
    assert_eq!(response.header("content-length").unwrap(), "7");
}

#[test]
fn chunked_response_streams_piecewise_before_completion()
{
    let server = TestServer::start();
    server.spawn(|_, _| {
        let mut wire = response_head(
            "HTTP/1.1 200 OK",
            "Transfer-Encoding: chunked\r\nX-Stream: yes",
        )
        .into_bytes();
        wire.extend_from_slice(b"4\r\naaaa\r\n5\r\nbbbbb\r\n0\r\n\r\n");
        Action::Respond(wire)
    });

    let url = server.url("/stream");
    let mut response =
        client().send(&Request::get(Url::parse(&url).unwrap())).unwrap();
    assert_eq!(response.header("transfer-encoding").unwrap(), "chunked");
    assert_eq!(response.header("x-stream").unwrap(), "yes");

    let mut collected = Vec::new();
    let total = response
        .stream_chunks(&mut |chunk| {
            collected.push(chunk.to_vec());
            Ok(())
        })
        .unwrap();
    assert_eq!(total, 9);
    assert_eq!(
        collected.iter().map(Vec::as_slice).collect::<Vec<_>>(),
        vec![&b"aaaa"[..], &b"bbbbb"[..]]
    );
}

#[test]
fn elastic_connection_close_mode_is_visible_in_flags()
{
    let server = TestServer::start();
    server.spawn(|_, _| {
        Action::Respond(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi".to_vec())
    });
    let url = server.url("/close");
    let mut response =
        client().send(&Request::get(Url::parse(&url).unwrap())).unwrap();
    assert!(!response.keep_alive());
    assert_eq!(response.read_all(16).unwrap(), b"hi");
}

// ---- redirects ----------------------------------------------------------

#[test]
fn absolute_redirect_chain_hits_the_new_target()
{
    let server = TestServer::start();
    let port = server.port();
    server.spawn(move |index, request: CapturedRequest| match index
    {
        0 => Action::Respond(
            response_head(
                "HTTP/1.1 302 Found",
                &format!(
                    "Location: http://127.0.0.1:{port}/final\r\nContent-Length: 0"
                ),
            )
            .into_bytes(),
        ),
        _ => {
            assert_eq!(request.target, "/final", "redirect target must be hit");
            assert_eq!(request.method, "GET");
            Action::Respond(ok_200(b"redirected"))
        }
    });

    let url = server.url("/start");
    let mut response =
        client().send(&Request::get(Url::parse(&url).unwrap())).unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.read_all(1024).unwrap(), b"redirected");
}

#[test]
fn relative_location_with_dot_segments_resolves()
{
    let server = TestServer::start();
    server.spawn(|index, request: CapturedRequest| match index
    {
        0 => Action::Respond(
            response_head(
                "HTTP/1.1 301 Moved Permanently",
                "Location: sub/dir/../else\r\nContent-Length: 0",
            )
            .into_bytes(),
        ),
        _ => {
            assert_eq!(request.target, "/sub/else", "relative URL must merge");
            Action::Respond(ok_200(b"relative-ok"))
        }
    });

    let url = server.url("/a");
    let mut response =
        client().send(&Request::get(Url::parse(&url).unwrap())).unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.read_all(1024).unwrap(), b"relative-ok");
}

#[test]
fn scheme_relative_location_resolves()
{
    let server = TestServer::start();
    let port = server.port();
    server.spawn(move |index, request: CapturedRequest| match index
    {
        0 => Action::Respond(
            response_head(
                "HTTP/1.1 307 Temporary Redirect",
                &format!("Location: //127.0.0.1:{port}/here\r\nContent-Length: 0"),
            )
            .into_bytes(),
        ),
        _ => {
            assert_eq!(request.target, "/here");
            Action::Respond(ok_200(b"scheme-ok"))
        }
    });

    let url = server.url("/x");
    let mut response =
        client().send(&Request::get(Url::parse(&url).unwrap())).unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.read_all(1024).unwrap(), b"scheme-ok");
}

#[test]
fn redirect_cycle_is_detected()
{
    // /a -> /b on the same origin; /b -> /a completes the loop.
    let server = TestServer::start();
    let port = server.port();
    server.spawn(move |index, _| {
        let target = if index == 0 { "/b" } else { "/a" };
        Action::Respond(
            response_head(
                "HTTP/1.1 302 Found",
                &format!("Location: http://127.0.0.1:{port}{target}\r\nContent-Length: 0"),
            )
            .into_bytes(),
        )
    });
    let url = server.url("/a");
    let result = client().send(&Request::get(Url::parse(&url).unwrap()));
    match result
    {
        Err(HttpError::RedirectLoop) => {}
        Err(HttpError::TooManyRedirects { limit }) => assert_eq!(limit, 5),
        other => panic!("expected redirect bookkeeping error, got {other:?}"),
    }
}

#[test]
fn redirect_depth_limit_short_circuits_expected_302s()
{
    let server = TestServer::start();
    let port = server.port();
    server.spawn(move |index, _| {
        Action::Respond(
            response_head(
                "HTTP/1.1 302 Found",
                &format!("Location: http://127.0.0.1:{port}/t{index}\r\nContent-Length: 0"),
            )
            .into_bytes(),
        )
    });
    let config = HttpClientConfig {
        max_redirects: 2,
        ..HttpClientConfig::default()
    };
    let url = server.url("/a");
    let result =
        client_with_config(config).send(&Request::get(Url::parse(&url).unwrap()));
    match result
    {
        Err(HttpError::TooManyRedirects { limit }) => assert_eq!(limit, 2),
        other => panic!("expected TooManyRedirects, got {other:?}"),
    }
}

#[test]
fn post_through_302_demotes_to_get_and_drops_the_body()
{
    let server = TestServer::start();
    let port = server.port();
    server.spawn(move |index, request: CapturedRequest| match index
    {
        0 => {
            assert_eq!(request.method, "POST");
            assert_eq!(request.body, b"form-data");
            Action::Respond(
                response_head(
                    "HTTP/1.1 302 Found",
                    &format!("Location: http://127.0.0.1:{port}/after\r\nContent-Length: 0"),
                )
                .into_bytes(),
            )
        }
        _ => {
            assert_eq!(request.method, "GET", "303/302 demote POST to GET");
            assert!(request.body.is_empty(), "body must be dropped on demote");
            Action::Respond(ok_200(b"after"))
        }
    });

    let url = server.url("/submit");
    let request =
        Request::post(Url::parse(&url).unwrap(), b"form-data".to_vec());
    let mut response = client().send(&request).unwrap();
    assert_eq!(response.read_all(64).unwrap(), b"after");
}
#[test]
fn head_request_remains_head_after_see_other()
{
    let server = TestServer::start();
    let port = server.port();
    server.spawn(move |index, request: CapturedRequest| {
        assert_eq!(request.method, "HEAD");
        if index == 0
        {
            Action::Respond(
                response_head(
                    "HTTP/1.1 303 See Other",
                    &format!("Location: http://127.0.0.1:{port}/final\r\nContent-Length: 0"),
                )
                .into_bytes(),
            )
        }
        else
        {
            Action::Respond(ok_200(b""))
        }
    });
    let url = server.url("/head");
    let request = Request::new(
        memento_net::http::Method::Head,
        Url::parse(&url).unwrap(),
    );
    let response = client().send(&request).unwrap();
    assert_eq!(response.status(), 200);
}

// ---- failure modes ------------------------------------------------------

#[test]
fn aborted_socket_reads_as_unexpected_eof()
{
    let server = TestServer::start();
    server.spawn(|_, _| {
        // head promises a body, then the socket dies
        Action::Respond(
            response_head("HTTP/1.1 200 OK", "Content-Length: 100").into_bytes(),
        )
    });
    let url = server.url("/abort");
    let mut response =
        client().send(&Request::get(Url::parse(&url).unwrap())).unwrap();
    let error = response.read_all(1024).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn stalled_server_fires_the_read_timeout()
{
    let server = TestServer::start();
    server.spawn(|_, _| Action::Stall);
    let config = HttpClientConfig {
        timeout: Some(Duration::from_millis(200)),
        ..HttpClientConfig::default()
    };
    let url = server.url("/slow");
    let result =
        client_with_config(config).send(&Request::get(Url::parse(&url).unwrap()));
    let error = result.expect_err("stalled server must time out");
    assert!(
        error.is_timeout(),
        "expected a timeout classification, got {error:?}"
    );
}

#[test]
fn oversized_header_is_rejected_as_a_protocol_error()
{
    let server = TestServer::start();
    server.spawn(|_, _| {
        let mut raw = b"HTTP/1.1 200 OK\r\n".to_vec();
        raw.extend_from_slice(b"X-Fill: ");
        raw.extend(vec![b'a'; 128 * 1024]);
        raw.extend_from_slice(b"\r\n\r\n");
        Action::Respond(raw)
    });
    let url = server.url("/big");
    let result = client().send(&Request::get(Url::parse(&url).unwrap()));
    match result
    {
        Err(HttpError::Protocol(why)) => {
            assert!(
                why.contains("limit") || why.contains("exceeds"),
                "unexpected protocol reason {why:?}"
            );
        }
        other => panic!("expected protocol error, got {other:?}"),
    }
}

#[test]
fn malformed_status_line_is_rejected_as_a_protocol_error()
{
    let server = TestServer::start();
    server.spawn(|_, _| {
        Action::Respond(b"THIS IS NOT HTTP\r\nContent-Length: 0\r\n\r\n".to_vec())
    });
    let url = server.url("/bad");
    let result = client().send(&Request::get(Url::parse(&url).unwrap()));
    match result
    {
        Err(HttpError::Protocol(why)) => {
            assert!(why.contains("status line"), "got {why:?}");
        }
        other => panic!("expected protocol error, got {other:?}"),
    }
}

#[test]
fn keep_alive_flag_marks_the_connection_reusable()
{
    let server = TestServer::start();
    server.spawn(|_, _| {
        Action::Respond(
            response_head("HTTP/1.1 200 OK", "Content-Length: 0").into_bytes(),
        )
    });
    let url = server.url("/");
    let response =
        client().send(&Request::get(Url::parse(&url).unwrap())).unwrap();
    assert!(response.keep_alive());
}

#[test]
fn https_endpoint_uses_the_default_transport()
{
    let request = Request::get(
        Url::parse("https://127.0.0.1:1/secure").unwrap(),
    );
    assert!(
        matches!(client().send(&request), Err(HttpError::Io(_))),
        "HTTPS must attempt a secure TCP connection rather than fail fast"
    );
}
