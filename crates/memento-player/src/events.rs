//! Event vocabulary and the event-source abstraction for the Qt-free mpv
//! player core.
//!
//! This module defines *what can happen* to the player. It deliberately
//! contains no libmpv calls: the core never creates or drives an mpv handle
//! (see `mpv.rs`, the certified command bridge — the compiled adapter is the
//! only side that talks to libmpv). Probing calls the C++ player performs
//! inside its handlers (`mpv_get_property("video-params/w", …)` on
//! FILE_LOADED, `sub-start`/`sub-end` on `sub-text`, re-fetching
//! `track-list` on `track-list/count`) are the *source adapter's* job; the
//! results arrive here as ordinary event payloads.
//!
//! The central modeling decision (PROGRESS Entry 15 lesson) is the split
//! between [`EngineEvent`] and [`Intent`]:
//!
//! - `MPV_EVENT_SEEK` is an *engine* notification that some seek (user,
//!   script, auto-pause correction, internal cache op) happened. It is **not**
//!   user seek intent.
//! - `MpvController::seekRequested` — emitted after `MpvController::seek()`
//!   successfully queues an absolute seek — **is** the intent source.
//!   Conflating the two is the regression `asr_controller_retry_test`
//!   initially tripped over.
//!
//! A future UI drives the core by implementing [`MpvEventSource`] and feeding
//! it to `machine::PlayerCore`; `ScriptedEventSource` replays a fixed
//! sequence (tests, golden traces, fixture parity).

use std::ffi::CStr;

use crate::mpv::{CursorAutohide, ExtractedSubLine, ExtractedTrack, TrackType};

/// The initial `mpv_observe_property` order of `MpvPlayer::initializeMpv()`.
///
/// mpv delivers the initial property-change events per observed property, so
/// this order fixes the *initial application order* of volume, speed and
/// subtitle state: `volume-max` is observed before `volume`, and
/// `sub-delay` before `sub-speed`. Replaying events in any order produces
/// the same final state (setters are change-gated), but the *signal order* a
/// UI observes depends on this sequence. The `track-list/count` observation
/// has no payload here — the adapter re-fetches the full `track-list` node
/// and forwards it as [`PropertyChange::TrackList`].
pub const OBSERVED_PROPERTIES_INIT_ORDER: &[(&str, &str)] = &[
    ("chapter-list", "node"),
    ("duration", "double"),
    ("fullscreen", "flag"),
    ("media-title", "string"),
    ("path", "string"),
    ("pause", "flag"),
    ("time-pos", "double"),
    ("track-list/count", "int64"),
    ("volume-max", "int64"),
    ("volume", "int64"),
    ("video-params/w", "int64"),
    ("video-params/h", "int64"),
    ("aid", "int64"),
    ("aid", "flag"),
    ("secondary-sid", "int64"),
    ("secondary-sid", "flag"),
    ("sid", "int64"),
    ("sid", "flag"),
    ("vid", "int64"),
    ("vid", "flag"),
    ("secondary-sub-text", "string"),
    ("secondary-sub-delay", "double"),
    ("secondary-sub-visibility", "flag"),
    ("sub-delay", "double"),
    ("sub-speed", "double"),
    ("sub-lines", "node"),
    ("sub-text", "string"),
    ("sub-visibility", "flag"),
    ("cursor-autohide", "int64"),
    ("cursor-autohide", "string"),
    ("cursor-autohide-fs-only", "flag"),
];

/// A media track delivered by a `track-list` update (owned, Qt-free).
///
/// Field names and defaults mirror `MpvTrack` (`mpvtrack.h`): `id` 0,
/// type [`TrackType::None`], empty strings, all flags false. Entries whose
/// type did not parse to `audio`/`video`/`sub` are `TrackType::None` and are
/// dropped by the state bucketing — matching `MpvState::setTracks`.
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

