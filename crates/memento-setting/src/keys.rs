////////////////////////////////////////////////////////////////////////////////
//
// Copyright (c) 2026 Ripose
//
// This file is part of Memento.
//
// Memento is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, version 2 of the License.
//
// Memento is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with Memento.  If not, see <https://www.gnu.org/licenses/>.
//
////////////////////////////////////////////////////////////////////////////////

//! Rust port of the `Keys` namespace constants of `src/setting/keys.h`.
//!
//! Module structure mirrors the C++ nesting; every constant keeps its exact
//! C++ SCREAMING_SNAKE name, and comments anchor each declaration to its
//! `keys.h` line. Assertions against the literal C++ strings live in the
//! `#[cfg(test)]` module at the bottom of this file.
//!
//! ## Excluded on purpose (C++-only, stays shell-side)
//!
//! * keys.h:192-194
//!   `Keys::Behavior::FILE_OPEN_CUSTOM_DEFAULT`
//!   (`"file://" + QStandardPaths::writableLocation(HomeLocation)`) — a
//!   runtime `QString` built from QStandardPaths, not pure data.
//! * keys.h:272
//!   `Keys::Interface::Subtitle::FONT_DEFAULT` — `QFont("Noto Sans CJK JP", 24)`.
//! * keys.h:309-316
//!   `Keys::Interface::SEARCH_EXPRESSION_FONT_DEFAULT` — `QFont`, with a
//!   `Q_OS_MACOS` size switch (28 vs 20).
//! * keys.h:319-326
//!   `Keys::Interface::SEARCH_READING_FONT_DEFAULT` — `QFont` (16 vs 12).
//! * keys.h:329-336
//!   `Keys::Interface::SEARCH_CONJ_FONT_DEFAULT` — `QFont` (16 vs 12).
//! * keys.h:339-347
//!   `Keys::Interface::SEARCH_TAG_FONT_DEFAULT` — `QFont` (14 vs 10,
//!   `QFont::Bold`).
//! * keys.h:350-357
//!   `Keys::Interface::SEARCH_GLOSSARY_FONT_DEFAULT` — `QFont` (14 vs 10).
//! * keys.h:360-367
//!   `Keys::Interface::SEARCH_KANJI_FONT_DEFAULT` —
//!   `QFont("Kanji Stroke Orders", 156 vs 108)`.
//! * keys.h:393-400
//!   `Keys::Interface::SubtitleList::PRIMARY_FONT_DEFAULT` — `QFont` (20 vs 16).
//! * keys.h:403-410
//!   `Keys::Interface::SubtitleList::SECONDARY_FONT_DEFAULT` —
//!   `QFont("Sans Serif", 20 vs 16)`.
//! * keys.h:471-475
//!   `Keys::Asr::BACKEND_DEFAULT` is not a plain constant; it is a
//!   conditional expression over the CMake feature defines
//!   `HAVE_ASR_BACKEND_WHISPER` / `HAVE_ASR_BACKEND_ONNX`. The pure selection
//!   rule is ported as [`asr_backend_default`]; actual backend availability
//!   is resolved from build configuration on the C++ side.

pub mod version {
    /// keys.h:87
    pub const VERSION: &str = "version";
    /// keys.h:88
    pub const CURRENT: u32 = 6;
}

pub mod window {
    /// keys.h:93
    pub const GROUP: &str = "main-window";

    /// keys.h:95
    pub const SUBTITLE_LIST: &str = "subtitle-list";
    /// keys.h:96
    pub const SEARCH: &str = "search";
}

pub mod internal {
    /// keys.h:101
    pub const GROUP: &str = "internal";

    /// keys.h:103
    pub const AUTO_UPDATE_OPT_IN_SHOWN: &str = "auto-update-opt-in-shown";
    /// keys.h:104-105
    pub const WHISPER_NO_MODEL_PROMPT_DISMISSED: &str =
        "whisper-no-model-prompt-dismissed";
}

pub mod recent {
    /// keys.h:110
    pub const GROUP: &str = "recent";

    /// keys.h:112
    pub const FILES: &str = "files";
}

pub mod subtitle_list {
    /// `Keys::SubtitleList` (keys.h:115-124).
    ///
    /// keys.h:117
    pub const GROUP: &str = "subtitle-list";

    /// keys.h:119
    pub const IGNORE_WHITESPACE: &str = "ignore-whitespace";
    /// keys.h:120
    pub const IGNORE_WHITESPACE_DEFAULT: bool = false;

    /// keys.h:122
    pub const AUTO_SEEK: &str = "auto-seek";
    /// keys.h:123
    pub const AUTO_SEEK_DEFAULT: bool = false;
}

pub mod audio_source {
    use crate::enums::AudioSourceType;

    /// `Keys::AudioSource` (keys.h:126-141).
    ///
    /// keys.h:128
    pub const GROUP: &str = "audio-srcs";

    /// keys.h:130
    pub const NAME: &str = "name";
    /// keys.h:131
    pub const NAME_DEFAULT: &str = "JapanesePod101";

    /// keys.h:133
    pub const URL: &str = "url";
    /// keys.h:134
    pub const URL_DEFAULT: &str = "https://assets.languagepod101.com/dictionary/japanese/audiomp3.php?kanji={expression}&kana={reading}";

    /// keys.h:136
    pub const TYPE: &str = "type";
    /// keys.h:137
    pub const TYPE_DEFAULT: AudioSourceType = AudioSourceType::AudioSourceTypeFile;

    /// keys.h:139
    pub const MD5: &str = "md5";
    /// keys.h:140
    pub const MD5_DEFAULT: &str = "7e2c2f954ef6051373ba916f000168dc";
}

pub mod application {
    use crate::enums::Language;

    /// `Keys::Application` (keys.h:143-152).
    ///
    /// keys.h:145
    pub const GROUP: &str = "application";

    /// keys.h:147
    pub const LANGUAGE: &str = "language";
    /// keys.h:148
    pub const LANGUAGE_DEFAULT: Language = Language::LanguageSystem;

    /// keys.h:150
    pub const AUTO_UPDATE_CHECK: &str = "auto-update-check";
    /// keys.h:151
    pub const AUTO_UPDATE_CHECK_DEFAULT: bool = false;
}

pub mod behavior {
    use crate::enums::Directory;

