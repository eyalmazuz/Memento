//! Fast Pure-Rust EBML Matroska (`.mkv`/`.mka`/`.mks`) reader for extracting:
//! - Embedded font attachments (`extract_mkv_fonts`) via `SeekHead` fast-jump (< 5 ms even on GB files)
//! - Subtitle track metadata (`list_mkv_subtitle_tracks`)
//! - Complete embedded subtitle tracks (`extract_mkv_subtitle_track`) for `S_TEXT/ASS`,
//!   `S_TEXT/SSA`, `S_TEXT/UTF8` (SRT), and `S_TEXT/WEBVTT` without invoking external binaries.

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use flate2::read::ZlibDecoder;

use crate::parser::{format_ass_timecode, strip_ass_dialogue_text};

// EBML / Matroska element IDs (raw big-endian VINT including marker bit).
const ID_EBML: u32 = 0x1A45_DFA3;
const ID_SEGMENT: u32 = 0x1853_8067;
const ID_SEEK_HEAD: u32 = 0x114D_9B74;
const ID_SEEK: u32 = 0x4DBB;
const ID_SEEK_ID: u32 = 0x53AB;
const ID_SEEK_POSITION: u32 = 0x53AC;

const ID_INFO: u32 = 0x1549_A966;
const ID_TIMESTAMP_SCALE: u32 = 0x2AD7_B1;

const ID_TRACKS: u32 = 0x1654_AE6B;
const ID_TRACK_ENTRY: u32 = 0xAE;
const ID_TRACK_NUMBER: u32 = 0xD7;
const ID_TRACK_TYPE: u32 = 0x83;
const ID_TRACK_NAME: u32 = 0x536E;
const ID_TRACK_LANGUAGE: u32 = 0x22B5_9C;
const ID_TRACK_LANGUAGE_BCP47: u32 = 0x22B5_9D;
const ID_CODEC_ID: u32 = 0x86;
const ID_CODEC_PRIVATE: u32 = 0x63A2;
const ID_FLAG_DEFAULT: u32 = 0x88;
const ID_FLAG_FORCED: u32 = 0x55AA;

const ID_CONTENT_ENCODINGS: u32 = 0x6D80;
const ID_CONTENT_ENCODING: u32 = 0x6240;
const ID_CONTENT_ENCODING_SCOPE: u32 = 0x5032;
const ID_CONTENT_ENCODING_TYPE: u32 = 0x5033;
const ID_CONTENT_COMPRESSION: u32 = 0x5034;
const ID_CONTENT_COMP_ALGO: u32 = 0x4254;
const ID_CONTENT_COMP_SETTINGS: u32 = 0x4255;

const ID_ATTACHMENTS: u32 = 0x1941_A469;
const ID_ATTACHED_FILE: u32 = 0x61A7;
const ID_FILE_NAME: u32 = 0x466E;
const ID_FILE_MIME_TYPE: u32 = 0x4660;
const ID_FILE_DATA: u32 = 0x465C;

const ID_CLUSTER: u32 = 0x1F43_B675;
const ID_CLUSTER_TIMESTAMP: u32 = 0xE7;
const ID_SIMPLE_BLOCK: u32 = 0xA3;
const ID_BLOCK_GROUP: u32 = 0xA0;
const ID_BLOCK: u32 = 0xA1;
const ID_BLOCK_DURATION: u32 = 0x9B;

const TRACK_TYPE_SUBTITLE: u64 = 0x11;
const MAX_FONT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CODEC_PRIVATE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_BLOCK_BYTES: u64 = 4 * 1024 * 1024;
const BUF_READER_CAPACITY: usize = 256 * 1024;

/// Subtitle codec identified from Matroska `CodecID`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MkvSubtitleCodec {
    Ass,
    Ssa,
    Srt,
    WebVtt,
    Other,
}

impl MkvSubtitleCodec {
    pub fn from_codec_id(codec_id: &str) -> Self {
        match codec_id {
            "S_TEXT/ASS" => Self::Ass,
            "S_TEXT/SSA" => Self::Ssa,
            "S_TEXT/UTF8" => Self::Srt,
            "S_TEXT/WEBVTT" => Self::WebVtt,
            _ => Self::Other,
        }
    }

    pub fn is_ass_or_ssa(self) -> bool {
        matches!(self, Self::Ass | Self::Ssa)
    }
}

/// Embedded font attachment extracted from a Matroska `Attachments` section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MkvFontAttachment {
    pub file_name: String,
    pub mime_type: String,
    pub data: Vec<u8>,
}

/// Metadata for a single subtitle track inside a Matroska container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MkvSubtitleTrackInfo {
    /// Matroska `TrackNumber` (matches `mpv`'s `track-list/N/src-id`!).
    pub track_number: u64,
    /// 0-based subtitle track index in track order (matches `ffmpeg -map 0:s:N`).
    pub subtitle_index: usize,
    pub codec_id: String,
    pub codec: MkvSubtitleCodec,
    pub name: Option<String>,
    pub language: Option<String>,
    pub default: bool,
    pub forced: bool,
    /// Raw `CodecPrivate` bytes (for ASS/SSA: `[Script Info]` + `[V4+ Styles]` + `[Events]` header).
    pub codec_private: Vec<u8>,
}

impl MkvSubtitleTrackInfo {
    /// Decode `codec_private` as UTF-8 text (stripping any leading UTF-8 BOM).
    pub fn codec_private_utf8(&self) -> String {
        let s = String::from_utf8_lossy(&self.codec_private);
        s.strip_prefix('\u{feff}').unwrap_or(&s).to_string()
    }
}

/// One decoded subtitle cue extracted from a Matroska track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MkvSubtitleCue {
    pub read_order: i64,
    pub layer: i32,
    pub start_ms: i64,
    pub end_ms: i64,
    pub style: String,
    pub name: String,
    pub margin_l: i32,
    pub margin_r: i32,
    pub margin_v: i32,
    pub effect: String,
    /// Raw cue payload text (for ASS/SSA: dialogue `Text` field with override tags intact).
    pub raw_text: String,
    /// Clean human-readable text (override tags & drawings stripped for ASS/SSA).
    pub plain_text: String,
}

/// Complete extracted subtitle track (header metadata + all sorted cues).
#[derive(Debug, Clone)]
pub struct ExtractedSubtitleTrack {
    pub info: MkvSubtitleTrackInfo,
    pub cues: Vec<MkvSubtitleCue>,
}

