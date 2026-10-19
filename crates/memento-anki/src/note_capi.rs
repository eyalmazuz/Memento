//! In-memory UTF-16 note metadata ABI used for compiled-original Qt parity.
//! Input JSON is borrowed for the duration of one call; the complete output
//! tree is Rust-owned and freed recursively by the paired free function.

use std::panic::catch_unwind;
use std::ptr;

use crate::json::{insert_qjson, JsonObject, JsonValue};
use crate::note::{build_note_metadata, DuplicatePolicy};

const NULL: u32 = 0;
const BOOL: u32 = 1;
const NUMBER: u32 = 2;
const STRING: u32 = 3;
const ARRAY: u32 = 4;
const OBJECT: u32 = 5;
const UNDEFINED: u32 = 6;

#[repr(C)]
pub struct MementoAnkiNoteTextViewC
{
    pub data: *const u16,
    pub len: u64,
}

#[repr(C)]
pub struct MementoAnkiNoteEntryInC
{
    pub key: MementoAnkiNoteTextViewC,
    pub value: *const MementoAnkiNoteJsonInC,
}

#[repr(C)]
pub struct MementoAnkiNoteJsonInC
{
    pub kind: u32,
    pub boolean: bool,
    pub number: f64,
    pub text: MementoAnkiNoteTextViewC,
    pub items: *const MementoAnkiNoteJsonInC,
    pub items_len: u64,
    pub entries: *const MementoAnkiNoteEntryInC,
    pub entries_len: u64,
}

#[repr(C)]
pub struct MementoAnkiNoteInputC
{
    pub deck: MementoAnkiNoteTextViewC,
    pub model: MementoAnkiNoteTextViewC,
    pub tags: *const MementoAnkiNoteTextViewC,
    pub tags_len: u64,
    pub fields: MementoAnkiNoteJsonInC,
    pub duplicate_policy: u32,
}

#[repr(C)]
pub struct MementoAnkiFreqEntryC
{
    pub dictionary: MementoAnkiNoteTextViewC,
    pub freq: MementoAnkiNoteTextViewC,
}

#[repr(C)]
pub struct MementoAnkiFuriganaPairC
{
    pub surface: MementoAnkiNoteTextViewC,
    pub reading: MementoAnkiNoteTextViewC,
}

#[repr(C)]
pub struct MementoAnkiPitchEntryC
{
    pub dictionary_name: MementoAnkiNoteTextViewC,
    pub positions: *const u8,
    pub positions_len: u64,
    pub mora: *const MementoAnkiNoteTextViewC,
    pub mora_len: u64,
}

#[repr(C)]
pub struct MementoAnkiTagEntryC
{
    pub name: MementoAnkiNoteTextViewC,
    pub notes: MementoAnkiNoteTextViewC,
}

#[repr(C)]
pub struct MementoAnkiKanjiDefC
{
    pub dictionary_name: MementoAnkiNoteTextViewC,
    pub tags: *const MementoAnkiNoteTextViewC,
    pub tags_len: u64,
    pub glossary: *const MementoAnkiNoteTextViewC,
    pub glossary_len: u64,
}

#[repr(C)]
pub struct MementoAnkiPitchGraphEntryC
{
    pub dictionary_name: MementoAnkiNoteTextViewC,
    pub positions_count: u64,
    pub svg_graphs: *const MementoAnkiNoteTextViewC,
    pub svg_graphs_len: u64,
}

#[repr(C)]
pub struct MementoAnkiPitchCategoryInputC
{
    pub mora_len: u64,
    pub positions: *const u8,
    pub positions_len: u64,
}

#[repr(C)]
pub struct MementoAnkiStrokeCountStatC
{
    pub notes: MementoAnkiNoteTextViewC,
    pub value: MementoAnkiNoteTextViewC,
}

#[repr(C)]
pub struct MementoAnkiTermGlossaryDefC
{
    pub dict_id: i64,
    pub dict_name: MementoAnkiNoteTextViewC,
    pub tags: *const MementoAnkiNoteTextViewC,
    pub tags_len: u64,
    pub items: *const MementoAnkiNoteTextViewC,
    pub items_len: u64,
    pub has_styles: bool,
    pub stylesheet: MementoAnkiNoteTextViewC,
}

#[repr(C)]
pub struct MementoAnkiTermGlossaryOutputC
{
    pub glossary: *mut u16,
    pub glossary_len: u64,
    pub glossary_brief: *mut u16,
    pub glossary_brief_len: u64,
    pub glossary_compact: *mut u16,
    pub glossary_compact_len: u64,
}

#[repr(C)]
pub struct MementoAnkiNoteEntryC
{
    pub key: *mut u16,
    pub key_len: u64,
    pub value: MementoAnkiNoteJsonC,
}

#[repr(C)]
pub struct MementoAnkiNoteJsonC
{
    pub kind: u32,
    pub boolean: bool,
    pub number: f64,
    pub text: *mut u16,
    pub text_len: u64,
    pub items: *mut MementoAnkiNoteJsonC,
    pub items_len: u64,
    pub entries: *mut MementoAnkiNoteEntryC,
    pub entries_len: u64,
}

// All non-null output pointers own Box<[T]> allocations (never Vec capacities).
// Dropping each element automatically releases its nested allocations.
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

impl Drop for MementoAnkiNoteJsonC
{
    fn drop(&mut self)
    {
        unsafe
        {
            drop_boxed_slice(self.text, self.text_len);
            drop_boxed_slice(self.items, self.items_len);
            drop_boxed_slice(self.entries, self.entries_len);
        }
    }
}

impl Drop for MementoAnkiNoteEntryC
{
    fn drop(&mut self)
    {
        unsafe { drop_boxed_slice(self.key, self.key_len) };
        // Rust drops `value` recursively after this method returns.
    }
}

impl MementoAnkiNoteJsonC
{
    fn null() -> Self
    {
        Self {
            kind: NULL, boolean: false, number: 0.0,
            text: ptr::null_mut(), text_len: 0,
            items: ptr::null_mut(), items_len: 0,
            entries: ptr::null_mut(), entries_len: 0,
        }
    }

    fn from_json(value: JsonValue) -> Self
    {
        let mut output = Self::null();
        match value
        {
            JsonValue::Undefined =>
            {
                output.kind = UNDEFINED;
            },
            JsonValue::Null => {},
            JsonValue::Bool(value) =>
            {
                output.kind = BOOL;
                output.boolean = value;
            },
            JsonValue::Number(value) =>
            {
                output.kind = NUMBER;
                output.number = value;
            },
            JsonValue::String(text) =>
            {
                output.kind = STRING;
                (output.text, output.text_len) = own_slice(text);
            },
            JsonValue::Array(items) =>
            {
                output.kind = ARRAY;
                (output.items, output.items_len) = own_slice(
                    items.into_iter().map(Self::from_json).collect(),
                );
            },
            JsonValue::Object(entries) =>
            {
                output.kind = OBJECT;
                (output.entries, output.entries_len) = own_slice(
                    entries.into_iter().map(|(key, value)| {
                        let value = Self::from_json(value);
                        let (key, key_len) = own_slice(key);
                        MementoAnkiNoteEntryC { key, key_len, value }
                    }).collect(),
                );
            },
        }
        output
    }
}

