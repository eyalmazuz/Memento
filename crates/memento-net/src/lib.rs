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

//! Qt-free networking core for Memento: the pure-Rust replacement for the
//! `QNetworkAccessManager` flows in `ankiclient.cpp`,
//! `downloadmanager.cpp`, and `audiofiles.cpp`.
//!
//! # Layers
//!
//! * [`http`] — HTTP/1.1 over TCP with rustls-backed HTTPS and validated
//!   Mozilla WebPKI roots: canonical requests, framing, redirects, timeouts,
//!   and byte-level streaming for downloads.
//! * [`anki`] — typed AnkiConnect client. Request bodies and reply-shape
//!   validation reuse the already-ported core in
//!   `memento_anki::client` (`ReplyValidation`, `TypedReplyStatus`,
//!   `check_ankiconnect_version`, host normalization); errors split into a
//!   network half (socket/timeouts) and a protocol half (reply shape),
//!   matching `AnkiClient::processReply`'s error strings.
//! * [`download`] — model-file downloads through a
//!   `path + ".download"` staging file, temp-file + rename finalization,
//!   failure cleanup, and progress/speed computation bounced through the
//!   ported `memento_setting` DownloadManager helpers.
//!
//! # Testing
//!
//! All behavioral suites run against ephemeral `std::net::TcpListener`
//! servers bound to `127.0.0.1:0` inside the test process; there is no
//! dependence on any external host.

pub mod anki;
pub mod capi;
pub mod download;
pub mod error;
pub mod http;

pub use error::{HttpError, HttpResult};
