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

//! C ABI surface for `memento-config`.
//!
//! Provides a durable, Qt-free configuration store handle for C/C++ clients.
//! Converts raw C pointers (UTF-16 strings, primitive scalars) into the typed
//! Rust configuration model, ensuring strict observable parity with QSettings.

use std::fs;
use std::path::{Path, PathBuf};
use std::ptr;

use crate::canonical::{decode_strings, FORMAT_MAGIC};
use crate::config::{Config, RawDocument};
use crate::keys::{self, GENERAL_GROUP};
use crate::value::{Value, ValueType};

/// Opaque wrapper around the Rust `Config` store and its bound filesystem path.
pub struct MementoConfigHandle {
    pub(crate) config: Config,
    pub(crate) file_path: PathBuf,
}

fn decode_u16(ptr: *const u16, len: u64) -> String {
    if ptr.is_null() || len == 0 {
        String::new()
    } else {
        let slice = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
        String::from_utf16_lossy(slice)
    }
}

fn own_u16_slice(items: Vec<u16>) -> (*mut u16, u64) {
    if items.is_empty() {
        return (ptr::null_mut(), 0);
    }
    let boxed = items.into_boxed_slice();
    let len = boxed.len() as u64;
    (Box::into_raw(boxed) as *mut u16, len)
}

unsafe fn drop_boxed_u16(data: *mut u16, len: u64) {
    if !data.is_null() {
        drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
            data,
            len as usize,
        )));
    }
}

fn resolve_coordinate(section: &str, key: &str) -> (String, String) {
    let key = key.trim();
    let sec = section.trim();

    // Build the combined path
    let combined = if sec.is_empty() || sec.eq_ignore_ascii_case(GENERAL_GROUP) {
        key.to_string()
    } else if key.is_empty() {
        sec.to_string()
    } else {
        format!("{sec}/{key}")
    };

    // Normalize backslashes (array syntax like 1\name or 1/name) to slashes
    let normalized = combined.replace('\\', "/");

    // If there is any separator, split at the LAST slash
    if let Some((grp_part, key_part)) = normalized.rsplit_once('/') {
        let grp = if grp_part.is_empty() || grp_part.eq_ignore_ascii_case(GENERAL_GROUP) {
            GENERAL_GROUP.to_string()
        } else {
            grp_part.to_string()
        };
        (grp, key_part.to_string())
    } else {
        let grp = if sec.eq_ignore_ascii_case(GENERAL_GROUP) {
            sec.to_string()
        } else {
            GENERAL_GROUP.to_string()
        };
        (grp, normalized)
    }
}

fn config_get_at(config: &Config, grp: &str, k: &str) -> Option<Value> {
    if let Some(val) = config.get_at(grp, k) {
        return Some(val);
    }
    if grp.eq_ignore_ascii_case(GENERAL_GROUP) {
        let alt = if grp == GENERAL_GROUP { "General" } else { GENERAL_GROUP };
        if let Some(val) = config.get_at(alt, k) {
            return Some(val);
        }
    }
    None
}

fn config_raw_at<'a>(config: &'a Config, grp: &str, k: &str) -> Option<&'a str> {
    if let Some(raw) = config.raw_at(&crate::keys::KeyPath::new(grp, k)) {
        return Some(raw);
    }
    if grp.eq_ignore_ascii_case(GENERAL_GROUP) {
        let alt = if grp == GENERAL_GROUP { "General" } else { GENERAL_GROUP };
        if let Some(raw) = config.raw_at(&crate::keys::KeyPath::new(alt, k)) {
            return Some(raw);
        }
    }
    None
}

fn target_group<'a>(config: &Config, grp: &'a str, k: &str) -> &'a str {
    if grp.eq_ignore_ascii_case(GENERAL_GROUP) {
        if config.raw_at(&crate::keys::KeyPath::new("General", k)).is_some()
            || config.groups().iter().any(|g| g == "General")
        {
            return "General";
        }
    }
    grp
}