// SAFETY: caller guarantees any nonempty pointer spans its given number of
// initialized T elements; after the length/null checks from_raw_parts is safe.
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

// SAFETY: view's buffer remains valid for the call, as specified by the ABI.
unsafe fn text(value: &MementoAnkiNoteTextViewC) -> Option<Vec<u16>>
{
    Some(unsafe { view(value.data, value.len)? }.to_vec())
}

// SAFETY: all reachable input arrays are acyclic and remain readable.
unsafe fn json(value: &MementoAnkiNoteJsonInC) -> Option<JsonValue>
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
        },
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
        },
        _ => None,
    }
}

/// # Safety
/// `input` must point to a readable, acyclic input tree and `out` must be
/// writable and not alias input. Input pointers remain valid for the call.
/// Release successful output with `memento_anki_free_note`.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_build_note(
    input: *const MementoAnkiNoteInputC,
    out: *mut MementoAnkiNoteJsonC,
) -> bool
{
    if out.is_null()
    {
        return false;
    }
    unsafe { out.write(MementoAnkiNoteJsonC::null()) };
    let Some(input) = (unsafe { input.as_ref() }) else { return false };
    let Ok(Some(note)) = catch_unwind(|| {
        let deck = unsafe { text(&input.deck)? };
        let model = unsafe { text(&input.model)? };
        let tag_views = unsafe { view(input.tags, input.tags_len)? };
        let mut tags = Vec::with_capacity(tag_views.len());
        for tag in tag_views
        {
            tags.push(unsafe { text(tag)? });
        }
        let fields = unsafe { json(&input.fields)? };
        let policy = match input.duplicate_policy
        {
            0 => DuplicatePolicy::None,
            1 => DuplicatePolicy::DifferentDeck,
            2 => DuplicatePolicy::SameDeck,
            _ => return None,
        };
        Some(MementoAnkiNoteJsonC::from_json(JsonValue::Object(
            build_note_metadata(deck, model, tags, fields, policy),
        )))
    }) else { return false };
    unsafe { out.write(note) };
    true
}

/// # Safety
/// `out` is null or is the writable original output of a successful
/// `memento_anki_build_note`, not an input tree or a copied output value.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_free_note(out: *mut MementoAnkiNoteJsonC)
{
    if out.is_null() { return; }
    drop(std::mem::replace(unsafe { &mut *out }, MementoAnkiNoteJsonC::null()));
}

/// # Safety
/// `input` and `replacer` must point to readable UTF-16 code units if their length > 0.
/// `out` and `out_len` must be writable and non-null.
/// Releases the output buffer with `memento_anki_free_u16`.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_replace_newlines(
    input: *const u16,
    input_len: u64,
    replacer: *const u16,
    replacer_len: u64,
    out: *mut *mut u16,
    out_len: *mut u64,
) -> bool
{
    if out.is_null() || out_len.is_null()
    {
        return false;
    }
    unsafe {
        *out = ptr::null_mut();
        *out_len = 0;
    }
    let Ok(Some((ptr, len))) = catch_unwind(|| {
        let input_slice = unsafe { view(input, input_len)? };
        let replacer_slice = unsafe { view(replacer, replacer_len)? };
        let result = crate::note::replace_newlines(input_slice, replacer_slice);
        Some(own_slice(result))
    }) else {
        return false;
    };
    unsafe {
        *out = ptr;
        *out_len = len;
    }
    true
}

/// # Safety
/// `ptr` must be null or allocated by `memento_anki_replace_newlines` with matching `len`.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_free_u16(ptr: *mut u16, len: u64)
{
    if !ptr.is_null() && len > 0
    {
        unsafe { drop_boxed_slice(ptr, len) };
    }
}

/// # Safety
/// `freq` must point to readable UTF-16 code units if `freq_len` > 0.
/// `out_value` must be writable and non-null.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_parse_frequency_value(
    freq: *const u16,
    freq_len: u64,
    out_value: *mut f64,
) -> bool
{
    if out_value.is_null()
    {
        return false;
    }
    unsafe { *out_value = 0.0 };
    let Ok(Some(val)) = catch_unwind(|| {
        let slice = unsafe { view(freq, freq_len)? };
        crate::note::parse_frequency_value(slice)
    }) else {
        return false;
    };
    unsafe { *out_value = val };
    true
}

/// # Safety
/// `entries` must point to readable `MementoAnkiFreqEntryC` elements if `entries_len` > 0.
/// Non-null out pointers must be writable. Release `out_numbers` with `memento_anki_free_f64`.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_frequency_stats(
    entries: *const MementoAnkiFreqEntryC,
    entries_len: u64,
    out_numbers: *mut *mut f64,
    out_numbers_len: *mut u64,
    out_harmonic: *mut i64,
    has_harmonic: *mut bool,
    out_average: *mut i64,
    has_average: *mut bool,
) -> bool
{
    if (!out_numbers.is_null() && out_numbers_len.is_null())
        || (out_numbers.is_null() && !out_numbers_len.is_null())
    {
        return false;
    }
    if !out_numbers.is_null()
    {
        unsafe { *out_numbers = ptr::null_mut() };
    }
    if !out_numbers_len.is_null()
    {
        unsafe { *out_numbers_len = 0 };
    }
    if !has_harmonic.is_null()
    {
        unsafe { *has_harmonic = false };
    }
    if !out_harmonic.is_null()
    {
        unsafe { *out_harmonic = 0 };
    }
    if !has_average.is_null()
    {
        unsafe { *has_average = false };
    }
    if !out_average.is_null()
    {
        unsafe { *out_average = 0 };
    }

    let Ok(Some((numbers, harmonic, average))) = catch_unwind(|| {
        let entries_slice = unsafe { view(entries, entries_len)? };
        let mut rust_entries = Vec::with_capacity(entries_slice.len());
        for entry in entries_slice
        {
            let dict = unsafe { view(entry.dictionary.data, entry.dictionary.len)? };
            let freq = unsafe { view(entry.freq.data, entry.freq.len)? };
            rust_entries.push(crate::note::FrequencyEntry { dictionary: dict, freq });
        }
        Some(crate::note::frequency_stats(&rust_entries))
    }) else {
        return false;
    };

    if !has_harmonic.is_null()
    {
        if let Some(h) = harmonic
        {
            unsafe {
                *has_harmonic = true;
                if !out_harmonic.is_null()
                {
                    *out_harmonic = h;
                }
            }
        }
    }

    if !has_average.is_null()
    {
        if let Some(a) = average
        {
            unsafe {
                *has_average = true;
                if !out_average.is_null()
                {
                    *out_average = a;
                }
            }
        }
    }

    if !out_numbers.is_null() && !out_numbers_len.is_null()
    {
        let (ptr, len) = own_slice(numbers);
        unsafe {
            *out_numbers = ptr;
            *out_numbers_len = len;
        }
    }

    true
}

