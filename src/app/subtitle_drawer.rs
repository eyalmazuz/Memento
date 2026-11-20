//! Subtitle drawer row height estimation with incremental prefix-sum caching,
//! O(N) search highlighting, delay-adjusted timestamps (`H3.7`), Primary &
//! Secondary selection/copy/lookup (`M1`), and drawer callbacks.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use slint::{ComponentHandle, Model as _, VecModel};

use crate::app::clipboard::copy_text_to_clipboard;
use crate::controller::dict;
use crate::controller::player::{
    refresh_subtitle_list_with_timing as base_refresh_subtitle_list_with_timing,
    refresh_subtitle_marks,
};
use crate::controller::subtitles::SubtitleController;
use crate::ui::{self, MementoApp, PopupBridge, SubEntry};
use memento_player::subtitle_to_media_time;

pub(crate) fn estimate_drawer_row_height(
    text: &str,
    available_text_width: f32,
    font_size: f32,
) -> f32 {
    let usable_w = available_text_width.max(120.0);
    let scale = (font_size.clamp(9.0, 48.0)) / 13.0;
    let mut visual_lines = 0usize;
    for raw_line in text.split('\n') {
        let line_px: f32 = raw_line
            .chars()
            .map(|ch| {
                if ch.is_ascii() {
                    7.5 * scale
                } else {
                    13.5 * scale
                }
            })
            .sum();
        let wrapped = ((line_px / usable_w).ceil() as usize).max(1);
        visual_lines += wrapped;
    }
    let visual_lines = visual_lines.max(1);
    let line_height = (17.0 * scale).max(15.0);
    (visual_lines as f32 * line_height + 6.0).max(24.0)
}

#[derive(Clone, PartialEq, Eq)]
struct RowBoundarySig {
    id: i32,
    start_bits: u32,
    text_len: usize,
    text_sample: String,
}

impl RowBoundarySig {
    fn from_entry(entry: &SubEntry) -> Self {
        let s = entry.text.as_str();
        let sample: String = s.chars().take(24).collect();
        Self {
            id: entry.id,
            start_bits: entry.start.to_bits(),
            text_len: s.len(),
            text_sample: sample,
        }
    }
}

#[derive(Default)]
struct TabHeightCache {
    text_width_bits: u32,
    font_size_bits: u32,
    revision: u64,
    first_sig: Option<RowBoundarySig>,
    last_sig: Option<RowBoundarySig>,
    /// Cumulative heights: `prefix[0] = 0.0`, `prefix[i + 1] = prefix[i] + row_h[i]`.
    prefix: Vec<f32>,
}

#[derive(Default)]
struct SecondarySelectionState {
    sid: i64,
    selected: Vec<usize>,
    anchor: Option<usize>,
}

impl SecondarySelectionState {
    fn sync_track(&mut self, sid: i64, entry_count: usize) {
        if self.sid != sid {
            self.sid = sid;
            self.selected.clear();
            self.anchor = None;
        }
        self.selected.retain(|&idx| idx < entry_count);
        if self.anchor.is_some_and(|a| a >= entry_count) {
            self.anchor = None;
        }
    }

    fn apply_click(&mut self, index: usize, entry_count: usize, ctrl: bool, shift: bool) {
        if index >= entry_count {
            return;
        }
        if shift {
            let anchor = *self.anchor.get_or_insert(index);
            if !ctrl {
                self.selected.clear();
            }
            self.selected.extend(anchor.min(index)..=anchor.max(index));
            self.selected.sort_unstable();
            self.selected.dedup();
        } else if ctrl {
            self.anchor = Some(index);
            if let Some(pos) = self.selected.iter().position(|&i| i == index) {
                self.selected.remove(pos);
            } else {
                self.selected.push(index);
                self.selected.sort_unstable();
            }
        } else {
            self.selected.clear();
            self.selected.push(index);
            self.anchor = Some(index);
        }
    }

    fn select_range(&mut self, first: usize, last: usize, previous: &[usize], entry_count: usize) {
        if first >= entry_count || last >= entry_count {
            return;
        }
        self.anchor = Some(first);
        self.selected = previous
            .iter()
            .copied()
            .filter(|&i| i < entry_count)
            .collect();
        self.selected.extend(first.min(last)..=first.max(last));
        self.selected.sort_unstable();
        self.selected.dedup();
    }

    fn set_selected(&mut self, index: usize, entry_count: usize, selected: bool) {
        if index >= entry_count {
            return;
        }
        self.anchor = Some(index);
        if let Some(pos) = self.selected.iter().position(|&i| i == index) {
            if !selected {
                self.selected.remove(pos);
            }
        } else if selected {
            self.selected.push(index);
            self.selected.sort_unstable();
        }
    }
}

#[derive(Default)]
struct DrawerCaches {
    primary_revision: u64,
    secondary_revision: u64,
    primary_heights: TabHeightCache,
    secondary_heights: TabHeightCache,
    last_secondary_model: Option<SecondaryModelState>,
    secondary_selection: SecondarySelectionState,
    suppress_search_auto_seek: bool,
}

struct SecondaryModelState {
    model_ptr: usize,
    sid: i64,
    entry_count: usize,
    remove_regex: String,
    sub_speed_bits: u64,
    secondary_sub_delay_bits: u64,
    first_cue_sig: Option<(u64, usize)>,
    last_cue_sig: Option<(u64, usize)>,
}

thread_local! {
    static DRAWER_CACHES: RefCell<DrawerCaches> = RefCell::new(DrawerCaches::default());
}

fn invalidate_height_cache(is_secondary: bool) {
    DRAWER_CACHES.with(|slot| {
        let mut caches = slot.borrow_mut();
        if is_secondary {
            caches.secondary_revision = caches.secondary_revision.wrapping_add(1);
        } else {
            caches.primary_revision = caches.primary_revision.wrapping_add(1);
        }
    });
}

fn ensure_prefix_heights<'a>(
    cache: &'a mut TabHeightCache,
    revision: u64,
    entries: &slint::ModelRc<SubEntry>,
    text_width: f32,
    font_size: f32,
) -> &'a [f32] {
    let count = entries.row_count();
    let width_bits = text_width.to_bits();
    let font_bits = font_size.to_bits();
    let first_sig = entries.row_data(0).map(|e| RowBoundarySig::from_entry(&e));
    let last_sig = count
        .checked_sub(1)
        .and_then(|idx| entries.row_data(idx))
        .map(|e| RowBoundarySig::from_entry(&e));

    let cached_count = cache.prefix.len().saturating_sub(1);
    let params_match = cache.text_width_bits == width_bits
        && cache.font_size_bits == font_bits
        && cache.revision == revision
        && cache.first_sig == first_sig;

    if params_match && cached_count == count && cache.last_sig == last_sig {
        return &cache.prefix;
    }

    if params_match && count > cached_count && cached_count > 0 {
        // Incremental append (e.g. streaming ASR cues): only measure newly appended rows.
        cache.prefix.reserve(count - cached_count);
        let mut running = *cache.prefix.last().unwrap_or(&0.0);
        for idx in cached_count..count {
            let h = entries
                .row_data(idx)
                .map(|entry| estimate_drawer_row_height(entry.text.as_str(), text_width, font_size))
                .unwrap_or(24.0);
            running += h;
            cache.prefix.push(running);
        }
        cache.last_sig = last_sig;
        return &cache.prefix;
    }

    // Full rebuild into reused buffer.
    cache.prefix.clear();
    cache.prefix.reserve(count + 1);
    cache.prefix.push(0.0);
    let mut running = 0.0f32;
    for idx in 0..count {
        let h = entries
            .row_data(idx)
            .map(|entry| estimate_drawer_row_height(entry.text.as_str(), text_width, font_size))
            .unwrap_or(24.0);
        running += h;
        cache.prefix.push(running);
    }
    cache.text_width_bits = width_bits;
    cache.font_size_bits = font_bits;
    cache.revision = revision;
    cache.first_sig = first_sig;
    cache.last_sig = last_sig;
    &cache.prefix
}

