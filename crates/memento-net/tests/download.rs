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

//! Download suites against ephemeral servers: fresh downloads replace stale
//! staging files, failures remove partial files, and progress events use the
//! ported byte/sec math. All traffic stays on 127.0.0.1.
//!
//! No external hosts are used.

mod common;

use std::fs;
use std::path::PathBuf;

use common::{response_head, Action, CapturedRequest, TestServer};

use memento_net::download::{
    recompute_progress, Downloader, DownloadError, DownloadProgress, ProgressState,
};

fn workdir() -> PathBuf
{
    let base = std::env::temp_dir();
    let dir = base.join(format!(
        "memento-net-dl-{}-{}",
        std::process::id(),
        nanos()
    ));
    fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn nanos() -> String
{
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    since.as_nanos().to_string()
}


// ---- basic download -----------------------------------------------------

#[test]
fn fresh_download_stages_then_renames()
{
    let server = TestServer::start();
    server.spawn(move |_, request: CapturedRequest| {
        assert_eq!(request.method, "GET");
        assert_eq!(request.target, "/model.zip");
        assert!(request.header("range").is_none(), "fresh download sends no Range");
        let body = b"this is a fake model archive";
        let head = response_head(
            "HTTP/1.1 200 OK",
            &format!(
                "Content-Length: {}\r\nETag: \"model-1\"\r\nConnection: close",
                body.len()
            ),
        );
        let mut wire = head.into_bytes();
        wire.extend_from_slice(body);
        Action::Respond(wire)
    });

    let dir = workdir();
    let target = dir.join("model.zip");
    fs::write(format!("{}.download", target.display()), b"stale partial").unwrap();
    let downloader = Downloader::default();
    let mut events = Vec::new();
    let outcome = downloader
        .download(&server.url("/model.zip"), &target, |event| {
            assert!(!event.data.is_empty());
            events.push(event.progress.received);
            Ok(())
        })
        .expect("download must succeed");

    assert_eq!(
        fs::read(&target).unwrap(),
        b"this is a fake model archive",
        "final file must hold the body"
    );
    assert_eq!(outcome.bytes, 28);
    assert!(
        fs::read_to_string(dir.join("model.zip.download")).is_err(),
        "staging file must be gone after rename"
    );
    assert_eq!(events.last().copied(), Some(28));
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn progress_events_report_total_and_speed_via_ported_math()
{
    let server = TestServer::start();
    server.spawn(|_, _| {
        let head = response_head("HTTP/1.1 200 OK", "Content-Length: 8");
        let mut wire = head.into_bytes();
        wire.extend_from_slice(b"12345678");
        Action::Respond(wire)
    });

    let dir = workdir();
    let target = dir.join("counted.bin");
    let downloader = Downloader::default();
    let mut progress_seen: Vec<DownloadProgress> = Vec::new();
    downloader
        .download(&server.url("/counted.bin"), &target, |event| {
            progress_seen.push(event.progress);
            Ok(())
        })
        .expect("download must succeed");

    // Total comes from Content-Length through the ported bounce; the last
    // event is the complete file with a plausible speed.
    let last = progress_seen.last().expect("at least one chunk");
    assert_eq!(last.received, 8);
    assert_eq!(last.total, 8);
    assert!((last.fraction - 1.0).abs() < f64::EPSILON);
    assert!(last.speed_bps >= 0);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn existing_target_short_circuits_the_download()
{
    let server = TestServer::start();
    // No handler spawns: any request would hang; the early exit must not
    // dial at all.
    let dir = workdir();
    let target = dir.join("already.zip");
    fs::write(&target, b"prior").unwrap();
    let downloader = Downloader::default();
    let outcome = downloader
        .download(&server.url("/already.zip"), &target, |_| Ok(()))
        .expect("existing target must early-out");
    assert_eq!(outcome.bytes, 5);
    assert_eq!(fs::read(&target).unwrap(), b"prior");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn server_error_keeps_the_response_status()
{
    let server = TestServer::start();
    server.spawn(|_, _| {
        Action::Respond(
            response_head("HTTP/1.1 404 Not Found", "Content-Length: 0").into_bytes(),
        )
    });
    let dir = workdir();
    let target = dir.join("missing.zip");
    let downloader = Downloader::default();
    match downloader.download(&server.url("/missing.zip"), &target, |_| Ok(()))
    {
        Err(DownloadError::Status { status, reason }) => {
            assert_eq!(status, 404);
            assert_eq!(reason, "Not Found");
        }
        other => panic!("expected Status error, got {other:?}"),
    }
    assert!(
        fs::read(format!("{}.download", target.display())).is_err(),
        "failed downloads must remove their staging file"
    );
    fs::remove_dir_all(&dir).unwrap();
}


#[test]
fn callback_io_error_is_preserved_and_staging_is_removed()
{
    let server = TestServer::start();
    server.spawn(|_, _| {
        let body = b"abc";
        let mut wire = response_head("HTTP/1.1 200 OK", "Content-Length: 3").into_bytes();
        wire.extend_from_slice(body);
        Action::Respond(wire)
    });
    let dir = workdir();
    let target = dir.join("callback.bin");
    let downloader = Downloader::default();
    let error = downloader
        .download(&server.url("/callback.bin"), &target, |_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "cache write denied",
            ))
        })
        .expect_err("callback error must abort the download");
    assert!(matches!(
        error,
        DownloadError::Io(error)
            if error.kind() == std::io::ErrorKind::PermissionDenied
                && error.to_string() == "cache write denied"
    ));
    assert!(!dir.join("callback.bin.download").exists());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn incomplete_body_is_reported_and_not_renamed()
{
    let server = TestServer::start();
    server.spawn(|_, _| {
        Action::Respond(
            response_head("HTTP/1.1 200 OK", "Content-Length: 10").into_bytes(),
        )
    });
    let dir = workdir();
    let target = dir.join("cut.bin");
    let downloader = Downloader::default();
    match downloader.download(&server.url("/cut.bin"), &target, |_| Ok(()))
    {
        Err(DownloadError::IncompleteBody { received, expected }) => {
            assert_eq!(received, 0);
            assert_eq!(expected, 10);
        }
        Err(DownloadError::Network(_)) => {
            // EOF could surface before the length check; either way the
            // exchange failed without renaming.
        }
        other => panic!("expected an incomplete-body error, got {other:?}"),
    }
    assert!(fs::read(&target).is_err(), "target must not exist");
    assert!(
        !dir.join("cut.bin.download").exists(),
        "failed downloads remove the partial staging file"
    );
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn https_download_uses_the_default_transport()
{
    let downloader = Downloader::default();
    let dir = workdir();
    let target = dir.join("secure.bin");
    assert!(
        matches!(
            downloader.download("https://127.0.0.1:1/x", &target, |_| Ok(())),
            Err(DownloadError::Network(_))
        ),
        "HTTPS must attempt a secure connection rather than fail fast"
    );
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn deep_progress_math_is_the_ported_downloadmanager_helper()
{
    // Direct bounce check: speed == memento_setting::models::download_speed,
    // fraction == received/total with Qt's qFuzzyCompare gate.
    let mut state = ProgressState::default();
    let (speed, fraction, changed) = recompute_progress(&mut state, 500, 1000, 500);
    assert_eq!(speed, 1000);
    assert!((fraction - 0.5).abs() < f64::EPSILON);
    assert!(changed);
    let (_, _, changed_again) = recompute_progress(&mut state, 500, 1000, 500);
    assert!(!changed_again);
}