/// Copies an [`ExtractedTrack`] (C-ABI view over mpv-owned node memory) into
/// an owned [`Track`].
///
/// # Safety
///
/// `t`'s `title`, `lang`, `external_filename` and `codec` pointers must be
/// valid C strings (or null — the extractor guarantees non-null, defaulting
/// to `c""`) for the duration of this call. The mpv node the pointers borrow
/// from must not be freed concurrently. Byte sequences that are not UTF-8 are
/// lossily replaced, matching `QString::fromUtf8`.
///
/// # Panics
///
/// Panics on interior NUL bytes. mpv C strings are NUL-terminated by
/// contract, so a NUL cannot occur; the panic documents that this is a
/// contract violation rather than silently truncating.
pub unsafe fn track_from_extracted(t: &ExtractedTrack) -> Track {
    unsafe fn owned(ptr: *const std::ffi::c_char) -> String {
        if ptr.is_null() {
            String::new()
        } else {
            let bytes = unsafe { CStr::from_ptr(ptr) }.to_bytes();
            String::from_utf8_lossy(bytes).into_owned()
        }
    }
    unsafe {
        Track {
            id: t.id,
            track_type: t.track_type,
            src_id: t.src_id,
            title: owned(t.title),
            language: owned(t.lang),
            album_art: t.albumart,
            default_track: t.default_track,
            selected: t.selected,
            main_selection: t.main_selection,
            external: t.external,
            external_filename: owned(t.external_filename),
            codec: owned(t.codec),
        }
    }
}

/// One `sub-lines` entry copied from the `primarySubtitleLinesChanged`
/// payload (`id`, `text`, `start`, `end` — the fields the C++ handler put
/// into a `QVariantMap` for the QML subtitle list).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SubLine {
    pub id: i64,
    pub text: String,
    pub start: f64,
    pub end: f64,
}

/// Copies an [`ExtractedSubLine`] (borrowed from mpv node memory) into an
/// owned [`SubLine`]. The extractor only yields complete lines, so `text` is
/// non-null here.
///
/// # Safety
///
/// `line.text` must be a valid NUL-terminated C string for the duration of
/// the call. Non-UTF-8 bytes are lossily replaced (`QString::fromUtf8`
/// parity).
///
/// # Panics
///
/// Panics on interior NUL bytes — an mpv contract violation (see
/// [`track_from_extracted`]).
pub unsafe fn sub_line_from_extracted(line: &ExtractedSubLine) -> SubLine {
    unsafe {
        SubLine {
            id: line.id,
            text: String::from_utf8_lossy(CStr::from_ptr(line.text).to_bytes()).into_owned(),
            start: line.start,
            end: line.end,
        }
    }
}

/// Which property a `MPV_FORMAT_NONE` ("property became unavailable")
/// property change refers to.
///
/// The C++ property handlers match on an exact mpv format, so an
/// unavailable-notification never mutates state: the previously delivered
/// value is retained indefinitely (stale) until the next valid delivery.
/// Naming the property keeps that behavior explicit instead of dropping the
/// event silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertyName {
    ChapterList,
    Duration,
    Fullscreen,
    MediaTitle,
    Path,
    Pause,
    TimePos,
    TrackListCount,
    Volume,
    VolumeMax,
    VideoWidth,
    VideoHeight,
    Aid,
    SecondarySid,
    Sid,
    Vid,
    SubDelay,
    SubSpeed,
    SecondarySubDelay,
    SubText,
    SecondarySubText,
    SubLines,
    SubVisibility,
    SecondarySubVisibility,
    CursorAutohide,
    CursorAutohideFsOnly,
}

