//! Marker substitution and note metadata assembly from `src/anki/notebuilder.cpp`.

use crate::json::{JsonObject, JsonValue};
use crate::tokenizer::{self, Marker};

/// Whether an Anki note may duplicate another note (see
/// `Anki::Note::Context::setDuplicatePolicy`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DuplicatePolicy {
    None,
    DifferentDeck,
    SameDeck,
}

/// The result of processing one marker. An empty handled marker is a valid
/// fallback until a later marker produces text or media; an unhandled marker
/// leaves the original token intact if no later marker handles it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct MarkerResult {
    pub handled: bool,
    pub media: bool,
    pub text: Vec<u16>,
}

impl MarkerResult {
    fn is_empty(&self) -> bool {
        !self.media && self.text.is_empty()
    }
}

/// `QString::replace(before, after)` for a nonempty `before`, matching all
/// nonoverlapping occurrences case-sensitively. Unlike an interpolation pass,
/// each successive token is replaced in the *current* field; this matters
/// when a previous replacement produced text matching a later token.
pub(super) fn replace_all(value: &mut Vec<u16>, before: &[u16], after: &[u16]) {
    if before.is_empty() || before == after {
        return;
    }
    let Some(first) = value
        .windows(before.len())
        .position(|window| window == before)
    else {
        return;
    };

    let mut replaced = Vec::with_capacity(value.len());
    replaced.extend_from_slice(&value[..first]);
    let mut offset = first;
    loop {
        replaced.extend_from_slice(after);
        offset += before.len();
        let Some(next) = value[offset..]
            .windows(before.len())
            .position(|window| window == before)
        else {
            replaced.extend_from_slice(&value[offset..]);
            break;
        };
        replaced.extend_from_slice(&value[offset..offset + next]);
        offset += next;
    }
    *value = replaced;
}

/// `replaceNewLines`: replace only U+000A, not CR or Unicode line separators.
/// Replacement text is not searched again within the same call.
pub fn replace_newlines(input: &[u16], replacement: &[u16]) -> Vec<u16> {
    let mut result = input.to_vec();
    replace_all(&mut result, &[b'\n' as u16], replacement);
    result
}

/// Mirrors the two `Anki::Note::build` field loops (term and kanji) in
/// `notebuilder.cpp`. Tokens are parsed from the ORIGINAL field by the Stage 1
/// tokenizer, then `QString::replace(token.raw, result.text)` replaces every
/// occurrence in the CURRENT field, in token order. This can cascade.
///
/// For each alternative marker, try `common` first; if it is unhandled, try
/// `specific`. A handled but empty result remains the fallback while later
/// alternatives are considered. A media result counts as nonempty even when
/// its text is empty. Both closures are called in the C++ order, allowing a
/// caller to collect file/media field names, but this function itself does
/// not create or attach Anki media.
pub fn substitute_markers<C, S>(field: &[u16], mut common: C, mut specific: S) -> Vec<u16>
where
    C: FnMut(&Marker) -> MarkerResult,
    S: FnMut(&Marker) -> MarkerResult,
{
    let tokens = tokenizer::tokenize(field);
    let mut result_field = field.to_vec();
    for token in tokens {
        let mut final_result = MarkerResult::default();
        for marker in &token.markers {
            let mut result = common(marker);
            if !result.is_empty() {
                final_result = result;
                break;
            } else if result.handled {
                final_result = result;
                continue;
            }

            result = specific(marker);
            if !result.is_empty() {
                final_result = result;
                break;
            } else if result.handled {
                final_result = result;
            }
        }
        if final_result.handled {
            replace_all(&mut result_field, &token.raw, &final_result.text);
        }
    }
    result_field
}

fn key(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// Pure JSON-object metadata from `Context::setDeck`, `setModel`, `setTags`,
/// `setDuplicatePolicy` and `setFields`, in the same order as the C++ note
/// builders. `fields` is a complete QJsonValue, matching `Context::setFields`
/// (the normal note builders pass a QJsonObject of string field values).
pub fn build_note_metadata(
    deck: Vec<u16>,
    model: Vec<u16>,
    tags: Vec<Vec<u16>>,
    fields: JsonValue,
    policy: DuplicatePolicy,
) -> JsonObject {
    let mut note = JsonObject::new();
    note.insert(key("deckName"), JsonValue::String(deck));
    note.insert(key("modelName"), JsonValue::String(model));
    note.insert(
        key("tags"),
        JsonValue::Array(tags.into_iter().map(JsonValue::String).collect()),
    );

    let mut options = JsonObject::new();
    match policy {
        DuplicatePolicy::None => {
            options.insert(key("allowDuplicate"), JsonValue::Bool(false));
        }
        DuplicatePolicy::DifferentDeck => {
            options.insert(key("duplicateScope"), JsonValue::String(key("deck")));
        }
        DuplicatePolicy::SameDeck => {
            options.insert(key("allowDuplicate"), JsonValue::Bool(true));
        }
    }
    note.insert(key("options"), JsonValue::Object(options));

    crate::json::insert_qjson(&mut note, key("fields"), fields);
    note
}
