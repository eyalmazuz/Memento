//! Qt-free networking core for Subomi: the pure-Rust replacement for the
//! `QNetworkAccessManager` flows in `ankiclient.cpp`,
//! `downloadmanager.cpp`, and `audiofiles.cpp`.
//!
//! # Layers
//!
//! * [`http`] — HTTP/1.1 over TCP with `ureq` + rustls-backed HTTPS and
//!   validated Mozilla WebPKI roots, `url`-backed URL parsing, redirect/SSRF
//!   policy, timeouts, and byte-level streaming for downloads.
//! * [`anki`] — typed AnkiConnect client. Request bodies and reply-shape
//!   validation reuse the already-ported core in
//!   `subomi_anki::client` (`ReplyValidation`, `TypedReplyStatus`,
//!   `check_ankiconnect_version`, host normalization); errors split into a
//!   network half (socket/timeouts) and a protocol half (reply shape),
//!   matching `AnkiClient::processReply`'s error strings.
//! * [`download`] — model-file downloads through a
//!   `path + ".download"` staging file, temp-file + rename finalization,
//!   failure cleanup, and progress/speed computation bounced through the
//!   ported `subomi_setting` DownloadManager helpers.
//!
//! # Testing
//!
//! All behavioral suites run against ephemeral `std::net::TcpListener`
//! servers bound to `127.0.0.1:0` inside the test process; there is no
//! dependence on any external host.

pub mod anki;
pub mod download;
pub mod error;
pub mod http;

pub use error::{HttpError, HttpResult};