/// Creates a new configuration store handle bound to `ini_path`.
///
/// If the file exists on disk, it is loaded (supporting both canonical format
/// and legacy QSettings INI format). If it does not exist, an empty configuration
/// is initialized and will be saved to `ini_path` upon `memento_config_sync`.
/// If `ini_path` contains newline or section markup rather than an existing file,
/// it is parsed directly as an in-memory INI document.
///
/// # Safety
/// If `path_len > 0`, `ini_path` must point to at least `path_len` readable `uint16_t`s.
#[no_mangle]
pub unsafe extern "C" fn memento_config_create(
    ini_path: *const u16,
    path_len: u64,
) -> *mut MementoConfigHandle {
    let path_str = decode_u16(ini_path, path_len);
    let file_path = PathBuf::from(&path_str);

    // 1. If the path exists as a file, load it.
    if !path_str.is_empty() && file_path.is_file() {
        if let Ok(text) = fs::read_to_string(&file_path) {
            let clean = text.strip_prefix('\u{FEFF}').unwrap_or(&text).trim_start();
            if clean.starts_with(FORMAT_MAGIC) {
                if let Ok(doc) = Config::parse_document_text(&text) {
                    let cfg = Config::from_document(doc);
                    return Box::into_raw(Box::new(MementoConfigHandle {
                        config: cfg,
                        file_path,
                    }));
                }
            }
            if let Ok(legacy) = crate::legacy_ini::parse_legacy_ini(&text) {
                let doc = RawDocument {
                    revision: Some(0),
                    changelog_rows: Vec::new(),
                    sections: legacy.sections,
                };
                let cfg = Config::from_document(doc);
                return Box::into_raw(Box::new(MementoConfigHandle {
                    config: cfg,
                    file_path,
                }));
            }
        }
    }

    // 2. If it is not an existing file, but contains INI markup (newlines, '[' and '='),
    // parse it directly in-memory as an INI string.
    if !path_str.is_empty()
        && (path_str.contains('\n') || (path_str.contains('[') && path_str.contains('=')))
    {
        let clean = path_str.strip_prefix('\u{FEFF}').unwrap_or(&path_str).trim_start();
        if clean.starts_with(FORMAT_MAGIC) {
            if let Ok(doc) = Config::parse_document_text(&path_str) {
                let cfg = Config::from_document(doc);
                return Box::into_raw(Box::new(MementoConfigHandle {
                    config: cfg,
                    file_path: PathBuf::new(),
                }));
            }
        }
        if let Ok(legacy) = crate::legacy_ini::parse_legacy_ini(&path_str) {
            let doc = RawDocument {
                revision: Some(0),
                changelog_rows: Vec::new(),
                sections: legacy.sections,
            };
            let cfg = Config::from_document(doc);
            return Box::into_raw(Box::new(MementoConfigHandle {
                config: cfg,
                file_path: PathBuf::new(),
            }));
        }
    }

    // 3. Otherwise, empty default configuration bound to file_path.
    let cfg = Config::default_empty();
    Box::into_raw(Box::new(MementoConfigHandle {
        config: cfg,
        file_path,
    }))
}

/// Destroys a configuration store handle.
///
/// # Safety
/// `handle` must be null or a valid pointer returned by `memento_config_create`.
#[no_mangle]
pub unsafe extern "C" fn memento_config_free(handle: *mut MementoConfigHandle) {
    if !handle.is_null() {
        drop(Box::from_raw(handle));
    }
}

/// Reads an integer configuration value with fallback.
///
/// # Safety
/// String pointers must point to at least their respective length in `uint16_t`s when non-null.
#[no_mangle]
pub unsafe extern "C" fn memento_config_get_int(
    handle: *const MementoConfigHandle,
    section: *const u16,
    section_len: u64,
    key: *const u16,
    key_len: u64,
    default_val: i64,
) -> i64 {
    if handle.is_null() {
        return default_val;
    }
    let handle = &*handle;
    let sec_str = decode_u16(section, section_len);
    let key_str = decode_u16(key, key_len);
    let (grp, k) = resolve_coordinate(&sec_str, &key_str);

    match config_get_at(&handle.config, &grp, &k) {
        Some(Value::Int(n)) => n,
        Some(Value::Bool(b)) => {
            if b {
                1
            } else {
                0
            }
        }
        Some(Value::Real(f)) => f as i64,
        Some(Value::Text(s)) => s.parse::<i64>().unwrap_or(default_val),
        _ => default_val,
    }
}

