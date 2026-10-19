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

//! Model-file downloader, replacing `downloadmanager.cpp`'s
//! `QNetworkAccessManager` download flow.
//!
//! The downloader keeps the observable file behavior of the C++ path:
//! existing targets short-circuit, each attempt truncates
//! `path + ".download"`, successful downloads rename the staging file onto
//! the target, and failures remove the partial. Progress speed and change
//! decisions reuse the ported `memento_setting` DownloadManager helpers.
//!
//! ## MD5 / hashing
//!
//! There is deliberately **no** MD5 hook here: the `md5` crate is not
//! available to this crate, and hashing (including the `skipHash`
//! reuse-of-cache shortcut used by `audiofiles.cpp`) belongs to the
//! caller. The API stays byte-stream based on purpose: every decoded chunk
//! is handed to the caller's callback (see [`DownloadEvent`]), which is
//! what a future `memento-audio`/`memento-util` integration feeds its
//! incremental hasher with.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use memento_setting::models::download_progress_update;
use memento_setting::models::download_speed;

use crate::error::HttpError;
use crate::http::{HttpClient, Request, Url};

/// The per-chunk progress payload. `speed_bps` comes straight from the
/// ported `memento_setting::models::download_speed`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DownloadProgress
{
    /// Bytes of the current download received so far.
    pub received: i64,
    /// Total size in bytes when the server declared one; `0` when
    /// unknown.
    pub total: i64,
    pub speed_bps: i64,
    /// Received/total computed by the ported `download_progress_update`.
    pub fraction: f64,
}

/// One decoded chunk plus the progress computed for it. Consumed by the
/// caller's callback; a hashing caller feeds `data` incrementally here.
#[derive(Debug)]
pub struct DownloadEvent<'a>
{
    pub data: &'a [u8],
    pub progress: DownloadProgress,
}

/// Running state for [`recompute_progress`].
#[derive(Clone, Copy, Debug, Default)]
pub struct ProgressState
{
    prev_progress: f64,
    prev_received: i64,
    prev_total: i64,
    prev_speed: i64,
}

/// The bounce point between the byte stream and the ported
/// `memento_setting` DownloadManager arithmetic: computes bytes/sec
/// (`download_speed`) and the fuzzy-compared progress fraction
/// (`download_progress_update`). Returns `changed` the way
/// `DownloadManager::setDownloadProgress` gates its signal.
pub fn recompute_progress(
    state: &mut ProgressState,
    received: i64,
    total: i64,
    elapsed_ms: i64,
) -> (i64, f64, bool)
{
    let speed = download_speed(received, elapsed_ms);
    let (fraction, changed) = download_progress_update(
        state.prev_progress,
        state.prev_received,
        state.prev_total,
        state.prev_speed,
        received,
        total,
        speed,
    );
    if changed
    {
        state.prev_progress = fraction;
        state.prev_received = received;
        state.prev_total = total;
        state.prev_speed = speed;
    }
    (speed, fraction, changed)
}

#[derive(Debug)]
#[non_exhaustive]
pub enum DownloadError
{
    /// A custom HTTP transport refused an HTTPS endpoint.
    Tls,
    /// URL composition, protocol violations, redirect bookkeeping.
    Transport(HttpError),
    /// Network read/write I/O (connect refused, resets, timeouts).
    Network(io::Error),
    /// Server answered with >= 400.
    Status
    {
        status: u16,
        reason: String,
    },
    /// The body ended before the declared length was reached.
    IncompleteBody
    {
        received: u64,
        expected: u64,
    },
    /// Local I/O failure (staging, callback, rename, directory creation).
    Io(io::Error),
}

impl From<HttpError> for DownloadError
{
    fn from(err: HttpError) -> Self
    {
        match err
        {
            HttpError::TlsUnsupported => Self::Tls,
            HttpError::Io(io_err) => Self::Network(io_err),
            other => Self::Transport(other),
        }
    }
}

impl std::fmt::Display for DownloadError
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result
    {
        match self
        {
            Self::Tls => write!(
                f,
                "download endpoint requires TLS, but the transport does not provide it"
            ),
            Self::Transport(http_err) => write!(f, "{http_err}"),
            Self::Network(err) => write!(f, "download stalled: {err}"),
            Self::Status { status, reason } => {
                write!(f, "download failed with status {status} {reason}")
            }
            Self::IncompleteBody { received, expected } => {
                write!(f, "download was cut short at {received}/{expected} bytes")
            }
            Self::Io(err) => write!(f, "filesystem error: {err}"),
        }
    }
}

impl std::error::Error for DownloadError
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)>
    {
        match self
        {
            Self::Network(err) | Self::Io(err) => Some(err),
            Self::Transport(http_err) => Some(http_err),
            _ => None,
        }
    }
}

pub type DownloadResult<T> = Result<T, DownloadError>;

/// Summary of a finished download.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadOutcome
{
    pub target: PathBuf,
    pub bytes: u64,
}

pub struct Downloader
{
    http: HttpClient,
}

impl Default for Downloader
{
    fn default() -> Self
    {
        Self::new(HttpClient::default())
    }
}

impl Downloader
{
    pub fn new(http: HttpClient) -> Self
    {
        Self { http }
    }

