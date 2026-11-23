//! Native Win32 COM (`IFileOpenDialog`) and macOS AppKit (`NSOpenPanel`) file
//! and folder pickers implemented directly over OS FFI (zero extra crates).
//!
//! ## Threading contract
//! - **Windows**: `IFileOpenDialog` requires a COM Single-Threaded Apartment
//!   (STA). Calling [`open_files_windows`] from a worker thread initializes STA
//!   cleanly; if invoked on an MTA thread (`RPC_E_CHANGED_MODE`), it automatically
//!   hops to a dedicated STA helper thread.
//! - **macOS**: AppKit `NSOpenPanel` MUST execute on the main thread and NEVER
//!   on a worker thread. [`open_files_macos_blocking`] checks
//!   `[NSThread isMainThread]` and synchronously hops to the main GCD queue via
//!   `dispatch_sync_f` if called off-thread, while [`open_files_macos_async`]
//!   submits directly to the main GCD queue via `dispatch_async_f`.

use std::collections::HashSet;
use std::path::Path;

use crate::{FileDialogKind, FileDialogOptions};

/// Format a list of glob patterns (e.g. `["*.mkv", "*.srt"]`) into the
/// semicolon-separated `COMDLG_FILTERSPEC::pszSpec` format expected by Win32
/// `IFileOpenDialog::SetFileTypes` (`"*.mkv;*.srt"`, mapping `"*"` to `"*.*"`).
#[cfg_attr(not(any(target_os = "windows", test)), allow(dead_code))]
pub(crate) fn format_windows_filter_spec(patterns: &[String]) -> String {
    let mut specs = Vec::with_capacity(patterns.len());
    for raw in patterns {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == "*" {
            specs.push("*.*");
        } else {
            specs.push(trimmed);
        }
    }
    if specs.is_empty() {
        "*.*".to_owned()
    } else {
        specs.join(";")
    }
}

/// Normalize a starting directory path for Win32 `SHCreateItemFromParsingName`
/// by converting forward slashes to backslashes.
#[cfg_attr(not(any(target_os = "windows", test)), allow(dead_code))]
pub(crate) fn normalize_windows_dialog_dir(path: &Path) -> String {
    path.to_string_lossy().replace('/', "\\")
}

