use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use memento_definition::{Color as DefColor, LegacyGlossaryStyle};
use memento_dict::searchcontroller::TextReplacementOptions;
use memento_setting::keys as raw;
use slint::{ComponentHandle as _, Model as _, SharedString, VecModel};

use crate::app::preferences::{parse_font_attributes, resolve_is_light_theme, ParsedFontAttributes};
use crate::controller::anki::EntryAnkiStatus;
use crate::controller::config::ConfigController;
use crate::controller::dict::{DictController, SearchHit, TermFrequency, TermPitchDetail};
pub(super) use crate::ui::glossary_render::{
    build_selection_rects_for_cells, utf16_units_to_char_count, RenderedGlossaryDoc,
    GLOSSARY_SCAN_MAX_CHARS, MAX_NESTED_POPUP_DEPTH, NESTED_DISMISS_GRACE_MS,
};
pub(super) use crate::ui::popup_css::apply_popup_css_theme_to_bridge;
use crate::ui::popup_css::{
    InterfaceFontFallback, PopupCssTheme, PopupInterfaceFonts, StylesheetCache,
};
use crate::ui::popup_model::{resolve_pitch_mora_levels, rich_pitch_graphs_from_details};
use crate::ui::rich_db::RichDbHelper;
use crate::ui::tags::{
    extract_inline_structured_tags_from_value, suppress_redundant_tags_for_sense, tag_brush,
};
use crate::ui::text_engine::SubtitleRenderStyle;
use crate::ui::{
    DictResult, MementoApp, PopupFrequency, RichDerivationStep, RichKanjiDetail, RichPitchEntry,
    RichPitchGraph, RichPitchMora, RichTagPill, SubtitleLine, SubtitleOverlayBridge, SubtitleWord,
};

pub(super) const ROOT_GLOSSARY_CACHE_MAX_ENTRIES: usize = 384;
pub(super) const ROOT_GLOSSARY_CACHE_MAX_BYTES: usize = 32 * 1024 * 1024;
pub(super) const NESTED_GLOSSARY_CACHE_MAX_ENTRIES: usize = 128;
pub(super) const NESTED_GLOSSARY_CACHE_MAX_BYTES: usize = 12 * 1024 * 1024;
pub(super) const IDLE_POPUP_SHRINK_DELAY: Duration = Duration::from_secs(45);
pub(super) const IDLE_GLOSSARY_CACHE_MAX_ENTRIES: usize = 16;
pub(super) const IDLE_GLOSSARY_CACHE_MAX_BYTES: usize = 4 * 1024 * 1024;
pub(super) const IDLE_DICT_ASSET_CACHE_MAX_ENTRIES: usize = 32;
pub(super) const IDLE_DICT_ASSET_CACHE_MAX_BYTES: usize = 4 * 1024 * 1024;
pub(super) const IDLE_PARSED_GLOSSARY_MAX_ENTRIES: usize = 64;
pub(super) const GLOSSARY_PRERENDER_MAX_ROWS: usize = 6;
pub(super) const MAX_NAV_HISTORY: usize = 64;
pub(super) const MAX_PENDING_NESTED_ANKI_ADDS: usize = 16;
pub(super) const FRESH_CHECK_THROTTLE: Duration = Duration::from_millis(1500);

#[derive(Clone)]
pub(super) struct PopupHistoryEntry {
    pub(super) label: String,
    pub(super) rows: Vec<DictResult>,
    pub(super) hits: Vec<SearchHit>,
    pub(super) loaded_count: i32,
    pub(super) headword: String,
    pub(super) reading: String,
    pub(super) pitch_accent: String,
    pub(super) freq_rank: String,
    pub(super) jpdb_rank: String,
    pub(super) kana_reading_active: bool,
    pub(super) active_kana_reading: String,
}

#[derive(Default, Clone)]
pub(super) struct RichRowExtras {
    pub(super) hit: Option<SearchHit>,
    pub(super) parsed_glossary: Option<Arc<serde_json::Value>>,
    pub(super) term_pills: Vec<RichTagPill>,
    pub(super) def_pills: Vec<RichTagPill>,
    pub(super) derivation_steps: Vec<RichDerivationStep>,
    pub(super) pitch_graphs: Vec<RichPitchGraph>,
    pub(super) pitch_details: Vec<TermPitchDetail>,
    pub(super) pitch_entries: Vec<RichPitchEntry>,
    pub(super) pitch_category: Option<&'static str>,
    pub(super) kanji_detail: Option<RichKanjiDetail>,
    pub(super) detail_states: HashMap<String, bool>,
}

#[derive(Clone)]
pub(super) struct GlossarySelectionState {
    pub(super) row_index: i32,
    pub(super) anchor_char: usize,
    pub(super) focus_char: usize,
    pub(super) dragging: bool,
    pub(super) press_pos: (f32, f32),
}

#[derive(Clone)]
pub(super) struct HoveredSubtitleCluster {
    pub(super) word: SubtitleWord,
    pub(super) char_offset: usize,
    pub(super) global_char_index: usize,
    pub(super) global_utf16_offset: usize,
    pub(super) sub_query: String,
}

#[derive(Clone, Debug)]
pub(super) struct GlossaryScanHighlight {
    pub(super) row_index: i32,
    pub(super) cell_indices: Vec<usize>,
}

