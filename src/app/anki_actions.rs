//! Anki note creation, note updating, Anki browser navigation,
//! popup pronunciation audio source resolution, and word audio playback.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use slint::{ComponentHandle, Model as _, VecModel};

use crate::controller::anki::AnkiController;
use crate::controller::config::{AnkiNoteConfig, AudioSourceEntry, ConfigController};
use crate::controller::dict::{TermFrequency, TermPitchGraph};
use crate::controller::player::filter_subtitle_text;
use crate::controller::subtitles::SubtitleController;
use crate::controller::{anki, anki_media, audio, media};
use crate::ui::{
    self, popup_pitch_graphs, DictResult, MementoApp, PopupAudioSource, PopupBridge, PopupFrequency,
};
use memento_player::MpvInstance;

#[derive(Default)]
pub(crate) struct PopupAudioState {
    pub(crate) serial: u64,
    pub(crate) expression: String,
    pub(crate) reading: String,
    pub(crate) sources: Vec<AudioSourceEntry>,
}

pub(crate) fn set_popup_term_metadata(
    app: &MementoApp,
    expression: &str,
    reading: &str,
    graphs: Vec<TermPitchGraph>,
    frequencies: Vec<TermFrequency>,
) {
    let rows = app.get_dict_results();
    if let Some(index) = (0..rows.row_count()).find(|&index| {
        rows.row_data(index).is_some_and(|row| {
            row.term_start
                && row.expression.as_str() == expression
                && row.reading.as_str() == reading
        })
    }) {
        if let Some(mut row) = rows.row_data(index) {
            row.pitch_graphs = Rc::new(VecModel::from(popup_pitch_graphs(graphs))).into();
            row.frequencies = Rc::new(VecModel::from(
                frequencies
                    .into_iter()
                    .map(|entry| PopupFrequency {
                        dictionary: entry.dictionary.into(),
                        value: entry.value.into(),
                    })
                    .collect::<Vec<_>>(),
            ))
            .into();
            rows.set_row_data(index, row.clone());
            let popup = app.get_popup_results();
            if let Some(popup_index) = (0..popup.row_count()).find(|&popup_index| {
                popup
                    .row_data(popup_index)
                    .is_some_and(|visible| visible.index == row.index)
            }) {
                popup.set_row_data(popup_index, row);
            }
        }
    }
}

pub(crate) fn set_popup_audio_sources(
    app: &MementoApp,
    cache: &Arc<Mutex<PopupAudioState>>,
    expression: &str,
    reading: &str,
    entries: Vec<AudioSourceEntry>,
) {
    let sources: Vec<PopupAudioSource> = entries
        .iter()
        .enumerate()
        .map(|(index, source)| PopupAudioSource {
            name: source.name.clone().into(),
            index: index as i32,
        })
        .collect();
    {
        let mut state = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *state = PopupAudioState {
            serial: state.serial.wrapping_add(1),
            expression: expression.to_owned(),
            reading: reading.to_owned(),
            sources: entries,
        };
    }
    app.set_popup_audio_sources(Rc::new(VecModel::from(sources)).into());
    app.set_popup_selected_audio_source(-1);
    app.set_popup_audio_sources_loading(false);
}

fn maybe_auto_play_first_term_audio(app: &MementoApp, expression: &str, reading: &str) {
    if app.get_auto_play_audio_enabled() && !expression.is_empty() {
        app.invoke_play_audio_for_word(expression.into(), reading.into());
    }
}

pub(crate) fn install_auto_play_first_term_audio(app: &MementoApp) {
    let auto_audio_app = app.as_weak();
    app.global::<PopupBridge>()
        .on_first_term_ready(move |expression, reading| {
            if let Some(app) = auto_audio_app.upgrade() {
                maybe_auto_play_first_term_audio(&app, expression.as_str(), reading.as_str());
            }
        });
}

pub(crate) struct PreparedAnkiNoteRequest {
    pub(crate) profile: AnkiNoteConfig,
    pub(crate) context: anki::NoteFieldsContext,
    pub(crate) selected_occurrences: Vec<(String, usize)>,
    pub(crate) screenshot_request: Option<Result<memento_player::mpv::ScreenshotRequest, String>>,
    pub(crate) video_screenshot_request:
        Option<Result<memento_player::mpv::ScreenshotRequest, String>>,
    pub(crate) media_input: anki_media::NoteMediaInput,
    pub(crate) unavailable: Vec<String>,
    pub(crate) show_tag: bool,
}

