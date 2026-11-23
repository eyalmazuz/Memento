//! Marker substitution and Anki note JSON metadata assembly.

use serde_json::{Map, Value};

use crate::tokenizer::{self, Marker};

/// Whether an Anki note may duplicate another note.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DuplicatePolicy {
    None,
    DifferentDeck,
    SameDeck,
}

/// The result of evaluating one marker. An empty handled marker acts as a
/// fallback until a later alternative produces text or media; an unhandled
/// marker leaves the original token intact if no later alternative handles it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MarkerResult {
    pub handled: bool,
    pub media: bool,
    pub text: String,
}

impl MarkerResult {
    fn is_empty(&self) -> bool {
        !self.media && self.text.is_empty()
    }
}

/// Substitutes `{marker|fallback}` tokens in `field`, evaluating `common` then
/// `specific` for each alternative marker in token order.
///
/// Each handled token replaces all non-overlapping occurrences of its raw text
/// in the current field buffer in token order.
pub fn substitute_markers<C, S>(field: &str, mut common: C, mut specific: S) -> String
where
    C: FnMut(&Marker) -> MarkerResult,
    S: FnMut(&Marker) -> MarkerResult,
{
    let tokens = tokenizer::tokenize(field);
    let mut result_field = field.to_owned();
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
        if final_result.handled && token.raw != final_result.text {
            result_field = result_field.replace(&token.raw, &final_result.text);
        }
    }
    result_field
}

/// Builds an AnkiConnect `addNote` note JSON object with deck, model, tags,
/// duplicate-policy options, and fields.
pub fn build_note_metadata(
    deck: String,
    model: String,
    tags: Vec<String>,
    fields: Value,
    policy: DuplicatePolicy,
) -> Map<String, Value> {
    let mut note = Map::new();
    note.insert("deckName".to_owned(), Value::String(deck));
    note.insert("modelName".to_owned(), Value::String(model));
    note.insert(
        "tags".to_owned(),
        Value::Array(tags.into_iter().map(Value::String).collect()),
    );

    let mut options = Map::new();
    match policy {
        DuplicatePolicy::None => {
            options.insert("allowDuplicate".to_owned(), Value::Bool(false));
        }
        DuplicatePolicy::DifferentDeck => {
            options.insert("duplicateScope".to_owned(), Value::String("deck".to_owned()));
        }
        DuplicatePolicy::SameDeck => {
            options.insert("allowDuplicate".to_owned(), Value::Bool(true));
        }
    }
    note.insert("options".to_owned(), Value::Object(options));
    note.insert("fields".to_owned(), fields);
    note
}
