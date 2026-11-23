//! Owned track metadata shared by libmpv event delivery.

use crate::mpv::TrackType;

/// A media track delivered by a `track-list` update.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Track {
    pub id: i64,
    pub track_type: TrackType,
    pub src_id: i64,
    pub title: String,
    pub language: String,
    pub album_art: bool,
    pub default_track: bool,
    pub selected: bool,
    pub main_selection: i64,
    pub external: bool,
    pub external_filename: String,
    pub codec: String,
}