/// # Safety
/// `ptr` must be null or allocated by `memento_anki_frequency_stats` with matching `len`.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_free_f64(ptr: *mut f64, len: u64)
{
    if !ptr.is_null() && len > 0
    {
        unsafe { drop_boxed_slice(ptr, len) };
    }
}

/// # Safety
/// `rules` must point to readable `MementoAnkiNoteTextViewC` elements if `rules_len` > 0.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_is_kifuku_applicable(
    rules: *const MementoAnkiNoteTextViewC,
    rules_len: u64,
) -> bool
{
    let Ok(Some(result)) = catch_unwind(|| {
        let rule_views = unsafe { view(rules, rules_len)? };
        let mut rust_rules = Vec::with_capacity(rule_views.len());
        for r in rule_views
        {
            rust_rules.push(unsafe { view(r.data, r.len)? });
        }
        Some(crate::note::is_kifuku_applicable(&rust_rules))
    }) else {
        return false;
    };
    result
}

/// # Safety
/// `pairs` must point to readable `MementoAnkiFuriganaPairC` elements if `pairs_len` > 0.
/// `out` and `out_len` must be writable and non-null. Release `out` with `memento_anki_free_u16`.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_format_furigana(
    pairs: *const MementoAnkiFuriganaPairC,
    pairs_len: u64,
    plain: bool,
    out: *mut *mut u16,
    out_len: *mut u64,
) -> bool
{
    if out.is_null() || out_len.is_null()
    {
        return false;
    }
    unsafe {
        *out = ptr::null_mut();
        *out_len = 0;
    }
    let Ok(Some((ptr, len))) = catch_unwind(|| {
        let pair_views = unsafe { view(pairs, pairs_len)? };
        let mut rust_pairs = Vec::with_capacity(pair_views.len());
        for p in pair_views
        {
            let surface = unsafe { view(p.surface.data, p.surface.len)? };
            let reading = unsafe { view(p.reading.data, p.reading.len)? };
            rust_pairs.push(crate::note::FuriganaPair { surface, reading });
        }
        let result = crate::note::format_furigana(&rust_pairs, plain);
        Some(own_slice(result))
    }) else {
        return false;
    };
    unsafe {
        *out = ptr;
        *out_len = len;
    }
    true
}

/// # Safety
/// `entries` must point to readable `MementoAnkiFreqEntryC` elements if `entries_len` > 0.
/// `out` and `out_len` must be writable and non-null. Release `out` with `memento_anki_free_u16`.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_build_frequencies_html(
    entries: *const MementoAnkiFreqEntryC,
    entries_len: u64,
    value_only: bool,
    min_value_only: bool,
    out: *mut *mut u16,
    out_len: *mut u64,
) -> bool
{
    if out.is_null() || out_len.is_null()
    {
        return false;
    }
    unsafe {
        *out = ptr::null_mut();
        *out_len = 0;
    }
    let Ok(Some((ptr, len))) = catch_unwind(|| {
        let entries_slice = unsafe { view(entries, entries_len)? };
        let mut rust_entries = Vec::with_capacity(entries_slice.len());
        for entry in entries_slice
        {
            let dict = unsafe { view(entry.dictionary.data, entry.dictionary.len)? };
            let freq = unsafe { view(entry.freq.data, entry.freq.len)? };
            rust_entries.push(crate::note::FrequencyEntry { dictionary: dict, freq });
        }
        let result = crate::note::build_frequencies_html(&rust_entries, value_only, min_value_only);
        Some(own_slice(result))
    }) else {
        return false;
    };
    unsafe {
        *out = ptr;
        *out_len = len;
    }
    true
}

/// # Safety
/// `pitches` must point to readable `MementoAnkiPitchEntryC` elements if `pitches_len` > 0.
/// Non-null out pointers must be writable. Release allocated buffers with `memento_anki_free_u16`.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_format_pitch(
    pitches: *const MementoAnkiPitchEntryC,
    pitches_len: u64,
    out_pitch: *mut *mut u16,
    out_pitch_len: *mut u64,
    out_positions: *mut *mut u16,
    out_positions_len: *mut u64,
) -> bool
{
    if !out_pitch.is_null()
    {
        unsafe { *out_pitch = ptr::null_mut() };
    }
    if !out_pitch_len.is_null()
    {
        unsafe { *out_pitch_len = 0 };
    }
    if !out_positions.is_null()
    {
        unsafe { *out_positions = ptr::null_mut() };
    }
    if !out_positions_len.is_null()
    {
        unsafe { *out_positions_len = 0 };
    }

    let Ok(Some((pitch_res, pos_res))) = catch_unwind(|| {
        let pitch_views = unsafe { view(pitches, pitches_len)? };
        let mut mora_holders: Vec<Vec<&[u16]>> = Vec::with_capacity(pitch_views.len());
        let mut dict_names = Vec::with_capacity(pitch_views.len());
        let mut positions_slices = Vec::with_capacity(pitch_views.len());

        for p in pitch_views
        {
            let dict_name = unsafe { view(p.dictionary_name.data, p.dictionary_name.len)? };
            dict_names.push(dict_name);
            let pos = unsafe { view(p.positions, p.positions_len)? };
            positions_slices.push(pos);
            let mora_views = unsafe { view(p.mora, p.mora_len)? };
            let mut mora_slices = Vec::with_capacity(mora_views.len());
            for m in mora_views
            {
                mora_slices.push(unsafe { view(m.data, m.len)? });
            }
            mora_holders.push(mora_slices);
        }

        let mut final_entries = Vec::with_capacity(pitch_views.len());
        for i in 0..pitch_views.len()
        {
            final_entries.push(crate::note::PitchEntry {
                dictionary_name: dict_names[i],
                positions: positions_slices[i],
                mora: &mora_holders[i],
            });
        }

        let pitch = crate::note::format_pitch(&final_entries);
        let pos = crate::note::format_pitch_positions(&final_entries);
        Some((pitch, pos))
    }) else {
        return false;
    };

    if !out_pitch.is_null() && !out_pitch_len.is_null()
    {
        let (ptr, len) = own_slice(pitch_res);
        unsafe {
            *out_pitch = ptr;
            *out_pitch_len = len;
        }
    }
    if !out_positions.is_null() && !out_positions_len.is_null()
    {
        let (ptr, len) = own_slice(pos_res);
        unsafe {
            *out_positions = ptr;
            *out_positions_len = len;
        }
    }
    true
}

