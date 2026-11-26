//! Persistent in-process `libmecab` (`libloading`) tagger with a warm piped
//! `mecab` subprocess daemon fallback.

use std::borrow::Cow;
use std::ffi::{c_char, c_void, CStr};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;

/// Returns `true` when `path` contains whitespace or non-ASCII characters that
/// require Win32 8.3 short-path conversion (`GetShortPathNameW`) before passing
/// raw option bytes into `libmecab`'s ANSI `mecab_new2`.
pub fn path_needs_windows_short_name(path: &str) -> bool {
    path.chars().any(|c| !c.is_ascii() || c.is_ascii_whitespace())
}

fn quote_mecab_arg_bytes(bytes: &[u8]) -> Cow<'_, [u8]> {
    let needs_quotes = bytes.iter().any(|&b| b == b' ' || b == b'\t');
    let already_quoted = bytes.len() >= 2 && bytes.first() == Some(&b'"') && bytes.last() == Some(&b'"');
    if !needs_quotes || already_quoted {
        return Cow::Borrowed(bytes);
    }
    let mut quoted = Vec::with_capacity(bytes.len() + 2);
    quoted.push(b'"');
    for &b in bytes {
        if b == b'"' {
            quoted.push(b'\\');
        }
        quoted.push(b);
    }
    quoted.push(b'"');
    Cow::Owned(quoted)
}

/// Format raw `libmecab` tagger initialization option bytes for a discovered
/// dictionary directory (`dicdir`) and optional resource file (`dicrc`).
///
/// - Empty `dicdir_bytes` returns an empty vector so `libmecab` falls back to
///   its system default `mecabrc`.
/// - Preserves raw ANSI / Windows 8.3 short-path bytes without UTF-8
///   re-encoding, and quotes arguments containing spaces.
pub fn format_dicdir_tagger_arg(
    is_windows: bool,
    dicdir_bytes: &[u8],
    dicrc_bytes: &[u8],
) -> Vec<u8> {
    if dicdir_bytes.is_empty() {
        return Vec::new();
    }

    let dicdir_arg = quote_mecab_arg_bytes(dicdir_bytes);
    let dicrc_arg = quote_mecab_arg_bytes(dicrc_bytes);

    let mut arg = Vec::with_capacity(8 + dicdir_arg.len() + dicrc_arg.len());
    arg.extend_from_slice(if is_windows { b" -d " } else { b"-d " });
    arg.extend_from_slice(&dicdir_arg);
    if !dicrc_arg.is_empty() {
        arg.extend_from_slice(b" -r ");
        arg.extend_from_slice(&dicrc_arg);
    }
    arg
}

/// Returns `true` if `dir` is an existing directory containing a MeCab
/// dictionary (`dicrc` or `sys.dic`).
pub fn is_valid_mecab_dicdir(dir: &Path) -> bool {
    dir.is_dir() && (dir.join("dicrc").is_file() || dir.join("sys.dic").is_file())
}

/// Resolve the `dicrc` resource file inside `dicdir`, if present.
pub fn resolve_mecab_dicrc(dicdir: &Path) -> Option<PathBuf> {
    let dicrc = dicdir.join("dicrc");
    dicrc.is_file().then_some(dicrc)
}

/// Build the ordered list of explicit override MeCab dictionary candidates
/// (`MECAB_DICDIR` env var and executable-adjacent / macOS `.app` bundled paths).
pub fn mecab_dicdir_override_candidates(
    env_dicdir: Option<&Path>,
    exe_dir: Option<&Path>,
    target_os: &str,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(env_dir) = env_dicdir {
        if !env_dir.as_os_str().is_empty() {
            candidates.push(env_dir.to_path_buf());
        }
    }
    if let Some(exe_dir) = exe_dir {
        candidates.push(exe_dir.join("ipadic"));
        candidates.push(exe_dir.join("unidic"));
        candidates.push(exe_dir.join("dic"));
        candidates.push(exe_dir.join("share").join("mecab").join("dic").join("ipadic"));
        candidates.push(exe_dir.join("share").join("mecab").join("dic").join("unidic"));

        if target_os == "macos" {
            if let Some(contents_dir) = exe_dir.parent() {
                let resources = contents_dir.join("Resources");
                candidates.push(resources.join("ipadic"));
                candidates.push(resources.join("unidic"));
                candidates.push(resources.join("dic"));
                candidates.push(
                    resources
                        .join("share")
                        .join("mecab")
                        .join("dic")
                        .join("ipadic"),
                );
                candidates.push(
                    resources
                        .join("share")
                        .join("mecab")
                        .join("dic")
                        .join("unidic"),
                );
            }
        }
    }
    candidates
}