impl ExtractedSubtitleTrack {
    /// Reconstruct a complete standalone `.ass` document combining `CodecPrivate` and all
    /// extracted cues as `Dialogue:` lines.
    pub fn to_ass_document(&self) -> String {
        let header = self.info.codec_private_utf8();
        let mut out = String::with_capacity(header.len() + self.cues.len() * 96);
        out.push_str(header.trim_end());
        out.push('\n');

        let has_events = header
            .lines()
            .any(|l| l.trim().eq_ignore_ascii_case("[Events]"));
        if !has_events {
            out.push_str(
                "\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
            );
        } else {
            let has_format = header
                .lines()
                .skip_while(|l| !l.trim().eq_ignore_ascii_case("[Events]"))
                .skip(1)
                .any(|l| l.trim().to_ascii_lowercase().starts_with("format:"));
            if !has_format {
                out.push_str(
                    "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
                );
            }
        }

        for cue in &self.cues {
            let start_tc = format_ass_timecode(cue.start_ms);
            let end_tc = format_ass_timecode(cue.end_ms.max(cue.start_ms));
            out.push_str(&format!(
                "Dialogue: {},{},{},{},{},{},{},{},{},{}\n",
                cue.layer,
                start_tc,
                end_tc,
                cue.style,
                cue.name,
                cue.margin_l,
                cue.margin_r,
                cue.margin_v,
                cue.effect,
                cue.raw_text,
            ));
        }
        out
    }
}

#[derive(Debug, Clone)]
enum ContentCompression {
    Zlib,
    HeaderStripping(Vec<u8>),
    Unsupported(u64),
}

#[derive(Debug, Clone)]
struct TrackEncoding {
    scope: u64,
    compression: ContentCompression,
}

#[derive(Debug, Clone)]
struct InternalTrackMeta {
    info: MkvSubtitleTrackInfo,
    encodings: Vec<TrackEncoding>,
}

/// Extract all embedded font attachments from a Matroska container (`< 5 ms` via `SeekHead`).
pub fn extract_mkv_fonts(path: impl AsRef<Path>) -> io::Result<Vec<MkvFontAttachment>> {
    let mut reader = open_reader(path.as_ref())?;
    let seg = find_segment(&mut reader)?;
    let meta = scan_segment_metadata(&mut reader, &seg, true, false)?;
    Ok(meta.fonts)
}

/// List all subtitle tracks (with `TrackNumber` / `src-id`, codec, language, and `CodecPrivate`)
/// in a Matroska container without scanning video/audio clusters.
pub fn list_mkv_subtitle_tracks(path: impl AsRef<Path>) -> io::Result<Vec<MkvSubtitleTrackInfo>> {
    let mut reader = open_reader(path.as_ref())?;
    let seg = find_segment(&mut reader)?;
    let meta = scan_segment_metadata(&mut reader, &seg, false, true)?;
    Ok(meta.tracks.into_iter().map(|t| t.info).collect())
}

/// Extract all cues of the specified subtitle track (`track_number` = Matroska `TrackNumber`,
/// matching `mpv`'s `track-list/N/src-id`) from a Matroska container.
pub fn extract_mkv_subtitle_track(
    path: impl AsRef<Path>,
    track_number: u64,
) -> io::Result<ExtractedSubtitleTrack> {
    let mut reader = open_reader(path.as_ref())?;
    let seg = find_segment(&mut reader)?;
    let meta = scan_segment_metadata(&mut reader, &seg, false, true)?;

    let target = meta
        .tracks
        .into_iter()
        .find(|t| t.info.track_number == track_number)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("Subtitle track_number {track_number} not found in MKV"),
            )
        })?;

    let mut cues = Vec::new();
    let cluster_start = meta.first_cluster_pos.unwrap_or(seg.data_start);
    reader.seek(SeekFrom::Start(cluster_start))?;

    let ts_scale = meta.timestamp_scale;
    while stream_pos(&mut reader)? < seg.end_pos {
        let Some(hdr) = read_element_header(&mut reader, seg.end_pos)? else {
            break;
        };
        let elem_end = element_end_pos(hdr.data_start, hdr.size, seg.end_pos);
        if hdr.id == ID_CLUSTER {
            parse_cluster_for_track(&mut reader, elem_end, ts_scale, &target, &mut cues)?;
        } else if hdr.size.is_none() {
            // Cannot skip a non-Cluster element with unknown size safely.
            break;
        }
        // Advance cleanly to next top-level element inside Segment.
        let cur = stream_pos(&mut reader)?;
        if hdr.size.is_some() && cur != elem_end {
            reader.seek(SeekFrom::Start(elem_end))?;
        }
    }

    cues.sort_by_key(|c| (c.start_ms, c.read_order));
    dedup_extracted_cues(&mut cues, target.info.codec.is_ass_or_ssa());
    Ok(ExtractedSubtitleTrack {
        info: target.info,
        cues,
    })
}

/// Normalize extracted plain-text subtitle content: strips trailing NULs,
/// converts CRLF (`\r\n`) and CR (`\r`) line endings to `\n`, collapses
/// redundant blank lines inside multi-line cues, and trims leading/trailing
/// whitespace and blank lines.
fn normalize_plain_subtitle_text(text: &str) -> String {
    let stripped = text.trim_end_matches('\0');
    let normalized = stripped.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(normalized.len());
    for line in normalized.split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line.trim_end_matches([' ', '\t']));
    }
    out.trim().to_string()
}

/// Remove exact duplicate MKV subtitle packets (timestamps within 10ms).
///
/// For ASS/SSA tracks, two cues are only considered duplicates when their
/// `layer`, `style`, and `raw_text` match (preserving distinct `\pos(...)`
/// simultaneous dialogue lines required by `libass` rendering). For SRT/WebVTT
/// tracks, identical normalized `plain_text` within 10ms is deduplicated.
fn dedup_extracted_cues(cues: &mut Vec<MkvSubtitleCue>, is_ass: bool) {
    const TIME_EPS_MS: i64 = 10;
    let mut kept: Vec<MkvSubtitleCue> = Vec::with_capacity(cues.len());
    for cue in cues.drain(..) {
        let is_dup = kept
            .iter()
            .rev()
            .take_while(|prev| cue.start_ms.saturating_sub(prev.start_ms) <= TIME_EPS_MS)
            .any(|prev| {
                (cue.start_ms - prev.start_ms).abs() <= TIME_EPS_MS
                    && (cue.end_ms - prev.end_ms).abs() <= TIME_EPS_MS
                    && if is_ass {
                        cue.layer == prev.layer
                            && cue.style == prev.style
                            && cue.raw_text == prev.raw_text
                    } else {
                        cue.plain_text == prev.plain_text
                    }
            });
        if !is_dup {
            kept.push(cue);
        }
    }
    *cues = kept;
}

/// Zero-syscall position tracking wrapper around `BufReader<File>` so scanning
/// tens of thousands of Matroska `SimpleBlock`s avoids `lseek(SEEK_CUR)` per element
/// while keeping `BufReader`'s 256 KB buffer hot via `seek_relative`.
struct TrackedReader {
    inner: BufReader<File>,
    pos: u64,
}