pub fn scroll_drawer_to_row_accurate(app: &MementoApp, row_idx: usize) {
    let is_secondary = app.get_subtitle_drawer_tab() == 1;
    let entries = if is_secondary {
        app.get_secondary_subtitle_entries()
    } else {
        app.get_subtitle_entries()
    };
    let count = entries.row_count();
    if count == 0 || row_idx >= count {
        return;
    }
    let chrome_w = if app.get_sub_list_show_timestamps() {
        94.0
    } else {
        36.0
    };
    let text_width = (app.get_sidebar_width() - chrome_w).max(120.0);
    let font_size = if is_secondary {
        app.get_sub_list_secondary_font_size()
    } else {
        app.get_sub_list_primary_font_size()
    };

    let (row_y, target_h, total_h) = DRAWER_CACHES.with(|slot| {
        let mut caches = slot.borrow_mut();
        let (revision, height_cache) = if is_secondary {
            (caches.secondary_revision, &mut caches.secondary_heights)
        } else {
            (caches.primary_revision, &mut caches.primary_heights)
        };
        let prefix = ensure_prefix_heights(height_cache, revision, &entries, text_width, font_size);
        let y = prefix[row_idx];
        let h = (prefix[row_idx + 1] - y).max(24.0);
        let total = *prefix.last().unwrap_or(&0.0);
        (y, h, total)
    });
    app.invoke_scroll_subtitle_drawer_to_offset(row_y, target_h, total_h);
}

pub fn auto_scroll_subtitle_drawer_if_needed(app: &MementoApp, subtitles: &SubtitleController) {
    if !app.get_sub_drawer_open() || app.get_subtitle_drawer_user_scrolling() {
        return;
    }
    let active_idx = if app.get_subtitle_drawer_tab() == 1 {
        subtitles
            .active_indices(subtitles.secondary_sid())
            .first()
            .copied()
    } else {
        subtitles
            .active_indices(subtitles.primary_sid())
            .first()
            .copied()
    };
    if let Some(row) = active_idx {
        scroll_drawer_to_row_accurate(app, row);
    }
}

fn apply_secondary_selection_marks<M: slint::Model<Data = SubEntry>>(
    model: &M,
    selected: &[usize],
) {
    let selected_set: HashSet<usize> = selected.iter().copied().collect();
    for model_row in 0..model.row_count() {
        if let Some(mut row) = model.row_data(model_row) {
            let entry_idx = usize::try_from(row.id - 1).unwrap_or(model_row);
            let is_sel = selected_set.contains(&entry_idx);
            if row.selected != is_sel {
                row.selected = is_sel;
                model.set_row_data(model_row, row);
            }
        }
    }
}

/// Re-run the currently active subtitle search without seeking playback (used
/// when live ASR or newly parsed embedded cues arrive in `sync_loop.rs`, M1).
pub(crate) fn rerun_active_subtitle_search_silently(app: &MementoApp) {
    let query = app.get_subtitle_search_query();
    if query.trim().is_empty() {
        return;
    }
    DRAWER_CACHES.with(|slot| {
        slot.borrow_mut().suppress_search_auto_seek = true;
    });
    app.invoke_search_subtitles(query);
    DRAWER_CACHES.with(|slot| {
        slot.borrow_mut().suppress_search_auto_seek = false;
    });
}

/// Extract the lookup slice starting at character index `char_idx` from a cue line.
pub(crate) fn slice_lookup_from_char(text: &str, char_idx: i32) -> Option<String> {
    let char_count = text.chars().count();
    if char_count == 0 {
        return None;
    }
    let clamped = (char_idx.max(0) as usize).min(char_count - 1);
    let byte_start = text
        .char_indices()
        .nth(clamped)
        .map(|(byte_idx, _)| byte_idx)
        .unwrap_or(0);
    let tail = text[byte_start..].trim_start();
    let candidate = if tail.is_empty() { text.trim() } else { tail };
    (!candidate.is_empty()).then(|| candidate.to_owned())
}

pub fn refresh_subtitle_list_with_timing(
    app: &MementoApp,
    model: &VecModel<SubEntry>,
    subtitles: &SubtitleController,
    sub_speed: f64,
    sub_delay: f64,
) {
    base_refresh_subtitle_list_with_timing(app, model, subtitles, sub_speed, sub_delay);
    invalidate_height_cache(false);
    auto_scroll_subtitle_drawer_if_needed(app, subtitles);
}

pub fn refresh_subtitle_list(
    app: &MementoApp,
    model: &VecModel<SubEntry>,
    subtitles: &SubtitleController,
) {
    refresh_subtitle_list_with_timing(app, model, subtitles, 1.0, 0.0);
}

pub fn refresh_secondary_subtitle_list_with_timing(
    app: &MementoApp,
    model: &VecModel<SubEntry>,
    subtitles: &SubtitleController,
    sub_speed: f64,
    secondary_sub_delay: f64,
) {
    let query = app.get_subtitle_search_query();
    let active_tab = app.get_subtitle_drawer_tab();
    let ignore_whitespace = app.get_subtitle_search_ignore_whitespace();
    let remove_regex = app.get_search_remove_regex();
    let compiled_regex = memento_dict::normalize::compile_remove_regex(remove_regex.as_str());
    let entries = subtitles.secondary_entries();
    let sid = subtitles.secondary_sid();
    app.set_has_secondary_subtitles(sid > 0 || !entries.is_empty());

    let selected_snapshot = DRAWER_CACHES.with(|slot| {
        let mut caches = slot.borrow_mut();
        caches.secondary_selection.sync_track(sid, entries.len());
        caches.secondary_selection.selected.clone()
    });
    let selected_set: HashSet<usize> = selected_snapshot.iter().copied().collect();

    let matching = if active_tab == 1 && !query.trim().is_empty() {
        search_entries(entries, &query, ignore_whitespace, compiled_regex.as_ref())
    } else {
        Vec::new()
    };
    let active = subtitles.active_indices(sid);
    let active_set: HashSet<usize> = active.iter().copied().collect();

    let model_ptr = (model as *const VecModel<SubEntry>) as usize;
    let first_cue_sig = entries.first().map(|c| (c.start.to_bits(), c.text.len()));
    let last_cue_sig = entries.last().map(|c| (c.start.to_bits(), c.text.len()));
    let sub_speed_bits = sub_speed.to_bits();
    let secondary_sub_delay_bits = secondary_sub_delay.to_bits();

    let can_patch_in_place = compiled_regex.is_none()
        && model.row_count() == entries.len()
        && DRAWER_CACHES.with(|slot| {
            let caches = slot.borrow();
            caches.last_secondary_model.as_ref().is_some_and(|prev| {
                prev.model_ptr == model_ptr
                    && prev.sid == sid
                    && prev.entry_count == entries.len()
                    && prev.remove_regex == remove_regex.as_str()
                    && prev.sub_speed_bits == sub_speed_bits
                    && prev.secondary_sub_delay_bits == secondary_sub_delay_bits
                    && prev.first_cue_sig == first_cue_sig
                    && prev.last_cue_sig == last_cue_sig
            })
        });

    if can_patch_in_place {
        let mut match_cursor = 0usize;
        for index in 0..entries.len() {
            while match_cursor < matching.len() && matching[match_cursor] < index {
                match_cursor += 1;
            }
            let is_match = matching.get(match_cursor) == Some(&index);
            let is_active = active_set.contains(&index);
            let is_selected = selected_set.contains(&index);
            if let Some(mut row) = model.row_data(index) {
                if row.is_active != is_active
                    || row.search_match != is_match
                    || row.selected != is_selected
                {
                    row.is_active = is_active;
                    row.search_match = is_match;
                    row.selected = is_selected;
                    model.set_row_data(index, row);
                }
            }
        }
    } else {
        let mut match_cursor = 0usize;
        let mut rows = Vec::with_capacity(entries.len());
        for (index, cue) in entries.iter().enumerate() {
            while match_cursor < matching.len() && matching[match_cursor] < index {
                match_cursor += 1;
            }
            let is_match = matching.get(match_cursor) == Some(&index);
            let raw = String::from_utf16_lossy(&cue.text);
            let display = memento_dict::normalize::apply_compiled_remove_regex(
                &raw,
                compiled_regex.as_ref(),
            );
            if compiled_regex.is_some() && display.trim().is_empty() {
                continue;
            }
            let start = subtitle_to_media_time(cue.start, sub_speed, secondary_sub_delay) as f32;
            let end = subtitle_to_media_time(cue.end, sub_speed, secondary_sub_delay) as f32;
            rows.push(SubEntry {
                id: (index + 1) as i32,
                start,
                end,
                text: display.into_owned().into(),
                selected: selected_set.contains(&index),
                is_active: active_set.contains(&index),
                search_match: is_match,
            });
        }
        model.set_vec(rows);
        DRAWER_CACHES.with(|slot| {
            let mut caches = slot.borrow_mut();
            caches.last_secondary_model = Some(SecondaryModelState {
                model_ptr,
                sid,
                entry_count: entries.len(),
                remove_regex: remove_regex.to_string(),
                sub_speed_bits,
                secondary_sub_delay_bits,
                first_cue_sig,
                last_cue_sig,
            });
            caches.secondary_revision = caches.secondary_revision.wrapping_add(1);
        });
    }

    auto_scroll_subtitle_drawer_if_needed(app, subtitles);
}