/// Reads a boolean configuration value with fallback.
///
/// # Safety
/// String pointers must point to at least their respective length in `uint16_t`s when non-null.
#[no_mangle]
pub unsafe extern "C" fn memento_config_get_bool(
    handle: *const MementoConfigHandle,
    section: *const u16,
    section_len: u64,
    key: *const u16,
    key_len: u64,
    default_val: bool,
) -> bool {
    if handle.is_null() {
        return default_val;
    }
    let handle = &*handle;
    let sec_str = decode_u16(section, section_len);
    let key_str = decode_u16(key, key_len);
    let (grp, k) = resolve_coordinate(&sec_str, &key_str);

    match config_get_at(&handle.config, &grp, &k) {
        Some(Value::Bool(b)) => b,
        Some(Value::Int(n)) => n != 0,
        Some(Value::Real(f)) => f != 0.0,
        Some(Value::Text(s)) => {
            if s.eq_ignore_ascii_case("true") || s == "1" {
                true
            } else if s.eq_ignore_ascii_case("false") || s == "0" {
                false
            } else {
                default_val
            }
        }
        _ => default_val,
    }
}

/// Reads a double-precision float configuration value with fallback.
///
/// # Safety
/// String pointers must point to at least their respective length in `uint16_t`s when non-null.
#[no_mangle]
pub unsafe extern "C" fn memento_config_get_double(
    handle: *const MementoConfigHandle,
    section: *const u16,
    section_len: u64,
    key: *const u16,
    key_len: u64,
    default_val: f64,
) -> f64 {
    if handle.is_null() {
        return default_val;
    }
    let handle = &*handle;
    let sec_str = decode_u16(section, section_len);
    let key_str = decode_u16(key, key_len);
    let (grp, k) = resolve_coordinate(&sec_str, &key_str);

    match config_get_at(&handle.config, &grp, &k) {
        Some(Value::Real(f)) => f,
        Some(Value::Int(n)) => n as f64,
        Some(Value::Bool(b)) => {
            if b {
                1.0
            } else {
                0.0
            }
        }
        Some(Value::Text(s)) => s.parse::<f64>().unwrap_or(default_val),
        _ => default_val,
    }
}

/// Reads a UTF-16 string configuration value.
///
/// Returns `true` and allocates `*out_val` (to be freed with `memento_config_free_u16`)
/// if the key is found and convertible to text; returns `false` otherwise.
///
/// # Safety
/// `out_val` and `out_len` must be non-null writable pointers.
#[no_mangle]
pub unsafe extern "C" fn memento_config_get_string(
    handle: *const MementoConfigHandle,
    section: *const u16,
    section_len: u64,
    key: *const u16,
    key_len: u64,
    out_val: *mut *mut u16,
    out_len: *mut u64,
) -> bool {
    if handle.is_null() || out_val.is_null() || out_len.is_null() {
        return false;
    }
    *out_val = ptr::null_mut();
    *out_len = 0;

    let handle = &*handle;
    let sec_str = decode_u16(section, section_len);
    let key_str = decode_u16(key, key_len);
    let (grp, k) = resolve_coordinate(&sec_str, &key_str);

    let string_value = match config_get_at(&handle.config, &grp, &k) {
        Some(Value::Text(s)) => s,
        Some(Value::Int(n)) => n.to_string(),
        Some(Value::Bool(b)) => (if b { "true" } else { "false" }).to_string(),
        Some(Value::Real(f)) => f.to_string(),
        Some(Value::Strings(items)) => items.join(", "),
        Some(Value::Ints(items)) => items
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(", "),
        Some(Value::Map(_)) => return false,
        None => {
            if let Some(raw) = config_raw_at(&handle.config, &grp, &k) {
                raw.to_string()
            } else {
                return false;
            }
        }
    };

    let utf16: Vec<u16> = string_value.encode_utf16().collect();
    let (p, l) = own_u16_slice(utf16);
    *out_val = p;
    *out_len = l;
    true
}

/// Sets an integer configuration value.
///
/// # Safety
/// String pointers must point to at least their respective length in `uint16_t`s when non-null.
#[no_mangle]
pub unsafe extern "C" fn memento_config_set_int(
    handle: *mut MementoConfigHandle,
    section: *const u16,
    section_len: u64,
    key: *const u16,
    key_len: u64,
    val: i64,
) {
    if handle.is_null() {
        return;
    }
    let handle = &mut *handle;
    let sec_str = decode_u16(section, section_len);
    let key_str = decode_u16(key, key_len);
    let (grp, k) = resolve_coordinate(&sec_str, &key_str);
    let target_grp = target_group(&handle.config, &grp, &k);

    let ty = keys::lookup(target_grp, &k).map(|key_model| key_model.value_type(handle.config.context()));
    let value = match ty {
        Some(ValueType::Bool) => Value::Bool(val != 0),
        Some(ValueType::Real) => Value::Real(val as f64),
        Some(ValueType::Text) => Value::Text(val.to_string()),
        _ => Value::Int(val),
    };
    let _ = handle.config.put_at(target_grp, &k, &value);
}

