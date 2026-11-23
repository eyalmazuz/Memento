//! HTTP request model.

use super::headers::Headers;
use super::url::Url;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Method
{
    Get,
    Head,
    Post,
    Put,
    Delete,
    Patch,
}

impl Method
{
    pub const fn as_str(self) -> &'static str
    {
        match self
        {
            Self::Get => "GET",
            Self::Head => "HEAD",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
            Self::Patch => "PATCH",
        }
    }

    pub(crate) const fn to_http(self) -> ureq::http::Method
    {
        match self
        {
            Self::Get => ureq::http::Method::GET,
            Self::Head => ureq::http::Method::HEAD,
            Self::Post => ureq::http::Method::POST,
            Self::Put => ureq::http::Method::PUT,
            Self::Delete => ureq::http::Method::DELETE,
            Self::Patch => ureq::http::Method::PATCH,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request
{
    pub method: Method,
    pub url: Url,
    pub headers: Headers,
    pub body: Option<Vec<u8>>,
}

impl Request
{
    pub fn new(method: Method, url: Url) -> Self
    {
        Self {
            method,
            url,
            headers: Headers::new(),
            body: None,
        }
    }

    pub fn get(url: Url) -> Self
    {
        Self::new(Method::Get, url)
    }

    pub fn post(url: Url, body: Vec<u8>) -> Self
    {
        Self {
            method: Method::Post,
            url,
            headers: Headers::new(),
            body: Some(body),
        }
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self
    {
        self.headers.insert(name, value);
        self
    }

    pub fn with_body(mut self, body: Vec<u8>) -> Self
    {
        self.body = Some(body);
        self
    }
}
