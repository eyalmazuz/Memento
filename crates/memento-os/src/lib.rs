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

//! Cross-platform OS integration for Memento:
//! - Session-bus screensaver inhibition (`org.freedesktop.ScreenSaver` on Linux/Unix,
//!   `IOPMAssertionCreateWithName` on macOS, `SetThreadExecutionState` on Windows).
//! - Non-blocking XDG Desktop Portal `org.freedesktop.portal.FileChooser.OpenFile`
//!   file dialogs with multi-file playlist & subtitle filter support and background
//!   `zenity`/`kdialog` fallback.
//! - `file://` URI and `text/uri-list` drag-and-drop path decoding.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    OnceLock,
};

pub mod windows_fullscreen;

pub use windows_fullscreen::{
    capture_win32_window_snapshot, compute_windows_borderless_fullscreen_rect,
    enter_windows_borderless_fullscreen, exit_windows_borderless_fullscreen, query_window_monitor,
    strip_win32_window_frame_styles, MonitorRect, Win32WindowStyleSnapshot, WindowRect,
    WIN32_BORDERLESS_STRIP_EX_STYLE_MASK, WIN32_BORDERLESS_STRIP_STYLE_MASK,
    WINDOWS_OPENGL_FULLSCREEN_EXTRA_HEIGHT_PX,
};

#[cfg(all(unix, not(target_os = "macos")))]
use zbus::blocking::{fdo::DBusProxy, Connection, Proxy};
#[cfg(all(unix, not(target_os = "macos")))]
use zbus::names::{BusName, InterfaceName, WellKnownName};
#[cfg(all(unix, not(target_os = "macos")))]
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};
#[cfg(all(unix, not(target_os = "macos")))]
use zbus_xml::Node;

#[cfg(all(unix, not(target_os = "macos")))]
const SERVICE: &str = "org.freedesktop.ScreenSaver";
#[cfg(all(unix, not(target_os = "macos")))]
const PATH: &str = "/org/freedesktop/ScreenSaver";
#[cfg(all(unix, not(target_os = "macos")))]
const INTERFACE: &str = "org.freedesktop.ScreenSaver";
pub const APPLICATION_ID: &str = "io.github.ripose-jp.memento";
pub const WAYLAND_APP_ID: &str = "memento";

#[cfg(all(unix, not(target_os = "macos")))]
const PORTAL_SERVICE: &str = "org.freedesktop.portal.Desktop";
#[cfg(all(unix, not(target_os = "macos")))]
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
#[cfg(all(unix, not(target_os = "macos")))]
const PORTAL_FILE_CHOOSER_IFACE: &str = "org.freedesktop.portal.FileChooser";
#[cfg(all(unix, not(target_os = "macos")))]
const PORTAL_REQUEST_IFACE: &str = "org.freedesktop.portal.Request";

// Keep a single session-bus sender for the whole application, as Qt does.
// Many ScreenSaver services associate a cookie with its D-Bus sender and
// automatically release it if that sender disconnects between the two calls.
#[cfg(all(unix, not(target_os = "macos")))]
static SESSION: OnceLock<Connection> = OnceLock::new();

#[cfg(all(unix, not(target_os = "macos")))]
fn session_connection() -> Option<&'static Connection> {
    if let Some(connection) = SESSION.get() {
        return Some(connection);
    }
    // Failure is not cached: a session bus can become available later.
    let connection = Connection::session().ok()?;
    Some(SESSION.get_or_init(|| connection))
}

// QDBusInterface introspects before looking up the service owner and caches
// metadata after it sees this interface. The first lookup may activate a
// D-Bus-activatable service; later lookups only need the owner check.
#[cfg(all(unix, not(target_os = "macos")))]
static METADATA_CACHED: AtomicBool = AtomicBool::new(false);

static NEXT_PORTAL_TOKEN: AtomicU64 = AtomicU64::new(1);

