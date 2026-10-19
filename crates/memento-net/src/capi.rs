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

//! C ABI surface for `memento-net`.
//! Provides `extern "C"` functions for AnkiConnect requests, file downloads
//! with progress callbacks, and memory deallocation.

use std::ffi::{c_char, c_void, CStr, CString};
use std::path::Path;
use std::time::Duration;

use crate::download::Downloader;
use crate::http::{HttpClient, HttpClientConfig, Request, Url};

/// Progress callback invoked during file downloads:
/// `(received_bytes, total_bytes, speed_bps, user_data)`.
pub type ProgressCallback =
    unsafe extern "C" fn(received: i64, total: i64, speed: i64, user_data: *mut c_void);

/// Converts a C string pointer to a Rust `&str`. Returns `None` for null pointers.
///
/// # Safety
/// If non-null, `ptr` must point to a valid null-terminated UTF-8 string.
unsafe fn cstr_to_str<'a>(ptr: *const c_char) -> Result<Option<&'a str>, &'static str>
{
    if ptr.is_null()
    {
        return Ok(None);
    }
    match CStr::from_ptr(ptr).to_str()
    {
        Ok(s) => Ok(Some(s)),
        Err(_) => Err("invalid UTF-8 string"),
    }
}

/// Allocates an owned C string for `s` and writes its pointer and length to
/// `out_ptr` and `out_len`.
///
/// # Safety
/// If non-null, `out_ptr` and `out_len` must be writable.
unsafe fn write_out_string(s: &str, out_ptr: *mut *mut c_char, out_len: *mut u64)
{
    let bytes = s.as_bytes();
    // Replace any internal null bytes to guarantee CString succeeds
    let safe_bytes: Vec<u8> = bytes
        .iter()
        .map(|&b| if b == 0 { b'?' } else { b })
        .collect();
    if let Ok(c_str) = CString::new(safe_bytes)
    {
        let raw = c_str.into_raw();
        if !out_ptr.is_null()
        {
            *out_ptr = raw;
        }
        else
        {
            drop(CString::from_raw(raw));
        }
        if !out_len.is_null()
        {
            *out_len = bytes.len() as u64;
        }
    }
}

/// Writes an error message to `out_err` and `out_err_len`, returning `false`.
///
/// # Safety
/// If non-null, `out_err` and `out_err_len` must be writable.
unsafe fn set_error(err: &str, out_err: *mut *mut c_char, out_err_len: *mut u64) -> bool
{
    write_out_string(err, out_err, out_err_len);
    false
}

/// Sends an AnkiConnect action request over HTTP/1.1.
///
/// # Safety
/// `url_utf8` and `action_utf8` must point to valid null-terminated UTF-8 strings.
/// `key_utf8` and `params_json_utf8` may be null or point to null-terminated UTF-8 strings.
/// Output pointers that are non-null must be writable.
#[no_mangle]
pub unsafe extern "C" fn memento_net_anki_request(
    url_utf8: *const c_char,
    action_utf8: *const c_char,
    version: i32,
    key_utf8: *const c_char,
    params_json_utf8: *const c_char,
    out_reply_json: *mut *mut c_char,
    out_reply_len: *mut u64,
    out_err: *mut *mut c_char,
    out_err_len: *mut u64,
) -> bool
{
    if !out_reply_json.is_null()
    {
        *out_reply_json = std::ptr::null_mut();
    }
    if !out_reply_len.is_null()
    {
        *out_reply_len = 0;
    }
    if !out_err.is_null()
    {
        *out_err = std::ptr::null_mut();
    }
    if !out_err_len.is_null()
    {
        *out_err_len = 0;
    }

    let unwind_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let url_str = match cstr_to_str(url_utf8)
        {
            Ok(Some(s)) if !s.is_empty() => s,
            Ok(_) => return set_error("url is null or empty", out_err, out_err_len),
            Err(e) => return set_error(e, out_err, out_err_len),
        };

        let action_str = match cstr_to_str(action_utf8)
        {
            Ok(Some(s)) if !s.is_empty() => s,
            Ok(_) => return set_error("action is null or empty", out_err, out_err_len),
            Err(e) => return set_error(e, out_err, out_err_len),
        };

        let key_opt = match cstr_to_str(key_utf8)
        {
            Ok(opt) => opt,
            Err(e) => return set_error(e, out_err, out_err_len),
        };

        let params_val = match cstr_to_str(params_json_utf8)
        {
            Ok(Some(s)) if !s.trim().is_empty() => {
                match serde_json::from_str::<serde_json::Value>(s)
                {
                    Ok(val) => Some(val),
                    Err(e) => {
                        return set_error(
                            &format!("invalid params JSON: {e}"),
                            out_err,
                            out_err_len,
                        )
                    }
                }
            }
            Ok(_) => None,
            Err(e) => return set_error(e, out_err, out_err_len),
        };

        let mut msg = serde_json::json!({
            "action": action_str,
            "version": version,
        });
        if let Some(key) = key_opt
        {
            msg["key"] = serde_json::json!(key);
        }
        if let Some(params) = params_val
        {
            msg["params"] = params;
        }

        let body = match serde_json::to_vec(&msg)
        {
            Ok(b) => b,
            Err(e) => {
                return set_error(
                    &format!("JSON serialization failed: {e}"),
                    out_err,
                    out_err_len,
                )
            }
        };

        let url = match Url::parse(url_str)
        {
            Ok(u) => u,
            Err(e) => return set_error(&format!("invalid URL: {e}"), out_err, out_err_len),
        };

        let request = Request::post(url, body).header("Content-Type", "application/json");

        let config = HttpClientConfig {
            timeout: Some(Duration::from_millis(5000)),
            ..HttpClientConfig::default()
        };
        let client = HttpClient::new(config);

        let mut response = match client.send(&request)
        {
            Ok(resp) => resp,
            Err(e) => return set_error(&e.to_string(), out_err, out_err_len),
        };

        if response.status() >= 400
        {
            return set_error(
                &format!("HTTP error {}: {}", response.status(), response.reason()),
                out_err,
                out_err_len,
            );
        }

        const MAX_REPLY_BYTES: u64 = 32 * 1024 * 1024;
        let reply_bytes = match response.read_all(MAX_REPLY_BYTES)
        {
            Ok(b) => b,
            Err(e) => {
                return set_error(
                    &format!("failed to read response body: {e}"),
                    out_err,
                    out_err_len,
                )
            }
        };

        let reply_str = match std::str::from_utf8(&reply_bytes)
        {
            Ok(s) => s,
            Err(e) => {
                return set_error(
                    &format!("response is not valid UTF-8: {e}"),
                    out_err,
                    out_err_len,
                )
            }
        };

        write_out_string(reply_str, out_reply_json, out_reply_len);
        true
    }));

    match unwind_res
    {
        Ok(success) => success,
        Err(_) => set_error("panic in memento_net_anki_request", out_err, out_err_len),
    }
}