/// A decoded `MPV_EVENT_PROPERTY_CHANGE` payload.
///
/// Variants carry already-decoded, typed values — the mpv-format checks
/// (`MPV_FORMAT_DOUBLE`/`MPV_FORMAT_FLAG`/…) stay in the source adapter,
/// exactly as the C++ property map does them. Events that would fail those
/// checks are [`PropertyChange::Unavailable`] (or are simply never emitted).
#[derive(Debug, Clone, PartialEq)]
pub enum PropertyChange {
    Chapters(Vec<f64>),
    /// `duration` in seconds. Unlike most properties this has no
    /// "unavailable" special-casing beyond the generic one — the C++ handler
    /// keeps whatever was last delivered.
    Duration(f64),
    Fullscreen(bool),
    MediaTitle(String),
    Path(String),
    Pause(bool),
    TimePos(f64),
    /// Full `track-list` re-fetch (the `track-list/count` observation).
    TrackList(Vec<Track>),
    Volume(i64),
    VolumeMax(i64),
    VideoWidth(i64),
    VideoHeight(i64),
    /// `aid` delivered as `MPV_FORMAT_INT64`.
    Aid(i64),
    /// `aid` delivered as `MPV_FORMAT_FLAG` `false` ("track disabled") —
    /// resets the selection to 0.
    AidDisabled,
    /// `sid` as `MPV_FORMAT_INT64`.
    Sid(i64),
    /// `sid` as `MPV_FORMAT_FLAG` `false`. Besides zeroing the selection the
    /// C++ handler also resets the *primary* subtitle text and start/end
    /// times (delay, speed and visibility are untouched).
    SidDisabled,
    /// `secondary-sid` as `MPV_FORMAT_INT64`.
    SecondarySid(i64),
    /// `secondary-sid` as `MPV_FORMAT_FLAG` `false` — resets the secondary
    /// subtitle text and start/end times.
    SecondarySidDisabled,
    Vid(i64),
    VidDisabled,
    /// `sub-speed` — applied to *both* the primary and secondary subtitle
    /// objects (a single observation in C++).
    SubSpeed(f64),
    /// `sub-delay` — primary subtitle only.
    SubDelay(f64),
    /// `secondary-sub-delay` — secondary subtitle only.
    SecondarySubDelay(f64),
    /// `sub-text`. The C++ handler additionally probes `sub-start`/`sub-end`
    /// from the mpv handle inside the handler; a failed probe leaves that
    /// component at `0.0`, which is what `None` means here (the adapter
    /// performs the probe, never the core).
    SubText { text: String, start: Option<f64>, end: Option<f64> },
    /// `secondary-sub-text` with the same `secondary-sub-start`/`-end`
    /// probe semantics.
    SecondarySubText { text: String, start: Option<f64>, end: Option<f64> },
    /// `sub-lines` snapshot forwarded to the UI as the primary subtitle line
    /// list (`primarySubtitleLinesChanged`). This is a player-level signal,
    /// not an `MpvState` property.
    SubLines(Vec<SubLine>),
    SubVisibility(bool),
    SecondarySubVisibility(bool),
    /// `cursor-autohide` decoded into its three forms. Delivered via two
    /// observations (int64 → timeout form, string → `always`/`no`); other
    /// strings and formats are dropped by the decoder.
    CursorAutohide(CursorAutohide),
    CursorAutohideFullscreenOnly(bool),
    /// The property change arrived as `MPV_FORMAT_NONE` ("property became
    /// unavailable") or with a format the observed format never accepts.
    /// C++ handlers match the exact format, so this mutates nothing — the
    /// previously delivered value is retained stale. The name keeps the
    /// behavior explicit for tests and diagnostics.
    Unavailable(PropertyName),
}

/// Events originating from the mpv event queue.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent {

    /// `MPV_EVENT_START_FILE` — mpv begins loading a file.
    StartFile,
    /// `MPV_EVENT_FILE_LOADED`. The C++ handler probes `video-params/w|h`
    /// from the handle and reports 0 for a failed/absent probe; the adapter
    /// performs that probe and supplies the values here.
    FileLoaded { video_width: i64, video_height: i64 },
    /// `MPV_EVENT_END_FILE` — finished *or* aborted.
    EndFile,
    /// `MPV_EVENT_SEEK` — an engine-side notification that a seek happened
    /// for *any* reason. This is deliberately distinct from
    /// [`Intent::SeekAbsolute`]; see the module documentation.
    EngineSeek,
    /// `MPV_EVENT_SHUTDOWN` — the mpv handle is gone.
    Shutdown,
    Property(PropertyChange),
}

/// Controller/user intents — the `MpvController`-side actions a UI invokes.
///
/// These are *signals*, not engine events: play/pause are applied to mpv as
/// properties by the adapter, and a seek is queued by the adapter before this
/// intent is fed back into the core (mirroring the C++ sequencing where
/// `MpvController::seek()` queues the `mpv_command_async` and *then* emits
/// `seekRequested`; a failed queue emits nothing).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Intent {
    /// `MpvController::play()` — sets `pause=0`.
    Play,
    /// `MpvController::pause()` — sets `pause=1`.
    Pause,
    /// `MpvController::stop()` — media lifecycle teardown.
    Stop,
    /// `MpvController::seekRequested(time)` — a successfully queued absolute
    /// seek. The seek-latch source for consumers (ASR retry logic).
    SeekAbsolute(f64),
}