/// Build the ordered list of platform-standard system MeCab dictionary
/// locations (macOS Homebrew, standard Linux paths, Windows Program Files).
pub fn mecab_dicdir_system_candidates(target_os: &str) -> Vec<PathBuf> {
    match target_os {
        "macos" => vec![
            PathBuf::from("/opt/homebrew/lib/mecab/dic/ipadic"),
            PathBuf::from("/opt/homebrew/lib/mecab/dic/unidic"),
            PathBuf::from("/usr/local/lib/mecab/dic/ipadic"),
            PathBuf::from("/usr/local/lib/mecab/dic/unidic"),
        ],
        "windows" => vec![
            PathBuf::from(r"C:\Program Files\MeCab\dic\ipadic"),
            PathBuf::from(r"C:\Program Files (x86)\MeCab\dic\ipadic"),
        ],
        _ => vec![
            PathBuf::from("/var/lib/mecab/dic/debian"),
            PathBuf::from("/usr/lib/x86_64-linux-gnu/mecab/dic/unidic"),
            PathBuf::from("/usr/lib/x86_64-linux-gnu/mecab/dic/ipadic"),
            PathBuf::from("/usr/lib/aarch64-linux-gnu/mecab/dic/unidic"),
            PathBuf::from("/usr/lib/aarch64-linux-gnu/mecab/dic/ipadic"),
            PathBuf::from("/usr/lib/mecab/dic/unidic"),
            PathBuf::from("/usr/lib/mecab/dic/ipadic"),
            PathBuf::from("/usr/share/mecab/dic/unidic"),
            PathBuf::from("/usr/share/mecab/dic/ipadic"),
            PathBuf::from("/usr/local/lib/mecab/dic/unidic"),
            PathBuf::from("/usr/local/lib/mecab/dic/ipadic"),
        ],
    }
}

/// Probe `override_candidates` first and then `system_candidates`, returning
/// the first directory passing [`is_valid_mecab_dicdir`].
pub fn discover_mecab_dicdir_in(
    override_candidates: &[PathBuf],
    system_candidates: &[PathBuf],
) -> Option<PathBuf> {
    override_candidates
        .iter()
        .chain(system_candidates.iter())
        .find(|dir| is_valid_mecab_dicdir(dir))
        .cloned()
}

/// Discover an explicit MeCab dictionary override (`MECAB_DICDIR` or
/// executable-adjacent bundled dictionary).
pub fn discover_mecab_override_dicdir() -> Option<PathBuf> {
    let env_dicdir = std::env::var_os("MECAB_DICDIR").map(PathBuf::from);
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    let overrides = mecab_dicdir_override_candidates(
        env_dicdir.as_deref(),
        exe_dir.as_deref(),
        std::env::consts::OS,
    );
    overrides.into_iter().find(|dir| is_valid_mecab_dicdir(dir))
}

/// Automatically discover a MeCab dictionary directory by probing `MECAB_DICDIR`,
/// executable-adjacent bundled directories, macOS Homebrew paths, and standard
/// OS paths.
pub fn discover_mecab_dicdir() -> Option<PathBuf> {
    let env_dicdir = std::env::var_os("MECAB_DICDIR").map(PathBuf::from);
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    let overrides = mecab_dicdir_override_candidates(
        env_dicdir.as_deref(),
        exe_dir.as_deref(),
        std::env::consts::OS,
    );
    let system = mecab_dicdir_system_candidates(std::env::consts::OS);
    discover_mecab_dicdir_in(&overrides, &system)
}

#[cfg(target_os = "windows")]
mod win_mecab_ffi {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use super::path_needs_windows_short_name;