    /// `Keys::Behavior` (keys.h:154-194).
    ///
    /// keys.h:156
    pub const GROUP: &str = "behavior";

    /// keys.h:158
    pub const AUTO_FIT: &str = "autofit";
    /// keys.h:159
    pub const AUTO_FIT_DEFAULT: bool = true;

    /// keys.h:161
    pub const AUTO_FIT_PERCENT: &str = "autofit-percent";
    /// keys.h:162
    pub const AUTO_FIT_PERCENT_DEFAULT: i32 = 100;

    /// keys.h:164
    pub const OSC_MPV_CURSOR: &str = "osc-mpv-cursor";
    /// keys.h:165
    pub const OSC_MPV_CURSOR_DEFAULT: bool = false;

    /// keys.h:167
    pub const OSC_DURATION: &str = "osc-duration";
    /// keys.h:168
    pub const OSC_DURATION_DEFAULT: i32 = 700;

    /// keys.h:170
    pub const OSC_FADE: &str = "osc-fade";
    /// keys.h:171
    pub const OSC_FADE_DEFAULT: i32 = 250;

    /// keys.h:173
    pub const OSC_MIN_MOVE: &str = "osc-min-move";
    /// keys.h:174
    pub const OSC_MIN_MOVE_DEFAULT: i32 = 0;

    /// keys.h:176
    pub const OSC_PREVIEW_THUMBNAILS: &str = "osc-preview-thumbnails";
    /// keys.h:177
    pub const OSC_PREVIEW_THUMBNAILS_DEFAULT: bool = true;

    /// keys.h:179
    pub const SUBTITLE_PAUSE: &str = "subtitle-pause";
    /// keys.h:180
    pub const SUBTITLE_PAUSE_DEFAULT: bool = false;

    /// keys.h:182
    pub const SUBTITLE_CURSOR_SHOW: &str = "subtitle-cursor-show";
    /// keys.h:183
    pub const SUBTITLE_CURSOR_SHOW_DEFAULT: bool = false;

    /// keys.h:185
    pub const SECONDARY_SUBTITLE_CURSOR_SHOW: &str = "subtitle-cursor-show-secondary";
    /// keys.h:186
    pub const SECONDARY_SUBTITLE_CURSOR_SHOW_DEFAULT: bool = false;

    /// keys.h:188
    pub const FILE_OPEN_DIR: &str = "file-open-dir";
    /// keys.h:189
    pub const FILE_OPEN_DIR_DEFAULT: Directory = Directory::DirectoryMovies;

    /// keys.h:191 — key name only.
    pub const FILE_OPEN_CUSTOM: &str = "file-open-custom";
    // keys.h:192-194 — FILE_OPEN_CUSTOM_DEFAULT is
    // "file://" + QStandardPaths::writableLocation(QStandardPaths::HomeLocation).
    // C++-only, stays shell-side (QStandardPaths).
}

pub mod dictionary {
    /// `Keys::Dictionary` (keys.h:196-201).
    ///
    /// keys.h:198
    pub const GROUP: &str = "dictionary";

    /// keys.h:200
    pub const ORDER: &str = "order";
}

pub mod search {
    use crate::enums::{GlossaryStyle, Modifier, SearchMethod};

    /// `Keys::Search` (keys.h:203-260).
    ///
    /// keys.h:205
    pub const GROUP: &str = "search";

    /// `Keys::Search::Matcher` (keys.h:207-217).
    pub mod matcher {
        /// keys.h:209
        pub const EXACT: &str = "exact-matcher";
        /// keys.h:210
        pub const EXACT_DEFAULT: bool = true;

        /// keys.h:212
        pub const DECONJ: &str = "deconj-matcher";
        /// keys.h:213
        pub const DECONJ_DEFAULT: bool = true;

        /// keys.h:215
        pub const MECAB_IPADIC: &str = "ipadic-matcher";
        /// keys.h:216
        pub const MECAB_IPADIC_DEFAULT: bool = true;
    }

    /// keys.h:219
    pub const MIDDLE_MOUSE_SCAN: &str = "middle-mouse-scan";
    /// keys.h:220
    pub const MIDDLE_MOUSE_SCAN_DEFAULT: bool = false;

    /// keys.h:222
    pub const METHOD: &str = "method";
    /// keys.h:223
    pub const METHOD_DEFAULT: SearchMethod = SearchMethod::SearchMethodHover;

    /// keys.h:225
    pub const DELAY: &str = "delay";
    /// keys.h:226
    pub const DELAY_DEFAULT: i32 = 250;

    /// keys.h:228
    pub const MODIFIER: &str = "modifier";
    /// keys.h:229
    pub const MODIFIER_DEFAULT: Modifier = Modifier::ModifierShift;

    /// keys.h:231
    pub const HIDE_MPV_SUBS: &str = "hide-subs";
    /// keys.h:232
    pub const HIDE_MPV_SUBS_DEFAULT: bool = true;

    /// keys.h:234
    pub const HIDE_SUBS: &str = "hide-bar";
    /// keys.h:235
    pub const HIDE_SUBS_DEFAULT: bool = false;

    /// keys.h:237
    pub const PAUSE_ON_HOVER: &str = "hover-pause";
    /// keys.h:238
    pub const PAUSE_ON_HOVER_DEFAULT: bool = false;

    /// keys.h:240
    pub const AUTO_PLAY_AUDIO: &str = "auto-play-audio";
    /// keys.h:241
    pub const AUTO_PLAY_AUDIO_DEFAULT: bool = false;

    /// keys.h:243
    pub const GLOSSARY_STYLE: &str = "list-result";
    /// keys.h:244
    pub const GLOSSARY_STYLE_DEFAULT: GlossaryStyle =
        GlossaryStyle::GlossaryStyleBullet;

    /// keys.h:246
    pub const REPLACE_NEWLINES: &str = "replace-lines";
    /// keys.h:247
    pub const REPLACE_NEWLINES_DEFAULT: bool = false;

    /// keys.h:249
    pub const REPLACE_NEWLINES_WITH: &str = "replace-with";
    /// keys.h:250
    pub const REPLACE_NEWLINES_WITH_DEFAULT: &str = "";

    /// keys.h:252
    pub const REMOVE_REGEX: &str = "remove-regex";
    /// keys.h:253
    pub const REMOVE_REGEX_DEFAULT: &str = "";