/// Downloads a file from `url_utf8` to `target_path_utf8` with optional progress reporting.
///
/// # Safety
/// `url_utf8` and `target_path_utf8` must point to valid null-terminated UTF-8 strings.
/// Output pointers that are non-null must be writable.
#[no_mangle]
pub unsafe extern "C" fn memento_net_download_file(
    url_utf8: *const c_char,
    target_path_utf8: *const c_char,
    timeout_ms: i64,
    progress_cb: Option<ProgressCallback>,
    user_data: *mut c_void,
    out_err: *mut *mut c_char,
    out_err_len: *mut u64,
) -> bool
{
    if !out_err.is_null()
    {
        *out_err = std::ptr::null_mut();
    }
    if !out_err_len.is_null()
    {
        *out_err_len = 0;
    }

    let unwind_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let url_str = match cstr_to_str(url_utf8)
        {
            Ok(Some(s)) if !s.is_empty() => s,
            Ok(_) => return set_error("url is null or empty", out_err, out_err_len),
            Err(e) => return set_error(e, out_err, out_err_len),
        };

        let target_str = match cstr_to_str(target_path_utf8)
        {
            Ok(Some(s)) if !s.is_empty() => s,
            Ok(_) => return set_error("target path is null or empty", out_err, out_err_len),
            Err(e) => return set_error(e, out_err, out_err_len),
        };

        let target_path = Path::new(target_str);

        let timeout = if timeout_ms > 0
        {
            Some(Duration::from_millis(timeout_ms as u64))
        }
        else
        {
            None
        };

        let config = HttpClientConfig {
            timeout,
            connect_timeout: timeout.or(HttpClientConfig::default().connect_timeout),
            ..HttpClientConfig::default()
        };
        let client = HttpClient::new(config);
        let downloader = Downloader::new(client);

        let result = downloader.download(url_str, target_path, |event| {
            if let Some(cb) = progress_cb
            {
                cb(
                    event.progress.received,
                    event.progress.total,
                    event.progress.speed_bps,
                    user_data,
                );
            }
            Ok(())
        });

        match result
        {
            Ok(_) => true,
            Err(err) => set_error(&err.to_string(), out_err, out_err_len),
        }
    }));

    match unwind_res
    {
        Ok(success) => success,
        Err(_) => set_error(
            "panic in memento_net_download_file",
            out_err,
            out_err_len,
        ),
    }
}

/// Frees a string allocated by memento-net C API functions.
///
/// # Safety
/// If non-null, `ptr` must be a pointer allocated by a memento-net C API function.
#[no_mangle]
pub unsafe extern "C" fn memento_net_free_string(ptr: *mut c_char)
{
    if !ptr.is_null()
    {
        drop(CString::from_raw(ptr));
    }
}

#[cfg(test)]
mod tests
{
    use super::*;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn free_string_null_and_valid()
    {
        unsafe {
            memento_net_free_string(std::ptr::null_mut());
            let cstr = CString::new("test string").unwrap();
            let raw = cstr.into_raw();
            memento_net_free_string(raw);
        }
    }