    const CP_ACP: u32 = 0;
    const CP_UTF8: u32 = 65001;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetShortPathNameW(
            lpsz_long_path: *const u16,
            lpsz_short_path: *mut u16,
            cch_buffer: u32,
        ) -> u32;
        fn WideCharToMultiByte(
            code_page: u32,
            dw_flags: u32,
            lp_wide_char_str: *const u16,
            cch_wide_char: i32,
            lp_multi_byte_str: *mut u8,
            cb_multi_byte: i32,
            lp_default_char: *const u8,
            lp_used_default_char: *mut i32,
        ) -> i32;
    }

    fn short_path_wide(wide_nul: &[u16]) -> Option<Vec<u16>> {
        let needed = unsafe { GetShortPathNameW(wide_nul.as_ptr(), std::ptr::null_mut(), 0) };
        if needed == 0 {
            return None;
        }
        let mut buf = vec![0u16; needed as usize];
        let written =
            unsafe { GetShortPathNameW(wide_nul.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
        if written > 0 && (written as usize) < buf.len() {
            buf.truncate(written as usize);
            Some(buf)
        } else {
            None
        }
    }

    fn wide_to_codepage(wide: &[u16], code_page: u32) -> Option<Vec<u8>> {
        if wide.is_empty() {
            return Some(Vec::new());
        }
        let check_default = code_page != CP_UTF8;
        let mut used_default: i32 = 0;
        let needed = unsafe {
            let used_ptr = if check_default {
                &mut used_default as *mut i32
            } else {
                std::ptr::null_mut()
            };
            WideCharToMultiByte(
                code_page,
                0,
                wide.as_ptr(),
                wide.len() as i32,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
                used_ptr,
            )
        };
        if needed <= 0 || (check_default && used_default != 0) {
            return None;
        }
        let mut out = vec![0u8; needed as usize];
        used_default = 0;
        let written = unsafe {
            let used_ptr = if check_default {
                &mut used_default as *mut i32
            } else {
                std::ptr::null_mut()
            };
            WideCharToMultiByte(
                code_page,
                0,
                wide.as_ptr(),
                wide.len() as i32,
                out.as_mut_ptr(),
                out.len() as i32,
                std::ptr::null(),
                used_ptr,
            )
        };
        if written <= 0 || (check_default && used_default != 0) {
            return None;
        }
        out.truncate(written as usize);
        Some(out)
    }

    /// Convert a Windows path to raw bytes suitable for `libmecab`'s ANSI
    /// `mecab_new2`: converts paths with spaces or non-ASCII characters via
    /// `GetShortPathNameW` (8.3 short path) and encodes UTF-16 to `CP_ACP`
    /// (falling back to UTF-8 if lossy under the active ANSI code page).
    pub(super) fn path_to_windows_mecab_bytes(path: &Path) -> Vec<u8> {
        let wide_nul: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let short_owned = if path_needs_windows_short_name(&path.to_string_lossy()) {
            short_path_wide(&wide_nul)
        } else {
            None
        };
        let effective_wide: &[u16] = match short_owned.as_deref() {
            Some(short) => short,
            None => &wide_nul[..wide_nul.len().saturating_sub(1)],
        };
        wide_to_codepage(effective_wide, CP_ACP)
            .or_else(|| wide_to_codepage(effective_wide, CP_UTF8))
            .unwrap_or_else(|| String::from_utf16_lossy(effective_wide).into_bytes())
    }
}

fn path_to_mecab_arg_bytes(path: &Path) -> Vec<u8> {
    #[cfg(target_os = "windows")]
    {
        win_mecab_ffi::path_to_windows_mecab_bytes(path)
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(not(any(unix, target_os = "windows")))]
    {
        path.to_string_lossy().into_owned().into_bytes()
    }
}

fn build_mecab_tagger_arg_for_dicdir(is_windows: bool, dicdir: &Path) -> Vec<u8> {
    let dicdir_bytes = path_to_mecab_arg_bytes(dicdir);
    let dicrc_bytes = resolve_mecab_dicrc(dicdir)
        .map(|dicrc| path_to_mecab_arg_bytes(&dicrc))
        .unwrap_or_default();
    format_dicdir_tagger_arg(is_windows, &dicdir_bytes, &dicrc_bytes)
}

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
    /// Load `libmecab` from standard platform library names, automatically
    /// probing `MECAB_DICDIR`, executable-adjacent bundled dictionaries,
    /// system default `mecabrc`, and platform-standard paths (Homebrew / Linux /
    /// Windows), converting Windows `dicdir` paths containing spaces or
    /// non-ASCII characters via `GetShortPathNameW`.
    pub fn new() -> Result<Self, String> {
        let is_windows = cfg!(target_os = "windows");

        // 1. Explicit overrides (`MECAB_DICDIR` or executable-adjacent bundled dictionary).
        if let Some(override_dir) = discover_mecab_override_dicdir() {
            let arg = build_mecab_tagger_arg_for_dicdir(is_windows, &override_dir);
            if let Ok(tagger) = Self::new_with_config(None, &arg) {
                return Ok(tagger);
            }
        }

        // 2. On Unix/macOS, prefer system default `mecabrc` when no explicit override
        //    is configured so custom `/etc/mecabrc` setups (e.g. UniDic) are respected.
        let mut default_err: Option<String> = None;
        if !is_windows {
            match Self::new_with_config(None, &[]) {
                Ok(tagger) => return Ok(tagger),
                Err(err) => default_err = Some(err),
            }
        }

        // 3. Probe platform-standard dictionary directories (Homebrew, Linux, Windows).
        let mut last_err = default_err;
        for candidate_dir in mecab_dicdir_system_candidates(std::env::consts::OS) {
            if !is_valid_mecab_dicdir(&candidate_dir) {
                continue;
            }
            let arg = build_mecab_tagger_arg_for_dicdir(is_windows, &candidate_dir);
            match Self::new_with_config(None, &arg) {
                Ok(tagger) => return Ok(tagger),
                Err(err) => last_err = Some(err),
            }
        }

        // 4. Final fallback to bare system default `mecabrc` on Windows (or return
        //    the recorded error on Unix where step 2 already attempted it).
        if is_windows {
            Self::new_with_config(None, &[])
        } else {
            Err(last_err.unwrap_or_else(|| "Failed to initialize libmecab".to_owned()))
        }
    }

    /// Load `libmecab` from an optional explicit library path and raw tagger argument bytes
    /// (such as those produced by [`format_dicdir_tagger_arg`]).
    pub fn new_with_config(lib_path: Option<&str>, tagger_arg: &[u8]) -> Result<Self, String> {
        let mut candidates: Vec<String> = Vec::new();
        if let Some(custom) = lib_path {
            if !custom.is_empty() {
                candidates.push(custom.to_owned());
            }
        }
        #[cfg(target_os = "macos")]
        {
            if let Ok(exe) = std::env::current_exe() {
                if let Some(contents) = exe.parent().and_then(Path::parent) {
                    candidates.push(
                        contents
                            .join("Frameworks")
                            .join("libmecab.dylib")
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
            }
            candidates.extend(
                [
                    "libmecab.dylib",
                    "libmecab.2.dylib",
                    "/opt/homebrew/lib/libmecab.dylib",
                    "/usr/local/lib/libmecab.dylib",
                ]
                .into_iter()
                .map(str::to_owned),
            );
        }
        #[cfg(target_os = "windows")]
        {
            if let Ok(exe) = std::env::current_exe() {
                if let Some(exe_dir) = exe.parent() {
                    candidates.push(exe_dir.join("libmecab.dll").to_string_lossy().into_owned());
                    candidates.push(exe_dir.join("mecab.dll").to_string_lossy().into_owned());
                }
            }
            candidates.extend(["libmecab.dll", "mecab.dll"].into_iter().map(str::to_owned));
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            candidates.extend(
                [
                    "libmecab.so.2",
                    "libmecab.so",
                    "/usr/lib/libmecab.so.2",
                    "/usr/lib/libmecab.so",
                    "/usr/lib/x86_64-linux-gnu/libmecab.so.2",
                    "/usr/local/lib/libmecab.so.2",
                    "/usr/local/lib/libmecab.so",
                ]
                .into_iter()
                .map(str::to_owned),
            );
        }

        let mut last_err = String::from("no libmecab candidate paths configured");
        let mut loaded_lib = None;
        for candidate in &candidates {
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

        // `mecab_new2` stores initialization error strings in a non-thread-safe
        // global buffer inside `libmecab` read by `mecab_strerror(NULL)`.
        // Serialize tagger creation + global error reads across threads.
        let tagger = {
            let _init_guard = MECAB_INIT_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
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
            tagger
        };

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
            // Replace embedded NUL bytes with ASCII space instead of deleting them
            // so MeCab never glues tokens across a NUL byte and desynchronizes
            // downstream surface alignment against the original text.
            if line.as_bytes().contains(&0) {
                buf.extend(line.bytes().map(|b| if b == 0 { b' ' } else { b }));
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
            .map(|c| match c {
                '\r' | '\n' | '\0' => ' ',
                other => other,
            })
            .collect();
        if clean.bytes().all(|b| b == b' ') {
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
    /// Spawn a persistent piped `mecab` daemon, automatically honoring an
    /// explicit `MECAB_DICDIR` or executable-adjacent bundled dictionary override
    /// when present and falling back to system default `mecabrc` otherwise.
    pub fn new() -> Result<Self, String> {
        if let Some(override_dir) = discover_mecab_override_dicdir() {
            let mut args = vec!["-d".to_owned(), override_dir.to_string_lossy().into_owned()];
            if let Some(dicrc) = resolve_mecab_dicrc(&override_dir) {
                args.push("-r".to_owned());
                args.push(dicrc.to_string_lossy().into_owned());
            }
            if let Ok(daemon) = Self::with_args(args) {
                return Ok(daemon);
            }
        }
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
            .map(|c| match c {
                '\r' | '\n' | '\0' => ' ',
                other => other,
            })
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

enum MecabInit<B = MecabBackend> {
    Uninitialized,
    Ready(B),
    Unavailable(String),
}

impl<B> MecabInit<B> {
    fn is_ready(&self) -> bool {
        matches!(self, Self::Ready(_))
    }

    fn take_reset(&mut self) -> Self {
        std::mem::replace(self, Self::Uninitialized)
    }

    fn get_or_init_with<F>(&mut self, init: F) -> Result<&mut B, String>
    where
        F: FnOnce() -> Result<B, String>,
    {
        if matches!(self, Self::Uninitialized) {
            *self = match init() {
                Ok(backend) => Self::Ready(backend),
                Err(err) => Self::Unavailable(err),
            };
        }
        match self {
            Self::Ready(backend) => Ok(backend),
            Self::Unavailable(err) => Err(err.clone()),
            Self::Uninitialized => unreachable!(),
        }
    }
}

static MECAB_INIT_LOCK: Mutex<()> = Mutex::new(());
static GLOBAL_MECAB: Mutex<MecabInit> = Mutex::new(MecabInit::Uninitialized);

/// Returns `true` if the process-wide MeCab backend is currently loaded in
/// memory without triggering lazy initialization.
pub fn is_mecab_loaded() -> bool {
    let guard = match GLOBAL_MECAB.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.is_ready()
}

/// Drop the process-wide cached MeCab backend (unloading `libmecab` or
/// terminating and reaping the piped `mecab` daemon) and reset the cache so a
/// subsequent parse re-initializes cleanly.
///
/// Returns `true` iff a loaded `MecabInit::Ready` backend was actually dropped.
pub fn unload_mecab() -> bool {
    let prev = {
        let mut guard = match GLOBAL_MECAB.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.take_reset()
    };
    matches!(prev, MecabInit::Ready(_))
}

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
///
/// Initialization failure is cached as unavailable so callers do not repeatedly
/// probe shared libraries or spawn subprocesses; call [`unload_mecab`] to reset.
pub fn parse_mecab(text: &str) -> Result<String, String> {
    if text.is_empty() {
        return Ok(String::new());
    }
    let mut guard = GLOBAL_MECAB
        .lock()
        .map_err(|_| "MeCab mutex poisoned".to_string())?;
    let backend = guard.get_or_init_with(MecabBackend::init)?;
    backend.parse(text)
}

/// Parse `text` into up to `n` N-best `EOS`-terminated lattice path blocks using
/// the process-wide warm MeCab backend.
pub fn mecab_nbest_lines(text: &str, n: usize) -> Result<Vec<String>, String> {
    if text.is_empty() || n == 0 {
        return Ok(Vec::new());
    }
    let mut guard = GLOBAL_MECAB
        .lock()
        .map_err(|_| "MeCab mutex poisoned".to_string())?;
    let backend = guard.get_or_init_with(MecabBackend::init)?;
    backend.parse_nbest(text, n)
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
            crate::furiganaparser::katakana_to_hiragana_str(&raw_reading)
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

fn sanitize_lookup_query(text: &str) -> Cow<'_, str> {
    // Substitute non-BMP Joyo `𠮟` (U+20B9F) with BMP `叱` (U+53F1) and replace
    // line breaks / NUL bytes with spaces (never deleting them) so MeCab keeps
    // token boundaries aligned against the original `text`.
    if text.contains(['𠮟', '\n', '\r', '\0']) {
        Cow::Owned(
            text.chars()
                .map(|c| match c {
                    '𠮟' => '叱',
                    '\r' | '\n' | '\0' => ' ',
                    other => other,
                })
                .collect(),
        )
    } else {
        Cow::Borrowed(text)
    }
}

/// Tokenize `text` for dictionary lookup using the warm in-process `libmecab` tagger
/// (or piped `mecab` daemon fallback), supporting both IPADic and UniDic feature layouts.
pub fn mecab_tokenize_for_lookup(text: &str) -> Result<Vec<MecabLookupToken>, String> {
    if text.is_empty() {
        return Ok(Vec::new());
    }

    let query = sanitize_lookup_query(text);
    let output = parse_mecab(&query)?;
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

    static GLOBAL_MECAB_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn in_process_libmecab_parses_and_drops_cleanly_under_100us() {
        let Ok(mut tagger) = LibMecabTagger::new() else {
            // Skip gracefully on CI environments without system libmecab installed.
            return;
        };
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
        let budget_us = if std::env::var_os("CI").is_some() {
            2000.0
        } else {
            100.0
        };
        assert!(
            avg_micros < budget_us,
            "warm in-process libmecab should stay under {budget_us:.0}us per cue, took {avg_micros:.2}us"
        );
    }

    #[test]
    fn piped_mecab_daemon_handles_multiline_and_auto_restarts() {
        let Ok(mut daemon) = PipedMecabDaemon::new() else {
            // Skip gracefully on CI environments without the mecab CLI binary installed.
            return;
        };
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
        let _global_guard = GLOBAL_MECAB_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Ok(shite) = mecab_tokenize_for_lookup("していました") else {
            // Skip gracefully on CI environments without system MeCab installed.
            return;
        };
        let has_unidic = shite.first().and_then(|t| t.alt_lemma.as_deref()) == Some("為る");
        assert!(shite.len() >= 4);
        assert_eq!(shite[0].surface_clean, "し");
        assert_eq!(shite[0].lemma, "する");
        if has_unidic {
            assert_eq!(shite[0].alt_lemma.as_deref(), Some("為る"));
        }
        assert_eq!(shite[1].lemma, "て");
        assert_eq!(shite[2].lemma, "いる");
        if has_unidic {
            assert_eq!(shite[2].alt_lemma.as_deref(), Some("居る"));
        }

        let tabete = mecab_tokenize_for_lookup("食べてしまいました").expect("live mecab");
        assert_eq!(tabete[0].lemma, "食べる");
        assert_eq!(tabete[1].lemma, "て");
        assert_eq!(tabete[2].lemma, "しまう");
        if has_unidic {
            assert_eq!(tabete[2].alt_lemma.as_deref(), Some("仕舞う"));
        }

        // Single-kana particles never emit verb/noun lemmas (UniDic tags standalone
        // "は" as "助詞"; IPADic tags standalone "は" as "フィラー"):
        for particle in ["は", "の", "と"] {
            let p_toks = mecab_tokenize_for_lookup(particle).expect("particle parse");
            assert_eq!(p_toks.len(), 1);
            assert_eq!(p_toks[0].lemma, particle);
            assert_eq!(p_toks[0].alt_lemma, None);
            assert!(
                matches!(p_toks[0].pos.as_str(), "助詞" | "フィラー"),
                "unexpected POS for {particle}: {}",
                p_toks[0].pos
            );
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
        let budget_us = if std::env::var_os("CI").is_some() {
            5000.0
        } else {
            1000.0
        };
        assert!(
            avg_micros < budget_us,
            "mecab_tokenize_for_lookup must run under {budget_us:.0}us, took {avg_micros:.2}us"
        );
    }

    #[test]
    fn mecab_init_caches_unavailable_and_allows_reset() {
        let mut state: MecabInit<u32> = MecabInit::Uninitialized;
        let mut calls = 0usize;
        let err1 = state
            .get_or_init_with(|| {
                calls += 1;
                Err("missing mecab".to_string())
            })
            .unwrap_err();
        let err2 = state
            .get_or_init_with(|| {
                calls += 1;
                Ok(42)
            })
            .unwrap_err();
        assert_eq!(calls, 1, "Unavailable must be cached without re-probing");
        assert_eq!(err1, "missing mecab");
        assert_eq!(err2, "missing mecab");

        state = MecabInit::Uninitialized;
        let val = state
            .get_or_init_with(|| {
                calls += 1;
                Ok(99)
            })
            .unwrap();
        assert_eq!(*val, 99);
        assert_eq!(calls, 2);
    }

    #[test]
    fn is_mecab_loaded_and_unload_mecab_report_ready_drop_accurately() {
        // State-machine transitions (independent of host MeCab installation):
        let mut state: MecabInit<u32> = MecabInit::Uninitialized;
        assert!(!state.is_ready());
        assert!(!matches!(state.take_reset(), MecabInit::Ready(_)));

        let _ = state.get_or_init_with(|| Err("missing".to_string()));
        assert!(!state.is_ready());
        // Resetting Unavailable clears the error cache and reports `false` (no Ready backend dropped).
        assert!(!matches!(state.take_reset(), MecabInit::Ready(_)));
        assert!(!state.is_ready());

        assert_eq!(*state.get_or_init_with(|| Ok(7)).unwrap(), 7);
        assert!(state.is_ready());
        assert!(matches!(state.take_reset(), MecabInit::Ready(_)));
        assert!(!state.is_ready());
        assert!(!matches!(state.take_reset(), MecabInit::Ready(_)));

        // Global lazy state (serialized against live global-tagger benchmark tests):
        let _global_guard = GLOBAL_MECAB_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let _ = unload_mecab();
        assert!(!is_mecab_loaded());
        assert!(!unload_mecab());
        assert!(!is_mecab_loaded());

        // When host MeCab is available, verify lazy Ready -> unload -> Uninitialized cycle:
        if parse_mecab("猫").is_ok() {
            assert!(is_mecab_loaded());
            assert!(unload_mecab());
            assert!(!is_mecab_loaded());
            assert!(!unload_mecab());
        }
    }

    #[test]
    fn nul_byte_is_replaced_with_space_preserving_alignment() {
        assert_eq!(&*sanitize_lookup_query("食\0べる"), "食 べる");
        let mecab_out =
            "食\t名詞,一般,*,*,*,*,食,ショク,ショク\nべる\t名詞,一般,*,*,*,*,べる,ベル,ベル\nEOS\n";
        let tokens = parse_mecab_lookup_tokens("食\0べる", mecab_out);
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0].surface, "食");
        assert_eq!(tokens[0].surface_clean, "食");
        assert_eq!(tokens[1].surface, "\0べる");
        assert_eq!(tokens[1].surface_clean, "べる");
    }

    #[test]
    fn windows_short_path_detection_and_tagger_arg_formatting() {
        assert!(!path_needs_windows_short_name(r"C:\Subomi\ipadic"));
        assert!(!path_needs_windows_short_name(r"C:\PROGRA~1\ipadic"));
        assert!(path_needs_windows_short_name(r"C:\Program Files\Subomi\ipadic"));
        assert!(path_needs_windows_short_name(r"C:\Users\太郎\Subomi\ipadic"));
        assert!(path_needs_windows_short_name("C:/Users/José/dic"));

        // Windows 8.3 short paths (including raw ANSI code-page bytes) pass through verbatim.
        assert_eq!(
            format_dicdir_tagger_arg(true, b"C:\\PROGRA~1\\ipadic", b"C:\\PROGRA~1\\ipadic\\dicrc"),
            b" -d C:\\PROGRA~1\\ipadic -r C:\\PROGRA~1\\ipadic\\dicrc"
        );
        assert_eq!(
            format_dicdir_tagger_arg(true, b"C:\\\x8e\x96\\ipadic", b"C:\\\x8e\x96\\ipadic\\dicrc"),
            b" -d C:\\\x8e\x96\\ipadic -r C:\\\x8e\x96\\ipadic\\dicrc"
        );
        // Windows path with remaining spaces (e.g. 8.3 disabled on NTFS volume) is quoted safely.
        assert_eq!(
            format_dicdir_tagger_arg(
                true,
                b"D:\\My Folder\\ipadic",
                b"D:\\My Folder\\ipadic\\dicrc"
            ),
            b" -d \"D:\\My Folder\\ipadic\" -r \"D:\\My Folder\\ipadic\\dicrc\""
        );
        // Windows dicdir without dicrc emits only `-d`.
        assert_eq!(
            format_dicdir_tagger_arg(true, b"C:\\PROGRA~1\\ipadic", b""),
            b" -d C:\\PROGRA~1\\ipadic"
        );
        // Unix / macOS Homebrew formatting and quoting.
        assert_eq!(
            format_dicdir_tagger_arg(
                false,
                b"/opt/homebrew/lib/mecab/dic/ipadic",
                b"/opt/homebrew/lib/mecab/dic/ipadic/dicrc"
            ),
            b"-d /opt/homebrew/lib/mecab/dic/ipadic -r /opt/homebrew/lib/mecab/dic/ipadic/dicrc"
        );
        assert_eq!(
            format_dicdir_tagger_arg(
                false,
                b"/Applications/My Subomi.app/Contents/Resources/ipadic",
                b"/Applications/My Subomi.app/Contents/Resources/ipadic/dicrc"
            ),
            b"-d \"/Applications/My Subomi.app/Contents/Resources/ipadic\" -r \"/Applications/My Subomi.app/Contents/Resources/ipadic/dicrc\""
        );
        // Empty dicdir falls back to empty bytes (system default `mecabrc`).
        assert_eq!(format_dicdir_tagger_arg(true, b"", b""), b"");
        assert_eq!(format_dicdir_tagger_arg(false, b"", b""), b"");
    }

    #[test]
    fn mecab_dicdir_discovery_probes_env_exe_adjacent_homebrew_and_linux_in_order() {
        // Candidate ordering checks across platforms.
        let mac_overrides = mecab_dicdir_override_candidates(
            Some(Path::new("/custom/mecab/dic")),
            Some(Path::new("/Applications/Subomi.app/Contents/MacOS")),
            "macos",
        );
        assert_eq!(mac_overrides[0], PathBuf::from("/custom/mecab/dic"));
        assert!(mac_overrides.contains(&PathBuf::from(
            "/Applications/Subomi.app/Contents/MacOS/ipadic"
        )));
        assert!(mac_overrides.contains(&PathBuf::from(
            "/Applications/Subomi.app/Contents/MacOS/unidic"
        )));
        assert!(mac_overrides.contains(&PathBuf::from(
            "/Applications/Subomi.app/Contents/MacOS/dic"
        )));
        assert!(mac_overrides.contains(&PathBuf::from(
            "/Applications/Subomi.app/Contents/MacOS/share/mecab/dic/ipadic"
        )));
        assert!(mac_overrides.contains(&PathBuf::from(
            "/Applications/Subomi.app/Contents/Resources/ipadic"
        )));
        assert!(mac_overrides.contains(&PathBuf::from(
            "/Applications/Subomi.app/Contents/Resources/unidic"
        )));

        let mac_system = mecab_dicdir_system_candidates("macos");
        assert_eq!(
            mac_system,
            vec![
                PathBuf::from("/opt/homebrew/lib/mecab/dic/ipadic"),
                PathBuf::from("/opt/homebrew/lib/mecab/dic/unidic"),
                PathBuf::from("/usr/local/lib/mecab/dic/ipadic"),
                PathBuf::from("/usr/local/lib/mecab/dic/unidic"),
            ]
        );

        let linux_system = mecab_dicdir_system_candidates("linux");
        assert!(linux_system.contains(&PathBuf::from("/var/lib/mecab/dic/debian")));
        assert!(linux_system.contains(&PathBuf::from("/usr/lib/x86_64-linux-gnu/mecab/dic/ipadic")));
        assert!(linux_system.contains(&PathBuf::from("/usr/share/mecab/dic/ipadic")));

        // Filesystem-backed probe with a temporary directory tree.
        let unique = format!(
            "subomi_mecab_probe_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let root = std::env::temp_dir().join(unique);
        let exe_dir = root.join("bin");
        let empty_dic = exe_dir.join("ipadic");
        let valid_share_dic = exe_dir.join("share").join("mecab").join("dic").join("ipadic");
        let env_dic = root.join("env_unidic");
        std::fs::create_dir_all(&empty_dic).unwrap();
        std::fs::create_dir_all(&valid_share_dic).unwrap();
        std::fs::create_dir_all(&env_dic).unwrap();
        std::fs::write(valid_share_dic.join("dicrc"), b"; dicrc").unwrap();
        std::fs::write(env_dic.join("sys.dic"), b"dummy").unwrap();

        // Empty `ipadic/` without `dicrc` or `sys.dic` is skipped in favor of valid `share/mecab/dic/ipadic`.
        assert!(!is_valid_mecab_dicdir(&empty_dic));
        assert!(is_valid_mecab_dicdir(&valid_share_dic));
        assert_eq!(
            resolve_mecab_dicrc(&valid_share_dic),
            Some(valid_share_dic.join("dicrc"))
        );

        let without_env = mecab_dicdir_override_candidates(None, Some(&exe_dir), "linux");
        assert_eq!(
            discover_mecab_dicdir_in(&without_env, &[]),
            Some(valid_share_dic.clone())
        );

        // `MECAB_DICDIR` takes highest precedence over executable-adjacent paths.
        let with_env = mecab_dicdir_override_candidates(Some(&env_dic), Some(&exe_dir), "linux");
        assert_eq!(
            discover_mecab_dicdir_in(&with_env, &[]),
            Some(env_dic.clone())
        );

        // Clean fallback to `None` (system default `mecabrc`) when no candidates exist.
        assert_eq!(
            discover_mecab_dicdir_in(&[root.join("missing")], &[root.join("also_missing")]),
            None
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