    /// keys.h:255
    pub const SUBTITLE_FURIGANA: &str = "subtitle-furigana";
    /// keys.h:256
    pub const SUBTITLE_FURIGANA_DEFAULT: bool = false;

    /// keys.h:258
    pub const SHOW_ANKI_GLOSSARY_CHECKBOX: &str = "show-anki-glossary-checkbox";
    /// keys.h:259
    pub const SHOW_ANKI_GLOSSARY_CHECKBOX_DEFAULT: bool = true;
}

pub mod interface {
    /// `Keys::Interface` (keys.h:262-412).
    ///
    /// keys.h:264
    pub const GROUP: &str = "interface";

    /// keys.h:266
    pub const SYSTEM_ICONS: &str = "system-icons";
    /// keys.h:267
    pub const SYSTEM_ICONS_DEFAULT: bool = true;

    /// `Keys::Interface::Subtitle` (keys.h:269-297).
    pub mod subtitle {
        /// keys.h:271
        pub const FONT: &str = "sub-font";
        // keys.h:272 — FONT_DEFAULT = QFont("Noto Sans CJK JP", 24).
        // C++-only, stays shell-side (QFont).

        /// keys.h:274
        pub const LINE_SPACING: &str = "sub-line-spacing";
        /// keys.h:275
        pub const LINE_SPACING_DEFAULT: f64 = 0.0;

        /// keys.h:277
        pub const SCALE: &str = "sub-scale";
        /// keys.h:278
        pub const SCALE_DEFAULT: f64 = 0.05;

        /// keys.h:280
        pub const OFFSET: &str = "sub-offset";
        /// keys.h:281
        pub const OFFSET_DEFAULT: f64 = 0.045;

        /// keys.h:283
        pub const STROKE: &str = "sub-stroke";
        /// keys.h:284
        pub const STROKE_DEFAULT: f64 = 15.0;

        /// keys.h:286
        pub const TEXT_COLOR: &str = "sub-text-color";
        /// keys.h:287
        pub const TEXT_COLOR_DEFAULT: &str = "#FFFFFFFF";

        /// keys.h:289
        pub const BACKGROUND_COLOR: &str = "sub-bg-color";
        /// keys.h:290
        pub const BACKGROUND_COLOR_DEFAULT: &str = "#00000000";

        /// keys.h:292
        pub const STROKE_COLOR: &str = "sub-stroke-color";
        /// keys.h:293
        pub const STROKE_COLOR_DEFAULT: &str = "#FF000000";

        /// keys.h:295
        pub const FURIGANA_RATIO: &str = "sub-furigana-ratio";
        /// keys.h:296
        pub const FURIGANA_RATIO_DEFAULT: f64 = 0.4;
    }

    /// keys.h:299
    pub const POPUP_WIDTH: &str = "sub-popup-width";
    /// keys.h:300
    pub const POPUP_WIDTH_DEFAULT: i32 = 500;

    /// keys.h:302
    pub const POPUP_HEIGHT: &str = "sub-popup-height";
    /// keys.h:303
    pub const POPUP_HEIGHT_DEFAULT: i32 = 400;

    /// keys.h:305
    pub const SEARCH_WINDOW: &str = "search-window";
    /// keys.h:306
    pub const SEARCH_WINDOW_DEFAULT: bool = false;

    // keys.h:308 — SEARCH_EXPRESSION_FONT key name (pure):
    /// keys.h:308
    pub const SEARCH_EXPRESSION_FONT: &str = "search-expression-font";
    // keys.h:309-316 — SEARCH_EXPRESSION_FONT_DEFAULT = QFont(..., 28 macOS
    // / 20 otherwise). C++-only, stays shell-side (QFont, Q_OS_MACOS).

    /// keys.h:318
    pub const SEARCH_READING_FONT: &str = "search-reading-font";
    // keys.h:319-326 — SEARCH_READING_FONT_DEFAULT. QFont; C++-only,
    // stays shell-side.

    /// keys.h:328
    pub const SEARCH_CONJ_FONT: &str = "search-conj-font";
    // keys.h:329-336 — SEARCH_CONJ_FONT_DEFAULT. QFont; C++-only,
    // stays shell-side.

    /// keys.h:338
    pub const SEARCH_TAG_FONT: &str = "search-tag-font";
    // keys.h:339-347 — SEARCH_TAG_FONT_DEFAULT (QFont, Bold). C++-only,
    // stays shell-side.

    /// keys.h:349
    pub const SEARCH_GLOSSARY_FONT: &str = "search-glossary-font";
    // keys.h:350-357 — SEARCH_GLOSSARY_FONT_DEFAULT. QFont; C++-only,
    // stays shell-side.

    /// keys.h:359
    pub const SEARCH_KANJI_FONT: &str = "search-kanji-font";
    // keys.h:360-367 — SEARCH_KANJI_FONT_DEFAULT. QFont; C++-only,
    // stays shell-side.

    /// `Keys::Interface::SubtitleList` (keys.h:369-411).
    pub mod subtitle_list {
        /// keys.h:371
        pub const WINDOW: &str = "sub-list-window";
        /// keys.h:372
        pub const WINDOW_DEFAULT: bool = false;

        /// keys.h:374
        pub const TIMESTAMPS: &str = "sub-list-timestamps";
        /// keys.h:375
        pub const TIMESTAMPS_DEFAULT: bool = false;

        /// keys.h:377
        pub const BACKGROUND_COLOR: &str = "sub-list-background-color";
        /// keys.h:378
        pub const BACKGROUND_COLOR_DEFAULT: &str = "#FF000000";

        /// keys.h:380
        pub const TEXT_COLOR: &str = "sub-list-text-color";
        /// keys.h:381
        pub const TEXT_COLOR_DEFAULT: &str = "#FFFFFFFF";

        /// keys.h:383
        pub const HOVER_COLOR: &str = "sub-list-hover-color";
        /// keys.h:384
        pub const HOVER_COLOR_DEFAULT: &str = "#FF1A3B5C";

        /// keys.h:386
        pub const SELECTED_COLOR: &str = "sub-list-selected-color";
        /// keys.h:387
        pub const SELECTED_COLOR_DEFAULT: &str = "#FF295A8A";

