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

//! Persistent in-process `libmecab` (`libloading`) tagger with a warm piped
//! `mecab` subprocess daemon fallback.

use std::ffi::{c_char, c_void, CStr};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Mutex, OnceLock};

type MecabNew2Fn = unsafe extern "C" fn(*const c_char) -> *mut c_void;
type MecabSparseToStr2Fn =
    unsafe extern "C" fn(*mut c_void, *const c_char, usize) -> *const c_char;
type MecabNbestSparseToStr2Fn =
    unsafe extern "C" fn(*mut c_void, usize, *const c_char, usize) -> *const c_char;
type MecabStrerrorFn = unsafe extern "C" fn(*mut c_void) -> *const c_char;
type MecabDestroyFn = unsafe extern "C" fn(*mut c_void);

/// Morphological token extracted from MeCab (supporting both 9-column IPADic and
/// 10/17/26/29+ column UniDic) for dictionary lookup and compound query generation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MecabLookupToken {
    /// Raw surface slice from the input text, including any leading whitespace before this token.
    pub surface: String,
    /// Clean morpheme surface without leading whitespace.
    pub surface_clean: String,
    /// Primary dictionary base form (`orthBase` col 10 in UniDic matching input script,
    /// or `base_form` col 6 in IPADic, with UniDic `-variant` suffixes stripped).
    pub lemma: String,
    /// Secondary canonical lemma when UniDic `lemma` (col 7, trimmed before `'-'`)
    /// differs from `lemma` (e.g. `"仕舞う"` alongside `"しまう"`, `"為る"` alongside `"する"`).
    pub alt_lemma: Option<String>,
    /// Orthographic surface reading in hiragana (or empty when unavailable/`*`).
    pub reading: String,
    /// Primary part-of-speech tag (`pos1`, col 0, e.g. `"動詞"`, `"形容詞"`, `"名詞"`, `"助詞"`).
    pub pos: String,
    /// Conjugation type (`cType`, col 4, e.g. `"五段-カ行"`, `"下一段-バ行"`, `"一段"`, `"サ行変格"`).
    pub conj_type: String,
}

/// Dynamically loaded in-process `libmecab` tagger.
///
/// The internal buffer returned by `mecab_sparse_tostr2` belongs to the
/// `mecab_t` instance and is freed only when `mecab_destroy` is called on the
/// owned `tagger` pointer in [`Drop`].
pub struct LibMecabTagger {
    tagger: *mut c_void,
    mecab_sparse_tostr2: MecabSparseToStr2Fn,
    mecab_nbest_sparse_tostr2: Option<MecabNbestSparseToStr2Fn>,
    mecab_strerror: MecabStrerrorFn,
    mecab_destroy: MecabDestroyFn,
    _lib: libloading::Library,
}

// Safety: `mecab_t` is only accessed through `&mut self` (guarded by a `Mutex`
// when shared across threads).
unsafe impl Send for LibMecabTagger {}

impl LibMecabTagger {
    /// Load `libmecab` from standard platform library names with default options.
    pub fn new() -> Result<Self, String> {
        let default_arg = crate::quickutils::format_mecab_tagger_arg(false, false, b"", b"");
        Self::new_with_config(None, &default_arg)
    }

