//! Pure operations from `src/anki/notebuilder.cpp`. The note C ABI exposes
//! metadata assembly for parity tests; production `Anki::Note::build` still
//! supplies its own marker values and media side effects in C++.

use crate::json::{JsonObject, JsonValue};
use crate::tokenizer::{self, Marker};

/// Whether an Anki note may duplicate another note (see
/// `Anki::Note::Context::setDuplicatePolicy`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DuplicatePolicy
{
    None,
    DifferentDeck,
    SameDeck,
}

/// The result of processing one marker. An empty handled marker is a valid
/// fallback until a later marker produces text or media; an unhandled marker
/// leaves the original token intact if no later marker handles it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct MarkerResult
{
    pub handled: bool,
    pub media: bool,
    pub text: Vec<u16>,
}

impl MarkerResult
{
    fn is_empty(&self) -> bool
    {
        !self.media && self.text.is_empty()
    }
}

/// `QString::replace(before, after)` for a nonempty `before`, matching all
/// nonoverlapping occurrences case-sensitively. Unlike an interpolation pass,
/// each successive token is replaced in the *current* field; this matters
/// when a previous replacement produced text matching a later token.
fn replace_all(value: &mut Vec<u16>, before: &[u16], after: &[u16])
{
    if before.is_empty() || before == after
    {
        return;
    }
    let Some(first) = value.windows(before.len()).position(|window| window == before) else
    {
        return;
    };

    let mut replaced = Vec::with_capacity(value.len());
    replaced.extend_from_slice(&value[..first]);
    let mut offset = first;
    loop
    {
        replaced.extend_from_slice(after);
        offset += before.len();
        let Some(next) = value[offset..].windows(before.len()).position(|window| window == before) else
        {
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
pub fn replace_newlines(input: &[u16], replacement: &[u16]) -> Vec<u16>
{
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
pub fn substitute_markers<C, S>(
    field: &[u16],
    mut common: C,
    mut specific: S,
) -> Vec<u16>
where
    C: FnMut(&Marker) -> MarkerResult,
    S: FnMut(&Marker) -> MarkerResult,
{
    let tokens = tokenizer::tokenize(field);
    let mut result_field = field.to_vec();
    for token in tokens
    {
        let mut final_result = MarkerResult::default();
        for marker in &token.markers
        {
            let mut result = common(marker);
            if !result.is_empty()
            {
                final_result = result;
                break;
            }
            else if result.handled
            {
                final_result = result;
                continue;
            }

            result = specific(marker);
            if !result.is_empty()
            {
                final_result = result;
                break;
            }
            else if result.handled
            {
                final_result = result;
            }
        }
        if final_result.handled
        {
            replace_all(&mut result_field, &token.raw, &final_result.text);
        }
    }
    result_field
}

fn key(text: &str) -> Vec<u16>
{
    text.encode_utf16().collect()
}

/// Pure JSON-object metadata from `Context::setDeck`, `setModel`, `setTags`,
/// `setDuplicatePolicy` and `setFields`, in the same order as the C++ note
/// builders. `fields` is a complete QJsonValue, matching `Context::setFields`
/// (the normal note builders pass a QJsonObject of string field values).
/// Anki media are attached later by C++.
pub fn build_note_metadata(
    deck: Vec<u16>,
    model: Vec<u16>,
    tags: Vec<Vec<u16>>,
    fields: JsonValue,
    policy: DuplicatePolicy,
) -> JsonObject
{
    let mut note = JsonObject::new();
    note.insert(key("deckName"), JsonValue::String(deck));
    note.insert(key("modelName"), JsonValue::String(model));
    note.insert(
        key("tags"),
        JsonValue::Array(tags.into_iter().map(JsonValue::String).collect()),
    );

    let mut options = JsonObject::new();
    match policy
    {
        DuplicatePolicy::None =>
        {
            options.insert(key("allowDuplicate"), JsonValue::Bool(false));
        }
        DuplicatePolicy::DifferentDeck =>
        {
            options.insert(key("duplicateScope"), JsonValue::String(key("deck")));
        }
        DuplicatePolicy::SameDeck =>
        {
            options.insert(key("allowDuplicate"), JsonValue::Bool(true));
        }
    }
    note.insert(key("options"), JsonValue::Object(options));

    crate::json::insert_qjson(&mut note, key("fields"), fields);
    note
}

/// Borrowed frequency dictionary entry for aggregate stats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrequencyEntry<'a>
{
    pub dictionary: &'a [u16],
    pub freq: &'a [u16],
}

/// Matches `parseFrequencyValue(const QString &frequency)` in `notebuilder.cpp`.
/// Matches `^[+-]?([0-9]+([.][0-9]*)?|[.][0-9]+)` at start using ASCII digits
/// and '.', or exact star ratings ★ through ★★★★★.
pub fn parse_frequency_value(frequency: &[u16]) -> Option<f64>
{
    if frequency.is_empty()
    {
        return None;
    }

    let mut idx = 0;
    if frequency[0] == b'+' as u16 || frequency[0] == b'-' as u16
    {
        idx += 1;
    }

    let is_ascii_digit = |c: u16| c >= b'0' as u16 && c <= b'9' as u16;

    let mut matched_len = 0;
    if idx < frequency.len() && is_ascii_digit(frequency[idx])
    {
        while idx < frequency.len() && is_ascii_digit(frequency[idx])
        {
            idx += 1;
        }
        if idx < frequency.len() && frequency[idx] == b'.' as u16
        {
            idx += 1;
            while idx < frequency.len() && is_ascii_digit(frequency[idx])
            {
                idx += 1;
            }
        }
        matched_len = idx;
    }
    else if idx < frequency.len() && frequency[idx] == b'.' as u16
    {
        if idx + 1 < frequency.len() && is_ascii_digit(frequency[idx + 1])
        {
            idx += 2;
            while idx < frequency.len() && is_ascii_digit(frequency[idx])
            {
                idx += 1;
            }
            matched_len = idx;
        }
    }

    if matched_len > 0
    {
        let s: String = frequency[..matched_len].iter().map(|&c| c as u8 as char).collect();
        if let Ok(val) = s.parse::<f64>()
        {
            if val.is_finite()
            {
                return Some(val);
            }
        }
        return None;
    }

    const STAR: u16 = 0x2605;
    match frequency
    {
        [STAR] => Some(20.0),
        [STAR, STAR] => Some(40.0),
        [STAR, STAR, STAR] => Some(60.0),
        [STAR, STAR, STAR, STAR] => Some(80.0),
        [STAR, STAR, STAR, STAR, STAR] => Some(100.0),
        _ => None,
    }
}

/// Matches `getFrequencyNumbers` in `notebuilder.cpp`.
/// Only selects the first positive frequency (> 0.0) from each dictionary.
/// An empty dictionary first entry is skipped because `previousDictionary` starts empty.
pub fn frequency_numbers(entries: &[FrequencyEntry<'_>]) -> Vec<f64>
{
    let mut previous_dictionary: &[u16] = &[];
    let mut numbers = Vec::new();
    for entry in entries
    {
        if entry.dictionary == previous_dictionary || entry.freq.is_empty()
        {
            continue;
        }
        if let Some(val) = parse_frequency_value(entry.freq)
        {
            if val > 0.0
            {
                previous_dictionary = entry.dictionary;
                numbers.push(val);
            }
        }
    }
    numbers
}

/// Matches `getFrequencyHarmonic` in `notebuilder.cpp`.
pub fn frequency_harmonic(numbers: &[f64]) -> Option<i64>
{
    if numbers.is_empty()
    {
        return None;
    }
    let mut sum = 0.0;
    for &val in numbers
    {
        if val != 0.0
        {
            sum += 1.0 / val;
        }
    }
    if sum == 0.0
    {
        return None;
    }
    let harmonic = (numbers.len() as f64 / sum).floor();
    Some(harmonic as i64)
}

/// Matches `getFrequencyAverage` in `notebuilder.cpp`.
pub fn frequency_average(numbers: &[f64]) -> Option<i64>
{
    if numbers.is_empty()
    {
        return None;
    }
    let sum: f64 = numbers.iter().sum();
    let average = (sum / numbers.len() as f64).floor();
    Some(average as i64)
}

/// Combines frequency numbers, harmonic mean, and average calculation.
pub fn frequency_stats(entries: &[FrequencyEntry<'_>]) -> (Vec<f64>, Option<i64>, Option<i64>)
{
    let numbers = frequency_numbers(entries);
    let harmonic = frequency_harmonic(&numbers);
    let average = frequency_average(&numbers);
    (numbers, harmonic, average)
}

/// Matches `isKifukuApplicable` in `notebuilder.cpp`.
pub fn is_kifuku_applicable(rules: &[&[u16]]) -> bool
{
    let vs = [b'v' as u16, b's' as u16];
    let v1 = [b'v' as u16, b'1' as u16];
    let v5 = [b'v' as u16, b'5' as u16];
    let vk = [b'v' as u16, b'k' as u16];
    let vz = [b'v' as u16, b'z' as u16];
    let adj_i = [b'a' as u16, b'd' as u16, b'j' as u16, b'-' as u16, b'i' as u16];
    let mut can_be_kifuku = false;
    for &rule in rules
    {
        if rule == vs
        {
            return false;
        }
        else if rule == v1 || rule == v5 || rule == vk || rule == vz || rule == adj_i
        {
            can_be_kifuku = true;
        }
    }
    can_be_kifuku
}

/// Borrowed furigana pair (surface and optional reading).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FuriganaPair<'a>
{
    pub surface: &'a [u16],
    pub reading: &'a [u16],
}

/// Matches `getFurigana` and `getFuriganaPlain` pair formatting in `notebuilder.cpp`.
pub fn format_furigana(pairs: &[FuriganaPair<'_>], plain: bool) -> Vec<u16>
{
    let mut result = Vec::new();
    let ruby_open = key("<ruby>");
    let rt_open = key("<rt>");
    let ruby_close = key("</rt></ruby>");
    for pair in pairs
    {
        if pair.reading.is_empty()
        {
            result.extend_from_slice(pair.surface);
        }
        else if plain
        {
            result.extend_from_slice(pair.surface);
            result.push(b'[' as u16);
            result.extend_from_slice(pair.reading);
            result.push(b']' as u16);
        }
        else
        {
            result.extend_from_slice(&ruby_open);
            result.extend_from_slice(pair.surface);
            result.extend_from_slice(&rt_open);
            result.extend_from_slice(pair.reading);
            result.extend_from_slice(&ruby_close);
        }
    }
    result
}

/// Matches `buildFrequencies` in `notebuilder.cpp`.
pub fn build_frequencies_html(
    entries: &[FrequencyEntry<'_>],
    value_only: bool,
    min_value_only: bool,
) -> Vec<u16>
{
    if entries.is_empty()
    {
        return Vec::new();
    }

    const MIN_VALUE_DEFAULT: f64 = 9999999.0;
    let mut min_freq_dictionary: Vec<u16> = Vec::new();
    let mut min_freq = MIN_VALUE_DEFAULT;
    let mut min_freq_str: Vec<u16> = Vec::new();

    let colon = b':' as u16;
    let colon_space = key(": ");
    let space = key(" ");
    let ul_open = key("<ul>");
    let ul_close = key("</ul>");
    let li_open = key("<li>");
    let li_close = key("</li>");
    let br = key("<br>");

    let mut freq_str: Vec<u16> = Vec::new();
    if !value_only
    {
        freq_str.extend_from_slice(&ul_open);
    }

    for entry in entries
    {
        let ends_with_colon = entry.dictionary.last() == Some(&colon);
        let sep = if ends_with_colon { &space } else { &colon_space };

        if !value_only
        {
            freq_str.extend_from_slice(&li_open);
            freq_str.extend_from_slice(entry.dictionary);
            freq_str.extend_from_slice(sep);
        }
        freq_str.extend_from_slice(entry.freq);
        if value_only
        {
            freq_str.extend_from_slice(&br);
        }
        else
        {
            freq_str.extend_from_slice(&li_close);
        }

        let freq_value = parse_frequency_value(entry.freq);
        if min_value_only
        {
            if let Some(val) = freq_value
            {
                if val < min_freq
                {
                    min_freq = val;
                    min_freq_str = entry.freq.to_vec();
                    min_freq_dictionary = entry.dictionary.to_vec();
                    min_freq_dictionary.extend_from_slice(sep);
                }
            }
        }
    }

    if !value_only
    {
        freq_str.extend_from_slice(&ul_close);
    }

    if min_value_only
    {
        if !value_only
        {
            freq_str = ul_open;
            freq_str.extend_from_slice(&li_open);
            freq_str.extend_from_slice(&min_freq_dictionary);
            freq_str.extend_from_slice(&min_freq_str);
            freq_str.extend_from_slice(&li_close);
            freq_str.extend_from_slice(&ul_close);
        }
        else
        {
            freq_str = min_freq_str;
        }
    }

    let empty_ul = key("<ul></ul>");
    if freq_str == empty_ul
    {
        freq_str.clear();
    }

    freq_str
}

/// Borrowed pitch entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PitchEntry<'a>
{
    pub dictionary_name: &'a [u16],
    pub positions: &'a [u8],
    pub mora: &'a [&'a [u16]],
}

/// Matches `getPitch` in `notebuilder.cpp`.
pub fn format_pitch(pitches: &[PitchEntry<'_>]) -> Vec<u16>
{
    if pitches.is_empty()
    {
        return Vec::new();
    }

    let multiple_dicts = pitches.len() > 1;
    let mut pitch = key("<span class=\"memento-pitch\">");

    let ul_open = key("<ul>");
    let ul_close = key("</ul>");
    let ol_open = key("<ol>");
    let ol_close = key("</ol>");
    let li_styled = key("<li style=\"padding: 2px 0px 2px 0px;\">");
    let li_open = key("<li>");
    let li_close = key("</li>");
    let i_open = key("<i>");
    let i_close = key("</i>");
    let span_h_open = key("<span style=\"border-top: solid;\">");
    let span_hl_open = key("<span style=\"border-top: solid; border-right: solid;\">");
    let span_close = key("</span>");

    if multiple_dicts
    {
        pitch.extend_from_slice(&ul_open);
    }

    for p in pitches
    {
        let multiple_pitches = p.positions.len() > 1;

        if multiple_dicts
        {
            pitch.extend_from_slice(&li_open);
            pitch.extend_from_slice(&i_open);
            pitch.extend_from_slice(p.dictionary_name);
            pitch.extend_from_slice(&i_close);
        }
        if multiple_pitches
        {
            pitch.extend_from_slice(&ol_open);
        }

        for &pos in p.positions
        {
            if p.mora.is_empty()
            {
                continue;
            }

            if multiple_pitches
            {
                pitch.extend_from_slice(&li_styled);
            }

            match pos
            {
                0 =>
                {
                    pitch.extend_from_slice(p.mora[0]);
                    if p.mora.len() > 1
                    {
                        pitch.extend_from_slice(&span_h_open);
                        for m in &p.mora[1..]
                        {
                            pitch.extend_from_slice(m);
                        }
                        pitch.extend_from_slice(&span_close);
                    }
                }
                1 =>
                {
                    pitch.extend_from_slice(&span_hl_open);
                    pitch.extend_from_slice(p.mora[0]);
                    pitch.extend_from_slice(&span_close);
                    if p.mora.len() > 1
                    {
                        for m in &p.mora[1..]
                        {
                            pitch.extend_from_slice(m);
                        }
                    }
                }
                _ =>
                {
                    let pos_usize = pos as usize;
                    pitch.extend_from_slice(p.mora[0]);

                    let mut text = Vec::new();
                    let limit = pos_usize.min(p.mora.len());
                    for i in 1..limit
                    {
                        text.extend_from_slice(p.mora[i]);
                    }
                    if !text.is_empty()
                    {
                        pitch.extend_from_slice(&span_hl_open);
                        pitch.extend_from_slice(&text);
                        pitch.extend_from_slice(&span_close);
                    }

                    let mut text2 = Vec::new();
                    if pos_usize < p.mora.len()
                    {
                        for i in pos_usize..p.mora.len()
                        {
                            text2.extend_from_slice(p.mora[i]);
                        }
                    }
                    if !text2.is_empty()
                    {
                        pitch.extend_from_slice(&text2);
                    }
                }
            }

            if multiple_pitches
            {
                pitch.extend_from_slice(&li_close);
            }
        }

        if multiple_pitches
        {
            pitch.extend_from_slice(&ol_close);
        }
        if multiple_dicts
        {
            pitch.extend_from_slice(&li_close);
        }
    }

    if multiple_dicts
    {
        pitch.extend_from_slice(&ul_close);
    }
    pitch.extend_from_slice(&span_close);
    pitch
}

/// Matches `getPitchPositions` in `notebuilder.cpp`.
pub fn format_pitch_positions(pitches: &[PitchEntry<'_>]) -> Vec<u16>
{
    if pitches.is_empty()
    {
        return Vec::new();
    }

    let multiple_dicts = pitches.len() > 1;
    let mut pitch_pos = key("<span class=\"memento-pitch-position\">");

    let ul_open = key("<ul>");
    let ul_close = key("</ul>");
    let ol_open = key("<ol>");
    let ol_close = key("</ol>");
    let li_open = key("<li>");
    let li_close = key("</li>");
    let i_open = key("<i>");
    let i_close = key("</i>");
    let span_close = key("</span>");

    if multiple_dicts
    {
        pitch_pos.extend_from_slice(&ul_open);
    }

    for p in pitches
    {
        let multiple_pitches = p.positions.len() > 1;

        if multiple_dicts
        {
            pitch_pos.extend_from_slice(&li_open);
            pitch_pos.extend_from_slice(&i_open);
            pitch_pos.extend_from_slice(p.dictionary_name);
            pitch_pos.extend_from_slice(&i_close);
        }
        if multiple_pitches
        {
            pitch_pos.extend_from_slice(&ol_open);
        }

        for &pos in p.positions
        {
            if p.mora.is_empty()
            {
                continue;
            }

            if multiple_pitches
            {
                pitch_pos.extend_from_slice(&li_open);
            }

            pitch_pos.push(b'[' as u16);
            let s = pos.to_string();
            for b in s.bytes()
            {
                pitch_pos.push(b as u16);
            }
            pitch_pos.push(b']' as u16);

            if multiple_pitches
            {
                pitch_pos.extend_from_slice(&li_close);
            }
        }

        if multiple_pitches
        {
            pitch_pos.extend_from_slice(&ol_close);
        }
        if multiple_dicts
        {
            pitch_pos.extend_from_slice(&li_close);
        }
    }

    if multiple_dicts
    {
        pitch_pos.extend_from_slice(&ul_close);
    }
    pitch_pos.extend_from_slice(&span_close);
    pitch_pos
}

/// Matches `getSelection` in `notebuilder.cpp`.
pub fn format_selection(selections: &[&[u16]]) -> Vec<u16>
{
    if selections.is_empty()
    {
        return Vec::new();
    }
    if selections.len() == 1
    {
        return selections[0].to_vec();
    }
    let ul_open = key("<ul>");
    let ul_close = key("</ul>");
    let li_open = key("<li>");
    let li_close = key("</li>");

    let mut result = ul_open;
    for &item in selections
    {
        result.extend_from_slice(&li_open);
        result.extend_from_slice(item);
        result.extend_from_slice(&li_close);
    }
    result.extend_from_slice(&ul_close);
    result
}

/// Borrowed tag entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TagEntry<'a>
{
    pub name: &'a [u16],
    pub notes: &'a [u16],
}

/// Matches `getTags` and `getTagsBrief` in `notebuilder.cpp`.
pub fn format_tags(tags: &[TagEntry<'_>], brief: bool) -> Vec<u16>
{
    if tags.is_empty()
    {
        return Vec::new();
    }
    let ul_open = key("<ul>");
    let ul_close = key("</ul>");
    let li_open = key("<li>");
    let li_close = key("</li>");
    let colon_space = key(": ");

    let mut result = ul_open;
    for tag in tags
    {
        result.extend_from_slice(&li_open);
        result.extend_from_slice(tag.name);
        if !brief && !tag.notes.is_empty()
        {
            result.extend_from_slice(&colon_space);
            result.extend_from_slice(tag.notes);
        }
        result.extend_from_slice(&li_close);
    }
    result.extend_from_slice(&ul_close);
    result
}

/// Matches `getKunyomi` / `getOnyomi` in `notebuilder.cpp`.
pub fn format_kanji_readings(readings: &[&[u16]]) -> Vec<u16>
{
    if readings.is_empty()
    {
        return Vec::new();
    }
    let comma_space = key(", ");
    let mut result = Vec::new();
    for (i, &reading) in readings.iter().enumerate()
    {
        if i > 0
        {
            result.extend_from_slice(&comma_space);
        }
        result.extend_from_slice(reading);
    }
    result
}

/// Borrowed kanji definition entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KanjiDefEntry<'a>
{
    pub dictionary_name: &'a [u16],
    pub tags: &'a [&'a [u16]],
    pub glossary: &'a [&'a [u16]],
}

/// Matches `getGlossary(const QList<KanjiDefinition *> &)` in `notebuilder.cpp`.
pub fn format_kanji_glossary(defs: &[KanjiDefEntry<'_>]) -> Vec<u16>
{
    if defs.is_empty()
    {
        return Vec::new();
    }
    let ol_open = key("<ol>");
    let ol_close = key("</ol>");
    let li_i_open = key("<li><i>(");
    let i_li_close = key(")</i></li>");
    let li_open = key("<li>");
    let li_close = key("</li>");
    let comma_space = key(", ");

    let mut glossary = ol_open.clone();
    for def in defs
    {
        if def.glossary.is_empty()
        {
            continue;
        }
        glossary.extend_from_slice(&li_i_open);
        for tag in def.tags
        {
            glossary.extend_from_slice(tag);
            glossary.extend_from_slice(&comma_space);
        }
        glossary.extend_from_slice(def.dictionary_name);
        glossary.extend_from_slice(&i_li_close);
        glossary.extend_from_slice(&ol_open);
        for glos in def.glossary
        {
            glossary.extend_from_slice(&li_open);
            glossary.extend_from_slice(glos);
            glossary.extend_from_slice(&li_close);
        }
        glossary.extend_from_slice(&ol_close);
    }
    glossary.extend_from_slice(&ol_close);

    let empty_ol = key("<ol></ol>");
    if glossary == empty_ol
    {
        glossary.clear();
    }
    glossary
}

/// Matches `createAudio` URL and filename formatting in `notebuilder.cpp`.
pub fn format_audio_url_and_filename(
    url: &[u16],
    source_name: &[u16],
    expression: &[u16],
    reading: &[u16],
    reading_as_expression: bool,
) -> (Vec<u16>, Vec<u16>)
{
    let expression_val = if reading_as_expression
    {
        reading
    }
    else
    {
        expression
    };
    let reading_val = if reading.is_empty() || reading_as_expression
    {
        expression_val
    }
    else
    {
        reading
    };

    let mut formatted_url = url.to_vec();
    let replace_expression = key("{expression}");
    let replace_reading = key("{reading}");
    replace_all(&mut formatted_url, &replace_expression, expression_val);
    replace_all(&mut formatted_url, &replace_reading, reading_val);

    let mut filename = key("memento_");
    filename.extend_from_slice(source_name);
    filename.push(b'_' as u16);
    filename.extend_from_slice(reading);
    filename.push(b'_' as u16);
    filename.extend_from_slice(expression);
    filename.extend_from_slice(&key(".mp3"));

    for ch in &mut filename
    {
        if *ch == b' ' as u16
        {
            *ch = b'_' as u16;
        }
    }

    (formatted_url, filename)
}

/// Borrowed pitch graph entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PitchGraphEntry<'a>
{
    pub dictionary_name: &'a [u16],
    pub positions_count: usize,
    pub svg_graphs: &'a [&'a [u16]],
}

/// Matches `getPitchGraphs` in `notebuilder.cpp`.
pub fn format_pitch_graphs(pitches: &[PitchGraphEntry<'_>]) -> Vec<u16>
{
    if pitches.is_empty()
    {
        return Vec::new();
    }

    let multiple_dicts = pitches.len() > 1;
    let mut pitch_graph = key("<span class=\"memento-pitch-graph\">");

    let ul_open = key("<ul>");
    let ul_close = key("</ul>");
    let ol_open = key("<ol>");
    let ol_close = key("</ol>");
    let li_open = key("<li>");
    let li_close = key("</li>");
    let li_i_open = key("<li><i>");
    let i_close = key("</i>");
    let span_close = key("</span>");

    if multiple_dicts
    {
        pitch_graph.extend_from_slice(&ul_open);
    }

    for p in pitches
    {
        let multiple_pitches = p.positions_count > 1;

        if multiple_dicts
        {
            pitch_graph.extend_from_slice(&li_i_open);
            pitch_graph.extend_from_slice(p.dictionary_name);
            pitch_graph.extend_from_slice(&i_close);
        }
        if multiple_pitches
        {
            pitch_graph.extend_from_slice(&ol_open);
        }

        for &svg in p.svg_graphs
        {
            if multiple_pitches
            {
                pitch_graph.extend_from_slice(&li_open);
            }
            pitch_graph.extend_from_slice(svg);
            if multiple_pitches
            {
                pitch_graph.extend_from_slice(&li_close);
            }
        }

        if multiple_pitches
        {
            pitch_graph.extend_from_slice(&ol_close);
        }
        if multiple_dicts
        {
            pitch_graph.extend_from_slice(&li_close);
        }
    }

    if multiple_dicts
    {
        pitch_graph.extend_from_slice(&ul_close);
    }
    pitch_graph.extend_from_slice(&span_close);
    pitch_graph
}

/// Pitch accent category classification.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum PitchCategory
{
    Heiban = 1,
    Kifuku = 2,
    Atamadaka = 3,
    Odaka = 4,
    Nakadaka = 5,
}

impl PitchCategory
{
    pub const fn as_str(&self) -> &'static str
    {
        match self
        {
            PitchCategory::Heiban => "heiban",
            PitchCategory::Kifuku => "kifuku",
            PitchCategory::Atamadaka => "atamadaka",
            PitchCategory::Odaka => "odaka",
            PitchCategory::Nakadaka => "nakadaka",
        }
    }
}

/// Matches classification inside `getPitchCategories` in `notebuilder.cpp`.
pub fn classify_pitch(mora_len: usize, pos: u8, can_be_kifuku: bool) -> Option<PitchCategory>
{
    if mora_len == 0
    {
        return None;
    }
    match pos
    {
        0 => Some(PitchCategory::Heiban),
        1 => Some(if can_be_kifuku { PitchCategory::Kifuku } else { PitchCategory::Atamadaka }),
        _ =>
        {
            if mora_len == pos as usize
            {
                Some(PitchCategory::Odaka)
            }
            else if can_be_kifuku
            {
                Some(PitchCategory::Kifuku)
            }
            else
            {
                Some(PitchCategory::Nakadaka)
            }
        }
    }
}

/// Input entry for pitch category classification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PitchCategoryInput<'a>
{
    pub mora_len: usize,
    pub positions: &'a [u8],
}

/// Matches `getPitchCategories` classification in `notebuilder.cpp`.
/// Returns unique categories in encounter order.
pub fn classify_pitch_categories(
    pitches: &[PitchCategoryInput<'_>],
    can_be_kifuku: bool,
) -> Vec<PitchCategory>
{
    let mut categories = Vec::with_capacity(5);
    for p in pitches
    {
        for &pos in p.positions
        {
            if let Some(cat) = classify_pitch(p.mora_len, pos, can_be_kifuku)
            {
                if !categories.contains(&cat)
                {
                    categories.push(cat);
                }
            }
        }
    }
    categories
}

/// Matches `formatPitchCategories` in `notebuilder.cpp`:
/// Formats unique pitch categories as comma-separated string.
pub fn format_pitch_categories(
    pitches: &[PitchCategoryInput<'_>],
    can_be_kifuku: bool,
) -> Vec<u16>
{
    let categories = classify_pitch_categories(pitches, can_be_kifuku);
    if categories.is_empty()
    {
        return Vec::new();
    }
    let mut result = Vec::new();
    for (i, cat) in categories.iter().enumerate()
    {
        if i > 0
        {
            result.push(b',' as u16);
        }
        for b in cat.as_str().as_bytes()
        {
            result.push(*b as u16);
        }
    }
    result
}

/// Borrowed stat tag entry for stroke count lookup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StrokeCountStat<'a>
{
    pub notes: &'a [u16],
    pub value: &'a [u16],
}

/// Matches `getStrokeCount` in `notebuilder.cpp`.
pub fn find_stroke_count<'a>(stats: &[StrokeCountStat<'a>]) -> Option<&'a [u16]>
{
    const STROKE_COUNT: &[u16] = &[
        b'S' as u16, b't' as u16, b'r' as u16, b'o' as u16, b'k' as u16, b'e' as u16,
        b' ' as u16,
        b'c' as u16, b'o' as u16, b'u' as u16, b'n' as u16, b't' as u16,
    ];
    stats.iter()
        .find(|stat| stat.notes == STROKE_COUNT)
        .map(|stat| stat.value)
}

/// Borrowed definition entry for term glossary formatting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TermGlossaryDef<'a>
{
    pub dict_id: i64,
    pub dict_name: &'a [u16],
    pub tags: &'a [&'a [u16]],
    pub items: &'a [&'a [u16]],
    pub has_styles: bool,
    pub stylesheet: &'a [u16],
}

/// Output HTML for term glossary markers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TermGlossaryOutput
{
    pub glossary: Vec<u16>,
    pub glossary_brief: Vec<u16>,
    pub glossary_compact: Vec<u16>,
}

/// Matches `getGlossary(const QList<TermDefinition *> &)` in `notebuilder.cpp`.
pub fn format_term_glossary(defs: &[TermGlossaryDef<'_>]) -> TermGlossaryOutput
{
    if defs.is_empty()
    {
        return TermGlossaryOutput::default();
    }

    let glossary_open = key("<div class=\"yomitan-glossary memento-glossary\"><ol>");
    let glossary_close = key(
        "<style>.memento-glossary {text-align: left;}.memento-glossary img {display: inline-block;}.memento-glossary table {table-layout: auto;border-collapse: collapse;}.memento-glossary th {font-weight: bold;border-style: solid;padding: 0.25em;vertical-align: top;border-width: 1px;border-color: currentColor;}.memento-glossary td {border-style: solid;padding: 0.25em;vertical-align: top;border-width: 1px;border-color: currentColor;}</style></ol></div>"
    );

    let mut glossary = glossary_open.clone();
    let mut glossary_brief = glossary_open.clone();
    let mut glossary_compact = glossary_open;

    let li_dict_open = key("<li data-dictionary=\"");
    let quote_close = key("\"><i>(");
    let comma_space = key(", ");
    let i_span_open = key(")</i> <span>");
    let li_open = key("<li>");
    let li_close = key("</li>");
    let span_li_close = key("</span></li>");
    let pipe_sep = key(" | ");
    let style_open = key("<style>");
    let style_close = key("</style>");

    let mut dictionaries_with_styles: Vec<i64> = Vec::new();

    for def in defs
    {
        for target in [&mut glossary, &mut glossary_compact]
        {
            target.extend_from_slice(&li_dict_open);
            target.extend_from_slice(def.dict_name);
            target.extend_from_slice(&quote_close);
            for tag in def.tags
            {
                target.extend_from_slice(tag);
                target.extend_from_slice(&comma_space);
            }
            target.extend_from_slice(def.dict_name);
            target.extend_from_slice(&i_span_open);
        }

        for (i, item) in def.items.iter().enumerate()
        {
            glossary.extend_from_slice(item);

            glossary_brief.extend_from_slice(&li_open);
            glossary_brief.extend_from_slice(item);
            glossary_brief.extend_from_slice(&li_close);

            if i > 0
            {
                glossary_compact.extend_from_slice(&pipe_sep);
            }
            glossary_compact.extend_from_slice(item);
        }

        glossary.extend_from_slice(&span_li_close);
        glossary_compact.extend_from_slice(&span_li_close);

        if def.has_styles && !dictionaries_with_styles.contains(&def.dict_id)
        {
            dictionaries_with_styles.push(def.dict_id);
            for target in [&mut glossary, &mut glossary_brief, &mut glossary_compact]
            {
                target.extend_from_slice(&style_open);
                target.extend_from_slice(def.stylesheet);
                target.extend_from_slice(&style_close);
            }
        }
    }

    glossary.extend_from_slice(&glossary_close);
    glossary_brief.extend_from_slice(&glossary_close);
    glossary_compact.extend_from_slice(&glossary_close);

    TermGlossaryOutput {
        glossary,
        glossary_brief,
        glossary_compact,
    }
}

/// Timing source for media clips.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum TimingSource
{
    Subtitle = 0,
    Context = 1,
}

/// Parameters used to group `{audio-media}` fields that share one clip.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioMediaParams
{
    pub source: TimingSource,
    pub normalize: bool,
    pub db: f64,
}

/// Parameters used to group screenshot fields that share one captured image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenshotParams
{
    pub max_width: i32,
    pub max_height: i32,
    pub keep_aspect_ratio: bool,
}

/// Parameters used to group `{video}` fields that share one video clip.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VideoParams
{
    pub source: TimingSource,
    pub audio: bool,
    pub subtitles: bool,
    pub normalize: bool,
    pub db: f64,
}

fn marker_arg<'a>(marker: &'a Marker, key: &str) -> Option<&'a [u16]>
{
    marker.args.iter().find_map(|(arg_key, value)| {
        let matches = arg_key.len() == key.len()
            && arg_key
                .iter()
                .zip(key.bytes())
                .all(|(&unit, byte)| unit == byte as u16);
        matches.then_some(value.as_slice())
    })
}

fn equals_ascii(value: &[u16], expected: &str) -> bool
{
    value.len() == expected.len()
        && value
            .iter()
            .zip(expected.bytes())
            .all(|(&unit, byte)| unit == byte as u16)
}

fn equals_ascii_ignore_case(value: &[u16], expected: &str) -> bool
{
    value.len() == expected.len()
        && value
            .iter()
            .zip(expected.bytes())
            .all(|(&unit, byte)| {
                let lower = if (b'A' as u16..=b'Z' as u16).contains(&unit) {
                    unit + (b'a' - b'A') as u16
                } else {
                    unit
                };
                lower == byte.to_ascii_lowercase() as u16
            })
}

fn parse_timing_source(value: &[u16]) -> Option<TimingSource>
{
    if equals_ascii(value, "subtitle")
    {
        Some(TimingSource::Subtitle)
    }
    else if equals_ascii(value, "context")
    {
        Some(TimingSource::Context)
    }
    else
    {
        None
    }
}

/// `QVariant(QString).toBool()`: every nonempty string is true except `0` and
/// case-insensitive `false`.
fn marker_bool(value: &[u16]) -> bool
{
    !value.is_empty()
        && !equals_ascii(value, "0")
        && !equals_ascii_ignore_case(value, "false")
}

fn is_qchar_space(unit: u16) -> bool
{
    matches!(
        unit,
        0x0009..=0x000d
            | 0x0020
            | 0x00a0
            | 0x1680
            | 0x2000..=0x200a
            | 0x2028..=0x2029
            | 0x202f
            | 0x205f
            | 0x3000
    )
}

fn trim_qstring(value: &[u16]) -> &[u16]
{
    let start = value
        .iter()
        .position(|&unit| !is_qchar_space(unit))
        .unwrap_or(value.len());
    let end = value
        .iter()
        .rposition(|&unit| !is_qchar_space(unit))
        .map_or(start, |index| index + 1);
    &value[start..end]
}

fn parse_marker_i32(value: &[u16]) -> Option<i32>
{
    let value = trim_qstring(value);
    let (negative, digits) = match value.first().copied()
    {
        Some(0x002b) => (false, &value[1..]),
        Some(0x002d) => (true, &value[1..]),
        _ => (false, value),
    };
    if digits.is_empty()
    {
        return None;
    }

    let limit = if negative {
        i32::MAX as u64 + 1
    } else {
        i32::MAX as u64
    };
    let mut magnitude = 0u64;
    for &unit in digits
    {
        if !(b'0' as u16..=b'9' as u16).contains(&unit)
        {
            return None;
        }
        magnitude = magnitude
            .checked_mul(10)?
            .checked_add((unit - b'0' as u16) as u64)?;

        if magnitude > limit
        {
            return None;
        }
    }

    if negative
    {
        if magnitude == i32::MAX as u64 + 1
        {
            Some(i32::MIN)
        }
        else
        {
            Some(-(magnitude as i32))
        }
    }
    else
    {
        Some(magnitude as i32)
    }
}

fn parse_marker_f64(value: &[u16]) -> Option<f64>
{
    String::from_utf16(trim_qstring(value)).ok()?.parse().ok()
}

fn marker_source(marker: &Marker) -> Option<TimingSource>
{
    marker_arg(marker, "source")
        .map(parse_timing_source)
        .unwrap_or(Some(TimingSource::Subtitle))
}

fn marker_db(marker: &Marker, default: f64) -> Option<f64>
{
    marker_arg(marker, "db")
        .map(parse_marker_f64)
        .unwrap_or(Some(default))
}

/// Parses the `{audio-media}` marker's arguments, using profile defaults when
/// `norm` or `db` is omitted. Invalid `source` or `db` leaves the marker
/// unhandled, matching `getAudioMediaParams` in `notebuilder.cpp`.
pub fn audio_media_params(
    marker: &Marker,
    profile_normalize: bool,
    profile_db: f64,
) -> Option<AudioMediaParams>
{
    Some(AudioMediaParams {
        source: marker_source(marker)?,
        normalize: marker_arg(marker, "norm")
            .map(marker_bool)
            .unwrap_or(profile_normalize),
        db: marker_db(marker, profile_db)?,
    })
}

/// Parses `{audio-context}` arguments before forcing context timing. An
/// explicitly invalid `source` remains invalid, as in the C++ call order.
pub fn audio_context_params(
    marker: &Marker,
    profile_normalize: bool,
    profile_db: f64,
) -> Option<AudioMediaParams>
{
    let mut params = audio_media_params(marker, profile_normalize, profile_db)?;
    params.source = TimingSource::Context;
    Some(params)
}

/// Parses the `{screenshot}` and `{screenshot-video}` sizing arguments.
pub fn screenshot_params(marker: &Marker) -> Option<ScreenshotParams>
{
    Some(ScreenshotParams {
        max_width: marker_arg(marker, "max-width")
            .map(parse_marker_i32)
            .unwrap_or(Some(-1))?,
        max_height: marker_arg(marker, "max-height")
            .map(parse_marker_i32)
            .unwrap_or(Some(-1))?,
        keep_aspect_ratio: marker_arg(marker, "keep-ratio")
            .map(marker_bool)
            .unwrap_or(true),
    })
}

/// Parses the `{video}` marker's arguments, using profile normalization
/// defaults when `norm` or `db` is omitted.
pub fn video_params(
    marker: &Marker,
    profile_normalize: bool,
    profile_db: f64,
) -> Option<VideoParams>
{
    Some(VideoParams {
        source: marker_source(marker)?,
        audio: marker_arg(marker, "audio")
            .map(marker_bool)
            .unwrap_or(true),
        subtitles: marker_arg(marker, "subtitles")
            .map(marker_bool)
            .unwrap_or(true),
        normalize: marker_arg(marker, "norm")
            .map(marker_bool)
            .unwrap_or(profile_normalize),
        db: marker_db(marker, profile_db)?,
    })
}

/// Matches `createAudioMediaHelper` / `createVideoHelper` timing computation in `notebuilder.cpp`.
pub fn compute_clip_range(
    source: TimingSource,
    sub_start: f64,
    sub_end: f64,
    ctx_start: f64,
    ctx_end: f64,
    pad_start: f64,
    pad_end: f64,
) -> Option<(f64, f64)>
{
    let (raw_start, raw_end) = match source
    {
        TimingSource::Subtitle => (sub_start - pad_start, sub_end + pad_end),
        TimingSource::Context => (ctx_start - pad_start, ctx_end + pad_end),
    };
    // std::max(raw, 0.0) in C++ keeps an unordered operand (NaN), whereas
    // f64::max discards it and could turn an invalid start into zero.
    let start = if raw_start < 0.0 { 0.0 } else { raw_start };
    let end = if raw_end < 0.0 { 0.0 } else { raw_end };
    if start < end
    {
        Some((start, end))
    }
    else
    {
        None
    }
}

#[cfg(test)]
mod tests
{
    use super::*;
    use std::cell::RefCell;

    fn u(text: &str) -> Vec<u16>
    {
        text.encode_utf16().collect()
    }

    fn parse_marker(template: &str) -> Marker
    {
        tokenizer::tokenize(&u(template))
            .into_iter()
            .next()
            .unwrap()
            .markers
            .into_iter()
            .next()
            .unwrap()
    }

    fn text(value: &str) -> MarkerResult
    {
        MarkerResult { handled: true, text: u(value), ..Default::default() }
    }

    #[test]
    fn newlines_are_literal_and_replacement_is_not_recursive()
    {
        assert_eq!(replace_newlines(&u("A\r\nB\nC\u{2028}"), &u("<br>\n")), u("A\r<br>\nB<br>\nC\u{2028}"));
        assert_eq!(replace_newlines(&u("a\nb"), &[]), u("ab"));
    }

    #[test]
    fn alternatives_preserve_empty_fallback_and_unknown_token()
    {
        let calls = RefCell::new(Vec::new());
        let result = substitute_markers(
            &u("[{unknown}] {blank|missing} {blank|filled} {missing|filled}"),
            |marker|
            {
                calls.borrow_mut().push(format!("common:{}", String::from_utf16_lossy(&marker.name)));
                if marker.name == u("blank") { MarkerResult { handled: true, ..Default::default() } }
                else { MarkerResult::default() }
            },
            |marker|
            {
                calls.borrow_mut().push(format!("specific:{}", String::from_utf16_lossy(&marker.name)));
                if marker.name == u("filled") { text("OK") } else { MarkerResult::default() }
            },
        );
        assert_eq!(result, u("[{unknown}]  OK OK"));
        assert_eq!(calls.into_inner(), [
            "common:unknown", "specific:unknown", "common:blank", "common:missing",
            "specific:missing", "common:blank", "common:filled", "specific:filled",
            "common:missing", "specific:missing", "common:filled", "specific:filled",
        ]);
    }

    #[test]
    fn media_short_circuits_and_removes_raw_marker()
    {
        let result = substitute_markers(
            &u("before {audio|expression} after"),
            |marker| if marker.name == u("audio") {
                MarkerResult { handled: true, media: true, ..Default::default() }
            } else { panic!("media did not short circuit") },
            |_| panic!("specific handler should not run"),
        );
        assert_eq!(result, u("before  after"));
    }

    #[test]
    fn replacements_are_global_and_cascade_between_original_tokens()
    {
        let result = substitute_markers(
            &u("{a} / {a} / {b}"),
            |_| MarkerResult::default(),
            |marker| if marker.name == u("a") { text("{b}") } else { text("x") },
        );
        assert_eq!(result, u("x / x / x"));
        // After the first token's replacement, the second ORIGINAL {a}
        // token is still processed. It can replace newly inserted {a} text.
        let result = substitute_markers(
            &u("{a} {a}"),
            |_| MarkerResult::default(),
            |_| text("{a}x"),
        );
        assert_eq!(result, u("{a}xx {a}xx"));
    }

    #[test]
    fn utf16_code_units_survive_even_without_tokens()
    {
        // PCRE2/Qt yields no tokens if the original field contains a lone
        // surrogate. We still preserve the QString's raw contents.
        let input = vec![0xD800, b'{' as u16, b'a' as u16, b'}' as u16];
        assert_eq!(substitute_markers(&input, |_| panic!("no match"), |_| panic!("no match")), input);
    }

    #[test]
    fn metadata_has_exact_qjson_field_and_policy_shapes()
    {
        for (policy, expected_key, expected_value) in [
            (DuplicatePolicy::None, "allowDuplicate", JsonValue::Bool(false)),
            (DuplicatePolicy::DifferentDeck, "duplicateScope", JsonValue::String(u("deck"))),
            (DuplicatePolicy::SameDeck, "allowDuplicate", JsonValue::Bool(true)),
        ]
        {
            let mut fields = JsonObject::new();
            fields.insert(u("Front"), JsonValue::String(u("old")));
            fields.insert(u("Front"), JsonValue::String(u("<new>")));
            let note = build_note_metadata(
                u("deck\u{1F600}"), u("model"), vec![u("a"), u("a")],
                JsonValue::Object(fields), policy,
            );
            assert_eq!(note.len(), 5);
            assert_eq!(note.get(&u("deckName")), Some(&JsonValue::String(u("deck\u{1F600}"))));
            assert_eq!(note.get(&u("modelName")), Some(&JsonValue::String(u("model"))));
            assert_eq!(note.get(&u("tags")), Some(&JsonValue::Array(vec![
                JsonValue::String(u("a")), JsonValue::String(u("a")),
            ])));
            let Some(JsonValue::Object(fields)) = note.get(&u("fields")) else { panic!("fields object") };
            assert_eq!(fields.len(), 1);
            assert_eq!(fields.get(&u("Front")), Some(&JsonValue::String(u("<new>"))));
            let Some(JsonValue::Object(options)) = note.get(&u("options")) else { panic!("options object") };
            assert_eq!(options.len(), 1);
            assert_eq!(options.get(&u(expected_key)), Some(&expected_value));
        }
    }

    #[test]
    fn parse_frequency_value_matches_qt_regex_and_stars()
    {
        assert_eq!(parse_frequency_value(&u("123.45Hz")), Some(123.45));
        assert_eq!(parse_frequency_value(&u("+0.5")), Some(0.5));
        assert_eq!(parse_frequency_value(&u(".25")), Some(0.25));
        assert_eq!(parse_frequency_value(&u("-.5")), Some(-0.5));
        assert_eq!(parse_frequency_value(&u("12.")), Some(12.0));
        assert_eq!(parse_frequency_value(&u("★")), Some(20.0));
        assert_eq!(parse_frequency_value(&u("★★")), Some(40.0));
        assert_eq!(parse_frequency_value(&u("★★★")), Some(60.0));
        assert_eq!(parse_frequency_value(&u("★★★★")), Some(80.0));
        assert_eq!(parse_frequency_value(&u("★★★★★")), Some(100.0));

        // Rejections:
        assert_eq!(parse_frequency_value(&u("★★★★★★")), None);
        assert_eq!(parse_frequency_value(&u("１２３")), None);
        assert_eq!(parse_frequency_value(&u("")), None);
        assert_eq!(parse_frequency_value(&u("+")), None);
        assert_eq!(parse_frequency_value(&u(".")), None);
        assert_eq!(parse_frequency_value(&u("+.")), None);
        assert_eq!(parse_frequency_value(&u("abc")), None);
        assert_eq!(parse_frequency_value(&u("★ ")), None);
        assert_eq!(parse_frequency_value(&u(" ★")), None);
    }

    #[test]
    fn frequency_stats_aggregates_and_skips_duplicates_and_empty_dicts()
    {
        let d_empty = u("");
        let d_a = u("DictA");
        let d_b = u("DictB");
        let f_zero = u("0.0");
        let f_neg = u("-5.0");
        let f_3 = u("3");
        let f_5 = u("5");
        let f_10 = u("10.5");
        let f_20 = u("20.0");

        // Empty dictionary first entry is skipped
        let entries = [
            FrequencyEntry { dictionary: &d_empty, freq: &f_10 },
            FrequencyEntry { dictionary: &d_a, freq: &f_20 },
        ];
        let (numbers, harmonic, average) = frequency_stats(&entries);
        assert_eq!(numbers, [20.0]);
        assert_eq!(harmonic, Some(20));
        assert_eq!(average, Some(20));

        // Non-positive frequencies do not update previousDictionary
        let entries2 = [
            FrequencyEntry { dictionary: &d_a, freq: &f_zero },
            FrequencyEntry { dictionary: &d_a, freq: &f_neg },
            FrequencyEntry { dictionary: &d_a, freq: &f_10 },
            FrequencyEntry { dictionary: &d_a, freq: &f_20 }, // skipped because d_a was set by f_10
        ];
        let (numbers2, _, _) = frequency_stats(&entries2);
        assert_eq!(numbers2, [10.5]);

        // Multiple dictionaries and harmonic vs average flooring
        // 3 and 5: harmonic = floor(2 / (1/3 + 1/5)) = floor(3.75) = 3.
        // average = floor((3 + 5) / 2) = floor(4.0) = 4.
        let entries3 = [
            FrequencyEntry { dictionary: &d_a, freq: &f_3 },
            FrequencyEntry { dictionary: &d_b, freq: &f_5 },
        ];
        let (numbers3, harmonic3, average3) = frequency_stats(&entries3);
        assert_eq!(numbers3, [3.0, 5.0]);
        assert_eq!(harmonic3, Some(3));
        assert_eq!(average3, Some(4));

        // Empty entries
        let (numbers4, harmonic4, average4) = frequency_stats(&[]);
        assert!(numbers4.is_empty());
        assert_eq!(harmonic4, None);
        assert_eq!(average4, None);
    }

    #[test]
    fn test_is_kifuku_applicable()
    {
        let v1 = u("v1");
        let v5 = u("v5");
        let vk = u("vk");
        let vz = u("vz");
        let adj_i = u("adj-i");
        let vs = u("vs");
        let n = u("n");

        assert!(!is_kifuku_applicable(&[]));
        assert!(is_kifuku_applicable(&[&v1]));
        assert!(is_kifuku_applicable(&[&v5]));
        assert!(is_kifuku_applicable(&[&vk]));
        assert!(is_kifuku_applicable(&[&vz]));
        assert!(is_kifuku_applicable(&[&adj_i]));
        assert!(!is_kifuku_applicable(&[&n]));
        assert!(!is_kifuku_applicable(&[&vs]));
        assert!(!is_kifuku_applicable(&[&v1, &vs]));
        assert!(!is_kifuku_applicable(&[&vs, &v1]));
        assert!(is_kifuku_applicable(&[&n, &v5]));
    }

    #[test]
    fn test_format_furigana()
    {
        let surface1 = u("日");
        let reading1 = u("に");
        let surface2 = u("本");
        let reading2 = u("ほん");
        let reading_empty = u("");

        let pairs = [
            FuriganaPair { surface: &surface1, reading: &reading1 },
            FuriganaPair { surface: &surface2, reading: &reading2 },
        ];
        assert_eq!(
            format_furigana(&pairs, false),
            u("<ruby>日<rt>に</rt></ruby><ruby>本<rt>ほん</rt></ruby>")
        );
        assert_eq!(
            format_furigana(&pairs, true),
            u("日[に]本[ほん]")
        );

        let pairs_with_plain = [
            FuriganaPair { surface: &surface1, reading: &reading_empty },
            FuriganaPair { surface: &surface2, reading: &reading2 },
        ];
        assert_eq!(
            format_furigana(&pairs_with_plain, false),
            u("日<ruby>本<rt>ほん</rt></ruby>")
        );
        assert_eq!(
            format_furigana(&pairs_with_plain, true),
            u("日本[ほん]")
        );

        assert_eq!(format_furigana(&[], false), u(""));
        assert_eq!(format_furigana(&[], true), u(""));
    }

    #[test]
    fn test_build_frequencies_html()
    {
        let d1 = u("Dict1");
        let d2 = u("Dict2:");
        let f1 = u("100");
        let f2 = u("50");
        let f_invalid = u("invalid");

        let entries = [
            FrequencyEntry { dictionary: &d1, freq: &f1 },
            FrequencyEntry { dictionary: &d2, freq: &f2 },
        ];

        // Normal HTML list
        assert_eq!(
            build_frequencies_html(&entries, false, false),
            u("<ul><li>Dict1: 100</li><li>Dict2: 50</li></ul>")
        );
        // Value only
        assert_eq!(
            build_frequencies_html(&entries, true, false),
            u("100<br>50<br>")
        );
        // Min value only (not value only)
        assert_eq!(
            build_frequencies_html(&entries, false, true),
            u("<ul><li>Dict2: 50</li></ul>")
        );
        // Min value only and value only
        assert_eq!(
            build_frequencies_html(&entries, true, true),
            u("50")
        );

        // Empty
        assert_eq!(build_frequencies_html(&[], false, false), u(""));

        // No valid freq with min_value_only
        let entries_invalid = [
            FrequencyEntry { dictionary: &d1, freq: &f_invalid },
        ];
        assert_eq!(
            build_frequencies_html(&entries_invalid, false, true),
            u("<ul><li></li></ul>")
        );
        assert_eq!(
            build_frequencies_html(&entries_invalid, true, true),
            u("")
        );
    }

    #[test]
    fn test_format_pitch_and_positions()
    {
        let d1 = u("DictA");
        let d2 = u("DictB");
        let m1 = u("に");
        let m2 = u("ほ");
        let m3 = u("ん");

        // Single dict, single pitch, pos 0 (heiban)
        let mora = [&m1[..], &m2[..], &m3[..]];
        let pos0 = [0u8];
        let p_heiban = [
            PitchEntry { dictionary_name: &d1, positions: &pos0, mora: &mora },
        ];
        assert_eq!(
            format_pitch(&p_heiban),
            u("<span class=\"memento-pitch\">に<span style=\"border-top: solid;\">ほん</span></span>")
        );
        assert_eq!(
            format_pitch_positions(&p_heiban),
            u("<span class=\"memento-pitch-position\">[0]</span>")
        );

        // Pos 1 (atamadaka)
        let pos1 = [1u8];
        let p_atama = [
            PitchEntry { dictionary_name: &d1, positions: &pos1, mora: &mora },
        ];
        assert_eq!(
            format_pitch(&p_atama),
            u("<span class=\"memento-pitch\"><span style=\"border-top: solid; border-right: solid;\">に</span>ほん</span>")
        );
        assert_eq!(
            format_pitch_positions(&p_atama),
            u("<span class=\"memento-pitch-position\">[1]</span>")
        );

        // Pos 2 (nakadaka)
        let pos2 = [2u8];
        let p_naka = [
            PitchEntry { dictionary_name: &d1, positions: &pos2, mora: &mora },
        ];
        assert_eq!(
            format_pitch(&p_naka),
            u("<span class=\"memento-pitch\">に<span style=\"border-top: solid; border-right: solid;\">ほ</span>ん</span>")
        );

        // Multiple pitches in single dict -> <ol>
        let pos_multi = [0u8, 2u8];
        let p_multi_pos = [
            PitchEntry { dictionary_name: &d1, positions: &pos_multi, mora: &mora },
        ];
        assert_eq!(
            format_pitch(&p_multi_pos),
            u("<span class=\"memento-pitch\"><ol><li style=\"padding: 2px 0px 2px 0px;\">に<span style=\"border-top: solid;\">ほん</span></li><li style=\"padding: 2px 0px 2px 0px;\">に<span style=\"border-top: solid; border-right: solid;\">ほ</span>ん</li></ol></span>")
        );
        assert_eq!(
            format_pitch_positions(&p_multi_pos),
            u("<span class=\"memento-pitch-position\"><ol><li>[0]</li><li>[2]</li></ol></span>")
        );

        // Multiple dicts -> <ul>
        let p_multi_dicts = [
            PitchEntry { dictionary_name: &d1, positions: &pos0, mora: &mora },
            PitchEntry { dictionary_name: &d2, positions: &pos1, mora: &mora },
        ];
        assert_eq!(
            format_pitch(&p_multi_dicts),
            u("<span class=\"memento-pitch\"><ul><li><i>DictA</i>に<span style=\"border-top: solid;\">ほん</span></li><li><i>DictB</i><span style=\"border-top: solid; border-right: solid;\">に</span>ほん</li></ul></span>")
        );
        assert_eq!(
            format_pitch_positions(&p_multi_dicts),
            u("<span class=\"memento-pitch-position\"><ul><li><i>DictA</i>[0]</li><li><i>DictB</i>[1]</li></ul></span>")
        );

        // Empty
        assert_eq!(format_pitch(&[]), u(""));
        assert_eq!(format_pitch_positions(&[]), u(""));
    }

    #[test]
    fn test_format_selection()
    {
        let s1 = u("first");
        let s2 = u("second");
        assert_eq!(format_selection(&[]), u(""));
        assert_eq!(format_selection(&[&s1]), u("first"));
        assert_eq!(
            format_selection(&[&s1, &s2]),
            u("<ul><li>first</li><li>second</li></ul>")
        );
    }

    #[test]
    fn test_format_tags()
    {
        let t1_name = u("JLPT N5");
        let t1_notes = u("common");
        let t2_name = u("archaic");
        let t2_notes = u("");

        let tags = [
            TagEntry { name: &t1_name, notes: &t1_notes },
            TagEntry { name: &t2_name, notes: &t2_notes },
        ];

        assert_eq!(
            format_tags(&tags, false),
            u("<ul><li>JLPT N5: common</li><li>archaic</li></ul>")
        );
        assert_eq!(
            format_tags(&tags, true),
            u("<ul><li>JLPT N5</li><li>archaic</li></ul>")
        );
        assert_eq!(format_tags(&[], false), u(""));
    }

    #[test]
    fn test_format_kanji_readings()
    {
        let r1 = u("ひと");
        let r2 = u("ひと.つ");
        assert_eq!(format_kanji_readings(&[]), u(""));
        assert_eq!(format_kanji_readings(&[&r1]), u("ひと"));
        assert_eq!(format_kanji_readings(&[&r1, &r2]), u("ひと, ひと.つ"));
    }

    #[test]
    fn test_format_kanji_glossary()
    {
        let d1 = u("Kanjidic");
        let t1 = u("N5");
        let g1 = u("one");
        let g2 = u("1");

        let tags1 = [&t1[..]];
        let glos1 = [&g1[..], &g2[..]];
        let defs = [
            KanjiDefEntry { dictionary_name: &d1, tags: &tags1, glossary: &glos1 },
        ];

        assert_eq!(
            format_kanji_glossary(&defs),
            u("<ol><li><i>(N5, Kanjidic)</i></li><ol><li>one</li><li>1</li></ol></ol>")
        );

        // Empty glossary clears <ol></ol>
        let empty_glos: [&[u16]; 0] = [];
        let defs_empty_glos = [
            KanjiDefEntry { dictionary_name: &d1, tags: &tags1, glossary: &empty_glos },
        ];
        assert_eq!(format_kanji_glossary(&defs_empty_glos), u(""));
        assert_eq!(format_kanji_glossary(&[]), u(""));
    }

    #[test]
    fn test_format_audio_url_and_filename()
    {
        let url = u("http://example.com/{expression}/{reading}.mp3");
        let src = u("Forvo Audio");
        let expr = u("食べる");
        let read = u("たべる");

        // Normal reading
        let (out_url, out_fn) = format_audio_url_and_filename(&url, &src, &expr, &read, false);
        assert_eq!(out_url, u("http://example.com/食べる/たべる.mp3"));
        assert_eq!(out_fn, u("memento_Forvo_Audio_たべる_食べる.mp3"));

        // Reading as expression
        let (out_url2, out_fn2) = format_audio_url_and_filename(&url, &src, &expr, &read, true);
        assert_eq!(out_url2, u("http://example.com/たべる/たべる.mp3"));
        assert_eq!(out_fn2, u("memento_Forvo_Audio_たべる_食べる.mp3"));

        // Empty reading falls back to expression in URL
        let empty_read = u("");
        let (out_url3, out_fn3) = format_audio_url_and_filename(&url, &src, &expr, &empty_read, false);
        assert_eq!(out_url3, u("http://example.com/食べる/食べる.mp3"));
        assert_eq!(out_fn3, u("memento_Forvo_Audio__食べる.mp3"));
    }

    #[test]
    fn test_format_pitch_graphs()
    {
        assert_eq!(format_pitch_graphs(&[]), u(""));

        let d1 = u("NHK");
        let d2 = u("Daijisen");
        let svg1 = u("<svg>1</svg>");
        let svg2 = u("<svg>2</svg>");

        // Single dict, single pitch, 1 position
        let svgs1 = [&svg1[..]];
        let single = [PitchGraphEntry {
            dictionary_name: &d1,
            positions_count: 1,
            svg_graphs: &svgs1,
        }];
        assert_eq!(
            format_pitch_graphs(&single),
            u("<span class=\"memento-pitch-graph\"><svg>1</svg></span>")
        );

        // Single dict, multiple pitches (positions_count = 2) -> <ol><li>...</li></ol>
        let svgs2 = [&svg1[..], &svg2[..]];
        let multi_pos = [PitchGraphEntry {
            dictionary_name: &d1,
            positions_count: 2,
            svg_graphs: &svgs2,
        }];
        assert_eq!(
            format_pitch_graphs(&multi_pos),
            u("<span class=\"memento-pitch-graph\"><ol><li><svg>1</svg></li><li><svg>2</svg></li></ol></span>")
        );

        // Multiple dicts -> <ul><li><i>{dict}</i>...</li></ul>
        let multi_dict = [
            PitchGraphEntry {
                dictionary_name: &d1,
                positions_count: 1,
                svg_graphs: &svgs1,
            },
            PitchGraphEntry {
                dictionary_name: &d2,
                positions_count: 2,
                svg_graphs: &svgs2,
            },
        ];
        assert_eq!(
            format_pitch_graphs(&multi_dict),
            u("<span class=\"memento-pitch-graph\"><ul><li><i>NHK</i><svg>1</svg></li><li><i>Daijisen</i><ol><li><svg>1</svg></li><li><svg>2</svg></li></ol></li></ul></span>")
        );

        // Mora empty: positions_count = 2, svg_graphs empty -> <ol></ol>
        let empty_svgs: [&[u16]; 0] = [];
        let mora_empty_entry = [PitchGraphEntry {
            dictionary_name: &d1,
            positions_count: 2,
            svg_graphs: &empty_svgs,
        }];
        assert_eq!(
            format_pitch_graphs(&mora_empty_entry),
            u("<span class=\"memento-pitch-graph\"><ol></ol></span>")
        );
    }

    #[test]
    fn test_classify_pitch_categories()
    {
        assert_eq!(classify_pitch(0, 0, false), None);
        assert_eq!(classify_pitch(3, 0, false), Some(PitchCategory::Heiban));
        assert_eq!(classify_pitch(3, 1, false), Some(PitchCategory::Atamadaka));
        assert_eq!(classify_pitch(3, 1, true), Some(PitchCategory::Kifuku));
        assert_eq!(classify_pitch(3, 3, false), Some(PitchCategory::Odaka));
        assert_eq!(classify_pitch(3, 2, false), Some(PitchCategory::Nakadaka));
        assert_eq!(classify_pitch(3, 2, true), Some(PitchCategory::Kifuku));

        // Deduplication in order of appearance
        let pos1 = [0u8, 2u8];
        let pos2 = [0u8, 1u8];
        let inputs = [
            PitchCategoryInput { mora_len: 3, positions: &pos1 },
            PitchCategoryInput { mora_len: 3, positions: &pos2 },
        ];
        let cats = classify_pitch_categories(&inputs, false);
        assert_eq!(cats, [PitchCategory::Heiban, PitchCategory::Nakadaka, PitchCategory::Atamadaka]);

        assert_eq!(format_pitch_categories(&[], false), u(""));
        assert_eq!(format_pitch_categories(&inputs, false), u("heiban,nakadaka,atamadaka"));
        assert_eq!(format_pitch_categories(&inputs, true), u("heiban,kifuku"));
    }

    #[test]
    fn test_find_stroke_count()
    {
        assert_eq!(find_stroke_count(&[]), None);

        let sc_name = u("Stroke count");
        let val = u("12");
        let other_name = u("Grade");
        let other_val = u("1");

        let stats = [
            StrokeCountStat { notes: &other_name, value: &other_val },
            StrokeCountStat { notes: &sc_name, value: &val },
        ];
        assert_eq!(find_stroke_count(&stats), Some(&val[..]));

        let no_sc = [StrokeCountStat { notes: &other_name, value: &other_val }];
        assert_eq!(find_stroke_count(&no_sc), None);
    }

    #[test]
    fn test_format_term_glossary()
    {
        assert_eq!(format_term_glossary(&[]), TermGlossaryOutput::default());

        let d1 = u("JMdict");
        let t1 = u("v1");
        let item1 = u("to eat");
        let item2 = u("to consume");
        let style1 = u(".jmdict { color: red; }");

        let tags = [&t1[..]];
        let items = [&item1[..], &item2[..]];
        let defs = [
            TermGlossaryDef {
                dict_id: 1,
                dict_name: &d1,
                tags: &tags,
                items: &items,
                has_styles: true,
                stylesheet: &style1,
            },
        ];

        let contains_subslice = |haystack: &[u16], needle: &[u16]| {
            haystack.windows(needle.len()).any(|w| w == needle)
        };

        let out = format_term_glossary(&defs);
        assert!(out.glossary.starts_with(&key("<div class=\"yomitan-glossary memento-glossary\"><ol>")));
        assert!(contains_subslice(&out.glossary, &u("<li data-dictionary=\"JMdict\"><i>(v1, JMdict)</i> <span>to eatto consume</span></li>")));
        assert!(contains_subslice(&out.glossary, &u("<style>.jmdict { color: red; }</style>")));

        assert!(contains_subslice(&out.glossary_brief, &u("<li>to eat</li><li>to consume</li>")));
        assert!(!contains_subslice(&out.glossary_brief, &u("<li data-dictionary")));

        assert!(contains_subslice(&out.glossary_compact, &u("to eat | to consume")));

        // Deduplication of styles with same dict_id
        let defs2 = [
            TermGlossaryDef {
                dict_id: 1,
                dict_name: &d1,
                tags: &tags,
                items: &items,
                has_styles: true,
                stylesheet: &style1,
            },
            TermGlossaryDef {
                dict_id: 1,
                dict_name: &d1,
                tags: &tags,
                items: &items,
                has_styles: true,
                stylesheet: &style1,
            },
        ];
        let out2 = format_term_glossary(&defs2);
        let s = String::from_utf16_lossy(&out2.glossary);
        assert_eq!(s.matches("<style>.jmdict { color: red; }</style>").count(), 1);
    }

    #[test]
    fn media_marker_arguments_match_qt_conversions()
    {
        let defaults = parse_marker("{audio-media}");
        assert_eq!(
            audio_media_params(&defaults, true, -20.0),
            Some(AudioMediaParams {
                source: TimingSource::Subtitle,
                normalize: true,
                db: -20.0,
            })
        );

        let args = parse_marker("{audio-media:source=context,norm=FaLsE,db=-12.5}");
        assert_eq!(
            audio_media_params(&args, true, -20.0),
            Some(AudioMediaParams {
                source: TimingSource::Context,
                normalize: false,
                db: -12.5,
            })
        );
        assert_eq!(
            audio_context_params(&args, false, -20.0),
            Some(AudioMediaParams {
                source: TimingSource::Context,
                normalize: false,
                db: -12.5,
            })
        );

        let invalid_source = parse_marker("{audio-context:source=invalid}");
        assert_eq!(audio_context_params(&invalid_source, false, -20.0), None);
        let invalid_db = parse_marker("{audio-media:db=not-a-number}");
        assert_eq!(audio_media_params(&invalid_db, false, -20.0), None);

        let truthy = parse_marker("{audio-media:norm=no}");
        assert!(
            audio_media_params(&truthy, false, -20.0)
                .unwrap()
                .normalize
        );

        let mut whitespace_db = Marker::default();
        whitespace_db.insert_arg(
            u("db"),
            u("\u{00a0}-12.5\u{2003}"),
        );
        assert_eq!(
            audio_media_params(&whitespace_db, false, -20.0)
                .unwrap()
                .db,
            -12.5
        );
    }

    #[test]
    fn screenshot_and_video_marker_arguments_keep_qt_defaults()
    {
        assert_eq!(
            screenshot_params(&parse_marker("{screenshot}")),
            Some(ScreenshotParams {
                max_width: -1,
                max_height: -1,
                keep_aspect_ratio: true,
            })
        );
        assert_eq!(
            screenshot_params(&parse_marker(
                "{screenshot:max-width=+1920,max-height=-4,keep-ratio=FALSE}",
            )),
            Some(ScreenshotParams {
                max_width: 1920,
                max_height: -4,
                keep_aspect_ratio: false,
            })
        );

        let mut whitespace_width = Marker::default();
        whitespace_width.insert_arg(
            u("max-width"),
            u("\u{00a0}+1920\u{2003}"),
        );
        assert_eq!(screenshot_params(&whitespace_width).unwrap().max_width, 1920);
        assert_eq!(
            screenshot_params(&parse_marker("{screenshot:max-width=2147483648}")),
            None
        );
        assert_eq!(
            screenshot_params(&parse_marker("{screenshot:max-width=wide}")),
            None
        );

        assert_eq!(
            video_params(&parse_marker("{video}"), false, -20.0),
            Some(VideoParams {
                source: TimingSource::Subtitle,
                audio: true,
                subtitles: true,
                normalize: false,
                db: -20.0,
            })
        );
        assert_eq!(
            video_params(
                &parse_marker(
                    "{video:source=context,audio=0,subtitles=no,norm=FALSE,db=3.25}",
                ),
                true,
                -20.0,
            ),
            Some(VideoParams {
                source: TimingSource::Context,
                audio: false,
                subtitles: true,
                normalize: false,
                db: 3.25,
            })
        );
    }

    #[test]
    fn test_compute_clip_range()
    {
        // Subtitle source
        assert_eq!(
            compute_clip_range(TimingSource::Subtitle, 5.0, 10.0, 2.0, 15.0, 0.5, 0.5),
            Some((4.5, 10.5))
        );

        // Context source
        assert_eq!(
            compute_clip_range(TimingSource::Context, 5.0, 10.0, 2.0, 15.0, 0.5, 0.5),
            Some((1.5, 15.5))
        );

        // Clamping to 0.0
        assert_eq!(
            compute_clip_range(TimingSource::Subtitle, 0.2, 1.0, 0.0, 0.0, 0.5, 0.0),
            Some((0.0, 1.0))
        );

        // start >= end -> None
        assert_eq!(
            compute_clip_range(TimingSource::Subtitle, 1.0, 0.5, 0.0, 0.0, 0.0, 0.0),
            None
        );
        assert_eq!(
            compute_clip_range(TimingSource::Subtitle, 0.2, 0.2, 0.0, 0.0, 0.0, 0.0),
            None
        );
    }

    #[test]
    fn invalid_clip_start_does_not_become_zero()
    {
        // A missing/invalid timestamp or NaN padding must not turn into
        // a valid [0, end] clip. C++ std::max(NaN, 0.0) retains NaN.
        assert_eq!(
            compute_clip_range(TimingSource::Subtitle, f64::NAN, 4.0, 0.0, 0.0, 0.0, 0.0),
            None
        );
        assert_eq!(
            compute_clip_range(TimingSource::Context, 0.0, 0.0, 1.0, 4.0, f64::NAN, 0.0),
            None
        );
    }
}