        /// keys.h:389
        pub const SELECTED_HOVER_COLOR: &str = "sub-list-selected-hover-color";
        /// keys.h:390
        pub const SELECTED_HOVER_COLOR_DEFAULT: &str = "#FF3F576E";

        // keys.h:392 — PRIMARY_FONT key name (pure):
        /// keys.h:392
        pub const PRIMARY_FONT: &str = "sub-list-primary-font";
        // keys.h:393-400 — PRIMARY_FONT_DEFAULT. QFont; C++-only,
        // stays shell-side.

        // keys.h:402 — SECONDARY_FONT key name (pure):
        /// keys.h:402
        pub const SECONDARY_FONT: &str = "sub-list-secondary-font";
        // keys.h:403-410 — SECONDARY_FONT_DEFAULT. QFont; C++-only,
        // stays shell-side.
    }
}

pub mod keybind {
    /// `Keys::Keybind` (keys.h:414-445).
    ///
    /// keys.h:416
    pub const GROUP: &str = "keybind";

    /// keys.h:418
    pub const CURRENT_PROFILE: &str = "current";

    /// `Keys::Keybind::Profile` (keys.h:420-444).
    pub mod profile {
        /// keys.h:422
        pub const GROUP: &str = "profiles";

        /// keys.h:424
        pub const CARD_ADD: &str = "card-add";
        /// keys.h:425
        pub const CARD_NEXT: &str = "card-next";
        /// keys.h:426
        pub const CARD_PREVIOUS: &str = "card-prev";
        /// keys.h:427
        pub const OPEN_FILE: &str = "open-file";
        /// keys.h:428
        pub const OPEN_URL: &str = "open-url";
        /// keys.h:429
        pub const OSC_VISIBILITY: &str = "osc-visibility";
        /// keys.h:430
        pub const SHOW_SEARCH: &str = "show-search";
        /// keys.h:431
        pub const SHOW_SUBTITLE_LIST: &str = "show-sub-list";
        /// keys.h:432
        pub const START_OCR: &str = "start-ocr";
        /// keys.h:433
        pub const SUBTITLE_AUTO_PAUSE: &str = "sub-auto-pause";
        /// keys.h:434
        pub const SUBTITLE_DECREASE_SIZE: &str = "sub-decrease-size";
        /// keys.h:435
        pub const SUBTITLE_INCREASE_SIZE: &str = "sub-increase-size";
        /// keys.h:436
        pub const SUBTITLE_MOVE_DOWN: &str = "sub-move-down";
        /// keys.h:437
        pub const SUBTITLE_MOVE_UP: &str = "sub-move-up";
        /// keys.h:438
        pub const SUBTITLE_SHOW: &str = "sub-show";
        /// keys.h:439
        pub const SUBTITLE_FURIGANA: &str = "sub-furigana";
        /// keys.h:440
        pub const SUBTITLE_LIST_COPY_SELECTED: &str = "sub-list-copy-selected";
        /// keys.h:441
        pub const SUBTITLE_LIST_FIND: &str = "sub-list-find";
        /// keys.h:442
        pub const SUBTITLE_LIST_FIND_NEXT: &str = "sub-list-find-next";
        /// keys.h:443
        pub const SUBTITLE_LIST_FIND_PREVIOUS: &str = "sub-list-find-prev";
    }
}

pub mod ocr {
    /// `Keys::Ocr` (keys.h:447-459).
    ///
    /// keys.h:449
    pub const GROUP: &str = "ocr";

    /// keys.h:451
    pub const ENABLED: &str = "enable";
    /// keys.h:452
    pub const ENABLED_DEFAULT: bool = true;

    /// keys.h:454
    pub const USE_GPU: &str = "use-gpu";
    /// keys.h:455
    pub const USE_GPU_DEFAULT: bool = true;

    /// keys.h:457
    pub const MODEL: &str = "model";
    /// keys.h:458
    pub const MODEL_DEFAULT: &str = "kha-white/manga-ocr-base";
}

pub mod asr {
    /// `Keys::Asr` (keys.h:461-479).
    ///
    /// keys.h:463
    pub const GROUP: &str = "asr";

    /// keys.h:465
    pub const ENABLED: &str = "enable";
    /// keys.h:466
    pub const ENABLED_DEFAULT: bool = true;

    /// keys.h:468
    pub const BACKEND: &str = "backend";
    /// keys.h:469
    pub const BACKEND_WHISPER: &str = "whisper";
    /// keys.h:470
    pub const BACKEND_ONNX: &str = "onnx";
    // keys.h:471-475 — BACKEND_DEFAULT is selected by the CMake feature
    // defines HAVE_ASR_BACKEND_WHISPER / HAVE_ASR_BACKEND_ONNX. The pure
    // selection rule is [`asr_backend_default`]; C++-only, stays shell-side
    // otherwise (build configuration, not runtime data).

    /// keys.h:477
    pub const START_FROM_BEGINNING: &str = "start-from-beginning";
    /// keys.h:478
    pub const START_FROM_BEGINNING_DEFAULT: bool = false;

    /// Pure port of keys.h:471-475:
    ///
    /// ```text
    /// without WHISPER and with ONNX -> "onnx", else -> "whisper"
    /// ```
    ///
    /// `have_whisper` / `have_onnx` correspond to the CMake defines
    /// `HAVE_ASR_BACKEND_WHISPER` / `HAVE_ASR_BACKEND_ONNX` (truthy when the
    /// define exists, false when it does not).
    pub const fn backend_default(have_whisper: bool, have_onnx: bool) -> &'static str {
        if !have_whisper && have_onnx {
            BACKEND_ONNX
        } else {
            BACKEND_WHISPER
        }
    }
}

pub mod whisper {
    /// `Keys::Whisper` (keys.h:481-520).
    ///
    /// keys.h:483
    pub const GROUP: &str = "whisper";

    /// keys.h:485
    pub const ENABLED: &str = "enable";
    /// keys.h:486
    pub const ENABLED_DEFAULT: bool = true;

    /// keys.h:488
    pub const MODEL: &str = "model";
    /// keys.h:489
    pub const MODEL_DEFAULT: &str = "base";

    /// keys.h:491
    pub const CUSTOM_MODEL: &str = "custom-model";
    /// keys.h:492
    pub const CUSTOM_MODEL_DEFAULT: &str = "";

