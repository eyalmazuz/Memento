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

//! An insertion-ordered header map with HTTP token validation. Unlike a
//! `QHash`, order matters: request canonicalization must reproduce the
//! `Host … Content-Length` ordering the test fixtures assert, and response
//! parsing must keep duplicate headers inspectable.

use crate::error::{HttpError, HttpResult};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Headers(Vec<(String, String)>);

impl Headers
{
    pub fn new() -> Self
    {
        Self(Vec::new())
    }

    /// Appends a header. Name and value are validated to be wire-legal;
    /// parsing of peer headers relies on this rejecting CRLF smuggling.
    pub fn insert(&mut self, name: impl Into<String>, value: impl Into<String>)
    {
        self.0.push((name.into(), value.into()));
    }

    /// First occurrence (matched case-insensitively), mirroring most
    /// consumers' view of `QNetworkRequest::header`.
    pub fn get(&self, name: &str) -> Option<&str>
    {
        self.0
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn get_all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str>
    {
        self.0
            .iter()
            .filter(move |(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn contains(&self, name: &str) -> bool
    {
        self.get(name).is_some()
    }

    pub fn is_empty(&self) -> bool
    {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize
    {
        self.0.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)>
    {
        self.0.iter().map(|(key, value)| (key.as_str(), value.as_str()))
    }

    /// True when `name` exists and one of its comma-separated values equals
    /// `token` (case-insensitive, surrounding whitespace ignored). Handlese
    /// the `Connection: close` / `keep-alive` decisions.
    pub fn contains_token(&self, name: &str, token: &str) -> bool
    {
        self.get_all(name).any(|value| {
            value
                .split(',')
                .any(|part| part.trim().eq_ignore_ascii_case(token))
        })
    }
}

/// Splits a raw `Name: value` line (without the trailing CRLF) into its
/// parsed parts, enforcing the token rules.
pub fn split_header(line: &str) -> HttpResult<(String, String)>
{
    let (name, value) = line
        .split_once(':')
        .ok_or_else(|| HttpError::Protocol(format!("missing colon in header {line:?}")))?;

    if name.contains(' ') || !is_token(name)
    {
        return Err(HttpError::Protocol(format!(
            "invalid header field name {name:?}"
        )));
    }

    let value = value.trim_matches([' ', '\t']);
    if !valid_header_value(value)
    {
        return Err(HttpError::Protocol(format!(
            "header value for {name} contains forbidden bytes"
        )));
    }

    Ok((name.to_owned(), value.to_owned()))
}

/// Header field names are RFC 7230 tokens: visible ASCII without the
/// delimiters `()<>@,;:\"/[]?={} \t`.
pub fn is_token(name: &str) -> bool
{
    if name.is_empty()
    {
        return false;
    }
    name.bytes().all(is_tchar)
}

fn is_tchar(byte: u8) -> bool
{
    match byte
    {
        b'!' | b'#' | b'$' | b'%' | b'&' | b'\'' | b'*' | b'+' | b'-' | b'.'
        | b'^' | b'_' | b'`' | b'|' | b'~' => true,
        b'0'..=b'9' => true,
        b'a'..=b'z' | b'A'..=b'Z' => true,
        _ => false,
    }
}

/// Header values may not contain CR, LF or NUL (and no other C0 controls).
pub fn valid_header_value(value: &str) -> bool
{
    value
        .bytes()
        .all(|b| (0x20..=0x7e).contains(&b) || b == b'\t' || b >= 0x80)
}
