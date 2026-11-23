//! `subomi-config` — the Qt-free settings persistence engine of the
//! pure-Rust Subomi.
//!
//! Layering (top to bottom):
//!
//! * [`error`] — every failure surface of the engine;
//! * [`value`] — the [`Value`] model mirroring what Subomi stores through
//!   `QSettings`, with strict [`FromValue`]/[`IntoValue`] conversions;
//! * [`keys`] — the idiomatic typed key model over the certified
//!   `subomi_setting` constants ([`keys::raw`]), including
//!   environment-dependent default handling ([`keys::DefaultContext`]);
//! * [`canonical`] — the diffable `subomi-config-store v1` file format and
//!   the scalar codec shared with the legacy loader;
//! * [`legacy_ini`] — the QSettings `IniFormat` interop loader (the path
//!   off Qt for existing installs);
//! * [`config`] — the store itself: typed get/set with dirty tracking,
//!   transactions, synchronous observers, atomic save with a change-log,
//!   and the `put_at`/`get_at`/`remove_at` migration data plane.
//!
//! All defaults come from the certified `subomi_setting` constants; the
//! two Qt-dependent defaults (`FILE_OPEN_CUSTOM`'s `file:// + home`
//! runtime value and the `QFont` typography defaults) are handled exactly
//! as documented there. No filesystem paths are hard-coded: the config
//! directory is a call parameter everywhere.

pub mod canonical;
pub mod config;
pub mod error;
pub mod keys;
pub mod legacy_ini;
pub mod ui_types;
pub mod value;

pub use config::{
    Change, ChangeKind, ChangeSet, ChangelogEntry, Config, RawDocument, Subscription,
    Transaction,
};
pub use error::{ConfigError, ConfigResult};
pub use keys::{
    file_open_custom_default, lookup, ApplicationField, AsrField, AudioSourceField,
    BehaviorField, DefaultContext, DictionaryField, InterfaceField, InterfaceSearchFontField,
    InterfaceSubtitleField, InterfaceSubtitleListField, InternalField, Key, KeyPath,
    KeybindProfileField, OcrField, OnnxField, RecentField, SearchField, SearchMatcherField,
    SubtitleListField, WhisperField, WindowField, GENERAL_GROUP, KEYBIND_PROFILES_PREFIX,
};
pub use legacy_ini::{parse_legacy_ini, LegacyIni};
pub use ui_types::{ArgbColor, FontSpec};
pub use value::{FromValue, IntoValue, Value, ValueType};