    /// keys.h:494
    pub const VAD_ENABLED: &str = "vad-enable";
    /// keys.h:495
    pub const VAD_ENABLED_DEFAULT: bool = false;

    /// keys.h:497
    pub const VAD_MODEL: &str = "vad-model";
    /// keys.h:498
    pub const VAD_MODEL_DEFAULT: &str = "ggml-silero-v6.2.0.bin";

    /// keys.h:500
    pub const USE_GPU: &str = "use-gpu";
    /// keys.h:501
    pub const USE_GPU_DEFAULT: bool = true;

    /// keys.h:503
    pub const GPU_DEVICE: &str = "gpu-device";
    /// keys.h:504
    pub const GPU_DEVICE_DEFAULT: i32 = 0;

    /// keys.h:506
    pub const THREADS: &str = "threads";
    /// keys.h:507
    pub const THREADS_DEFAULT: i32 = 0;

    /// keys.h:509
    pub const BEST_OF: &str = "best-of";
    /// keys.h:510
    pub const BEST_OF_DEFAULT: i32 = 5;

    /// keys.h:512
    pub const BEAM_SIZE: &str = "beam-size";
    /// keys.h:513
    pub const BEAM_SIZE_DEFAULT: i32 = 5;

    /// keys.h:515
    pub const FLASH_ATTN: &str = "flash-attn";
    /// keys.h:516
    pub const FLASH_ATTN_DEFAULT: bool = true;

    /// keys.h:518
    pub const SUPPRESS_REGEX: &str = "suppress-regex";
    /// keys.h:519
    pub const SUPPRESS_REGEX_DEFAULT: &str = "^\\[_extra_token_[0-9]+\\]$";
}

pub mod onnx {
    /// `Keys::Onnx` (keys.h:522-543).
    ///
    /// keys.h:524
    pub const GROUP: &str = "onnx";

    /// keys.h:526
    pub const MODEL: &str = "model";
    /// keys.h:527
    pub const MODEL_DEFAULT: &str = "kitsune-transcribe-t0.6b-ja";

    /// keys.h:529
    pub const CUSTOM_MODEL: &str = "custom-model";
    /// keys.h:530
    pub const CUSTOM_MODEL_DEFAULT: &str = "";

    /// keys.h:532
    pub const USE_GPU: &str = "use-gpu";
    /// keys.h:533
    pub const USE_GPU_DEFAULT: bool = false;

    /// keys.h:535
    pub const GPU_DEVICE: &str = "gpu-device";
    /// keys.h:536
    pub const GPU_DEVICE_DEFAULT: i32 = 0;

    /// keys.h:538
    pub const THREADS: &str = "threads";
    /// keys.h:539
    pub const THREADS_DEFAULT: i32 = 0;

    /// keys.h:541
    pub const PUNCTUATION: &str = "punctuation";
    /// keys.h:542
    pub const PUNCTUATION_DEFAULT: bool = true;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enums::{Directory, GlossaryStyle, Language, Modifier, SearchMethod};

    // keys.h:87-88 — Version.
    #[test]
    fn keys_h_l0087_version_constants() {
        assert_eq!(version::VERSION, "version");
        assert_eq!(version::CURRENT, 6);
    }

    // keys.h:91-97 — Window key names.
    #[test]
    fn keys_h_l0091_window_constants() {
        assert_eq!(window::GROUP, "main-window");
        assert_eq!(window::SUBTITLE_LIST, "subtitle-list");
        assert_eq!(window::SEARCH, "search");
    }

    // keys.h:99-106 — Internal key names.
    #[test]
    fn keys_h_l0099_internal_constants() {
        assert_eq!(internal::GROUP, "internal");
        assert_eq!(internal::AUTO_UPDATE_OPT_IN_SHOWN, "auto-update-opt-in-shown");
        assert_eq!(
            internal::WHISPER_NO_MODEL_PROMPT_DISMISSED,
            "whisper-no-model-prompt-dismissed"
        );
    }

    // keys.h:108-113 — Recent key names.
    #[test]
    fn keys_h_l0108_recent_constants() {
        assert_eq!(recent::GROUP, "recent");
        assert_eq!(recent::FILES, "files");
    }

    // keys.h:115-124 — SubtitleList.
    #[test]
    fn keys_h_l0115_subtitle_list_constants() {
        assert_eq!(subtitle_list::GROUP, "subtitle-list");
        assert_eq!(subtitle_list::IGNORE_WHITESPACE, "ignore-whitespace");
        assert_eq!(subtitle_list::IGNORE_WHITESPACE_DEFAULT, false);
        assert_eq!(subtitle_list::AUTO_SEEK, "auto-seek");
        assert_eq!(subtitle_list::AUTO_SEEK_DEFAULT, false);
    }

    // keys.h:126-141 — AudioSource.
    #[test]
    fn keys_h_l0126_audio_source_constants() {
        assert_eq!(audio_source::GROUP, "audio-srcs");
        assert_eq!(audio_source::NAME, "name");
        assert_eq!(audio_source::NAME_DEFAULT, "JapanesePod101");
        assert_eq!(audio_source::URL, "url");
        assert_eq!(
            audio_source::URL_DEFAULT,
            "https://assets.languagepod101.com/dictionary/japanese/audiomp3.php?kanji={expression}&kana={reading}"
        );
        assert_eq!(audio_source::TYPE, "type");
        assert_eq!(audio_source::TYPE_DEFAULT.as_i32(), 0);
        assert_eq!(audio_source::MD5, "md5");
        assert_eq!(audio_source::MD5_DEFAULT, "7e2c2f954ef6051373ba916f000168dc");
    }

    // keys.h:143-152 — Application.
    #[test]
    fn keys_h_l0143_application_constants() {
        assert_eq!(application::GROUP, "application");
        assert_eq!(application::LANGUAGE, "language");
        assert_eq!(application::LANGUAGE_DEFAULT, Language::LanguageSystem);
        assert_eq!(application::AUTO_UPDATE_CHECK, "auto-update-check");
        assert_eq!(application::AUTO_UPDATE_CHECK_DEFAULT, false);
    }

