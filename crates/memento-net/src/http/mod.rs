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

//! Blocking, Qt-free HTTP/1.1 core for Memento.
//!
//! HTTP/1.1 request canonicalization, response head parsing, Content-Length
//! and chunked body decoding, redirect following, and a byte-level streaming
//! hook ([`Response::stream_chunks`] plus [`std::io::Read`]) for downloads.
//! The built-in transport secures HTTPS with rustls and WebPKI root
//! validation; custom transports may supply other network policies.

pub mod client;
pub mod headers;
pub mod request;
pub mod response;
pub mod url;

pub use client::{HttpClient, HttpClientConfig, TcpTransport, Transport};
pub use headers::Headers;
pub use request::{Method, Request};
pub use response::Response;
pub use url::{Scheme, Url};
