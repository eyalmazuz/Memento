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

//! Maps an OCR selection from player coordinates to screenshot pixels, as in
//! `OcrController::readRegionAsync` in `src/ocr/ocrcontroller.cpp`. Qt first
//! scales the screenshot to the player size with `KeepAspectRatio`, clips the
//! normalized selection against that displayed rectangle, rounds the mapped
//! coordinates with `qRound`, then intersects the pixel rect with the image.
//! The caller checks selection, player, and screenshot dimensions before this
//! calculation; this module also accepts other dimensions for direct callers.

/// Status reported to the C++ caller; the application-level error strings
/// stay in `ocrcontroller.cpp`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum OcrRegionStatus {
    /// `image_rect` holds the mapped, valid pixel rect.
    Ok = 0,
    /// "The selected region does not include video."
    NoVideoRegion = 1,
    /// "The selected region is empty."
    EmptyRegion = 2,
}

/// Keep the operand order explicit when intersecting floating-point rects;
/// `f64::min`/`max` have different NaN behavior.
fn qmin_f64(a: f64, b: f64) -> f64 {
    if a < b { a } else { b }
}

fn qmax_f64(a: f64, b: f64) -> f64 {
    if a < b { b } else { a }
}

/// For coordinates representable as `int`, `qRound` adds a signed half before
/// truncation. Values outside that domain cannot form a portable `QRect`.
fn qt_round(value: f64) -> Option<i32> {
    let shifted = value + (0.5f64).copysign(value);
    if !shifted.is_finite() || !(-2147483648.0..2147483648.0).contains(&shifted) {
        return None;
    }
    Some(shifted.trunc() as i32)
}

/// Integer `QRect` intersection with inclusive right/bottom edges. A null
/// result is represented as four zero coordinates and dimensions here.
fn qrect_i32_intersected(
    ax: i32, ay: i32, aw: i32, ah: i32,
    bx: i32, by: i32, bw: i32, bh: i32,
) -> (i32, i32, i32, i32) {
    let a_x1 = ax;
    let a_y1 = ay;
    let a_x2 = ax.wrapping_add(aw).wrapping_sub(1);
    let a_y2 = ay.wrapping_add(ah).wrapping_sub(1);
    let b_x1 = bx;
    let b_y1 = by;
    let b_x2 = bx.wrapping_add(bw).wrapping_sub(1);
    let b_y2 = by.wrapping_add(bh).wrapping_sub(1);

    if a_x1 == 0 && a_y1 == 0 && a_x2 == -1 && a_y2 == -1 {
        return (0, 0, 0, 0);
    }
    if b_x1 == 0 && b_y1 == 0 && b_x2 == -1 && b_y2 == -1 {
        return (0, 0, 0, 0);
    }

    let (mut a_l, mut a_r) = (a_x1, a_x2);
    if a_x2 < a_x1.wrapping_sub(1) {
        a_l = a_x2.wrapping_add(1);
        a_r = a_x1.wrapping_sub(1);
    }
    let (mut b_l, mut b_r) = (b_x1, b_x2);
    if b_x2 < b_x1.wrapping_sub(1) {
        b_l = b_x2.wrapping_add(1);
        b_r = b_x1.wrapping_sub(1);
    }
    if a_l > b_r || b_l > a_r {
        return (0, 0, 0, 0);
    }

    let (mut a_t, mut a_b) = (a_y1, a_y2);
    if a_y2 < a_y1.wrapping_sub(1) {
        a_t = a_y2.wrapping_add(1);
        a_b = a_y1.wrapping_sub(1);
    }
    let (mut b_t, mut b_b) = (b_y1, b_y2);
    if b_y2 < b_y1.wrapping_sub(1) {
        b_t = b_y2.wrapping_add(1);
        b_b = b_y1.wrapping_sub(1);
    }
    if a_t > b_b || b_t > a_b {
        return (0, 0, 0, 0);
    }

    let x = q32max(a_l, b_l);
    let right = q32min(a_r, b_r);
    let y = q32max(a_t, b_t);
    let bottom = q32min(a_b, b_b);
    (x, y, right.wrapping_sub(x).wrapping_add(1),
         bottom.wrapping_sub(y).wrapping_add(1))
}

fn q32min(a: i32, b: i32) -> i32 {
    if a < b { a } else { b }
}