    // keys.h:154-194 — Behavior (except the excluded line 192-194 default).
    #[test]
    fn keys_h_l0154_behavior_constants() {
        assert_eq!(behavior::GROUP, "behavior");
        assert_eq!(behavior::AUTO_FIT, "autofit");
        assert_eq!(behavior::AUTO_FIT_DEFAULT, true);
        assert_eq!(behavior::AUTO_FIT_PERCENT, "autofit-percent");
        assert_eq!(behavior::AUTO_FIT_PERCENT_DEFAULT, 100);
        assert_eq!(behavior::OSC_MPV_CURSOR, "osc-mpv-cursor");
        assert_eq!(behavior::OSC_MPV_CURSOR_DEFAULT, false);
        assert_eq!(behavior::OSC_DURATION, "osc-duration");
        assert_eq!(behavior::OSC_DURATION_DEFAULT, 700);
        assert_eq!(behavior::OSC_FADE, "osc-fade");
        assert_eq!(behavior::OSC_FADE_DEFAULT, 250);
        assert_eq!(behavior::OSC_MIN_MOVE, "osc-min-move");
        assert_eq!(behavior::OSC_MIN_MOVE_DEFAULT, 0);
        assert_eq!(behavior::OSC_PREVIEW_THUMBNAILS, "osc-preview-thumbnails");
        assert_eq!(behavior::OSC_PREVIEW_THUMBNAILS_DEFAULT, true);
        assert_eq!(behavior::SUBTITLE_PAUSE, "subtitle-pause");
        assert_eq!(behavior::SUBTITLE_PAUSE_DEFAULT, false);
        assert_eq!(behavior::SUBTITLE_CURSOR_SHOW, "subtitle-cursor-show");
        assert_eq!(behavior::SUBTITLE_CURSOR_SHOW_DEFAULT, false);
        assert_eq!(
            behavior::SECONDARY_SUBTITLE_CURSOR_SHOW,
            "subtitle-cursor-show-secondary"
        );
        assert_eq!(behavior::SECONDARY_SUBTITLE_CURSOR_SHOW_DEFAULT, false);
        assert_eq!(behavior::FILE_OPEN_DIR, "file-open-dir");
        assert_eq!(behavior::FILE_OPEN_DIR_DEFAULT, Directory::DirectoryMovies);
        assert_eq!(behavior::FILE_OPEN_CUSTOM, "file-open-custom");
        // keys.h:192-194 — FILE_OPEN_CUSTOM_DEFAULT excluded (QStandardPaths).
    }

    // keys.h:196-201 — Dictionary.
    #[test]
    fn keys_h_l0196_dictionary_constants() {
        assert_eq!(dictionary::GROUP, "dictionary");
        assert_eq!(dictionary::ORDER, "order");
    }

    // keys.h:203-260 — Search (including Matcher).
    #[test]
    fn keys_h_l0203_search_constants() {
        assert_eq!(search::GROUP, "search");
        assert_eq!(search::matcher::EXACT, "exact-matcher");
        assert_eq!(search::matcher::EXACT_DEFAULT, true);
        assert_eq!(search::matcher::DECONJ, "deconj-matcher");
        assert_eq!(search::matcher::DECONJ_DEFAULT, true);
        assert_eq!(search::matcher::MECAB_IPADIC, "ipadic-matcher");
        assert_eq!(search::matcher::MECAB_IPADIC_DEFAULT, true);
        assert_eq!(search::MIDDLE_MOUSE_SCAN, "middle-mouse-scan");
        assert_eq!(search::MIDDLE_MOUSE_SCAN_DEFAULT, false);
        assert_eq!(search::METHOD, "method");
        assert_eq!(search::METHOD_DEFAULT, SearchMethod::SearchMethodHover);
        assert_eq!(search::DELAY, "delay");
        assert_eq!(search::DELAY_DEFAULT, 250);
        assert_eq!(search::MODIFIER, "modifier");
        assert_eq!(search::MODIFIER_DEFAULT, Modifier::ModifierShift);
        assert_eq!(search::HIDE_MPV_SUBS, "hide-subs");
        assert_eq!(search::HIDE_MPV_SUBS_DEFAULT, true);
        assert_eq!(search::HIDE_SUBS, "hide-bar");
        assert_eq!(search::HIDE_SUBS_DEFAULT, false);
        assert_eq!(search::PAUSE_ON_HOVER, "hover-pause");
        assert_eq!(search::PAUSE_ON_HOVER_DEFAULT, false);
        assert_eq!(search::AUTO_PLAY_AUDIO, "auto-play-audio");
        assert_eq!(search::AUTO_PLAY_AUDIO_DEFAULT, false);
        assert_eq!(search::GLOSSARY_STYLE, "list-result");
        assert_eq!(search::GLOSSARY_STYLE_DEFAULT, GlossaryStyle::GlossaryStyleBullet);
        assert_eq!(search::REPLACE_NEWLINES, "replace-lines");
        assert_eq!(search::REPLACE_NEWLINES_DEFAULT, false);
        assert_eq!(search::REPLACE_NEWLINES_WITH, "replace-with");
        assert_eq!(search::REPLACE_NEWLINES_WITH_DEFAULT, "");
        assert_eq!(search::REMOVE_REGEX, "remove-regex");
        assert_eq!(search::REMOVE_REGEX_DEFAULT, "");
        assert_eq!(search::SUBTITLE_FURIGANA, "subtitle-furigana");
        assert_eq!(search::SUBTITLE_FURIGANA_DEFAULT, false);
        assert_eq!(
            search::SHOW_ANKI_GLOSSARY_CHECKBOX,
            "show-anki-glossary-checkbox"
        );
        assert_eq!(search::SHOW_ANKI_GLOSSARY_CHECKBOX_DEFAULT, true);
    }

    // keys.h:262-297, 299-306 — Interface top level + Subtitle.
    #[test]
    fn keys_h_l0262_interface_constants() {
        assert_eq!(interface::GROUP, "interface");
        assert_eq!(interface::SYSTEM_ICONS, "system-icons");
        assert_eq!(interface::SYSTEM_ICONS_DEFAULT, true);
        assert_eq!(interface::POPUP_WIDTH, "sub-popup-width");
        assert_eq!(interface::POPUP_WIDTH_DEFAULT, 500);
        assert_eq!(interface::POPUP_HEIGHT, "sub-popup-height");
        assert_eq!(interface::POPUP_HEIGHT_DEFAULT, 400);
        assert_eq!(interface::SEARCH_WINDOW, "search-window");
        assert_eq!(interface::SEARCH_WINDOW_DEFAULT, false);
    }