    /// Load `libmecab` from an optional explicit library path and raw tagger argument bytes
    /// (such as those produced by [`crate::quickutils::format_mecab_tagger_arg`]).
    pub fn new_with_config(lib_path: Option<&str>, tagger_arg: &[u8]) -> Result<Self, String> {
        let mut candidates: Vec<&str> = Vec::new();
        if let Some(custom) = lib_path {
            if !custom.is_empty() {
                candidates.push(custom);
            }
        }
        #[cfg(target_os = "macos")]
        {
            candidates.extend_from_slice(&[
                "libmecab.dylib",
                "libmecab.2.dylib",
                "/opt/homebrew/lib/libmecab.dylib",
                "/usr/local/lib/libmecab.dylib",
            ]);
        }
        #[cfg(target_os = "windows")]
        {
            candidates.extend_from_slice(&["libmecab.dll", "mecab.dll"]);
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            candidates.extend_from_slice(&[
                "libmecab.so.2",
                "libmecab.so",
                "/usr/lib/libmecab.so.2",
                "/usr/lib/libmecab.so",
                "/usr/lib/x86_64-linux-gnu/libmecab.so.2",
                "/usr/local/lib/libmecab.so.2",
                "/usr/local/lib/libmecab.so",
            ]);
        }

        let mut last_err = String::from("no libmecab candidate paths configured");
        let mut loaded_lib = None;
        for candidate in candidates {
            match unsafe { libloading::Library::new(candidate) } {
                Ok(lib) => {
                    loaded_lib = Some(lib);
                    break;
                }
                Err(err) => {
                    last_err = format!("{candidate}: {err}");
                }
            }
        }

        let lib = loaded_lib.ok_or_else(|| format!("Failed to load libmecab: {last_err}"))?;

        let (
            mecab_new2,
            mecab_sparse_tostr2,
            mecab_nbest_sparse_tostr2,
            mecab_strerror,
            mecab_destroy,
        ) = unsafe {
            let new2: libloading::Symbol<MecabNew2Fn> = lib
                .get(b"mecab_new2\0")
                .map_err(|e| format!("missing mecab_new2: {e}"))?;
            let sparse_tostr2: libloading::Symbol<MecabSparseToStr2Fn> = lib
                .get(b"mecab_sparse_tostr2\0")
                .map_err(|e| format!("missing mecab_sparse_tostr2: {e}"))?;
            let nbest_sparse_tostr2 = lib
                .get::<MecabNbestSparseToStr2Fn>(b"mecab_nbest_sparse_tostr2\0")
                .ok()
                .map(|sym| *sym);
            let strerror: libloading::Symbol<MecabStrerrorFn> = lib
                .get(b"mecab_strerror\0")
                .map_err(|e| format!("missing mecab_strerror: {e}"))?;
            let destroy: libloading::Symbol<MecabDestroyFn> = lib
                .get(b"mecab_destroy\0")
                .map_err(|e| format!("missing mecab_destroy: {e}"))?;
            (
                *new2,
                *sparse_tostr2,
                nbest_sparse_tostr2,
                *strerror,
                *destroy,
            )
        };

        let mut c_arg: Vec<u8> = tagger_arg.iter().copied().filter(|&b| b != 0).collect();
        c_arg.push(0);

        let tagger = unsafe { (mecab_new2)(c_arg.as_ptr() as *const c_char) };
        if tagger.is_null() {
            let err_msg = unsafe {
                let err_ptr = (mecab_strerror)(std::ptr::null_mut());
                if err_ptr.is_null() {
                    "unknown mecab_new2 error".to_string()
                } else {
                    CStr::from_ptr(err_ptr).to_string_lossy().into_owned()
                }
            };
            return Err(format!("mecab_new2 returned null: {err_msg}"));
        }

        Ok(Self {
            tagger,
            mecab_sparse_tostr2,
            mecab_nbest_sparse_tostr2,
            mecab_strerror,
            mecab_destroy,
            _lib: lib,
        })
    }

    /// Parse `text` into MeCab lattice output lines (ending with `EOS\n`).
    /// Processes each `\n`-delimited line individually.
    pub fn parse(&mut self, text: &str) -> Result<String, String> {
        if text.is_empty() {
            return Ok(String::new());
        }
        if self.tagger.is_null() {
            return Err("MeCab tagger pointer is null".into());
        }

        let mut combined = String::with_capacity(text.len() * 8);
        let mut buf: Vec<u8> = Vec::with_capacity(text.len() + 1);
        for raw_line in text.split('\n') {
            let line = raw_line.trim_end_matches('\r');
            if line.is_empty() {
                continue;
            }
            buf.clear();
            if line.as_bytes().contains(&0) {
                buf.extend(line.bytes().filter(|&b| b != 0));
            } else {
                buf.extend_from_slice(line.as_bytes());
            }
            let len = buf.len();
            if len == 0 {
                continue;
            }
            buf.push(0);

            let out_ptr = unsafe {
                (self.mecab_sparse_tostr2)(self.tagger, buf.as_ptr() as *const c_char, len)
            };
            if out_ptr.is_null() {
                let err_msg = unsafe {
                    let err_ptr = (self.mecab_strerror)(self.tagger);
                    if err_ptr.is_null() {
                        "unknown mecab_sparse_tostr2 error".to_string()
                    } else {
                        CStr::from_ptr(err_ptr).to_string_lossy().into_owned()
                    }
                };
                return Err(format!("mecab_sparse_tostr2 failed: {err_msg}"));
            }

            let piece = unsafe { CStr::from_ptr(out_ptr).to_string_lossy() };
            combined.push_str(&piece);
            if !combined.ends_with('\n') {
                combined.push('\n');
            }
        }

        Ok(combined)
    }

