//! Blocking, Qt-free HTTP/1.1 core for Subomi backed by `ureq` (with rustls
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