    #[test]
    fn anki_request_invalid_inputs()
    {
        unsafe {
            let mut out_reply: *mut c_char = std::ptr::null_mut();
            let mut out_reply_len: u64 = 0;
            let mut out_err: *mut c_char = std::ptr::null_mut();
            let mut out_err_len: u64 = 0;

            // Null URL
            let ok = memento_net_anki_request(
                std::ptr::null(),
                b"version\0".as_ptr() as *const c_char,
                6,
                std::ptr::null(),
                std::ptr::null(),
                &mut out_reply,
                &mut out_reply_len,
                &mut out_err,
                &mut out_err_len,
            );
            assert!(!ok);
            assert!(!out_err.is_null());
            memento_net_free_string(out_err);

            // Invalid JSON params
            out_err = std::ptr::null_mut();
            let ok = memento_net_anki_request(
                b"http://127.0.0.1:8765\0".as_ptr() as *const c_char,
                b"version\0".as_ptr() as *const c_char,
                6,
                std::ptr::null(),
                b"not json{\0".as_ptr() as *const c_char,
                &mut out_reply,
                &mut out_reply_len,
                &mut out_err,
                &mut out_err_len,
            );
            assert!(!ok);
            assert!(!out_err.is_null());
            memento_net_free_string(out_err);
        }
    }

    #[test]
    fn anki_request_mock_roundtrip()
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 1024];
            loop
            {
                let n = stream.read(&mut buf).unwrap();
                if n == 0
                {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
                if let Some(head_end) =
                    request.windows(4).position(|window| window == b"\r\n\r\n")
                {
                    let head = std::str::from_utf8(&request[..head_end]).unwrap();
                    let content_length = head
                        .lines()
                        .find_map(|line| {
                            line.strip_prefix("Content-Length: ")
                                .and_then(|value| value.parse::<usize>().ok())
                        })
                        .unwrap();
                    if request.len() >= head_end + 4 + content_length
                    {
                        break;
                    }
                }
            }
            let head_end = request.windows(4).position(|window| window == b"\r\n\r\n").unwrap();
            assert!(request.starts_with(b"POST / HTTP/1.1\r\n"));
            let payload: serde_json::Value =
                serde_json::from_slice(&request[head_end + 4..]).unwrap();
            assert_eq!(payload["key"], "");

            let reply = r#"{"result":6,"error":null}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                reply.len(),
                reply
            );
            stream.write_all(resp.as_bytes()).unwrap();
        });

        unsafe {
            let url = format!("http://127.0.0.1:{}\0", port);
            let mut out_reply: *mut c_char = std::ptr::null_mut();
            let mut out_reply_len: u64 = 0;
            let mut out_err: *mut c_char = std::ptr::null_mut();
            let mut out_err_len: u64 = 0;

            let ok = memento_net_anki_request(
                url.as_ptr() as *const c_char,
                b"version\0".as_ptr() as *const c_char,
                6,
                b"\0".as_ptr() as *const c_char,
                std::ptr::null(),
                &mut out_reply,
                &mut out_reply_len,
                &mut out_err,
                &mut out_err_len,
            );
            assert!(ok);
            assert!(!out_reply.is_null());
            let reply_str = CStr::from_ptr(out_reply).to_str().unwrap();
            assert_eq!(reply_str, r#"{"result":6,"error":null}"#);
            assert_eq!(out_reply_len, reply_str.len() as u64);
            memento_net_free_string(out_reply);
        }
    }

    #[test]
    fn download_file_mock_roundtrip()
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf).unwrap();

            let payload = b"download test content";
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                payload.len()
            );
            stream.write_all(resp.as_bytes()).unwrap();
            stream.write_all(payload).unwrap();
        });

        let temp_dir = std::env::temp_dir().join(format!("memento_capi_test_{}", std::process::id()));
        fs::create_dir_all(&temp_dir).unwrap();
        let target_file = temp_dir.join("test.bin");
        let target_cstr = format!("{}\0", target_file.display());
        let url_cstr = format!("http://127.0.0.1:{}/test.bin\0", port);

        #[derive(Default)]
        struct ProgressData {
            invocations: usize,
            last_received: i64,
            last_total: i64,
        }

        unsafe extern "C" fn progress_tracker(
            received: i64,
            total: i64,
            _speed: i64,
            user_data: *mut c_void,
        ) {
            let data = &mut *(user_data as *mut ProgressData);
            data.invocations += 1;
            data.last_received = received;
            data.last_total = total;
        }

        let mut progress_data = ProgressData::default();
        let mut out_err: *mut c_char = std::ptr::null_mut();
        let mut out_err_len: u64 = 0;

        unsafe {
            let ok = memento_net_download_file(
                url_cstr.as_ptr() as *const c_char,
                target_cstr.as_ptr() as *const c_char,
                2000,
                Some(progress_tracker),
                &mut progress_data as *mut ProgressData as *mut c_void,
                &mut out_err,
                &mut out_err_len,
            );
            assert!(ok, "download failed: {:?}", out_err);
            assert!(target_file.exists());
            assert_eq!(fs::read(&target_file).unwrap(), b"download test content");
            assert!(progress_data.invocations > 0);
            assert_eq!(progress_data.last_received, 21);
            assert_eq!(progress_data.last_total, 21);
        }

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