    /// Parse a single-line `text` into up to `n` N-best `EOS`-delimited MeCab lattice paths.
    /// Falls back to a single 1-best parse when `n <= 1` or `mecab_nbest_sparse_tostr2` is unavailable.
    pub fn parse_nbest(&mut self, text: &str, n: usize) -> Result<Vec<String>, String> {
        if text.is_empty() || n == 0 {
            return Ok(Vec::new());
        }
        let clean: String = text
            .chars()
            .filter(|&c| c != '\r' && c != '\n' && c != '\0')
            .collect();
        if clean.is_empty() {
            return Ok(Vec::new());
        }
        if n == 1 || self.mecab_nbest_sparse_tostr2.is_none() {
            let single = self.parse(&clean)?;
            return Ok(if single.is_empty() {
                Vec::new()
            } else {
                vec![single]
            });
        }
        if self.tagger.is_null() {
            return Err("MeCab tagger pointer is null".into());
        }

        let nbest_fn = self.mecab_nbest_sparse_tostr2.unwrap();
        let len = clean.len();
        let mut buf = Vec::with_capacity(len + 1);
        buf.extend_from_slice(clean.as_bytes());
        buf.push(0);

        let out_ptr =
            unsafe { (nbest_fn)(self.tagger, n, buf.as_ptr() as *const c_char, len) };
        if out_ptr.is_null() {
            let err_msg = unsafe {
                let err_ptr = (self.mecab_strerror)(self.tagger);
                if err_ptr.is_null() {
                    "unknown mecab_nbest_sparse_tostr2 error".to_string()
                } else {
                    CStr::from_ptr(err_ptr).to_string_lossy().into_owned()
                }
            };
            return Err(format!("mecab_nbest_sparse_tostr2 failed: {err_msg}"));
        }

        let raw = unsafe { CStr::from_ptr(out_ptr).to_string_lossy() };
        Ok(split_eos_blocks(&raw))
    }
}

impl Drop for LibMecabTagger {
    fn drop(&mut self) {
        if !self.tagger.is_null() {
            unsafe {
                (self.mecab_destroy)(self.tagger);
            }
            self.tagger = std::ptr::null_mut();
        }
    }
}

/// Persistent warm piped `mecab` subprocess fallback when `libmecab` cannot be loaded.
pub struct PipedMecabDaemon {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    args: Vec<String>,
}

impl PipedMecabDaemon {
    /// Spawn a persistent piped `mecab` daemon with default arguments.
    pub fn new() -> Result<Self, String> {
        Self::with_args(Vec::new())
    }

    /// Spawn a persistent piped `mecab` daemon with explicit CLI arguments.
    pub fn with_args(args: Vec<String>) -> Result<Self, String> {
        let (child, stdin, stdout) = Self::spawn_process(&args)?;
        Ok(Self {
            child,
            stdin,
            stdout,
            args,
        })
    }

