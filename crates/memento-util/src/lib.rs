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

//! Rust ports of Memento utility, furigana, and OCR algorithms.

pub mod furiganautils;
pub use furiganautils::{
    contains_kanji, distribute_furigana, distribute_furigana_str, is_kanji_char, simplify_surface,
    simplify_surface_str, FuriganaSpan,
};

pub mod furiganaparser;

pub mod mecab;

pub mod ocrutils;

pub mod fileutils;

pub mod imageutils;

pub mod quickutils;
