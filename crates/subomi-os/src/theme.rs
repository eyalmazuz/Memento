//! Cross-platform system color scheme detection and live change notification.
//!
//! - **Linux/Unix**: Queries `org.freedesktop.portal.Settings.Read` for
//!   `("org.freedesktop.appearance", "color-scheme")` (`1` = Dark, `2` = Light,
//!   `0` = Unknown), fixing GNOME Flatpak `#331`, with live `SettingChanged`
//!   signal watching and `GTK_THEME` / `gsettings` fallbacks.
//! - **Windows**: Queries `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize`
//!   `AppsUseLightTheme` (`0` = Dark, `1` = Light) via Win32 `RegGetValueW`
//!   (`advapi32.dll`) and watches changes via `RegNotifyChangeKeyValue`.
//! - **macOS**: Queries `AppleInterfaceStyle` via CoreFoundation
//!   `CFPreferencesCopyAppValue` (falling back to `defaults read -g AppleInterfaceStyle`).

#[cfg(any(
    all(unix, not(target_os = "macos")),
    target_os = "macos"
))]
use std::process::Command;

#[cfg(all(unix, not(target_os = "macos")))]
use zbus::blocking::{Connection, Proxy};
#[cfg(all(unix, not(target_os = "macos")))]
use zbus::zvariant::{OwnedValue, Value};

#[cfg(all(unix, not(target_os = "macos")))]
const PORTAL_SERVICE: &str = "org.freedesktop.portal.Desktop";
#[cfg(all(unix, not(target_os = "macos")))]
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
#[cfg(all(unix, not(target_os = "macos")))]
const PORTAL_SETTINGS_IFACE: &str = "org.freedesktop.portal.Settings";
#[cfg(all(unix, not(target_os = "macos")))]
const PORTAL_APPEARANCE_NS: &str = "org.freedesktop.appearance";
#[cfg(all(unix, not(target_os = "macos")))]
const PORTAL_COLOR_SCHEME_KEY: &str = "color-scheme";

/// System-wide preferred color scheme reported by the OS desktop environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SystemColorScheme {
    Dark,
    Light,
    Unknown,
}

/// Parse an XDG Desktop Portal `org.freedesktop.appearance` / `color-scheme`
/// uint32 value (`1` = Dark, `2` = Light, `0` or other = Unknown).
pub const fn parse_portal_color_scheme(value: u32) -> SystemColorScheme {
    match value {
        1 => SystemColorScheme::Dark,
        2 => SystemColorScheme::Light,
        _ => SystemColorScheme::Unknown,
    }
}

/// Parse a `GTK_THEME` environment variable value (e.g. `"Adwaita:dark"`,
/// `"Yaru-dark"`, `"Breeze-Light"`, `"Adwaita:light"`).
pub fn parse_gtk_theme_color_scheme(gtk_theme: &str) -> SystemColorScheme {
    let trimmed = gtk_theme.trim();
    if trimmed.is_empty() {
        return SystemColorScheme::Unknown;
    }

    // Explicit `ThemeName:variant` syntax takes precedence over theme name tokens.
    if let Some((_base, variant)) = trimmed.rsplit_once(':') {
        let variant_trimmed = variant.trim();
        if variant_trimmed.eq_ignore_ascii_case("dark") {
            return SystemColorScheme::Dark;
        }
        if variant_trimmed.eq_ignore_ascii_case("light") {
            return SystemColorScheme::Light;
        }
    }

    let mut saw_light = false;
    for token in trimmed.split([':', '-', '_', ' ', '/']) {
        if token.eq_ignore_ascii_case("dark") {
            return SystemColorScheme::Dark;
        }
        if token.eq_ignore_ascii_case("light") {
            saw_light = true;
        }
    }

    if saw_light {
        SystemColorScheme::Light
    } else {
        SystemColorScheme::Unknown
    }
}

/// Parse stdout from `gsettings get org.gnome.desktop.interface color-scheme`
/// (`'prefer-dark'`, `'prefer-light'`, `'default'`).
pub fn parse_gsettings_color_scheme(output: &str) -> SystemColorScheme {
    let cleaned = output.trim().trim_matches(|c| c == '\'' || c == '"').trim();
    if cleaned.eq_ignore_ascii_case("prefer-dark") || cleaned.eq_ignore_ascii_case("dark") {
        SystemColorScheme::Dark
    } else if cleaned.eq_ignore_ascii_case("prefer-light") || cleaned.eq_ignore_ascii_case("light")
    {
        SystemColorScheme::Light
    } else {
        SystemColorScheme::Unknown
    }
}

