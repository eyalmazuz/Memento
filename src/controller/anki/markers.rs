//! Template marker classification, precomputed `RenderedTemplateData`, and substitution.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use memento_anki::note::{
    build_frequencies_html, format_tags, substitute_markers, FrequencyEntry, MarkerResult,
    TagEntry, TermGlossaryVariant, TermGlossaryVariants,
};
use memento_anki::tokenizer::{tokenize, Marker};
use memento_net::anki::MediaFilePayload;

use crate::controller::config::AnkiNoteConfig;

use super::context::{filter_subtitle_text, read_clipboard_text, NoteFieldsContext};
use super::css_glossary::{
    format_context_glossary_selected, format_context_glossary_variant, GlossaryOutput,
};
use super::payload::NoteBuildError;

fn is_media_marker(name: &str) -> bool {
    matches!(
        name,
        "audio"
            | "audio-media"
            | "audio-context"
            | "sentence-audio"
            | "screenshot"
            | "screenshot-video"
            | "image"
            | "video"
    )
}

fn is_known_marker(name: &str) -> bool {
    matches!(
        name,
        "audio"
            | "audio-media"
            | "audio-context"
            | "sentence-audio"
            | "clipboard"
            | "cloze-prefix"
            | "cloze-body"
            | "cloze-suffix"
            | "context"
            | "context-2"
            | "frequency-average-occurrence"
            | "frequency-average-rank"
            | "frequency-harmonic-occurrence"
            | "frequency-harmonic-rank"
            | "frequencies"
            | "glossary"
            | "glossary-brief"
            | "glossary-compact"
            | "part-of-speech"
            | "screenshot"
            | "screenshot-video"
            | "image"
            | "sentence"
            | "sentence-2"
            | "subtitle"
            | "subtitle-2"
            | "tags"
            | "tags-brief"
            | "title"
            | "video"
            | "expression"
            | "furigana"
            | "furigana-plain"
            | "pitch"
            | "pitch-accent"
            | "pitch-accents"
            | "pitch-categories"
            | "pitch-accent-categories"
            | "pitch-graphs"
            | "pitch-graph"
            | "pitch-accent-graphs"
            | "pitch-positions"
            | "pitch-position"
            | "pitch-accent-positions"
            | "reading"
            | "selection"
            | "character"
            | "kunyomi"
            | "onyomi"
            | "stroke-count"
    )
}

fn glossary_variant_for_marker(name: &str) -> Option<TermGlossaryVariant> {
    match name {
        "glossary" => Some(TermGlossaryVariant::Full),
        "glossary-brief" => Some(TermGlossaryVariant::Brief),
        "glossary-compact" => Some(TermGlossaryVariant::Compact),
        _ => None,
    }
}

fn set_variant_flag(variants: &mut TermGlossaryVariants, variant: TermGlossaryVariant) {
    match variant {
        TermGlossaryVariant::Full => variants.full = true,
        TermGlossaryVariant::Brief => variants.brief = true,
        TermGlossaryVariant::Compact => variants.compact = true,
    }
}

fn marker_result(value: &str) -> MarkerResult {
    MarkerResult {
        handled: true,
        media: false,
        text: value.encode_utf16().collect(),
    }
}

fn marker_arg<'a>(marker: &'a Marker, key: &str) -> Option<&'a [u16]> {
    marker.args.iter().find_map(|(arg_key, value)| {
        arg_key
            .iter()
            .copied()
            .eq(key.encode_utf16())
            .then_some(value.as_slice())
    })
}