    /// Downloads `url` onto `target`, byte-stream based. Every decoded
    /// chunk is handed to `on_chunk` as a [`DownloadEvent`] (data +
    /// progress); returning an `Err` aborts the download and removes the
    /// staging file, matching the C++ failure cleanup.
    pub fn download(
        &self,
        url: &str,
        target: &Path,
        mut on_chunk: impl FnMut(&DownloadEvent<'_>) -> io::Result<()>,
    ) -> DownloadResult<DownloadOutcome>
    {
        let parsed = Url::parse(url)?;
        if target.as_os_str().is_empty()
        {
            return Err(DownloadError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "target path is empty",
            )));
        }

        if target.exists()
        {
            let bytes = fs::metadata(target).map_err(DownloadError::Io)?.len();
            return Ok(DownloadOutcome {
                target: target.to_owned(),
                bytes,
            });
        }

        if let Some(parent) = target.parent()
        {
            if !parent.as_os_str().is_empty()
            {
                fs::create_dir_all(parent).map_err(DownloadError::Io)?;
            }
        }

        let staging = staging_path(target);
        let _cleanup = StagingCleanup { path: staging.clone() };
        let mut staging_file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&staging)
            .map_err(DownloadError::Io)?;

        let request = Request::get(parsed);
        let mut response = self.http.send(&request)?;
        if response.status() >= 400
        {
            return Err(DownloadError::Status {
                status: response.status(),
                reason: response.reason().to_owned(),
            });
        }

        let total = response.content_length().unwrap_or(0);
        let started = Instant::now();
        let mut state = ProgressState::default();
        let mut received = 0u64;
        let mut sink_error = None;
        let stream_result = response.stream_chunks(&mut |chunk| {
            received += chunk.len() as u64;
            on_chunk(&DownloadEvent {
                data: chunk,
                progress: progress_for(&mut state, &started, received, total),
            })
            .or_else(|error| {
                sink_error = Some(error);
                Err(io::Error::other("download callback failed"))
            })?;
            staging_file.write_all(chunk).or_else(|error| {
                sink_error = Some(error);
                Err(io::Error::other("staging write failed"))
            })?;
            Ok(())
        });
        if let Some(error) = sink_error
        {
            return Err(DownloadError::Io(error));
        }
        stream_result.map_err(DownloadError::Network)?;

        if let Some(expected) = response.content_length()
        {
            if received != expected
            {
                return Err(DownloadError::IncompleteBody {
                    received,
                    expected,
                });
            }
        }

        staging_file.flush().map_err(DownloadError::Io)?;
        drop(staging_file);
        let _ = fs::remove_file(target);
        fs::rename(&staging, target).map_err(DownloadError::Io)?;

        Ok(DownloadOutcome {
            target: target.to_owned(),
            bytes: received,
        })
    }
}

struct StagingCleanup
{
    path: PathBuf,
}

impl Drop for StagingCleanup
{
    fn drop(&mut self)
    {
        let _ = fs::remove_file(&self.path);
    }
}

fn progress_for(
    state: &mut ProgressState,
    started: &Instant,
    received: u64,
    total: u64,
) -> DownloadProgress
{
    let (speed, fraction, _) = recompute_progress(
        state,
        received as i64,
        total as i64,
        started.elapsed().as_millis() as i64,
    );
    DownloadProgress {
        received: received as i64,
        total: total as i64,
        speed_bps: speed,
        fraction,
    }
}

/// `path + ".download"`, matching `downloadmanager.cpp` exactly.
pub fn staging_path(target: &Path) -> PathBuf
{
    let mut name = target.as_os_str().to_owned();
    name.push(".download");
    PathBuf::from(name)
}


#[cfg(test)]
mod tests
{
    use super::*;

    #[test]
    fn staging_name_matches_downloadmanager()
    {
        assert_eq!(
            staging_path(Path::new("/tmp/model.zip")),
            PathBuf::from("/tmp/model.zip.download")
        );
    }

    #[test]
    fn progress_bounce_uses_ported_speed_math()
    {
        // elapsed 0 is clamped to 1ms by the ported helper
        let mut state = ProgressState::default();
        let (speed, fraction, changed) =
            recompute_progress(&mut state, 1000, 1000, 1);
        assert_eq!(speed, 1_000_000);
        assert!((fraction - 1.0).abs() < f64::EPSILON);
        assert!(changed);

        // identical values (including elapsed time) are suppressed
        // (setDownloadProgress's qFuzzyCompare gate)
        let again = recompute_progress(&mut state, 1000, 1000, 1);
        assert!(!again.2);

        // a different speed (bytes moved, time passed) is reported
        let moved = recompute_progress(&mut state, 2000, 2000, 4);
        assert!(moved.2);
        assert_eq!(moved.0, 500_000);
    }

    #[test]
    fn empty_target_path_is_rejected_without_creating_a_staging_file()
    {
        let result = Downloader::default().download(
            "http://127.0.0.1:8765/model.bin",
            Path::new(""),
            |_| Ok(()),
        );
        assert!(matches!(
            result,
            Err(DownloadError::Io(error))
                if error.kind() == io::ErrorKind::InvalidInput
        ));
    }
}