    fn spawn_process(
        args: &[String],
    ) -> Result<(Child, BufWriter<ChildStdin>, BufReader<ChildStdout>), String> {
        let mut child = Command::new("mecab")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("MeCab is unavailable: {error}"))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "MeCab stdin pipe unavailable".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "MeCab stdout pipe unavailable".to_string())?;

        Ok((child, BufWriter::new(stdin), BufReader::new(stdout)))
    }

    fn restart(&mut self) -> Result<(), String> {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let (child, stdin, stdout) = Self::spawn_process(&self.args)?;
        self.child = child;
        self.stdin = stdin;
        self.stdout = stdout;
        Ok(())
    }

    fn parse_single_line(&mut self, line: &str) -> Result<String, String> {
        let clean: String = line
            .chars()
            .filter(|&c| c != '\r' && c != '\n' && c != '\0')
            .collect();
        if clean.is_empty() {
            return Ok(String::new());
        }

        self.stdin
            .write_all(clean.as_bytes())
            .and_then(|()| self.stdin.write_all(b"\n"))
            .and_then(|()| self.stdin.flush())
            .map_err(|e| format!("MeCab stdin write failed: {e}"))?;

        let mut output = String::new();
        let mut row = String::new();
        loop {
            row.clear();
            let bytes_read = self
                .stdout
                .read_line(&mut row)
                .map_err(|e| format!("MeCab stdout read failed: {e}"))?;
            if bytes_read == 0 {
                return Err("MeCab process closed stdout unexpectedly (EOF)".into());
            }
            output.push_str(&row);
            if row.trim_end_matches(&['\r', '\n'][..]) == "EOS" {
                break;
            }
        }
        Ok(output)
    }

    /// Parse `text` line-by-line so embedded `\n` never desynchronizes `EOS` framing,
    /// automatically restarting the child process once if it died.
    pub fn parse(&mut self, text: &str) -> Result<String, String> {
        if text.is_empty() {
            return Ok(String::new());
        }

        let mut combined = String::new();
        for raw_line in text.split('\n') {
            let line = raw_line.trim_end_matches('\r');
            if line.is_empty() {
                continue;
            }
            match self.parse_single_line(line) {
                Ok(piece) => combined.push_str(&piece),
                Err(_) => {
                    self.restart()?;
                    let piece = self.parse_single_line(line)?;
                    combined.push_str(&piece);
                }
            }
        }
        Ok(combined)
    }
}

impl Drop for PipedMecabDaemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

enum MecabBackend {
    InProcess(LibMecabTagger),
    Piped(PipedMecabDaemon),
}

impl MecabBackend {
    fn init() -> Result<Self, String> {
        match LibMecabTagger::new() {
            Ok(tagger) => Ok(Self::InProcess(tagger)),
            Err(_) => PipedMecabDaemon::new().map(Self::Piped),
        }
    }

    fn parse(&mut self, text: &str) -> Result<String, String> {
        match self {
            Self::InProcess(tagger) => tagger.parse(text),
            Self::Piped(daemon) => daemon.parse(text),
        }
    }

    fn parse_nbest(&mut self, text: &str, n: usize) -> Result<Vec<String>, String> {
        match self {
            Self::InProcess(tagger) => tagger.parse_nbest(text, n),
            Self::Piped(daemon) => {
                if text.is_empty() || n == 0 {
                    return Ok(Vec::new());
                }
                let out = daemon.parse(text)?;
                Ok(split_eos_blocks(&out))
            }
        }
    }
}

static GLOBAL_MECAB: OnceLock<Mutex<Option<MecabBackend>>> = OnceLock::new();

fn split_eos_blocks(raw: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current = String::new();
    for line in raw.lines() {
        let trimmed = line.trim_end_matches('\r');
        if trimmed == "EOS" {
            if !current.is_empty() {
                current.push_str("EOS\n");
                blocks.push(std::mem::take(&mut current));
            }
        } else if !trimmed.is_empty() {
            current.push_str(trimmed);
            current.push('\n');
        }
    }
    if !current.is_empty() {
        blocks.push(current);
    }
    blocks
}

/// Parse `text` using the process-wide warm MeCab backend (preferring in-process
/// `libmecab` via `libloading`, falling back to a persistent piped `mecab` daemon).
pub fn parse_mecab(text: &str) -> Result<String, String> {
    if text.is_empty() {
        return Ok(String::new());
    }
    let mutex = GLOBAL_MECAB.get_or_init(|| Mutex::new(MecabBackend::init().ok()));
    let mut guard = mutex
        .lock()
        .map_err(|_| "MeCab mutex poisoned".to_string())?;
    if guard.is_none() {
        *guard = Some(MecabBackend::init()?);
    }
    guard.as_mut().unwrap().parse(text)
}