/// Encode a Rust string as a NUL-terminated UTF-16 buffer for Win32 wide APIs.
#[cfg_attr(not(any(target_os = "windows", test)), allow(dead_code))]
pub(crate) fn encode_utf16_nul(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Extract deduplicated bare filename extensions (without leading `*.` or `.`)
/// for macOS `NSOpenPanel::setAllowedFileTypes:`. Returns an empty vector for
/// folder pickers or wildcard-only dialogs.
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub(crate) fn extract_macos_allowed_extensions(options: &FileDialogOptions) -> Vec<String> {
    if options.kind == FileDialogKind::Folder {
        return Vec::new();
    }
    let mut seen = HashSet::new();
    let mut extensions = Vec::new();
    for filter in options.filters() {
        for raw in filter.patterns {
            let trimmed = raw.trim();
            if trimmed.is_empty() || trimmed == "*" || trimmed == "*.*" {
                continue;
            }
            let ext = trimmed
                .strip_prefix("*.")
                .or_else(|| trimmed.strip_prefix('.'))
                .unwrap_or(trimmed);
            if ext.is_empty() || ext.contains('*') || ext.contains('?') {
                continue;
            }
            let lower = ext.to_ascii_lowercase();
            if seen.insert(lower.clone()) {
                extensions.push(lower);
            }
        }
    }
    extensions
}

/// Return `true` when any dialog filter includes an "All Files" wildcard (`*` or `*.*`).
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub(crate) fn has_all_files_wildcard(options: &FileDialogOptions) -> bool {
    options.filters().iter().any(|filter| {
        filter
            .patterns
            .iter()
            .any(|p| matches!(p.trim(), "*" | "*.*"))
    })
}

// ── Windows Native COM `IFileOpenDialog` ─────────────────────────────────────

#[cfg(target_os = "windows")]
mod win_com_dialog {
    use std::ffi::{c_void, OsString};
    use std::os::windows::ffi::OsStringExt;
    use std::path::PathBuf;

    use super::{encode_utf16_nul, format_windows_filter_spec, normalize_windows_dialog_dir};
    use crate::FileDialogOptions;

    type Hresult = i32;

    const S_OK: Hresult = 0;
    const S_FALSE: Hresult = 1;
    const RPC_E_CHANGED_MODE: Hresult = 0x8001_0106u32 as i32;

    const COINIT_APARTMENTTHREADED: u32 = 0x2;
    const COINIT_DISABLE_OLE1DDE: u32 = 0x4;
    const CLSCTX_INPROC_SERVER: u32 = 0x1;

    const FOS_PICKFOLDERS: u32 = 0x0000_0020;
    const FOS_FORCEFILESYSTEM: u32 = 0x0000_0040;
    const FOS_ALLOWMULTISELECT: u32 = 0x0000_0200;
    const FOS_PATHMUSTEXIST: u32 = 0x0000_0800;
    const FOS_FILEMUSTEXIST: u32 = 0x0000_1000;

    const SIGDN_FILESYSPATH: u32 = 0x8005_8000;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Guid {
        data1: u32,
        data2: u16,
        data3: u16,
        data4: [u8; 8],
    }

    const CLSID_FILE_OPEN_DIALOG: Guid = Guid {
        data1: 0xDC1C_5A9C,
        data2: 0xE88A,
        data3: 0x4DDE,
        data4: [0xA5, 0xA1, 0x60, 0xF8, 0x2A, 0x20, 0xAE, 0xF7],
    };

    const IID_IFILE_OPEN_DIALOG: Guid = Guid {
        data1: 0xD57C_7288,
        data2: 0xD4AD,
        data3: 0x4768,
        data4: [0xBE, 0x02, 0x9D, 0x96, 0x95, 0x32, 0xD9, 0x60],
    };

    const IID_ISHELL_ITEM: Guid = Guid {
        data1: 0x4382_6D1E,
        data2: 0xE718,
        data3: 0x42EE,
        data4: [0xBC, 0x55, 0xA1, 0xE2, 0x61, 0xC3, 0x7B, 0xFE],
    };

    #[repr(C)]
    struct ComDlgFilterSpec {
        psz_name: *const u16,
        psz_spec: *const u16,
    }

    #[repr(C)]
    #[allow(dead_code)]
    struct IUnknownVtbl {
        query_interface:
            unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> Hresult,
        add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
        release: unsafe extern "system" fn(*mut c_void) -> u32,
    }

    #[repr(C)]
    #[allow(dead_code)]
    struct IModalWindowVtbl {
        base: IUnknownVtbl,
        show: unsafe extern "system" fn(*mut c_void, isize) -> Hresult,
    }

    #[repr(C)]
    #[allow(dead_code)]
    struct IFileDialogVtbl {
        base: IModalWindowVtbl,
        set_file_types:
            unsafe extern "system" fn(*mut c_void, u32, *const ComDlgFilterSpec) -> Hresult,
        set_file_type_index: unsafe extern "system" fn(*mut c_void, u32) -> Hresult,
        get_file_type_index: unsafe extern "system" fn(*mut c_void, *mut u32) -> Hresult,
        advise: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut u32) -> Hresult,
        unadvise: unsafe extern "system" fn(*mut c_void, u32) -> Hresult,
        set_options: unsafe extern "system" fn(*mut c_void, u32) -> Hresult,
        get_options: unsafe extern "system" fn(*mut c_void, *mut u32) -> Hresult,
        set_default_folder: unsafe extern "system" fn(*mut c_void, *mut c_void) -> Hresult,
        set_folder: unsafe extern "system" fn(*mut c_void, *mut c_void) -> Hresult,
        get_folder: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> Hresult,
        get_current_selection: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> Hresult,
        set_file_name: unsafe extern "system" fn(*mut c_void, *const u16) -> Hresult,
        get_file_name: unsafe extern "system" fn(*mut c_void, *mut *mut u16) -> Hresult,
        set_title: unsafe extern "system" fn(*mut c_void, *const u16) -> Hresult,
        set_ok_button_label: unsafe extern "system" fn(*mut c_void, *const u16) -> Hresult,
        set_file_name_label: unsafe extern "system" fn(*mut c_void, *const u16) -> Hresult,
        get_result: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> Hresult,
        add_place: unsafe extern "system" fn(*mut c_void, *mut c_void, u32) -> Hresult,
        set_default_extension: unsafe extern "system" fn(*mut c_void, *const u16) -> Hresult,
        close: unsafe extern "system" fn(*mut c_void, Hresult) -> Hresult,
        set_client_guid: unsafe extern "system" fn(*mut c_void, *const Guid) -> Hresult,
        clear_client_data: unsafe extern "system" fn(*mut c_void) -> Hresult,
        set_filter: unsafe extern "system" fn(*mut c_void, *mut c_void) -> Hresult,
    }

    #[repr(C)]
    #[allow(dead_code)]
    struct IFileOpenDialogVtbl {
        base: IFileDialogVtbl,
        get_results: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> Hresult,
        get_selected_items: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> Hresult,
    }

    #[repr(C)]
    #[allow(dead_code)]
    struct IShellItemVtbl {
        base: IUnknownVtbl,
        bind_to_handler: unsafe extern "system" fn(
            *mut c_void,
            *mut c_void,
            *const Guid,
            *const Guid,
            *mut *mut c_void,
        ) -> Hresult,
        get_parent: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> Hresult,
        get_display_name: unsafe extern "system" fn(*mut c_void, u32, *mut *mut u16) -> Hresult,
        get_attributes: unsafe extern "system" fn(*mut c_void, u32, *mut u32) -> Hresult,
        compare: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, *mut i32) -> Hresult,
    }

    #[repr(C)]
    #[allow(dead_code)]
    struct IShellItemArrayVtbl {
        base: IUnknownVtbl,
        bind_to_handler: unsafe extern "system" fn(
            *mut c_void,
            *mut c_void,
            *const Guid,
            *const Guid,
            *mut *mut c_void,
        ) -> Hresult,
        get_property_store:
            unsafe extern "system" fn(*mut c_void, u32, *const Guid, *mut *mut c_void) -> Hresult,
        get_property_description_list: unsafe extern "system" fn(
            *mut c_void,
            *const c_void,
            *const Guid,
            *mut *mut c_void,
        ) -> Hresult,
        get_attributes: unsafe extern "system" fn(*mut c_void, u32, u32, *mut u32) -> Hresult,
        get_count: unsafe extern "system" fn(*mut c_void, *mut u32) -> Hresult,
        get_item_at: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> Hresult,
        enum_items: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> Hresult,
    }

    #[link(name = "ole32")]
    extern "system" {
        fn CoInitializeEx(pv_reserved: *const c_void, dw_co_init: u32) -> Hresult;
        fn CoUninitialize();
        fn CoCreateInstance(
            rclsid: *const Guid,
            punk_outer: *mut c_void,
            dw_cls_context: u32,
            riid: *const Guid,
            ppv: *mut *mut c_void,
        ) -> Hresult;
        fn CoTaskMemFree(pv: *mut c_void);
    }

    #[link(name = "shell32")]
    extern "system" {
        fn SHCreateItemFromParsingName(
            psz_path: *const u16,
            pbc: *mut c_void,
            riid: *const Guid,
            ppv: *mut *mut c_void,
        ) -> Hresult;
    }

    struct ComInitGuard {
        should_uninit: bool,
    }

    impl Drop for ComInitGuard {
        fn drop(&mut self) {
            if self.should_uninit {
                unsafe {
                    CoUninitialize();
                }
            }
        }
    }

    struct ComObject(*mut c_void);

    impl ComObject {
        fn as_ptr(&self) -> *mut c_void {
            self.0
        }
    }

    impl Drop for ComObject {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    let vtbl = *(self.0 as *const *const IUnknownVtbl);
                    ((*vtbl).release)(self.0);
                }
            }
        }
    }

    #[inline]
    fn succeeded(hr: Hresult) -> bool {
        hr >= 0
    }

    unsafe fn shell_item_to_path(item: *mut c_void) -> Option<PathBuf> {
        if item.is_null() {
            return None;
        }
        let vtbl = *(item as *const *const IShellItemVtbl);
        let mut wide_ptr: *mut u16 = std::ptr::null_mut();
        let hr = ((*vtbl).get_display_name)(item, SIGDN_FILESYSPATH, &mut wide_ptr);
        if !succeeded(hr) || wide_ptr.is_null() {
            return None;
        }
        let mut len = 0usize;
        while *wide_ptr.add(len) != 0 {
            len += 1;
        }
        let slice = std::slice::from_raw_parts(wide_ptr, len);
        let path = PathBuf::from(OsString::from_wide(slice));
        CoTaskMemFree(wide_ptr.cast::<c_void>());
        Some(path)
    }

    fn run_on_sta_thread(options: &FileDialogOptions) -> Vec<PathBuf> {
        let init_hr = unsafe {
            CoInitializeEx(
                std::ptr::null(),
                COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE,
            )
        };
        if init_hr == RPC_E_CHANGED_MODE {
            // Current thread is already MTA; hop to a fresh STA thread so COM dialog works.
            let owned = options.clone();
            return std::thread::Builder::new()
                .name("subomi-win32-file-dialog".to_owned())
                .spawn(move || run_on_sta_thread(&owned))
                .ok()
                .and_then(|handle| handle.join().ok())
                .unwrap_or_default();
        }
        if !succeeded(init_hr) {
            return Vec::new();
        }
        let _com_guard = ComInitGuard {
            should_uninit: init_hr == S_OK || init_hr == S_FALSE,
        };

        let mut raw_dialog: *mut c_void = std::ptr::null_mut();
        let hr = unsafe {
            CoCreateInstance(
                &CLSID_FILE_OPEN_DIALOG,
                std::ptr::null_mut(),
                CLSCTX_INPROC_SERVER,
                &IID_IFILE_OPEN_DIALOG,
                &mut raw_dialog,
            )
        };
        if !succeeded(hr) || raw_dialog.is_null() {
            return Vec::new();
        }
        let dialog = ComObject(raw_dialog);

        unsafe {
            let vtbl = *(dialog.as_ptr() as *const *const IFileOpenDialogVtbl);

            // Configure dialog options (folder vs file, multi-select, filesystem paths).
            let mut flags: u32 = 0;
            if succeeded(((*vtbl).base.get_options)(dialog.as_ptr(), &mut flags)) {
                flags |= FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST;
                if options.is_folder() {
                    flags |= FOS_PICKFOLDERS;
                } else {
                    flags |= FOS_FILEMUSTEXIST;
                }
                if options.multiple {
                    flags |= FOS_ALLOWMULTISELECT;
                }
                let _ = ((*vtbl).base.set_options)(dialog.as_ptr(), flags);
            }

            if !options.title.is_empty() {
                let title_w = encode_utf16_nul(&options.title);
                let _ = ((*vtbl).base.set_title)(dialog.as_ptr(), title_w.as_ptr());
            }

            // Keep wide filter strings alive across `SetFileTypes` and `Show`.
            let filter_buffers: Vec<(Vec<u16>, Vec<u16>)> = if options.is_folder() {
                Vec::new()
            } else {
                options
                    .filters()
                    .into_iter()
                    .map(|filter| {
                        let name_w = encode_utf16_nul(&filter.label);
                        let spec_w = encode_utf16_nul(&format_windows_filter_spec(&filter.patterns));
                        (name_w, spec_w)
                    })
                    .collect()
            };
            let filter_specs: Vec<ComDlgFilterSpec> = filter_buffers
                .iter()
                .map(|(name_w, spec_w)| ComDlgFilterSpec {
                    psz_name: name_w.as_ptr(),
                    psz_spec: spec_w.as_ptr(),
                })
                .collect();
            if !filter_specs.is_empty() {
                let _ = ((*vtbl).base.set_file_types)(
                    dialog.as_ptr(),
                    filter_specs.len() as u32,
                    filter_specs.as_ptr(),
                );
                let _ = ((*vtbl).base.set_file_type_index)(dialog.as_ptr(), 1);
            }

            if let Some(initial_dir) = options.initial_dir.as_deref() {
                let dir_w = encode_utf16_nul(&normalize_windows_dialog_dir(initial_dir));
                let mut folder_item: *mut c_void = std::ptr::null_mut();
                let item_hr = SHCreateItemFromParsingName(
                    dir_w.as_ptr(),
                    std::ptr::null_mut(),
                    &IID_ISHELL_ITEM,
                    &mut folder_item,
                );
                if succeeded(item_hr) && !folder_item.is_null() {
                    let folder_obj = ComObject(folder_item);
                    let _ = ((*vtbl).base.set_folder)(dialog.as_ptr(), folder_obj.as_ptr());
                }
            }

            let show_hr = ((*vtbl).base.base.show)(dialog.as_ptr(), 0);
            if !succeeded(show_hr) {
                // Cancelled (`HRESULT_FROM_WIN32(ERROR_CANCELLED)`) or dialog error.
                return Vec::new();
            }

            let mut results = Vec::new();
            if options.multiple {
                let mut raw_array: *mut c_void = std::ptr::null_mut();
                if succeeded(((*vtbl).get_results)(dialog.as_ptr(), &mut raw_array))
                    && !raw_array.is_null()
                {
                    let array = ComObject(raw_array);
                    let arr_vtbl = *(array.as_ptr() as *const *const IShellItemArrayVtbl);
                    let mut count: u32 = 0;
                    if succeeded(((*arr_vtbl).get_count)(array.as_ptr(), &mut count)) {
                        for idx in 0..count {
                            let mut raw_item: *mut c_void = std::ptr::null_mut();
                            if succeeded(((*arr_vtbl).get_item_at)(
                                array.as_ptr(),
                                idx,
                                &mut raw_item,
                            )) && !raw_item.is_null()
                            {
                                let item = ComObject(raw_item);
                                if let Some(path) = shell_item_to_path(item.as_ptr()) {
                                    results.push(path);
                                }
                            }
                        }
                    }
                }
            } else {
                let mut raw_item: *mut c_void = std::ptr::null_mut();
                if succeeded(((*vtbl).base.get_result)(dialog.as_ptr(), &mut raw_item))
                    && !raw_item.is_null()
                {
                    let item = ComObject(raw_item);
                    if let Some(path) = shell_item_to_path(item.as_ptr()) {
                        results.push(path);
                    }
                }
            }
            results
        }
    }

    pub(crate) fn open_files_windows(options: &FileDialogOptions) -> Vec<PathBuf> {
        run_on_sta_thread(options)
    }
}