fn q32max(a: i32, b: i32) -> i32 {
    if a < b { b } else { a }
}

/// Maps a selection (player coordinates) to screenshot pixels. `Ok` carries
/// the rounded, clamped rect; other statuses leave the rect fields at zero.
/// Returns `None` when rounded coordinates cannot be represented as `i32`.
pub fn map_ocr_region(
    selection_x: f64,
    selection_y: f64,
    selection_width: f64,
    selection_height: f64,
    player_width: f64,
    player_height: f64,
    image_width: i32,
    image_height: i32,
) -> Option<(OcrRegionStatus, (i32, i32, i32, i32))> {
    const EMPTY: (i32, i32, i32, i32) = (0, 0, 0, 0);
    // imageSize (the screenshot dimensions, implicitly converted to QSizeF)
    let image_w = f64::from(image_width);
    let image_h = f64::from(image_height);

    // QSizeF::scaled(playerSize, Qt::KeepAspectRatio). qIsNull doubles agree:
    // true only for plus-or-minus zero.
    let is_null_size = |value: f64| value >= 0.0 && value <= 0.0;
    let (displayed_w, displayed_h) =
        if is_null_size(image_w) || is_null_size(image_h) {
            // QSizeF(QSize) always yields finite, non-null screenshot dims in
            // the application, but mirror the algorithm on every input.
            (player_width, player_height)
        } else {
            let rw = player_height * image_w / image_h;
            if rw <= player_width {
                (rw, player_height)
            } else {
                (player_width, player_width * image_h / image_w)
            }
        };

    // QRectF displayedVideo
    let displayed_x = (player_width - displayed_w) / 2.0;
    let displayed_y = (player_height - displayed_h) / 2.0;

    // selection.normalized()
    let (sel_x, sel_w) = if selection_width < 0.0 {
        (selection_x + selection_width, -selection_width)
    } else {
        (selection_x, selection_width)
    };
    let (sel_y, sel_h) = if selection_height < 0.0 {
        (selection_y + selection_height, -selection_height)
    } else {
        (selection_y, selection_height)
    };

    // QRectF::intersected(displayedVideo)
    let (mut left1, mut right1) = (sel_x, sel_x);
    if sel_w < 0.0 {
        left1 += sel_w;
    } else {
        right1 += sel_w;
    }
    if left1 == right1 {
        return Some((OcrRegionStatus::NoVideoRegion, EMPTY));
    }
    let (mut left2, mut right2) = (displayed_x, displayed_x);
    if displayed_w < 0.0 {
        left2 += displayed_w;
    } else {
        right2 += displayed_w;
    }
    if left2 == right2 {
        return Some((OcrRegionStatus::NoVideoRegion, EMPTY));
    }
    if left1 >= right2 || left2 >= right1 {
        return Some((OcrRegionStatus::NoVideoRegion, EMPTY));
    }

    let (mut top1, mut bottom1) = (sel_y, sel_y);
    if sel_h < 0.0 {
        top1 += sel_h;
    } else {
        bottom1 += sel_h;
    }
    if top1 == bottom1 {
        return Some((OcrRegionStatus::NoVideoRegion, EMPTY));
    }
    let (mut top2, mut bottom2) = (displayed_y, displayed_y);
    if displayed_h < 0.0 {
        top2 += displayed_h;
    } else {
        bottom2 += displayed_h;
    }
    if top2 == bottom2 {
        return Some((OcrRegionStatus::NoVideoRegion, EMPTY));
    }
    if top1 >= bottom2 || top2 >= bottom1 {
        return Some((OcrRegionStatus::NoVideoRegion, EMPTY));
    }

    let selected_x = qmax_f64(left1, left2);
    let selected_y = qmax_f64(top1, top2);
    let selected_w = qmin_f64(right1, right2) - selected_x;
    let selected_h = qmin_f64(bottom1, bottom2) - selected_y;
    if selected_w <= 0.0 || selected_h <= 0.0 {
        return Some((OcrRegionStatus::NoVideoRegion, EMPTY));
    }

    let x_scale = image_w / displayed_w;
    let y_scale = image_h / displayed_h;
    let round_x = qt_round((selected_x - displayed_x) * x_scale)?;
    let round_y = qt_round((selected_y - displayed_y) * y_scale)?;
    let round_w = qt_round(selected_w * x_scale)?;
    let round_h = qt_round(selected_h * y_scale)?;

    // imageRect.intersected(QRect(QPoint(0, 0), screenshot.size()))
    let (clamp_x, clamp_y, clamp_w, clamp_h) = qrect_i32_intersected(
        round_x, round_y, round_w, round_h, 0, 0, image_width, image_height,
    );
    if clamp_w <= 0 || clamp_h <= 0 {
        return Some((OcrRegionStatus::EmptyRegion, EMPTY));
    }

    Some((OcrRegionStatus::Ok, (clamp_x, clamp_y, clamp_w, clamp_h)))
}

