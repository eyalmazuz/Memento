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

//! Binary file algorithms; ownership and QFile state remain with the caller.

use base64::{engine::general_purpose::STANDARD, Engine};
use md5::{Digest, Md5};
use std::io::{self, Read};

/// Hash a reader without buffering the entire file. An empty reader hashes to
/// the ordinary MD5 of the empty byte string; read failures are not hashes.
pub fn calculate_md5(mut reader: impl Read) -> io::Result<[u8; 16]> {
    let mut hasher = Md5::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hasher.finalize().into())
}

/// Compute the exact padded RFC 4648 Base64 length without overflow.
pub fn base64_len(input_len: usize) -> Option<usize> {
    input_len
        .checked_div(3)?
        .checked_mul(4)?
        .checked_add(if input_len % 3 == 0 { 0 } else { 4 })
}

/// Encode into caller-owned storage; never allocates an intermediate string.
pub fn to_base64(input: &[u8], output: &mut [u8]) -> Option<usize> {
    let len = base64_len(input.len())?;
    if output.len() < len {
        return None;
    }
    STANDARD.encode_slice(input, output).ok()
}