#[derive(Default, Debug)]
pub struct Screensaver {
    inhibited: bool,
    cookie: u32,
    last_active_request: Option<bool>,
    inhibit_attempts: u64,
}

// Qt's QDBusInterface::isValid() checks whether the service currently owns
// its bus name, NOT whether the requested interface appears in introspection.
// When another interface is exported at the path, Qt still attempts the call,
// and UnInhibit clears the cookie even if the call fails.
#[cfg(all(unix, not(target_os = "macos")))]
fn screen_saver_proxy(connection: &Connection) -> Option<Proxy<'_>> {
    let proxy = Proxy::new(connection, SERVICE, PATH, INTERFACE).ok()?;
    if !METADATA_CACHED.load(Ordering::Relaxed) {
        if let Ok(xml) = proxy.introspect() {
            if let Ok(node) = Node::try_from(xml.as_str()) {
                if node
                    .interfaces()
                    .iter()
                    .any(|interface| interface.name().as_str() == INTERFACE)
                {
                    METADATA_CACHED.store(true, Ordering::Relaxed);
                }
            }
        }
    }
    let bus = DBusProxy::new(connection).ok()?;
    if !bus.name_has_owner(BusName::try_from(SERVICE).ok()?).ok()? {
        return None;
    }
    Some(proxy)
}

#[cfg(target_os = "windows")]
mod win_ffi {
    pub type ExecutionState = u32;
    pub const ES_CONTINUOUS: ExecutionState = 0x8000_0000;
    pub const ES_SYSTEM_REQUIRED: ExecutionState = 0x0000_0001;
    pub const ES_DISPLAY_REQUIRED: ExecutionState = 0x0000_0002;

    extern "system" {
        pub fn SetThreadExecutionState(es_flags: ExecutionState) -> ExecutionState;
    }
}

#[cfg(target_os = "macos")]
mod macos_ffi {
    use std::ffi::{c_char, c_void};

    pub type CFStringRef = *const c_void;
    pub type CFAllocatorRef = *const c_void;
    pub type CFStringEncoding = u32;
    pub type IOPMAssertionID = u32;
    pub type IOPMAssertionLevel = u32;
    pub type IOReturn = i32;

    pub const K_CF_STRING_ENCODING_UTF8: CFStringEncoding = 0x0800_0100;
    pub const K_IOPM_ASSERTION_LEVEL_ON: IOPMAssertionLevel = 255;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        pub fn CFStringCreateWithCString(
            alloc: CFAllocatorRef,
            c_str: *const c_char,
            encoding: CFStringEncoding,
        ) -> CFStringRef;
        pub fn CFRelease(cf: *const c_void);
    }

    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        pub fn IOPMAssertionCreateWithName(
            assertion_type: CFStringRef,
            assertion_level: IOPMAssertionLevel,
            assertion_name: CFStringRef,
            assertion_id: *mut IOPMAssertionID,
        ) -> IOReturn;
        pub fn IOPMAssertionRelease(assertion_id: IOPMAssertionID) -> IOReturn;
    }
}

impl Screensaver {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_inhibited(&self) -> bool {
        self.inhibited
    }

    pub fn cookie(&self) -> u32 {
        self.cookie
    }

    pub fn inhibit_attempts(&self) -> u64 {
        self.inhibit_attempts
    }

    fn apply_inhibit_reply(&mut self, reply: Option<u32>) {
        if let Some(cookie) = reply {
            if cookie != 0 {
                self.cookie = cookie;
                self.inhibited = true;
            }
        }
    }

