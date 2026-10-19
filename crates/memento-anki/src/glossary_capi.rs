//! C ABI for glossary building (`src/anki/glossarybuilder.cpp`).
//! The borrowed JSON input tree is converted to Rust JsonValue, processed,
//! and output structures are Rust-owned boxed slices freed by
//! `memento_anki_free_glossary`.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;

use crate::glossary::build_glossary;
use crate::json::{insert_qjson, JsonObject, JsonValue};

const NULL: u32 = 0;
const BOOL: u32 = 1;
const NUMBER: u32 = 2;
const STRING: u32 = 3;
const ARRAY: u32 = 4;
const OBJECT: u32 = 5;
const UNDEFINED: u32 = 6;

#[repr(C)]
pub struct MementoAnkiGlossaryTextViewC
{
    pub data: *const u16,
    pub len: u64,
}

#[repr(C)]
pub struct MementoAnkiGlossaryEntryInC
{
    pub key: MementoAnkiGlossaryTextViewC,
    pub value: *const MementoAnkiGlossaryJsonInC,
}

#[repr(C)]
pub struct MementoAnkiGlossaryJsonInC
{
    pub kind: u32,
    pub boolean: bool,
    pub number: f64,
    pub text: MementoAnkiGlossaryTextViewC,
    pub items: *const MementoAnkiGlossaryJsonInC,
    pub items_len: u64,
    pub entries: *const MementoAnkiGlossaryEntryInC,
    pub entries_len: u64,
}

#[repr(C)]
pub struct MementoAnkiGlossaryStringC
{
    pub data: *mut u16,
    pub len: u64,
}

#[repr(C)]
pub struct MementoAnkiGlossaryFileInfoC
{
    pub path: *mut u16,
    pub path_len: u64,
    pub name: *mut u16,
    pub name_len: u64,
}

#[repr(C)]
pub struct MementoAnkiGlossaryOutputC
{
    pub entries: *mut MementoAnkiGlossaryStringC,
    pub entries_len: u64,
    pub files: *mut MementoAnkiGlossaryFileInfoC,
    pub files_len: u64,
}

pub type MementoAnkiGlossaryMd5Fn = unsafe extern "C" fn(
    full_path: *const u16,
    full_path_len: u64,
    out_hex32: *mut u16,
    user_data: *mut std::ffi::c_void,
) -> bool;

unsafe fn drop_boxed_slice<T>(data: *mut T, len: u64)
{
    if !data.is_null()
    {
        drop(unsafe { Box::from_raw(ptr::slice_from_raw_parts_mut(data, len as usize)) });
    }
}

fn own_slice<T>(items: Vec<T>) -> (*mut T, u64)
{
    if items.is_empty()
    {
        return (ptr::null_mut(), 0);
    }
    let boxed = items.into_boxed_slice();
    let len = boxed.len() as u64;
    (Box::into_raw(boxed) as *mut T, len)
}

unsafe fn view<'a, T>(data: *const T, len: u64) -> Option<&'a [T]>
{
    let length = usize::try_from(len).ok()?;
    if length > isize::MAX as usize / std::mem::size_of::<T>()
    {
        return None;
    }
    if length == 0
    {
        Some(&[])
    }
    else if data.is_null()
    {
        None
    }
    else
    {
        Some(unsafe { std::slice::from_raw_parts(data, length) })
    }
}

unsafe fn text(value: &MementoAnkiGlossaryTextViewC) -> Option<Vec<u16>>
{
    Some(unsafe { view(value.data, value.len)? }.to_vec())
}

unsafe fn json(value: &MementoAnkiGlossaryJsonInC) -> Option<JsonValue>
{
    match value.kind
    {
        UNDEFINED => Some(JsonValue::Undefined),
        NULL => Some(JsonValue::Null),
        BOOL => Some(JsonValue::Bool(value.boolean)),
        NUMBER => Some(JsonValue::from_qjson_number(value.number)),
        STRING => Some(JsonValue::String(unsafe { text(&value.text)? })),
        ARRAY =>
        {
            let items = unsafe { view(value.items, value.items_len)? };
            let mut array = Vec::with_capacity(items.len());
            for item in items
            {
                let parsed = unsafe { json(item)? };
                array.push(if matches!(parsed, JsonValue::Undefined) {
                    JsonValue::Null
                } else {
                    parsed
                });
            }
            Some(JsonValue::Array(array))
        }
        OBJECT =>
        {
            let entries = unsafe { view(value.entries, value.entries_len)? };
            let mut object = JsonObject::new();
            for entry in entries
            {
                let key = unsafe { text(&entry.key)? };
                let item = unsafe { entry.value.as_ref()? };
                insert_qjson(&mut object, key, unsafe { json(item)? });
            }
            Some(JsonValue::Object(object))
        }
        _ => None,
    }
}