#[cfg(target_os = "windows")]
pub(crate) use win_com_dialog::open_files_windows;

// ── macOS Native AppKit `NSOpenPanel` (Main-Thread Safe) ─────────────────────

#[cfg(target_os = "macos")]
mod macos_open_panel {
    use std::ffi::{c_char, c_void, CStr, CString};
    use std::path::PathBuf;

    use super::{extract_macos_allowed_extensions, has_all_files_wildcard};
    use crate::FileDialogOptions;

    type Id = *mut c_void;
    type Sel = *mut c_void;
    type Class = *mut c_void;
    type ObjcBool = i8;
    type NSInteger = isize;
    type NSUInteger = usize;

    const NO: ObjcBool = 0;
    const YES: ObjcBool = 1;
    const NS_MODAL_RESPONSE_OK: NSInteger = 1;

    #[link(name = "objc")]
    extern "C" {
        fn objc_getClass(name: *const c_char) -> Class;
        fn sel_registerName(name: *const c_char) -> Sel;
        fn objc_msgSend();
        fn objc_autoreleasePoolPush() -> *mut c_void;
        fn objc_autoreleasePoolPop(pool: *mut c_void);
    }

    #[link(name = "AppKit", kind = "framework")]
    extern "C" {}

    #[link(name = "Foundation", kind = "framework")]
    extern "C" {}