/// Parse `text` into up to `n` N-best `EOS`-terminated lattice path blocks using
/// the process-wide warm MeCab backend.
pub fn mecab_nbest_lines(text: &str, n: usize) -> Result<Vec<String>, String> {
    if text.is_empty() || n == 0 {
        return Ok(Vec::new());
    }
    let mutex = GLOBAL_MECAB.get_or_init(|| Mutex::new(MecabBackend::init().ok()));
    let mut guard = mutex
        .lock()
        .map_err(|_| "MeCab mutex poisoned".to_string())?;
    if guard.is_none() {
        *guard = Some(MecabBackend::init()?);
    }
    guard.as_mut().unwrap().parse_nbest(text, n)
}

fn clean_feature_atom(field: &str) -> Option<&str> {
    let trimmed = field.trim();
    if trimmed.is_empty() || trimmed == "*" {
        None
    } else {
        Some(trimmed)
    }
}

fn strip_unidic_variant_suffix(raw: &str) -> &str {
    if let Some((head, _tail)) = raw.split_once('-') {
        if !head.is_empty() {
            return head;
        }
    }
    raw
}

fn strip_ipadic_variant_suffix(raw: &str) -> &str {
    if let Some((head, tail)) = raw.split_once('-') {
        if !head.is_empty()
            && matches!(
                tail,
                "動詞" | "他動詞" | "自動詞" | "名詞" | "形容詞" | "副詞" | "代名詞"
            )
        {
            return head;
        }
    }
    raw
}

fn is_single_non_kanji_kana(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(ch) = chars.next() else {
        return false;
    };
    if chars.next().is_some() {
        return false;
    }
    !crate::furiganautils::is_kanji_char(ch)
        && matches!(
            ch,
            '\u{3040}'..='\u{309F}' | '\u{30A0}'..='\u{30FF}' | '\u{FF65}'..='\u{FF9F}'
        )
}