impl TrackedReader {
    fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        Ok(Self {
            inner: BufReader::with_capacity(BUF_READER_CAPACITY, file),
            pos: 0,
        })
    }

    #[inline]
    fn seek_relative(&mut self, offset: i64) -> io::Result<()> {
        if offset == 0 {
            return Ok(());
        }
        let new_pos = (self.pos as i128)
            .checked_add(offset as i128)
            .filter(|&p| p >= 0)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before start"))?;
        self.inner.seek_relative(offset)?;
        self.pos = new_pos as u64;
        Ok(())
    }
}

impl Read for TrackedReader {
    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for TrackedReader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        match pos {
            SeekFrom::Start(target) => {
                let delta = (target as i128) - (self.pos as i128);
                if let Ok(d64) = i64::try_from(delta) {
                    self.seek_relative(d64)?;
                    return Ok(self.pos);
                }
                let p = self.inner.seek(SeekFrom::Start(target))?;
                self.pos = p;
                Ok(p)
            }
            SeekFrom::Current(delta) => {
                self.seek_relative(delta)?;
                Ok(self.pos)
            }
            SeekFrom::End(off) => {
                let p = self.inner.seek(SeekFrom::End(off))?;
                self.pos = p;
                Ok(p)
            }
        }
    }

    #[inline]
    fn stream_position(&mut self) -> io::Result<u64> {
        Ok(self.pos)
    }
}

fn open_reader(path: &Path) -> io::Result<TrackedReader> {
    TrackedReader::open(path)
}

#[derive(Debug, Clone, Copy)]
struct SegmentBounds {
    data_start: u64,
    end_pos: u64,
}

#[derive(Debug, Clone, Copy)]
struct ElementHeader {
    id: u32,
    size: Option<u64>,
    data_start: u64,
}

#[inline]
fn stream_pos<R: Seek>(reader: &mut R) -> io::Result<u64> {
    reader.stream_position()
}

#[inline]
fn element_end_pos(data_start: u64, size: Option<u64>, parent_end: u64) -> u64 {
    match size {
        Some(s) => data_start.saturating_add(s).min(parent_end),
        None => parent_end,
    }
}

fn find_segment<R: Read + Seek>(reader: &mut R) -> io::Result<SegmentBounds> {
    let file_len = reader.seek(SeekFrom::End(0))?;
    reader.seek(SeekFrom::Start(0))?;

    let first = read_element_header(reader, file_len)?.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "Empty file or invalid EBML header")
    })?;
    if first.id != ID_EBML {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Not an EBML/Matroska file (magic 0x{:X})", first.id),
        ));
    }
    let ebml_end = element_end_pos(first.data_start, first.size, file_len);
    reader.seek(SeekFrom::Start(ebml_end))?;

    while stream_pos(reader)? < file_len {
        let Some(hdr) = read_element_header(reader, file_len)? else {
            break;
        };
        let end_pos = element_end_pos(hdr.data_start, hdr.size, file_len);
        if hdr.id == ID_SEGMENT {
            return Ok(SegmentBounds {
                data_start: hdr.data_start,
                end_pos,
            });
        }
        if hdr.size.is_none() {
            break;
        }
        reader.seek(SeekFrom::Start(end_pos))?;
    }

    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "Matroska Segment element not found",
    ))
}

struct SegmentMetadata {
    timestamp_scale: u64,
    tracks: Vec<InternalTrackMeta>,
    fonts: Vec<MkvFontAttachment>,
    first_cluster_pos: Option<u64>,
}

fn scan_segment_metadata(
    reader: &mut TrackedReader,
    seg: &SegmentBounds,
    want_fonts: bool,
    want_tracks: bool,
) -> io::Result<SegmentMetadata> {
    let mut timestamp_scale: u64 = 1_000_000;
    let mut tracks: Vec<InternalTrackMeta> = Vec::new();
    let mut fonts: Vec<MkvFontAttachment> = Vec::new();
    let mut first_cluster_pos: Option<u64> = None;

    let mut visited_offsets: HashSet<u64> = HashSet::new();
    let mut pending_seeks: Vec<(u32, u64)> = Vec::new();
    let mut saw_info = false;
    let mut saw_tracks = false;
    let mut saw_attachments = false;

    reader.seek(SeekFrom::Start(seg.data_start))?;

    // 1. Scan leading top-level elements before the first Cluster.
    while stream_pos(reader)? < seg.end_pos {
        let elem_start = stream_pos(reader)?;
        let Some(hdr) = read_element_header(reader, seg.end_pos)? else {
            break;
        };
        let elem_end = element_end_pos(hdr.data_start, hdr.size, seg.end_pos);
        visited_offsets.insert(elem_start);

        match hdr.id {
            ID_SEEK_HEAD => {
                parse_seek_head(reader, elem_end, seg.data_start, &mut pending_seeks)?;
            }
            ID_INFO => {
                timestamp_scale = parse_info(reader, elem_end)?;
                saw_info = true;
            }
            ID_TRACKS if want_tracks => {
                tracks = parse_tracks(reader, elem_end)?;
                saw_tracks = true;
            }
            ID_ATTACHMENTS if want_fonts => {
                parse_attachments(reader, elem_end, &mut fonts)?;
                saw_attachments = true;
            }
            ID_CLUSTER => {
                first_cluster_pos = Some(elem_start);
                break;
            }
            _ => {}
        }

        if hdr.size.is_none() {
            break;
        }
        reader.seek(SeekFrom::Start(elem_end))?;
    }

    // 2. Follow SeekHead entries (including chained secondary SeekHeads) to find any
    //    metadata placed after Clusters (common for Attachments / trailing SeekHead).
    let mut idx = 0;
    while idx < pending_seeks.len() {
        let (target_id, abs_pos) = pending_seeks[idx];
        idx += 1;

        if abs_pos < seg.data_start || abs_pos >= seg.end_pos || !visited_offsets.insert(abs_pos) {
            continue;
        }
        let needed = match target_id {
            ID_SEEK_HEAD => true,
            ID_INFO => !saw_info,
            ID_TRACKS => want_tracks && !saw_tracks,
            ID_ATTACHMENTS => want_fonts && !saw_attachments,
            _ => false,
        };
        if !needed {
            continue;
        }

        reader.seek(SeekFrom::Start(abs_pos))?;
        let Some(hdr) = read_element_header(reader, seg.end_pos)? else {
            continue;
        };
        let elem_end = element_end_pos(hdr.data_start, hdr.size, seg.end_pos);
        match hdr.id {
            ID_SEEK_HEAD => {
                parse_seek_head(reader, elem_end, seg.data_start, &mut pending_seeks)?;
            }
            ID_INFO if !saw_info => {
                timestamp_scale = parse_info(reader, elem_end)?;
                saw_info = true;
            }
            ID_TRACKS if want_tracks && !saw_tracks => {
                tracks = parse_tracks(reader, elem_end)?;
                saw_tracks = true;
            }
            ID_ATTACHMENTS if want_fonts && !saw_attachments => {
                parse_attachments(reader, elem_end, &mut fonts)?;
                saw_attachments = true;
            }
            _ => {}
        }
    }

    Ok(SegmentMetadata {
        timestamp_scale,
        tracks,
        fonts,
        first_cluster_pos,
    })
}