    pub fn inhibit(&mut self, reason: &str) {
        self.last_active_request = Some(true);
        if self.inhibited {
            return;
        }
        self.inhibit_attempts = self.inhibit_attempts.saturating_add(1);
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let reply = session_connection()
                .and_then(screen_saver_proxy)
                .and_then(|proxy| {
                    proxy
                        .call::<_, _, u32>("Inhibit", &(APPLICATION_ID, reason))
                        .ok()
                });
            self.apply_inhibit_reply(reply);
        }
        #[cfg(target_os = "windows")]
        {
            let _ = reason;
            unsafe {
                win_ffi::SetThreadExecutionState(
                    win_ffi::ES_CONTINUOUS
                        | win_ffi::ES_DISPLAY_REQUIRED
                        | win_ffi::ES_SYSTEM_REQUIRED,
                );
            }
            self.cookie = 1;
            self.inhibited = true;
        }
        #[cfg(target_os = "macos")]
        {
            use std::ffi::CString;
            let Ok(reason_c) = CString::new(reason) else {
                return;
            };
            let Ok(type_c) = CString::new("NoDisplaySleepAssertion") else {
                return;
            };
            unsafe {
                let type_cf = macos_ffi::CFStringCreateWithCString(
                    std::ptr::null(),
                    type_c.as_ptr(),
                    macos_ffi::K_CF_STRING_ENCODING_UTF8,
                );
                let reason_cf = macos_ffi::CFStringCreateWithCString(
                    std::ptr::null(),
                    reason_c.as_ptr(),
                    macos_ffi::K_CF_STRING_ENCODING_UTF8,
                );
                let mut assertion_id: macos_ffi::IOPMAssertionID = 0;
                if !type_cf.is_null() && !reason_cf.is_null() {
                    let status = macos_ffi::IOPMAssertionCreateWithName(
                        type_cf,
                        macos_ffi::K_IOPM_ASSERTION_LEVEL_ON,
                        reason_cf,
                        &mut assertion_id,
                    );
                    if status == 0 && assertion_id != 0 {
                        self.apply_inhibit_reply(Some(assertion_id));
                    }
                }
                if !reason_cf.is_null() {
                    macos_ffi::CFRelease(reason_cf);
                }
                if !type_cf.is_null() {
                    macos_ffi::CFRelease(type_cf);
                }
            }
        }
    }

    pub fn uninhibit(&mut self) {
        self.last_active_request = Some(false);
        if !self.inhibited {
            return;
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            if self.cookie != 0 {
                if let Some(connection) = session_connection() {
                    if let Some(proxy) = screen_saver_proxy(connection) {
                        let _: zbus::Result<()> = proxy.call("UnInhibit", &(self.cookie,));
                    }
                }
                self.cookie = 0;
            }
        }
        #[cfg(target_os = "windows")]
        {
            unsafe {
                win_ffi::SetThreadExecutionState(win_ffi::ES_CONTINUOUS);
            }
            self.cookie = 0;
        }
        #[cfg(target_os = "macos")]
        {
            if self.cookie != 0 {
                unsafe {
                    let _ = macos_ffi::IOPMAssertionRelease(self.cookie);
                }
                self.cookie = 0;
            }
        }
        self.inhibited = false;
    }

    /// Synchronize screensaver inhibition with active playback (`!paused && file_loaded`).
    /// Transition-gated so repeated 60Hz ticks in the same playback state never
    /// issue redundant D-Bus calls when no ScreenSaver service is running.
    pub fn sync_playback(&mut self, file_loaded: bool, paused: bool, reason: &str) {
        let active = file_loaded && !paused;
        if self.last_active_request == Some(active) {
            return;
        }
        self.last_active_request = Some(active);
        if active {
            self.inhibit(reason);
        } else {
            self.uninhibit();
        }
    }
}

impl Drop for Screensaver {
    fn drop(&mut self) {
        self.uninhibit();
    }
}