fn marker_bool_arg(marker: &Marker, key: &str, default: bool) -> bool {
    let Some(value) = marker_arg(marker, key) else {
        return default;
    };
    if value.is_empty() {
        return false;
    }
    let text = String::from_utf16_lossy(value);
    let trimmed = text.trim();
    !trimmed.is_empty() && trimmed != "0" && !trimmed.eq_ignore_ascii_case("false")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GlossaryDictFilter {
    Unfiltered,
    ById(i64),
    Invalid,
}

fn parse_glossary_dict_filter(marker: &Marker) -> GlossaryDictFilter {
    let Some(raw_val) = marker_arg(marker, "dict") else {
        return GlossaryDictFilter::Unfiltered;
    };
    let Ok(text) = String::from_utf16(raw_val) else {
        return GlossaryDictFilter::Invalid;
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return GlossaryDictFilter::Invalid;
    }
    match trimmed.parse::<i64>() {
        Ok(id) => GlossaryDictFilter::ById(id),
        Err(_) => GlossaryDictFilter::Invalid,
    }
}

pub(crate) fn template_references_any_marker(template: &str, marker_names: &[&str]) -> bool {
    for token in tokenize(&template.encode_utf16().collect::<Vec<_>>()) {
        for marker in &token.markers {
            let name = String::from_utf16_lossy(&marker.name);
            if marker_names.iter().any(|expected| *expected == name) {
                return true;
            }
        }
    }
    false
}

pub(crate) fn templates_reference_marker<'a>(
    templates: impl Iterator<Item = &'a String>,
    marker_name: &str,
) -> bool {
    for template in templates {
        if template_references_any_marker(template, &[marker_name]) {
            return true;
        }
    }
    false
}

fn resolve_context_tags_selected(
    ctx: &NoteFieldsContext,
    need_full: bool,
    need_brief: bool,
) -> (String, String) {
    if !need_full && !need_brief {
        return (String::new(), String::new());
    }
    // `memento_anki::note::format_tags` HTML-escapes tag names and notes internally.
    if !ctx.tag_entries.is_empty() {
        let names_u16: Vec<Vec<u16>> = ctx
            .tag_entries
            .iter()
            .map(|t| t.name.encode_utf16().collect())
            .collect();
        let notes_u16: Vec<Vec<u16>> = if need_full {
            ctx.tag_entries
                .iter()
                .map(|t| t.notes.encode_utf16().collect())
                .collect()
        } else {
            Vec::new()
        };
        let entries: Vec<TagEntry<'_>> = names_u16
            .iter()
            .enumerate()
            .map(|(i, name)| TagEntry {
                name,
                notes: notes_u16.get(i).map(Vec::as_slice).unwrap_or(&[]),
            })
            .collect();
        let full = if need_full {
            String::from_utf16_lossy(&format_tags(&entries, false))
        } else {
            String::new()
        };
        let brief = if need_brief {
            String::from_utf16_lossy(&format_tags(&entries, true))
        } else {
            String::new()
        };
        return (full, brief);
    }
    if !ctx.tags.is_empty() {
        let names_u16: Vec<Vec<u16>> = ctx
            .tags
            .iter()
            .map(|t| t.encode_utf16().collect())
            .collect();
        let entries: Vec<TagEntry<'_>> = names_u16
            .iter()
            .map(|name| TagEntry { name, notes: &[] })
            .collect();
        let full = if need_full {
            String::from_utf16_lossy(&format_tags(&entries, false))
        } else {
            String::new()
        };
        let brief = if need_brief {
            String::from_utf16_lossy(&format_tags(&entries, true))
        } else {
            String::new()
        };
        return (full, brief);
    }
    (String::new(), String::new())
}

fn resolve_part_of_speech(ctx: &NoteFieldsContext) -> String {
    if !ctx.part_of_speech.is_empty() {
        return ctx.part_of_speech.clone();
    }
    let pos: Vec<&str> = ctx
        .tag_entries
        .iter()
        .filter(|t| t.category == "partOfSpeech")
        .map(|t| t.name.as_str())
        .collect();
    pos.join(", ")
}

/// Borrowed or transformed view of the common subtitle/context/cloze/clipboard text fields,
/// avoiding cloning `NoteFieldsContext` (and its multi-MB media attachments) during note builds.
pub(crate) struct CommonTextFields<'a> {
    pub(crate) sentence: Cow<'a, str>,
    pub(crate) context: Cow<'a, str>,
    pub(crate) selection: Cow<'a, str>,
    pub(crate) secondary_subtitle: Cow<'a, str>,
    pub(crate) secondary_context: Cow<'a, str>,
    pub(crate) title: Cow<'a, str>,
    pub(crate) cloze_prefix: Cow<'a, str>,
    pub(crate) cloze_body: Cow<'a, str>,
    pub(crate) cloze_suffix: Cow<'a, str>,
    pub(crate) clipboard: Cow<'a, str>,
}