fn parse_seek_head<R: Read + Seek>(
    reader: &mut R,
    end_pos: u64,
    segment_data_start: u64,
    out: &mut Vec<(u32, u64)>,
) -> io::Result<()> {
    while stream_pos(reader)? < end_pos {
        let Some(hdr) = read_element_header(reader, end_pos)? else {
            break;
        };
        let child_end = element_end_pos(hdr.data_start, hdr.size, end_pos);
        if hdr.id == ID_SEEK {
            let mut seek_id: Option<u32> = None;
            let mut seek_pos: Option<u64> = None;
            while stream_pos(reader)? < child_end {
                let Some(sub) = read_element_header(reader, child_end)? else {
                    break;
                };
                let sub_end = element_end_pos(sub.data_start, sub.size, child_end);
                let sz = sub_end.saturating_sub(sub.data_start);
                match sub.id {
                    ID_SEEK_ID if sz >= 1 && sz <= 4 => {
                        let bytes = read_exact_vec(reader, sz as usize)?;
                        let mut id = 0u32;
                        for b in bytes {
                            id = (id << 8) | b as u32;
                        }
                        seek_id = Some(id);
                    }
                    ID_SEEK_POSITION if sz <= 8 => {
                        seek_pos = Some(read_uint(reader, sz as usize)?);
                    }
                    _ => {}
                }
                if stream_pos(reader)? != sub_end {
                    reader.seek(SeekFrom::Start(sub_end))?;
                }
            }
            if let (Some(id), Some(rel)) = (seek_id, seek_pos) {
                if let Some(abs) = segment_data_start.checked_add(rel) {
                    out.push((id, abs));
                }
            }
        }
        if stream_pos(reader)? != child_end {
            reader.seek(SeekFrom::Start(child_end))?;
        }
    }
    Ok(())
}

fn parse_info<R: Read + Seek>(reader: &mut R, end_pos: u64) -> io::Result<u64> {
    let mut scale: u64 = 1_000_000;
    while stream_pos(reader)? < end_pos {
        let Some(hdr) = read_element_header(reader, end_pos)? else {
            break;
        };
        let child_end = element_end_pos(hdr.data_start, hdr.size, end_pos);
        let sz = child_end.saturating_sub(hdr.data_start);
        if hdr.id == ID_TIMESTAMP_SCALE && sz <= 8 {
            let v = read_uint(reader, sz as usize)?;
            if v > 0 {
                scale = v;
            }
        }
        if stream_pos(reader)? != child_end {
            reader.seek(SeekFrom::Start(child_end))?;
        }
    }
    Ok(scale)
}

fn parse_tracks<R: Read + Seek>(
    reader: &mut R,
    end_pos: u64,
) -> io::Result<Vec<InternalTrackMeta>> {
    let mut out = Vec::new();
    let mut sub_index: usize = 0;

    while stream_pos(reader)? < end_pos {
        let Some(hdr) = read_element_header(reader, end_pos)? else {
            break;
        };
        let child_end = element_end_pos(hdr.data_start, hdr.size, end_pos);
        if hdr.id == ID_TRACK_ENTRY {
            if let Some(mut entry) = parse_track_entry(reader, child_end, sub_index)? {
                // Decompress CodecPrivate if an encoding applies to scope bit 2.
                if !entry.info.codec_private.is_empty() && !entry.encodings.is_empty() {
                    if let Ok(decoded) = apply_track_encodings(
                        &entry.info.codec_private,
                        &entry.encodings,
                        2,
                        MAX_CODEC_PRIVATE_BYTES,
                    ) {
                        entry.info.codec_private = decoded;
                    }
                }
                sub_index += 1;
                out.push(entry);
            }
        }
        if stream_pos(reader)? != child_end {
            reader.seek(SeekFrom::Start(child_end))?;
        }
    }
    Ok(out)
}

fn parse_track_entry<R: Read + Seek>(
    reader: &mut R,
    end_pos: u64,
    subtitle_index: usize,
) -> io::Result<Option<InternalTrackMeta>> {
    let mut track_number: u64 = 0;
    let mut track_type: u64 = 0;
    let mut codec_id = String::new();
    let mut name: Option<String> = None;
    let mut language: Option<String> = None;
    let mut language_bcp47: Option<String> = None;
    let mut default = true; // Matroska spec default for FlagDefault is 1.
    let mut forced = false;
    let mut codec_private: Vec<u8> = Vec::new();
    let mut encodings: Vec<TrackEncoding> = Vec::new();

    while stream_pos(reader)? < end_pos {
        let Some(hdr) = read_element_header(reader, end_pos)? else {
            break;
        };
        let child_end = element_end_pos(hdr.data_start, hdr.size, end_pos);
        let sz = child_end.saturating_sub(hdr.data_start);
        match hdr.id {
            ID_TRACK_NUMBER if sz <= 8 => track_number = read_uint(reader, sz as usize)?,
            ID_TRACK_TYPE if sz <= 8 => track_type = read_uint(reader, sz as usize)?,
            ID_TRACK_NAME if sz <= 4096 => {
                name = Some(read_utf8(reader, sz as usize)?);
            }
            ID_TRACK_LANGUAGE if sz <= 256 => {
                language = Some(read_utf8(reader, sz as usize)?);
            }
            ID_TRACK_LANGUAGE_BCP47 if sz <= 256 => {
                language_bcp47 = Some(read_utf8(reader, sz as usize)?);
            }
            ID_CODEC_ID if sz <= 256 => {
                codec_id = read_utf8(reader, sz as usize)?;
            }
            ID_FLAG_DEFAULT if sz <= 8 => default = read_uint(reader, sz as usize)? != 0,
            ID_FLAG_FORCED if sz <= 8 => forced = read_uint(reader, sz as usize)? != 0,
            ID_CODEC_PRIVATE if sz <= MAX_CODEC_PRIVATE_BYTES => {
                codec_private = read_exact_vec(reader, sz as usize)?;
            }
            ID_CONTENT_ENCODINGS => {
                parse_content_encodings(reader, child_end, &mut encodings)?;
            }
            _ => {}
        }
        if stream_pos(reader)? != child_end {
            reader.seek(SeekFrom::Start(child_end))?;
        }
    }

    if track_type != TRACK_TYPE_SUBTITLE || track_number == 0 {
        return Ok(None);
    }

    let codec = MkvSubtitleCodec::from_codec_id(&codec_id);
    Ok(Some(InternalTrackMeta {
        info: MkvSubtitleTrackInfo {
            track_number,
            subtitle_index,
            codec_id,
            codec,
            name,
            language: language_bcp47.or(language),
            default,
            forced,
            codec_private,
        },
        encodings,
    }))
}