    extern "C" {
        static _dispatch_main_q: u8;
        fn dispatch_async_f(
            queue: *const c_void,
            context: *mut c_void,
            work: extern "C" fn(*mut c_void),
        );
        fn dispatch_sync_f(
            queue: *const c_void,
            context: *mut c_void,
            work: extern "C" fn(*mut c_void),
        );
    }

    struct AutoreleasePoolGuard(*mut c_void);

    impl AutoreleasePoolGuard {
        unsafe fn push() -> Self {
            Self(objc_autoreleasePoolPush())
        }
    }

    impl Drop for AutoreleasePoolGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    objc_autoreleasePoolPop(self.0);
                }
            }
        }
    }

    #[inline]
    unsafe fn cls(name_nul: &[u8]) -> Class {
        objc_getClass(name_nul.as_ptr().cast::<c_char>())
    }

    #[inline]
    unsafe fn sel(name_nul: &[u8]) -> Sel {
        sel_registerName(name_nul.as_ptr().cast::<c_char>())
    }

    // Never call `objc_msgSend` as a variadic function (UB on aarch64);
    // transmute to the exact non-variadic ABI signature for each selector.
    #[inline]
    unsafe fn msg_send_id(obj: Id, s: Sel) -> Id {
        let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc_msgSend as *const ());
        f(obj, s)
    }

    #[inline]
    unsafe fn msg_send_bool(obj: Id, s: Sel) -> ObjcBool {
        let f: unsafe extern "C" fn(Id, Sel) -> ObjcBool =
            std::mem::transmute(objc_msgSend as *const ());
        f(obj, s)
    }

    #[inline]
    unsafe fn msg_send_usize(obj: Id, s: Sel) -> NSUInteger {
        let f: unsafe extern "C" fn(Id, Sel) -> NSUInteger =
            std::mem::transmute(objc_msgSend as *const ());
        f(obj, s)
    }

    #[inline]
    unsafe fn msg_send_isize(obj: Id, s: Sel) -> NSInteger {
        let f: unsafe extern "C" fn(Id, Sel) -> NSInteger =
            std::mem::transmute(objc_msgSend as *const ());
        f(obj, s)
    }

    #[inline]
    unsafe fn msg_send_cstr(obj: Id, s: Sel) -> *const c_char {
        let f: unsafe extern "C" fn(Id, Sel) -> *const c_char =
            std::mem::transmute(objc_msgSend as *const ());
        f(obj, s)
    }

    #[inline]
    unsafe fn msg_send_void_id(obj: Id, s: Sel, arg: Id) {
        let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc_msgSend as *const ());
        f(obj, s, arg);
    }

    #[inline]
    unsafe fn msg_send_void_bool(obj: Id, s: Sel, arg: ObjcBool) {
        let f: unsafe extern "C" fn(Id, Sel, ObjcBool) =
            std::mem::transmute(objc_msgSend as *const ());
        f(obj, s, arg);
    }

    #[inline]
    unsafe fn msg_send_id_cstr(obj: Id, s: Sel, arg: *const c_char) -> Id {
        let f: unsafe extern "C" fn(Id, Sel, *const c_char) -> Id =
            std::mem::transmute(objc_msgSend as *const ());
        f(obj, s, arg)
    }

    #[inline]
    unsafe fn msg_send_id_usize(obj: Id, s: Sel, arg: NSUInteger) -> Id {
        let f: unsafe extern "C" fn(Id, Sel, NSUInteger) -> Id =
            std::mem::transmute(objc_msgSend as *const ());
        f(obj, s, arg)
    }

    #[inline]
    unsafe fn msg_send_id_id_bool(obj: Id, s: Sel, arg1: Id, arg2: ObjcBool) -> Id {
        let f: unsafe extern "C" fn(Id, Sel, Id, ObjcBool) -> Id =
            std::mem::transmute(objc_msgSend as *const ());
        f(obj, s, arg1, arg2)
    }

    #[inline]
    unsafe fn msg_send_id_ptr_usize(obj: Id, s: Sel, arg1: *const Id, arg2: NSUInteger) -> Id {
        let f: unsafe extern "C" fn(Id, Sel, *const Id, NSUInteger) -> Id =
            std::mem::transmute(objc_msgSend as *const ());
        f(obj, s, arg1, arg2)
    }

    unsafe fn ns_string_from_str(text: &str) -> Option<Id> {
        let cstr = CString::new(text).ok()?;
        let ns_string_cls = cls(b"NSString\0");
        if ns_string_cls.is_null() {
            return None;
        }
        let s = msg_send_id_cstr(
            ns_string_cls,
            sel(b"stringWithUTF8String:\0"),
            cstr.as_ptr(),
        );
        (!s.is_null()).then_some(s)
    }

    fn is_main_thread() -> bool {
        unsafe {
            let ns_thread_cls = cls(b"NSThread\0");
            if ns_thread_cls.is_null() {
                return false;
            }
            msg_send_bool(ns_thread_cls, sel(b"isMainThread\0")) != NO
        }
    }

    /// Execute `NSOpenPanel` directly on the current thread.
    ///
    /// # Safety / Threading
    /// Caller MUST be on the macOS main thread.
    fn run_open_panel_on_current_main_thread(options: &FileDialogOptions) -> Vec<PathBuf> {
        unsafe {
            let _pool = AutoreleasePoolGuard::push();

            let panel_cls = cls(b"NSOpenPanel\0");
            if panel_cls.is_null() {
                return Vec::new();
            }
            let panel = msg_send_id(panel_cls, sel(b"openPanel\0"));
            if panel.is_null() {
                return Vec::new();
            }

            if !options.title.is_empty() {
                if let Some(title_ns) = ns_string_from_str(&options.title) {
                    msg_send_void_id(panel, sel(b"setTitle:\0"), title_ns);
                }
            }

            let is_folder = options.is_folder();
            msg_send_void_bool(
                panel,
                sel(b"setCanChooseFiles:\0"),
                if is_folder { NO } else { YES },
            );
            msg_send_void_bool(
                panel,
                sel(b"setCanChooseDirectories:\0"),
                if is_folder { YES } else { NO },
            );
            msg_send_void_bool(
                panel,
                sel(b"setCanCreateDirectories:\0"),
                if is_folder { YES } else { NO },
            );
            msg_send_void_bool(
                panel,
                sel(b"setAllowsMultipleSelection:\0"),
                if options.multiple { YES } else { NO },
            );

            if let Some(dir) = options.initial_dir.as_deref() {
                let dir_str = dir.to_string_lossy();
                if let Some(dir_ns) = ns_string_from_str(&dir_str) {
                    let ns_url_cls = cls(b"NSURL\0");
                    if !ns_url_cls.is_null() {
                        let url = msg_send_id_id_bool(
                            ns_url_cls,
                            sel(b"fileURLWithPath:isDirectory:\0"),
                            dir_ns,
                            YES,
                        );
                        if !url.is_null() {
                            msg_send_void_id(panel, sel(b"setDirectoryURL:\0"), url);
                        }
                    }
                }
            }

            if !is_folder {
                let exts = extract_macos_allowed_extensions(options);
                if !exts.is_empty() {
                    let ns_exts: Vec<Id> = exts
                        .iter()
                        .filter_map(|ext| ns_string_from_str(ext))
                        .collect();
                    if !ns_exts.is_empty() {
                        let ns_array_cls = cls(b"NSArray\0");
                        if !ns_array_cls.is_null() {
                            let arr = msg_send_id_ptr_usize(
                                ns_array_cls,
                                sel(b"arrayWithObjects:count:\0"),
                                ns_exts.as_ptr(),
                                ns_exts.len(),
                            );
                            if !arr.is_null() {
                                msg_send_void_id(panel, sel(b"setAllowedFileTypes:\0"), arr);
                            }
                        }
                    }
                }
                if has_all_files_wildcard(options) {
                    msg_send_void_bool(panel, sel(b"setAllowsOtherFileTypes:\0"), YES);
                }
            }

            let response = msg_send_isize(panel, sel(b"runModal\0"));
            if response != NS_MODAL_RESPONSE_OK {
                return Vec::new();
            }

            let urls = msg_send_id(panel, sel(b"URLs\0"));
            if urls.is_null() {
                return Vec::new();
            }
            let count = msg_send_usize(urls, sel(b"count\0"));
            let mut paths = Vec::with_capacity(count);
            let sel_obj_at = sel(b"objectAtIndex:\0");
            let sel_path = sel(b"path\0");
            let sel_utf8 = sel(b"UTF8String\0");
            for idx in 0..count {
                let url = msg_send_id_usize(urls, sel_obj_at, idx);
                if url.is_null() {
                    continue;
                }
                let path_ns = msg_send_id(url, sel_path);
                if path_ns.is_null() {
                    continue;
                }
                let cstr = msg_send_cstr(path_ns, sel_utf8);
                if cstr.is_null() {
                    continue;
                }
                let path_str = CStr::from_ptr(cstr).to_string_lossy().into_owned();
                if !path_str.is_empty() {
                    paths.push(PathBuf::from(path_str));
                }
            }
            paths
        }
    }

    struct SyncDialogContext {
        options: FileDialogOptions,
        result: Vec<PathBuf>,
    }

    extern "C" fn sync_dialog_trampoline(ctx: *mut c_void) {
        let state = unsafe { &mut *(ctx as *mut SyncDialogContext) };
        state.result = run_open_panel_on_current_main_thread(&state.options);
    }

    /// Blocking macOS `NSOpenPanel` dialog guaranteed to execute on the main thread.
    pub(crate) fn open_files_macos_blocking(options: &FileDialogOptions) -> Vec<PathBuf> {
        if is_main_thread() {
            return run_open_panel_on_current_main_thread(options);
        }
        let mut ctx = SyncDialogContext {
            options: options.clone(),
            result: Vec::new(),
        };
        unsafe {
            let main_queue = (&_dispatch_main_q as *const u8).cast::<c_void>();
            dispatch_sync_f(
                main_queue,
                (&mut ctx as *mut SyncDialogContext).cast::<c_void>(),
                sync_dialog_trampoline,
            );
        }
        ctx.result
    }

    struct AsyncDialogContext {
        options: FileDialogOptions,
        callback: Box<dyn FnOnce(Vec<PathBuf>) + Send + 'static>,
    }

    extern "C" fn async_dialog_trampoline(ctx: *mut c_void) {
        let boxed = unsafe { Box::from_raw(ctx as *mut AsyncDialogContext) };
        let paths = run_open_panel_on_current_main_thread(&boxed.options);
        if !paths.is_empty() {
            (boxed.callback)(paths);
        }
    }

    /// Non-blocking macOS `NSOpenPanel` dialog dispatched onto the main GCD queue
    /// (NEVER a worker thread).
    pub(crate) fn open_files_macos_async(
        options: FileDialogOptions,
        callback: impl FnOnce(Vec<PathBuf>) + Send + 'static,
    ) {
        let ctx = Box::new(AsyncDialogContext {
            options,
            callback: Box::new(callback),
        });
        unsafe {
            let main_queue = (&_dispatch_main_q as *const u8).cast::<c_void>();
            dispatch_async_f(
                main_queue,
                Box::into_raw(ctx).cast::<c_void>(),
                async_dialog_trampoline,
            );
        }
    }
}