fn replace_newlines_cow<'a>(value: &'a str, newline_replacer: &str) -> Cow<'a, str> {
    if value.contains('\n') {
        Cow::Owned(value.replace('\n', newline_replacer))
    } else {
        Cow::Borrowed(value)
    }
}

fn filter_and_replace_newlines<'a>(
    value: &'a str,
    remove_regex: Option<&str>,
    replace_lines: bool,
    replace_with: &str,
    newline_replacer: &str,
) -> Cow<'a, str> {
    let filter_active = remove_regex.is_some_and(|r| !r.is_empty()) || replace_lines;
    if filter_active {
        let filtered = filter_subtitle_text(value, remove_regex, replace_lines, replace_with);
        if filtered.contains('\n') {
            Cow::Owned(filtered.replace('\n', newline_replacer))
        } else {
            Cow::Owned(filtered)
        }
    } else {
        replace_newlines_cow(value, newline_replacer)
    }
}

impl<'a> CommonTextFields<'a> {
    pub(crate) fn raw(ctx: &'a NoteFieldsContext, clipboard: &'a str) -> Self {
        Self {
            sentence: Cow::Borrowed(&ctx.sentence),
            context: Cow::Borrowed(&ctx.context),
            selection: Cow::Borrowed(&ctx.selection),
            secondary_subtitle: Cow::Borrowed(&ctx.secondary_subtitle),
            secondary_context: Cow::Borrowed(&ctx.secondary_context),
            title: Cow::Borrowed(&ctx.title),
            cloze_prefix: Cow::Borrowed(&ctx.cloze_prefix),
            cloze_body: Cow::Borrowed(&ctx.cloze_body),
            cloze_suffix: Cow::Borrowed(&ctx.cloze_suffix),
            clipboard: Cow::Borrowed(clipboard),
        }
    }

    pub(crate) fn transformed(
        ctx: &'a NoteFieldsContext,
        clipboard: &'a str,
        newline_replacer: &str,
    ) -> Self {
        let remove_regex = ctx.subtitle_filter_regex.as_deref();
        let replace_lines = ctx.replace_lines;
        let replace_with = ctx.replace_lines_with.as_str();
        Self {
            sentence: filter_and_replace_newlines(
                &ctx.sentence,
                remove_regex,
                replace_lines,
                replace_with,
                newline_replacer,
            ),
            context: filter_and_replace_newlines(
                &ctx.context,
                remove_regex,
                replace_lines,
                replace_with,
                newline_replacer,
            ),
            selection: replace_newlines_cow(&ctx.selection, newline_replacer),
            secondary_subtitle: replace_newlines_cow(&ctx.secondary_subtitle, newline_replacer),
            secondary_context: replace_newlines_cow(&ctx.secondary_context, newline_replacer),
            title: replace_newlines_cow(&ctx.title, newline_replacer),
            cloze_prefix: filter_and_replace_newlines(
                &ctx.cloze_prefix,
                remove_regex,
                replace_lines,
                replace_with,
                newline_replacer,
            ),
            cloze_body: filter_and_replace_newlines(
                &ctx.cloze_body,
                remove_regex,
                replace_lines,
                replace_with,
                newline_replacer,
            ),
            cloze_suffix: filter_and_replace_newlines(
                &ctx.cloze_suffix,
                remove_regex,
                replace_lines,
                replace_with,
                newline_replacer,
            ),
            clipboard: replace_newlines_cow(clipboard, newline_replacer),
        }
    }
}