fn parse_content_encodings<R: Read + Seek>(
    reader: &mut R,
    end_pos: u64,
    out: &mut Vec<TrackEncoding>,
) -> io::Result<()> {
    while stream_pos(reader)? < end_pos {
        let Some(hdr) = read_element_header(reader, end_pos)? else {
            break;
        };
        let child_end = element_end_pos(hdr.data_start, hdr.size, end_pos);
        if hdr.id == ID_CONTENT_ENCODING {
            let mut scope: u64 = 1;
            let mut enc_type: u64 = 0;
            let mut compression: Option<ContentCompression> = None;

            while stream_pos(reader)? < child_end {
                let Some(sub) = read_element_header(reader, child_end)? else {
                    break;
                };
                let sub_end = element_end_pos(sub.data_start, sub.size, child_end);
                let sz = sub_end.saturating_sub(sub.data_start);
                match sub.id {
                    ID_CONTENT_ENCODING_SCOPE if sz <= 8 => {
                        scope = read_uint(reader, sz as usize)?;
                    }
                    ID_CONTENT_ENCODING_TYPE if sz <= 8 => {
                        enc_type = read_uint(reader, sz as usize)?;
                    }
                    ID_CONTENT_COMPRESSION => {
                        compression = Some(parse_content_compression(reader, sub_end)?);
                    }
                    _ => {}
                }
                if stream_pos(reader)? != sub_end {
                    reader.seek(SeekFrom::Start(sub_end))?;
                }
            }
            if enc_type == 0 {
                out.push(TrackEncoding {
                    scope,
                    compression: compression.unwrap_or(ContentCompression::Zlib),
                });
            }
        }
        if stream_pos(reader)? != child_end {
            reader.seek(SeekFrom::Start(child_end))?;
        }
    }
    Ok(())
}

fn parse_content_compression<R: Read + Seek>(
    reader: &mut R,
    end_pos: u64,
) -> io::Result<ContentCompression> {
    let mut algo: u64 = 0; // Matroska default is 0 (zlib).
    let mut settings: Vec<u8> = Vec::new();
    while stream_pos(reader)? < end_pos {
        let Some(hdr) = read_element_header(reader, end_pos)? else {
            break;
        };
        let child_end = element_end_pos(hdr.data_start, hdr.size, end_pos);
        let sz = child_end.saturating_sub(hdr.data_start);
        match hdr.id {
            ID_CONTENT_COMP_ALGO if sz <= 8 => algo = read_uint(reader, sz as usize)?,
            ID_CONTENT_COMP_SETTINGS if sz <= 4096 => {
                settings = read_exact_vec(reader, sz as usize)?;
            }
            _ => {}
        }
        if stream_pos(reader)? != child_end {
            reader.seek(SeekFrom::Start(child_end))?;
        }
    }
    Ok(match algo {
        0 => ContentCompression::Zlib,
        3 => ContentCompression::HeaderStripping(settings),
        other => ContentCompression::Unsupported(other),
    })
}

fn apply_track_encodings(
    data: &[u8],
    encodings: &[TrackEncoding],
    scope_mask: u64,
    max_output_bytes: u64,
) -> io::Result<Vec<u8>> {
    let mut buf = data.to_vec();
    // Matroska specifies decoding in reverse order of ContentEncodingOrder.
    for enc in encodings.iter().rev() {
        if (enc.scope & scope_mask) == 0 {
            continue;
        }
        match &enc.compression {
            ContentCompression::Zlib => {
                let mut decoder =
                    ZlibDecoder::new(buf.as_slice()).take(max_output_bytes.saturating_add(1));
                let mut out = Vec::new();
                decoder.read_to_end(&mut out)?;
                if out.len() as u64 > max_output_bytes {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "Matroska zlib payload exceeded limit ({max_output_bytes} bytes)"
                        ),
                    ));
                }
                buf = out;
            }
            ContentCompression::HeaderStripping(prefix) => {
                let total = (prefix.len() as u64).saturating_add(buf.len() as u64);
                if total > max_output_bytes {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "Matroska header-stripped payload exceeded limit ({max_output_bytes} bytes)"
                        ),
                    ));
                }
                let mut out = Vec::with_capacity(prefix.len() + buf.len());
                out.extend_from_slice(prefix);
                out.extend_from_slice(&buf);
                buf = out;
            }
            ContentCompression::Unsupported(algo) => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!("Unsupported Matroska ContentCompAlgo {algo}"),
                ));
            }
        }
    }
    Ok(buf)
}

fn parse_attachments<R: Read + Seek>(
    reader: &mut R,
    end_pos: u64,
    out: &mut Vec<MkvFontAttachment>,
) -> io::Result<()> {
    while stream_pos(reader)? < end_pos {
        let Some(hdr) = read_element_header(reader, end_pos)? else {
            break;
        };
        let child_end = element_end_pos(hdr.data_start, hdr.size, end_pos);
        if hdr.id == ID_ATTACHED_FILE {
            let mut file_name = String::new();
            let mut mime_type = String::new();
            let mut data_span: Option<(u64, u64)> = None;

            while stream_pos(reader)? < child_end {
                let Some(sub) = read_element_header(reader, child_end)? else {
                    break;
                };
                let sub_end = element_end_pos(sub.data_start, sub.size, child_end);
                let sz = sub_end.saturating_sub(sub.data_start);
                match sub.id {
                    ID_FILE_NAME if sz <= 4096 => file_name = read_utf8(reader, sz as usize)?,
                    ID_FILE_MIME_TYPE if sz <= 512 => mime_type = read_utf8(reader, sz as usize)?,
                    ID_FILE_DATA if sz > 0 && sz <= MAX_FONT_BYTES => {
                        data_span = Some((sub.data_start, sz));
                    }
                    _ => {}
                }
                if stream_pos(reader)? != sub_end {
                    reader.seek(SeekFrom::Start(sub_end))?;
                }
            }

            if is_font_attachment(&file_name, &mime_type) {
                if let Some((offset, sz)) = data_span {
                    reader.seek(SeekFrom::Start(offset))?;
                    let data = read_exact_vec(reader, sz as usize)?;
                    out.push(MkvFontAttachment {
                        file_name,
                        mime_type,
                        data,
                    });
                }
            }
        }
        if stream_pos(reader)? != child_end {
            reader.seek(SeekFrom::Start(child_end))?;
        }
    }
    Ok(())
}

