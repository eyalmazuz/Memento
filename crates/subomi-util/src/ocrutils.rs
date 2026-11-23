//! OCR selection mapping (player coordinates → screenshot pixel rectangle) and
//! Python virtual-environment executable resolution.

/// Result status of mapping an OCR drag selection onto screenshot pixels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OcrRegionStatus {
    /// Selection overlaps the video frame and maps to a non-empty pixel rect.
    Ok,
    /// Selection lies completely outside the letterboxed video frame.
    NoVideoRegion,
    /// Selection collapses to zero pixels after rounding/clamping.
    EmptyRegion,
}

fn round_to_i32(value: f64) -> Option<i32> {
    if !value.is_finite() {
        return None;
    }
    let rounded = value.round();
    if rounded < f64::from(i32::MIN) || rounded > f64::from(i32::MAX) {
        return None;
    }
    Some(rounded as i32)
}

/// Maps a drag selection in player coordinates onto screenshot pixel coordinates
/// `(x, y, width, height)` accounting for aspect-ratio letterboxing.
///
/// Returns `None` if intermediate coordinates are non-finite or overflow `i32`.
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

    let image_w = f64::from(image_width);
    let image_h = f64::from(image_height);
    if image_width <= 0 || image_height <= 0 || player_width <= 0.0 || player_height <= 0.0 {
        return Some((OcrRegionStatus::NoVideoRegion, EMPTY));
    }

    // Fit screenshot inside player bounds while preserving aspect ratio.
    let fit_w = player_height * image_w / image_h;
    let (displayed_w, displayed_h) = if fit_w <= player_width {
        (fit_w, player_height)
    } else {
        (player_width, player_width * image_h / image_w)
    };
    let displayed_x = (player_width - displayed_w) * 0.5;
    let displayed_y = (player_height - displayed_h) * 0.5;

    // Normalize potentially negative drag extents.
    let (sel_x0, sel_x1) = if selection_width < 0.0 {
        (selection_x + selection_width, selection_x)
    } else {
        (selection_x, selection_x + selection_width)
    };
    let (sel_y0, sel_y1) = if selection_height < 0.0 {
        (selection_y + selection_height, selection_y)
    } else {
        (selection_y, selection_y + selection_height)
    };

    // Intersect selection with displayed video rectangle.
    let clip_x0 = sel_x0.max(displayed_x);
    let clip_y0 = sel_y0.max(displayed_y);
    let clip_x1 = sel_x1.min(displayed_x + displayed_w);
    let clip_y1 = sel_y1.min(displayed_y + displayed_h);
    let clip_w = clip_x1 - clip_x0;
    let clip_h = clip_y1 - clip_y0;
    if !(clip_w > 0.0 && clip_h > 0.0) {
        return Some((OcrRegionStatus::NoVideoRegion, EMPTY));
    }

    // Scale into screenshot pixel coordinates and clamp to `[0..image_w, 0..image_h]`.
    let x_scale = image_w / displayed_w;
    let y_scale = image_h / displayed_h;
    let px = round_to_i32((clip_x0 - displayed_x) * x_scale)?;
    let py = round_to_i32((clip_y0 - displayed_y) * y_scale)?;
    let pw = round_to_i32(clip_w * x_scale)?;
    let ph = round_to_i32(clip_h * y_scale)?;

    let left = px.clamp(0, image_width);
    let top = py.clamp(0, image_height);
    let right = px.saturating_add(pw).clamp(0, image_width);
    let bottom = py.saturating_add(ph).clamp(0, image_height);
    let width = right - left;
    let height = bottom - top;
    if width <= 0 || height <= 0 {
        return Some((OcrRegionStatus::EmptyRegion, EMPTY));
    }

    Some((OcrRegionStatus::Ok, (left, top, width, height)))
}

/// Resolves the Python executable inside a virtual environment directory.
///
/// Returns `None` when an explicit Python executable override is already active
/// or `venv` is empty.
pub fn resolve_venv_python_str(
    python_exec_is_set: bool,
    venv: &str,
    is_windows: bool,
) -> Option<String> {
    if python_exec_is_set || venv.is_empty() {
        return None;
    }

    let sub_path = if is_windows {
        "Scripts/python.exe"
    } else {
        "bin/python"
    };

    let normalized = if is_windows {
        venv.replace('\\', "/")
    } else {
        venv.to_owned()
    };

    let trimmed = normalized.trim_end_matches('/');
    if trimmed.is_empty() {
        return Some(format!("/{sub_path}"));
    }
    Some(format!("{trimmed}/{sub_path}"))
}

#[cfg(test)]
mod tests {
    use super::{map_ocr_region, resolve_venv_python_str, OcrRegionStatus};

    #[test]
    fn maps_letterboxed_and_pillarboxed_regions() {
        // 16:9 video inside 1000x800 player -> 1000x562.5 centered at y=118.75.
        let (status, rect) =
            map_ocr_region(0.0, 0.0, 1000.0, 800.0, 1000.0, 800.0, 1920, 1080).unwrap();
        assert_eq!(status, OcrRegionStatus::Ok);
        assert_eq!(rect, (0, 0, 1920, 1080));

        // Reverse drag normalizes cleanly.
        let (status, rect) =
            map_ocr_region(500.0, 400.0, -500.0, -400.0, 1000.0, 800.0, 1920, 1080).unwrap();
        assert_eq!(status, OcrRegionStatus::Ok);
        assert_eq!(rect.0, 0);
        assert_eq!(rect.1, 0);
        assert_eq!(rect.2, 960);
        assert!(rect.3 > 0);
    }

    #[test]
    fn detects_outside_video_and_tiny_empty_regions() {
        assert_eq!(
            map_ocr_region(0.0, 0.0, 100.0, 50.0, 1000.0, 800.0, 1920, 1080),
            Some((OcrRegionStatus::NoVideoRegion, (0, 0, 0, 0)))
        );
        assert_eq!(
            map_ocr_region(100.0, 200.0, 0.0001, 10.0, 1000.0, 800.0, 100, 100),
            Some((OcrRegionStatus::EmptyRegion, (0, 0, 0, 0)))
        );
    }

    #[test]
    fn resolves_unix_and_windows_venv_python() {
        assert_eq!(resolve_venv_python_str(true, "/tmp/venv", false), None);
        assert_eq!(resolve_venv_python_str(false, "", false), None);
        assert_eq!(
            resolve_venv_python_str(false, "/home/u/.venv/", false).as_deref(),
            Some("/home/u/.venv/bin/python")
        );
        assert_eq!(
            resolve_venv_python_str(false, r"C:\Users\u\.venv\", true).as_deref(),
            Some("C:/Users/u/.venv/Scripts/python.exe")
        );
        assert_eq!(
            resolve_venv_python_str(false, "/", false).as_deref(),
            Some("/bin/python")
        );
    }
}