// ── File Dialog & Drag-and-Drop URI Utilities ───────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileDialogKind {
    MediaAndSubtitles,
    SubtitlesOnly,
    ArchiveZip,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDialogFilter {
    pub label: String,
    pub patterns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDialogOptions {
    pub title: String,
    pub kind: FileDialogKind,
    pub multiple: bool,
    pub initial_dir: Option<PathBuf>,
}

impl FileDialogOptions {
    pub fn media_and_subtitles(initial_dir: Option<PathBuf>) -> Self {
        Self {
            title: "Select Media or Subtitle Files".to_owned(),
            kind: FileDialogKind::MediaAndSubtitles,
            multiple: true,
            initial_dir,
        }
    }

    pub fn subtitles_only(initial_dir: Option<PathBuf>) -> Self {
        Self {
            title: "Select Subtitle File".to_owned(),
            kind: FileDialogKind::SubtitlesOnly,
            multiple: false,
            initial_dir,
        }
    }

    pub fn archive_zip(initial_dir: Option<PathBuf>) -> Self {
        Self {
            title: "Select Yomitan Dictionary Archive".to_owned(),
            kind: FileDialogKind::ArchiveZip,
            multiple: false,
            initial_dir,
        }
    }

    pub fn filters(&self) -> Vec<FileDialogFilter> {
        match self.kind {
            FileDialogKind::MediaAndSubtitles => vec![
                FileDialogFilter {
                    label: "Media & Subtitle Files".to_owned(),
                    patterns: [
                        "*.mkv", "*.mp4", "*.webm", "*.avi", "*.mov", "*.m4v", "*.mpg", "*.mpeg",
                        "*.ts", "*.m2ts", "*.vob", "*.ogv", "*.ogg", "*.mp3", "*.flac", "*.wav",
                        "*.m4a", "*.aac", "*.opus", "*.srt", "*.ass", "*.ssa", "*.vtt", "*.sub",
                        "*.idx", "*.sup", "*.lrc", "*.mks", "*.pgs",
                    ]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                },
                FileDialogFilter {
                    label: "Video Files".to_owned(),
                    patterns: [
                        "*.webm", "*.mkv", "*.vob", "*.ogv", "*.ogg", "*.avi", "*.mov", "*.qt",
                        "*.mp4", "*.m4p", "*.m4v", "*.mpg", "*.mp2", "*.mpeg", "*.mpe", "*.mpv",
                        "*.ts", "*.m2ts",
                    ]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                },
                FileDialogFilter {
                    label: "Subtitle Files".to_owned(),
                    patterns: [
                        "*.ass", "*.ssa", "*.srt", "*.vtt", "*.sub", "*.idx", "*.sup", "*.lrc",
                        "*.mks", "*.pgs", "*.rt", "*.scc", "*.smi", "*.utf-8", "*.utf", "*.utf8",
                    ]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                },
                FileDialogFilter {
                    label: "All Files".to_owned(),
                    patterns: vec!["*".to_owned()],
                },
            ],
            FileDialogKind::SubtitlesOnly => vec![
                FileDialogFilter {
                    label: "Subtitle Files".to_owned(),
                    patterns: [
                        "*.ass", "*.ssa", "*.srt", "*.vtt", "*.sub", "*.idx", "*.sup", "*.lrc",
                        "*.mks", "*.pgs", "*.rt", "*.scc", "*.smi", "*.utf-8", "*.utf", "*.utf8",
                    ]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                },
                FileDialogFilter {
                    label: "All Files".to_owned(),
                    patterns: vec!["*".to_owned()],
                },
            ],
            FileDialogKind::ArchiveZip => vec![
                FileDialogFilter {
                    label: "ZIP Archives".to_owned(),
                    patterns: vec!["*.zip".to_owned()],
                },
                FileDialogFilter {
                    label: "All Files".to_owned(),
                    patterns: vec!["*".to_owned()],
                },
            ],
        }
    }
}

/// Sanitize a D-Bus unique connection name (e.g. `":1.42"`) into the portal
/// request path segment (`"1_42"`).
pub fn sanitize_portal_sender(unique_name: &str) -> String {
    unique_name
        .trim_start_matches(':')
        .chars()
        .map(|ch| if ch == '.' || ch == ':' || ch == '/' { '_' } else { ch })
        .collect()
}

/// Build the expected XDG Desktop Portal request object path for `unique_name`
/// and `handle_token`.
pub fn portal_request_path(unique_name: &str, handle_token: &str) -> String {
    format!(
        "/org/freedesktop/portal/desktop/request/{}/{}",
        sanitize_portal_sender(unique_name),
        handle_token
    )
}

fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Percent-decode a URI component into raw UTF-8 string.
pub fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Decode a `file://` URI or plain path into a [`PathBuf`].
pub fn decode_file_uri(uri_or_path: &str) -> Option<PathBuf> {
    let trimmed = uri_or_path.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix("file://localhost") {
        let decoded = percent_decode(rest);
        return (!decoded.is_empty()).then(|| PathBuf::from(decoded));
    }
    if let Some(rest) = trimmed.strip_prefix("file://") {
        let decoded = percent_decode(rest);
        #[cfg(target_os = "windows")]
        let decoded = decoded.strip_prefix('/').unwrap_or(&decoded).to_owned();
        return (!decoded.is_empty()).then(|| PathBuf::from(decoded));
    }
    Some(PathBuf::from(trimmed))
}

/// Parse a `text/uri-list` payload or newline-separated path list into [`PathBuf`]s.
pub fn parse_uri_list(payload: &str) -> Vec<PathBuf> {
    payload
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(decode_file_uri)
        .collect()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn extract_portal_uris(results: &HashMap<String, OwnedValue>) -> Vec<PathBuf> {
    let Some(uris_val) = results.get("uris") else {
        return Vec::new();
    };
    if let Value::Array(arr) = &**uris_val {
        return arr
            .iter()
            .filter_map(|item| match item {
                Value::Str(s) => decode_file_uri(s.as_str()),
                _ => None,
            })
            .collect();
    }
    Vec::new()
}

/// Attempt to open a file dialog via `org.freedesktop.portal.FileChooser.OpenFile`.
/// Returns `Ok(paths)` if the portal handled the dialog (empty if cancelled),
/// or `Err(...)` if the portal service is unavailable so the caller can fall back.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn open_files_via_portal(options: &FileDialogOptions) -> Result<Vec<PathBuf>, String> {
    let connection = Connection::session().map_err(|e| format!("D-Bus session unavailable: {e}"))?;
    let bus = DBusProxy::new(&connection).map_err(|e| format!("DBusProxy failed: {e}"))?;
    let portal_bus_name =
        BusName::try_from(PORTAL_SERVICE).map_err(|e| format!("Invalid bus name: {e}"))?;
    let has_owner = bus.name_has_owner(portal_bus_name.clone()).unwrap_or(false);
    if !has_owner {
        if let Ok(wk) = WellKnownName::try_from(PORTAL_SERVICE) {
            let _ = bus.start_service_by_name(wk, 0);
        }
        if !bus.name_has_owner(portal_bus_name).unwrap_or(false) {
            return Err("org.freedesktop.portal.Desktop has no owner".to_owned());
        }
    }

    let unique_name = connection
        .unique_name()
        .ok_or_else(|| "D-Bus connection has no unique name".to_owned())?
        .as_str()
        .to_owned();
    let token_id = NEXT_PORTAL_TOKEN.fetch_add(1, Ordering::Relaxed);
    let handle_token = format!("memento_fc_{}_{}", std::process::id(), token_id);
    let expected_path_str = portal_request_path(&unique_name, &handle_token);
    let expected_path = ObjectPath::try_from(expected_path_str.as_str())
        .map_err(|e| format!("Invalid portal request path: {e}"))?;

    // Subscribe to `org.freedesktop.portal.Request.Response` BEFORE calling `OpenFile`
    // so an immediate response signal is never missed.
    let req_proxy = Proxy::new(
        &connection,
        PORTAL_SERVICE,
        expected_path,
        PORTAL_REQUEST_IFACE,
    )
    .map_err(|e| format!("Could not create portal request proxy: {e}"))?;
    let mut signals = req_proxy
        .receive_signal("Response")
        .map_err(|e| format!("Could not subscribe to portal Response signal: {e}"))?;

    let chooser = Proxy::new(
        &connection,
        PORTAL_SERVICE,
        PORTAL_PATH,
        PORTAL_FILE_CHOOSER_IFACE,
    )
    .map_err(|e| format!("Could not create portal FileChooser proxy: {e}"))?;

    let mut portal_opts: HashMap<&str, Value<'_>> = HashMap::new();
    portal_opts.insert("handle_token", Value::from(handle_token.as_str()));
    portal_opts.insert("multiple", Value::from(options.multiple));
    portal_opts.insert("modal", Value::from(true));

    let filters: Vec<(String, Vec<(u32, String)>)> = options
        .filters()
        .into_iter()
        .map(|f| {
            let rules = f.patterns.into_iter().map(|pat| (0u32, pat)).collect();
            (f.label, rules)
        })
        .collect();
    if let Some(first_filter) = filters.first().cloned() {
        portal_opts.insert("current_filter", Value::from(first_filter));
    }
    portal_opts.insert("filters", Value::from(filters));

    if let Some(dir) = options.initial_dir.as_deref().and_then(Path::to_str) {
        let mut dir_bytes = dir.as_bytes().to_vec();
        dir_bytes.push(0);
        portal_opts.insert("current_folder", Value::from(dir_bytes));
    }

    let returned_handle: OwnedObjectPath = chooser
        .call("OpenFile", &("", options.title.as_str(), portal_opts))
        .map_err(|e| format!("FileChooser.OpenFile failed: {e}"))?;

    if returned_handle.as_str() != expected_path_str {
        let req_iface = InterfaceName::try_from(PORTAL_REQUEST_IFACE)
            .map_err(|e| format!("Invalid interface: {e}"))?;
        let alt_proxy = Proxy::new(&connection, PORTAL_SERVICE, returned_handle, req_iface)
            .map_err(|e| format!("Alt request proxy failed: {e}"))?;
        signals = alt_proxy
            .receive_signal("Response")
            .map_err(|e| format!("Alt Response subscription failed: {e}"))?;
    }

    let Some(message) = signals.next() else {
        return Err("Portal signal stream ended without Response".to_owned());
    };
    let (response_code, results): (u32, HashMap<String, OwnedValue>) = message
        .body()
        .deserialize()
        .map_err(|e| format!("Could not decode portal Response body: {e}"))?;
    if response_code == 0 {
        Ok(extract_portal_uris(&results))
    } else {
        // User cancelled or closed the portal dialog (`1` or `2`).
        Ok(Vec::new())
    }
}

#[cfg(not(all(unix, not(target_os = "macos"))))]
pub fn open_files_via_portal(_options: &FileDialogOptions) -> Result<Vec<PathBuf>, String> {
    Err("XDG Desktop Portal is only supported on Linux/Unix".to_owned())
}

fn open_files_cli_fallback(options: &FileDialogOptions) -> Vec<PathBuf> {
    let mut zenity_args = vec![
        "--file-selection".to_owned(),
        format!("--title={}", options.title),
    ];
    if options.multiple {
        zenity_args.push("--multiple".to_owned());
        zenity_args.push("--separator=\n".to_owned());
    }
    if let Some(dir) = options.initial_dir.as_deref() {
        let mut path_str = dir.to_string_lossy().into_owned();
        if !path_str.ends_with('/') {
            path_str.push('/');
        }
        zenity_args.push(format!("--filename={path_str}"));
    }
    for filter in options.filters() {
        zenity_args.push(format!(
            "--file-filter={} | {}",
            filter.label,
            filter.patterns.join(" ")
        ));
    }

    if let Ok(output) = Command::new("zenity").args(&zenity_args).output() {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            return parse_uri_list(&stdout);
        }
        return Vec::new();
    }

    let mut kdialog_args = vec![
        "--getopenfilename".to_owned(),
        options
            .initial_dir
            .as_deref()
            .and_then(Path::to_str)
            .unwrap_or(".")
            .to_owned(),
    ];
    if options.multiple {
        kdialog_args.push("--multiple".to_owned());
        kdialog_args.push("--separate-output".to_owned());
    }
    if let Ok(output) = Command::new("kdialog").args(&kdialog_args).output() {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            return parse_uri_list(&stdout);
        }
    }
    Vec::new()
}