/// Everything the core can be told. Cloned rather than copied: property
/// payloads own heap data (`String`/`Vec`), exactly like the mpv node
/// contents the adapter copies out.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Engine(EngineEvent),
    Intent(Intent),
}

impl From<EngineEvent> for Event {
    fn from(e: EngineEvent) -> Self {
        Event::Engine(e)
    }
}

impl From<Intent> for Event {
    fn from(i: Intent) -> Self {
        Event::Intent(i)
    }
}

/// The input seam of the player core: whatever produces mpv/controller
/// events. Implementors must not require libmpv calls from inside the core;
/// probing stays on the adapter side (see the module documentation).
pub trait MpvEventSource {
    /// Returns the next event, or `None` when the queue is drained —
    /// mirroring the `mpv_wait_event(..) == MPV_EVENT_NONE` stop condition of
    /// `MpvPlayer::handleMpvEvents()`. Events must be returned in delivery
    /// order.
    fn poll(&mut self) -> Option<Event>;
}

/// A scripted [`MpvEventSource`] that replays a fixed sequence and then
/// reports exhausted. Used for fixture traces matching real playback
/// sessions (initial observation batch, load, ticks, seek, end-file, …).
#[derive(Debug, Default)]
pub struct ScriptedEventSource {
    events: Vec<Event>,
    cursor: usize,
}

impl ScriptedEventSource {
    pub fn new(events: Vec<Event>) -> Self {
        Self { events, cursor: 0 }
    }

    pub fn push(&mut self, event: impl Into<Event>) {
        self.events.push(event.into());
    }

    pub fn remaining(&self) -> usize {
        self.events.len().saturating_sub(self.cursor)
    }
}