/// Precomputed marker data computed once per note build, rendering only the glossary
/// variants, tag styles, POS, and clipboard text actually referenced by the templates.
pub(crate) struct RenderedTemplateData {
    pub(crate) glossary: GlossaryOutput,
    pub(crate) filtered_glossaries: HashMap<i64, GlossaryOutput>,
    pub(crate) tags_full: String,
    pub(crate) tags_brief: String,
    pub(crate) part_of_speech: String,
    pub(crate) clipboard: String,
    pub(crate) has_media_marker: bool,
}

impl RenderedTemplateData {
    pub(crate) fn for_templates(
        field_templates: &HashMap<String, String>,
        ctx: &NoteFieldsContext,
    ) -> Self {
        let mut unfiltered_variants = TermGlossaryVariants::NONE;
        let mut filtered_variants: HashMap<i64, TermGlossaryVariants> = HashMap::new();
        let mut need_tags_full = false;
        let mut need_tags_brief = false;
        let mut need_pos = false;
        let mut need_clipboard = false;
        let mut has_media_marker = false;

        for template in field_templates.values() {
            for token in tokenize(&template.encode_utf16().collect::<Vec<_>>()) {
                for marker in &token.markers {
                    let name = String::from_utf16_lossy(&marker.name);
                    match name.as_str() {
                        "glossary" | "glossary-brief" | "glossary-compact" => {
                            if let Some(variant) = glossary_variant_for_marker(&name) {
                                match parse_glossary_dict_filter(marker) {
                                    GlossaryDictFilter::Unfiltered => {
                                        set_variant_flag(&mut unfiltered_variants, variant);
                                    }
                                    GlossaryDictFilter::ById(id) => {
                                        let entry = filtered_variants
                                            .entry(id)
                                            .or_insert(TermGlossaryVariants::NONE);
                                        set_variant_flag(entry, variant);
                                    }
                                    GlossaryDictFilter::Invalid => {}
                                }
                            }
                        }
                        "tags" => need_tags_full = true,
                        "tags-brief" => need_tags_brief = true,
                        "part-of-speech" => need_pos = true,
                        "clipboard" => need_clipboard = true,
                        other if is_media_marker(other) => has_media_marker = true,
                        _ => {}
                    }
                }
            }
        }

        let glossary = format_context_glossary_selected(ctx, None, unfiltered_variants);
        let mut filtered_glossaries = HashMap::with_capacity(filtered_variants.len());
        for (dict_id, variants) in filtered_variants {
            filtered_glossaries.insert(
                dict_id,
                format_context_glossary_selected(ctx, Some(dict_id), variants),
            );
        }
        let (tags_full, tags_brief) =
            resolve_context_tags_selected(ctx, need_tags_full, need_tags_brief);
        let part_of_speech = if need_pos {
            resolve_part_of_speech(ctx)
        } else {
            String::new()
        };
        let clipboard = match &ctx.clipboard {
            Some(text) => text.clone(),
            None if need_clipboard => read_clipboard_text(),
            None => String::new(),
        };

        Self {
            glossary,
            filtered_glossaries,
            tags_full,
            tags_brief,
            part_of_speech,
            clipboard,
            has_media_marker,
        }
    }
}

fn render_frequencies_marker(marker: &Marker, ctx: &NoteFieldsContext) -> String {
    let value_only = marker_bool_arg(marker, "value-only", false);
    let min_value = marker_bool_arg(marker, "min-value", false);
    if !ctx.frequency_entries.is_empty() {
        let dict_u16: Vec<Vec<u16>> = ctx
            .frequency_entries
            .iter()
            .map(|(d, _)| d.encode_utf16().collect())
            .collect();
        let freq_u16: Vec<Vec<u16>> = ctx
            .frequency_entries
            .iter()
            .map(|(_, f)| f.encode_utf16().collect())
            .collect();
        let entries: Vec<FrequencyEntry<'_>> = dict_u16
            .iter()
            .zip(&freq_u16)
            .map(|(d, f)| FrequencyEntry {
                dictionary: d,
                freq: f,
            })
            .collect();
        return String::from_utf16_lossy(&build_frequencies_html(&entries, value_only, min_value));
    }
    ctx.frequencies.clone()
}

