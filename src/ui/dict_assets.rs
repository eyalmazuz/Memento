//! Local dictionary resource directory resolution and bounded LRU raster/SVG asset cache.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use resvg::{tiny_skia, usvg};

use super::text_engine::{with_shared_text_engine, TextEngine};

const DICT_ASSET_CACHE_MAX_ENTRIES: usize = 128;
const DICT_ASSET_CACHE_MAX_BYTES: usize = 12 * 1024 * 1024; // 12 MiB

#[derive(Hash, PartialEq, Eq, Clone)]
struct DictAssetCacheKey {
    path: PathBuf,
    mtime: Option<SystemTime>,
    file_len: u64,
    width_tenth: Option<i32>,
    height_tenth: Option<i32>,
    monochrome_tint: Option<[u8; 4]>,
    current_color: String,
    scale_quarter: u16,
}

#[derive(Clone)]
struct CachedDictAsset {
    pixmap: Arc<tiny_skia::Pixmap>,
    logical_w: f32,
    logical_h: f32,
    bytes: usize,
    last_used: u64,
}

struct DictAssetLruCache {
    entries: HashMap<DictAssetCacheKey, CachedDictAsset>,
    total_bytes: usize,
    clock: u64,
}

impl DictAssetLruCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            total_bytes: 0,
            clock: 0,
        }
    }

    #[inline]
    fn next_stamp(&mut self) -> u64 {
        self.clock = self.clock.wrapping_add(1);
        self.clock
    }

    fn get(&mut self, key: &DictAssetCacheKey) -> Option<(Arc<tiny_skia::Pixmap>, f32, f32)> {
        let stamp = self.next_stamp();
        let entry = self.entries.get_mut(key)?;
        entry.last_used = stamp;
        Some((entry.pixmap.clone(), entry.logical_w, entry.logical_h))
    }

    fn insert(
        &mut self,
        key: DictAssetCacheKey,
        pixmap: Arc<tiny_skia::Pixmap>,
        logical_w: f32,
        logical_h: f32,
    ) {
        let bytes = pixmap.data().len();
        if bytes > DICT_ASSET_CACHE_MAX_BYTES {
            return;
        }
        let stamp = self.next_stamp();
        if let Some(existing) = self.entries.get_mut(&key) {
            self.total_bytes = self.total_bytes.saturating_sub(existing.bytes) + bytes;
            existing.pixmap = pixmap;
            existing.logical_w = logical_w;
            existing.logical_h = logical_h;
            existing.bytes = bytes;
            existing.last_used = stamp;
            return;
        }
        while self.entries.len() >= DICT_ASSET_CACHE_MAX_ENTRIES
            || self.total_bytes.saturating_add(bytes) > DICT_ASSET_CACHE_MAX_BYTES
        {
            let Some(oldest_key) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            if let Some(removed) = self.entries.remove(&oldest_key) {
                self.total_bytes = self.total_bytes.saturating_sub(removed.bytes);
            }
        }
        self.total_bytes = self.total_bytes.saturating_add(bytes);
        self.entries.insert(
            key,
            CachedDictAsset {
                pixmap,
                logical_w,
                logical_h,
                bytes,
                last_used: stamp,
            },
        );
    }

    fn shrink_to(&mut self, max_entries: usize, max_bytes: usize) {
        while self.entries.len() > max_entries || self.total_bytes > max_bytes {
            let Some(oldest_key) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            if let Some(removed) = self.entries.remove(&oldest_key) {
                self.total_bytes = self.total_bytes.saturating_sub(removed.bytes);
            } else {
                break;
            }
        }
        self.entries.shrink_to(max_entries.max(16));
    }
}

fn shared_dict_asset_cache() -> &'static Mutex<DictAssetLruCache> {
    static CACHE: OnceLock<Mutex<DictAssetLruCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(DictAssetLruCache::new()))
}

pub(super) fn shrink_dict_asset_cache(max_entries: usize, max_bytes: usize) {
    let mut cache = shared_dict_asset_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache.shrink_to(max_entries, max_bytes);
}