/// # Safety
/// `selections` must point to readable `MementoAnkiNoteTextViewC` elements if `len` > 0.
/// `out` and `out_len` must be writable and non-null. Release `out` with `memento_anki_free_u16`.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_format_selection(
    selections: *const MementoAnkiNoteTextViewC,
    len: u64,
    out: *mut *mut u16,
    out_len: *mut u64,
) -> bool
{
    if out.is_null() || out_len.is_null()
    {
        return false;
    }
    unsafe {
        *out = ptr::null_mut();
        *out_len = 0;
    }
    let Ok(Some((ptr, out_l))) = catch_unwind(|| {
        let views = unsafe { view(selections, len)? };
        let mut rust_slices = Vec::with_capacity(views.len());
        for v in views
        {
            rust_slices.push(unsafe { view(v.data, v.len)? });
        }
        let result = crate::note::format_selection(&rust_slices);
        Some(own_slice(result))
    }) else {
        return false;
    };
    unsafe {
        *out = ptr;
        *out_len = out_l;
    }
    true
}

/// # Safety
/// `tags` must point to readable `MementoAnkiTagEntryC` elements if `len` > 0.
/// `out` and `out_len` must be writable and non-null. Release `out` with `memento_anki_free_u16`.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_format_tags(
    tags: *const MementoAnkiTagEntryC,
    len: u64,
    brief: bool,
    out: *mut *mut u16,
    out_len: *mut u64,
) -> bool
{
    if out.is_null() || out_len.is_null()
    {
        return false;
    }
    unsafe {
        *out = ptr::null_mut();
        *out_len = 0;
    }
    let Ok(Some((ptr, out_l))) = catch_unwind(|| {
        let views = unsafe { view(tags, len)? };
        let mut rust_tags = Vec::with_capacity(views.len());
        for t in views
        {
            let name = unsafe { view(t.name.data, t.name.len)? };
            let notes = unsafe { view(t.notes.data, t.notes.len)? };
            rust_tags.push(crate::note::TagEntry { name, notes });
        }
        let result = crate::note::format_tags(&rust_tags, brief);
        Some(own_slice(result))
    }) else {
        return false;
    };
    unsafe {
        *out = ptr;
        *out_len = out_l;
    }
    true
}

/// # Safety
/// `readings` must point to readable `MementoAnkiNoteTextViewC` elements if `len` > 0.
/// `out` and `out_len` must be writable and non-null. Release `out` with `memento_anki_free_u16`.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_format_kanji_readings(
    readings: *const MementoAnkiNoteTextViewC,
    len: u64,
    out: *mut *mut u16,
    out_len: *mut u64,
) -> bool
{
    if out.is_null() || out_len.is_null()
    {
        return false;
    }
    unsafe {
        *out = ptr::null_mut();
        *out_len = 0;
    }
    let Ok(Some((ptr, out_l))) = catch_unwind(|| {
        let views = unsafe { view(readings, len)? };
        let mut rust_slices = Vec::with_capacity(views.len());
        for v in views
        {
            rust_slices.push(unsafe { view(v.data, v.len)? });
        }
        let result = crate::note::format_kanji_readings(&rust_slices);
        Some(own_slice(result))
    }) else {
        return false;
    };
    unsafe {
        *out = ptr;
        *out_len = out_l;
    }
    true
}

/// # Safety
/// `defs` must point to readable `MementoAnkiKanjiDefC` elements if `defs_len` > 0.
/// `out` and `out_len` must be writable and non-null. Release `out` with `memento_anki_free_u16`.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_format_kanji_glossary(
    defs: *const MementoAnkiKanjiDefC,
    defs_len: u64,
    out: *mut *mut u16,
    out_len: *mut u64,
) -> bool
{
    if out.is_null() || out_len.is_null()
    {
        return false;
    }
    unsafe {
        *out = ptr::null_mut();
        *out_len = 0;
    }
    let Ok(Some((ptr, out_l))) = catch_unwind(|| {
        let def_views = unsafe { view(defs, defs_len)? };
        let mut tag_holders: Vec<Vec<&[u16]>> = Vec::with_capacity(def_views.len());
        let mut glos_holders: Vec<Vec<&[u16]>> = Vec::with_capacity(def_views.len());
        let mut dict_names = Vec::with_capacity(def_views.len());

        for d in def_views
        {
            let dict = unsafe { view(d.dictionary_name.data, d.dictionary_name.len)? };
            dict_names.push(dict);

            let tag_views = unsafe { view(d.tags, d.tags_len)? };
            let mut tags = Vec::with_capacity(tag_views.len());
            for t in tag_views
            {
                tags.push(unsafe { view(t.data, t.len)? });
            }
            tag_holders.push(tags);

            let glos_views = unsafe { view(d.glossary, d.glossary_len)? };
            let mut glossary = Vec::with_capacity(glos_views.len());
            for g in glos_views
            {
                glossary.push(unsafe { view(g.data, g.len)? });
            }
            glos_holders.push(glossary);
        }

        let mut rust_defs = Vec::with_capacity(def_views.len());
        for i in 0..def_views.len()
        {
            rust_defs.push(crate::note::KanjiDefEntry {
                dictionary_name: dict_names[i],
                tags: &tag_holders[i],
                glossary: &glos_holders[i],
            });
        }

        let result = crate::note::format_kanji_glossary(&rust_defs);
        Some(own_slice(result))
    }) else {
        return false;
    };
    unsafe {
        *out = ptr;
        *out_len = out_l;
    }
    true
}

