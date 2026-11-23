//! Single-pass extraction of structured `mpv_node` properties (`track-list`,
//! `sub-lines`, `chapter-list`, `osd-dimensions`) directly into owned Rust types.

use std::ffi::CStr;

use crate::events::Track;

use super::ffi;
use super::node::{copy_mpv_string, iter_node_array, iter_node_map};
use super::types::{AudioTrack, OsdDimensions, SubLine, SubtitleTrack, TrackType, VideoTrack};

pub(super) struct ExtractedTracks {
    pub(super) audio: Vec<AudioTrack>,
    pub(super) video: Vec<VideoTrack>,
    pub(super) subtitle: Vec<SubtitleTrack>,
    pub(super) all: Vec<Track>,
}

pub(super) fn extract_all_tracks_from_node(node: *const ffi::mpv_node) -> ExtractedTracks {
    let Some(array_iter) = (unsafe { iter_node_array(node) }) else {
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
        let Some(map_iter) = (unsafe { iter_node_map(entry) }) else {
            continue;
        };

        let mut track = Track::default();
        for (key, val) in map_iter {
            match key {
                b"id" if val.format == ffi::MPV_FORMAT_INT64 => {
                    track.id = unsafe { val.u.int64 };
                }
                b"type" if val.format == ffi::MPV_FORMAT_STRING => {
                    let s = unsafe { val.u.string };
                    if !s.is_null() {
                        let type_bytes = unsafe { CStr::from_ptr(s) }.to_bytes();
                        track.track_type = TrackType::from_bytes(type_bytes);
                    }
                }
                b"src-id" if val.format == ffi::MPV_FORMAT_INT64 => {
                    track.src_id = unsafe { val.u.int64 };
                }
                b"title" if val.format == ffi::MPV_FORMAT_STRING => {
                    track.title = copy_mpv_string(unsafe { val.u.string });
                }
                b"lang" if val.format == ffi::MPV_FORMAT_STRING => {
                    track.language = copy_mpv_string(unsafe { val.u.string });
                }
                b"albumart" if val.format == ffi::MPV_FORMAT_FLAG => {
                    track.album_art = unsafe { val.u.flag != 0 };
                }
                b"default" if val.format == ffi::MPV_FORMAT_FLAG => {
                    track.default_track = unsafe { val.u.flag != 0 };
                }
                b"selected" if val.format == ffi::MPV_FORMAT_FLAG => {
                    track.selected = unsafe { val.u.flag != 0 };
                }
                b"main-selection" if val.format == ffi::MPV_FORMAT_INT64 => {
                    track.main_selection = unsafe { val.u.int64 };
                }
                b"external" if val.format == ffi::MPV_FORMAT_FLAG => {
                    track.external = unsafe { val.u.flag != 0 };
                }
                b"external-filename" if val.format == ffi::MPV_FORMAT_STRING => {
                    track.external_filename = copy_mpv_string(unsafe { val.u.string });
                }
                b"codec" if val.format == ffi::MPV_FORMAT_STRING => {
                    track.codec = copy_mpv_string(unsafe { val.u.string });
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
    node: *const ffi::mpv_node,
) -> (Vec<AudioTrack>, Vec<SubtitleTrack>, Vec<Track>) {
    let extracted = extract_all_tracks_from_node(node);
    (extracted.audio, extracted.subtitle, extracted.all)
}

pub(super) fn extract_string_list_from_node(node: *const ffi::mpv_node) -> Vec<String> {
    let Some(array_iter) = (unsafe { iter_node_array(node) }) else {
        return Vec::new();
    };
    let mut items = Vec::new();
    for entry in array_iter {
        if entry.format == ffi::MPV_FORMAT_STRING {
            let value = copy_mpv_string(unsafe { entry.u.string });
            if !value.is_empty() {
                items.push(value);
            }
        }
    }
    items
}

pub(super) fn extract_sub_lines_from_node(node: *const ffi::mpv_node) -> Vec<SubLine> {
    let Some(array_iter) = (unsafe { iter_node_array(node) }) else {
        return Vec::new();
    };

    let mut lines = Vec::new();
    for entry in array_iter {
        let Some(map_iter) = (unsafe { iter_node_map(entry) }) else {
            continue;
        };

        let mut has_id = false;
        let mut text_ptr = std::ptr::null();
        let mut has_start = false;
        let mut has_end = false;
        let mut id = 0i64;
        let mut start = 0.0f64;
        let mut end = 0.0f64;

        for (key, val) in map_iter {
            match key {
                b"id" if val.format == ffi::MPV_FORMAT_INT64 => {
                    id = unsafe { val.u.int64 };
                    has_id = true;
                }
                b"text" if val.format == ffi::MPV_FORMAT_STRING => {
                    let s = unsafe { val.u.string };
                    if !s.is_null() {
                        text_ptr = s;
                    }
                }
                b"start" if val.format == ffi::MPV_FORMAT_DOUBLE => {
                    start = unsafe { val.u.double_ };
                    has_start = start.is_finite();
                }
                b"end" if val.format == ffi::MPV_FORMAT_DOUBLE => {
                    end = unsafe { val.u.double_ };
                    has_end = end.is_finite();
                }
                _ => {}
            }
        }

        if has_id && !text_ptr.is_null() && has_start && has_end {
            lines.push(SubLine {
                id,
                text: copy_mpv_string(text_ptr),
                start,
                end,
            });
        }
    }

    lines
}

pub(super) fn extract_chapters_from_node(node: *const ffi::mpv_node) -> Vec<f64> {
    let Some(array_iter) = (unsafe { iter_node_array(node) }) else {
        return Vec::new();
    };

    let mut chapters = Vec::new();
    for entry in array_iter {
        let Some(map_iter) = (unsafe { iter_node_map(entry) }) else {
            continue;
        };
        for (key, val) in map_iter {
            if key == b"time" && val.format == ffi::MPV_FORMAT_DOUBLE {
                chapters.push(unsafe { val.u.double_ });
            }
        }
    }
    chapters
}

pub(super) fn extract_osd_dimensions_from_node(node: *const ffi::mpv_node) -> OsdDimensions {
    let mut dims = OsdDimensions::default();
    let Some(root) = (unsafe { node.as_ref() }) else {
        return dims;
    };
    let Some(map_iter) = (unsafe { iter_node_map(root) }) else {
        return dims;
    };

    for (key, val) in map_iter {
        let as_i64 = || -> Option<i64> {
            match val.format {
                ffi::MPV_FORMAT_INT64 => Some(unsafe { val.u.int64 }),
                ffi::MPV_FORMAT_DOUBLE => {
                    let d = unsafe { val.u.double_ };
                    d.is_finite().then_some(d.round() as i64)
                }
                _ => None,
            }
        };
        let as_f64 = || -> Option<f64> {
            match val.format {
                ffi::MPV_FORMAT_DOUBLE => Some(unsafe { val.u.double_ }),
                ffi::MPV_FORMAT_INT64 => Some(unsafe { val.u.int64 } as f64),
                _ => None,
            }
        };
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