pub(crate) fn prepare_anki_note_request(
    app: &MementoApp,
    definitions: &slint::ModelRc<DictResult>,
    row_idx: usize,
    config: &ConfigController,
    audio_cache: &Arc<Mutex<PopupAudioState>>,
    subtitles: &RefCell<SubtitleController>,
    mpv: &RefCell<Option<MpvInstance>>,
) -> Result<PreparedAnkiNoteRequest, Option<String>> {
    let Some(hit) = definitions.row_data(row_idx) else {
        return Err(None);
    };
    let profile = if hit.is_kanji {
        config.anki_kanji_note_config()
    } else {
        config.anki_profile_note_config()
    };
    let Some(profile) = profile else {
        return Err(Some(
            "Select an Anki profile and configure this note type".to_owned(),
        ));
    };
    if !profile.enabled {
        return Err(Some("Enable Anki integration in Preferences".to_owned()));
    }
    let (remove_regex, replace_lines, replace_with) = (
        app.get_search_remove_regex().to_string(),
        app.get_search_replace_lines(),
        app.get_search_replace_with().to_string(),
    );
    let ass_lookup = ui::active_ass_lookup_for_app(app);
    let base_subtitle = if let Some(ref ass_hit) = ass_lookup {
        filter_subtitle_text(
            &ass_hit.event_plain_text,
            &remove_regex,
            replace_lines,
            &replace_with,
        )
    } else {
        app.get_active_sub_text().to_string()
    };
    let mut context = anki::NoteFieldsContext::new(
        hit.expression.to_string(),
        hit.reading.to_string(),
        base_subtitle,
    );
    context.is_kanji = hit.is_kanji;
    let selected_glossary = app.global::<PopupBridge>().get_selected_glossary_text();
    if !selected_glossary.is_empty() {
        context.selection = selected_glossary.to_string();
    }
    // Never assign the scanned subtitle token into `context.selection` (#262);
    // use it purely to locate `cloze_prefix` / `cloze_body` / `cloze_suffix`.
    let (scanned_surface, token_offset_in_sentence) = if let Some(ref ass_hit) = ass_lookup {
        let off = if remove_regex.is_empty() && !replace_lines {
            Some(ass_hit.byte_offset_in_event)
        } else {
            None
        };
        (ass_hit.scanned_surface.clone(), off)
    } else {
        let start = app.get_selected_token_index();
        if start >= 0 {
            let words = app.get_active_sub_words();
            let start_idx = start as usize;
            let count = app.get_selected_token_count().max(1) as usize;
            let byte_offset: usize = (0..start_idx)
                .filter_map(|i| words.row_data(i))
                .map(|word| word.text.len())
                .sum();
            let surface: String = (start_idx..start_idx.saturating_add(count))
                .filter_map(|i| words.row_data(i))
                .map(|word| word.text.to_string())
                .collect();
            (surface, Some(byte_offset))
        } else {
            (String::new(), None)
        }
    };
    let (
        active_start,
        active_end,
        context_start,
        context_end,
        selected_cues,
        active_pos_in_selected,
    ) = {
        let subs = subtitles.borrow();
        let selected_indices = subs.selected_indices();
        let active_indices = subs.active_indices(subs.primary_sid());
        let active_pos = selected_indices
            .iter()
            .position(|idx| active_indices.contains(idx));
        let cues = subs
            .selected_cues()
            .iter()
            .map(|cue| {
                let raw = String::from_utf16_lossy(&cue.text);
                filter_subtitle_text(&raw, &remove_regex, replace_lines, &replace_with)
            })
            .collect::<Vec<_>>();
        let active = subs.active_cue();
        let (start, end) = if let Some(ref ass_hit) = ass_lookup {
            if ass_hit.event_end_s > ass_hit.event_start_s {
                (ass_hit.event_start_s, ass_hit.event_end_s)
            } else {
                (
                    active.map(|cue| cue.start).unwrap_or(0.0),
                    active.map(|cue| cue.end).unwrap_or(0.0),
                )
            }
        } else {
            (
                active.map(|cue| cue.start).unwrap_or(0.0),
                active.map(|cue| cue.end).unwrap_or(0.0),
            )
        };
        let (ctx_start, ctx_end) = subs.selected_time_range().unwrap_or((start, end));
        (start, end, ctx_start, ctx_end, cues, active_pos)
    };
    anki::apply_subtitle_cloze(
        &mut context,
        &selected_cues,
        active_pos_in_selected,
        &scanned_surface,
        token_offset_in_sentence,
    );
    context.secondary_subtitle = app.get_secondary_sub_text().to_string();
    context.title = app.get_media_title().to_string();
    context.dictionary_name = hit.dict_name.to_string();
    let mut occ_by_dict: HashMap<String, usize> = HashMap::new();
    let mut selected = Vec::new();
    let mut selected_occurrences = Vec::new();
    for row in (0..definitions.row_count()).filter_map(|r| definitions.row_data(r)) {
        if row.expression == hit.expression
            && row.reading == hit.reading
            && row.is_kanji == hit.is_kanji
        {
            let dict_name = row.dict_name.to_string();
            let occ = *occ_by_dict
                .entry(dict_name.clone())
                .and_modify(|c| *c += 1)
                .or_insert(0);
            if row.selected {
                selected_occurrences.push((dict_name, occ));
                selected.push(row);
            }
        }
    }
    if selected.is_empty() {
        return Err(Some(
            "Select at least one dictionary entry for this term".to_owned(),
        ));
    }
    context.glossary_definitions = selected
        .iter()
        .map(|row| anki::GlossaryDefinition {
            dictionary_id: 0,
            dictionary_name: row.dict_name.to_string(),
            glossary: row.glossary.to_string(),
            tags: row
                .pos_tags
                .split(|character: char| character == ',' || character.is_whitespace())
                .filter(|tag| !tag.is_empty())
                .map(str::to_owned)
                .collect(),
            notes: row.notes.to_string(),
            example_ja: row.example_ja.to_string(),
            example_en: row.example_en.to_string(),
            ..Default::default()
        })
        .collect();
    context.glossary_items = selected
        .iter()
        .map(|row| row.glossary.to_string())
        .collect();
    context.tags = selected
        .iter()
        .flat_map(|row| {
            row.pos_tags
                .split(|character: char| character == ',' || character.is_whitespace())
        })
        .filter(|tag| !tag.is_empty())
        .map(str::to_owned)
        .collect();
    context.tags.sort();
    context.tags.dedup();
    if hit.is_kanji {
        context.character = hit.expression.to_string();
        for component in hit.reading.split(';') {
            let component = component.trim();
            if let Some(on) = component.strip_prefix("On:") {
                context.onyomi = on.trim().to_owned();
            }
            if let Some(kun) = component.strip_prefix("Kun:") {
                context.kunyomi = kun.trim().to_owned();
            }
        }
        context.stroke_count = hit
            .notes
            .split(';')
            .filter_map(|part| {
                let (label, value) = part.split_once(':')?;
                (label.to_ascii_lowercase().contains("stroke") || label.contains("画数"))
                    .then(|| value.trim().to_owned())
            })
            .next()
            .unwrap_or_default();
    }
    let (wants_screenshot, wants_video_screenshot) = anki_media::requested_screenshots(&profile);
    let mut screenshot_request = None;
    let mut video_screenshot_request = None;
    let media_input = {
        let player_ref = mpv.borrow();
        let player = player_ref.as_ref();
        let media_ctx = player
            .and_then(MpvInstance::capture_encode_context)
            .map(|ctx| media::MediaContext::new(ctx.path, ctx.audio_id));
        let mpv_start = player.map(MpvInstance::get_sub_start).unwrap_or(0.0);
        let mpv_end = player.map(MpvInstance::get_sub_end).unwrap_or(0.0);
        let subtitle_start = if active_end > active_start {
            active_start
        } else {
            mpv_start
        };
        let subtitle_end = if active_end > active_start {
            active_end
        } else {
            mpv_end
        };
        for (wanted, with_subs, request) in [
            (wants_screenshot, true, &mut screenshot_request),
            (wants_video_screenshot, false, &mut video_screenshot_request),
        ] {
            if wanted {
                *request = Some(
                    player
                        .ok_or_else(|| "No video is loaded".to_owned())
                        .and_then(|player| player.request_screenshot(with_subs)),
                );
            }
        }
        anki_media::NoteMediaInput {
            media: media_ctx,
            subtitle_start,
            subtitle_end,
            context_start: if context_end > context_start {
                context_start
            } else {
                subtitle_start
            },
            context_end: if context_end > context_start {
                context_end
            } else {
                subtitle_end
            },
            screenshot: None,
            screenshot_video: None,
            subtitle_id: player.map(MpvInstance::primary_sid).unwrap_or(0),
            audio_sources: {
                let state = audio_cache
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let matching = state.expression == hit.expression.as_str()
                    && state.reading == hit.reading.as_str();
                let mut sources = if matching {
                    state.sources.clone()
                } else {
                    Vec::new()
                };
                drop(state);
                if sources.is_empty() {
                    sources = config.audio_source_entries();
                }
                let selected_src = if matching {
                    app.get_popup_selected_audio_source()
                } else {
                    -1
                };
                if selected_src >= 0 {
                    sources = sources
                        .into_iter()
                        .nth(selected_src as usize)
                        .into_iter()
                        .collect();
                }
                sources
            },
        }
    };
    context.secondary_context = anki::secondary_context_for_range(
        subtitles.borrow().secondary_entries(),
        media_input.context_start,
        media_input.context_end,
        &context.secondary_subtitle,
    );
    let unavailable = anki::unavailable_profile_markers(&profile);
    let show_tag = config.anki_show_tag();
    Ok(PreparedAnkiNoteRequest {
        profile,
        context,
        selected_occurrences,
        screenshot_request,
        video_screenshot_request,
        media_input,
        unavailable,
        show_tag,
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn install_update_anki_note_handler(
    app: &MementoApp,
    config: Rc<ConfigController>,
    audio_cache: Arc<Mutex<PopupAudioState>>,
    subtitles: Rc<RefCell<SubtitleController>>,
    mpv: Rc<RefCell<Option<MpvInstance>>>,
    database_path: PathBuf,
    resources_path: PathBuf,
    config_dir: PathBuf,
    lookup_epoch: Arc<AtomicU64>,
) {
    let update_app = app.as_weak();
    app.global::<PopupBridge>().on_update_anki_note_requested(
        move |row_idx, raw_note_id, _expression, _reading, _glossary_summary| {
            let Some(app) = update_app.upgrade() else {
                return;
            };
            if row_idx < 0 {
                return;
            }
            let row_usize = row_idx as usize;
            let Some(note_id) = ui::popup_row_anki_note_id_i64(&app, row_usize)
                .or_else(|| (raw_note_id > 0).then_some(i64::from(raw_note_id)))
            else {
                app.set_status_message("Could not resolve Anki note ID to update".into());
                return;
            };
            let pb = app.global::<PopupBridge>();
            let nested_depth = pb.get_nested_depth();
            let definitions = if nested_depth > 0 {
                pb.get_nested_results()
            } else {
                app.get_dict_results()
            };
            let prepared = match prepare_anki_note_request(
                &app,
                &definitions,
                row_usize,
                &config,
                &audio_cache,
                &subtitles,
                &mpv,
            ) {
                Ok(prepared) => prepared,
                Err(Some(msg)) => {
                    app.set_status_message(msg.into());
                    return;
                }
                Err(None) => return,
            };
            let PreparedAnkiNoteRequest {
                profile,
                mut context,
                selected_occurrences,
                screenshot_request,
                video_screenshot_request,
                mut media_input,
                unavailable,
                show_tag,
            } = prepared;
            app.set_status_message(format!("Updating Anki note #{note_id}…").into());
            let response_app = update_app.clone();
            let response_epoch = lookup_epoch.clone();
            let request_epoch = response_epoch.load(Ordering::Acquire);
            let metadata_path = database_path.clone();
            let res_dir = resources_path.clone();
            let cfg_dir = config_dir.clone();
            std::thread::spawn(move || {
                media_input.screenshot = screenshot_request
                    .and_then(|request| request.ok())
                    .and_then(|request| request.wait().ok());
                media_input.screenshot_video = video_screenshot_request
                    .and_then(|request| request.ok())
                    .and_then(|request| request.wait().ok());
                let controller = AnkiController::new(profile.endpoint());
                let resource_roots = vec![cfg_dir.join("dict_resources"), res_dir];
                let _ = anki::populate_note_context_from_sqlite(
                    &mut context,
                    &metadata_path,
                    &resource_roots,
                    &selected_occurrences,
                );
                let result = controller.update_existing_note_with_media(
                    note_id,
                    &profile,
                    &mut context,
                    media_input,
                    anki::AnkiUpdateMode::OverwriteNonEmpty,
                );
                let updated_profile = profile.clone();
                let _ = response_app.upgrade_in_event_loop(move |app| {
                    if app.get_dict_open()
                        && response_epoch.load(Ordering::Acquire) == request_epoch
                    {
                        if let Ok(updated_id) = &result {
                            ui::mark_popup_term_anki_updated(
                                &app,
                                row_usize,
                                nested_depth,
                                *updated_id,
                                &updated_profile,
                                show_tag,
                            );
                        }
                    }
                    app.set_status_message(match result {
                        Ok(updated_id) if unavailable.is_empty() => {
                            format!("Updated Anki note #{updated_id}").into()
                        }
                        Ok(updated_id) => {
                            eprintln!(
                                "Anki note #{updated_id}: unavailable profile markers left empty: {}",
                                unavailable.join(", ")
                            );
                            format!("Updated Anki note #{updated_id}; some fields are empty").into()
                        }
                        Err(error) => {
                            eprintln!("Could not update Anki note #{note_id}: {error}");
                            format!("Could not update Anki note #{note_id}: {error}").into()
                        }
                    });
                });
            });
        },
    );
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn install_anki_and_audio_handlers(
    app: &MementoApp,
    config_ctrl: Rc<ConfigController>,
    popup_audio_cache: Arc<Mutex<PopupAudioState>>,
    sub_ctrl: Rc<RefCell<SubtitleController>>,
    mpv: Rc<RefCell<Option<MpvInstance>>>,
    dictionary_database_path: PathBuf,
    dictionary_resources_path: PathBuf,
    config_dir: PathBuf,
    lookup_epoch: Arc<AtomicU64>,
) {
    let audio_player: Rc<RefCell<Option<audio::AudioController>>> = Rc::new(RefCell::new(None));

    let source_menu_app = app.as_weak();
    let source_menu_config = config_ctrl.clone();
    let source_menu_cache = popup_audio_cache.clone();
    app.on_popup_audio_sources_requested(move |expression, reading| {
        let Some(app) = source_menu_app.upgrade() else {
            return;
        };
        let expression = expression.to_string();
        let reading = reading.to_string();
        let already_resolved = {
            let state = source_menu_cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.expression == expression
                && state.reading == reading
                && (!state.sources.is_empty() || app.get_popup_audio_sources_loading())
        };
        if already_resolved {
            return;
        }
        let sources = source_menu_config.audio_source_entries();
        set_popup_audio_sources(&app, &source_menu_cache, &expression, &reading, Vec::new());
        app.set_popup_audio_sources_loading(true);
        let serial = source_menu_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .serial;
        let weak = source_menu_app.clone();
        let cache = source_menu_cache.clone();
        std::thread::spawn(move || {
            let files = audio::enumerate_word_audio_sources(&expression, &reading, &sources);
            let _ = weak.upgrade_in_event_loop(move |app| {
                let still_current = {
                    let state = cache
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    state.serial == serial
                        && state.expression == expression
                        && state.reading == reading
                };
                if still_current {
                    set_popup_audio_sources(&app, &cache, &expression, &reading, files);
                }
            });
        });
    });

    let audio_app = app.as_weak();
    let audio_config = config_ctrl.clone();
    let playback_audio_cache = popup_audio_cache.clone();
    app.on_play_audio_for_word(move |expression, reading| {
        let Some(app) = audio_app.upgrade() else {
            return;
        };
        let state = playback_audio_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let matching = state.expression == expression.as_str() && state.reading == reading.as_str();
        let mut sources = if matching {
            state.sources.clone()
        } else {
            Vec::new()
        };
        drop(state);
        if sources.is_empty() {
            sources = audio_config.audio_source_entries();
        }
        let selected_source = if matching {
            app.get_popup_selected_audio_source()
        } else {
            -1
        };
        if selected_source >= 0 {
            sources = sources
                .into_iter()
                .nth(selected_source as usize)
                .into_iter()
                .collect();
        }
        if sources.is_empty() {
            app.set_status_message("No pronunciation audio source is configured".into());
            return;
        }
        let mut player = audio_player.borrow_mut();
        if player.is_none() {
            match audio::AudioController::new() {
                Ok(controller) => *player = Some(controller),
                Err(error) => {
                    app.set_status_message(
                        format!("Pronunciation audio is unavailable: {error}").into(),
                    );
                    return;
                }
            }
        }
        let weak = app.as_weak();
        let result = player.as_ref().unwrap().play_word(
            expression.to_string(),
            reading.to_string(),
            sources,
            move |result| {
                if let Err(error) = result {
                    let _ = weak.upgrade_in_event_loop(move |app| {
                        app.set_status_message(
                            format!("Could not play pronunciation: {error}").into(),
                        );
                    });
                }
            },
        );
        if let Err(error) = result {
            app.set_status_message(format!("Could not start pronunciation: {error}").into());
        }
    });

    install_auto_play_first_term_audio(app);
    install_update_anki_note_handler(
        app,
        config_ctrl.clone(),
        popup_audio_cache.clone(),
        sub_ctrl.clone(),
        mpv.clone(),
        dictionary_database_path.clone(),
        dictionary_resources_path.clone(),
        config_dir.clone(),
        lookup_epoch.clone(),
    );

    let note_app = app.as_weak();
    let note_config = config_ctrl.clone();
    let note_audio_cache = popup_audio_cache;
    let note_subtitles = sub_ctrl;
    let note_mpv = mpv;
    let note_database_path = dictionary_database_path;
    let note_resources_path = dictionary_resources_path;
    let note_config_dir = config_dir;
    let note_lookup_epoch = lookup_epoch;
    app.on_add_anki_note(move |index| {
        let Some(app) = note_app.upgrade() else {
            return;
        };
        if index < 0 {
            return;
        }
        app.set_anki_added_note_id("".into());
        app.set_anki_added_result_index(-1);
        let definitions = app.get_dict_results();
        let prepared = match prepare_anki_note_request(
            &app,
            &definitions,
            index as usize,
            &note_config,
            &note_audio_cache,
            &note_subtitles,
            &note_mpv,
        ) {
            Ok(prepared) => prepared,
            Err(Some(msg)) => {
                app.set_status_message(msg.into());
                return;
            }
            Err(None) => return,
        };
        let PreparedAnkiNoteRequest {
            profile,
            mut context,
            selected_occurrences,
            screenshot_request,
            video_screenshot_request,
            mut media_input,
            unavailable,
            show_tag,
        } = prepared;
        app.set_status_message("Adding note to Anki…".into());
        let response_app = note_app.clone();
        let response_epoch = note_lookup_epoch.clone();
        let request_epoch = response_epoch.load(Ordering::Acquire);
        let metadata_path = note_database_path.clone();
        let res_dir = note_resources_path.clone();
        let cfg_dir = note_config_dir.clone();
        std::thread::spawn(move || {
            media_input.screenshot = screenshot_request
                .and_then(|request| request.ok())
                .and_then(|request| request.wait().ok());
            media_input.screenshot_video = video_screenshot_request
                .and_then(|request| request.ok())
                .and_then(|request| request.wait().ok());
            let controller = AnkiController::new(profile.endpoint());
            let resource_roots = vec![cfg_dir.join("dict_resources"), res_dir];
            let _ = anki::populate_note_context_from_sqlite(
                &mut context,
                &metadata_path,
                &resource_roots,
                &selected_occurrences,
            );
            let result = anki_media::prepare_note_media(&profile, &mut context, media_input)
                .and_then(|()| controller.add_note_from_profile(&profile, &context));
            let added_profile = profile.clone();
            let expire_app = response_app.clone();
            let _ = response_app.upgrade_in_event_loop(move |app| {
                if app.get_dict_open() && response_epoch.load(Ordering::Acquire) == request_epoch {
                    if let Ok(note_id) = &result {
                        ui::mark_popup_term_anki_added(
                            &app,
                            index as usize,
                            *note_id,
                            &added_profile,
                            show_tag,
                        );
                        let note_id = note_id.to_string();
                        app.set_anki_added_note_id(note_id.clone().into());
                        app.set_anki_added_result_index(index);
                        slint::Timer::single_shot(std::time::Duration::from_secs(15), move || {
                            if let Some(app) = expire_app.upgrade() {
                                if app.get_anki_added_note_id() == note_id {
                                    app.set_anki_added_note_id("".into());
                                    app.set_anki_added_result_index(-1);
                                }
                            }
                        });
                    }
                }
                app.set_status_message(match result {
                    Ok(note_id) if unavailable.is_empty() => {
                        format!("Added Anki note {note_id}").into()
                    }
                    Ok(note_id) => {
                        eprintln!(
                            "Anki note {note_id}: unavailable profile markers left empty: {}",
                            unavailable.join(", ")
                        );
                        format!("Added Anki note {note_id}; some fields are empty").into()
                    }
                    Err(error) => {
                        eprintln!("Could not add Anki note: {error}");
                        format!("Could not add Anki note: {error}").into()
                    }
                });
            });
        });
    });

    let browse_app = app.as_weak();
    let browse_config = config_ctrl;
    app.on_browse_anki_note(move |id| {
        let Some(query) = anki::normalize_anki_browse_target(id.as_str()) else {
            return;
        };
        let endpoint = browse_config.anki_endpoint();
        let weak = browse_app.clone();
        std::thread::spawn(move || {
            let result = AnkiController::new(endpoint).client().open_browse(&query);
            if let Err(error) = result {
                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.set_status_message(format!("Could not open Anki browser: {error}").into());
                });
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::preferences::refresh_player_preferences;
    use crate::app::workers::{display_word_lookup, WordLookupResult};
    use crate::controller::dict::SearchHit;
    use crate::ui::{dictionary_result_rows, set_dictionary_rows};

    #[test]
    fn auto_play_audio_triggers_on_first_term_result_when_enabled() {
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        install_auto_play_first_term_audio(&app);
        let cfg = ConfigController::new();
        cfg.edit_setting("search/auto-play-audio", "true").unwrap();
        refresh_player_preferences(&app, &cfg);
        assert!(app.get_auto_play_audio_enabled());

        let played = Rc::new(RefCell::new(Vec::<(String, String)>::new()));
        let played_sink = played.clone();
        app.on_play_audio_for_word(move |expr, reading| {
            played_sink
                .borrow_mut()
                .push((expr.to_string(), reading.to_string()));
        });

        let term = SearchHit::new("食べる", "たべる", "to eat", "v1", 10, false);
        let kanji = SearchHit::new("食", "しょく", "eat", "kanji", 1, true);
        display_word_lookup(
            &app,
            WordLookupResult {
                hits: vec![kanji.clone(), term.clone()],
                error: None,
                database_open: true,
                matched_units: 3,
                headword: "食べる".into(),
                reading: "たべる".into(),
                pitch: String::new(),
                freq: String::new(),
                jpdb: String::new(),
            },
            "食べる",
            &[],
        );
        assert_eq!(
            played.borrow().as_slice(),
            &[("食べる".to_owned(), "たべる".to_owned())]
        );

        played.borrow_mut().clear();
        display_word_lookup(
            &app,
            WordLookupResult {
                hits: vec![kanji],
                error: None,
                database_open: true,
                matched_units: 1,
                headword: "食".into(),
                reading: "しょく".into(),
                pitch: String::new(),
                freq: String::new(),
                jpdb: String::new(),
            },
            "食",
            &[],
        );
        assert!(played.borrow().is_empty());

        played.borrow_mut().clear();
        cfg.edit_setting("search/auto-play-audio", "false").unwrap();
        refresh_player_preferences(&app, &cfg);
        assert!(!app.get_auto_play_audio_enabled());
        display_word_lookup(
            &app,
            WordLookupResult {
                hits: vec![term],
                error: None,
                database_open: true,
                matched_units: 3,
                headword: "食べる".into(),
                reading: "たべる".into(),
                pitch: String::new(),
                freq: String::new(),
                jpdb: String::new(),
            },
            "食べる",
            &[],
        );
        assert!(played.borrow().is_empty());
    }

    #[test]
    fn update_anki_note_requested_resolves_lossless_64bit_id_and_updates_note() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        i_slint_backend_testing::init_no_event_loop();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let target_note_id: i64 = 1_732_012_345_678;

        let server_actions = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let server_actions_bg = server_actions.clone();
        let server = std::thread::spawn(move || {
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(3)));
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                let mut header_end = None;
                let mut content_len = 0usize;
                loop {
                    let n = stream.read(&mut tmp).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if header_end.is_none() {
                        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            let hdr = String::from_utf8_lossy(&buf[..pos]);
                            for line in hdr.lines() {
                                if let Some((k, v)) = line.split_once(':') {
                                    if k.eq_ignore_ascii_case("content-length") {
                                        content_len = v.trim().parse().unwrap_or(0);
                                    }
                                }
                            }
                            header_end = Some(pos + 4);
                        }
                    }
                    if let Some(start) = header_end {
                        if buf.len() >= start + content_len {
                            break;
                        }
                    }
                }
                let start = header_end.unwrap_or(buf.len());
                let body: serde_json::Value =
                    serde_json::from_slice(&buf[start..]).unwrap_or(serde_json::Value::Null);
                let action = body
                    .get("action")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_owned();
                server_actions_bg.lock().unwrap().push(body);
                let reply = match action.as_str() {
                    "notesInfo" => serde_json::json!({
                        "result": [{
                            "noteId": target_note_id,
                            "modelName": "JP Mining",
                            "tags": ["existing-tag"],
                            "fields": {
                                "Expression": { "value": "食べる", "order": 0 },
                                "Sentence": { "value": "", "order": 1 },
                                "Glossary": { "value": "old def", "order": 2 }
                            }
                        }],
                        "error": null
                    }),
                    "updateNoteFields" | "addTags" => serde_json::json!({
                        "result": null,
                        "error": null
                    }),
                    _ => serde_json::json!({ "result": null, "error": "unexpected action" }),
                };
                let payload = reply.to_string();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    payload.len(),
                    payload
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
            }
        });

        let dir = tempfile::TempDir::new().unwrap();
        let cfg = ConfigController::new();
        cfg.edit_setting("anki/enabled", "true").unwrap();
        cfg.edit_setting("anki/profile/host", "127.0.0.1").unwrap();
        cfg.edit_setting("anki/profile/port", &port.to_string())
            .unwrap();
        cfg.edit_setting("anki/profile/term/deck", "Mining")
            .unwrap();
        cfg.edit_setting("anki/profile/term/model", "JP Mining")
            .unwrap();
        cfg.edit_setting(
            "anki/profile/term/fields",
            &serde_json::json!([
                ["Expression", "{expression}"],
                ["Sentence", "{sentence}"],
                ["Glossary", "{glossary}"]
            ])
            .to_string(),
        )
        .unwrap();
        cfg.edit_setting("anki/profile/tags", "memento-updated")
            .unwrap();
        let config_ctrl = Rc::new(cfg);

        let app = MementoApp::new().unwrap();
        app.set_dict_open(true);
        app.set_active_sub_text("今日ご飯を食べる。".into());
        let hit = SearchHit::new("食べる", "たべる", "to eat", "v1", 10, false);
        set_dictionary_rows(&app, dictionary_result_rows(&[hit], &[]));

        let pb = app.global::<PopupBridge>();
        pb.set_row_anki_duplicates(Rc::new(VecModel::from(vec![true])).into());
        pb.set_row_anki_note_ids(Rc::new(VecModel::from(vec![i32::MAX])).into());
        pb.set_row_anki_note_id_strings(
            Rc::new(VecModel::from(vec![slint::SharedString::from(
                target_note_id.to_string(),
            )]))
            .into(),
        );

        let audio_cache = Arc::new(Mutex::new(PopupAudioState::default()));
        let sub_ctrl = Rc::new(RefCell::new(SubtitleController::new()));
        let mpv_slot: Rc<RefCell<Option<MpvInstance>>> = Rc::new(RefCell::new(None));
        let epoch = Arc::new(AtomicU64::new(1));

        install_update_anki_note_handler(
            &app,
            config_ctrl,
            audio_cache,
            sub_ctrl,
            mpv_slot,
            dir.path().join("dictionaries.sqlite"),
            dir.path().join("res"),
            dir.path().to_path_buf(),
            epoch,
        );

        pb.invoke_update_anki_note_requested(
            0,
            i32::MAX,
            "食べる".into(),
            "たべる".into(),
            "to eat".into(),
        );
        assert!(app
            .get_status_message()
            .contains(&target_note_id.to_string()));

        server.join().unwrap();
        let actions = server_actions.lock().unwrap();
        assert_eq!(actions.len(), 3);
        assert_eq!(actions[0]["action"], "notesInfo");
        assert_eq!(actions[0]["params"]["notes"][0], target_note_id);
        assert_eq!(actions[1]["action"], "updateNoteFields");
        assert_eq!(actions[1]["params"]["note"]["id"], target_note_id);
        assert_eq!(
            actions[1]["params"]["note"]["fields"]["Sentence"],
            "今日ご飯を食べる。"
        );
        assert_eq!(actions[2]["action"], "addTags");
        assert_eq!(actions[2]["params"]["notes"][0], target_note_id);
        assert_eq!(actions[2]["params"]["tags"], "memento-updated");
    }
}