/// # Safety
/// Any views passed must be readable.
/// Non-null out pointers must be writable. Release allocated buffers with `memento_anki_free_u16`.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_format_audio_url_and_filename(
    url: MementoAnkiNoteTextViewC,
    source_name: MementoAnkiNoteTextViewC,
    expression: MementoAnkiNoteTextViewC,
    reading: MementoAnkiNoteTextViewC,
    reading_as_expression: bool,
    out_url: *mut *mut u16,
    out_url_len: *mut u64,
    out_filename: *mut *mut u16,
    out_filename_len: *mut u64,
) -> bool
{
    if !out_url.is_null()
    {
        unsafe { *out_url = ptr::null_mut() };
    }
    if !out_url_len.is_null()
    {
        unsafe { *out_url_len = 0 };
    }
    if !out_filename.is_null()
    {
        unsafe { *out_filename = ptr::null_mut() };
    }
    if !out_filename_len.is_null()
    {
        unsafe { *out_filename_len = 0 };
    }

    let Ok(Some((url_res, fn_res))) = catch_unwind(|| {
        let u_slice = unsafe { view(url.data, url.len)? };
        let sn_slice = unsafe { view(source_name.data, source_name.len)? };
        let exp_slice = unsafe { view(expression.data, expression.len)? };
        let r_slice = unsafe { view(reading.data, reading.len)? };
        Some(crate::note::format_audio_url_and_filename(
            u_slice, sn_slice, exp_slice, r_slice, reading_as_expression,
        ))
    }) else {
        return false;
    };

    if !out_url.is_null() && !out_url_len.is_null()
    {
        let (ptr, len) = own_slice(url_res);
        unsafe {
            *out_url = ptr;
            *out_url_len = len;
        }
    }
    if !out_filename.is_null() && !out_filename_len.is_null()
    {
        let (ptr, len) = own_slice(fn_res);
        unsafe {
            *out_filename = ptr;
            *out_filename_len = len;
        }
    }
    true
}

/// # Safety
/// `pitches` must point to readable `MementoAnkiPitchGraphEntryC` elements if `pitches_len` > 0.
/// `out` and `out_len` must be writable and non-null. Release `out` with `memento_anki_free_u16`.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_format_pitch_graphs(
    pitches: *const MementoAnkiPitchGraphEntryC,
    pitches_len: u64,
    out: *mut *mut u16,
    out_len: *mut u64,
) -> bool
{
    if out.is_null() || out_len.is_null()
    {
        return false;
    }
    unsafe {
        *out = ptr::null_mut();
        *out_len = 0;
    }
    let Ok(Some((ptr, len))) = catch_unwind(|| {
        let pitch_views = unsafe { view(pitches, pitches_len)? };
        let mut svg_holders: Vec<Vec<&[u16]>> = Vec::with_capacity(pitch_views.len());
        let mut dict_names = Vec::with_capacity(pitch_views.len());

        for p in pitch_views
        {
            let dict_name = unsafe { view(p.dictionary_name.data, p.dictionary_name.len)? };
            dict_names.push(dict_name);
            let svg_views = unsafe { view(p.svg_graphs, p.svg_graphs_len)? };
            let mut svg_slices = Vec::with_capacity(svg_views.len());
            for s in svg_views
            {
                svg_slices.push(unsafe { view(s.data, s.len)? });
            }
            svg_holders.push(svg_slices);
        }

        let mut rust_entries = Vec::with_capacity(pitch_views.len());
        for i in 0..pitch_views.len()
        {
            rust_entries.push(crate::note::PitchGraphEntry {
                dictionary_name: dict_names[i],
                positions_count: pitch_views[i].positions_count as usize,
                svg_graphs: &svg_holders[i],
            });
        }

        let result = crate::note::format_pitch_graphs(&rust_entries);
        Some(own_slice(result))
    }) else {
        return false;
    };
    unsafe {
        *out = ptr;
        *out_len = len;
    }
    true
}

/// # Safety
/// `pitches` must point to readable `MementoAnkiPitchCategoryInputC` elements if `pitches_len` > 0.
/// `out_categories` and `out_categories_len` must be writable and non-null.
/// Release `out_categories` with `memento_anki_free_u8`.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_classify_pitch_categories(
    pitches: *const MementoAnkiPitchCategoryInputC,
    pitches_len: u64,
    can_be_kifuku: bool,
    out_categories: *mut *mut u8,
    out_categories_len: *mut u64,
) -> bool
{
    if out_categories.is_null() || out_categories_len.is_null()
    {
        return false;
    }
    unsafe {
        *out_categories = ptr::null_mut();
        *out_categories_len = 0;
    }
    let Ok(Some((ptr, len))) = catch_unwind(|| {
        let pitch_views = unsafe { view(pitches, pitches_len)? };
        let mut rust_inputs = Vec::with_capacity(pitch_views.len());
        for p in pitch_views
        {
            let pos_slice = unsafe { view(p.positions, p.positions_len)? };
            rust_inputs.push(crate::note::PitchCategoryInput {
                mora_len: p.mora_len as usize,
                positions: pos_slice,
            });
        }
        let categories = crate::note::classify_pitch_categories(&rust_inputs, can_be_kifuku);
        let u8_vec: Vec<u8> = categories.into_iter().map(|c| c as u8).collect();
        Some(own_slice(u8_vec))
    }) else {
        return false;
    };
    unsafe {
        *out_categories = ptr;
        *out_categories_len = len;
    }
    true
}

/// # Safety
/// `ptr` must be null or allocated by `memento_anki_classify_pitch_categories` with matching `len`.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_free_u8(ptr: *mut u8, len: u64)
{
    if !ptr.is_null() && len > 0
    {
        unsafe { drop_boxed_slice(ptr, len) };
    }
}

/// # Safety
/// `stats` must point to readable `MementoAnkiStrokeCountStatC` elements if `stats_len` > 0.
/// `out_value` and `out_value_len` must be writable and non-null. Release `out_value` with `memento_anki_free_u16`.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_find_stroke_count(
    stats: *const MementoAnkiStrokeCountStatC,
    stats_len: u64,
    out_value: *mut *mut u16,
    out_value_len: *mut u64,
) -> bool
{
    if out_value.is_null() || out_value_len.is_null()
    {
        return false;
    }
    unsafe {
        *out_value = ptr::null_mut();
        *out_value_len = 0;
    }
    let Ok(Some((ptr, len))) = catch_unwind(|| {
        let stat_views = unsafe { view(stats, stats_len)? };
        let mut rust_stats = Vec::with_capacity(stat_views.len());
        for s in stat_views
        {
            let notes = unsafe { view(s.notes.data, s.notes.len)? };
            let value = unsafe { view(s.value.data, s.value.len)? };
            rust_stats.push(crate::note::StrokeCountStat { notes, value });
        }
        let matched = crate::note::find_stroke_count(&rust_stats);
        if let Some(val) = matched
        {
            Some(own_slice(val.to_vec()))
        }
        else
        {
            Some((ptr::null_mut(), 0))
        }
    }) else {
        return false;
    };
    unsafe {
        *out_value = ptr;
        *out_value_len = len;
    }
    true
}