impl MpvEventSource for ScriptedEventSource {
    fn poll(&mut self) -> Option<Event> {
        let event = self.events.get(self.cursor).cloned();
        if event.is_some() {
            self.cursor += 1;
        }
        event
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mpv::CursorAutohide;

    #[test]
    fn observed_property_init_order_matches_initialize_mpv() {
        // The C++ observe list, verified against mpvplayer.cpp:607-640.
        let names: Vec<&str> =
            OBSERVED_PROPERTIES_INIT_ORDER.iter().map(|(n, _)| *n).collect();
        assert_eq!(
            names,
            vec![
                "chapter-list",
                "duration",
                "fullscreen",
                "media-title",
                "path",
                "pause",
                "time-pos",
                "track-list/count",
                "volume-max",
                "volume",
                "video-params/w",
                "video-params/h",
                "aid",
                "aid",
                "secondary-sid",
                "secondary-sid",
                "sid",
                "sid",
                "vid",
                "vid",
                "secondary-sub-text",
                "secondary-sub-delay",
                "secondary-sub-visibility",
                "sub-delay",
                "sub-speed",
                "sub-lines",
                "sub-text",
                "sub-visibility",
                "cursor-autohide",
                "cursor-autohide",
                "cursor-autohide-fs-only",
            ]
        );
        // Application-order facts a UI depends on.
        let pos = |name: &str| names.iter().position(|n| *n == name).unwrap();
        assert!(pos("volume-max") < pos("volume"));
        assert!(pos("sub-delay") < pos("sub-speed"));
        assert!(pos("track-list/count") < pos("volume-max"));
        assert!(pos("secondary-sub-delay") < pos("secondary-sub-visibility"));
    }

    #[test]
    fn scripted_source_preserves_order_then_exhausts() {
        let mut source = ScriptedEventSource::new(vec![
            EngineEvent::StartFile.into(),
            EngineEvent::FileLoaded { video_width: 640, video_height: 360 }.into(),
        ]);
        assert_eq!(source.remaining(), 2);
        assert_eq!(source.poll(), Some(Event::Engine(EngineEvent::StartFile)));
        assert_eq!(source.remaining(), 1);
        assert_eq!(
            source.poll(),
            Some(Event::Engine(EngineEvent::FileLoaded {
                video_width: 640,
                video_height: 360
            }))
        );
        source.push(Intent::Play);
        assert_eq!(source.poll(), Some(Event::Intent(Intent::Play)));
        assert_eq!(source.remaining(), 0);
        assert_eq!(source.poll(), None);
        assert_eq!(source.poll(), None);
    }

    #[test]
    fn intent_and_engine_seek_are_distinct_variants() {
        // The regression PROGRESS Entry 15 warns about, encoded in the type
        // system: MPV_EVENT_SEEK (engine) and seekRequested (intent) cannot
        // be conflated — the machine matches them exhaustively and separately.
        let engine = Event::Engine(EngineEvent::EngineSeek);
        let intent = Event::Intent(Intent::SeekAbsolute(42.0));
        assert_ne!(engine, intent);
        match (engine, intent) {
            (Event::Engine(EngineEvent::EngineSeek), Event::Intent(Intent::SeekAbsolute(t))) => {
                assert_eq!(t, 42.0);
            }
            _ => unreachable!("variants must stay disjoint"),
        }
    }

    #[test]
    fn intent_copy_semantics_and_conversion() {
        let events: Vec<Event> = vec![
            Intent::Pause.into(),
            EngineEvent::Shutdown.into(),
            Intent::SeekAbsolute(-1.5).into(),
        ];
        assert_eq!(
            events,
            vec![
                Event::Intent(Intent::Pause),
                Event::Engine(EngineEvent::Shutdown),
                Event::Intent(Intent::SeekAbsolute(-1.5)),
            ]
        );
    }

    #[test]
    fn owned_track_conversion_drops_no_fields() {
        let mut extracted = ExtractedTrack::default();
        let title = c"JP AAC";
        let lang = c"ja";
        let filename = c"/tmp/ext.ass";
        extracted.id = 2;
        extracted.track_type = TrackType::Audio;
        extracted.src_id = 7;
        extracted.title = title.as_ptr();
        extracted.lang = lang.as_ptr();
        extracted.albumart = true;
        extracted.default_track = true;
        extracted.selected = true;
        extracted.main_selection = 1;
        extracted.external = true;
        extracted.external_filename = filename.as_ptr();
        extracted.codec = c"aac".as_ptr();

        let owned = unsafe { track_from_extracted(&extracted) };
        assert_eq!(owned.id, 2);
        assert_eq!(owned.track_type, TrackType::Audio);
        assert_eq!(owned.src_id, 7);
        assert_eq!(owned.title, "JP AAC");
        assert_eq!(owned.language, "ja");
        assert!(owned.album_art);
        assert!(owned.default_track);
        assert!(owned.selected);
        assert_eq!(owned.main_selection, 1);
        assert!(owned.external);
        assert_eq!(owned.external_filename, "/tmp/ext.ass");
        assert_eq!(owned.codec, "aac");
        assert_eq!(owned, Track::default().with_fields());
    }

    /// Small helper to keep the assertion above readable.
    impl Track {
        fn with_fields(mut self) -> Self {
            self.id = 2;
            self.track_type = TrackType::Audio;
            self.src_id = 7;
            self.title = "JP AAC".into();
            self.language = "ja".into();
            self.album_art = true;
            self.default_track = true;
            self.selected = true;
            self.main_selection = 1;
            self.external = true;
            self.external_filename = "/tmp/ext.ass".into();
            self.codec = "aac".into();
            self
        }
    }

    #[test]
    fn owned_track_conversion_of_defaults_is_empty() {
        let owned = unsafe { track_from_extracted(&ExtractedTrack::default()) };
        assert_eq!(owned, Track::default());
    }

    #[test]
    fn owned_sub_line_conversion() {
        let text = c"こんにちは";
        let line = ExtractedSubLine { id: 11, text: text.as_ptr(), start: 1.25, end: 2.5 };
        let owned = unsafe { sub_line_from_extracted(&line) };
        assert_eq!(
            owned,
            SubLine { id: 11, text: "こんにちは".into(), start: 1.25, end: 2.5 }
        );
    }

    #[test]
    fn cursor_autohide_payload_round_trip_through_property() {
        let number = PropertyChange::CursorAutohide(CursorAutohide::Number(1000));
        assert_eq!(
            number,
            PropertyChange::CursorAutohide(CursorAutohide::Number(1000))
        );
        let (hidden, timeout) = CursorAutohide::Always.to_c_params();
        assert!(hidden);
        assert_eq!(timeout, -1);
        let (hidden, _) = CursorAutohide::Never.to_c_params();
        assert!(!hidden);
    }
}