#[derive(Clone, Debug)]
pub(super) struct HoveredGlossaryTarget {
    pub(super) depth: i32,
    pub(super) row_index: i32,
    pub(super) start_char: usize,
    pub(super) query: String,
    pub(super) cell_indices: Vec<usize>,
    pub(super) anchor_x: f32,
    pub(super) anchor_y: f32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PendingNestedAnkiAdd {
    pub(super) depth: usize,
    pub(super) row_index: usize,
    pub(super) expression: String,
    pub(super) reading: String,
    pub(super) is_kanji: bool,
    pub(super) reading_as_expression: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PendingPanelAnkiAdd {
    pub(super) row_index: usize,
    pub(super) expression: String,
    pub(super) reading: String,
    pub(super) is_kanji: bool,
    pub(super) reading_as_expression: bool,
}

#[derive(Default)]
pub(super) struct NestedPopupLevel {
    pub(super) label: String,
    pub(super) rows: Vec<DictResult>,
    pub(super) hits: Vec<SearchHit>,
    pub(super) headword: String,
    pub(super) reading: String,
    pub(super) anchor_x: f32,
    pub(super) anchor_y: f32,
    pub(super) locked: bool,
    pub(super) source_row_index: i32,
    pub(super) source_start_char: usize,
    pub(super) rich_epoch: u64,
    pub(super) row_extras: HashMap<i32, RichRowExtras>,
    pub(super) glossary_cache: GlossaryDocLruCache,
    pub(super) last_rendered_key: HashMap<i32, GlossaryDocCacheKey>,
    pub(super) selections: BTreeMap<(usize, usize), GlossarySelectionState>,
    pub(super) active_drag_selection_key: Option<(usize, usize)>,
    pub(super) scan_highlight: Option<GlossaryScanHighlight>,
    pub(super) expanded_cards: HashSet<i32>,
    pub(super) row_anki_duplicates: Vec<bool>,
    pub(super) row_anki_note_ids_i64: Vec<i64>,
    pub(super) row_anki_browse_queries: Vec<SharedString>,
    pub(super) row_anki_tags: Vec<SharedString>,
    pub(super) row_anki_addable: Vec<bool>,
    pub(super) row_reading_as_expression: Vec<bool>,
    pub(super) term_kanji_statuses: HashMap<(String, String), EntryAnkiStatus>,
    pub(super) term_kana_statuses: HashMap<(String, String), EntryAnkiStatus>,
    pub(super) manual_reading_as_expr: HashMap<(String, String), bool>,
}

/// Independent rich popup state backing `DictionarySearchPanel` (`scan-depth == -1`).
#[derive(Default)]
pub(super) struct PanelPopupState {
    pub(super) rows: Vec<DictResult>,
    pub(super) hits: Vec<SearchHit>,
    pub(super) rich_epoch: u64,
    pub(super) row_extras: HashMap<i32, RichRowExtras>,
    pub(super) glossary_cache: GlossaryDocLruCache,
    pub(super) last_rendered_key: HashMap<i32, GlossaryDocCacheKey>,
    pub(super) selections: BTreeMap<(usize, usize), GlossarySelectionState>,
    pub(super) active_drag_selection_key: Option<(usize, usize)>,
    pub(super) expanded_cards: HashSet<i32>,
    pub(super) row_anki_duplicates: Vec<bool>,
    pub(super) row_anki_note_ids_i64: Vec<i64>,
    pub(super) row_anki_browse_queries: Vec<SharedString>,
    pub(super) row_anki_tags: Vec<SharedString>,
    pub(super) row_anki_addable: Vec<bool>,
    pub(super) row_reading_as_expression: Vec<bool>,
    pub(super) term_kanji_statuses: HashMap<(String, String), EntryAnkiStatus>,
    pub(super) term_kana_statuses: HashMap<(String, String), EntryAnkiStatus>,
    pub(super) manual_reading_as_expr: HashMap<(String, String), bool>,
    pub(super) last_panel_scan: String,
}

pub(super) struct PanelSearchRequest {
    pub(super) gen_id: u64,
    pub(super) query: String,
    pub(super) kana_only: bool,
    pub(super) kanji_only: bool,
    pub(super) excluded: Vec<String>,
    pub(super) search_cfg: DictSearchConfig,
}

pub(super) struct PanelSearchResponse {
    pub(super) gen_id: u64,
    pub(super) hits: Vec<SearchHit>,
    pub(super) excluded: Vec<String>,
    pub(super) term_meta_rows: Vec<(
        String,
        String,
        Vec<crate::controller::dict::TermPitchGraph>,
        Vec<TermFrequency>,
    )>,
    pub(super) term_pitch_details: HashMap<(String, String), Vec<TermPitchDetail>>,
    pub(super) kanji_frequencies: HashMap<String, Vec<TermFrequency>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AnkiCheckTarget {
    Root,
    Nested(usize),
    Panel,
}

pub(super) struct AnkiPopupCheckRequest {
    pub(super) nav_epoch: u64,
    pub(super) lookup_epoch: Option<u64>,
    pub(super) target: AnkiCheckTarget,
    pub(super) show_tags: bool,
    pub(super) term_profile: Option<crate::controller::config::AnkiNoteConfig>,
    pub(super) kanji_profile: Option<crate::controller::config::AnkiNoteConfig>,
    pub(super) term_entries: Vec<(String, String)>,
    pub(super) kanji_entries: Vec<(String, String)>,
}

pub(super) struct AnkiPopupCheckResponse {
    pub(super) nav_epoch: u64,
    pub(super) lookup_epoch: Option<u64>,
    pub(super) target: AnkiCheckTarget,
    pub(super) show_tags: bool,
    pub(super) term_statuses: HashMap<(String, String), EntryAnkiStatus>,
    pub(super) term_kana_statuses: HashMap<(String, String), EntryAnkiStatus>,
    pub(super) kanji_statuses: HashMap<String, EntryAnkiStatus>,
}

pub(super) struct NestedScanRequest {
    pub(super) epoch: u64,
    pub(super) parent_depth: i32,
    pub(super) row_index: i32,
    pub(super) start_char: usize,
    pub(super) query: String,
    pub(super) cell_indices: Vec<usize>,
    pub(super) anchor_x: f32,
    pub(super) anchor_y: f32,
    pub(super) locked: bool,
    pub(super) excluded_glossaries: Vec<String>,
    pub(super) search_config: DictSearchConfig,
}

pub(super) struct NestedScanResponse {
    pub(super) epoch: u64,
    pub(super) parent_depth: i32,
    pub(super) row_index: i32,
    pub(super) start_char: usize,
    pub(super) matched_char_count: usize,
    pub(super) cell_indices: Vec<usize>,
    pub(super) anchor_x: f32,
    pub(super) anchor_y: f32,
    pub(super) locked: bool,
    pub(super) hits: Vec<SearchHit>,
    pub(super) excluded_glossaries: Vec<String>,
    pub(super) headword: String,
    pub(super) reading: String,
    pub(super) term_meta_rows: Vec<(
        String,
        String,
        Vec<crate::controller::dict::TermPitchGraph>,
        Vec<TermFrequency>,
    )>,
    pub(super) term_pitch_details: HashMap<(String, String), Vec<TermPitchDetail>>,
    pub(super) kanji_frequencies: HashMap<String, Vec<TermFrequency>>,
}

pub(super) enum NavQueryKind {
    RootTerm {
        query: String,
        kana_mode: Option<String>,
    },
    RootKanji {
        query: String,
    },
    NestedTerm {
        from_depth: i32,
        query: String,
    },
    NestedKanji {
        from_depth: i32,
        query: String,
    },
}

impl NavQueryKind {
    pub(super) fn is_nested(&self) -> bool {
        matches!(self, Self::NestedTerm { .. } | Self::NestedKanji { .. })
    }
}

pub(super) struct NavLookupRequest {
    pub(super) epoch: u64,
    pub(super) kind: NavQueryKind,
    pub(super) excluded: Vec<String>,
    pub(super) search_config: DictSearchConfig,
}

pub(super) struct NavLookupResponse {
    pub(super) epoch: u64,
    pub(super) kind: NavQueryKind,
    pub(super) hits: Vec<SearchHit>,
    pub(super) excluded: Vec<String>,
    pub(super) headword: String,
    pub(super) reading: String,
    pub(super) pitch: String,
    pub(super) freq: String,
    pub(super) jpdb: String,
    pub(super) term_meta_rows: Vec<(
        String,
        String,
        Vec<crate::controller::dict::TermPitchGraph>,
        Vec<TermFrequency>,
    )>,
    pub(super) term_pitch_details: HashMap<(String, String), Vec<TermPitchDetail>>,
    pub(super) kanji_frequencies: HashMap<String, Vec<TermFrequency>>,
}

pub(super) enum DictWorkerRequest {
    NestedScan(NestedScanRequest),
    NavLookup(NavLookupRequest),
}

pub(super) struct GlossaryPreRenderItem {
    pub(super) cache_key: GlossaryDocCacheKey,
    pub(super) raw_glossary: String,
    pub(super) fallback_glossary: String,
    pub(super) parsed_glossary: Option<Arc<serde_json::Value>>,
    pub(super) stylesheet: Option<Arc<memento_definition::ParsedStylesheet>>,
    pub(super) detail_states: HashMap<String, bool>,
    pub(super) legacy_style: LegacyGlossaryStyle,
    pub(super) dict_resource_dirs: Vec<PathBuf>,
    pub(super) dic_id: i64,
    pub(super) dict_name: String,
    pub(super) avail_width_px: f32,
    pub(super) selected: bool,
    pub(super) theme: Arc<PopupCssTheme>,
    pub(super) render_scale: f32,
}

pub(super) struct GlossaryPreRenderBatch {
    pub(super) epoch: u64,
    pub(super) items: Vec<GlossaryPreRenderItem>,
}

pub(super) struct GlossaryPreRenderDone {
    pub(super) epoch: u64,
    pub(super) cache_key: GlossaryDocCacheKey,
    pub(super) doc: crate::ui::glossary_render::RasterizedGlossaryDoc,
}

pub(super) fn collect_hits_term_and_kanji_meta(
    dict: &DictController,
    hits: &[SearchHit],
) -> (
    Vec<(
        String,
        String,
        Vec<crate::controller::dict::TermPitchGraph>,
        Vec<TermFrequency>,
    )>,
    HashMap<(String, String), Vec<TermPitchDetail>>,
    HashMap<String, Vec<TermFrequency>>,
) {
    let mut term_meta_rows = Vec::new();
    let mut term_pitch_details = HashMap::new();
    let mut kanji_frequencies = HashMap::new();
    for h in hits {
        if h.is_kanji {
            if !h.expression.is_empty() && !kanji_frequencies.contains_key(&h.expression) {
                let freqs = dict.get_kanji_frequencies(&h.expression);
                kanji_frequencies.insert(h.expression.clone(), freqs);
            }
        } else {
            let key = (h.expression.clone(), h.reading.clone());
            if !term_pitch_details.contains_key(&key) {
                let (details, freqs) =
                    dict.get_pitch_details_and_frequencies(&h.expression, &h.reading);
                let graphs: Vec<crate::controller::dict::TermPitchGraph> = details
                    .iter()
                    .map(|detail| crate::controller::dict::TermPitchGraph {
                        dictionary: detail.dictionary.clone(),
                        mora: detail.mora.clone(),
                        position: detail.position,
                    })
                    .collect();
                term_meta_rows.push((h.expression.clone(), h.reading.clone(), graphs, freqs));
                term_pitch_details.insert(key, details);
            }
        }
    }
    (term_meta_rows, term_pitch_details, kanji_frequencies)
}

pub(super) fn execute_nav_lookup(
    dict: &mut DictController,
    req: NavLookupRequest,
) -> NavLookupResponse {
    match req.kind {
        NavQueryKind::RootTerm {
            ref query,
            ref kana_mode,
        } => {
            let raw_hits = dict.search(query).to_vec();
            let mut hits = if let Some(kana_target) = kana_mode.as_deref() {
                let norm = crate::controller::dict::normalize_reading(kana_target);
                let filtered: Vec<_> = raw_hits
                    .iter()
                    .filter(|h| {
                        crate::controller::dict::normalize_reading(&h.reading) == norm
                            || crate::controller::dict::normalize_reading(&h.expression) == norm
                    })
                    .cloned()
                    .collect();
                if filtered.is_empty() {
                    raw_hits
                } else {
                    filtered
                }
            } else {
                raw_hits
            };
            if hits.is_empty() && query.chars().count() == 1 {
                hits = dict.search_kanji(query).to_vec();
            }
            let headword = hits
                .first()
                .map(|h| h.expression.clone())
                .unwrap_or_else(|| query.clone());
            let reading = hits.first().map(|h| h.reading.clone()).unwrap_or_default();
            let (pitch, freq, jpdb) = if !headword.is_empty() {
                dict.get_term_meta(&headword, &reading)
            } else {
                (String::new(), String::new(), String::new())
            };
            let (term_meta_rows, term_pitch_details, kanji_frequencies) =
                collect_hits_term_and_kanji_meta(dict, &hits);
            NavLookupResponse {
                epoch: req.epoch,
                kind: req.kind,
                hits,
                excluded: req.excluded,
                headword,
                reading,
                pitch,
                freq,
                jpdb,
                term_meta_rows,
                term_pitch_details,
                kanji_frequencies,
            }
        }
        NavQueryKind::RootKanji { ref query } => {
            let hits = dict.search_kanji(query).to_vec();
            let reading = hits.first().map(|h| h.reading.clone()).unwrap_or_default();
            let headword = query.clone();
            let mut kanji_frequencies = HashMap::new();
            if !hits.is_empty() {
                kanji_frequencies.insert(query.clone(), dict.get_kanji_frequencies(query));
            }
            NavLookupResponse {
                epoch: req.epoch,
                kind: req.kind,
                hits,
                excluded: req.excluded,
                headword,
                reading,
                pitch: String::new(),
                freq: String::new(),
                jpdb: String::new(),
                term_meta_rows: Vec::new(),
                term_pitch_details: HashMap::new(),
                kanji_frequencies,
            }
        }
        NavQueryKind::NestedTerm { ref query, .. } => {
            let mut hits = dict.search(query).to_vec();
            if hits.is_empty() && query.chars().count() == 1 {
                hits = dict.search_kanji(query).to_vec();
            }
            let headword = hits
                .first()
                .map(|h| h.expression.clone())
                .unwrap_or_else(|| query.clone());
            let reading = hits.first().map(|h| h.reading.clone()).unwrap_or_default();
            let (term_meta_rows, term_pitch_details, kanji_frequencies) =
                collect_hits_term_and_kanji_meta(dict, &hits);
            NavLookupResponse {
                epoch: req.epoch,
                kind: req.kind,
                hits,
                excluded: req.excluded,
                headword,
                reading,
                pitch: String::new(),
                freq: String::new(),
                jpdb: String::new(),
                term_meta_rows,
                term_pitch_details,
                kanji_frequencies,
            }
        }
        NavQueryKind::NestedKanji { ref query, .. } => {
            let hits = dict.search_kanji(query).to_vec();
            let reading = hits.first().map(|h| h.reading.clone()).unwrap_or_default();
            let headword = query.clone();
            let mut kanji_frequencies = HashMap::new();
            if !hits.is_empty() {
                kanji_frequencies.insert(query.clone(), dict.get_kanji_frequencies(query));
            }
            NavLookupResponse {
                epoch: req.epoch,
                kind: req.kind,
                hits,
                excluded: req.excluded,
                headword,
                reading,
                pitch: String::new(),
                freq: String::new(),
                jpdb: String::new(),
                term_meta_rows: Vec::new(),
                term_pitch_details: HashMap::new(),
                kanji_frequencies,
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DictSearchConfig {
    pub(super) dict_order: Vec<i64>,
    pub(super) exact_enabled: bool,
    pub(super) deconj_enabled: bool,
    pub(super) mecab_enabled: bool,
    pub(super) remove_regex: String,
    pub(super) replace_lines: bool,
    pub(super) replace_with: String,
    pub(super) text_replacements: TextReplacementOptions,
}

impl DictSearchConfig {
    pub(super) fn apply_to(&self, dict: &mut DictController) {
        dict.set_order(&self.dict_order);
        dict.set_exact_enabled(self.exact_enabled);
        dict.set_deconj_enabled(self.deconj_enabled);
        dict.set_mecab_enabled(self.mecab_enabled);
        dict.set_search_text_filters(&self.remove_regex, self.replace_lines, &self.replace_with);
        dict.set_text_replacements(self.text_replacements.clone());
    }
}

pub(super) struct CachedConfigSettings {
    pub(super) subtitle_style: SubtitleRenderStyle,
    pub(super) subtitle_font_attrs: ParsedFontAttributes,
    pub(super) popup_fonts: PopupInterfaceFonts,
    pub(super) popup_font_attrs: HashMap<&'static str, ParsedFontAttributes>,
    pub(super) is_light_theme: bool,
    pub(super) compact_glossaries: bool,
    pub(super) compact_tags: bool,
    pub(super) search_method: i32,
    pub(super) search_modifier: i32,
    pub(super) search_delay_ms: u64,
    pub(super) middle_mouse_scan: bool,
    pub(super) max_recursion_depth: usize,
    pub(super) exact_enabled: bool,
    pub(super) deconj_enabled: bool,
    pub(super) mecab_enabled: bool,
    pub(super) remove_regex: String,
    pub(super) replace_lines: bool,
    pub(super) replace_with: String,
    pub(super) text_replacements: TextReplacementOptions,
    pub(super) glossary_style: LegacyGlossaryStyle,
    pub(super) dict_order: Vec<i64>,
    pub(super) excluded_glossaries: Vec<String>,
    pub(super) anki_enabled: bool,
    pub(super) anki_show_tag: bool,
    pub(super) anki_term_profile: Option<crate::controller::config::AnkiNoteConfig>,
    pub(super) anki_kanji_profile: Option<crate::controller::config::AnkiNoteConfig>,
    pub(super) conf_mtime: Option<SystemTime>,
    pub(super) anki_mtime: Option<SystemTime>,
}

impl CachedConfigSettings {
    pub(super) fn from_controller(config: &ConfigController, config_dir: &Path) -> Self {
        let mut out = Self {
            subtitle_style: SubtitleRenderStyle::default(),
            subtitle_font_attrs: ParsedFontAttributes::default(),
            popup_fonts: PopupInterfaceFonts::default(),
            popup_font_attrs: HashMap::new(),
            is_light_theme: false,
            compact_glossaries: raw::interface::COMPACT_GLOSSARIES_DEFAULT,
            compact_tags: raw::interface::COMPACT_TAGS_DEFAULT,
            search_method: raw::search::METHOD_DEFAULT as i32,
            search_modifier: raw::search::MODIFIER_DEFAULT as i32,
            search_delay_ms: raw::search::DELAY_DEFAULT as u64,
            middle_mouse_scan: raw::search::MIDDLE_MOUSE_SCAN_DEFAULT,
            max_recursion_depth: MAX_NESTED_POPUP_DEPTH,
            exact_enabled: raw::search::matcher::EXACT_DEFAULT,
            deconj_enabled: raw::search::matcher::DECONJ_DEFAULT,
            mecab_enabled: raw::search::matcher::MECAB_IPADIC_DEFAULT,
            remove_regex: raw::search::REMOVE_REGEX_DEFAULT.to_owned(),
            replace_lines: raw::search::REPLACE_NEWLINES_DEFAULT,
            replace_with: raw::search::REPLACE_NEWLINES_WITH_DEFAULT.to_owned(),
            text_replacements: TextReplacementOptions::default(),
            glossary_style: LegacyGlossaryStyle::Bullet,
            dict_order: Vec::new(),
            excluded_glossaries: Vec::new(),
            anki_enabled: false,
            anki_show_tag: false,
            anki_term_profile: None,
            anki_kanji_profile: None,
            conf_mtime: std::fs::metadata(config_dir.join("memento.conf"))
                .ok()
                .and_then(|m| m.modified().ok()),
            anki_mtime: std::fs::metadata(config_dir.join("anki_connect.json"))
                .ok()
                .and_then(|m| m.modified().ok()),
        };
        out.apply_controller(config);
        out
    }

    pub(super) fn dict_search_config(&self) -> DictSearchConfig {
        DictSearchConfig {
            dict_order: self.dict_order.clone(),
            exact_enabled: self.exact_enabled,
            deconj_enabled: self.deconj_enabled,
            mecab_enabled: self.mecab_enabled,
            remove_regex: self.remove_regex.clone(),
            replace_lines: self.replace_lines,
            replace_with: self.replace_with.clone(),
            text_replacements: self.text_replacements.clone(),
        }
    }

    pub(super) fn apply_controller(&mut self, config: &ConfigController) {
        use memento_config::Value;
        self.is_light_theme = resolve_is_light_theme(config);
        self.compact_glossaries = match config
            .effective_setting(raw::interface::GROUP, raw::interface::COMPACT_GLOSSARIES)
        {
            Some(Value::Bool(b)) => b,
            _ => raw::interface::COMPACT_GLOSSARIES_DEFAULT,
        };
        self.compact_tags =
            match config.effective_setting(raw::interface::GROUP, raw::interface::COMPACT_TAGS) {
                Some(Value::Bool(b)) => b,
                _ => raw::interface::COMPACT_TAGS_DEFAULT,
            };
        if let Some(Value::Text(font_csv)) =
            config.effective_setting(raw::interface::GROUP, raw::interface::subtitle::FONT)
        {
            let parts: Vec<&str> = font_csv.split(',').collect();
            if let Some(fam) = parts.first().map(|s| s.trim()).filter(|s| !s.is_empty()) {
                self.subtitle_style.font_family = fam.to_owned();
            }
            if let Some(w_str) = parts.get(4).map(|s| s.trim()) {
                if let Ok(w) = w_str.parse::<i32>() {
                    self.subtitle_style.bold = w >= 65 || w >= 600;
                }
            }
            if let Some(i_str) = parts.get(5).map(|s| s.trim()) {
                self.subtitle_style.italic = i_str == "1" || i_str.eq_ignore_ascii_case("true");
            }
            self.subtitle_font_attrs =
                parse_font_attributes(&font_csv, "Noto Sans CJK JP", 24.0, 8.0, 200.0);
        }
        if let Some(Value::Text(c)) =
            config.effective_setting(raw::interface::GROUP, raw::interface::subtitle::TEXT_COLOR)
        {
            self.subtitle_style.text_rgba = parse_def_rgba(&c, [255, 255, 255, 255]);
        }
        if let Some(Value::Text(c)) =
            config.effective_setting(raw::interface::GROUP, raw::interface::subtitle::STROKE_COLOR)
        {
            self.subtitle_style.stroke_rgba = parse_def_rgba(&c, [0, 0, 0, 255]);
        }
        if let Some(Value::Text(c)) = config.effective_setting(
            raw::interface::GROUP,
            raw::interface::subtitle::BACKGROUND_COLOR,
        ) {
            self.subtitle_style.bg_rgba = parse_def_rgba(&c, [0, 0, 0, 0]);
        }
        if let Some(v) =
            config.effective_setting(raw::interface::GROUP, raw::interface::subtitle::STROKE)
        {
            let num = match v {
                Value::Real(f) => Some(f as f32),
                Value::Int(i) => Some(i as f32),
                _ => None,
            };
            if let Some(n) = num {
                self.subtitle_style.stroke_size = n.max(0.0);
            }
        }
        if let Some(v) = config.effective_setting(
            raw::interface::GROUP,
            raw::interface::subtitle::FURIGANA_RATIO,
        ) {
            let num = match v {
                Value::Real(f) => Some(f as f32),
                Value::Int(i) => Some(i as f32),
                _ => None,
            };
            if let Some(n) = num {
                self.subtitle_style.furigana_ratio = n.clamp(0.2, 0.8);
            }
        }
        if let Some(v) =
            config.effective_setting(raw::interface::GROUP, raw::interface::subtitle::LINE_SPACING)
        {
            let num = match v {
                Value::Real(f) => Some(f as f32),
                Value::Int(i) => Some(i as f32),
                _ => None,
            };
            if let Some(n) = num {
                self.subtitle_style.line_spacing = n.clamp(0.0, 120.0);
            }
        }
        let stock_config = ConfigController::new();
        self.popup_font_attrs.clear();
        for key in [
            raw::interface::SEARCH_EXPRESSION_FONT,
            raw::interface::SEARCH_READING_FONT,
            raw::interface::SEARCH_CONJ_FONT,
            raw::interface::SEARCH_TAG_FONT,
            raw::interface::SEARCH_GLOSSARY_FONT,
            raw::interface::SEARCH_KANJI_FONT,
        ] {
            if let Some(Value::Text(raw_csv)) = config.effective_setting(raw::interface::GROUP, key)
            {
                let attrs =
                    parse_font_attributes(&raw_csv, "Noto Sans CJK JP", 14.0, 8.0, 200.0);
                if attrs.underline || attrs.strikeout || attrs.letter_spacing.abs() > f32::EPSILON {
                    self.popup_font_attrs.insert(key, attrs);
                }
            }
        }
        self.popup_fonts = PopupInterfaceFonts {
            expression: parse_interface_font_setting(
                config,
                &stock_config,
                raw::interface::SEARCH_EXPRESSION_FONT,
                12.0,
                48.0,
            ),
            reading: parse_interface_font_setting(
                config,
                &stock_config,
                raw::interface::SEARCH_READING_FONT,
                8.0,
                28.0,
            ),
            conj: parse_interface_font_setting(
                config,
                &stock_config,
                raw::interface::SEARCH_CONJ_FONT,
                8.0,
                24.0,
            ),
            tag: parse_interface_font_setting(
                config,
                &stock_config,
                raw::interface::SEARCH_TAG_FONT,
                8.0,
                22.0,
            ),
            glossary: parse_interface_font_setting(
                config,
                &stock_config,
                raw::interface::SEARCH_GLOSSARY_FONT,
                9.0,
                32.0,
            ),
            kanji: parse_interface_font_setting(
                config,
                &stock_config,
                raw::interface::SEARCH_KANJI_FONT,
                16.0,
                200.0,
            ),
        };
        if let Some(Value::Int(m)) =
            config.effective_setting(raw::search::GROUP, raw::search::METHOD)
        {
            self.search_method = m as i32;
        }
        if let Some(Value::Int(m)) =
            config.effective_setting(raw::search::GROUP, raw::search::MODIFIER)
        {
            self.search_modifier = m as i32;
        }
        if let Some(Value::Int(d)) =
            config.effective_setting(raw::search::GROUP, raw::search::DELAY)
        {
            self.search_delay_ms = d.max(0) as u64;
        }
        if let Some(Value::Bool(b)) =
            config.effective_setting(raw::search::GROUP, raw::search::MIDDLE_MOUSE_SCAN)
        {
            self.middle_mouse_scan = b;
        }
        self.max_recursion_depth =
            match config.effective_setting(raw::search::GROUP, raw::search::MAX_RECURSION_DEPTH) {
                Some(Value::Int(d)) => d.clamp(0, 10) as usize,
                Some(Value::Real(f)) => (f.round() as i64).clamp(0, 10) as usize,
                Some(Value::Text(s)) => s
                    .trim()
                    .parse::<i64>()
                    .map(|d| d.clamp(0, 10) as usize)
                    .unwrap_or(MAX_NESTED_POPUP_DEPTH),
                _ => MAX_NESTED_POPUP_DEPTH,
            };
        self.exact_enabled = match config
            .effective_setting(raw::search::GROUP, raw::search::matcher::EXACT)
        {
            Some(Value::Bool(b)) => b,
            _ => raw::search::matcher::EXACT_DEFAULT,
        };
        self.deconj_enabled = match config
            .effective_setting(raw::search::GROUP, raw::search::matcher::DECONJ)
        {
            Some(Value::Bool(b)) => b,
            _ => raw::search::matcher::DECONJ_DEFAULT,
        };
        self.mecab_enabled = match config
            .effective_setting(raw::search::GROUP, raw::search::matcher::MECAB_IPADIC)
        {
            Some(Value::Bool(b)) => b,
            _ => raw::search::matcher::MECAB_IPADIC_DEFAULT,
        };
        self.remove_regex = match config
            .effective_setting(raw::search::GROUP, raw::search::REMOVE_REGEX)
        {
            Some(Value::Text(s)) => s,
            _ => raw::search::REMOVE_REGEX_DEFAULT.to_owned(),
        };
        self.replace_lines = match config
            .effective_setting(raw::search::GROUP, raw::search::REPLACE_NEWLINES)
        {
            Some(Value::Bool(b)) => b,
            _ => raw::search::REPLACE_NEWLINES_DEFAULT,
        };
        self.replace_with = match config
            .effective_setting(raw::search::GROUP, raw::search::REPLACE_NEWLINES_WITH)
        {
            Some(Value::Text(s)) => s,
            _ => raw::search::REPLACE_NEWLINES_WITH_DEFAULT.to_owned(),
        };
        self.text_replacements = config.text_replacement_options();
        if let Some(Value::Int(gs)) =
            config.effective_setting(raw::search::GROUP, raw::search::GLOSSARY_STYLE)
        {
            self.glossary_style = match gs {
                0 => LegacyGlossaryStyle::Bullet,
                1 => LegacyGlossaryStyle::LineBreak,
                2 => LegacyGlossaryStyle::Pipe,
                _ => LegacyGlossaryStyle::NoSeparator,
            };
        }
        if let Some(Value::Ints(order)) =
            config.effective_setting(raw::dictionary::GROUP, raw::dictionary::ORDER)
        {
            self.dict_order = order;
        }
        self.excluded_glossaries = config.anki_excluded_glossaries();
        self.anki_enabled = config.anki_enabled();
        self.anki_show_tag = config.anki_show_tag();
        self.anki_term_profile = config.anki_profile_note_config();
        self.anki_kanji_profile = config.anki_kanji_note_config();
    }

    pub(super) fn reload_if_changed_on_disk(&mut self, config_dir: &Path) -> bool {
        let conf_mtime = std::fs::metadata(config_dir.join("memento.conf"))
            .ok()
            .and_then(|m| m.modified().ok());
        let anki_mtime = std::fs::metadata(config_dir.join("anki_connect.json"))
            .ok()
            .and_then(|m| m.modified().ok());
        if conf_mtime == self.conf_mtime && anki_mtime == self.anki_mtime {
            return false;
        }
        self.conf_mtime = conf_mtime;
        self.anki_mtime = anki_mtime;
        if let Ok(reloaded) = ConfigController::load(config_dir, "memento.conf") {
            let _ = reloaded.load_anki_profiles(config_dir.join("anki_connect.json"));
            self.apply_controller(&reloaded);
            return true;
        }
        false
    }
}

#[derive(Hash, PartialEq, Eq, Clone)]
pub(super) struct GlossaryDocCacheKey {
    pub(super) row_index: i32,
    pub(super) width_px: i32,
    pub(super) selected: bool,
    pub(super) details_sig: String,
    pub(super) rich_epoch: u64,
    pub(super) scale_quarter: u16,
    pub(super) css_theme_light: bool,
    pub(super) compact_glossaries: bool,
}

struct GlossaryCacheEntry {
    doc: RenderedGlossaryDoc,
    bytes: usize,
    last_used: u64,
}

/// Bounded LRU cache for rendered glossary cards (capped by both entry count and
/// approximate RGBA pixel memory) so large result sets never suffer cliff `.clear()` thrash.
pub(super) struct GlossaryDocLruCache {
    entries: HashMap<GlossaryDocCacheKey, GlossaryCacheEntry>,
    total_bytes: usize,
    max_entries: usize,
    max_bytes: usize,
    clock: u64,
}

impl Default for GlossaryDocLruCache {
    fn default() -> Self {
        Self::new_nested()
    }
}

impl GlossaryDocLruCache {
    pub(super) fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            total_bytes: 0,
            max_entries: max_entries.max(1),
            max_bytes: max_bytes.max(1024 * 1024),
            clock: 0,
        }
    }

    pub(super) fn new_root() -> Self {
        Self::new(ROOT_GLOSSARY_CACHE_MAX_ENTRIES, ROOT_GLOSSARY_CACHE_MAX_BYTES)
    }

    pub(super) fn new_nested() -> Self {
        Self::new(
            NESTED_GLOSSARY_CACHE_MAX_ENTRIES,
            NESTED_GLOSSARY_CACHE_MAX_BYTES,
        )
    }

    fn approx_doc_bytes(doc: &RenderedGlossaryDoc) -> usize {
        let sz = doc.image.size();
        (sz.width as usize)
            .saturating_mul(sz.height as usize)
            .saturating_mul(4)
            .max(1024)
    }

    pub(super) fn get(&mut self, key: &GlossaryDocCacheKey) -> Option<RenderedGlossaryDoc> {
        let entry = self.entries.get_mut(key)?;
        self.clock = self.clock.wrapping_add(1);
        entry.last_used = self.clock;
        Some(entry.doc.clone())
    }

    pub(super) fn peek(&self, key: &GlossaryDocCacheKey) -> Option<RenderedGlossaryDoc> {
        self.entries.get(key).map(|e| e.doc.clone())
    }

    pub(super) fn contains_key(&self, key: &GlossaryDocCacheKey) -> bool {
        self.entries.contains_key(key)
    }

    pub(super) fn find_by_row_and_epoch(
        &self,
        row_index: i32,
        rich_epoch: u64,
    ) -> Option<RenderedGlossaryDoc> {
        self.entries
            .iter()
            .filter(|(k, _)| k.row_index == row_index && k.rich_epoch == rich_epoch)
            .max_by_key(|(_, entry)| entry.last_used)
            .map(|(_, entry)| entry.doc.clone())
    }

    pub(super) fn insert(&mut self, key: GlossaryDocCacheKey, doc: RenderedGlossaryDoc) {
        let bytes = Self::approx_doc_bytes(&doc);
        self.clock = self.clock.wrapping_add(1);
        let stamp = self.clock;
        if let Some(prev) = self.entries.insert(
            key,
            GlossaryCacheEntry {
                doc,
                bytes,
                last_used: stamp,
            },
        ) {
            self.total_bytes = self.total_bytes.saturating_sub(prev.bytes);
        }
        self.total_bytes = self.total_bytes.saturating_add(bytes);
        self.evict_if_needed();
    }

    fn evict_if_needed(&mut self) {
        while (self.entries.len() > self.max_entries || self.total_bytes > self.max_bytes)
            && self.entries.len() > 1
        {
            let Some(victim) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            if let Some(removed) = self.entries.remove(&victim) {
                self.total_bytes = self.total_bytes.saturating_sub(removed.bytes);
            } else {
                break;
            }
        }
    }

    pub(super) fn retain(&mut self, mut predicate: impl FnMut(&GlossaryDocCacheKey) -> bool) {
        self.entries.retain(|k, _| predicate(k));
        self.total_bytes = self.entries.values().map(|e| e.bytes).sum();
    }

    pub(super) fn shrink_to(&mut self, max_entries: usize, max_bytes: usize) {
        while self.entries.len() > max_entries || self.total_bytes > max_bytes {
            let Some(victim) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            if let Some(removed) = self.entries.remove(&victim) {
                self.total_bytes = self.total_bytes.saturating_sub(removed.bytes);
            } else {
                break;
            }
        }
        self.entries.shrink_to(max_entries.max(16));
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();
        self.total_bytes = 0;
    }
}

/// Cached O(1) lookup tables for active subtitle word UTF-16/char offsets and
/// same-line neighbor flags, keyed by Slint `ModelRc` pointer identity.
#[derive(Default)]
pub(super) struct SubtitleTokenLayoutCache {
    words_model: Option<slint::ModelRc<SubtitleWord>>,
    token_u16_starts: Vec<usize>,
    token_char_starts: Vec<usize>,
    lines_model: Option<slint::ModelRc<SubtitleLine>>,
    line_neighbors: HashMap<i32, (bool, bool)>,
}

impl SubtitleTokenLayoutCache {
    pub(super) fn clear(&mut self) {
        self.words_model = None;
        self.token_u16_starts.clear();
        self.token_char_starts.clear();
        self.lines_model = None;
        self.line_neighbors.clear();
    }

    fn ensure_words(&mut self, words: &slint::ModelRc<SubtitleWord>) {
        let count = words.row_count();
        if self.words_model.as_ref() == Some(words) && self.token_u16_starts.len() == count {
            return;
        }
        self.token_u16_starts.clear();
        self.token_char_starts.clear();
        self.token_u16_starts.reserve(count);
        self.token_char_starts.reserve(count);
        let mut acc_u16 = 0usize;
        let mut acc_chars = 0usize;
        for i in 0..count {
            self.token_u16_starts.push(acc_u16);
            self.token_char_starts.push(acc_chars);
            if let Some(w) = words.row_data(i) {
                acc_u16 += w.text.encode_utf16().count();
                acc_chars += w.text.chars().count();
            }
        }
        self.words_model = Some(words.clone());
    }

    pub(super) fn token_offsets(
        &mut self,
        words: &slint::ModelRc<SubtitleWord>,
        token_idx: usize,
    ) -> (usize, usize) {
        self.ensure_words(words);
        let count = self.token_u16_starts.len();
        if count == 0 {
            return (0, 0);
        }
        if token_idx < count {
            (
                self.token_char_starts[token_idx],
                self.token_u16_starts[token_idx],
            )
        } else {
            // If `token_idx >= count`, match the total prefix across all words in the model.
            let last = count - 1;
            let (last_chars, last_u16) = words.row_data(last).map_or((0, 0), |w| {
                (w.text.chars().count(), w.text.encode_utf16().count())
            });
            (
                self.token_char_starts[last] + last_chars,
                self.token_u16_starts[last] + last_u16,
            )
        }
    }

    pub(super) fn line_neighbors(
        &mut self,
        lines: &slint::ModelRc<SubtitleLine>,
        word_index: i32,
    ) -> (bool, bool) {
        if self.lines_model.as_ref() != Some(lines) {
            self.line_neighbors.clear();
            for li in 0..lines.row_count() {
                if let Some(line) = lines.row_data(li) {
                    let count = line.words.row_count();
                    for wi in 0..count {
                        if let Some(w) = line.words.row_data(wi) {
                            self.line_neighbors
                                .insert(w.index, (wi > 0, wi + 1 < count));
                        }
                    }
                }
            }
            self.lines_model = Some(lines.clone());
        }
        self.line_neighbors
            .get(&word_index)
            .copied()
            .unwrap_or((false, false))
    }
}

pub(super) struct RichUiMutable {
    pub(super) config_dir: PathBuf,
    pub(super) dict_resource_dirs: Vec<PathBuf>,
    pub(super) settings: CachedConfigSettings,
    pub(super) dict_ctrl: DictController,
    pub(super) applied_search_cfg: Option<DictSearchConfig>,
    pub(super) db_sig: crate::controller::dict::DbFileFingerprint,
    pub(super) db_helper: RichDbHelper,
    pub(super) stylesheets: StylesheetCache,
    pub(super) cached_theme: Arc<PopupCssTheme>,
    pub(super) cached_theme_fonts: PopupInterfaceFonts,
    pub(super) cached_theme_is_light: bool,
    pub(super) cached_compact_glossaries: bool,
    pub(super) cached_compact_tags: bool,
    pub(super) last_fresh_check: Option<Instant>,

    pub(super) rich_epoch: u64,
    pub(super) row_extras: HashMap<i32, RichRowExtras>,
    pub(super) current_hits: Vec<SearchHit>,
    pub(super) glossary_cache: GlossaryDocLruCache,
    pub(super) last_rendered_key: HashMap<i32, GlossaryDocCacheKey>,
    pub(super) selections: BTreeMap<(usize, usize), GlossarySelectionState>,
    pub(super) active_drag_selection_key: Option<(usize, usize)>,
    pub(super) root_scan_highlight: Option<GlossaryScanHighlight>,
    pub(super) root_expanded_cards: HashSet<i32>,
    pub(super) root_row_anki_note_ids_i64: Vec<i64>,
    pub(super) root_row_anki_addable: Vec<bool>,
    pub(super) root_row_reading_as_expression: Vec<bool>,
    pub(super) root_term_kanji_statuses: HashMap<(String, String), EntryAnkiStatus>,
    pub(super) root_term_kana_statuses: HashMap<(String, String), EntryAnkiStatus>,
    pub(super) root_manual_reading_as_expr: HashMap<(String, String), bool>,
    pub(super) last_active_glossary_row_idx: i32,

    pub(super) suppress_history_reset: bool,
    pub(super) back_stack: VecDeque<PopupHistoryEntry>,
    pub(super) forward_stack: VecDeque<PopupHistoryEntry>,

    pub(super) nested_stack: Vec<NestedPopupLevel>,
    pub(super) hovered_glossary_target: Option<HoveredGlossaryTarget>,
    pub(super) nested_card_hovered: bool,
    pub(super) nested_body_hover_count: usize,
    pub(super) nested_scan_timer: slint::Timer,
    pub(super) nested_dismiss_timer: slint::Timer,
    pub(super) pending_nested_anki_adds: VecDeque<PendingNestedAnkiAdd>,

    pub(super) panel: PanelPopupState,
    pub(super) pending_panel_anki_adds: VecDeque<PendingPanelAnkiAdd>,

    pub(super) hovered_cluster: Option<HoveredSubtitleCluster>,
    pub(super) hovered_ass_target: Option<crate::controller::ass::AssHoveredTarget>,
    pub(super) pending_ass_scan: Option<crate::controller::ass::AssHoveredTarget>,
    pub(super) ass_controller: Option<Rc<RefCell<crate::controller::ass::AssOverlayController>>>,
    pub(super) active_scan_utf16_range: Option<(usize, usize)>,
    pub(super) pending_scan_utf16_start: Option<usize>,
    pub(super) pending_scan_sub_query: Option<String>,
    pub(super) scan_timer: slint::Timer,
    pub(super) idle_shrink_timer: slint::Timer,
    pub(super) subtitle_token_layout_cache: SubtitleTokenLayoutCache,
    pub(super) pending_root_precomputed_meta: Option<(
        HashMap<(String, String), Vec<TermPitchDetail>>,
        HashMap<String, Vec<TermFrequency>>,
    )>,
    pub(super) last_observed_solo_width_px: Option<(i32, bool, i32)>,
    pub(super) last_observed_col_width_px: Option<(i32, bool, i32)>,
}

impl RichUiMutable {
    pub(super) fn effective_popup_theme(&self) -> Arc<PopupCssTheme> {
        Arc::clone(&self.cached_theme)
    }

    /// Rebuild `cached_theme` when user CSS, active light/dark theme, compact glossaries,
    /// or interface font fallbacks changed. Returns `true` if the effective `PopupCssTheme` changed.
    pub(super) fn refresh_cached_theme(&mut self, css_changed: bool) -> bool {
        let theme_mode_changed = self.stylesheets.set_light_theme(self.settings.is_light_theme);
        if !css_changed
            && !theme_mode_changed
            && self.settings.popup_fonts == self.cached_theme_fonts
            && self.settings.is_light_theme == self.cached_theme_is_light
            && self.settings.compact_glossaries == self.cached_compact_glossaries
        {
            return false;
        }
        let mut next = self.stylesheets.theme().with_interface_font_fallbacks(
            self.stylesheets.overrides(),
            &self.settings.popup_fonts,
        );
        next.compact_glossaries = next.compact_glossaries || self.settings.compact_glossaries;
        self.cached_theme_fonts = self.settings.popup_fonts.clone();
        self.cached_theme_is_light = self.settings.is_light_theme;
        self.cached_compact_glossaries = self.settings.compact_glossaries;
        let changed = css_changed || theme_mode_changed || *self.cached_theme != next;
        self.cached_theme = Arc::new(next);
        changed
    }

    pub(super) fn apply_search_config_if_needed(&mut self) {
        let search_cfg = self.settings.dict_search_config();
        if self.applied_search_cfg.as_ref() != Some(&search_cfg) {
            search_cfg.apply_to(&mut self.dict_ctrl);
            self.applied_search_cfg = Some(search_cfg);
        }
    }

    pub(super) fn nested_level(&self, depth: i32) -> Option<&NestedPopupLevel> {
        if depth <= 0 {
            return None;
        }
        self.nested_stack.get((depth - 1) as usize)
    }

    pub(super) fn nested_level_mut(&mut self, depth: i32) -> Option<&mut NestedPopupLevel> {
        if depth <= 0 {
            return None;
        }
        self.nested_stack.get_mut((depth - 1) as usize)
    }

    pub(super) fn clear_all_glossary_caches(&mut self) {
        self.rich_epoch = self.rich_epoch.wrapping_add(1);
        self.glossary_cache.clear();
        self.last_rendered_key.clear();
        for lvl in &mut self.nested_stack {
            lvl.rich_epoch = lvl.rich_epoch.wrapping_add(1);
            lvl.glossary_cache.clear();
            lvl.last_rendered_key.clear();
        }
        self.panel.rich_epoch = self.panel.rich_epoch.wrapping_add(1);
        self.panel.glossary_cache.clear();
        self.panel.last_rendered_key.clear();
    }
}

pub(super) fn sync_subtitle_overlay_style_to_bridge(
    app: &MementoApp,
    style: &SubtitleRenderStyle,
) {
    let sub = app.global::<SubtitleOverlayBridge>();
    sub.set_sub_font_family(style.font_family.clone().into());
    sub.set_sub_font_bold(style.bold);
    sub.set_sub_font_italic(style.italic);
    sub.set_sub_text_color(slint::Color::from_argb_u8(
        style.text_rgba[3],
        style.text_rgba[0],
        style.text_rgba[1],
        style.text_rgba[2],
    ));
    sub.set_sub_stroke_color(slint::Color::from_argb_u8(
        style.stroke_rgba[3],
        style.stroke_rgba[0],
        style.stroke_rgba[1],
        style.stroke_rgba[2],
    ));
    sub.set_sub_bg_color(slint::Color::from_argb_u8(
        style.bg_rgba[3],
        style.bg_rgba[0],
        style.bg_rgba[1],
        style.bg_rgba[2],
    ));
    let stroke_width = if style.stroke_size <= 0.0 {
        0.0
    } else {
        (style.stroke_size * 0.1).clamp(0.5, 4.0)
    };
    sub.set_sub_stroke_width(stroke_width);
}

/// Maps a 1-based `DictResult.index` (`row_index`) to a deterministic `(term_idx, def_idx)`
/// key in visual reading order so multi-definition persistent `{selection}` stays cleanly
/// ordered across definitions and terms (#262 / H5.1).
pub(super) fn row_term_def_key(rows: &[DictResult], row_index: i32) -> (usize, usize) {
    let mut term_idx = 0usize;
    let mut def_idx = 0usize;
    for (idx, row) in rows.iter().enumerate() {
        if idx > 0 && row.term_start {
            term_idx += 1;
            def_idx = 0;
        } else if idx > 0 {
            def_idx += 1;
        }
        if row.index == row_index {
            return (term_idx, def_idx);
        }
    }
    (row_index.max(0) as usize, 0)
}

pub(super) fn row_term_def_key_model(
    rows: &slint::ModelRc<DictResult>,
    row_index: i32,
) -> (usize, usize) {
    let mut term_idx = 0usize;
    let mut def_idx = 0usize;
    for idx in 0..rows.row_count() {
        let Some(row) = rows.row_data(idx) else {
            continue;
        };
        if idx > 0 && row.term_start {
            term_idx += 1;
            def_idx = 0;
        } else if idx > 0 {
            def_idx += 1;
        }
        if row.index == row_index {
            return (term_idx, def_idx);
        }
    }
    (row_index.max(0) as usize, 0)
}

/// RAII guard for `RichUiMutable::suppress_history_reset` so early returns or panics
/// can never leave history resets permanently suppressed.
pub(super) struct SuppressHistoryResetGuard<'a> {
    state: &'a RefCell<RichUiMutable>,
    prev: bool,
}

impl<'a> SuppressHistoryResetGuard<'a> {
    pub(super) fn enter(state: &'a RefCell<RichUiMutable>) -> Self {
        let prev = {
            let mut st = state.borrow_mut();
            let prev = st.suppress_history_reset;
            st.suppress_history_reset = true;
            prev
        };
        Self { state, prev }
    }
}

impl Drop for SuppressHistoryResetGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut st) = self.state.try_borrow_mut() {
            st.suppress_history_reset = self.prev;
        }
    }
}

pub(super) fn push_history_capped(stack: &mut VecDeque<PopupHistoryEntry>, entry: PopupHistoryEntry) {
    while stack.len() >= MAX_NAV_HISTORY {
        stack.pop_front();
    }
    stack.push_back(entry);
}

pub(super) fn encode_note_id_i32(nid: i64) -> i32 {
    if nid <= 0 {
        0
    } else if nid <= i64::from(i32::MAX) {
        nid as i32
    } else {
        i32::MAX
    }
}

pub(super) fn parse_def_rgba(value: &str, fallback: [u8; 4]) -> [u8; 4] {
    if let Some(c) = DefColor::from_string(value.trim()) {
        let r = ((u32::from(c.red) + 128) / 257) as u8;
        let g = ((u32::from(c.green) + 128) / 257) as u8;
        let b = ((u32::from(c.blue) + 128) / 257) as u8;
        let a = ((u32::from(c.alpha) + 128) / 257) as u8;
        [r, g, b, a]
    } else {
        fallback
    }
}

pub(super) fn parse_interface_font_setting(
    config: &ConfigController,
    stock_config: &ConfigController,
    key: &str,
    min_px: f32,
    max_px: f32,
) -> Option<InterfaceFontFallback> {
    let raw_val = config.get_setting(raw::interface::GROUP, key)?;
    let memento_config::Value::Text(ref raw_csv) = raw_val else {
        return None;
    };
    // `ConfigController::get_setting` synthesizes legacy Qt defaults (e.g., 10pt glossary,
    // 108pt Kanji Stroke Orders) when unset. Only override Memento's built-in Slint theme
    // defaults when the user actually customized the setting away from the stock Qt default.
    let stock_default = stock_config.effective_setting(raw::interface::GROUP, key);
    if stock_default.as_ref() == Some(&raw_val) {
        return None;
    }
    let spec = memento_config::QtFontSpec::parse(raw_csv).ok()?;
    let size_px = ((spec.point_size as f32) * (96.0 / 72.0)).clamp(min_px, max_px);
    Some(InterfaceFontFallback {
        family: spec.family,
        size_px,
        bold: spec.bold,
        italic: spec.italic,
    })
}

/// Converts detailed pitch entries (with 1-based `nasal`/`devoice` mora indices and optional
/// exact string `H`/`L` patterns like `"LHH"`, `"HLL"`) into Slint `RichPitchEntry` rows.
pub(super) fn build_rich_pitch_entries_from_details(
    details: &[TermPitchDetail],
    hide_repeated_dict: bool,
) -> Vec<RichPitchEntry> {
    details
        .iter()
        .enumerate()
        .map(|(entry_idx, detail)| {
            let len = detail.mora.len();
            let (mora_highs, particle_high) =
                resolve_pitch_mora_levels(len, detail.position, detail.pattern.as_deref());
            let mora: Vec<RichPitchMora> = detail
                .mora
                .iter()
                .enumerate()
                .map(|(index, text)| {
                    let high = mora_highs[index];
                    let next_high = if index + 1 < len {
                        mora_highs[index + 1]
                    } else {
                        particle_high
                    };
                    let one_based = index + 1;
                    RichPitchMora {
                        text: text.clone().into(),
                        high,
                        rises_after: index + 1 < len && !high && next_high,
                        falls_after: high && !next_high,
                        nasal: detail.nasal.contains(&one_based),
                        devoice: detail.devoice.contains(&one_based),
                    }
                })
                .collect();
            let hide_dict_name = hide_repeated_dict
                && entry_idx > 0
                && !detail.dictionary.is_empty()
                && details[entry_idx - 1].dictionary == detail.dictionary;
            let pattern_text = detail
                .pattern
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("")
                .into();
            RichPitchEntry {
                dictionary: detail.dictionary.clone().into(),
                position: detail.position,
                pattern_text,
                hide_dict_name,
                mora: Rc::new(VecModel::from(mora)).into(),
                tags: detail.tags.join(", ").into(),
            }
        })
        .collect()
}

pub(super) fn classify_pitch_category(
    pitches: &[RichPitchGraph],
    details: &[TermPitchDetail],
    group_tags: &str,
) -> Option<&'static str> {
    let first = pitches.first()?;
    let is_kifuku_pos = group_tags.split_whitespace().any(|t| {
        t == "adj-i"
            || t == "v1"
            || t == "vk"
            || t == "vs-i"
            || t == "vs-s"
            || t.starts_with("v5")
            || t.starts_with("v1-")
            || t.starts_with("v2")
            || t.starts_with("v4")
    });
    if let Some(detail) = details.first() {
        if let Some(pat) = detail.pattern.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            let len = detail.mora.len().max(1);
            let (mora_highs, particle_high) =
                resolve_pitch_mora_levels(len, detail.position, Some(pat));
            let drop_after = (0..mora_highs.len()).find(|&idx| {
                let cur = mora_highs[idx];
                let next = if idx + 1 < mora_highs.len() {
                    mora_highs[idx + 1]
                } else {
                    particle_high
                };
                cur && !next
            });
            return match drop_after {
                None => Some("heiban"),
                Some(_) if is_kifuku_pos => Some("kifuku"),
                Some(0) => Some("atamadaka"),
                Some(idx) if idx + 1 >= mora_highs.len() && mora_highs.len() > 1 => Some("odaka"),
                Some(_) => Some("nakadaka"),
            };
        }
    }
    let pos = first.position;
    if pos == 0 {
        return Some("heiban");
    }
    if is_kifuku_pos {
        return Some("kifuku");
    }
    if pos == 1 {
        Some("atamadaka")
    } else {
        let mora_count = first.mora.row_count() as i32;
        if mora_count > 1 && pos >= mora_count {
            Some("odaka")
        } else {
            Some("nakadaka")
        }
    }
}