/// # Safety
/// `defs` must point to readable `MementoAnkiTermGlossaryDefC` elements if `defs_len` > 0.
/// `out` must be writable and non-null. Release output with `memento_anki_free_term_glossary`.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_format_term_glossary(
    defs: *const MementoAnkiTermGlossaryDefC,
    defs_len: u64,
    out: *mut MementoAnkiTermGlossaryOutputC,
) -> bool
{
    if out.is_null()
    {
        return false;
    }
    unsafe {
        (*out).glossary = ptr::null_mut();
        (*out).glossary_len = 0;
        (*out).glossary_brief = ptr::null_mut();
        (*out).glossary_brief_len = 0;
        (*out).glossary_compact = ptr::null_mut();
        (*out).glossary_compact_len = 0;
    }
    let Ok(Some(output)) = catch_unwind(|| {
        let def_views = unsafe { view(defs, defs_len)? };
        let mut tag_holders: Vec<Vec<&[u16]>> = Vec::with_capacity(def_views.len());
        let mut item_holders: Vec<Vec<&[u16]>> = Vec::with_capacity(def_views.len());
        let mut dict_names = Vec::with_capacity(def_views.len());
        let mut stylesheets = Vec::with_capacity(def_views.len());

        for d in def_views
        {
            let dict = unsafe { view(d.dict_name.data, d.dict_name.len)? };
            dict_names.push(dict);

            let tag_views = unsafe { view(d.tags, d.tags_len)? };
            let mut tags = Vec::with_capacity(tag_views.len());
            for t in tag_views
            {
                tags.push(unsafe { view(t.data, t.len)? });
            }
            tag_holders.push(tags);

            let item_views = unsafe { view(d.items, d.items_len)? };
            let mut items = Vec::with_capacity(item_views.len());
            for it in item_views
            {
                items.push(unsafe { view(it.data, it.len)? });
            }
            item_holders.push(items);

            let style = unsafe { view(d.stylesheet.data, d.stylesheet.len)? };
            stylesheets.push(style);
        }

        let mut rust_defs = Vec::with_capacity(def_views.len());
        for i in 0..def_views.len()
        {
            rust_defs.push(crate::note::TermGlossaryDef {
                dict_id: def_views[i].dict_id,
                dict_name: dict_names[i],
                tags: &tag_holders[i],
                items: &item_holders[i],
                has_styles: def_views[i].has_styles,
                stylesheet: stylesheets[i],
            });
        }

        let res = crate::note::format_term_glossary(&rust_defs);
        let (g_ptr, g_len) = own_slice(res.glossary);
        let (gb_ptr, gb_len) = own_slice(res.glossary_brief);
        let (gc_ptr, gc_len) = own_slice(res.glossary_compact);
        Some(MementoAnkiTermGlossaryOutputC {
            glossary: g_ptr,
            glossary_len: g_len,
            glossary_brief: gb_ptr,
            glossary_brief_len: gb_len,
            glossary_compact: gc_ptr,
            glossary_compact_len: gc_len,
        })
    }) else {
        return false;
    };
    unsafe {
        *out = output;
    }
    true
}

/// # Safety
/// `out` must be null or point to a `MementoAnkiTermGlossaryOutputC` initialized by `memento_anki_format_term_glossary`.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_free_term_glossary(out: *mut MementoAnkiTermGlossaryOutputC)
{
    if out.is_null()
    {
        return;
    }
    unsafe {
        let entry = &mut *out;
        drop_boxed_slice(entry.glossary, entry.glossary_len);
        drop_boxed_slice(entry.glossary_brief, entry.glossary_brief_len);
        drop_boxed_slice(entry.glossary_compact, entry.glossary_compact_len);
        entry.glossary = ptr::null_mut();
        entry.glossary_len = 0;
        entry.glossary_brief = ptr::null_mut();
        entry.glossary_brief_len = 0;
        entry.glossary_compact = ptr::null_mut();
        entry.glossary_compact_len = 0;
    }
}

/// # Safety
/// `out_start` and `out_end` must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_compute_clip_range(
    source: u32,
    sub_start: f64,
    sub_end: f64,
    ctx_start: f64,
    ctx_end: f64,
    pad_start: f64,
    pad_end: f64,
    out_start: *mut f64,
    out_end: *mut f64,
) -> bool
{
    if out_start.is_null() || out_end.is_null()
    {
        return false;
    }
    let timing_source = match source
    {
        0 => crate::note::TimingSource::Subtitle,
        1 => crate::note::TimingSource::Context,
        _ => return false,
    };
    let Ok(Some((start, end))) = catch_unwind(|| {
        crate::note::compute_clip_range(
            timing_source, sub_start, sub_end, ctx_start, ctx_end, pad_start, pad_end,
        )
    }) else {
        return false;
    };
    unsafe {
        *out_start = start;
        *out_end = end;
    }
    true
}

#[cfg(test)]
mod tests
{
    use super::*;

    fn units(s: &str) -> Vec<u16> { s.encode_utf16().collect() }
    fn input_text(s: &[u16]) -> MementoAnkiNoteTextViewC
    {
        MementoAnkiNoteTextViewC { data: s.as_ptr(), len: s.len() as u64 }
    }

    #[test]
    fn builds_nested_fields_and_frees_complete_tree()
    {
        let deck = units("Deck😀");
        let model = units("Model");
        let tag = units("漢字");
        let field_key = units("Front");
        let field_value = units("<b>hello</b>");
        let field_text = MementoAnkiNoteJsonInC {
            kind: STRING, boolean: false, number: 0.0, text: input_text(&field_value),
            items: ptr::null(), items_len: 0, entries: ptr::null(), entries_len: 0,
        };
        let entries = [MementoAnkiNoteEntryInC {
            key: input_text(&field_key), value: &field_text,
        }];
        let input = MementoAnkiNoteInputC {
            deck: input_text(&deck), model: input_text(&model),
            tags: &input_text(&tag), tags_len: 1,
            fields: MementoAnkiNoteJsonInC {
                kind: OBJECT, boolean: false, number: 0.0,
                text: input_text(&[]), items: ptr::null(), items_len: 0,
                entries: entries.as_ptr(), entries_len: 1,
            },
            duplicate_policy: 1,
        };
        let mut note = MementoAnkiNoteJsonC::null();
        assert!(unsafe { memento_anki_build_note(&input, &mut note) });
        assert_eq!(note.kind, OBJECT);
        assert_eq!(note.entries_len, 5);
        let children = unsafe { std::slice::from_raw_parts(note.entries, note.entries_len as usize) };
        let fields = children.iter().find(|entry| unsafe {
            std::slice::from_raw_parts(entry.key, entry.key_len as usize) == units("fields")
        }).expect("fields");
        assert_eq!(fields.value.kind, OBJECT);
        assert_eq!(fields.value.entries_len, 1);
        let first = unsafe { &*fields.value.entries };
        assert_eq!(first.value.kind, STRING);
        assert_eq!(unsafe { std::slice::from_raw_parts(first.value.text, first.value.text_len as usize) }, field_value);
        unsafe { memento_anki_free_note(&mut note) };
        assert_eq!(note.kind, NULL);
        assert!(note.entries.is_null());
        unsafe { memento_anki_free_note(&mut note) };
    }