    // keys.h:269-297 — Interface::Subtitle (QFont default at line 272
    // excluded, documented above and at the module declaration).
    #[test]
    fn keys_h_l0269_interface_subtitle_constants() {
        assert_eq!(interface::subtitle::FONT, "sub-font"); // keys.h:271
        assert_eq!(
            interface::subtitle::LINE_SPACING,
            "sub-line-spacing"
        );
        assert!((interface::subtitle::LINE_SPACING_DEFAULT - 0.0).abs() < f64::EPSILON); // keys.h:275
        assert_eq!(interface::subtitle::SCALE, "sub-scale"); // keys.h:277
        assert!((interface::subtitle::SCALE_DEFAULT - 0.05).abs() < f64::EPSILON); // keys.h:278
        assert_eq!(interface::subtitle::OFFSET, "sub-offset"); // keys.h:280
        assert!((interface::subtitle::OFFSET_DEFAULT - 0.045).abs() < f64::EPSILON); // keys.h:281
        assert_eq!(interface::subtitle::STROKE, "sub-stroke"); // keys.h:283
        assert!((interface::subtitle::STROKE_DEFAULT - 15.0).abs() < f64::EPSILON); // keys.h:284
        assert_eq!(interface::subtitle::TEXT_COLOR, "sub-text-color"); // keys.h:286
        assert_eq!(interface::subtitle::TEXT_COLOR_DEFAULT, "#FFFFFFFF"); // keys.h:287
        assert_eq!(
            interface::subtitle::BACKGROUND_COLOR,
            "sub-bg-color"
        );
        assert_eq!(
            interface::subtitle::BACKGROUND_COLOR_DEFAULT,
            "#00000000"
        ); // keys.h:290
        assert_eq!(
            interface::subtitle::STROKE_COLOR,
            "sub-stroke-color"
        );
        assert_eq!(
            interface::subtitle::STROKE_COLOR_DEFAULT,
            "#FF000000"
        ); // keys.h:293
        assert_eq!(
            interface::subtitle::FURIGANA_RATIO,
            "sub-furigana-ratio"
        );
        assert!((interface::subtitle::FURIGANA_RATIO_DEFAULT - 0.4).abs() < f64::EPSILON); // keys.h:296
    }

    // keys.h:308-367 — Interface font *key names* are pure data and ported;
    // their QFont *defaults* (keys.h:309-367) are excluded, documented above.
    #[test]
    fn keys_h_l0308_interface_font_key_names() {
        assert_eq!(interface::SEARCH_EXPRESSION_FONT, "search-expression-font"); // keys.h:308
        assert_eq!(interface::SEARCH_READING_FONT, "search-reading-font"); // keys.h:318
        assert_eq!(interface::SEARCH_CONJ_FONT, "search-conj-font"); // keys.h:328
        assert_eq!(interface::SEARCH_TAG_FONT, "search-tag-font"); // keys.h:338
        assert_eq!(interface::SEARCH_GLOSSARY_FONT, "search-glossary-font"); // keys.h:349
        assert_eq!(interface::SEARCH_KANJI_FONT, "search-kanji-font"); // keys.h:359
    }

    // keys.h:369-411 — Interface::SubtitleList (QFont defaults at lines
    // 393-400 and 403-410 excluded, documented above).
    #[test]
    fn keys_h_l0369_interface_subtitle_list_constants() {
        assert_eq!(interface::subtitle_list::WINDOW, "sub-list-window");
        assert_eq!(interface::subtitle_list::WINDOW_DEFAULT, false);
        assert_eq!(interface::subtitle_list::TIMESTAMPS, "sub-list-timestamps");
        assert_eq!(interface::subtitle_list::TIMESTAMPS_DEFAULT, false);
        assert_eq!(interface::subtitle_list::BACKGROUND_COLOR, "sub-list-background-color");
        assert_eq!(interface::subtitle_list::BACKGROUND_COLOR_DEFAULT, "#FF000000");
        assert_eq!(interface::subtitle_list::TEXT_COLOR, "sub-list-text-color");
        assert_eq!(interface::subtitle_list::TEXT_COLOR_DEFAULT, "#FFFFFFFF");
        assert_eq!(interface::subtitle_list::HOVER_COLOR, "sub-list-hover-color");
        assert_eq!(interface::subtitle_list::HOVER_COLOR_DEFAULT, "#FF1A3B5C");
        assert_eq!(interface::subtitle_list::SELECTED_COLOR, "sub-list-selected-color");
        assert_eq!(interface::subtitle_list::SELECTED_COLOR_DEFAULT, "#FF295A8A");
        assert_eq!(interface::subtitle_list::SELECTED_HOVER_COLOR, "sub-list-selected-hover-color");
        assert_eq!(interface::subtitle_list::SELECTED_HOVER_COLOR_DEFAULT, "#FF3F576E");
        assert_eq!(interface::subtitle_list::PRIMARY_FONT, "sub-list-primary-font");
        assert_eq!(interface::subtitle_list::SECONDARY_FONT, "sub-list-secondary-font");
    }

