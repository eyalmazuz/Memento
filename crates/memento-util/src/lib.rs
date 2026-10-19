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

//! Rust ports of Memento utility, furigana, and OCR algorithms. The C++
//! application currently consumes the opt-in utility static library through [`capi`].

pub mod furiganautils;
pub use furiganautils::{simplify_surface, FuriganaSpan};

pub mod furiganaparser;

pub mod ocrutils;
pub mod ocrutils_capi;

pub mod capi;

pub mod fileutils;
pub mod fileutils_capi;

pub mod directoryutils;
pub mod directoryutils_capi;

pub mod imageutils;
pub mod imageutils_capi;

pub mod quickutils;