fn resolve_frequency_scalar<'a>(name: &str, ctx: &'a NoteFieldsContext) -> Option<&'a str> {
    match name {
        "frequency-harmonic-rank" => Some(if !ctx.frequency_harmonic_rank.is_empty() {
            ctx.frequency_harmonic_rank.as_str()
        } else {
            "9999999"
        }),
        "frequency-average-rank" => Some(if !ctx.frequency_average_rank.is_empty() {
            ctx.frequency_average_rank.as_str()
        } else {
            "9999999"
        }),
        "frequency-harmonic-occurrence" => Some(if !ctx.frequency_harmonic_occurrence.is_empty() {
            ctx.frequency_harmonic_occurrence.as_str()
        } else if !ctx.frequency_harmonic_rank.is_empty()
            && ctx.frequency_harmonic_rank != "9999999"
        {
            ctx.frequency_harmonic_rank.as_str()
        } else {
            "0"
        }),
        "frequency-average-occurrence" => Some(if !ctx.frequency_average_occurrence.is_empty() {
            ctx.frequency_average_occurrence.as_str()
        } else if !ctx.frequency_average_rank.is_empty()
            && ctx.frequency_average_rank != "9999999"
        {
            ctx.frequency_average_rank.as_str()
        } else {
            "0"
        }),
        _ => None,
    }
}

fn common_marker_value<'a>(
    name: &str,
    text_fields: &'a CommonTextFields<'_>,
) -> Option<&'a str> {
    match name {
        "cloze-prefix" => Some(text_fields.cloze_prefix.as_ref()),
        "cloze-body" => Some(text_fields.cloze_body.as_ref()),
        "cloze-suffix" => Some(text_fields.cloze_suffix.as_ref()),
        "subtitle" | "sentence" => Some(text_fields.sentence.as_ref()),
        "context" => Some(text_fields.context.as_ref()),
        "selection" => Some(text_fields.selection.as_ref()),
        "clipboard" => Some(text_fields.clipboard.as_ref()),
        "subtitle-2" | "sentence-2" => Some(text_fields.secondary_subtitle.as_ref()),
        "context-2" => {
            if !text_fields.secondary_context.is_empty() {
                Some(text_fields.secondary_context.as_ref())
            } else {
                Some(text_fields.secondary_subtitle.as_ref())
            }
        }
        "title" => Some(text_fields.title.as_ref()),
        _ => None,
    }
}

fn field_has_attachment(attachments: &[serde_json::Value], field: &str) -> bool {
    attachments.iter().any(|item| {
        item.get("fields")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|fields| fields.iter().any(|f| f.as_str() == Some(field)))
    })
}

fn media_marker_has_attachment(name: &str, field: &str, ctx: &NoteFieldsContext) -> bool {
    if !ctx.resolved_media_markers.is_empty() {
        return ctx
            .resolved_media_markers
            .iter()
            .any(|(f, m)| f == field && m == name);
    }
    match name {
        "audio" | "audio-media" | "audio-context" | "sentence-audio" => {
            field_has_attachment(&ctx.audio_attachments, field)
        }
        "screenshot" | "screenshot-video" | "image" => {
            field_has_attachment(&ctx.picture_attachments, field)
        }
        "video" => field_has_attachment(&ctx.video_attachments, field),
        _ => false,
    }
}