/// Parse Windows `AppsUseLightTheme` DWORD registry value (`0` = Dark, `1` = Light).
pub const fn parse_windows_apps_use_light_theme(value: u32) -> SystemColorScheme {
    match value {
        0 => SystemColorScheme::Dark,
        1 => SystemColorScheme::Light,
        _ => SystemColorScheme::Unknown,
    }
}

/// Parse macOS `AppleInterfaceStyle` preference (`"Dark"` = Dark, otherwise Light).
pub fn parse_macos_apple_interface_style(value: Option<&str>) -> SystemColorScheme {
    match value {
        Some(style) if style.trim().eq_ignore_ascii_case("dark") => SystemColorScheme::Dark,
        _ => SystemColorScheme::Light,
    }
}

/// Unwrap single- or double-nested `zvariant::Value` wrappers into `u32`.
///
/// `org.freedesktop.portal.Settings.Read` historically returns a double-wrapped
/// variant `v(v(u))`, whereas `ReadOne` and `SettingChanged` carry `v(u)`.
#[cfg(all(unix, not(target_os = "macos")))]
fn extract_u32_from_zvariant(val: &Value<'_>) -> Option<u32> {
    match val {
        Value::U32(v) => Some(*v),
        Value::I32(v) => u32::try_from(*v).ok(),
        Value::U64(v) => u32::try_from(*v).ok(),
        Value::I64(v) => u32::try_from(*v).ok(),
        Value::U8(v) => Some(u32::from(*v)),
        Value::Value(inner) => extract_u32_from_zvariant(inner),
        _ => None,
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn query_portal_color_scheme() -> Result<SystemColorScheme, String> {
    let connection =
        Connection::session().map_err(|e| format!("D-Bus session unavailable: {e}"))?;
    let proxy = Proxy::new(
        &connection,
        PORTAL_SERVICE,
        PORTAL_PATH,
        PORTAL_SETTINGS_IFACE,
    )
    .map_err(|e| format!("Could not create portal Settings proxy: {e}"))?;

    let raw: OwnedValue = proxy
        .call("Read", &(PORTAL_APPEARANCE_NS, PORTAL_COLOR_SCHEME_KEY))
        .map_err(|e| format!("Settings.Read failed: {e}"))?;

    let code = extract_u32_from_zvariant(&raw)
        .ok_or_else(|| "Unexpected variant type from Settings.Read".to_owned())?;
    Ok(parse_portal_color_scheme(code))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn detect_linux_color_scheme() -> SystemColorScheme {
    if let Ok(scheme) = query_portal_color_scheme() {
        if scheme != SystemColorScheme::Unknown {
            return scheme;
        }
    }

    if let Ok(gtk_theme) = std::env::var("GTK_THEME") {
        let scheme = parse_gtk_theme_color_scheme(&gtk_theme);
        if scheme != SystemColorScheme::Unknown {
            return scheme;
        }
    }

    if let Ok(output) = Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "color-scheme"])
        .output()
    {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let scheme = parse_gsettings_color_scheme(&stdout);
            if scheme != SystemColorScheme::Unknown {
                return scheme;
            }
        }
    }

    SystemColorScheme::Unknown
}

#[cfg(target_os = "windows")]
mod win_theme_ffi {
    use std::ffi::c_void;

    pub type Hkey = *mut c_void;
    pub type Bool = i32;

    // Matches Win32 `#define HKEY_CURRENT_USER ((HKEY)(ULONG_PTR)((LONG)0x80000001))`.
    pub const HKEY_CURRENT_USER: Hkey = (-2147483647i32 as isize) as Hkey;
    pub const RRF_RT_REG_DWORD: u32 = 0x0000_0010;
    pub const ERROR_SUCCESS: u32 = 0;
    pub const KEY_NOTIFY: u32 = 0x0010;
    pub const REG_NOTIFY_CHANGE_LAST_SET: u32 = 0x0000_0004;

