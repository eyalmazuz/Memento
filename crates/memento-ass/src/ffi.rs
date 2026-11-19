//! Minimal, stable C ABI bindings for system `libass` (`ass/ass.h`, `ass/ass_types.h`).
//!
//! Only the stable opaque-handle API (`ASS_Library`, `ASS_Renderer`, `ASS_Track`,
//! `ASS_Image`) is bound; internal track structs are never mutated directly.

#![allow(non_camel_case_types, dead_code)]

use std::ffi::{c_char, c_double, c_int, c_longlong, c_uchar, c_void};

#[repr(C)]
pub struct ASS_Library {
    _private: [u8; 0],
}

#[repr(C)]
pub struct ASS_Renderer {
    _private: [u8; 0],
}

#[repr(C)]
pub struct ASS_Track {
    _private: [u8; 0],
}

pub const IMAGE_TYPE_CHARACTER: c_int = 0;
pub const IMAGE_TYPE_OUTLINE: c_int = 1;
pub const IMAGE_TYPE_SHADOW: c_int = 2;

pub const ASS_HINTING_NONE: c_int = 0;
pub const ASS_HINTING_LIGHT: c_int = 1;
pub const ASS_HINTING_NORMAL: c_int = 2;
pub const ASS_HINTING_NATIVE: c_int = 3;

pub const ASS_SHAPING_SIMPLE: c_int = 0;
pub const ASS_SHAPING_COMPLEX: c_int = 1;

pub const ASS_FONTPROVIDER_NONE: c_int = 0;
pub const ASS_FONTPROVIDER_AUTODETECT: c_int = 1;
pub const ASS_FONTPROVIDER_CORETEXT: c_int = 2;
pub const ASS_FONTPROVIDER_FONTCONFIG: c_int = 3;
pub const ASS_FONTPROVIDER_DIRECTWRITE: c_int = 4;

pub const ASS_OVERRIDE_DEFAULT: c_int = 0;
pub const ASS_OVERRIDE_BIT_STYLE: c_int = 1 << 0;
pub const ASS_OVERRIDE_BIT_SELECTIVE_FONT_SCALE: c_int = 1 << 1;
pub const ASS_OVERRIDE_BIT_FONT_SIZE_FIELDS: c_int = 1 << 2;
pub const ASS_OVERRIDE_BIT_FONT_NAME: c_int = 1 << 3;
pub const ASS_OVERRIDE_BIT_COLORS: c_int = 1 << 4;
pub const ASS_OVERRIDE_BIT_ATTRIBUTES: c_int = 1 << 5;
pub const ASS_OVERRIDE_BIT_BORDER: c_int = 1 << 6;
pub const ASS_OVERRIDE_BIT_ALIGNMENT: c_int = 1 << 7;
pub const ASS_OVERRIDE_BIT_MARGINS: c_int = 1 << 8;
pub const ASS_OVERRIDE_FULL_STYLE: c_int = 1 << 9;
pub const ASS_OVERRIDE_BIT_JUSTIFY: c_int = 1 << 10;
pub const ASS_OVERRIDE_BIT_BLUR: c_int = 1 << 11;

#[repr(C)]
pub struct ASS_Image {
    pub w: c_int,
    pub h: c_int,
    pub stride: c_int,
    pub bitmap: *mut c_uchar,
    pub color: u32,
    pub dst_x: c_int,
    pub dst_y: c_int,
    pub next: *mut ASS_Image,
    pub kind: c_int,
}

/// Quiet message callback matching libass's `void (*msg_cb)(int, const char *, va_list, void *)`.
///
/// The callback ignores all parameters and returns immediately without inspecting `va_list`,
/// preventing libass diagnostic spam to stderr during frequent per-cue hitbox probes.
pub unsafe extern "C" fn silent_message_cb(
    _level: c_int,
    _fmt: *const c_char,
    _args: *mut c_void,
    _data: *mut c_void,
) {
}

extern "C" {
    pub fn ass_library_version() -> c_int;
    pub fn ass_library_init() -> *mut ASS_Library;
    pub fn ass_library_done(priv_: *mut ASS_Library);
    pub fn ass_set_fonts_dir(priv_: *mut ASS_Library, fonts_dir: *const c_char);
    pub fn ass_set_extract_fonts(priv_: *mut ASS_Library, extract: c_int);
    pub fn ass_set_message_cb(
        priv_: *mut ASS_Library,
        msg_cb: Option<
            unsafe extern "C" fn(c_int, *const c_char, *mut c_void, *mut c_void),
        >,
        data: *mut c_void,
    );
    pub fn ass_add_font(
        library: *mut ASS_Library,
        name: *const c_char,
        data: *const c_char,
        data_size: c_int,
    );
    pub fn ass_clear_fonts(library: *mut ASS_Library);

    pub fn ass_renderer_init(priv_: *mut ASS_Library) -> *mut ASS_Renderer;
    pub fn ass_renderer_done(priv_: *mut ASS_Renderer);
    pub fn ass_set_frame_size(priv_: *mut ASS_Renderer, w: c_int, h: c_int);
    pub fn ass_set_storage_size(priv_: *mut ASS_Renderer, w: c_int, h: c_int);
    pub fn ass_set_shaper(priv_: *mut ASS_Renderer, level: c_int);
    pub fn ass_set_margins(
        priv_: *mut ASS_Renderer,
        t: c_int,
        b: c_int,
        l: c_int,
        r: c_int,
    );
    pub fn ass_set_use_margins(priv_: *mut ASS_Renderer, use_: c_int);
    pub fn ass_set_pixel_aspect(priv_: *mut ASS_Renderer, par: c_double);
    pub fn ass_set_font_scale(priv_: *mut ASS_Renderer, font_scale: c_double);
    pub fn ass_set_hinting(priv_: *mut ASS_Renderer, ht: c_int);
    pub fn ass_set_line_spacing(priv_: *mut ASS_Renderer, line_spacing: c_double);
    pub fn ass_set_line_position(priv_: *mut ASS_Renderer, line_position: c_double);
    pub fn ass_set_fonts(
        priv_: *mut ASS_Renderer,
        default_font: *const c_char,
        default_family: *const c_char,
        dfp: c_int,
        config: *const c_char,
        update: c_int,
    );
    pub fn ass_set_selective_style_override_enabled(priv_: *mut ASS_Renderer, bits: c_int);
    pub fn ass_set_cache_limits(
        priv_: *mut ASS_Renderer,
        glyph_max: c_int,
        bitmap_max_size: c_int,
    );
    pub fn ass_render_frame(
        priv_: *mut ASS_Renderer,
        track: *mut ASS_Track,
        now: c_longlong,
        detect_change: *mut c_int,
    ) -> *mut ASS_Image;

    pub fn ass_read_memory(
        library: *mut ASS_Library,
        buf: *mut c_char,
        bufsize: usize,
        codepage: *const c_char,
    ) -> *mut ASS_Track;
    pub fn ass_free_track(track: *mut ASS_Track);
}