fn evaluate_specific_marker_named(
    field: &str,
    name: &str,
    marker: &Marker,
    ctx: &NoteFieldsContext,
    rendered: &RenderedTemplateData,
) -> MarkerResult {
    if let Some(variant) = glossary_variant_for_marker(name) {
        return match parse_glossary_dict_filter(marker) {
            GlossaryDictFilter::Invalid => marker_result("ERROR: Invalid dic-id value"),
            GlossaryDictFilter::ById(id) => {
                if let Some(filtered) = rendered.filtered_glossaries.get(&id) {
                    marker_result(filtered.variant(variant))
                } else {
                    let computed = format_context_glossary_variant(ctx, Some(id), variant);
                    marker_result(&computed)
                }
            }
            GlossaryDictFilter::Unfiltered => marker_result(rendered.glossary.variant(variant)),
        };
    }

    if name == "frequencies" {
        return marker_result(&render_frequencies_marker(marker, ctx));
    }
    if let Some(freq_scalar) = resolve_frequency_scalar(name, ctx) {
        return marker_result(freq_scalar);
    }

    if is_media_marker(name) {
        return MarkerResult {
            handled: true,
            media: media_marker_has_attachment(name, field, ctx),
            text: Vec::new(),
        };
    }

    let text_opt = match name {
        "expression" => Some(ctx.expression.as_str()),
        "reading" => Some(ctx.reading.as_str()),
        "tags" => Some(rendered.tags_full.as_str()),
        "tags-brief" => Some(rendered.tags_brief.as_str()),
        "part-of-speech" => Some(rendered.part_of_speech.as_str()),
        "furigana" => Some(ctx.furigana.as_str()),
        "furigana-plain" => Some(ctx.furigana_plain.as_str()),
        "pitch" | "pitch-accents" | "pitch-accent" => Some(ctx.pitch.as_str()),
        "pitch-graphs" | "pitch-graph" | "pitch-accent-graphs" => Some(ctx.pitch_graphs.as_str()),
        "pitch-positions" | "pitch-position" | "pitch-accent-positions" => {
            Some(ctx.pitch_positions.as_str())
        }
        "pitch-categories" | "pitch-accent-categories" => Some(ctx.pitch_categories.as_str()),
        "character" => Some(ctx.character.as_str()),
        "onyomi" => Some(ctx.onyomi.as_str()),
        "kunyomi" => Some(ctx.kunyomi.as_str()),
        "stroke-count" => Some(ctx.stroke_count.as_str()),
        _ => None,
    };

    text_opt.map(marker_result).unwrap_or_default()
}

/// Classification used by `anki_media::prepare_note_media` when resolving
/// `{a | b}` alternative markers containing mixed media and text markers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MarkerEvalKind {
    Media,
    NonEmptyText,
    EmptyText,
    Unknown,
}

pub(crate) fn evaluate_template_marker_kind(
    marker: &Marker,
    ctx: &NoteFieldsContext,
    rendered: &RenderedTemplateData,
) -> MarkerEvalKind {
    let name = String::from_utf16_lossy(&marker.name);
    if is_media_marker(&name) {
        return MarkerEvalKind::Media;
    }
    if !is_known_marker(&name) {
        return MarkerEvalKind::Unknown;
    }
    let text_fields = CommonTextFields::raw(ctx, &rendered.clipboard);
    if let Some(value) = common_marker_value(&name, &text_fields) {
        return if value.is_empty() {
            MarkerEvalKind::EmptyText
        } else {
            MarkerEvalKind::NonEmptyText
        };
    }
    let res = evaluate_specific_marker_named("", &name, marker, ctx, rendered);
    if !res.handled {
        MarkerEvalKind::Unknown
    } else if res.text.is_empty() {
        MarkerEvalKind::EmptyText
    } else {
        MarkerEvalKind::NonEmptyText
    }
}

/// Maps Anki field templates through the same token parser and ordered
/// alternative-marker substitution used by the C++ note builder.
pub fn map_note_fields(
    field_templates: &HashMap<String, String>,
    ctx: &NoteFieldsContext,
) -> HashMap<String, String> {
    let rendered = RenderedTemplateData::for_templates(field_templates, ctx);
    let text_fields = CommonTextFields::raw(ctx, &rendered.clipboard);
    map_note_fields_with_data(field_templates, ctx, &text_fields, &rendered)
}