    #[link(name = "advapi32")]
    extern "system" {
        pub fn RegGetValueW(
            hkey: Hkey,
            lp_sub_key: *const u16,
            lp_value: *const u16,
            dw_flags: u32,
            pdw_type: *mut u32,
            pv_data: *mut c_void,
            pcb_data: *mut u32,
        ) -> u32;
        pub fn RegOpenKeyExW(
            hkey: Hkey,
            lp_sub_key: *const u16,
            ul_options: u32,
            sam_desired: u32,
            phk_result: *mut Hkey,
        ) -> u32;
        pub fn RegNotifyChangeKeyValue(
            hkey: Hkey,
            b_watch_subtree: Bool,
            dw_notify_filter: u32,
            h_event: *mut c_void,
            f_asynchronous: Bool,
        ) -> u32;
        pub fn RegCloseKey(hkey: Hkey) -> u32;
    }

    pub fn wide_nul(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }
}

#[cfg(target_os = "windows")]
fn detect_windows_color_scheme() -> SystemColorScheme {
    use std::ffi::c_void;

    let sub_key = win_theme_ffi::wide_nul(
        r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
    );
    let value_name = win_theme_ffi::wide_nul("AppsUseLightTheme");
    let mut data: u32 = 0;
    let mut cb_data: u32 = std::mem::size_of::<u32>() as u32;

    let status = unsafe {
        win_theme_ffi::RegGetValueW(
            win_theme_ffi::HKEY_CURRENT_USER,
            sub_key.as_ptr(),
            value_name.as_ptr(),
            win_theme_ffi::RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut data as *mut u32).cast::<c_void>(),
            &mut cb_data,
        )
    };

    if status == win_theme_ffi::ERROR_SUCCESS && cb_data == std::mem::size_of::<u32>() as u32 {
        parse_windows_apps_use_light_theme(data)
    } else {
        SystemColorScheme::Unknown
    }
}

#[cfg(target_os = "macos")]
mod macos_theme_ffi {
    use std::ffi::{c_char, c_void};

    pub type CFAllocatorRef = *const c_void;
    pub type CFStringRef = *const c_void;
    pub type CFPropertyListRef = *const c_void;
    pub type CFStringEncoding = u32;
    pub type CFTypeID = usize;
    pub type CFIndex = isize;

    pub const K_CF_STRING_ENCODING_UTF8: CFStringEncoding = 0x0800_0100;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        pub static kCFPreferencesAnyApplication: CFStringRef;
        pub fn CFStringCreateWithCString(
            alloc: CFAllocatorRef,
            c_str: *const c_char,
            encoding: CFStringEncoding,
        ) -> CFStringRef;
        pub fn CFPreferencesCopyAppValue(
            key: CFStringRef,
            application_id: CFStringRef,
        ) -> CFPropertyListRef;
        pub fn CFGetTypeID(cf: *const c_void) -> CFTypeID;
        pub fn CFStringGetTypeID() -> CFTypeID;
        pub fn CFStringGetCString(
            the_string: CFStringRef,
            buffer: *mut c_char,
            buffer_size: CFIndex,
            encoding: CFStringEncoding,
        ) -> u8;
        pub fn CFRelease(cf: *const c_void);
    }
}

#[cfg(target_os = "macos")]
fn detect_macos_color_scheme() -> SystemColorScheme {
    use std::ffi::{CStr, CString};

    if let Ok(key_c) = CString::new("AppleInterfaceStyle") {
        unsafe {
            let key_cf = macos_theme_ffi::CFStringCreateWithCString(
                std::ptr::null(),
                key_c.as_ptr(),
                macos_theme_ffi::K_CF_STRING_ENCODING_UTF8,
            );
            if !key_cf.is_null() {
                let val = macos_theme_ffi::CFPreferencesCopyAppValue(
                    key_cf,
                    macos_theme_ffi::kCFPreferencesAnyApplication,
                );
                macos_theme_ffi::CFRelease(key_cf);

                if val.is_null() {
                    // Absence of `AppleInterfaceStyle` in global preferences indicates Light mode.
                    return parse_macos_apple_interface_style(None);
                }

                let mut result = SystemColorScheme::Light;
                if macos_theme_ffi::CFGetTypeID(val) == macos_theme_ffi::CFStringGetTypeID() {
                    let mut buf = [0u8; 64];
                    let ok = macos_theme_ffi::CFStringGetCString(
                        val,
                        buf.as_mut_ptr().cast(),
                        buf.len() as macos_theme_ffi::CFIndex,
                        macos_theme_ffi::K_CF_STRING_ENCODING_UTF8,
                    );
                    if ok != 0 {
                        if let Ok(c_str) = CStr::from_bytes_until_nul(&buf) {
                            result = parse_macos_apple_interface_style(c_str.to_str().ok());
                        }
                    }
                }
                macos_theme_ffi::CFRelease(val);
                return result;
            }
        }
    }

    // Fallback to `defaults read -g AppleInterfaceStyle` if CFString creation failed.
    if let Ok(output) = Command::new("defaults")
        .args(["read", "-g", "AppleInterfaceStyle"])
        .output()
    {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            return parse_macos_apple_interface_style(Some(&stdout));
        }
    }
    parse_macos_apple_interface_style(None)
}