    #[test]
    fn replace_newlines_c_abi()
    {
        let input = units("A\nB\nC");
        let replacer = units("<br>");
        let mut out: *mut u16 = ptr::null_mut();
        let mut out_len: u64 = 0;
        assert!(unsafe {
            memento_anki_replace_newlines(
                input.as_ptr(),
                input.len() as u64,
                replacer.as_ptr(),
                replacer.len() as u64,
                &mut out,
                &mut out_len,
            )
        });
        assert_eq!(
            unsafe { std::slice::from_raw_parts(out, out_len as usize) },
            units("A<br>B<br>C")
        );
        unsafe { memento_anki_free_u16(out, out_len) };
    }

    #[test]
    fn parse_frequency_and_stats_c_abi()
    {
        let freq_str = units("123.45Hz");
        let mut val = 0.0;
        assert!(unsafe {
            memento_anki_parse_frequency_value(freq_str.as_ptr(), freq_str.len() as u64, &mut val)
        });
        assert_eq!(val, 123.45);

        let d1 = units("Dict1");
        let d2 = units("Dict2");
        let f1 = units("3");
        let f2 = units("5");
        let entries = [
            MementoAnkiFreqEntryC {
                dictionary: input_text(&d1),
                freq: input_text(&f1),
            },
            MementoAnkiFreqEntryC {
                dictionary: input_text(&d2),
                freq: input_text(&f2),
            },
        ];

        let mut numbers: *mut f64 = ptr::null_mut();
        let mut numbers_len: u64 = 0;
        let mut harmonic: i64 = 0;
        let mut has_harmonic: bool = false;
        let mut average: i64 = 0;
        let mut has_average: bool = false;

        assert!(unsafe {
            memento_anki_frequency_stats(
                entries.as_ptr(),
                entries.len() as u64,
                &mut numbers,
                &mut numbers_len,
                &mut harmonic,
                &mut has_harmonic,
                &mut average,
                &mut has_average,
            )
        });

        assert_eq!(numbers_len, 2);
        assert_eq!(unsafe { std::slice::from_raw_parts(numbers, numbers_len as usize) }, [3.0, 5.0]);
        assert!(has_harmonic);
        assert_eq!(harmonic, 3);
        assert!(has_average);
        assert_eq!(average, 4);

        unsafe { memento_anki_free_f64(numbers, numbers_len) };
    }