pub fn refresh_secondary_subtitle_list(
    app: &MementoApp,
    model: &VecModel<SubEntry>,
    subtitles: &SubtitleController,
) {
    refresh_secondary_subtitle_list_with_timing(app, model, subtitles, 1.0, 0.0);
}

fn normalize_search_text(text: &str, ignore_whitespace: bool) -> String {
    if ignore_whitespace {
        text.chars()
            .filter(|c| !c.is_whitespace())
            .flat_map(char::to_lowercase)
            .collect()
    } else {
        text.chars().flat_map(char::to_lowercase).collect()
    }
}

fn search_entries(
    entries: &[memento_subtitle::SubtitleEntry],
    query: &str,
    ignore_whitespace: bool,
    remove_regex: Option<&memento_dict::normalize::Regex>,
) -> Vec<usize> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let normalized_query = normalize_search_text(trimmed, ignore_whitespace);
    if normalized_query.is_empty() {
        return Vec::new();
    }
    entries
        .iter()
        .enumerate()
        .filter_map(|(idx, cue)| {
            let raw = String::from_utf16_lossy(&cue.text);
            let text = memento_dict::normalize::apply_compiled_remove_regex(&raw, remove_regex);
            let haystack = normalize_search_text(&text, ignore_whitespace);
            haystack.contains(&normalized_query).then_some(idx)
        })
        .collect()
}

fn search_model_rows<M: slint::Model<Data = SubEntry>>(
    model: &M,
    query: &str,
    ignore_whitespace: bool,
) -> Vec<usize> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let normalized_query = normalize_search_text(trimmed, ignore_whitespace);
    if normalized_query.is_empty() {
        return Vec::new();
    }
    (0..model.row_count())
        .filter(|&row_idx| {
            model.row_data(row_idx).is_some_and(|row| {
                normalize_search_text(row.text.as_str(), ignore_whitespace)
                    .contains(&normalized_query)
            })
        })
        .collect()
}

/// Apply sorted `matching` indices to `model` in `O(N)` time, mutating only rows
/// whose `search_match` flag changed.
fn apply_sorted_search_highlights<M: slint::Model<Data = SubEntry>>(model: &M, matching: &[usize]) {
    let mut cursor = 0usize;
    let count = model.row_count();
    for index in 0..count {
        while cursor < matching.len() && matching[cursor] < index {
            cursor += 1;
        }
        let is_match = matching.get(cursor) == Some(&index);
        if let Some(mut row) = model.row_data(index) {
            if row.search_match != is_match {
                row.search_match = is_match;
                model.set_row_data(index, row);
            }
        }
    }
}

pub(crate) fn auto_seek_to_matched_row(app: &MementoApp, row: usize) {
    if !app.get_subtitle_search_auto_seek() {
        return;
    }
    let model = if app.get_subtitle_drawer_tab() == 1 {
        app.get_secondary_subtitle_entries()
    } else {
        app.get_subtitle_entries()
    };
    if let Some(entry) = model.row_data(row) {
        app.invoke_seek_to(entry.start.max(0.0));
    }
}

fn entry_index_for_model_row<M: slint::Model<Data = SubEntry>>(
    model: &M,
    row_idx: usize,
) -> usize {
    model
        .row_data(row_idx)
        .and_then(|row| usize::try_from(row.id - 1).ok())
        .unwrap_or(row_idx)
}

fn apply_primary_click_selection(
    subtitles: &mut SubtitleController,
    anchor_cell: &Cell<Option<usize>>,
    entry_index: usize,
    ctrl: bool,
    shift: bool,
) {
    let count = subtitles.primary_entries().len();
    if entry_index >= count {
        return;
    }
    if shift {
        let anchor = anchor_cell
            .get()
            .filter(|&a| a < count)
            .unwrap_or(entry_index);
        anchor_cell.set(Some(anchor));
        let lo = anchor.min(entry_index);
        let hi = anchor.max(entry_index);
        if ctrl {
            let prev = subtitles.selected_indices().to_vec();
            subtitles.select_range(lo, hi, &prev);
        } else {
            subtitles.clear_selection();
            subtitles.select_range(lo, hi, &[]);
        }
    } else if ctrl {
        subtitles.toggle_select(entry_index);
        anchor_cell.set(Some(entry_index));
    } else {
        subtitles.clear_selection();
        subtitles.set_selected(entry_index, true);
        anchor_cell.set(Some(entry_index));
    }
}