pub(crate) fn map_note_fields_with_data(
    field_templates: &HashMap<String, String>,
    ctx: &NoteFieldsContext,
    text_fields: &CommonTextFields<'_>,
    rendered: &RenderedTemplateData,
) -> HashMap<String, String> {
    let mut result = HashMap::with_capacity(field_templates.len());
    for (field_name, template) in field_templates {
        let input: Vec<u16> = template.encode_utf16().collect();
        let output = substitute_markers(
            &input,
            |marker| {
                let name = String::from_utf16_lossy(&marker.name);
                common_marker_value(&name, text_fields)
                    .map(marker_result)
                    .unwrap_or_default()
            },
            |marker| {
                let name = String::from_utf16_lossy(&marker.name);
                evaluate_specific_marker_named(field_name, &name, marker, ctx, rendered)
            },
        );
        result.insert(field_name.clone(), String::from_utf16_lossy(&output));
    }
    result
}

/// Profile markers for which this app currently has no source data.
pub fn unavailable_profile_markers(_profile: &AnkiNoteConfig) -> Vec<String> {
    Vec::new()
}

pub(crate) fn validate_template_markers(
    field: &str,
    template: &str,
    ctx: &NoteFieldsContext,
    text_fields: &CommonTextFields<'_>,
    rendered: &RenderedTemplateData,
) -> Result<(), NoteBuildError> {
    let input: Vec<u16> = template.encode_utf16().collect();
    for token in tokenize(&input) {
        for marker in &token.markers {
            let name = String::from_utf16_lossy(&marker.name);
            if let Some(value) = common_marker_value(&name, text_fields) {
                if !value.is_empty() {
                    break;
                }
                continue;
            }
            if !is_known_marker(&name) {
                return Err(NoteBuildError::MissingMarkerData {
                    field: field.to_owned(),
                    marker: name,
                });
            }
            let eval = evaluate_specific_marker_named(field, &name, marker, ctx, rendered);
            if eval.media || !eval.text.is_empty() {
                break;
            }
        }
    }
    Ok(())
}

/// Collects deduplicated dictionary media files referenced by glossary markers
/// in `field_templates`, honoring `{a | b}` alternative short-circuiting.
pub fn referenced_dictionary_media_files(
    field_templates: &HashMap<String, String>,
    ctx: &NoteFieldsContext,
) -> Vec<MediaFilePayload> {
    if ctx.glossary_definitions.is_empty() {
        return Vec::new();
    }
    let rendered = RenderedTemplateData::for_templates(field_templates, ctx);
    referenced_dictionary_media_files_with_data(field_templates, ctx, &rendered)
}

pub(crate) fn referenced_dictionary_media_files_with_data(
    field_templates: &HashMap<String, String>,
    ctx: &NoteFieldsContext,
    rendered: &RenderedTemplateData,
) -> Vec<MediaFilePayload> {
    if ctx.glossary_definitions.is_empty() {
        return Vec::new();
    }
    let text_fields = CommonTextFields::raw(ctx, &rendered.clipboard);

    let mut seen_filenames = HashSet::new();
    let mut files = Vec::new();
    let mut sorted_fields: Vec<(&String, &String)> = field_templates.iter().collect();
    sorted_fields.sort_by_key(|(name, _)| *name);

    for (field, template) in sorted_fields {
        for token in tokenize(&template.encode_utf16().collect::<Vec<_>>()) {
            for marker in &token.markers {
                let name = String::from_utf16_lossy(&marker.name);
                if let Some(value) = common_marker_value(&name, &text_fields) {
                    if !value.is_empty() {
                        break;
                    }
                    continue;
                }
                if glossary_variant_for_marker(&name).is_some() {
                    let filter = parse_glossary_dict_filter(marker);
                    for def in ctx.glossary_definitions.iter().filter(|def| match filter {
                        GlossaryDictFilter::Unfiltered => true,
                        GlossaryDictFilter::ById(id) => def.dictionary_id == id,
                        GlossaryDictFilter::Invalid => false,
                    }) {
                        for media in &def.media_files {
                            if seen_filenames.insert(media.filename.clone()) {
                                files.push(media.clone());
                            }
                        }
                    }
                }
                let eval = evaluate_specific_marker_named(field, &name, marker, ctx, rendered);
                if eval.media || !eval.text.is_empty() {
                    break;
                }
            }
        }
    }
    files
}