/// Blocking file chooser (intended to be called from [`open_files_async`] worker thread).
/// Uses XDG Desktop Portal `org.freedesktop.portal.FileChooser.OpenFile` first,
/// falling back to `zenity`/`kdialog` only if the portal is unavailable.
pub fn open_files_blocking(options: &FileDialogOptions) -> Vec<PathBuf> {
    match open_files_via_portal(options) {
        Ok(paths) => paths,
        Err(_) => open_files_cli_fallback(options),
    }
}

/// Spawn a non-blocking background file chooser dialog and invoke `callback`
/// with the selected file paths (if any).
pub fn open_files_async(
    options: FileDialogOptions,
    callback: impl FnOnce(Vec<PathBuf>) + Send + 'static,
) {
    let _ = std::thread::Builder::new()
        .name("memento-file-dialog".to_owned())
        .spawn(move || {
            let paths = open_files_blocking(&options);
            if !paths.is_empty() {
                callback(paths);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::{
        decode_file_uri, parse_uri_list, percent_decode, portal_request_path,
        sanitize_portal_sender, FileDialogKind, FileDialogOptions, Screensaver,
    };
    use std::path::PathBuf;

    #[test]
    fn inhibit_stays_false_on_failed_or_zero_cookie() {
        let mut s = Screensaver::default();
        s.apply_inhibit_reply(None);
        assert!(!s.inhibited);
        assert!(!s.is_inhibited());
        assert_eq!(s.cookie, 0);

        s.apply_inhibit_reply(Some(0));
        assert!(!s.inhibited);
        assert_eq!(s.cookie, 0);

        s.apply_inhibit_reply(Some(17));
        assert!(s.inhibited);
        assert!(s.is_inhibited());
        assert_eq!(s.cookie, 17);

        s.inhibited = false;
        s.cookie = 0;
    }

    #[test]
    fn portal_sender_and_request_path_follow_freedesktop_spec() {
        assert_eq!(sanitize_portal_sender(":1.42"), "1_42");
        assert_eq!(sanitize_portal_sender(":1.109.3"), "1_109_3");
        assert_eq!(
            portal_request_path(":1.42", "memento_fc_1"),
            "/org/freedesktop/portal/desktop/request/1_42/memento_fc_1"
        );
    }

    #[test]
    fn file_uri_and_uri_list_decoding_handles_spaces_cjk_and_comments() {
        assert_eq!(
            percent_decode("/home/user/%E6%97%A5%E6%9C%AC%E8%AA%9E%2001.mkv"),
            "/home/user/日本語 01.mkv"
        );
        assert_eq!(
            decode_file_uri("file:///home/user/Anime%20Episode%2001.mkv"),
            Some(PathBuf::from("/home/user/Anime Episode 01.mkv"))
        );
        assert_eq!(
            decode_file_uri("file://localhost/tmp/sub.srt"),
            Some(PathBuf::from("/tmp/sub.srt"))
        );
        assert_eq!(
            decode_file_uri("/var/media/movie.mp4"),
            Some(PathBuf::from("/var/media/movie.mp4"))
        );
        assert_eq!(decode_file_uri("   "), None);

        let list = "# comment\r\nfile:///tmp/ep%201.mkv\r\nfile:///tmp/ep%201.ja.srt\r\n\r\n";
        assert_eq!(
            parse_uri_list(list),
            vec![
                PathBuf::from("/tmp/ep 1.mkv"),
                PathBuf::from("/tmp/ep 1.ja.srt"),
            ]
        );
    }

    #[test]
    fn file_dialog_options_build_media_and_subtitle_filters() {
        let media = FileDialogOptions::media_and_subtitles(Some(PathBuf::from("/tmp")));
        assert!(media.multiple);
        assert_eq!(media.kind, FileDialogKind::MediaAndSubtitles);
        let filters = media.filters();
        assert!(filters.len() >= 3);
        assert!(filters[0].patterns.iter().any(|p| p == "*.mkv"));
        assert!(filters[0].patterns.iter().any(|p| p == "*.srt"));

        let subs = FileDialogOptions::subtitles_only(None);
        assert!(!subs.multiple);
        assert_eq!(subs.kind, FileDialogKind::SubtitlesOnly);
        let sub_filters = subs.filters();
        assert!(sub_filters[0].patterns.iter().any(|p| p == "*.ass"));
        assert!(sub_filters[0].patterns.iter().any(|p| p == "*.vtt"));
        assert!(sub_filters[0].patterns.iter().any(|p| p == "*.sub"));
    }

    #[test]
    fn sync_playback_is_transition_gated_across_repeated_active_ticks() {
        let mut ss = Screensaver::new();
        assert_eq!(ss.inhibit_attempts(), 0);
        // Repeated active playback ticks must only attempt inhibition once per transition.
        for _ in 0..10 {
            ss.sync_playback(true, false, "Playing media");
        }
        assert_eq!(ss.inhibit_attempts(), 1);
        // Transition to paused resets the gate; next active transition attempts once more.
        ss.sync_playback(true, true, "Playing media");
        assert_eq!(ss.inhibit_attempts(), 1);
        for _ in 0..10 {
            ss.sync_playback(true, false, "Playing media");
        }
        assert_eq!(ss.inhibit_attempts(), 2);
    }

    #[test]
    fn windows_borderless_fullscreen_rect_adds_one_pixel_preserving_monitor_offset() {
        use super::{
            compute_windows_borderless_fullscreen_rect, strip_win32_window_frame_styles,
            MonitorRect, WindowRect, WIN32_BORDERLESS_STRIP_EX_STYLE_MASK,
            WIN32_BORDERLESS_STRIP_STYLE_MASK, WINDOWS_OPENGL_FULLSCREEN_EXTRA_HEIGHT_PX,
        };

        assert_eq!(WINDOWS_OPENGL_FULLSCREEN_EXTRA_HEIGHT_PX, 1);

        let primary = MonitorRect::new(0, 0, 1920, 1080, 1.0);
        assert_eq!(
            compute_windows_borderless_fullscreen_rect(primary),
            WindowRect::new(0, 0, 1920, 1081)
        );

        let left_monitor = MonitorRect::new(-2560, -120, 2560, 1440, 1.5);
        assert_eq!(
            compute_windows_borderless_fullscreen_rect(left_monitor),
            WindowRect::new(-2560, -120, 2560, 1441)
        );

        let full_style = 0x10CF_0000u32; // WS_VISIBLE | WS_OVERLAPPEDWINDOW
        let full_ex = 0x0004_0301u32;
        let (stripped_style, stripped_ex) = strip_win32_window_frame_styles(full_style, full_ex);
        assert_eq!(stripped_style & WIN32_BORDERLESS_STRIP_STYLE_MASK, 0);
        assert_ne!(stripped_style & 0x1000_0000, 0); // WS_VISIBLE preserved
        assert_eq!(stripped_ex & WIN32_BORDERLESS_STRIP_EX_STYLE_MASK, 0);
        assert_ne!(stripped_ex & 0x0004_0000, 0); // WS_EX_APPWINDOW preserved
    }
}