/// Sets a boolean configuration value.
///
/// # Safety
/// String pointers must point to at least their respective length in `uint16_t`s when non-null.
#[no_mangle]
pub unsafe extern "C" fn memento_config_set_bool(
    handle: *mut MementoConfigHandle,
    section: *const u16,
    section_len: u64,
    key: *const u16,
    key_len: u64,
    val: bool,
) {
    if handle.is_null() {
        return;
    }
    let handle = &mut *handle;
    let sec_str = decode_u16(section, section_len);
    let key_str = decode_u16(key, key_len);
    let (grp, k) = resolve_coordinate(&sec_str, &key_str);
    let target_grp = target_group(&handle.config, &grp, &k);

    let ty = keys::lookup(target_grp, &k).map(|key_model| key_model.value_type(handle.config.context()));
    let value = match ty {
        Some(ValueType::Int) => Value::Int(if val { 1 } else { 0 }),
        Some(ValueType::Real) => Value::Real(if val { 1.0 } else { 0.0 }),
        Some(ValueType::Text) => Value::Text((if val { "true" } else { "false" }).to_string()),
        _ => Value::Bool(val),
    };
    let _ = handle.config.put_at(target_grp, &k, &value);
}

/// Sets a double-precision float configuration value.
///
/// # Safety
/// String pointers must point to at least their respective length in `uint16_t`s when non-null.
#[no_mangle]
pub unsafe extern "C" fn memento_config_set_double(
    handle: *mut MementoConfigHandle,
    section: *const u16,
    section_len: u64,
    key: *const u16,
    key_len: u64,
    val: f64,
) {
    if handle.is_null() {
        return;
    }
    let handle = &mut *handle;
    let sec_str = decode_u16(section, section_len);
    let key_str = decode_u16(key, key_len);
    let (grp, k) = resolve_coordinate(&sec_str, &key_str);
    let target_grp = target_group(&handle.config, &grp, &k);

    let ty = keys::lookup(target_grp, &k).map(|key_model| key_model.value_type(handle.config.context()));
    let value = match ty {
        Some(ValueType::Int) => Value::Int(val as i64),
        Some(ValueType::Bool) => Value::Bool(val != 0.0),
        Some(ValueType::Text) => Value::Text(val.to_string()),
        _ => Value::Real(val),
    };
    let _ = handle.config.put_at(target_grp, &k, &value);
}

/// Sets a UTF-16 string configuration value.
///
/// # Safety
/// String pointers must point to at least their respective length in `uint16_t`s when non-null.
#[no_mangle]
pub unsafe extern "C" fn memento_config_set_string(
    handle: *mut MementoConfigHandle,
    section: *const u16,
    section_len: u64,
    key: *const u16,
    key_len: u64,
    val: *const u16,
    val_len: u64,
) {
    if handle.is_null() {
        return;
    }
    let handle = &mut *handle;
    let sec_str = decode_u16(section, section_len);
    let key_str = decode_u16(key, key_len);
    let val_str = decode_u16(val, val_len);
    let (grp, k) = resolve_coordinate(&sec_str, &key_str);
    let target_grp = target_group(&handle.config, &grp, &k);

    let ty = keys::lookup(target_grp, &k).map(|key_model| key_model.value_type(handle.config.context()));
    let value = match ty {
        Some(ValueType::Int) => {
            if let Ok(n) = val_str.parse::<i64>() {
                Value::Int(n)
            } else {
                Value::Text(val_str)
            }
        }
        Some(ValueType::Bool) => {
            if val_str.eq_ignore_ascii_case("true") || val_str == "1" {
                Value::Bool(true)
            } else if val_str.eq_ignore_ascii_case("false") || val_str == "0" {
                Value::Bool(false)
            } else {
                Value::Text(val_str)
            }
        }
        Some(ValueType::Real) => {
            if let Ok(f) = val_str.parse::<f64>() {
                Value::Real(f)
            } else {
                Value::Text(val_str)
            }
        }
        Some(ValueType::Strings) => {
            if val_str.is_empty() {
                Value::Strings(Vec::new())
            } else if let Ok(items) = decode_strings(&val_str) {
                Value::Strings(items)
            } else {
                Value::Strings(val_str.split(',').map(|s| s.trim().to_string()).collect())
            }
        }
        Some(ValueType::Ints) => {
            if val_str.is_empty() {
                Value::Ints(Vec::new())
            } else if let Ok(items) = crate::canonical::decode_ints(&val_str) {
                Value::Ints(items)
            } else {
                let items: Vec<i64> = val_str
                    .trim_matches(|c| c == '[' || c == ']')
                    .split(',')
                    .filter_map(|s| s.trim().parse::<i64>().ok())
                    .collect();
                Value::Ints(items)
            }
        }
        _ => Value::Text(val_str),
    };
    let _ = handle.config.put_at(target_grp, &k, &value);
}