/// Detect the current OS color scheme preference (`Dark`, `Light`, or `Unknown`).
pub fn detect_system_color_scheme() -> SystemColorScheme {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        detect_linux_color_scheme()
    }
    #[cfg(target_os = "windows")]
    {
        detect_windows_color_scheme()
    }
    #[cfg(target_os = "macos")]
    {
        detect_macos_color_scheme()
    }
    #[cfg(not(any(unix, target_os = "windows")))]
    {
        SystemColorScheme::Unknown
    }
}

/// Spawn a background watcher that invokes `callback` whenever the OS desktop
/// color scheme changes at runtime.
///
/// - **Linux/Unix**: Subscribes to `org.freedesktop.portal.Settings.SettingChanged`
///   for `("org.freedesktop.appearance", "color-scheme")`.
/// - **Windows**: Watches `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize`
///   via `RegNotifyChangeKeyValue`.
/// - **macOS**: Currently a no-op; use [`detect_system_color_scheme`] at startup
///   or on window activation.
pub fn spawn_color_scheme_watcher(callback: impl Fn(SystemColorScheme) + Send + 'static) {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::thread::Builder::new()
            .name("subomi-color-scheme-watcher".to_owned())
            .spawn(move || {
                let Ok(connection) = Connection::session() else {
                    return;
                };
                let Ok(proxy) = Proxy::new(
                    &connection,
                    PORTAL_SERVICE,
                    PORTAL_PATH,
                    PORTAL_SETTINGS_IFACE,
                ) else {
                    return;
                };
                let Ok(signals) = proxy.receive_signal("SettingChanged") else {
                    return;
                };
                for message in signals {
                    let Ok((namespace, key, value)): zbus::Result<(String, String, OwnedValue)> =
                        message.body().deserialize()
                    else {
                        continue;
                    };
                    if namespace == PORTAL_APPEARANCE_NS && key == PORTAL_COLOR_SCHEME_KEY {
                        if let Some(code) = extract_u32_from_zvariant(&value) {
                            callback(parse_portal_color_scheme(code));
                        }
                    }
                }
            });
    }

    #[cfg(target_os = "windows")]
    {
        let _ = std::thread::Builder::new()
            .name("subomi-color-scheme-watcher".to_owned())
            .spawn(move || {
                let sub_key = win_theme_ffi::wide_nul(
                    r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
                );
                let mut hkey: win_theme_ffi::Hkey = std::ptr::null_mut();
                let open_status = unsafe {
                    win_theme_ffi::RegOpenKeyExW(
                        win_theme_ffi::HKEY_CURRENT_USER,
                        sub_key.as_ptr(),
                        0,
                        win_theme_ffi::KEY_NOTIFY,
                        &mut hkey,
                    )
                };
                if open_status != win_theme_ffi::ERROR_SUCCESS || hkey.is_null() {
                    return;
                }

                let mut last = detect_windows_color_scheme();
                loop {
                    let wait_status = unsafe {
                        win_theme_ffi::RegNotifyChangeKeyValue(
                            hkey,
                            0,
                            win_theme_ffi::REG_NOTIFY_CHANGE_LAST_SET,
                            std::ptr::null_mut(),
                            0,
                        )
                    };
                    if wait_status != win_theme_ffi::ERROR_SUCCESS {
                        break;
                    }
                    let current = detect_windows_color_scheme();
                    if current != last {
                        last = current;
                        callback(current);
                    }
                }
                unsafe {
                    win_theme_ffi::RegCloseKey(hkey);
                }
            });
    }

    #[cfg(not(any(all(unix, not(target_os = "macos")), target_os = "windows")))]
    {
        let _ = callback;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portal_color_scheme_values_parse_according_to_xdg_spec() {
        assert_eq!(parse_portal_color_scheme(0), SystemColorScheme::Unknown);
        assert_eq!(parse_portal_color_scheme(1), SystemColorScheme::Dark);
        assert_eq!(parse_portal_color_scheme(2), SystemColorScheme::Light);
        assert_eq!(parse_portal_color_scheme(3), SystemColorScheme::Unknown);
        assert_eq!(parse_portal_color_scheme(u32::MAX), SystemColorScheme::Unknown);
    }

    #[test]
    fn gtk_theme_parsing_handles_variants_suffixes_and_case() {
        assert_eq!(
            parse_gtk_theme_color_scheme("Adwaita:dark"),
            SystemColorScheme::Dark
        );
        assert_eq!(
            parse_gtk_theme_color_scheme("Adwaita:DARK"),
            SystemColorScheme::Dark
        );
        assert_eq!(
            parse_gtk_theme_color_scheme("Adwaita-dark"),
            SystemColorScheme::Dark
        );
        assert_eq!(
            parse_gtk_theme_color_scheme("Breeze_Dark"),
            SystemColorScheme::Dark
        );
        assert_eq!(
            parse_gtk_theme_color_scheme("Yaru-dark-compact"),
            SystemColorScheme::Dark
        );
        assert_eq!(
            parse_gtk_theme_color_scheme("Adwaita:light"),
            SystemColorScheme::Light
        );
        assert_eq!(
            parse_gtk_theme_color_scheme("Breeze-Light"),
            SystemColorScheme::Light
        );
        // Explicit `:light` variant overrides a theme base name containing `dark`.
        assert_eq!(
            parse_gtk_theme_color_scheme("Adwaita-dark:light"),
            SystemColorScheme::Light
        );
        // Neutral/unmarked theme names and empty strings stay Unknown so fallbacks run.
        assert_eq!(
            parse_gtk_theme_color_scheme("Adwaita"),
            SystemColorScheme::Unknown
        );
        assert_eq!(
            parse_gtk_theme_color_scheme("Arc-Darker"),
            SystemColorScheme::Unknown
        );
        assert_eq!(parse_gtk_theme_color_scheme(""), SystemColorScheme::Unknown);
        assert_eq!(
            parse_gtk_theme_color_scheme("   "),
            SystemColorScheme::Unknown
        );
    }

    #[test]
    fn gsettings_windows_and_macos_theme_parsers_handle_all_cases() {
        assert_eq!(
            parse_gsettings_color_scheme("'prefer-dark'\n"),
            SystemColorScheme::Dark
        );
        assert_eq!(
            parse_gsettings_color_scheme("'prefer-light'\n"),
            SystemColorScheme::Light
        );
        assert_eq!(
            parse_gsettings_color_scheme("'default'\n"),
            SystemColorScheme::Unknown
        );
        assert_eq!(
            parse_gsettings_color_scheme(""),
            SystemColorScheme::Unknown
        );

        assert_eq!(
            parse_windows_apps_use_light_theme(0),
            SystemColorScheme::Dark
        );
        assert_eq!(
            parse_windows_apps_use_light_theme(1),
            SystemColorScheme::Light
        );
        assert_eq!(
            parse_windows_apps_use_light_theme(2),
            SystemColorScheme::Unknown
        );

        assert_eq!(
            parse_macos_apple_interface_style(Some("Dark\n")),
            SystemColorScheme::Dark
        );
        assert_eq!(
            parse_macos_apple_interface_style(Some("dark")),
            SystemColorScheme::Dark
        );
        assert_eq!(
            parse_macos_apple_interface_style(None),
            SystemColorScheme::Light
        );
        assert_eq!(
            parse_macos_apple_interface_style(Some("Light")),
            SystemColorScheme::Light
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn portal_zvariant_unwraps_both_single_and_double_nested_variants() {
        let single = Value::U32(1);
        assert_eq!(extract_u32_from_zvariant(&single), Some(1));

        let double = Value::Value(Box::new(Value::U32(2)));
        assert_eq!(extract_u32_from_zvariant(&double), Some(2));

        let triple = Value::Value(Box::new(Value::Value(Box::new(Value::U32(0)))));
        assert_eq!(extract_u32_from_zvariant(&triple), Some(0));

        let non_numeric = Value::from("dark");
        assert_eq!(extract_u32_from_zvariant(&non_numeric), None);
    }
}