pub(crate) fn install_subtitle_selection(
    app: &MementoApp,
    subtitles: Rc<RefCell<SubtitleController>>,
    rows: Rc<VecModel<SubEntry>>,
) {
    let search_matches: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));
    let primary_anchor: Rc<Cell<Option<usize>>> = Rc::new(Cell::new(None));
    // Track whether the current primary selection came solely from a drawer lookup
    // so hover-lookup (e.g. Shift+move before Shift+Click) never clobbers an explicit
    // click/drag selection or range anchor.
    let selection_from_lookup: Rc<Cell<bool>> = Rc::new(Cell::new(false));
    // `subtitle_drawer.slint` emits `subtitle-row-clicked-mod` followed immediately
    // by legacy `row-clicked`; suppress the trailing legacy callback so clicks never double-toggle.
    let mod_click_handled: Rc<Cell<bool>> = Rc::new(Cell::new(false));

    let mod_app = app.as_weak();
    let mod_subs = subtitles.clone();
    let mod_rows = rows.clone();
    let mod_anchor = primary_anchor.clone();
    let mod_lookup_sel = selection_from_lookup.clone();
    let mod_handled_flag = mod_click_handled.clone();
    app.on_subtitle_row_clicked_mod(move |tab, row_idx, ctrl, shift| {
        mod_handled_flag.set(true);
        let Ok(model_row) = usize::try_from(row_idx) else {
            return;
        };
        let Some(app) = mod_app.upgrade() else {
            return;
        };
        if tab == 1 {
            let sec_model = app.get_secondary_subtitle_entries();
            let entry_idx = entry_index_for_model_row(&sec_model, model_row);
            let subs = mod_subs.borrow();
            let sid = subs.secondary_sid();
            let count = subs.secondary_entries().len().max(sec_model.row_count());
            let selected = DRAWER_CACHES.with(|slot| {
                let mut caches = slot.borrow_mut();
                caches.secondary_selection.sync_track(sid, count);
                caches
                    .secondary_selection
                    .apply_click(entry_idx, count, ctrl, shift);
                caches.secondary_selection.selected.clone()
            });
            apply_secondary_selection_marks(&sec_model, &selected);
        } else {
            mod_lookup_sel.set(false);
            let entry_idx = entry_index_for_model_row(mod_rows.as_ref(), model_row);
            let mut subs = mod_subs.borrow_mut();
            apply_primary_click_selection(&mut subs, &mod_anchor, entry_idx, ctrl, shift);
            refresh_subtitle_marks(&mod_rows, &subs);
        }
    });

    let controller = subtitles.clone();
    let model = rows.clone();
    let legacy_anchor = primary_anchor.clone();
    let legacy_lookup_sel = selection_from_lookup.clone();
    let legacy_handled_flag = mod_click_handled;
    app.on_select_row(move |row, extend_range| {
        if legacy_handled_flag.replace(false) {
            return;
        }
        let Ok(model_row) = usize::try_from(row) else {
            return;
        };
        legacy_lookup_sel.set(false);
        let entry_idx = entry_index_for_model_row(model.as_ref(), model_row);
        let mut subtitles = controller.borrow_mut();
        apply_primary_click_selection(
            &mut subtitles,
            &legacy_anchor,
            entry_idx,
            false,
            extend_range,
        );
        refresh_subtitle_marks(&model, &subtitles);
    });

    let range_app = app.as_weak();
    let controller = subtitles.clone();
    let model = rows.clone();
    let range_anchor = primary_anchor.clone();
    let range_lookup_sel = selection_from_lookup.clone();
    let drag_marks: RefCell<(i32, i64, Vec<usize>)> = RefCell::new((0, 0, Vec::new()));
    app.on_select_subtitle_range(move |first, last, begin| {
        let (Ok(first_row), Ok(last_row)) = (usize::try_from(first), usize::try_from(last)) else {
            return;
        };
        let Some(app) = range_app.upgrade() else {
            return;
        };
        let active_tab = app.get_subtitle_drawer_tab();
        let mut previous = drag_marks.borrow_mut();
        if active_tab == 1 {
            let sec_model = app.get_secondary_subtitle_entries();
            let first_idx = entry_index_for_model_row(&sec_model, first_row);
            let last_idx = entry_index_for_model_row(&sec_model, last_row);
            let subs = controller.borrow();
            let sid = subs.secondary_sid();
            let count = subs.secondary_entries().len().max(sec_model.row_count());
            let selected = DRAWER_CACHES.with(|slot| {
                let mut caches = slot.borrow_mut();
                caches.secondary_selection.sync_track(sid, count);
                if begin {
                    *previous = (1, sid, caches.secondary_selection.selected.clone());
                }
                if previous.0 != 1 || previous.1 != sid {
                    return caches.secondary_selection.selected.clone();
                }
                caches
                    .secondary_selection
                    .select_range(first_idx, last_idx, &previous.2, count);
                caches.secondary_selection.selected.clone()
            });
            apply_secondary_selection_marks(&sec_model, &selected);
            return;
        }
        range_lookup_sel.set(false);
        let first_idx = entry_index_for_model_row(model.as_ref(), first_row);
        let last_idx = entry_index_for_model_row(model.as_ref(), last_row);
        let mut subtitles = controller.borrow_mut();
        if begin {
            *previous = (
                0,
                subtitles.primary_sid(),
                subtitles.selected_indices().to_vec(),
            );
        }
        if previous.0 != 0 || previous.1 != subtitles.primary_sid() {
            return;
        }
        range_anchor.set(Some(first_idx));
        subtitles.select_range(first_idx, last_idx, &previous.2);
        refresh_subtitle_marks(&model, &subtitles);
    });

    let mark_app = app.as_weak();
    let controller = subtitles.clone();
    let model = rows.clone();
    let mark_anchor = primary_anchor.clone();
    let mark_lookup_sel = selection_from_lookup.clone();
    app.on_mark_subtitle_row(move |row| {
        let Ok(model_row) = usize::try_from(row) else {
            return;
        };
        let Some(app) = mark_app.upgrade() else {
            return;
        };
        if app.get_subtitle_drawer_tab() == 1 {
            let sec_model = app.get_secondary_subtitle_entries();
            let entry_idx = entry_index_for_model_row(&sec_model, model_row);
            let subs = controller.borrow();
            let sid = subs.secondary_sid();
            let count = subs.secondary_entries().len().max(sec_model.row_count());
            let selected = DRAWER_CACHES.with(|slot| {
                let mut caches = slot.borrow_mut();
                caches.secondary_selection.sync_track(sid, count);
                caches
                    .secondary_selection
                    .set_selected(entry_idx, count, true);
                caches.secondary_selection.selected.clone()
            });
            apply_secondary_selection_marks(&sec_model, &selected);
            return;
        }
        mark_lookup_sel.set(false);
        let entry_idx = entry_index_for_model_row(model.as_ref(), model_row);
        let mut subtitles = controller.borrow_mut();
        mark_anchor.set(Some(entry_idx));
        subtitles.set_selected(entry_idx, true);
        refresh_subtitle_marks(&model, &subtitles);
    });

    // M1: Dictionary lookup inside Primary/Secondary subtitle drawer rows, setting
    // that row as the active sentence & timing context for Anki mining without
    // clobbering an explicit click/drag multi-selection or range anchor.
    let lookup_app = app.as_weak();
    let lookup_subs = subtitles.clone();
    let lookup_rows = rows.clone();
    let lookup_sel_flag = selection_from_lookup;
    app.on_subtitle_row_lookup(move |tab, row_idx, char_idx, _lx, _ly| {
        let Ok(model_row) = usize::try_from(row_idx) else {
            return;
        };
        let Some(app) = lookup_app.upgrade() else {
            return;
        };
        let (row_entry, entry_idx) = if tab == 1 {
            let sec_model = app.get_secondary_subtitle_entries();
            let Some(entry) = sec_model.row_data(model_row) else {
                return;
            };
            let idx = entry_index_for_model_row(&sec_model, model_row);
            (entry, idx)
        } else {
            let Some(entry) = lookup_rows.row_data(model_row) else {
                return;
            };
            let idx = entry_index_for_model_row(lookup_rows.as_ref(), model_row);
            (entry, idx)
        };
        let Some(query) = slice_lookup_from_char(row_entry.text.as_str(), char_idx) else {
            return;
        };
        ui::clear_rich_bridge_ass_lookup(&app);
        if tab == 1 {
            let sec_model = app.get_secondary_subtitle_entries();
            {
                let subs = lookup_subs.borrow();
                let sid = subs.secondary_sid();
                let count = subs.secondary_entries().len().max(sec_model.row_count());
                let selected = DRAWER_CACHES.with(|slot| {
                    let mut caches = slot.borrow_mut();
                    caches.secondary_selection.sync_track(sid, count);
                    if caches.secondary_selection.selected.is_empty() {
                        caches
                            .secondary_selection
                            .set_selected(entry_idx, count, true);
                    }
                    caches.secondary_selection.selected.clone()
                });
                apply_secondary_selection_marks(&sec_model, &selected);
            }
            app.set_active_sub_text(row_entry.text.clone());
            app.set_secondary_sub_text(row_entry.text.clone());
        } else {
            {
                let mut subs = lookup_subs.borrow_mut();
                if subs.selected_indices().is_empty() || lookup_sel_flag.get() {
                    subs.clear_selection();
                    subs.set_selected(entry_idx, true);
                    lookup_sel_flag.set(true);
                    refresh_subtitle_marks(&lookup_rows, &subs);
                }
            }
            app.set_active_sub_text(row_entry.text.clone());
        }
        // Position popup cleanly near the right edge of the video area adjacent to the drawer.
        let half_video = (app.get_video_width() as f32) * 0.5;
        if half_video > 0.0 {
            app.set_popup_offset_x((half_video - 60.0).max(0.0));
        }
        let pb = app.global::<PopupBridge>();
        if pb.get_rich_enabled() {
            pb.invoke_panel_lookup_requested(query.into());
        } else {
            app.invoke_lookup_word(query.into());
        }
    });

    let search_subs = subtitles.clone();
    let search_rows = rows;
    let search_matches_rc = search_matches.clone();
    let weak = app.as_weak();
    app.on_search_subtitles(move |query| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        app.set_subtitle_search_query(query.clone());
        let active_tab = app.get_subtitle_drawer_tab();
        let ignore_whitespace = app.get_subtitle_search_ignore_whitespace();
        let suppress_seek = DRAWER_CACHES.with(|slot| slot.borrow().suppress_search_auto_seek);
        let mut matching = if query.trim().is_empty() {
            Vec::new()
        } else if active_tab == 1 {
            let sec_model = app.get_secondary_subtitle_entries();
            if sec_model.row_count() > 0 {
                search_model_rows(&sec_model, &query, ignore_whitespace)
            } else {
                let remove_regex = app.get_search_remove_regex();
                let compiled_regex =
                    memento_dict::normalize::compile_remove_regex(remove_regex.as_str());
                let subs = search_subs.borrow();
                search_entries(
                    subs.secondary_entries(),
                    &query,
                    ignore_whitespace,
                    compiled_regex.as_ref(),
                )
            }
        } else if search_rows.row_count() > 0 {
            search_model_rows(search_rows.as_ref(), &query, ignore_whitespace)
        } else {
            let remove_regex = app.get_search_remove_regex();
            let compiled_regex =
                memento_dict::normalize::compile_remove_regex(remove_regex.as_str());
            let subs = search_subs.borrow();
            search_entries(
                subs.primary_entries(),
                &query,
                ignore_whitespace,
                compiled_regex.as_ref(),
            )
        };
        // Ensure strictly ascending order for O(N) two-pointer highlight update.
        matching.sort_unstable();
        matching.dedup();

        if active_tab == 1 {
            let sec_model = app.get_secondary_subtitle_entries();
            apply_sorted_search_highlights(&sec_model, &matching);
            apply_sorted_search_highlights(search_rows.as_ref(), &[]);
        } else {
            apply_sorted_search_highlights(search_rows.as_ref(), &matching);
        }

        let total = matching.len() as i32;
        app.set_subtitle_search_total_matches(total);
        if let Some(&first_row) = matching.first() {
            if suppress_seek {
                let prev_row = app.get_subtitle_search_focused_row();
                if prev_row >= 0 {
                    if let Some(pos) = matching.iter().position(|&r| r as i32 == prev_row) {
                        app.set_subtitle_search_current_match((pos + 1) as i32);
                        app.set_subtitle_search_focused_row(prev_row);
                    } else {
                        app.set_subtitle_search_current_match(1);
                        app.set_subtitle_search_focused_row(first_row as i32);
                    }
                } else {
                    app.set_subtitle_search_current_match(1);
                    app.set_subtitle_search_focused_row(first_row as i32);
                }
            } else {
                app.set_subtitle_search_current_match(1);
                app.set_subtitle_search_focused_row(first_row as i32);
                scroll_drawer_to_row_accurate(&app, first_row);
                auto_seek_to_matched_row(&app, first_row);
            }
        } else {
            app.set_subtitle_search_current_match(0);
            app.set_subtitle_search_focused_row(-1);
        }
        *search_matches_rc.borrow_mut() = matching;
    });

    let next_matches = search_matches.clone();
    let next_weak = app.as_weak();
    app.on_next_subtitle_search_match(move || {
        let Some(app) = next_weak.upgrade() else {
            return;
        };
        let matches = next_matches.borrow();
        if matches.is_empty() {
            return;
        }
        let len = matches.len();
        let current = app.get_subtitle_search_current_match().max(1) as usize;
        let next_idx = current % len;
        let row = matches[next_idx];
        app.set_subtitle_search_current_match((next_idx + 1) as i32);
        app.set_subtitle_search_focused_row(row as i32);
        scroll_drawer_to_row_accurate(&app, row);
        auto_seek_to_matched_row(&app, row);
    });

    let prev_matches = search_matches;
    let prev_weak = app.as_weak();
    app.on_prev_subtitle_search_match(move || {
        let Some(app) = prev_weak.upgrade() else {
            return;
        };
        let matches = prev_matches.borrow();
        if matches.is_empty() {
            return;
        }
        let len = matches.len();
        let current = app.get_subtitle_search_current_match().max(1) as usize;
        let prev_idx = (current + len - 2) % len;
        let row = matches[prev_idx];
        app.set_subtitle_search_current_match((prev_idx + 1) as i32);
        app.set_subtitle_search_focused_row(row as i32);
        scroll_drawer_to_row_accurate(&app, row);
        auto_seek_to_matched_row(&app, row);
    });

    let tab_weak = app.as_weak();
    let tab_subs = subtitles.clone();
    app.on_subtitle_drawer_tab_changed(move |_tab| {
        let Some(app) = tab_weak.upgrade() else {
            return;
        };
        let q = app.get_subtitle_search_query();
        app.invoke_search_subtitles(q.clone());
        if q.trim().is_empty() {
            if let Ok(subs) = tab_subs.try_borrow() {
                auto_scroll_subtitle_drawer_if_needed(&app, &subs);
            }
        }
    });

    let user_scroll_timer = Rc::new(slint::Timer::default());
    let scroll_timer_handle = user_scroll_timer;
    let scroll_weak = app.as_weak();
    app.on_subtitle_drawer_user_scrolled(move || {
        let weak = scroll_weak.clone();
        scroll_timer_handle.start(
            slint::TimerMode::SingleShot,
            std::time::Duration::from_millis(2500),
            move || {
                if let Some(app) = weak.upgrade() {
                    app.set_subtitle_drawer_user_scrolling(false);
                }
            },
        );
    });

    let copy_subs = subtitles;
    let copy_weak = app.as_weak();
    app.on_copy_selected_subtitles(move || {
        let Some(app) = copy_weak.upgrade() else {
            return;
        };
        let subs = copy_subs.borrow();
        let remove_regex = app.get_search_remove_regex();
        let text = if app.get_subtitle_drawer_tab() == 1 {
            let sec_entries = subs.secondary_entries();
            let sid = subs.secondary_sid();
            let sec_model = app.get_secondary_subtitle_entries();
            let selected = DRAWER_CACHES.with(|slot| {
                let mut caches = slot.borrow_mut();
                caches
                    .secondary_selection
                    .sync_track(sid, sec_entries.len().max(sec_model.row_count()));
                caches.secondary_selection.selected.clone()
            });
            if !selected.is_empty() && !sec_entries.is_empty() {
                selected
                    .iter()
                    .filter_map(|&idx| sec_entries.get(idx))
                    .map(|c| {
                        let raw = String::from_utf16_lossy(&c.text);
                        dict::apply_remove_regex(&raw, remove_regex.as_str())
                    })
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n")
            } else if !selected.is_empty() {
                selected
                    .iter()
                    .filter_map(|&idx| sec_model.row_data(idx))
                    .map(|r| r.text.to_string())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                app.get_secondary_sub_text().to_string()
            }
        } else {
            let selected_cues = subs.selected_cues();
            if !selected_cues.is_empty() {
                selected_cues
                    .iter()
                    .map(|c| {
                        let raw = String::from_utf16_lossy(&c.text);
                        dict::apply_remove_regex(&raw, remove_regex.as_str())
                    })
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                let active = app.get_active_sub_text().to_string();
                if !active.is_empty() {
                    active
                } else {
                    subs.active_cue_text()
                        .map(|raw| dict::apply_remove_regex(&raw, remove_regex.as_str()))
                        .unwrap_or_default()
                }
            }
        };
        if !text.is_empty() {
            copy_text_to_clipboard(&text);
            app.set_copied_subtitle_text(text.into());
            app.set_status_message("Copied subtitle to clipboard".into());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::asr::AsrController;
    use crate::controller::player::{
        clear_subtitle_marks_on_advance, sync_player_tracks as sync_player_tracks_with_asr,
    };
    use crate::controller::subtitles::{create_cue, MemorySubtitleCue, ASR_TRACK_ID};
    use crate::settings_ui::clear_asr_for_settings;
    use crate::ui::{
        set_dictionary_rows, subtitle_lines_for_app, subtitle_lines_from_words,
        subtitle_token_width, DictResult, SubtitleWord,
    };
    use slint::platform::{Key, PointerEventButton, WindowEvent};
    use std::cell::Cell;

    #[test]
    fn context_marks_clear_when_playback_reaches_the_next_subtitle() {
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        let rows = VecModel::<SubEntry>::default();
        let mut subtitles = SubtitleController::new();
        let mut previous = None;
        subtitles.load_cues(vec![
            create_cue("Repeated line", 1., 2.),
            create_cue("Repeated line", 3., 4.),
            create_cue("Last", 5., 6.),
        ]);
        subtitles.update_position(1.5);
        clear_subtitle_marks_on_advance(&rows, &mut subtitles, &mut previous, true);
        subtitles.select_range(0, 2, &[]);
        refresh_subtitle_list(&app, &rows, &subtitles);
        for position in [1.8, 2.5] {
            subtitles.update_position(position);
            clear_subtitle_marks_on_advance(&rows, &mut subtitles, &mut previous, false);
            assert_eq!(
                subtitles.selected_indices(),
                &[0, 1, 2],
                "resuming and the gap retain marks until the next cue"
            );
        }
        subtitles.update_position(3.5);
        clear_subtitle_marks_on_advance(&rows, &mut subtitles, &mut previous, false);
        assert!(subtitles.selected_indices().is_empty());
        assert!(rows.iter().all(|row| !row.selected));
        subtitles.select_all();
        subtitles.update_position(5.5);
        clear_subtitle_marks_on_advance(&rows, &mut subtitles, &mut previous, true);
        clear_subtitle_marks_on_advance(&rows, &mut subtitles, &mut previous, false);
        assert_eq!(
            subtitles.selected_indices(),
            &[0, 1, 2],
            "a paused seek and resume on the same cue retain marks"
        );

        subtitles.start_asr_track();
        let cue = |id, start| MemorySubtitleCue {
            id,
            text: "ASR".encode_utf16().collect(),
            start,
            end: start + 1.,
        };
        subtitles.add_asr_cue(cue(1, 3.), 3.5);
        clear_subtitle_marks_on_advance(&rows, &mut subtitles, &mut previous, true);
        subtitles.select_all();
        subtitles.add_asr_cue(cue(2, 1.), 3.5);
        subtitles.add_asr_cue(cue(3, 5.), 3.5);
        refresh_subtitle_list(&app, &rows, &subtitles);
        clear_subtitle_marks_on_advance(&rows, &mut subtitles, &mut previous, false);
        assert_eq!(subtitles.selected_indices(), &[1]);
        subtitles.update_position(5.5);
        clear_subtitle_marks_on_advance(&rows, &mut subtitles, &mut previous, false);
        assert!(subtitles.selected_indices().is_empty());
        assert!(rows.iter().all(|row| !row.selected));
    }

    #[test]
    fn drawer_marks_multiple_cues_without_search_or_playback_overwriting_them() {
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        app.window().set_size(slint::LogicalSize::new(1280., 720.));
        app.set_duration(120.);
        app.set_sub_drawer_open(true);
        let subtitles = Rc::new(RefCell::new(SubtitleController::new()));
        subtitles.borrow_mut().load_cues(vec![
            create_cue("First", 1., 2.),
            create_cue("Middle", 3., 4.),
            create_cue("Last", 5., 6.),
        ]);
        let rows = Rc::new(VecModel::<SubEntry>::default());
        app.set_subtitle_entries(rows.clone().into());
        refresh_subtitle_list(&app, &rows, &subtitles.borrow());
        install_subtitle_selection(&app, subtitles.clone(), rows.clone());
        app.show().unwrap();
        let click = |x, y| {
            let position = slint::LogicalPosition::new(x, y);
            app.window().dispatch_event(WindowEvent::PointerPressed {
                position,
                button: PointerEventButton::Left,
            });
            app.window().dispatch_event(WindowEvent::PointerReleased {
                position,
                button: PointerEventButton::Left,
            });
        };
        click(950., 88.);
        assert_eq!(subtitles.borrow().selected_indices(), &[0]);
        app.window().dispatch_event(WindowEvent::KeyPressed {
            text: Key::Shift.into(),
        });
        click(950., 136.);
        app.window().dispatch_event(WindowEvent::KeyReleased {
            text: Key::Shift.into(),
        });
        assert_eq!(subtitles.borrow().selected_indices(), &[0, 1, 2]);
        // M1: Ctrl+Click toggles an individual row out of the selection without clearing others.
        app.window().dispatch_event(WindowEvent::KeyPressed {
            text: Key::Control.into(),
        });
        click(1100., 112.);
        app.window().dispatch_event(WindowEvent::KeyReleased {
            text: Key::Control.into(),
        });
        assert_eq!(subtitles.borrow().selected_indices(), &[0, 2]);
        app.invoke_search_subtitles("Middle".into());
        assert!(rows.row_data(1).unwrap().search_match);
        assert!(
            !rows.row_data(1).unwrap().selected,
            "search matches are not Anki marks"
        );
        subtitles.borrow_mut().update_position(3.5);
        refresh_subtitle_list(&app, &rows, &subtitles.borrow());
        assert!(rows.row_data(1).unwrap().is_active);
        assert!(!rows.row_data(1).unwrap().selected);
        assert!(rows.row_data(0).unwrap().selected && rows.row_data(2).unwrap().selected);
        app.invoke_search_subtitles("".into());
        assert!(rows.iter().all(|row| !row.search_match));
        let seek = Rc::new(Cell::new(0.));
        let result = seek.clone();
        app.on_seek_to(move |position| result.set(position));
        click(1100., 136.);
        click(1100., 136.);
        assert_eq!(seek.get(), 5.);
        assert!(subtitles.borrow().is_selected(2));
        subtitles.borrow_mut().clear_selection();
        refresh_subtitle_marks(&rows, &subtitles.borrow());
        assert!(rows.iter().all(|row| !row.selected));
        app.invoke_mark_subtitle_row(2);
        let position = slint::LogicalPosition::new(1100., 88.);
        app.window().dispatch_event(WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        });
        app.window().dispatch_event(WindowEvent::PointerMoved {
            position: slint::LogicalPosition::new(1100., 136.),
        });
        assert_eq!(subtitles.borrow().selected_indices(), &[0, 1, 2]);
        app.window()
            .dispatch_event(WindowEvent::PointerMoved { position });
        assert_eq!(subtitles.borrow().selected_indices(), &[0, 2]);
        app.window().dispatch_event(WindowEvent::PointerReleased {
            position,
            button: PointerEventButton::Left,
        });
        assert_eq!(
            subtitles.borrow().selected_indices(),
            &[0, 2],
            "release must not toggle the start row"
        );
        subtitles.borrow_mut().clear_selection();
        refresh_subtitle_marks(&rows, &subtitles.borrow());
        let position = slint::LogicalPosition::new(1100., 136.);
        app.window().dispatch_event(WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        });
        let position = slint::LogicalPosition::new(1100., 88.);
        app.window()
            .dispatch_event(WindowEvent::PointerMoved { position });
        assert_eq!(
            subtitles.borrow().selected_indices(),
            &[0, 1, 2],
            "upward drag must also mark a range"
        );
        app.window().dispatch_event(WindowEvent::PointerReleased {
            position,
            button: PointerEventButton::Left,
        });
        assert_eq!(subtitles.borrow().selected_indices(), &[0, 1, 2]);
        app.window().dispatch_event(WindowEvent::PointerMoved {
            position: slint::LogicalPosition::new(1100., 112.),
        });
        assert_eq!(subtitles.borrow().selected_indices(), &[0, 1, 2]);
        // M1: Ctrl+Click toggles an individual row out of the selection without clearing others.
        app.window().dispatch_event(WindowEvent::KeyPressed {
            text: Key::Control.into(),
        });
        click(1100., 112.);
        app.window().dispatch_event(WindowEvent::KeyReleased {
            text: Key::Control.into(),
        });
        assert_eq!(subtitles.borrow().selected_indices(), &[0, 2]);
        subtitles.borrow_mut().load_cues(
            (0..100)
                .map(|index| {
                    create_cue(
                        &format!("Cue {index}"),
                        index as f64 * 2.,
                        index as f64 * 2. + 1.,
                    )
                })
                .collect::<Vec<_>>(),
        );
        refresh_subtitle_list(&app, &rows, &subtitles.borrow());
        let position = slint::LogicalPosition::new(1100., 88.);
        app.window().dispatch_event(WindowEvent::PointerScrolled {
            position,
            delta_x: 0.,
            delta_y: -360.,
        });
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(500));
        app.window().dispatch_event(WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        });
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(600));
        let position = slint::LogicalPosition::new(1100., 136.);
        app.window()
            .dispatch_event(WindowEvent::PointerMoved { position });
        app.window().dispatch_event(WindowEvent::PointerReleased {
            position,
            button: PointerEventButton::Left,
        });
        let selection = subtitles.borrow().selected_indices().to_vec();
        assert_eq!(selection.len(), 3);
        assert!(
            selection[0] > 0,
            "wheel scrolling must still work before dragging"
        );
        assert_eq!(
            selection,
            (selection[0]..=selection[0] + 2).collect::<Vec<_>>()
        );
        app.hide().unwrap();
    }

    #[test]
    fn mpv_track_sync_preserves_asr_and_settings_reset_only_clears_its_cache() {
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        app.set_asr_enabled(true);
        let asr = Rc::new(RefCell::new(AsrController::new()));
        let subtitles = Rc::new(RefCell::new(SubtitleController::new()));
        let rows = Rc::new(VecModel::<SubEntry>::default());
        let restart = Rc::new(Cell::new(false));
        {
            let mut subtitles = subtitles.borrow_mut();
            subtitles.ensure_track(2);
            subtitles.set_track_cues(2, vec![create_cue("File", 1.0, 3.0)]);
            subtitles.start_asr_track();
            subtitles.add_asr_cue(
                MemorySubtitleCue {
                    id: 1,
                    text: "ASR".encode_utf16().collect(),
                    start: 1.0,
                    end: 3.0,
                },
                2.0,
            );
        }
        app.set_asr_track_selected(true);
        app.set_asr_running(true);
        let mut state = memento_player::mpv::PlaybackState::default();
        state.position = 2.0;
        let mut audio = Vec::new();
        let mut tracks = Vec::new();
        let _ = sync_player_tracks_with_asr(
            &app,
            &state,
            &mut subtitles.borrow_mut(),
            &rows,
            &mut audio,
            &mut tracks,
            None,
        );
        assert_eq!(subtitles.borrow().primary_sid(), ASR_TRACK_ID);
        assert_eq!(subtitles.borrow().active_cue_text().as_deref(), Some("ASR"));
        app.set_asr_running(false);
        let _ = sync_player_tracks_with_asr(
            &app,
            &state,
            &mut subtitles.borrow_mut(),
            &rows,
            &mut audio,
            &mut tracks,
            None,
        );
        assert_eq!(subtitles.borrow().primary_sid(), ASR_TRACK_ID);
        assert_eq!(subtitles.borrow().active_cue_text().as_deref(), Some("ASR"));
        app.set_asr_track_selected(false);
        state.primary_sid = 2;
        let _ = sync_player_tracks_with_asr(
            &app,
            &state,
            &mut subtitles.borrow_mut(),
            &rows,
            &mut audio,
            &mut tracks,
            None,
        );
        assert_eq!(rows.row_data(0).unwrap().text, "File");
        app.set_active_sub_text("File".into());
        clear_asr_for_settings(&app, &asr, &subtitles, &rows, &restart, None, None);
        assert_eq!(app.get_active_sub_text(), "File");
        assert_eq!(rows.row_data(0).unwrap().text, "File");
        assert!(!restart.get());
        assert!(!subtitles.borrow_mut().resume_asr_track(2.0));
        subtitles.borrow_mut().start_asr_track();
        app.set_asr_track_selected(true);
        app.set_asr_running(true);
        clear_asr_for_settings(&app, &asr, &subtitles, &rows, &restart, None, None);
        assert!(restart.get());
        assert!(app.get_asr_track_selected());
        assert!(!app.get_asr_running());
        assert!(rows.row_count() == 0);
        assert!(app.get_active_sub_text().is_empty());
    }

    #[test]
    fn furigana_subtitle_wraps_within_narrow_video_without_losing_text() {
        let parts = [
            ("生前", "せいぜん"),
            ("に", ""),
            ("世界", "せかい"),
            ("中", "じゅう"),
            ("を", ""),
            ("旅行", "りょこう"),
            ("し", ""),
            ("て", ""),
            ("集め", "あつめ"),
            ("た", ""),
            ("遺品", "いひん"),
            ("が", ""),
        ];
        let words: Vec<_> = parts
            .iter()
            .enumerate()
            .map(|(index, (text, reading))| SubtitleWord {
                text: (*text).into(),
                reading: (*reading).into(),
                query: (*text).into(),
                index: index as i32,
            })
            .collect();
        let lines = subtitle_lines_from_words(words, 428.0, 38.0, true);
        assert!(lines.len() >= 2);
        let mut rebuilt = String::new();
        for line in lines {
            let line_words: Vec<_> = line.words.iter().collect();
            let width: f32 = line_words
                .iter()
                .map(|word| subtitle_token_width(word, 38.0, true))
                .sum();
            assert!(width <= 428.0, "subtitle line exceeds video width: {width}");
            for word in line_words {
                rebuilt.push_str(word.text.as_str());
            }
        }
        assert_eq!(rebuilt, "生前に世界中を旅行して集めた遺品が");
    }

    #[test]
    fn subtitle_drawer_search_match_counter_navigation_tabs_copy_and_resizable_split() {
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        app.window().set_size(slint::LogicalSize::new(1280., 720.));
        app.set_sub_drawer_open(true);
        app.set_dict_search_open(true);
        let subtitles = Rc::new(RefCell::new(SubtitleController::new()));
        subtitles.borrow_mut().load_cues(vec![
            create_cue("Alpha one", 1., 2.),
            create_cue("Beta two", 3., 4.),
            create_cue("Alpha three", 5., 6.),
        ]);
        subtitles.borrow_mut().ensure_track(2);
        subtitles.borrow_mut().set_track_cues(
            2,
            vec![
                create_cue("Secondary cue A", 1., 2.),
                create_cue("Secondary cue B", 3., 4.),
            ],
        );
        subtitles.borrow_mut().set_secondary_sid(2);
        app.set_secondary_sid(2);

        let primary_rows = Rc::new(VecModel::<SubEntry>::default());
        let secondary_rows = Rc::new(VecModel::<SubEntry>::default());
        app.set_subtitle_entries(primary_rows.clone().into());
        app.set_secondary_subtitle_entries(secondary_rows.clone().into());
        refresh_subtitle_list(&app, &primary_rows, &subtitles.borrow());
        refresh_secondary_subtitle_list(&app, &secondary_rows, &subtitles.borrow());
        install_subtitle_selection(&app, subtitles.clone(), primary_rows.clone());
        app.show().unwrap();

        assert!(app.get_dict_search_open() && app.get_sub_drawer_open());
        app.set_sidebar_width(400.0);
        app.set_sidebar_split_ratio(0.35);
        assert_eq!(app.get_sidebar_width(), 400.0);
        assert!((app.get_sidebar_split_ratio() - 0.35).abs() < 1e-4);

        app.invoke_search_subtitles("Alpha".into());
        assert_eq!(app.get_subtitle_search_total_matches(), 2);
        assert_eq!(app.get_subtitle_search_current_match(), 1);
        assert_eq!(app.get_subtitle_search_focused_row(), 0);
        app.invoke_next_subtitle_search_match();
        assert_eq!(app.get_subtitle_search_current_match(), 2);
        assert_eq!(app.get_subtitle_search_focused_row(), 2);
        app.invoke_next_subtitle_search_match();
        assert_eq!(app.get_subtitle_search_current_match(), 1);
        assert_eq!(app.get_subtitle_search_focused_row(), 0);

        app.set_subtitle_drawer_tab(1);
        app.invoke_subtitle_drawer_tab_changed(1);
        assert_eq!(app.get_subtitle_search_total_matches(), 0);
        app.invoke_search_subtitles("Secondary".into());
        assert_eq!(app.get_subtitle_search_total_matches(), 2);
        assert_eq!(app.get_subtitle_search_current_match(), 1);
        assert_eq!(app.get_subtitle_search_focused_row(), 0);
        app.invoke_prev_subtitle_search_match();
        assert_eq!(app.get_subtitle_search_current_match(), 2);
        assert_eq!(app.get_subtitle_search_focused_row(), 1);
    }

    #[test]
    fn subtitle_drawer_auto_scrolls_on_active_cue_advance_and_respects_user_scrolling_and_hover_pause_release(
    ) {
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        app.window().set_size(slint::LogicalSize::new(1280., 720.));
        app.set_sub_drawer_open(true);
        let subtitles = Rc::new(RefCell::new(SubtitleController::new()));
        let mut cues = Vec::new();
        for i in 0..30 {
            let start = (i * 2 + 1) as f64;
            let text = if i < 5 {
                format!("Multi-line wrapped subtitle cue #{i} with extra long text that wraps across multiple lines in the subtitle drawer panel")
            } else {
                format!("Cue line #{i}")
            };
            cues.push(create_cue(&text, start, start + 1.5));
        }
        subtitles.borrow_mut().load_cues(cues);
        let rows = Rc::new(VecModel::<SubEntry>::default());
        app.set_subtitle_entries(rows.clone().into());
        install_subtitle_selection(&app, subtitles.clone(), rows.clone());
        app.show().unwrap();

        assert_eq!(app.get_subtitle_drawer_viewport_y(), 0.0);

        subtitles.borrow_mut().update_position(45.5);
        refresh_subtitle_list(&app, &rows, &subtitles.borrow());
        let scrolled_y = app.get_subtitle_drawer_viewport_y();
        assert!(
            scrolled_y < -100.0,
            "Active cue advance must auto-scroll SubtitleDrawerPanel (viewport_y={scrolled_y})"
        );

        app.set_subtitle_drawer_user_scrolling(true);
        subtitles.borrow_mut().update_position(1.5);
        refresh_subtitle_list(&app, &rows, &subtitles.borrow());
        assert_eq!(
            app.get_subtitle_drawer_viewport_y(),
            scrolled_y,
            "Auto-scroll must not fight the user while user_scrolling is active"
        );

        app.set_subtitle_drawer_user_scrolling(false);
        subtitles.borrow_mut().update_position(1.5);
        refresh_subtitle_list(&app, &rows, &subtitles.borrow());
        assert_eq!(app.get_subtitle_drawer_viewport_y(), 0.0);

        let hover_state = Rc::new(Cell::new(false));
        let hover_sink = hover_state.clone();
        app.on_subtitle_hover_changed(move |hovering| hover_sink.set(hovering));
        app.set_subtitles_visible(true);
        app.set_active_sub_text("生前に世界中".into());
        let words = vec![SubtitleWord {
            text: "生前".into(),
            reading: "せいぜん".into(),
            query: "生前".into(),
            index: 0,
        }];
        let lines = subtitle_lines_for_app(&app, words.clone());
        app.set_active_sub_words(Rc::new(VecModel::from(words)).into());
        app.set_active_sub_lines(Rc::new(VecModel::from(lines)).into());
        set_dictionary_rows(
            &app,
            vec![DictResult {
                index: 0,
                expression: "生前".into(),
                reading: "せいぜん".into(),
                glossary: "during one's lifetime".into(),
                dict_name: "JMdict".into(),
                selected: true,
                term_start: true,
                term_end: true,
                ..Default::default()
            }],
        );
        app.set_dict_open(true);

        let sub_y = 30.0 + app.get_primary_subtitle_y() + 10.0;
        app.window().dispatch_event(WindowEvent::PointerMoved {
            position: slint::LogicalPosition::new(470., sub_y),
        });
        assert!(
            hover_state.get(),
            "Hovering primary subtitle must activate hover-pause"
        );

        let popup_y = 30.0 + app.get_popup_y() + 40.0;
        let popup_x = app.get_popup_x() + 40.0;
        app.window().dispatch_event(WindowEvent::PointerMoved {
            position: slint::LogicalPosition::new(popup_x, popup_y),
        });
        assert!(
            hover_state.get(),
            "Hovering YomitanPopup must keep hover-pause active"
        );

        app.window().dispatch_event(WindowEvent::PointerMoved {
            position: slint::LogicalPosition::new(30., 12.),
        });
        assert!(
            !hover_state.get(),
            "Moving pointer from YomitanPopup onto menu bar / OSC / sidebar must release hover-pause"
        );
    }

    #[test]
    fn subtitle_drawer_m1_secondary_selection_copy_lookup_unicode_whitespace_and_h37_delay_seek() {
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        let subtitles = Rc::new(RefCell::new(SubtitleController::new()));
        subtitles.borrow_mut().load_cues(vec![
            create_cue("世界\u{3000}旅行", 10.0, 12.0),
            create_cue("別の字幕", 20.0, 22.0),
        ]);
        subtitles.borrow_mut().ensure_track(2);
        subtitles.borrow_mut().set_track_cues(
            2,
            vec![
                create_cue("Secondary one", 10.0, 12.0),
                create_cue("Secondary two", 20.0, 22.0),
                create_cue("Secondary three", 30.0, 32.0),
            ],
        );
        subtitles.borrow_mut().set_secondary_sid(2);
        app.set_secondary_sid(2);

        let primary_rows = Rc::new(VecModel::<SubEntry>::default());
        let secondary_rows = Rc::new(VecModel::<SubEntry>::default());
        app.set_subtitle_entries(primary_rows.clone().into());
        app.set_secondary_subtitle_entries(secondary_rows.clone().into());

        // H3.7: Apply sub-delay (+2.5s) and secondary-sub-delay (-1.5s) to timestamps & auto-seek.
        refresh_subtitle_list_with_timing(&app, &primary_rows, &subtitles.borrow(), 1.0, 2.5);
        refresh_secondary_subtitle_list_with_timing(
            &app,
            &secondary_rows,
            &subtitles.borrow(),
            1.0,
            -1.5,
        );
        assert!(app.get_has_secondary_subtitles());
        assert!((primary_rows.row_data(0).unwrap().start - 12.5).abs() < 1e-3);
        assert!((secondary_rows.row_data(0).unwrap().start - 8.5).abs() < 1e-3);

        install_subtitle_selection(&app, subtitles.clone(), primary_rows.clone());

        let sought = Rc::new(Cell::new(-1.0f32));
        let seek_sink = sought.clone();
        app.on_seek_to(move |sec| seek_sink.set(sec));
        app.set_subtitle_search_auto_seek(true);
        app.set_subtitle_search_ignore_whitespace(true);
        // M1: Ignore Whitespace matches across fullwidth Unicode ideographic space U+3000.
        app.invoke_search_subtitles("世界旅行".into());
        assert_eq!(app.get_subtitle_search_total_matches(), 1);
        assert!(
            (sought.get() - 12.5).abs() < 1e-3,
            "primary search auto-seek must include sub-delay (got {})",
            sought.get()
        );

        // M1: Secondary tab selection (ClearAndSelect, Ctrl+Click, Shift+Click) & Ctrl+Shift+C copy.
        app.set_subtitle_drawer_tab(1);
        app.invoke_subtitle_row_clicked_mod(1, 0, false, false);
        assert!(secondary_rows.row_data(0).unwrap().selected);
        assert!(!secondary_rows.row_data(1).unwrap().selected);
        app.invoke_subtitle_row_clicked_mod(1, 2, false, true);
        assert!(secondary_rows.row_data(0).unwrap().selected);
        assert!(secondary_rows.row_data(1).unwrap().selected);
        assert!(secondary_rows.row_data(2).unwrap().selected);
        app.invoke_subtitle_row_clicked_mod(1, 1, true, false);
        assert!(!secondary_rows.row_data(1).unwrap().selected);
        app.invoke_copy_selected_subtitles();
        assert_eq!(
            app.get_copied_subtitle_text().as_str(),
            "Secondary one\nSecondary three"
        );

        // Secondary search auto-seek uses secondary-sub-delay.
        app.invoke_search_subtitles("Secondary one".into());
        assert!(
            (sought.get() - 8.5).abs() < 1e-3,
            "secondary search auto-seek must include secondary-sub-delay (got {})",
            sought.get()
        );

        // M1: subtitle_row_lookup triggers lookup at char offset and sets row as sentence/timing context.
        let looked_up = Rc::new(RefCell::new(String::new()));
        let lookup_sink = looked_up.clone();
        app.on_lookup_word(move |q| *lookup_sink.borrow_mut() = q.to_string());
        app.invoke_subtitle_row_lookup(0, 1, 2, 100.0, 80.0);
        assert_eq!(looked_up.borrow().as_str(), "字幕");
        assert_eq!(app.get_active_sub_text().as_str(), "別の字幕");
        assert_eq!(subtitles.borrow().selected_indices(), &[1]);
    }
}
