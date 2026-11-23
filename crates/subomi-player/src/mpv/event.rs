//! Ordered events drained from libmpv.

use super::types::{CommandReplyResult, EndFileReason, MpvPropertyName, MpvPropertyValue};

/// Ordered event drained from `mpv_wait_event(0.0)`.
#[derive(Debug, Clone, PartialEq)]
pub enum MpvEvent {
    StartFile {
        playlist_entry_id: i64,
    },
    FileLoaded {
        video_width: i64,
        video_height: i64,
    },
    EndFile {
        reason: EndFileReason,
        playlist_entry_id: i64,
    },
    PlaybackRestart,
    Seek,
    Shutdown,
    QueueOverflow,
    CommandReply {
        reply_userdata: u64,
        error: i32,
        result: CommandReplyResult,
    },
    PropertyChange {
        name: MpvPropertyName,
        value: MpvPropertyValue,
    },
}