pub fn collect_dictionary_resource_dirs(
    config_dir: &Path,
    dictionary_db_path: &Path,
) -> Vec<PathBuf> {
    let mut dirs = vec![config_dir.join("dict_resources")];
    if let Some(parent) = dictionary_db_path.parent() {
        let res = parent.join("res");
        if !dirs.iter().any(|d| d == &res) {
            dirs.push(res);
        }
    }
    dirs
}

fn sanitize_relative_asset_path(rel: &str) -> Option<PathBuf> {
    let rel = rel.trim().trim_start_matches("./");
    if rel.is_empty() || rel.contains('\0') {
        return None;
    }
    let candidate = Path::new(rel);
    if candidate.is_absolute() {
        return None;
    }
    let mut clean = PathBuf::new();
    for comp in candidate.components() {
        match comp {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!clean.as_os_str().is_empty()).then_some(clean)
}

pub fn resolve_dictionary_asset_path(
    dict_resources_dir: &Path,
    dic_id: i64,
    dict_name: &str,
    rel_path: &str,
) -> Option<PathBuf> {
    let clean_rel = sanitize_relative_asset_path(rel_path)?;
    if dic_id > 0 {
        let by_id = dict_resources_dir
            .join(format!("dict-{dic_id}"))
            .join(&clean_rel);
        if by_id.is_file() {
            return Some(by_id);
        }
    }
    if !dict_name.is_empty()
        && !dict_name.contains('/')
        && !dict_name.contains('\\')
        && dict_name != "."
        && dict_name != ".."
    {
        let by_name = dict_resources_dir.join(dict_name).join(&clean_rel);
        if by_name.is_file() {
            return Some(by_name);
        }
    }
    None
}

pub fn resolve_dictionary_asset_path_in_dirs(
    dict_resource_dirs: &[PathBuf],
    dic_id: i64,
    dict_name: &str,
    rel_path: &str,
) -> Option<PathBuf> {
    dict_resource_dirs
        .iter()
        .find_map(|dir| resolve_dictionary_asset_path(dir, dic_id, dict_name, rel_path))
}

/// Rasterizes a local dictionary image or SVG (supporting `appearance: "monochrome"` tinting via `resvg`)
/// into a `tiny_skia::Pixmap`.
pub fn load_dictionary_image_pixmap(
    asset_path: &Path,
    requested_w: Option<f32>,
    requested_h: Option<f32>,
    monochrome_tint: Option<[u8; 4]>,
    current_color_hex: &str,
) -> Option<Arc<tiny_skia::Pixmap>> {
    load_dictionary_image_pixmap_scaled(
        asset_path,
        requested_w,
        requested_h,
        monochrome_tint,
        current_color_hex,
        1.0,
    )
    .map(|(pm, _, _)| pm)
}

/// Rasterizes a local dictionary image or SVG at `render_scale` physical density,
/// returning `(high_res_pixmap, logical_width_px, logical_height_px)` backed by a
/// bounded LRU cache so images/SVGs aren't re-read or re-rasterized per glossary render.
pub(super) fn load_dictionary_image_pixmap_scaled(
    asset_path: &Path,
    requested_w: Option<f32>,
    requested_h: Option<f32>,
    monochrome_tint: Option<[u8; 4]>,
    current_color_hex: &str,
    render_scale: f32,
) -> Option<(Arc<tiny_skia::Pixmap>, f32, f32)> {
    load_dictionary_image_pixmap_scaled_inner(
        None,
        asset_path,
        requested_w,
        requested_h,
        monochrome_tint,
        current_color_hex,
        render_scale,
    )
}

/// Same as `load_dictionary_image_pixmap_scaled`, reusing a caller-owned `TextEngine`
/// (e.g. a background `GlossaryRenderWorker`'s engine) for SVG font lookup.
pub(super) fn load_dictionary_image_pixmap_scaled_with_engine(
    engine: &mut TextEngine,
    asset_path: &Path,
    requested_w: Option<f32>,
    requested_h: Option<f32>,
    monochrome_tint: Option<[u8; 4]>,
    current_color_hex: &str,
    render_scale: f32,
) -> Option<(Arc<tiny_skia::Pixmap>, f32, f32)> {
    load_dictionary_image_pixmap_scaled_inner(
        Some(engine),
        asset_path,
        requested_w,
        requested_h,
        monochrome_tint,
        current_color_hex,
        render_scale,
    )
}

fn load_dictionary_image_pixmap_scaled_inner(
    engine: Option<&mut TextEngine>,
    asset_path: &Path,
    requested_w: Option<f32>,
    requested_h: Option<f32>,
    monochrome_tint: Option<[u8; 4]>,
    current_color_hex: &str,
    render_scale: f32,
) -> Option<(Arc<tiny_skia::Pixmap>, f32, f32)> {
    let scale = render_scale.clamp(1.0, 3.0);
    let scale_quarter = (scale * 4.0).round().clamp(4.0, 12.0) as u16;
    let meta = std::fs::metadata(asset_path).ok()?;
    let cache_key = DictAssetCacheKey {
        path: asset_path.to_path_buf(),
        mtime: meta.modified().ok(),
        file_len: meta.len(),
        width_tenth: requested_w.map(|w| (w * 10.0).round() as i32),
        height_tenth: requested_h.map(|h| (h * 10.0).round() as i32),
        monochrome_tint,
        current_color: current_color_hex.trim().to_ascii_lowercase(),
        scale_quarter,
    };
    {
        let mut cache = shared_dict_asset_cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(hit) = cache.get(&cache_key) {
            return Some(hit);
        }
    }

    let result = decode_and_rasterize_dictionary_asset(
        engine,
        asset_path,
        requested_w,
        requested_h,
        monochrome_tint,
        current_color_hex,
        scale,
    )?;
    {
        let mut cache = shared_dict_asset_cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.insert(cache_key, result.0.clone(), result.1, result.2);
    }
    Some(result)
}

fn decode_and_rasterize_dictionary_asset(
    engine: Option<&mut TextEngine>,
    asset_path: &Path,
    requested_w: Option<f32>,
    requested_h: Option<f32>,
    monochrome_tint: Option<[u8; 4]>,
    current_color_hex: &str,
    scale: f32,
) -> Option<(Arc<tiny_skia::Pixmap>, f32, f32)> {
    let bytes = std::fs::read(asset_path).ok()?;
    let ext = asset_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let trimmed_head = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .map_or(&[][..], |idx| &bytes[idx..]);
    let looks_svg = ext == "svg"
        || ext == "svgz"
        || trimmed_head.starts_with(b"<svg")
        || trimmed_head.starts_with(b"<?xml");

    if looks_svg {
        let mut opts = usvg::Options::default();
        opts.resources_dir = asset_path.parent().map(Path::to_path_buf);
        opts.font_family = "sans-serif".to_owned();
        opts.fontdb = match engine {
            Some(eng) => {
                eng.ensure_default_svg_fonts();
                eng.fontdb.clone()
            }
            None => with_shared_text_engine(|eng| {
                eng.ensure_default_svg_fonts();
                eng.fontdb.clone()
            }),
        };
        if !current_color_hex.is_empty() {
            opts.style_sheet = Some(format!("svg {{ color: {current_color_hex}; }}"));
        }
        let tree = usvg::Tree::from_data(&bytes, &opts).ok()?;
        let intrinsic_w = tree.size().width().max(1.0);
        let intrinsic_h = tree.size().height().max(1.0);
        let (target_w, target_h) = match (requested_w, requested_h) {
            (Some(w), Some(h)) => (w.max(1.0), h.max(1.0)),
            (Some(w), None) => {
                let w = w.max(1.0);
                (w, (w * intrinsic_h / intrinsic_w).max(1.0))
            }
            (None, Some(h)) => {
                let h = h.max(1.0);
                ((h * intrinsic_w / intrinsic_h).max(1.0), h)
            }
            (None, None) => (intrinsic_w.clamp(8.0, 360.0), intrinsic_h.clamp(8.0, 360.0)),
        };
        let logical_w = target_w.round().clamp(1.0, 1024.0);
        let logical_h = target_h.round().clamp(1.0, 1024.0);
        let pw = (logical_w * scale).round().clamp(1.0, 2048.0) as u32;
        let ph = (logical_h * scale).round().clamp(1.0, 2048.0) as u32;
        let mut pixmap = tiny_skia::Pixmap::new(pw, ph)?;
        let transform = tiny_skia::Transform::from_scale(
            pw as f32 / intrinsic_w,
            ph as f32 / intrinsic_h,
        );
        resvg::render(&tree, transform, &mut pixmap.as_mut());

        if let Some([tr, tg, tb, ta]) = monochrome_tint {
            for px in pixmap.pixels_mut() {
                let a = (u32::from(px.alpha()) * u32::from(ta) + 127) / 255;
                let r = (u32::from(tr) * a + 127) / 255;
                let g = (u32::from(tg) * a + 127) / 255;
                let b = (u32::from(tb) * a + 127) / 255;
                if let Some(tinted) =
                    tiny_skia::PremultipliedColorU8::from_rgba(r as u8, g as u8, b as u8, a as u8)
                {
                    *px = tinted;
                }
            }
        }
        return Some((Arc::new(pixmap), logical_w, logical_h));
    }

    let decoded = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let iw = decoded.width().max(1);
    let ih = decoded.height().max(1);
    let mut src_pixmap = tiny_skia::Pixmap::new(iw, ih)?;
    for (dst, src) in src_pixmap.pixels_mut().iter_mut().zip(decoded.pixels()) {
        let [r, g, b, a] = src.0;
        let pr = (u32::from(r) * u32::from(a) + 127) / 255;
        let pg = (u32::from(g) * u32::from(a) + 127) / 255;
        let pb = (u32::from(b) * u32::from(a) + 127) / 255;
        if let Some(pm) =
            tiny_skia::PremultipliedColorU8::from_rgba(pr as u8, pg as u8, pb as u8, a)
        {
            *dst = pm;
        }
    }
    let (target_w, target_h) = match (requested_w, requested_h) {
        (Some(w), Some(h)) => (w.max(1.0), h.max(1.0)),
        (Some(w), None) => {
            let w = w.max(1.0);
            (w, (w * ih as f32 / iw as f32).max(1.0))
        }
        (None, Some(h)) => {
            let h = h.max(1.0);
            ((h * iw as f32 / ih as f32).max(1.0), h)
        }
        (None, None) => {
            let fit = (340.0 / iw as f32).min(1.0);
            (iw as f32 * fit, ih as f32 * fit)
        }
    };
    let logical_w = target_w.round().clamp(1.0, 1024.0);
    let logical_h = target_h.round().clamp(1.0, 1024.0);
    let pw = (logical_w * scale).round().clamp(1.0, 2048.0) as u32;
    let ph = (logical_h * scale).round().clamp(1.0, 2048.0) as u32;
    if (pw == iw && ph == ih) || (scale > 1.0 && iw <= pw && ih <= ph) {
        return Some((Arc::new(src_pixmap), logical_w, logical_h));
    }
    let mut dst_pixmap = tiny_skia::Pixmap::new(pw, ph)?;
    let paint = tiny_skia::PixmapPaint {
        opacity: 1.0,
        blend_mode: tiny_skia::BlendMode::SourceOver,
        quality: tiny_skia::FilterQuality::Bilinear,
    };
    dst_pixmap.draw_pixmap(
        0,
        0,
        src_pixmap.as_ref(),
        &paint,
        tiny_skia::Transform::from_scale(pw as f32 / iw as f32, ph as f32 / ih as f32),
        None,
    );
    Some((Arc::new(dst_pixmap), logical_w, logical_h))
}
