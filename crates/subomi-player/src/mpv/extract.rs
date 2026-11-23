//! Single-pass extraction of structured `mpv_node` properties (`track-list`,
//! `sub-lines`, `chapter-list`, `osd-dimensions`) directly into owned Rust types.

use crate::events::Track;

use super::node::MpvNodeRef;
use super::types::{AudioTrack, OsdDimensions, SubLine, SubtitleTrack, TrackType, VideoTrack};

pub(super) struct ExtractedTracks {
    pub(super) audio: Vec<AudioTrack>,
    pub(super) video: Vec<VideoTrack>,
    pub(super) subtitle: Vec<SubtitleTrack>,
    pub(super) all: Vec<Track>,
}

pub(super) fn extract_all_tracks_from_node(node: Option<MpvNodeRef<'_>>) -> ExtractedTracks {
    let Some(array_iter) = node.and_then(MpvNodeRef::as_array) else {
        return ExtractedTracks {
            audio: Vec::new(),
            video: Vec::new(),
            subtitle: Vec::new(),
            all: Vec::new(),
        };
    };

    let mut audio = Vec::new();
    let mut video = Vec::new();
    let mut subtitle = Vec::new();
    let mut all = Vec::new();

    for entry in array_iter {
        let Some(map_iter) = entry.as_map() else {
            continue;
        };

        let mut track = Track::default();
        for (key, val) in map_iter {
            match key {
                b"id" => {
                    if let Some(id) = val.as_i64() {
                        track.id = id;
                    }
                }
                b"type" => {
                    if let Some(type_bytes) = val.as_str_bytes() {
                        track.track_type = TrackType::from_bytes(type_bytes);
                    }
                }
                b"src-id" => {
                    if let Some(id) = val.as_i64() {
                        track.src_id = id;
                    }
                }
                b"title" => {
                    if let Some(title) = val.as_string() {
                        track.title = title;
                    }
                }
                b"lang" => {
                    if let Some(lang) = val.as_string() {
                        track.language = lang;
                    }
                }
                b"albumart" => {
                    if let Some(flag) = val.as_bool() {
                        track.album_art = flag;
                    }
                }
                b"default" => {
                    if let Some(flag) = val.as_bool() {
                        track.default_track = flag;
                    }
                }
                b"selected" => {
                    if let Some(flag) = val.as_bool() {
                        track.selected = flag;
                    }
                }
                b"main-selection" => {
                    if let Some(sel) = val.as_i64() {
                        track.main_selection = sel;
                    }
                }
                b"external" => {
                    if let Some(flag) = val.as_bool() {
                        track.external = flag;
                    }
                }
                b"external-filename" => {
                    if let Some(path) = val.as_string() {
                        track.external_filename = path;
                    }
                }
                b"codec" => {
                    if let Some(codec) = val.as_string() {
                        track.codec = codec;
                    }
                }
                _ => {}
            }
        }

        match track.track_type {
            TrackType::Audio => {
                audio.push(AudioTrack {
                    id: track.id,
                    language: track.language.clone(),
                    title: track.title.clone(),
                    external_filename: track.external_filename.clone(),
                    external: track.external,
                    default_track: track.default_track,
                    selected: track.selected,
                });
            }
            TrackType::Video => {
                video.push(VideoTrack {
                    id: track.id,
                    title: track.title.clone(),
                    language: track.language.clone(),
                    codec: track.codec.clone(),
                    external: track.external,
                    default_track: track.default_track,
                    selected: track.selected,
                });
            }
            TrackType::Subtitle => {
                subtitle.push(SubtitleTrack {
                    id: track.id,
                    src_id: track.src_id,
                    codec: track.codec.clone(),
                    language: track.language.clone(),
                    title: track.title.clone(),
                    external_path: track.external_filename.clone(),
                    external: track.external,
                    default_track: track.default_track,
                    selected: track.selected,
                });
            }
            _ => {}
        }
        all.push(track);
    }

    ExtractedTracks {
        audio,
        video,
        subtitle,
        all,
    }
}