const SLASH: u16 = b'/' as u16;
const BACKSLASH: u16 = b'\\' as u16;
const COLON: u16 = b':' as u16;

const VENV_PYTHON_UNIX: &[u16] = &[
    b'b' as u16, b'i' as u16, b'n' as u16, b'/' as u16, b'p' as u16,
    b'y' as u16, b't' as u16, b'h' as u16, b'o' as u16, b'n' as u16,
];

const VENV_PYTHON_WIN: &[u16] = &[
    b'S' as u16, b'c' as u16, b'r' as u16, b'i' as u16, b'p' as u16,
    b't' as u16, b's' as u16, b'/' as u16, b'p' as u16, b'y' as u16,
    b't' as u16, b'h' as u16, b'o' as u16, b'n' as u16, b'.' as u16,
    b'e' as u16, b'x' as u16, b'e' as u16,
];

/// Resolves the virtual-environment Python executable path matching `QDir(venv).filePath(VENV_PYTHON)`.
/// Returns `None` if `python_exec_is_set` is true or `venv` is empty.
pub fn resolve_venv_python(
    python_exec_is_set: bool,
    venv: &[u16],
    is_windows: bool,
) -> Option<Vec<u16>> {
    if python_exec_is_set || venv.is_empty() {
        return None;
    }

    let sub_path = if is_windows {
        VENV_PYTHON_WIN
    } else {
        VENV_PYTHON_UNIX
    };

    let mut result = Vec::with_capacity(venv.len() + 1 + sub_path.len());

    if is_windows {
        result.extend(venv.iter().map(|&ch| if ch == BACKSLASH { SLASH } else { ch }));
    } else {
        result.extend_from_slice(venv);
    }

    // Qt's QDirPrivate::setPath strips a single trailing slash when length > 1,
    // preserving drive root (e.g. "C:/") on Windows.
    let is_win_drive_root = is_windows
        && result.len() == 3
        && result[1] == COLON
        && (result[0] as u8).is_ascii_alphabetic();

    if result.len() > 1 && result.last() == Some(&SLASH) && !is_win_drive_root {
        result.pop();
    }

    // QDir::filePath appends '/' if not already ending in '/'
    if result.last() != Some(&SLASH) {
        result.push(SLASH);
    }

    result.extend_from_slice(sub_path);
    Some(result)
}

