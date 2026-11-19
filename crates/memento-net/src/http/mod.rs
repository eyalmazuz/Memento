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

//! Blocking, Qt-free HTTP/1.1 core for Memento backed by `ureq` (with rustls
//! + WebPKI roots) and `url`.

pub mod client;
pub mod headers;
pub mod request;
pub mod response;
pub mod url;

pub use client::{HttpClient, HttpClientConfig};
pub use headers::Headers;
pub use request::{Method, Request};
pub use response::Response;
pub use url::{Scheme, Url};