fn is_font_attachment(file_name: &str, mime_type: &str) -> bool {
    let mime = mime_type.trim().to_ascii_lowercase();
    if mime.starts_with("font/")
        || matches!(
            mime.as_str(),
            "application/x-truetype-font"
                | "application/vnd.ms-opentype"
                | "application/font-sfnt"
                | "application/font-ttf"
                | "application/font-otf"
                | "application/x-font-ttf"
                | "application/x-font-otf"
        )
    {
        return true;
    }
    let lower = file_name.trim().to_ascii_lowercase();
    [".ttf", ".otf", ".ttc", ".otc", ".woff", ".woff2"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

fn parse_cluster_for_track(
    reader: &mut TrackedReader,
    cluster_end: u64,
    timestamp_scale: u64,
    target: &InternalTrackMeta,
    cues: &mut Vec<MkvSubtitleCue>,
) -> io::Result<()> {
    let mut cluster_ts: u64 = 0;

    while stream_pos(reader)? < cluster_end {
        let elem_start = stream_pos(reader)?;
        let Some(hdr) = read_element_header(reader, cluster_end)? else {
            break;
        };

        // If this Cluster had unknown size, encountering another top-level Segment child
        // terminates the Cluster cleanly: rewind so the outer Segment loop sees it.
        if is_top_level_segment_element(hdr.id) {
            reader.seek(SeekFrom::Start(elem_start))?;
            break;
        }

        let child_end = element_end_pos(hdr.data_start, hdr.size, cluster_end);
        let sz = child_end.saturating_sub(hdr.data_start);

        match hdr.id {
            ID_CLUSTER_TIMESTAMP if sz <= 8 => {
                cluster_ts = read_uint(reader, sz as usize)?;
            }
            ID_SIMPLE_BLOCK if sz >= 4 && sz <= MAX_BLOCK_BYTES => {
                // Fast-path skip: read only the leading TrackNumber VINT before touching
                // multi-kilobyte video/audio frame payloads!
                if let Some((trk, vint_len)) = read_data_vint(reader)? {
                    if vint_len <= sz as usize {
                        if trk == target.info.track_number {
                            let rem = (sz as usize) - vint_len;
                            let rest = read_exact_vec(reader, rem)?;
                            decode_block_rest(
                                &rest,
                                cluster_ts,
                                None,
                                timestamp_scale,
                                target,
                                cues,
                            );
                        } else {
                            let skip = (child_end as i64) - (stream_pos(reader)? as i64);
                            if skip != 0 {
                                reader.seek_relative(skip)?;
                            }
                        }
                        continue;
                    }
                }
            }
            ID_BLOCK_GROUP => {
                parse_block_group(reader, child_end, cluster_ts, timestamp_scale, target, cues)?;
            }
            _ => {}
        }

        let cur = stream_pos(reader)?;
        if cur != child_end {
            let skip = (child_end as i64) - (cur as i64);
            reader.seek_relative(skip)?;
        }
    }
    Ok(())
}

fn parse_block_group(
    reader: &mut TrackedReader,
    group_end: u64,
    cluster_ts: u64,
    timestamp_scale: u64,
    target: &InternalTrackMeta,
    cues: &mut Vec<MkvSubtitleCue>,
) -> io::Result<()> {
    let mut matching_block: Option<Vec<u8>> = None;
    let mut duration_ticks: Option<u64> = None;

    while stream_pos(reader)? < group_end {
        let Some(hdr) = read_element_header(reader, group_end)? else {
            break;
        };
        let child_end = element_end_pos(hdr.data_start, hdr.size, group_end);
        let sz = child_end.saturating_sub(hdr.data_start);

        match hdr.id {
            ID_BLOCK if sz >= 4 && sz <= MAX_BLOCK_BYTES => {
                if let Some((trk, vint_len)) = read_data_vint(reader)? {
                    if vint_len <= sz as usize && trk == target.info.track_number {
                        let rem = (sz as usize) - vint_len;
                        matching_block = Some(read_exact_vec(reader, rem)?);
                        continue;
                    }
                }
            }
            ID_BLOCK_DURATION if sz <= 8 => {
                duration_ticks = Some(read_uint(reader, sz as usize)?);
            }
            _ => {}
        }

        let cur = stream_pos(reader)?;
        if cur != child_end {
            let skip = (child_end as i64) - (cur as i64);
            reader.seek_relative(skip)?;
        }
    }

    if let Some(rest) = matching_block {
        decode_block_rest(
            &rest,
            cluster_ts,
            duration_ticks,
            timestamp_scale,
            target,
            cues,
        );
    }
    Ok(())
}

fn decode_block_rest(
    rest: &[u8],
    cluster_ts: u64,
    duration_ticks: Option<u64>,
    timestamp_scale: u64,
    target: &InternalTrackMeta,
    cues: &mut Vec<MkvSubtitleCue>,
) {
    // `rest` starts immediately after TrackNumber VINT:
    // bytes 0..2: signed big-endian i16 relative timestamp
    // byte  2:    flags (bits 1..2 = lacing)
    // bytes 3..:  frame payload(s)
    if rest.len() < 3 {
        return;
    }
    let rel_ts = i16::from_be_bytes([rest[0], rest[1]]) as i64;
    let flags = rest[2];
    let lacing = (flags >> 1) & 0x03;

    let start_ticks = (cluster_ts as i64).saturating_add(rel_ts);
    let start_ms = ticks_to_ms(start_ticks, timestamp_scale);
    let dur_ms = duration_ticks
        .map(|d| ticks_to_ms(d as i64, timestamp_scale))
        .unwrap_or(0);
    let end_ms = start_ms + dur_ms;

    let frames: Vec<&[u8]> = if lacing == 0 {
        vec![&rest[3..]]
    } else {
        split_laced_frames(&rest[3..], lacing)
    };

    for raw_frame in frames {
        let decoded_owned;
        let payload: &[u8] = if target.encodings.is_empty() {
            raw_frame
        } else {
            match apply_track_encodings(raw_frame, &target.encodings, 1, MAX_BLOCK_BYTES) {
                Ok(v) => {
                    decoded_owned = v;
                    &decoded_owned
                }
                Err(_) => continue,
            }
        };

        let text = String::from_utf8_lossy(payload);
        if target.info.codec.is_ass_or_ssa() {
            if let Some(cue) = parse_mkv_ass_block(&text, start_ms, end_ms) {
                cues.push(cue);
            }
        } else {
            let normalized = normalize_plain_subtitle_text(&text);
            cues.push(MkvSubtitleCue {
                read_order: cues.len() as i64,
                layer: 0,
                start_ms,
                end_ms,
                style: String::new(),
                name: String::new(),
                margin_l: 0,
                margin_r: 0,
                margin_v: 0,
                effect: String::new(),
                plain_text: normalized.clone(),
                raw_text: normalized,
            });
        }
    }
}

/// Parse a Matroska `S_TEXT/ASS` or `S_TEXT/SSA` block payload:
/// `ReadOrder,Layer,Style,Name,MarginL,MarginR,MarginV,Effect,Text`
fn parse_mkv_ass_block(payload: &str, start_ms: i64, end_ms: i64) -> Option<MkvSubtitleCue> {
    let payload = payload.trim_end_matches('\0');
    let mut parts = payload.splitn(9, ',');
    let read_order: i64 = parts.next()?.trim().parse().unwrap_or(0);
    let layer_str = parts.next()?.trim();
    let layer: i32 = layer_str
        .strip_prefix("Marked=")
        .unwrap_or(layer_str)
        .parse()
        .unwrap_or(0);
    let style = parts.next()?.trim().to_string();
    let name = parts.next()?.trim().to_string();
    let margin_l: i32 = parts.next()?.trim().parse().unwrap_or(0);
    let margin_r: i32 = parts.next()?.trim().parse().unwrap_or(0);
    let margin_v: i32 = parts.next()?.trim().parse().unwrap_or(0);
    let effect = parts.next()?.trim().to_string();
    let raw_text = parts
        .next()
        .unwrap_or("")
        .trim_end_matches(['\r', '\n'])
        .to_string();
    let plain_text = normalize_plain_subtitle_text(&strip_ass_dialogue_text(&raw_text));

    Some(MkvSubtitleCue {
        read_order,
        layer,
        start_ms,
        end_ms,
        style,
        name,
        margin_l,
        margin_r,
        margin_v,
        effect,
        raw_text,
        plain_text,
    })
}

fn split_laced_frames<'a>(data: &'a [u8], lacing: u8) -> Vec<&'a [u8]> {
    if data.is_empty() {
        return Vec::new();
    }
    let frame_count = data[0] as usize + 1;
    let body = &data[1..];
    match lacing {
        // Fixed-size lacing
        2 => {
            if frame_count == 0 || body.is_empty() || body.len() % frame_count != 0 {
                return Vec::new();
            }
            let chunk = body.len() / frame_count;
            if chunk == 0 {
                return Vec::new();
            }
            body.chunks(chunk).collect()
        }
        // Xiph lacing
        1 => {
            let mut pos = 0usize;
            let mut sizes = Vec::with_capacity(frame_count);
            for _ in 0..frame_count.saturating_sub(1) {
                let mut sz = 0usize;
                loop {
                    if pos >= body.len() {
                        return Vec::new();
                    }
                    let b = body[pos] as usize;
                    pos += 1;
                    sz += b;
                    if b < 255 {
                        break;
                    }
                }
                sizes.push(sz);
            }
            slice_frames_by_sizes(&body[pos..], &sizes)
        }
        // EBML lacing
        3 => {
            let mut pos = 0usize;
            let mut sizes = Vec::with_capacity(frame_count);
            let Some((first_sz, len)) = decode_vint_slice(&body[pos..]) else {
                return Vec::new();
            };
            pos += len;
            let mut prev = first_sz as i64;
            sizes.push(first_sz as usize);

            for _ in 1..frame_count.saturating_sub(1) {
                let Some((raw, l)) = decode_vint_slice(&body[pos..]) else {
                    return Vec::new();
                };
                pos += l;
                let bias = (1i64 << (7 * l - 1)) - 1;
                prev += raw as i64 - bias;
                if prev < 0 {
                    return Vec::new();
                }
                sizes.push(prev as usize);
            }
            slice_frames_by_sizes(&body[pos..], &sizes)
        }
        _ => vec![data],
    }
}