/// Checks whether the OCR model instance needs to be recreated.
///
/// Matches `OcrController::model()` in `src/ocr/ocrcontroller.cpp`:
/// `!has_model || cur_name != new_name || cur_gpu != new_gpu`.
pub fn should_recreate_model(
    has_model: bool,
    cur_name: &[u16],
    cur_gpu: bool,
    new_name: &[u16],
    new_gpu: bool,
) -> bool {
    !has_model || cur_gpu != new_gpu || cur_name != new_name
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(
        sel: (f64, f64, f64, f64), player: (f64, f64), image: (i32, i32),
    ) -> Option<(OcrRegionStatus, (i32, i32, i32, i32))> {
        map_ocr_region(
            sel.0, sel.1, sel.2, sel.3, player.0, player.1, image.0, image.1,
        )
    }

    fn ok(sel: (f64, f64, f64, f64), player: (f64, f64), image: (i32, i32))
        -> (i32, i32, i32, i32)
    {
        let (status, rect) = map(sel, player, image).unwrap();
        assert_eq!(status, OcrRegionStatus::Ok, "{sel:?} {player:?} {image:?}");
        rect
    }

    fn no_video(sel: (f64, f64, f64, f64), player: (f64, f64), image: (i32, i32)) {
        assert_eq!(
            map(sel, player, image).unwrap(),
            (OcrRegionStatus::NoVideoRegion, (0, 0, 0, 0)),
            "{sel:?} {player:?} {image:?}"
        );
    }

    #[test]
    fn qt_round_matches_the_ieee_copysign_form() {
        assert_eq!(qt_round(0.5), Some(1));
        assert_eq!(qt_round(2.5), Some(3));
        assert_eq!(qt_round(3.5), Some(4));
        assert_eq!(qt_round(-0.5), Some(-1));
        assert_eq!(qt_round(-2.5), Some(-3));
        assert_eq!(qt_round(-0.4), Some(0));
        // The ±0.5 add's *binary* rounding, not decimal half-away rounding.
        assert_eq!(qt_round(0.49999999999999994), Some(1));
        assert_eq!(qt_round(-0.49999999999999994), Some(-1));
        assert_eq!(qt_round(1.0), Some(1));
        assert_eq!(qt_round(-1.0), Some(-1));
        assert_eq!(qt_round(f64::NAN), None);
        // Unrepresentable QRect coordinates are rejected.
        assert_eq!(qt_round(3e9), None);
        assert_eq!(qt_round(-3e9), None);
        assert_eq!(qt_round(f64::INFINITY), None);
        assert_eq!(qt_round(f64::NEG_INFINITY), None);
    }

    #[test]
    fn exact_half_scale_player_covers_video() {
        // 1920x1080 screenshot in a 3840x2160 player: displayedSize is the
        // whole player, no letterbox offset, scale 0.5 everywhere.
        assert_eq!(ok((100.0, 100.0, 400.0, 200.0), (3840.0, 2160.0), (1920, 1080)),
                   (50, 50, 200, 100));
    }

    #[test]
    fn letterboxed_video_offsets_the_selection() {
        // 1920x1080 in a 1280x960 player: displayed 1280x720 at y offset
        // 120; scales are the clean 1.5 powers of the fraction 1920/1280.
        assert_eq!(ok((0.0, 240.0, 100.0, 100.0), (1280.0, 960.0), (1920, 1080)),
                   (0, 180, 150, 150));
    }

    #[test]
    fn ties_round_through_the_f64_add() {
        // 1024x768 screenshot shown inside a 512x512 player: displayed
        // (512, 384), y offset 64, scale exactly 2. A selection width of
        // 32.25 pixels maps onto 64.5 image pixels, the qRound tie.
        assert_eq!(ok((0.0, 100.0, 32.25, 40.0), (512.0, 512.0), (1024, 768)),
                   (0, 72, 65, 80));
    }

    #[test]
    fn unnormalized_selections_are_normalized_first() {
        let a = map((0.0, 240.0, 100.0, 100.0), (1280.0, 960.0), (1920, 1080));
        let b = map((100.0, 340.0, -100.0, -100.0), (1280.0, 960.0), (1920, 1080));
        assert_eq!(a, b);
        assert_eq!(a.unwrap().0, OcrRegionStatus::Ok);
    }

    #[test]
    fn selections_miss_the_video_region() {
        // 1920x1080 video displayed 1280x720 inside a square 1280x1280
        // player: everything right of 1280 is black bar.
        no_video((1900.0, 300.0, 100.0, 100.0), (1280.0, 1280.0), (1920, 1080));
        // A zero-width selection has no area.
        no_video((10.0, 10.0, 0.0, 5.0), (1280.0, 960.0), (1920, 1080));
        // Negative width normalized to a selection left of the video.
        no_video((-10.0, 10.0, -3.0, 5.0), (1280.0, 960.0), (1920, 1080));
    }

    #[test]
    fn clamped_rects_fall_back_to_empty_after_rounding() {
        // A sub-pixel selection survives the floating-point intersection
        // but rounds to a zero-height QRect.
        assert_eq!(
            map((0.0, 121.0, 40.0, 0.1), (1280.0, 960.0), (1920, 1080)).unwrap(),
            (OcrRegionStatus::EmptyRegion, (0, 0, 0, 0))
        );
    }

    #[test]
    fn zero_dimension_screenshot_follows_qrect_and_qsizef_nulls() {
        // qIsNull(wd || ht) → displayed is the player size itself; every
        // pixel offset scales by zero to QRect(0, 0, 0, 0), which
        // QRect::intersected treats as null → the C++ "empty" error path.
        assert_eq!(
            map((20.0, 20.0, 50.0, 40.0), (800.0, 600.0), (0, 0)).unwrap(),
            (OcrRegionStatus::EmptyRegion, (0, 0, 0, 0))
        );
    }

    fn u16_str(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn resolve_venv_python_checks_python_exec_set_and_empty() {
        assert_eq!(resolve_venv_python(true, &u16_str("/tmp/venv"), false), None);
        assert_eq!(resolve_venv_python(true, &u16_str("/tmp/venv"), true), None);
        assert_eq!(resolve_venv_python(false, &[], false), None);
        assert_eq!(resolve_venv_python(false, &[], true), None);
    }

    #[test]
    fn resolve_venv_python_unix_variants() {
        let check = |input: &str, expected: &str| {
            let res = resolve_venv_python(false, &u16_str(input), false).expect(input);
            assert_eq!(String::from_utf16_lossy(&res), expected, "failed for input {input}");
        };

        check("/tmp/venv", "/tmp/venv/bin/python");
        check("/tmp/venv/", "/tmp/venv/bin/python");
        check("/tmp/venv//", "/tmp/venv/bin/python");
        check("/tmp//venv///", "/tmp//venv//bin/python");
        check("/", "/bin/python");
        check("//", "/bin/python");
        check("///", "//bin/python");
        check("relative/path", "relative/path/bin/python");
        check("relative/path/", "relative/path/bin/python");
        check("relative/path//", "relative/path/bin/python");
        check("venv", "venv/bin/python");
        check(".", "./bin/python");
        check("./", "./bin/python");
        check("..", "../bin/python");
        check("a/b/c", "a/b/c/bin/python");
        check("/home/user/.env", "/home/user/.env/bin/python");
    }

    #[test]
    fn resolve_venv_python_windows_variants() {
        let check = |input: &str, expected: &str| {
            let res = resolve_venv_python(false, &u16_str(input), true).expect(input);
            assert_eq!(String::from_utf16_lossy(&res), expected, "failed for input {input}");
        };

        check("C:\\venv", "C:/venv/Scripts/python.exe");
        check("C:\\venv\\", "C:/venv/Scripts/python.exe");
        check("C:/venv", "C:/venv/Scripts/python.exe");
        check("C:/venv/", "C:/venv/Scripts/python.exe");
        check("C:\\", "C:/Scripts/python.exe");
        check("C:/", "C:/Scripts/python.exe");
        check("C:", "C:/Scripts/python.exe");
        check("C:\\venv//sub\\", "C:/venv//sub/Scripts/python.exe");
        check("\\\\server\\share\\venv", "//server/share/venv/Scripts/python.exe");
        check("/tmp/venv", "/tmp/venv/Scripts/python.exe");
        check("/tmp/venv/", "/tmp/venv/Scripts/python.exe");
    }

    #[test]
    fn should_recreate_model_truth_table() {
        let name_a = u16_str("model-a");
        let name_b = u16_str("model-b");
        let empty = u16_str("");

        // !has_model -> always true
        assert!(should_recreate_model(false, &name_a, true, &name_a, true));
        assert!(should_recreate_model(false, &name_a, true, &name_b, true));
        assert!(should_recreate_model(false, &name_a, true, &name_a, false));
        assert!(should_recreate_model(false, &empty, false, &empty, false));

        // has_model -> match name & gpu -> false
        assert!(!should_recreate_model(true, &name_a, true, &name_a, true));
        assert!(!should_recreate_model(true, &name_a, false, &name_a, false));
        assert!(!should_recreate_model(true, &empty, true, &empty, true));

        // has_model -> mismatch name -> true
        assert!(should_recreate_model(true, &name_a, true, &name_b, true));
        assert!(should_recreate_model(true, &name_a, false, &name_b, false));
        assert!(should_recreate_model(true, &name_a, true, &empty, true));
        assert!(should_recreate_model(true, &empty, true, &name_a, true));

        // has_model -> mismatch gpu -> true
        assert!(should_recreate_model(true, &name_a, true, &name_a, false));
        assert!(should_recreate_model(true, &name_a, false, &name_a, true));

        // has_model -> mismatch both -> true
        assert!(should_recreate_model(true, &name_a, true, &name_b, false));
    }
}