#[cfg(test)]
pub(super) fn extract_tracks_from_node(
    node: Option<MpvNodeRef<'_>>,
) -> (Vec<AudioTrack>, Vec<SubtitleTrack>, Vec<Track>) {
    let extracted = extract_all_tracks_from_node(node);
    (extracted.audio, extracted.subtitle, extracted.all)
}

pub(super) fn extract_string_list_from_node(node: Option<MpvNodeRef<'_>>) -> Vec<String> {
    let Some(array_iter) = node.and_then(MpvNodeRef::as_array) else {
        return Vec::new();
    };
    let mut items = Vec::new();
    for entry in array_iter {
        if let Some(value) = entry.as_string() {
            if !value.is_empty() {
                items.push(value);
            }
        }
    }
    items
}

pub(super) fn extract_sub_lines_from_node(node: Option<MpvNodeRef<'_>>) -> Vec<SubLine> {
    let Some(array_iter) = node.and_then(MpvNodeRef::as_array) else {
        return Vec::new();
    };

    let mut lines = Vec::new();
    for entry in array_iter {
        let Some(map_iter) = entry.as_map() else {
            continue;
        };

        let mut id: Option<i64> = None;
        let mut text: Option<String> = None;
        let mut start: Option<f64> = None;
        let mut end: Option<f64> = None;

        for (key, val) in map_iter {
            match key {
                b"id" => {
                    if let Some(v) = val.as_i64() {
                        id = Some(v);
                    }
                }
                b"text" => {
                    if let Some(v) = val.as_string() {
                        text = Some(v);
                    }
                }
                b"start" => {
                    if let Some(v) = val.as_f64().filter(|v| v.is_finite()) {
                        start = Some(v);
                    }
                }
                b"end" => {
                    if let Some(v) = val.as_f64().filter(|v| v.is_finite()) {
                        end = Some(v);
                    }
                }
                _ => {}
            }
        }

        if let (Some(id), Some(text), Some(start), Some(end)) = (id, text, start, end) {
            lines.push(SubLine {
                id,
                text,
                start,
                end,
            });
        }
    }

    lines
}

pub(super) fn extract_chapters_from_node(node: Option<MpvNodeRef<'_>>) -> Vec<f64> {
    let Some(array_iter) = node.and_then(MpvNodeRef::as_array) else {
        return Vec::new();
    };

    let mut chapters = Vec::new();
    for entry in array_iter {
        let Some(map_iter) = entry.as_map() else {
            continue;
        };
        for (key, val) in map_iter {
            if key == b"time" {
                if let Some(t) = val.as_f64() {
                    chapters.push(t);
                }
            }
        }
    }
    chapters
}

pub(super) fn extract_osd_dimensions_from_node(node: Option<MpvNodeRef<'_>>) -> OsdDimensions {
    let mut dims = OsdDimensions::default();
    let Some(map_iter) = node.and_then(MpvNodeRef::as_map) else {
        return dims;
    };

    for (key, val) in map_iter {
        let as_i64 = || {
            val.as_i64().or_else(|| {
                val.as_f64()
                    .filter(|d| d.is_finite())
                    .map(|d| d.round() as i64)
            })
        };
        let as_f64 = || val.as_f64().or_else(|| val.as_i64().map(|v| v as f64));
        match key {
            b"w" => {
                if let Some(v) = as_i64() {
                    dims.w = u32::try_from(v).unwrap_or(0);
                }
            }
            b"h" => {
                if let Some(v) = as_i64() {
                    dims.h = u32::try_from(v).unwrap_or(0);
                }
            }
            b"mt" => {
                if let Some(v) = as_i64() {
                    dims.mt = v as i32;
                }
            }
            b"mb" => {
                if let Some(v) = as_i64() {
                    dims.mb = v as i32;
                }
            }
            b"ml" => {
                if let Some(v) = as_i64() {
                    dims.ml = v as i32;
                }
            }
            b"mr" => {
                if let Some(v) = as_i64() {
                    dims.mr = v as i32;
                }
            }
            b"par" => {
                if let Some(v) = as_f64() {
                    if v.is_finite() && v > 0.0 {
                        dims.par = v;
                    }
                }
            }
            _ => {}
        }
    }
    dims
}