/// Atomically flushes the configuration store to disk.
///
/// Returns `true` on successful save, `false` otherwise.
///
/// # Safety
/// `handle` must be null or a valid pointer returned by `memento_config_create`.
#[no_mangle]
pub unsafe extern "C" fn memento_config_sync(handle: *mut MementoConfigHandle) -> bool {
    if handle.is_null() {
        return false;
    }
    let handle = &mut *handle;
    if handle.file_path.as_os_str().is_empty() {
        return false;
    }
    let (config_dir, file_name) = match (handle.file_path.parent(), handle.file_path.file_name()) {
        (Some(parent), Some(file)) => {
            let p = if parent.as_os_str().is_empty() {
                Path::new(".")
            } else {
                parent
            };
            (p, file.to_string_lossy().to_string())
        }
        (None, Some(file)) => (Path::new("."), file.to_string_lossy().to_string()),
        _ => return false,
    };
    handle.config.save(config_dir, &file_name).is_ok()
}

/// Deallocates a UTF-16 buffer allocated by `memento_config_get_string`.
///
/// # Safety
/// `ptr` must be null or an allocation returned by `memento_config_get_string` with matching `len`.
#[no_mangle]
pub unsafe extern "C" fn memento_config_free_u16(ptr: *mut u16, len: u64) {
    if !ptr.is_null() && len > 0 {
        drop_boxed_u16(ptr, len);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to_u16(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn test_capi_basic_get_set() {
        unsafe {
            let handle = memento_config_create(ptr::null(), 0);
            assert!(!handle.is_null());

            let section = to_u16("main-window");
            let key = to_u16("subtitle-list");
            assert!(!memento_config_get_bool(
                handle,
                section.as_ptr(),
                section.len() as u64,
                key.as_ptr(),
                key.len() as u64,
                false
            ));

            memento_config_set_bool(
                handle,
                section.as_ptr(),
                section.len() as u64,
                key.as_ptr(),
                key.len() as u64,
                true,
            );
            assert!(memento_config_get_bool(
                handle,
                section.as_ptr(),
                section.len() as u64,
                key.as_ptr(),
                key.len() as u64,
                false
            ));

            // Integer
            let ver_key = to_u16("version");
            assert_eq!(
                memento_config_get_int(
                    handle,
                    ptr::null(),
                    0,
                    ver_key.as_ptr(),
                    ver_key.len() as u64,
                    0
                ),
                0
            );
            memento_config_set_int(
                handle,
                ptr::null(),
                0,
                ver_key.as_ptr(),
                ver_key.len() as u64,
                6,
            );
            assert_eq!(
                memento_config_get_int(
                    handle,
                    ptr::null(),
                    0,
                    ver_key.as_ptr(),
                    ver_key.len() as u64,
                    0
                ),
                6
            );

            // Double
            let iface_sec = to_u16("interface");
            let scale_key = to_u16("scale");
            memento_config_set_double(
                handle,
                iface_sec.as_ptr(),
                iface_sec.len() as u64,
                scale_key.as_ptr(),
                scale_key.len() as u64,
                0.75,
            );
            let d = memento_config_get_double(
                handle,
                iface_sec.as_ptr(),
                iface_sec.len() as u64,
                scale_key.as_ptr(),
                scale_key.len() as u64,
                0.0,
            );
            assert!((d - 0.75).abs() < 1e-6);

            // String
            let search_sec = to_u16("search");
            let str_key = to_u16("replace-with");
            let str_val = to_u16("hello");
            memento_config_set_string(
                handle,
                search_sec.as_ptr(),
                search_sec.len() as u64,
                str_key.as_ptr(),
                str_key.len() as u64,
                str_val.as_ptr(),
                str_val.len() as u64,
            );

            let mut out_val = ptr::null_mut();
            let mut out_len = 0u64;
            let ok = memento_config_get_string(
                handle,
                search_sec.as_ptr(),
                search_sec.len() as u64,
                str_key.as_ptr(),
                str_key.len() as u64,
                &mut out_val,
                &mut out_len,
            );
            assert!(ok);
            assert_eq!(decode_u16(out_val, out_len), "hello");
            memento_config_free_u16(out_val, out_len);

            memento_config_free(handle);
        }
    }

    #[test]
    fn test_capi_legacy_ini_load_and_sync() {
        let legacy_ini = r#"
[main-window]
subtitle-list=true
search=true

[general]
version=6

[search]
delay=200
replace-with="replacement"
"#;
        unsafe {
            let u16_ini = to_u16(legacy_ini);
            let handle = memento_config_create(u16_ini.as_ptr(), u16_ini.len() as u64);
            assert!(!handle.is_null());

            let win = to_u16("main-window");
            let sub = to_u16("subtitle-list");
            assert!(memento_config_get_bool(
                handle,
                win.as_ptr(),
                win.len() as u64,
                sub.as_ptr(),
                sub.len() as u64,
                false
            ));

            let ver = to_u16("version");
            assert_eq!(
                memento_config_get_int(handle, ptr::null(), 0, ver.as_ptr(), ver.len() as u64, 0),
                6
            );

            let search = to_u16("search");
            let rep = to_u16("replace-with");
            let mut out_p = ptr::null_mut();
            let mut out_l = 0;
            assert!(memento_config_get_string(
                handle,
                search.as_ptr(),
                search.len() as u64,
                rep.as_ptr(),
                rep.len() as u64,
                &mut out_p,
                &mut out_l
            ));
            assert_eq!(decode_u16(out_p, out_l), "replacement");
            memento_config_free_u16(out_p, out_l);

            memento_config_free(handle);
        }
    }

    #[test]
    fn test_capi_file_save_and_reload() {
        let temp_dir = std::env::temp_dir().join(format!("memento_capi_test_{}", std::process::id()));
        let _ = fs::create_dir_all(&temp_dir);
        let file_path = temp_dir.join("test_config.store");
        let path_str = file_path.to_str().unwrap();
        let u16_path = to_u16(path_str);

        unsafe {
            let handle = memento_config_create(u16_path.as_ptr(), u16_path.len() as u64);
            assert!(!handle.is_null());

            let sec = to_u16("application");
            let key = to_u16("auto-update-check");
            memento_config_set_bool(
                handle,
                sec.as_ptr(),
                sec.len() as u64,
                key.as_ptr(),
                key.len() as u64,
                true,
            );

            let sync_ok = memento_config_sync(handle);
            assert!(sync_ok);
            memento_config_free(handle);

            // Reload from disk
            let reload_handle = memento_config_create(u16_path.as_ptr(), u16_path.len() as u64);
            assert!(!reload_handle.is_null());

            assert!(memento_config_get_bool(
                reload_handle,
                sec.as_ptr(),
                sec.len() as u64,
                key.as_ptr(),
                key.len() as u64,
                false
            ));

            memento_config_free(reload_handle);
        }
        let _ = fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn test_capi_resolve_coordinate_hierarchical() {
        assert_eq!(resolve_coordinate("", "volume"), ("general".to_string(), "volume".to_string()));
        assert_eq!(resolve_coordinate("General", "volume"), ("General".to_string(), "volume".to_string()));
        assert_eq!(resolve_coordinate("general", "volume"), ("general".to_string(), "volume".to_string()));
        assert_eq!(resolve_coordinate("", "main-window/subtitle-list"), ("main-window".to_string(), "subtitle-list".to_string()));
        assert_eq!(resolve_coordinate("main-window", "subtitle-list"), ("main-window".to_string(), "subtitle-list".to_string()));
        assert_eq!(resolve_coordinate("", "keybind/profiles/Default/card-add"), ("keybind/profiles/Default".to_string(), "card-add".to_string()));
        assert_eq!(resolve_coordinate("keybind", "profiles/Default/card-add"), ("keybind/profiles/Default".to_string(), "card-add".to_string()));
        assert_eq!(resolve_coordinate("keybind/profiles", "Default/card-add"), ("keybind/profiles/Default".to_string(), "card-add".to_string()));
        assert_eq!(resolve_coordinate("keybind/profiles/Default", "card-add"), ("keybind/profiles/Default".to_string(), "card-add".to_string()));
        assert_eq!(resolve_coordinate("audio", "1\\name"), ("audio/1".to_string(), "name".to_string()));
        assert_eq!(resolve_coordinate("", "audio/1\\name"), ("audio/1".to_string(), "name".to_string()));
    }

    #[test]
    fn test_capi_recent_files_and_dictionary_order() {
        let legacy_ini = r#"
[recent]
files=movie1.mkv, movie2.mkv

[dictionary]
order=10, 20, 30

[keybind/profiles/Default]
card-add=Ctrl+A
"#;
        unsafe {
            let u16_ini = to_u16(legacy_ini);
            let handle = memento_config_create(u16_ini.as_ptr(), u16_ini.len() as u64);
            assert!(!handle.is_null());

            // Recent files via section "recent"
            let sec_recent = to_u16("recent");
            let key_files = to_u16("files");
            let mut out_p = ptr::null_mut();
            let mut out_l = 0;
            assert!(memento_config_get_string(
                handle,
                sec_recent.as_ptr(),
                sec_recent.len() as u64,
                key_files.as_ptr(),
                key_files.len() as u64,
                &mut out_p,
                &mut out_l
            ));
            assert_eq!(decode_u16(out_p, out_l), "movie1.mkv, movie2.mkv");
            memento_config_free_u16(out_p, out_l);

            // Recent files via full path "recent/files" with null section
            let full_files = to_u16("recent/files");
            assert!(memento_config_get_string(
                handle,
                ptr::null(),
                0,
                full_files.as_ptr(),
                full_files.len() as u64,
                &mut out_p,
                &mut out_l
            ));
            assert_eq!(decode_u16(out_p, out_l), "movie1.mkv, movie2.mkv");
            memento_config_free_u16(out_p, out_l);

            // Dictionary order
            let sec_dict = to_u16("dictionary");
            let key_order = to_u16("order");
            assert!(memento_config_get_string(
                handle,
                sec_dict.as_ptr(),
                sec_dict.len() as u64,
                key_order.as_ptr(),
                key_order.len() as u64,
                &mut out_p,
                &mut out_l
            ));
            assert_eq!(decode_u16(out_p, out_l), "10, 20, 30");
            memento_config_free_u16(out_p, out_l);

            // Keybind with multi-part coordinate
            let full_keybind = to_u16("keybind/profiles/Default/card-add");
            assert!(memento_config_get_string(
                handle,
                ptr::null(),
                0,
                full_keybind.as_ptr(),
                full_keybind.len() as u64,
                &mut out_p,
                &mut out_l
            ));
            assert_eq!(decode_u16(out_p, out_l), "Ctrl+A");
            memento_config_free_u16(out_p, out_l);

            // Mutation of recent files and dictionary order
            let new_files = to_u16("new1.mkv, new2.mkv");
            memento_config_set_string(
                handle,
                sec_recent.as_ptr(),
                sec_recent.len() as u64,
                key_files.as_ptr(),
                key_files.len() as u64,
                new_files.as_ptr(),
                new_files.len() as u64,
            );
            assert!(memento_config_get_string(
                handle,
                sec_recent.as_ptr(),
                sec_recent.len() as u64,
                key_files.as_ptr(),
                key_files.len() as u64,
                &mut out_p,
                &mut out_l
            ));
            assert_eq!(decode_u16(out_p, out_l), "new1.mkv, new2.mkv");
            memento_config_free_u16(out_p, out_l);

            let new_order = to_u16("100, 200");
            memento_config_set_string(
                handle,
                sec_dict.as_ptr(),
                sec_dict.len() as u64,
                key_order.as_ptr(),
                key_order.len() as u64,
                new_order.as_ptr(),
                new_order.len() as u64,
            );
            assert!(memento_config_get_string(
                handle,
                sec_dict.as_ptr(),
                sec_dict.len() as u64,
                key_order.as_ptr(),
                key_order.len() as u64,
                &mut out_p,
                &mut out_l
            ));
            assert_eq!(decode_u16(out_p, out_l), "100, 200");
            memento_config_free_u16(out_p, out_l);

            memento_config_free(handle);
        }
    }
}