fn slice_frames_by_sizes<'a>(mut rem: &'a [u8], sizes: &[usize]) -> Vec<&'a [u8]> {
    let mut frames = Vec::with_capacity(sizes.len() + 1);
    for &sz in sizes {
        if sz > rem.len() {
            return Vec::new();
        }
        let (head, tail) = rem.split_at(sz);
        frames.push(head);
        rem = tail;
    }
    frames.push(rem);
    frames
}

fn decode_vint_slice(buf: &[u8]) -> Option<(u64, usize)> {
    let first = *buf.first()?;
    if first == 0 {
        return None;
    }
    let lz = first.leading_zeros() as usize;
    let len = lz + 1;
    if len > 8 || buf.len() < len {
        return None;
    }
    let mask = if lz == 7 { 0 } else { (1u8 << (7 - lz)) - 1 };
    let mut val = (first & mask) as u64;
    for &b in &buf[1..len] {
        val = (val << 8) | b as u64;
    }
    Some((val, len))
}

#[inline]
fn ticks_to_ms(ticks: i64, timestamp_scale: u64) -> i64 {
    let ns = (ticks as i128).saturating_mul(timestamp_scale as i128);
    (ns / 1_000_000) as i64
}

#[inline]
fn is_top_level_segment_element(id: u32) -> bool {
    matches!(
        id,
        ID_SEEK_HEAD
            | ID_INFO
            | ID_TRACKS
            | ID_ATTACHMENTS
            | ID_CLUSTER
            | 0x1C53_BB6B // Cues
            | 0x1043_A770 // Chapters
            | 0x1254_C367 // Tags
    )
}

fn read_element_header<R: Read + Seek>(
    reader: &mut R,
    limit: u64,
) -> io::Result<Option<ElementHeader>> {
    let start = stream_pos(reader)?;
    if start >= limit {
        return Ok(None);
    }

    let Some(id) = read_element_id(reader)? else {
        return Ok(None);
    };
    if stream_pos(reader)? >= limit {
        return Ok(None);
    }
    let Some(size) = read_element_size(reader)? else {
        return Ok(None);
    };
    let data_start = stream_pos(reader)?;
    if data_start > limit {
        return Ok(None);
    }
    Ok(Some(ElementHeader {
        id,
        size,
        data_start,
    }))
}

fn read_element_id<R: Read>(reader: &mut R) -> io::Result<Option<u32>> {
    let mut first = [0u8; 1];
    match reader.read(&mut first)? {
        0 => return Ok(None),
        _ => {}
    }
    let b0 = first[0];
    if b0 == 0 {
        return Ok(None);
    }
    let lz = b0.leading_zeros() as usize;
    if lz > 3 {
        return Ok(None);
    }
    let mut id = b0 as u32;
    if lz > 0 {
        let mut rest = [0u8; 3];
        if reader.read_exact(&mut rest[..lz]).is_err() {
            return Ok(None);
        }
        for &b in &rest[..lz] {
            id = (id << 8) | b as u32;
        }
    }
    Ok(Some(id))
}

fn read_element_size<R: Read>(reader: &mut R) -> io::Result<Option<Option<u64>>> {
    let Some((val, len)) = read_data_vint(reader)? else {
        return Ok(None);
    };
    let all_ones = (1u64 << (7 * len)) - 1;
    if val == all_ones {
        Ok(Some(None)) // Unknown-size sentinel (e.g. Segment / Cluster).
    } else {
        Ok(Some(Some(val)))
    }
}

fn read_data_vint<R: Read>(reader: &mut R) -> io::Result<Option<(u64, usize)>> {
    let mut first = [0u8; 1];
    match reader.read(&mut first)? {
        0 => return Ok(None),
        _ => {}
    }
    let b0 = first[0];
    if b0 == 0 {
        return Ok(None);
    }
    let lz = b0.leading_zeros() as usize;
    let len = lz + 1;
    if len > 8 {
        return Ok(None);
    }
    let mask = if lz == 7 { 0 } else { (1u8 << (7 - lz)) - 1 };
    let mut val = (b0 & mask) as u64;
    if lz > 0 {
        let mut rest = [0u8; 7];
        if reader.read_exact(&mut rest[..lz]).is_err() {
            return Ok(None);
        }
        for &b in &rest[..lz] {
            val = (val << 8) | b as u64;
        }
    }
    Ok(Some((val, len)))
}

