//! Generated Slint UI bindings and rich dictionary/subtitle bridges for Subomi.

pub use subomi_ui::*;

mod bridge;
mod dict_assets;
mod glossary_render;
mod popup_css;
mod popup_model;
mod rich_db;
mod subtitle_layout;
mod tags;
mod text_engine;

pub use bridge::{
    active_ass_lookup_for_app, active_subtitle_scan_utf16_range,
    bind_rich_bridge_ass_controller, bind_rich_bridge_lookup_epoch,
    clear_rich_bridge_ass_lookup, install_rich_ui_bridges, mark_popup_term_anki_added,
    mark_popup_term_anki_updated, nested_popup_row_anki_note_id_i64, popup_row_anki_note_id_i64,
    refresh_rich_ui_bridges,
};
pub use dict_assets::{
    collect_dictionary_resource_dirs, load_dictionary_image_pixmap,
    resolve_dictionary_asset_path, resolve_dictionary_asset_path_in_dirs,
};
pub use glossary_render::{
    compute_collapsible_metrics, extract_glossary_scan_substring,
    render_structured_glossary_entry, render_structured_glossary_entry_in_dirs,
    strict_char_index_at_point, GlossaryCharCell, GlossaryHitZone, GlossaryLineBand,
    GlossaryZoneAction, RenderedGlossaryDoc, MAX_NESTED_POPUP_DEPTH,
};
pub use popup_css::{
    default_custom_popup_css_template, default_custom_popup_outer_css_template,
    parse_dictionary_css_str, parse_popup_css_theme, parse_split_popup_css_theme,
    CssAttrMatchOp, CssAttrMatcher, DictStyleRule, FreqStyleRule, InterfaceFontFallback,
    PitchDictStyleRule, PopupCssExplicitOverrides, PopupCssTheme, PopupInterfaceFonts,
    ResolvedDictStyle, CUSTOM_POPUP_CSS_FILENAME, CUSTOM_POPUP_OUTER_CSS_FILENAME,
};
pub use popup_model::{
    dictionary_result_rows, install_popup_navigation, is_kanji_char, next_popup_batch_end,
    popup_pitch_graphs, prepare_text_fonts, rich_pitch_graphs_from_details, set_dictionary_rows,
    spawn_prepare_text_fonts, split_anki_tags_str,
};
pub use subtitle_layout::{
    active_subtitle_style_for_app, apply_subtitle_furigana, publish_active_subtitle,
    subtitle_lines_for_app, subtitle_lines_from_words, subtitle_lines_from_words_with_style,
    subtitle_token_width, subtitle_token_width_with_style, subtitle_words,
    subtitle_words_from_parts,
};
pub use tags::{extract_inline_structured_tags, suppress_redundant_tags, tag_category_hex_color};
pub(crate) use text_engine::cancel_subtitle_token_prewarm;
#[cfg(test)]
pub(crate) use text_engine::subtitle_token_prewarm_epoch_for_test;
pub use text_engine::{
    prewarm_subtitle_tokens, subtitle_cluster_at_point, subtitle_cluster_at_point_with_style,
    ShapedSubtitleToken, SubtitleRenderStyle,
};

/// Hint the OS allocator to return freed heap pages (Linux glibc `malloc_trim(0)`).
#[inline]
pub fn release_os_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        let _ = libc::malloc_trim(0);
    }
}