    #[test]
    fn marker_builders_c_abi()
    {
        // 1. is_kifuku_applicable
        let v1 = units("v1");
        let vs = units("vs");
        let rules_v1 = [input_text(&v1)];
        let rules_vs = [input_text(&v1), input_text(&vs)];
        assert!(unsafe { memento_anki_is_kifuku_applicable(rules_v1.as_ptr(), 1) });
        assert!(!unsafe { memento_anki_is_kifuku_applicable(rules_vs.as_ptr(), 2) });
        assert!(!unsafe { memento_anki_is_kifuku_applicable(ptr::null(), 0) });

        // 2. format_furigana
        let surf = units("漢");
        let read = units("かん");
        let pairs = [MementoAnkiFuriganaPairC {
            surface: input_text(&surf),
            reading: input_text(&read),
        }];
        let mut out: *mut u16 = ptr::null_mut();
        let mut out_len: u64 = 0;
        assert!(unsafe {
            memento_anki_format_furigana(pairs.as_ptr(), 1, false, &mut out, &mut out_len)
        });
        assert_eq!(
            unsafe { std::slice::from_raw_parts(out, out_len as usize) },
            units("<ruby>漢<rt>かん</rt></ruby>")
        );
        unsafe { memento_anki_free_u16(out, out_len) };

        // 3. build_frequencies_html
        let d = units("Dict");
        let f = units("42");
        let entries = [MementoAnkiFreqEntryC {
            dictionary: input_text(&d),
            freq: input_text(&f),
        }];
        assert!(unsafe {
            memento_anki_build_frequencies_html(entries.as_ptr(), 1, false, false, &mut out, &mut out_len)
        });
        assert_eq!(
            unsafe { std::slice::from_raw_parts(out, out_len as usize) },
            units("<ul><li>Dict: 42</li></ul>")
        );
        unsafe { memento_anki_free_u16(out, out_len) };

        // 4. format_pitch
        let mora1 = units("は");
        let mora2 = units("な");
        let mora_arr = [input_text(&mora1), input_text(&mora2)];
        let pos = [0u8];
        let pitches = [MementoAnkiPitchEntryC {
            dictionary_name: input_text(&d),
            positions: pos.as_ptr(),
            positions_len: 1,
            mora: mora_arr.as_ptr(),
            mora_len: 2,
        }];
        let mut out_pitch: *mut u16 = ptr::null_mut();
        let mut out_pitch_len: u64 = 0;
        let mut out_pos: *mut u16 = ptr::null_mut();
        let mut out_pos_len: u64 = 0;
        assert!(unsafe {
            memento_anki_format_pitch(
                pitches.as_ptr(),
                1,
                &mut out_pitch,
                &mut out_pitch_len,
                &mut out_pos,
                &mut out_pos_len,
            )
        });
        assert_eq!(
            unsafe { std::slice::from_raw_parts(out_pitch, out_pitch_len as usize) },
            units("<span class=\"memento-pitch\">は<span style=\"border-top: solid;\">な</span></span>")
        );
        assert_eq!(
            unsafe { std::slice::from_raw_parts(out_pos, out_pos_len as usize) },
            units("<span class=\"memento-pitch-position\">[0]</span>")
        );
        unsafe {
            memento_anki_free_u16(out_pitch, out_pitch_len);
            memento_anki_free_u16(out_pos, out_pos_len);
        }

        // 5. format_selection
        let sel1 = units("first");
        let sel2 = units("second");
        let sels = [input_text(&sel1), input_text(&sel2)];
        assert!(unsafe {
            memento_anki_format_selection(sels.as_ptr(), 2, &mut out, &mut out_len)
        });
        assert_eq!(
            unsafe { std::slice::from_raw_parts(out, out_len as usize) },
            units("<ul><li>first</li><li>second</li></ul>")
        );
        unsafe { memento_anki_free_u16(out, out_len) };

        // 6. format_tags
        let t_name = units("Tag1");
        let t_notes = units("Notes1");
        let tags = [MementoAnkiTagEntryC {
            name: input_text(&t_name),
            notes: input_text(&t_notes),
        }];
        assert!(unsafe {
            memento_anki_format_tags(tags.as_ptr(), 1, false, &mut out, &mut out_len)
        });
        assert_eq!(
            unsafe { std::slice::from_raw_parts(out, out_len as usize) },
            units("<ul><li>Tag1: Notes1</li></ul>")
        );
        unsafe { memento_anki_free_u16(out, out_len) };

        // 7. format_kanji_readings
        let r1 = units("ひと");
        let r2 = units("ひと.つ");
        let readings = [input_text(&r1), input_text(&r2)];
        assert!(unsafe {
            memento_anki_format_kanji_readings(readings.as_ptr(), 2, &mut out, &mut out_len)
        });
        assert_eq!(
            unsafe { std::slice::from_raw_parts(out, out_len as usize) },
            units("ひと, ひと.つ")
        );
        unsafe { memento_anki_free_u16(out, out_len) };

        // 8. format_kanji_glossary
        let g1 = units("one");
        let glos_arr = [input_text(&g1)];
        let tag_arr = [input_text(&t_name)];
        let defs = [MementoAnkiKanjiDefC {
            dictionary_name: input_text(&d),
            tags: tag_arr.as_ptr(),
            tags_len: 1,
            glossary: glos_arr.as_ptr(),
            glossary_len: 1,
        }];
        assert!(unsafe {
            memento_anki_format_kanji_glossary(defs.as_ptr(), 1, &mut out, &mut out_len)
        });
        assert_eq!(
            unsafe { std::slice::from_raw_parts(out, out_len as usize) },
            units("<ol><li><i>(Tag1, Dict)</i></li><ol><li>one</li></ol></ol>")
        );
        unsafe { memento_anki_free_u16(out, out_len) };

        // 9. format_audio_url_and_filename
        let url = units("http://test/{expression}/{reading}");
        let src = units("Audio Src");
        let expr = units("木");
        let read = units("き");
        let mut out_url: *mut u16 = ptr::null_mut();
        let mut out_url_len: u64 = 0;
        let mut out_fn: *mut u16 = ptr::null_mut();
        let mut out_fn_len: u64 = 0;
        assert!(unsafe {
            memento_anki_format_audio_url_and_filename(
                input_text(&url),
                input_text(&src),
                input_text(&expr),
                input_text(&read),
                false,
                &mut out_url,
                &mut out_url_len,
                &mut out_fn,
                &mut out_fn_len,
            )
        });
        assert_eq!(
            unsafe { std::slice::from_raw_parts(out_url, out_url_len as usize) },
            units("http://test/木/き")
        );
        assert_eq!(
            unsafe { std::slice::from_raw_parts(out_fn, out_fn_len as usize) },
            units("memento_Audio_Src_き_木.mp3")
        );
        unsafe {
            memento_anki_free_u16(out_url, out_url_len);
            memento_anki_free_u16(out_fn, out_fn_len);
        }

        // 10. format_pitch_graphs
        let svg = units("<svg>pitch</svg>");
        let svg_view = input_text(&svg);
        let pitch_graphs = [MementoAnkiPitchGraphEntryC {
            dictionary_name: input_text(&d),
            positions_count: 1,
            svg_graphs: &svg_view,
            svg_graphs_len: 1,
        }];
        assert!(unsafe {
            memento_anki_format_pitch_graphs(pitch_graphs.as_ptr(), 1, &mut out, &mut out_len)
        });
        assert_eq!(
            unsafe { std::slice::from_raw_parts(out, out_len as usize) },
            units("<span class=\"memento-pitch-graph\"><svg>pitch</svg></span>")
        );
        unsafe { memento_anki_free_u16(out, out_len) };

        // 11. classify_pitch_categories
        let pos_bytes = [0u8, 2u8];
        let cat_inputs = [MementoAnkiPitchCategoryInputC {
            mora_len: 3,
            positions: pos_bytes.as_ptr(),
            positions_len: 2,
        }];
        let mut out_cats: *mut u8 = ptr::null_mut();
        let mut out_cats_len: u64 = 0;
        assert!(unsafe {
            memento_anki_classify_pitch_categories(
                cat_inputs.as_ptr(), 1, false, &mut out_cats, &mut out_cats_len
            )
        });
        assert_eq!(out_cats_len, 2);
        assert_eq!(unsafe { std::slice::from_raw_parts(out_cats, out_cats_len as usize) }, [1u8, 5u8]); // Heiban = 1, Nakadaka = 5
        unsafe { memento_anki_free_u8(out_cats, out_cats_len) };

        // 12. find_stroke_count
        let sc_name = units("Stroke count");
        let sc_val = units("15");
        let stats = [MementoAnkiStrokeCountStatC {
            notes: input_text(&sc_name),
            value: input_text(&sc_val),
        }];
        assert!(unsafe {
            memento_anki_find_stroke_count(stats.as_ptr(), 1, &mut out, &mut out_len)
        });
        assert_eq!(
            unsafe { std::slice::from_raw_parts(out, out_len as usize) },
            units("15")
        );
        unsafe { memento_anki_free_u16(out, out_len) };

        // 13. format_term_glossary
        let item = units("meaning");
        let item_view = input_text(&item);
        let tag_view = input_text(&t_name);
        let style = units("body { color: blue; }");
        let term_defs = [MementoAnkiTermGlossaryDefC {
            dict_id: 42,
            dict_name: input_text(&d),
            tags: &tag_view,
            tags_len: 1,
            items: &item_view,
            items_len: 1,
            has_styles: true,
            stylesheet: input_text(&style),
        }];
        let mut term_glos_out = MementoAnkiTermGlossaryOutputC {
            glossary: ptr::null_mut(),
            glossary_len: 0,
            glossary_brief: ptr::null_mut(),
            glossary_brief_len: 0,
            glossary_compact: ptr::null_mut(),
            glossary_compact_len: 0,
        };
        assert!(unsafe {
            memento_anki_format_term_glossary(term_defs.as_ptr(), 1, &mut term_glos_out)
        });
        assert!(term_glos_out.glossary_len > 0);
        assert!(term_glos_out.glossary_brief_len > 0);
        assert!(term_glos_out.glossary_compact_len > 0);
        unsafe { memento_anki_free_term_glossary(&mut term_glos_out) };

        // 14. compute_clip_range
        let mut c_start = 0.0;
        let mut c_end = 0.0;
        assert!(unsafe {
            memento_anki_compute_clip_range(0, 10.0, 20.0, 5.0, 25.0, 1.0, 2.0, &mut c_start, &mut c_end)
        });
        assert_eq!(c_start, 9.0);
        assert_eq!(c_end, 22.0);

        // invalid source
        assert!(!unsafe {
            memento_anki_compute_clip_range(99, 10.0, 20.0, 5.0, 25.0, 1.0, 2.0, &mut c_start, &mut c_end)
        });
    }
}