fn find_mecab_surface_in_remaining(remaining: &str, raw_surface: &str) -> Option<(usize, usize)> {
    if !raw_surface.contains('叱') || !remaining.contains('𠮟') {
        return remaining
            .find(raw_surface)
            .map(|gap| (gap, raw_surface.len()));
    }
    let raw_chars: Vec<char> = raw_surface.chars().collect();
    for (byte_start, _) in remaining.char_indices() {
        let mut tail_chars = remaining[byte_start..].chars();
        let mut matched_bytes = 0;
        let mut ok = true;
        for &rc in &raw_chars {
            match tail_chars.next() {
                Some(tc) if tc == rc || (rc == '叱' && tc == '𠮟') => {
                    matched_bytes += tc.len_utf8();
                }
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            return Some((byte_start, matched_bytes));
        }
    }
    None
}

/// Parse a MeCab output block (supporting both 9-column IPADic and 10/17/26/29+ column UniDic)
/// aligned against `text` into morphological lookup tokens.
pub fn parse_mecab_lookup_tokens(text: &str, output: &str) -> Vec<MecabLookupToken> {
    if text.is_empty() || output.is_empty() {
        return Vec::new();
    }

    let mut tokens = Vec::new();
    let mut offset = 0usize;

    for raw_line in output.lines() {
        let line = raw_line.trim_end_matches('\r');
        if line == "EOS" {
            break;
        }
        if line.is_empty() {
            continue;
        }
        let Some((raw_surface, features)) = line.split_once('\t') else {
            continue;
        };
        if raw_surface.is_empty() || offset >= text.len() {
            continue;
        }

        let remaining = &text[offset..];
        let Some((gap, matched_len)) = find_mecab_surface_in_remaining(remaining, raw_surface)
        else {
            break;
        };

        let surface = text[offset..offset + gap + matched_len].to_owned();
        let surface_clean = text[offset + gap..offset + gap + matched_len].to_owned();
        offset += gap + matched_len;

        let fields = crate::furiganaparser::split_csv_row(features);
        let pos = fields
            .first()
            .and_then(|s| clean_feature_atom(s))
            .unwrap_or_default()
            .to_owned();
        let conj_type = fields
            .get(4)
            .and_then(|s| clean_feature_atom(s))
            .unwrap_or_default()
            .to_owned();

        let raw_reading =
            crate::furiganaparser::extract_surface_reading_from_fields(&surface_clean, &fields);
        let reading = if !raw_reading.is_empty() && raw_reading != "*" {
            let units: Vec<u16> = raw_reading.encode_utf16().collect();
            String::from_utf16_lossy(&crate::furiganaparser::katakana_to_hiragana(&units))
        } else {
            String::new()
        };

        let is_non_inflecting_pos =
            matches!(pos.as_str(), "助詞" | "記号" | "補助記号" | "空白");
        let single_kana = is_single_non_kanji_kana(&surface_clean);

        let (lemma, alt_lemma) = if fields.len() <= 9 {
            // IPADic (up to 9 columns): base_form at col 6, reading at col 7, pos at col 0.
            let base = fields
                .get(6)
                .and_then(|s| clean_feature_atom(s))
                .map(strip_ipadic_variant_suffix);
            if is_non_inflecting_pos || (single_kana && !matches!(pos.as_str(), "動詞" | "形容詞")) {
                (surface_clean.clone(), None)
            } else {
                (
                    base.unwrap_or(surface_clean.as_str()).to_owned(),
                    None,
                )
            }
        } else {
            // UniDic (10 / 17 / 26 / 29+ columns):
            // col 6 = lForm, col 7 = lemma, col 8 = orth, col 9 = pron, col 10 = orthBase.
            let orth_base = fields
                .get(10)
                .and_then(|s| clean_feature_atom(s))
                .map(strip_unidic_variant_suffix);
            let dict_lemma = fields
                .get(7)
                .and_then(|s| clean_feature_atom(s))
                .map(strip_unidic_variant_suffix);

            if is_non_inflecting_pos {
                let primary = orth_base
                    .filter(|&ob| ob == surface_clean)
                    .unwrap_or(surface_clean.as_str())
                    .to_owned();
                (primary, None)
            } else if single_kana && !matches!(pos.as_str(), "動詞" | "形容詞") {
                (surface_clean.clone(), None)
            } else {
                let primary = orth_base
                    .or(dict_lemma)
                    .unwrap_or(surface_clean.as_str())
                    .to_owned();
                let alt = dict_lemma
                    .filter(|&dl| dl != primary)
                    .map(str::to_owned);
                (primary, alt)
            }
        };

        tokens.push(MecabLookupToken {
            surface,
            surface_clean,
            lemma,
            alt_lemma,
            reading,
            pos,
            conj_type,
        });
    }

    tokens
}

/// Tokenize `text` for dictionary lookup using the warm in-process `libmecab` tagger
/// (or piped `mecab` daemon fallback), supporting both IPADic and UniDic feature layouts.
pub fn mecab_tokenize_for_lookup(text: &str) -> Result<Vec<MecabLookupToken>, String> {
    if text.is_empty() {
        return Ok(Vec::new());
    }

    // Substitute non-BMP Joyo `𠮟` (U+20B9F) with BMP `叱` (U+53F1) for MeCab lookup
    // while `parse_mecab_lookup_tokens` slices exact original spans from `text`.
    let sanitized: String;
    let query_ref = if text.contains('𠮟') || text.contains('\n') || text.contains('\r') {
        sanitized = text
            .chars()
            .map(|c| match c {
                '𠮟' => '叱',
                '\r' | '\n' => ' ',
                other => other,
            })
            .collect();
        sanitized.as_str()
    } else {
        text
    };

    let output = parse_mecab(query_ref)?;
    Ok(parse_mecab_lookup_tokens(text, &output))
}

/// Alias for [`mecab_tokenize_for_lookup`].
pub fn tokenize_for_lookup(text: &str) -> Result<Vec<MecabLookupToken>, String> {
    mecab_tokenize_for_lookup(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn in_process_libmecab_parses_and_drops_cleanly_under_100us() {
        let mut tagger = LibMecabTagger::new().expect("libmecab.so.2 should load on this system");
        // Warm up mmap pages once
        let cues = ["来ない", "見ない", "行った", "王さま", "氷", "お兄さん", "人々", "食べる"];
        for cue in &cues {
            let first = tagger.parse(cue).expect("warm up parse");
            assert!(first.contains("EOS"));
        }

        // Measure warm latency across multiple cues
        let iterations = 10;
        let start = Instant::now();
        for _ in 0..iterations {
            for cue in &cues {
                let out = tagger.parse(cue).expect("warm parse");
                assert!(out.contains("EOS"));
            }
        }
        let avg_micros =
            start.elapsed().as_micros() as f64 / (cues.len() * iterations) as f64;
        assert!(
            avg_micros < 100.0,
            "warm in-process libmecab should take <100us (0.1ms) per cue, took {avg_micros:.2}us"
        );
    }

    #[test]
    fn piped_mecab_daemon_handles_multiline_and_auto_restarts() {
        let mut daemon = PipedMecabDaemon::new().expect("mecab binary should spawn");
        let out = daemon.parse("昨日\n行った").expect("multiline parse");
        assert!(out.contains("昨日\t"));
        assert!(out.contains("行っ\t"));

        // Simulate child crash and verify transparent auto-restart
        let _ = daemon.child.kill();
        let _ = daemon.child.wait();
        let recovered = daemon.parse("食べる").expect("daemon should auto-restart");
        assert!(recovered.contains("食べる\t"));
    }

    #[test]
    fn parses_ipadic_9col_and_unidic_orthbase_lemma_variants_cleanly() {
        // IPADic 9-column fixture (with leading space before second token + unknown `*`):
        let ipadic_out = "食べ\t動詞,自立,*,*,一段,連用形,食べる,タベ,タベ\n\
て\t助詞,接続助詞,*,*,*,*,て,テ,テ\n\
しまっ\t動詞,非自立,*,*,五段・ワ行促音便,連用タ接続,しまう,シマッ,シマッ\n\
た\t助動詞,*,*,*,特殊・タ,基本形,た,タ,タ\nEOS\n";
        let ipadic_tokens = parse_mecab_lookup_tokens("食べて しまった", ipadic_out);
        assert_eq!(ipadic_tokens.len(), 4);
        assert_eq!(ipadic_tokens[0].surface, "食べ");
        assert_eq!(ipadic_tokens[0].surface_clean, "食べ");
        assert_eq!(ipadic_tokens[0].lemma, "食べる");
        assert_eq!(ipadic_tokens[0].reading, "たべ");
        assert_eq!(ipadic_tokens[0].pos, "動詞");
        assert_eq!(ipadic_tokens[0].conj_type, "一段");
        assert_eq!(ipadic_tokens[2].surface, " しまっ");
        assert_eq!(ipadic_tokens[2].surface_clean, "しまっ");
        assert_eq!(ipadic_tokens[2].lemma, "しまう");
        assert_eq!(ipadic_tokens[2].alt_lemma, None);

        // UniDic 29-column fixture with `orthBase` col 10 vs `lemma` col 7 and `-` variant suffixes:
        let unidic_out = "し\t動詞,非自立可能,,,サ行変格,連用形-一般,スル,為る-動詞,し,シ,する,スル,和,\"\",\"\",\"\",\"\",\"\",\"\",用,シ,スル,シ,スル,\"0\",\"C5\",\"\",1,1\n\
て\t助詞,接続助詞,,,,,テ,て,て,テ,て,テ,和,\"\",\"\",\"\",\"\",\"\",\"\",接助,テ,テ,テ,テ,\"\",\"\",\"\",1,1\n\
い\t動詞,非自立可能,,,上一段-ア行,連用形-一般,イル,居る,い,イ,いる,イル,和,\"\",\"\",\"\",\"\",\"\",\"\",用,イ,イル,イ,イル,\"0\",\"C4\",\"\",1,1\n\
行っ\t動詞,非自立可能,,,五段-カ行,連用形-促音便,イク,行く-他動詞,行っ,イッ,行く,イク,和,\"\",\"\",\"\",\"\",\"\",\"\",用,イッ,イク,イッ,イク,\"0\",\"C2\",\"\",1,1\n\
コーヒー\t名詞,普通名詞,一般,,,,コーヒー,コーヒー-coffee,コーヒー,コーヒー,コーヒー,コーヒー,外,\"\",\"\",\"\",\"\",\"\",\"\",体,コーヒー,コーヒー,コーヒー,コーヒー,\"3\",\"C2\",\"\",1,1\nEOS\n";
        let unidic_tokens = parse_mecab_lookup_tokens("してい行ったコーヒー", unidic_out);
        assert_eq!(unidic_tokens.len(), 5);
        assert_eq!(unidic_tokens[0].lemma, "する");
        assert_eq!(unidic_tokens[0].alt_lemma.as_deref(), Some("為る"));
        assert_eq!(unidic_tokens[1].lemma, "て");
        assert_eq!(unidic_tokens[1].alt_lemma, None);
        assert_eq!(unidic_tokens[2].lemma, "いる");
        assert_eq!(unidic_tokens[2].alt_lemma.as_deref(), Some("居る"));
        assert_eq!(unidic_tokens[3].lemma, "行く");
        assert_eq!(unidic_tokens[3].alt_lemma, None);
        assert_eq!(unidic_tokens[4].lemma, "コーヒー");
        assert_eq!(unidic_tokens[4].alt_lemma, None);
    }

    #[test]
    fn live_unidic_tokenize_for_lookup_nbest_and_sub_millisecond_latency() {
        let shite = mecab_tokenize_for_lookup("していました").expect("live mecab should succeed");
        assert!(shite.len() >= 4);
        assert_eq!(shite[0].surface_clean, "し");
        assert_eq!(shite[0].lemma, "する");
        assert_eq!(shite[0].alt_lemma.as_deref(), Some("為る"));
        assert_eq!(shite[1].lemma, "て");
        assert_eq!(shite[2].lemma, "いる");
        assert_eq!(shite[2].alt_lemma.as_deref(), Some("居る"));

        let tabete = mecab_tokenize_for_lookup("食べてしまいました").expect("live mecab");
        assert_eq!(tabete[0].lemma, "食べる");
        assert_eq!(tabete[1].lemma, "て");
        assert_eq!(tabete[2].lemma, "しまう");
        assert_eq!(tabete[2].alt_lemma.as_deref(), Some("仕舞う"));

        // Single-kana particles never emit verb/noun lemmas:
        for particle in ["は", "の", "と"] {
            let p_toks = mecab_tokenize_for_lookup(particle).expect("particle parse");
            assert_eq!(p_toks.len(), 1);
            assert_eq!(p_toks[0].lemma, particle);
            assert_eq!(p_toks[0].alt_lemma, None);
            assert_eq!(p_toks[0].pos, "助詞");
        }

        // N-best paths:
        let nbest = mecab_nbest_lines("いっていた", 3).expect("nbest should succeed");
        assert!(!nbest.is_empty());

        // Invalid config returns Err cleanly:
        assert!(LibMecabTagger::new_with_config(
            None,
            b"-d /definitely/missing/mecab/dic -r /definitely/missing/mecabrc"
        )
        .is_err());

        // Warm lookup latency < 1ms (< 1000us):
        let inputs = [
            "食べてしまいました",
            "食べさせられてしまった",
            "見ておいた",
            "日本語能力試験",
            "していました",
        ];
        let iterations = 10;
        let start = Instant::now();
        for _ in 0..iterations {
            for input in &inputs {
                let toks = mecab_tokenize_for_lookup(input).expect("warm lookup");
                assert!(!toks.is_empty());
            }
        }
        let avg_micros =
            start.elapsed().as_micros() as f64 / (inputs.len() * iterations) as f64;
        assert!(
            avg_micros < 1000.0,
            "mecab_tokenize_for_lookup must run under 1ms (1000us), took {avg_micros:.2}us"
        );
    }
}
