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

//! Port of Memento's Whisper ASR inference engine (`src/asr/whisper`).
//!
//! This crate binds the same whisper.cpp build the C++ application links
//! (FetchContent v1.9.4, see `extern/CMakeLists.txt`) through its C API and
//! reproduces the C++ `WhisperModel`/`WhisperBackend` behavior, exposed to
//! C++ via a C ABI (`capi`, declared in the header
//! `src/asr/rust/asr_whisper_capi.h`).
//!
//! Layout:
//! - [`engine`]: file-by-file port of `whispermodel.cpp` (WAV decode, full
//!   parameters, segment loop, abort handling, failure strings).
//! - [`capi`]: `extern "C"` surface consumed by the C++ shim in
//!   `src/asr/whisper/whispermodel.cpp` (`MEMENTO_ASR_WHISPER_RUST`).
//! - [`whisper_sys`]: bindgen bindings to the fetched `whisper.h` (build.rs).
//!
//! The `WhisperBackend` QFuture/retry machinery stays in C++ (see the port
//! plan): the Rust side performs exactly one transcription attempt per call.

pub mod capi;
pub mod engine;
pub mod whisper_sys;