    // keys.h:414-445 — Keybind / Profile.
    #[test]
    fn keys_h_l0414_keybind_constants() {
        assert_eq!(keybind::GROUP, "keybind");
        assert_eq!(keybind::CURRENT_PROFILE, "current");
        assert_eq!(keybind::profile::GROUP, "profiles");
        assert_eq!(keybind::profile::CARD_ADD, "card-add");
        assert_eq!(keybind::profile::CARD_NEXT, "card-next");
        assert_eq!(keybind::profile::CARD_PREVIOUS, "card-prev");
        assert_eq!(keybind::profile::OPEN_FILE, "open-file");
        assert_eq!(keybind::profile::OPEN_URL, "open-url");
        assert_eq!(keybind::profile::OSC_VISIBILITY, "osc-visibility");
        assert_eq!(keybind::profile::SHOW_SEARCH, "show-search");
        assert_eq!(keybind::profile::SHOW_SUBTITLE_LIST, "show-sub-list");
        assert_eq!(keybind::profile::START_OCR, "start-ocr");
        assert_eq!(keybind::profile::SUBTITLE_AUTO_PAUSE, "sub-auto-pause");
        assert_eq!(
            keybind::profile::SUBTITLE_DECREASE_SIZE,
            "sub-decrease-size"
        );
        assert_eq!(
            keybind::profile::SUBTITLE_INCREASE_SIZE,
            "sub-increase-size"
        );
        assert_eq!(keybind::profile::SUBTITLE_MOVE_DOWN, "sub-move-down");
        assert_eq!(keybind::profile::SUBTITLE_MOVE_UP, "sub-move-up");
        assert_eq!(keybind::profile::SUBTITLE_SHOW, "sub-show");
        assert_eq!(keybind::profile::SUBTITLE_FURIGANA, "sub-furigana");
        assert_eq!(
            keybind::profile::SUBTITLE_LIST_COPY_SELECTED,
            "sub-list-copy-selected"
        );
        assert_eq!(keybind::profile::SUBTITLE_LIST_FIND, "sub-list-find");
        assert_eq!(
            keybind::profile::SUBTITLE_LIST_FIND_NEXT,
            "sub-list-find-next"
        );
        assert_eq!(
            keybind::profile::SUBTITLE_LIST_FIND_PREVIOUS,
            "sub-list-find-prev"
        );
    }

    // keys.h:447-459 — Ocr.
    #[test]
    fn keys_h_l0447_ocr_constants() {
        assert_eq!(ocr::GROUP, "ocr");
        assert_eq!(ocr::ENABLED, "enable");
        assert_eq!(ocr::ENABLED_DEFAULT, true);
        assert_eq!(ocr::USE_GPU, "use-gpu");
        assert_eq!(ocr::USE_GPU_DEFAULT, true);
        assert_eq!(ocr::MODEL, "model");
        assert_eq!(ocr::MODEL_DEFAULT, "kha-white/manga-ocr-base");
    }

    // keys.h:461-479 — Asr.
    #[test]
    fn keys_h_l0461_asr_constants() {
        assert_eq!(asr::GROUP, "asr");
        assert_eq!(asr::ENABLED, "enable");
        assert_eq!(asr::ENABLED_DEFAULT, true);
        assert_eq!(asr::BACKEND, "backend");
        assert_eq!(asr::BACKEND_WHISPER, "whisper");
        assert_eq!(asr::BACKEND_ONNX, "onnx");
        assert_eq!(asr::START_FROM_BEGINNING, "start-from-beginning");
        assert_eq!(asr::START_FROM_BEGINNING_DEFAULT, false);
    }

    // keys.h:471-475 — the pure BACKEND_DEFAULT selection rule.
    #[test]
    fn keys_h_l0471_asr_backend_default_selection() {
        // Default branch: no definitions -> WHISPER.
        assert_eq!(asr::backend_default(false, false), "whisper");
        // Whisper defined -> never ONNX, regardless of ONNX.
        assert_eq!(asr::backend_default(true, false), "whisper");
        assert_eq!(asr::backend_default(true, true), "whisper");
        // Only ONNX defined -> ONNX.
        assert_eq!(asr::backend_default(false, true), "onnx");
    }

    // keys.h:481-520 — Whisper.
    #[test]
    fn keys_h_l0481_whisper_constants() {
        assert_eq!(whisper::GROUP, "whisper");
        assert_eq!(whisper::ENABLED, "enable");
        assert_eq!(whisper::ENABLED_DEFAULT, true);
        assert_eq!(whisper::MODEL, "model");
        assert_eq!(whisper::MODEL_DEFAULT, "base");
        assert_eq!(whisper::CUSTOM_MODEL, "custom-model");
        assert_eq!(whisper::CUSTOM_MODEL_DEFAULT, "");
        assert_eq!(whisper::VAD_ENABLED, "vad-enable");
        assert_eq!(whisper::VAD_ENABLED_DEFAULT, false);
        assert_eq!(whisper::VAD_MODEL, "vad-model");
        assert_eq!(whisper::VAD_MODEL_DEFAULT, "ggml-silero-v6.2.0.bin");
        assert_eq!(whisper::USE_GPU, "use-gpu");
        assert_eq!(whisper::USE_GPU_DEFAULT, true);
        assert_eq!(whisper::GPU_DEVICE, "gpu-device");
        assert_eq!(whisper::GPU_DEVICE_DEFAULT, 0);
        assert_eq!(whisper::THREADS, "threads");
        assert_eq!(whisper::THREADS_DEFAULT, 0);
        assert_eq!(whisper::BEST_OF, "best-of");
        assert_eq!(whisper::BEST_OF_DEFAULT, 5);
        assert_eq!(whisper::BEAM_SIZE, "beam-size");
        assert_eq!(whisper::BEAM_SIZE_DEFAULT, 5);
        assert_eq!(whisper::FLASH_ATTN, "flash-attn");
        assert_eq!(whisper::FLASH_ATTN_DEFAULT, true);
        assert_eq!(whisper::SUPPRESS_REGEX, "suppress-regex");
        assert_eq!(whisper::SUPPRESS_REGEX_DEFAULT, "^\\[_extra_token_[0-9]+\\]$");
    }

    // keys.h:522-543 — Onnx.
    #[test]
    fn keys_h_l0522_onnx_constants() {
        assert_eq!(onnx::GROUP, "onnx");
        assert_eq!(onnx::MODEL, "model");
        assert_eq!(onnx::MODEL_DEFAULT, "kitsune-transcribe-t0.6b-ja");
        assert_eq!(onnx::CUSTOM_MODEL, "custom-model");
        assert_eq!(onnx::CUSTOM_MODEL_DEFAULT, "");
        assert_eq!(onnx::USE_GPU, "use-gpu");
        assert_eq!(onnx::USE_GPU_DEFAULT, false);
        assert_eq!(onnx::GPU_DEVICE, "gpu-device");
        assert_eq!(onnx::GPU_DEVICE_DEFAULT, 0);
        assert_eq!(onnx::THREADS, "threads");
        assert_eq!(onnx::THREADS_DEFAULT, 0);
        assert_eq!(onnx::PUNCTUATION, "punctuation");
        assert_eq!(onnx::PUNCTUATION_DEFAULT, true);
    }
}