/// Build per-row rich metadata (`RichRowExtras`) and deferred kanji frequency updates for a
/// grouped result set. Returns `(extras_map, kanji_freq_updates)` without touching Slint models,
/// so callers can drop `RefCell` borrows before updating Slint UI state.
#[allow(clippy::too_many_arguments)]
pub(super) fn build_row_extras_for_grouped(
    db_helper: &mut RichDbHelper,
    dict_ctrl: &mut DictController,
    rows: &[DictResult],
    grouped_hits: &[SearchHit],
    precomputed_pitch_details: Option<&HashMap<(String, String), Vec<TermPitchDetail>>>,
    precomputed_kanji_freqs: Option<&HashMap<String, Vec<TermFrequency>>>,
    compact_tags: bool,
) -> (HashMap<i32, RichRowExtras>, Vec<(usize, Vec<TermFrequency>)>) {
    let mut extras_map = HashMap::new();
    let mut kanji_freq_updates = Vec::new();
    let mut group_start = 0usize;
    while group_start < rows.len() && group_start < grouped_hits.len() {
        let mut group_end = group_start + 1;
        while group_end < rows.len()
            && group_end < grouped_hits.len()
            && !rows[group_end].term_start
        {
            group_end += 1;
        }
        let first_hit = &grouped_hits[group_start];
        let mut raw_term_tags = Vec::new();
        for hit in &grouped_hits[group_start..group_end] {
            raw_term_tags.extend(db_helper.resolve_tag_list(hit.dictionary_id, &hit.term_tags));
        }
        let pitch_details = if !first_hit.is_kanji {
            if let Some(pre) = precomputed_pitch_details {
                pre.get(&(first_hit.expression.clone(), first_hit.reading.clone()))
                    .cloned()
                    .unwrap_or_default()
            } else {
                dict_ctrl.get_pitch_details(&first_hit.expression, &first_hit.reading)
            }
        } else {
            Vec::new()
        };
        let rich_pitches = rich_pitch_graphs_from_details(&pitch_details);
        let rich_pitch_entries = build_rich_pitch_entries_from_details(&pitch_details, true);
        let mut group_pos_tags = String::new();
        for hit in &grouped_hits[group_start..group_end] {
            if !hit.def_tags.is_empty() {
                group_pos_tags.push(' ');
                group_pos_tags.push_str(&hit.def_tags);
            }
            if !hit.term_tags.is_empty() {
                group_pos_tags.push(' ');
                group_pos_tags.push_str(&hit.term_tags);
            }
        }
        let group_pitch_category =
            classify_pitch_category(&rich_pitches, &pitch_details, &group_pos_tags);
        let kanji_freqs: Vec<TermFrequency> = if first_hit.is_kanji {
            if let Some(pre_kf) = precomputed_kanji_freqs {
                pre_kf
                    .get(&first_hit.expression)
                    .cloned()
                    .unwrap_or_default()
            } else {
                dict_ctrl.get_kanji_frequencies(&first_hit.expression)
            }
        } else {
            Vec::new()
        };

        let mut seen_sense_tags: HashSet<String> = HashSet::new();
        for idx in group_start..group_end {
            let hit = &grouped_hits[idx];
            let parsed_glossary = (!hit.is_kanji && !hit.raw_glossary.trim().is_empty())
                .then(|| hit.parsed_glossary())
                .flatten();
            let inline_tags = parsed_glossary
                .as_deref()
                .map(extract_inline_structured_tags_from_value)
                .unwrap_or_default();
            let raw_def_tags = db_helper.resolve_tag_list(hit.dictionary_id, &hit.def_tags);
            let (dedup_term, dedup_def) = suppress_redundant_tags_for_sense(
                &raw_term_tags,
                &raw_def_tags,
                &inline_tags,
                &mut seen_sense_tags,
                compact_tags,
            );

            let term_pills = if idx == group_start {
                dedup_term
                    .into_iter()
                    .map(|t| RichTagPill {
                        bg_color: tag_brush(&t.category),
                        name: t.name.into(),
                        category: t.category.into(),
                        notes: t.notes.into(),
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let def_pills = dedup_def
                .into_iter()
                .map(|t| RichTagPill {
                    bg_color: tag_brush(&t.category),
                    name: t.name.into(),
                    category: t.category.into(),
                    notes: t.notes.into(),
                })
                .collect();

            let derivation_steps = if idx == group_start {
                let total = hit.derivation_descriptions.len();
                hit.derivation_descriptions
                    .iter()
                    .enumerate()
                    .map(|(si, (label, desc))| RichDerivationStep {
                        label: label.clone().into(),
                        description: desc.clone().into(),
                        has_arrow: si + 1 < total,
                    })
                    .collect()
            } else {
                Vec::new()
            };

            let kanji_detail = if hit.is_kanji {
                db_helper.build_kanji_detail(&hit.expression, hit.dictionary_id)
            } else {
                None
            };
            let detail_states = parsed_glossary
                .as_deref()
                .map(collect_default_detail_states_from_value)
                .unwrap_or_default();

            extras_map.insert(
                (idx + 1) as i32,
                RichRowExtras {
                    hit: Some(hit.clone()),
                    parsed_glossary,
                    term_pills,
                    def_pills,
                    derivation_steps,
                    pitch_graphs: if idx == group_start {
                        rich_pitches.clone()
                    } else {
                        Vec::new()
                    },
                    pitch_details: if idx == group_start {
                        pitch_details.clone()
                    } else {
                        Vec::new()
                    },
                    pitch_entries: if idx == group_start {
                        rich_pitch_entries.clone()
                    } else {
                        Vec::new()
                    },
                    pitch_category: if idx == group_start {
                        group_pitch_category
                    } else {
                        None
                    },
                    kanji_detail,
                    detail_states,
                },
            );
        }

        if first_hit.is_kanji && !kanji_freqs.is_empty() {
            kanji_freq_updates.push((group_start, kanji_freqs));
        }

        group_start = group_end;
    }
    (extras_map, kanji_freq_updates)
}

pub(super) fn apply_kanji_freq_updates_to_rows(
    rows: &mut [DictResult],
    updates: Vec<(usize, Vec<TermFrequency>)>,
) {
    for (group_start, kanji_freqs) in updates {
        if let Some(row) = rows.get_mut(group_start) {
            row.frequencies = Rc::new(VecModel::from(
                kanji_freqs
                    .into_iter()
                    .map(|f| PopupFrequency {
                        dictionary: f.dictionary.into(),
                        value: f.value.into(),
                    })
                    .collect::<Vec<_>>(),
            ))
            .into();
        }
    }
}

pub(super) fn apply_kanji_freq_updates_to_app(
    app: &MementoApp,
    updates: Vec<(usize, Vec<TermFrequency>)>,
) {
    if updates.is_empty() {
        return;
    }
    let dict_rows = app.get_dict_results();
    let popup_rows = app.get_popup_results();
    for (group_start, kanji_freqs) in updates {
        if let Some(mut first_row) = dict_rows.row_data(group_start) {
            first_row.frequencies = Rc::new(VecModel::from(
                kanji_freqs
                    .into_iter()
                    .map(|f| PopupFrequency {
                        dictionary: f.dictionary.into(),
                        value: f.value.into(),
                    })
                    .collect::<Vec<_>>(),
            ))
            .into();
            dict_rows.set_row_data(group_start, first_row.clone());
            for pi in 0..popup_rows.row_count() {
                if popup_rows
                    .row_data(pi)
                    .is_some_and(|r| r.index == first_row.index)
                {
                    popup_rows.set_row_data(pi, first_row);
                    break;
                }
            }
        }
    }
}

/// Collect initial depth-first `<details>` open states (`"0"`, `"1"`, ...) from a
/// pre-parsed Yomitan `structured-content` JSON tree so toggling an initially `open: true`
/// `<details>` closes it on the first click without re-parsing the JSON string.
pub(super) fn collect_default_detail_states_from_value(
    parsed: &serde_json::Value,
) -> HashMap<String, bool> {
    fn walk(node: &serde_json::Value, next_id: &mut usize, out: &mut HashMap<String, bool>) {
        match node {
            serde_json::Value::Array(items) => {
                for item in items {
                    walk(item, next_id, out);
                }
            }
            serde_json::Value::Object(map) => {
                if map
                    .get("tag")
                    .and_then(|v| v.as_str())
                    .is_some_and(|t| t.eq_ignore_ascii_case("details"))
                {
                    let id = next_id.to_string();
                    *next_id += 1;
                    let is_open = map.get("open").and_then(|v| v.as_bool()).unwrap_or(false);
                    out.insert(id, is_open);
                }
                if let Some(content) = map.get("content") {
                    walk(content, next_id, out);
                }
            }
            _ => {}
        }
    }

    let mut out = HashMap::new();
    let mut next_id = 0usize;
    walk(parsed, &mut next_id, &mut out);
    out
}

pub(super) fn current_external_lookup_epoch(
    ext: &Mutex<Option<Arc<std::sync::atomic::AtomicU64>>>,
) -> Option<u64> {
    ext.lock()
        .ok()
        .and_then(|guard| guard.as_ref().map(|e| e.load(Ordering::Acquire)))
}

pub(super) fn is_modifier_only_key_text(text: &str) -> Option<(bool, bool, bool, bool)> {
    use slint::platform::Key;
    let mut chars = text.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    if ch == char::from(Key::Shift) || ch == char::from(Key::ShiftR) {
        Some((false, true, false, false))
    } else if ch == char::from(Key::Control) || ch == char::from(Key::ControlR) {
        Some((true, false, false, false))
    } else if ch == char::from(Key::Alt) || ch == char::from(Key::AltGr) {
        Some((false, false, true, false))
    } else if ch == char::from(Key::Meta) || ch == char::from(Key::MetaR) {
        Some((false, false, false, true))
    } else {
        None
    }
}

/// Resolve a bare modifier key press into `(control, shift, alt, meta)` flags,
/// supporting both Slint's private-use `Key::*` encodings, named modifier strings,
/// and `winit` events where `text` is empty while a modifier flag is active.
pub(super) fn resolve_bare_modifier_event(
    text: &str,
    control: bool,
    shift: bool,
    alt: bool,
    meta: bool,
) -> Option<(bool, bool, bool, bool)> {
    if let Some((mc, ms, ma, mm)) = is_modifier_only_key_text(text) {
        return Some((control || mc, shift || ms, alt || ma, meta || mm));
    }
    if text.is_empty() && (control || shift || alt || meta) {
        return Some((control, shift, alt, meta));
    }
    match text {
        "Shift" => Some((control, true, alt, meta)),
        "Control" | "Ctrl" => Some((true, shift, alt, meta)),
        "Alt" => Some((control, shift, true, meta)),
        "Meta" | "Super" => Some((control, shift, alt, true)),
        _ => None,
    }
}

pub(super) fn modifier_matches_setting(
    setting: i32,
    control: bool,
    shift: bool,
    alt: bool,
    meta: bool,
) -> bool {
    match setting {
        0 => alt,
        1 => control,
        2 => shift,
        3 => meta,
        _ => false,
    }
}

thread_local! {
    /// Persistent per-thread clipboard handle so X11 selection ownership stays alive across copies
    /// instead of dropping immediately and wiping the clipboard.
    static PERSISTENT_CLIPBOARD: RefCell<Option<copypasta::ClipboardContext>> =
        const { RefCell::new(None) };
}

pub(super) fn copy_to_persistent_clipboard(text: String) {
    use copypasta::ClipboardProvider;
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        PERSISTENT_CLIPBOARD.with(|slot| {
            let Ok(mut guard) = slot.try_borrow_mut() else {
                return;
            };
            if let Some(ctx) = guard.as_mut() {
                if ctx.set_contents(text.clone()).is_ok() {
                    return;
                }
                *guard = None;
            }
            if let Ok(mut fresh) = copypasta::ClipboardContext::new() {
                if fresh.set_contents(text).is_ok() {
                    *guard = Some(fresh);
                }
            }
        });
    }));
}