/// # Safety
/// `definitions` and `basepath` must point to valid initialized input buffers
/// for the duration of this call. `out` must be writable and non-null.
/// On return, `out` contains Rust-owned buffers that MUST be released with
/// `memento_anki_free_glossary`.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_build_glossary(
    definitions: *const MementoAnkiGlossaryJsonInC,
    definitions_len: u64,
    basepath: MementoAnkiGlossaryTextViewC,
    md5_fn: Option<MementoAnkiGlossaryMd5Fn>,
    user_data: *mut std::ffi::c_void,
    out: *mut MementoAnkiGlossaryOutputC,
) -> bool
{
    if out.is_null()
    {
        return false;
    }
    unsafe {
        out.write(MementoAnkiGlossaryOutputC {
            entries: ptr::null_mut(),
            entries_len: 0,
            files: ptr::null_mut(),
            files_len: 0,
        });
    }

    let Ok(Some(result)) = catch_unwind(AssertUnwindSafe(|| {
        let def_slice = unsafe { view(definitions, definitions_len)? };
        let mut defs = Vec::with_capacity(def_slice.len());
        for d in def_slice
        {
            defs.push(unsafe { json(d)? });
        }
        let basepath_vec = unsafe { text(&basepath)? };

        let mut md5_adapter = |full_path: &[u16]| -> Option<[u16; 32]> {
            let callback = md5_fn?;
            let mut buf = [0u16; 32];
            let ok = unsafe {
                callback(
                    full_path.as_ptr(),
                    full_path.len() as u64,
                    buf.as_mut_ptr(),
                    user_data,
                )
            };
            if ok {
                Some(buf)
            } else {
                None
            }
        };

        let (glossaries, file_entries) = build_glossary(&defs, &basepath_vec, &mut md5_adapter);

        let out_entries: Vec<MementoAnkiGlossaryStringC> = glossaries
            .into_iter()
            .map(|g| {
                let (data, len) = own_slice(g);
                MementoAnkiGlossaryStringC { data, len }
            })
            .collect();

        let out_files: Vec<MementoAnkiGlossaryFileInfoC> = file_entries
            .into_iter()
            .map(|(p, n)| {
                let (p_data, p_len) = own_slice(p);
                let (n_data, n_len) = own_slice(n);
                MementoAnkiGlossaryFileInfoC {
                    path: p_data,
                    path_len: p_len,
                    name: n_data,
                    name_len: n_len,
                }
            })
            .collect();

        let (entries_ptr, entries_len) = own_slice(out_entries);
        let (files_ptr, files_len) = own_slice(out_files);
        Some(MementoAnkiGlossaryOutputC {
            entries: entries_ptr,
            entries_len,
            files: files_ptr,
            files_len,
        })
    })) else {
        return false;
    };

    unsafe { out.write(result) };
    true
}

/// # Safety
/// `out` is null or points to a writable `MementoAnkiGlossaryOutputC` produced
/// by `memento_anki_build_glossary`. Resets `out` to empty state.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_free_glossary(out: *mut MementoAnkiGlossaryOutputC)
{
    if out.is_null()
    {
        return;
    }
    let val = unsafe {
        std::mem::replace(
            &mut *out,
            MementoAnkiGlossaryOutputC {
                entries: ptr::null_mut(),
                entries_len: 0,
                files: ptr::null_mut(),
                files_len: 0,
            },
        )
    };
    if !val.entries.is_null()
    {
        let entries_slice = unsafe {
            Box::from_raw(std::slice::from_raw_parts_mut(
                val.entries,
                val.entries_len as usize,
            ))
        };
        for entry in entries_slice
        {
            unsafe { drop_boxed_slice(entry.data, entry.len) };
        }
    }
    if !val.files.is_null()
    {
        let files_slice = unsafe {
            Box::from_raw(std::slice::from_raw_parts_mut(
                val.files,
                val.files_len as usize,
            ))
        };
        for file in files_slice
        {
            unsafe {
                drop_boxed_slice(file.path, file.path_len);
                drop_boxed_slice(file.name, file.name_len);
            }
        }
    }
}

#[cfg(test)]
mod tests
{
    use super::*;

    fn u(text: &str) -> Vec<u16>
    {
        text.encode_utf16().collect()
    }

    fn text_view(s: &[u16]) -> MementoAnkiGlossaryTextViewC
    {
        MementoAnkiGlossaryTextViewC {
            data: s.as_ptr(),
            len: s.len() as u64,
        }
    }

    unsafe extern "C" fn test_md5(
        full_path: *const u16,
        full_path_len: u64,
        out_hex32: *mut u16,
        _user_data: *mut std::ffi::c_void,
    ) -> bool
    {
        let path = std::slice::from_raw_parts(full_path, full_path_len as usize);
        let path_str = String::from_utf16_lossy(path);
        if path_str.ends_with("test.png")
        {
            let hash = "11223344556677889900aabbccddeeff";
            for (i, b) in hash.encode_utf16().enumerate()
            {
                *out_hex32.add(i) = b;
            }
            true
        }
        else
        {
            false
        }
    }

    #[test]
    fn c_abi_build_glossary_and_free()
    {
        let str_val = u("<b>hello</b>\nworld");
        let def1 = MementoAnkiGlossaryJsonInC {
            kind: STRING,
            boolean: false,
            number: 0.0,
            text: text_view(&str_val),
            items: ptr::null(),
            items_len: 0,
            entries: ptr::null(),
            entries_len: 0,
        };

        let base = u("/media");
        let defs = [def1];
        let mut out = MementoAnkiGlossaryOutputC {
            entries: ptr::null_mut(),
            entries_len: 0,
            files: ptr::null_mut(),
            files_len: 0,
        };

        assert!(unsafe {
            memento_anki_build_glossary(
                defs.as_ptr(),
                defs.len() as u64,
                text_view(&base),
                Some(test_md5),
                ptr::null_mut(),
                &mut out,
            )
        });

        assert_eq!(out.entries_len, 1);
        let entry = unsafe { &*out.entries };
        let entry_slice = unsafe { std::slice::from_raw_parts(entry.data, entry.len as usize) };
        assert_eq!(
            String::from_utf16_lossy(entry_slice),
            "&lt;b&gt;hello&lt;/b&gt;<br>world"
        );
        assert_eq!(out.files_len, 0);

        unsafe { memento_anki_free_glossary(&mut out) };
        assert!(out.entries.is_null());
        assert_eq!(out.entries_len, 0);
        // Double free safe
        unsafe { memento_anki_free_glossary(&mut out) };
    }
}