#[cfg(target_os = "macos")]
pub(crate) use macos_open_panel::{open_files_macos_async, open_files_macos_blocking};

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn windows_filter_spec_formats_semicolon_patterns_and_wildcards() {
        assert_eq!(
            format_windows_filter_spec(&["*.mkv".to_owned(), "*.srt".to_owned()]),
            "*.mkv;*.srt"
        );
        assert_eq!(format_windows_filter_spec(&["*".to_owned()]), "*.*");
        assert_eq!(format_windows_filter_spec(&[]), "*.*");
        assert_eq!(
            normalize_windows_dialog_dir(Path::new("C:/Users/Test/Videos")),
            r"C:\Users\Test\Videos"
        );
        assert_eq!(encode_utf16_nul("AB"), vec![65u16, 66u16, 0u16]);
    }

    #[test]
    fn macos_allowed_extensions_extracts_unique_lowercase_extensions_and_skips_folders() {
        let folder = FileDialogOptions::folder(Some(PathBuf::from("/tmp")));
        assert!(extract_macos_allowed_extensions(&folder).is_empty());
        assert!(!has_all_files_wildcard(&folder));

        let zip = FileDialogOptions::archive_zip(None);
        assert_eq!(extract_macos_allowed_extensions(&zip), vec!["zip"]);
        assert!(has_all_files_wildcard(&zip));

        let subs = FileDialogOptions::subtitles_only(None);
        let sub_exts = extract_macos_allowed_extensions(&subs);
        for required in [
            "ass", "ssa", "srt", "vtt", "sub", "idx", "sup", "lrc", "mks", "ttml", "smi", "rt",
        ] {
            assert!(
                sub_exts.iter().any(|e| e == required),
                "missing subtitle extension `{required}` in {sub_exts:?}"
            );
        }
        // No duplicates and no wildcards in macOS allowedFileTypes.
        let unique: HashSet<&str> = sub_exts.iter().map(String::as_str).collect();
        assert_eq!(unique.len(), sub_exts.len());
        assert!(!sub_exts.iter().any(|e| e.contains('*')));
    }
}