#[cfg(test)]
mod mecab_settings_tests {
    use super::*;

    #[test]
    fn cached_config_settings_tracks_mecab_ipadic_matcher_setting() {
        let dir = tempfile::TempDir::new().unwrap();
        let cfg = ConfigController::new();
        let cached = CachedConfigSettings::from_controller(&cfg, dir.path());
        assert_eq!(cached.exact_enabled, raw::search::matcher::EXACT_DEFAULT);
        assert_eq!(cached.deconj_enabled, raw::search::matcher::DECONJ_DEFAULT);
        assert_eq!(
            cached.mecab_enabled,
            raw::search::matcher::MECAB_IPADIC_DEFAULT
        );
        assert_eq!(cached.remove_regex, raw::search::REMOVE_REGEX_DEFAULT);
        assert_eq!(cached.replace_lines, raw::search::REPLACE_NEWLINES_DEFAULT);
        assert_eq!(
            cached.replace_with,
            raw::search::REPLACE_NEWLINES_WITH_DEFAULT
        );

        let exact_key = format!("{}/{}", raw::search::GROUP, raw::search::matcher::EXACT);
        let deconj_key = format!("{}/{}", raw::search::GROUP, raw::search::matcher::DECONJ);
        let mecab_key = format!(
            "{}/{}",
            raw::search::GROUP,
            raw::search::matcher::MECAB_IPADIC
        );
        let regex_key = format!("{}/{}", raw::search::GROUP, raw::search::REMOVE_REGEX);
        let lines_key = format!("{}/{}", raw::search::GROUP, raw::search::REPLACE_NEWLINES);
        let with_key = format!(
            "{}/{}",
            raw::search::GROUP,
            raw::search::REPLACE_NEWLINES_WITH
        );

        cfg.edit_setting(&exact_key, "false").unwrap();
        cfg.edit_setting(&deconj_key, "false").unwrap();
        cfg.edit_setting(&mecab_key, "false").unwrap();
        cfg.edit_setting(&regex_key, "\\[.*?\\]").unwrap();
        cfg.edit_setting(&lines_key, "true").unwrap();
        cfg.edit_setting(&with_key, " ").unwrap();
        let mut updated = CachedConfigSettings::from_controller(&cfg, dir.path());
        assert!(!updated.exact_enabled);
        assert!(!updated.deconj_enabled);
        assert!(!updated.mecab_enabled);
        assert_eq!(updated.remove_regex, "\\[.*?\\]");
        assert!(updated.replace_lines);
        assert_eq!(updated.replace_with, " ");

        let mut dict = DictController::new();
        updated.dict_search_config().apply_to(&mut dict);
        assert!(!dict.exact_enabled());
        assert!(!dict.deconj_enabled());
        assert!(!dict.mecab_enabled());

        cfg.edit_setting(&exact_key, "true").unwrap();
        cfg.edit_setting(&deconj_key, "true").unwrap();
        cfg.edit_setting(&mecab_key, "true").unwrap();
        updated.apply_controller(&cfg);
        assert!(updated.exact_enabled);
        assert!(updated.deconj_enabled);
        assert!(updated.mecab_enabled);
    }

    #[test]
    fn push_history_capped_enforces_max_entries() {
        let mut stack = VecDeque::new();
        for i in 0..(MAX_NAV_HISTORY + 12) {
            push_history_capped(
                &mut stack,
                PopupHistoryEntry {
                    label: format!("term-{i}"),
                    rows: Vec::new(),
                    hits: Vec::new(),
                    loaded_count: 1,
                    headword: String::new(),
                    reading: String::new(),
                    pitch_accent: String::new(),
                    freq_rank: String::new(),
                    jpdb_rank: String::new(),
                    kana_reading_active: false,
                    active_kana_reading: String::new(),
                },
            );
        }
        assert_eq!(stack.len(), MAX_NAV_HISTORY);
        assert_eq!(stack.front().unwrap().label, "term-12");
        assert_eq!(
            stack.back().unwrap().label,
            format!("term-{}", MAX_NAV_HISTORY + 11)
        );
    }
}