fn read_uint<R: Read>(reader: &mut R, len: usize) -> io::Result<u64> {
    if len == 0 {
        return Ok(0);
    }
    let mut buf = [0u8; 8];
    reader.read_exact(&mut buf[..len])?;
    let mut v = 0u64;
    for &b in &buf[..len] {
        v = (v << 8) | b as u64;
    }
    Ok(v)
}

fn read_exact_vec<R: Read>(reader: &mut R, len: usize) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    Ok(buf)
}

fn read_utf8<R: Read>(reader: &mut R, len: usize) -> io::Result<String> {
    let buf = read_exact_vec(reader, len)?;
    let trimmed = buf
        .iter()
        .position(|&b| b == 0)
        .map_or(buf.as_slice(), |idx| &buf[..idx]);
    Ok(String::from_utf8_lossy(trimmed).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ass_block_with_commas_in_text() {
        let cue = parse_mkv_ass_block(
            "42,0,Default,Shiroha,0,0,0,,{\\pos(480,502)}Hello, world, 鳥白島!",
            18_920,
            22_420,
        )
        .unwrap();
        assert_eq!(cue.read_order, 42);
        assert_eq!(cue.layer, 0);
        assert_eq!(cue.style, "Default");
        assert_eq!(cue.name, "Shiroha");
        assert_eq!(cue.start_ms, 18_920);
        assert_eq!(cue.end_ms, 22_420);
        assert_eq!(cue.raw_text, "{\\pos(480,502)}Hello, world, 鳥白島!");
        assert_eq!(cue.plain_text, "Hello, world, 鳥白島!");
    }

    #[test]
    fn font_attachment_detection_handles_mime_and_ext() {
        assert!(is_font_attachment("OpenSans-Bold.ttf", "application/x-truetype-font"));
        assert!(is_font_attachment("custom.otf", "application/octet-stream"));
        assert!(is_font_attachment("weird_name", "font/ttf"));
        assert!(!is_font_attachment("cover.jpg", "image/jpeg"));
    }

    #[test]
    fn zlib_and_header_stripping_encodings() {
        use flate2::{write::ZlibEncoder, Compression};
        use std::io::Write;

        let original = b"0,0,Default,,0,0,0,,Test";
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(original).unwrap();
        let compressed = enc.finish().unwrap();

        let decoded = apply_track_encodings(
            &compressed,
            &[TrackEncoding {
                scope: 1,
                compression: ContentCompression::Zlib,
            }],
            1,
            MAX_BLOCK_BYTES,
        )
        .unwrap();
        assert_eq!(decoded, original);

        // Exceeding max_output_bytes must return InvalidData instead of unbounded OOM.
        let bomb_err = apply_track_encodings(
            &compressed,
            &[TrackEncoding {
                scope: 1,
                compression: ContentCompression::Zlib,
            }],
            1,
            8,
        )
        .unwrap_err();
        assert_eq!(bomb_err.kind(), io::ErrorKind::InvalidData);

        let stripped = apply_track_encodings(
            b"World",
            &[TrackEncoding {
                scope: 1,
                compression: ContentCompression::HeaderStripping(b"Hello ".to_vec()),
            }],
            1,
            MAX_BLOCK_BYTES,
        )
        .unwrap();
        assert_eq!(stripped, b"Hello World");
    }

    #[test]
    fn malformed_lacing_and_truncated_vint_header_do_not_panic() {
        // Fixed-size lacing with empty body must return empty vec instead of chunks(0) panic.
        assert!(split_laced_frames(&[0x01], 2).is_empty());
        assert!(split_laced_frames(&[0x00], 2).is_empty());

        // Truncated multi-byte size VINT overshooting parent limit must return Ok(None).
        let buf: &[u8] = &[0xD7, 0x40, 0x05]; // 1-byte ID + 2-byte size VINT (3 bytes total)
        let mut cursor = io::Cursor::new(buf);
        // Limit=2 cuts right through the 2-byte size VINT so data_start (3) > limit (2).
        let hdr = read_element_header(&mut cursor, 2).unwrap();
        assert!(hdr.is_none());
    }

    #[test]
    fn normalizes_crlf_and_blank_lines_and_deduplicates_exact_packets() {
        assert_eq!(
            normalize_plain_subtitle_text("あっ。\r\n勝った勝った！\0"),
            "あっ。\n勝った勝った！"
        );
        assert_eq!(
            normalize_plain_subtitle_text("\r\n\nあっ。\n\n  \n勝った勝った！\r\n\n"),
            "あっ。\n勝った勝った！"
        );
        assert_eq!(
            normalize_plain_subtitle_text("勝ちました！\u{3000}エヘヘヘ！"),
            "勝ちました！\u{3000}エヘヘヘ！"
        );

        let make_cue = |read_order, start_ms, end_ms, raw: &str, plain: &str| MkvSubtitleCue {
            read_order,
            layer: 0,
            start_ms,
            end_ms,
            style: "Default".to_string(),
            name: String::new(),
            margin_l: 0,
            margin_r: 0,
            margin_v: 0,
            effect: String::new(),
            raw_text: raw.to_string(),
            plain_text: plain.to_string(),
        };

        // SRT: exact duplicate packet within 10ms collapses to 1 cue.
        let mut srt = vec![
            make_cue(0, 926_526, 929_028, "あっ。\n勝った勝った！", "あっ。\n勝った勝った！"),
            make_cue(1, 926_530, 929_030, "あっ。\n勝った勝った！", "あっ。\n勝った勝った！"),
            make_cue(2, 929_028, 931_197, "勝ちました！", "勝ちました！"),
        ];
        dedup_extracted_cues(&mut srt, false);
        assert_eq!(srt.len(), 2);

        // ASS: distinct \pos events at the exact same timestamp are preserved for libass,
        // while exact duplicate ASS packets are collapsed.
        let mut ass = vec![
            make_cue(0, 458_520, 461_860, "{\\pos(212,437)}えっ？", "えっ？"),
            make_cue(1, 458_520, 461_860, "{\\pos(672,497)}えっ？", "えっ？"),
            make_cue(2, 458_520, 461_860, "{\\pos(672,497)}えっ？", "えっ？"),
        ];
        dedup_extracted_cues(&mut ass, true);
        assert_eq!(ass.len(), 2);
        assert_eq!(ass[0].raw_text, "{\\pos(212,437)}えっ？");
        assert_eq!(ass[1].raw_text, "{\\pos(672,497)}えっ？");
    }
}
