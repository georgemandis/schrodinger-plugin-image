//! Schrödinger "Image" plugin: remove metadata, convert, and resize.
//!
//! Works on an image that is on the clipboard as pixels (a screenshot, an
//! image copied from a web page) and on a single image *file* copied in the
//! file manager. For a copied file the result is a new file, so pasting into
//! Mail or a chat attaches the processed copy; the original is never changed.
//!
//!   Remove metadata   drop location, camera, timestamps and comments
//!   As PNG / As JPEG  convert
//!   Resize            fit within a size chosen in the plugin's settings
//!   Remove background keep the subject, on a transparent background
//!
//! HEIC photos (the iPhone's default format) and background removal go
//! through the operating system by way of loupe, so they work on macOS.

use schrodinger_plugin_sdk::prelude::*;
use schrodinger_plugin_sdk::schrodinger_plugin;
use std::path::{Path, PathBuf};

mod metadata;

#[derive(Default)]
pub struct ImagePlugin;

const PNG_FORMATS: &[&str] = &["public.png", "PNG", "image/png"];
const JPEG_FORMATS: &[&str] = &["public.jpeg", "public.jpg", "image/jpeg", "JFIF"];
const TIFF_FORMATS: &[&str] = &["public.tiff", "image/tiff"];
const HEIF_FORMATS: &[&str] = &["public.heic", "public.heif", "image/heic", "image/heif"];
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "tif", "tiff", "gif", "bmp", "webp", "heic", "heif"];
/// Larger files are left alone; decoding them would stall the paste.
const MAX_FILE_BYTES: u64 = 150 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Png,
    Jpeg,
}

impl Kind {
    fn ext(self) -> &'static str {
        match self {
            Kind::Png => "png",
            Kind::Jpeg => "jpg",
        }
    }
}

/// The image to work on, and the file it came from if it was a copied file.
pub struct Source {
    pub bytes: Vec<u8>,
    pub file: Option<PathBuf>,
}

fn has_any(formats: &[String], names: &[&str]) -> bool {
    formats.iter().any(|f| names.iter().any(|n| f.eq_ignore_ascii_case(n)))
}

fn read_any(entry: &ClipEntry, names: &[&str]) -> Option<Vec<u8>> {
    names
        .iter()
        .find(|n| entry.meta.formats.iter().any(|f| f.eq_ignore_ascii_case(n)))
        .and_then(|n| entry.read_format(n).ok())
}

// --- copied files -------------------------------------------------------------

fn path_from_file_url(url: &str) -> Option<PathBuf> {
    let rest = url.trim().trim_end_matches('\0').strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    let bytes = rest.as_bytes();
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).ok().map(PathBuf::from)
}

/// Every file a clip refers to. macOS lists them all in
/// `NSFilenamesPboardType`; `public.file-url` holds only the first.
fn copied_paths(entry: &ClipEntry) -> Vec<PathBuf> {
    if let Some(bytes) = read_any(entry, &["NSFilenamesPboardType"]) {
        if let Ok(plist::Value::Array(items)) = plist::Value::from_reader(std::io::Cursor::new(&bytes)) {
            let paths: Vec<PathBuf> = items.iter().filter_map(|v| v.as_string()).map(PathBuf::from).collect();
            if !paths.is_empty() {
                return paths;
            }
        }
    }
    if let Some(bytes) = read_any(entry, &["text/uri-list"]) {
        let paths: Vec<PathBuf> = String::from_utf8_lossy(&bytes)
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
            .filter_map(path_from_file_url)
            .collect();
        if !paths.is_empty() {
            return paths;
        }
    }
    if let Some(bytes) = read_any(entry, &["CF_HDROP"]) {
        if bytes.len() >= 20 {
            let offset = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
            if let Some(list) = bytes.get(offset..) {
                let units: Vec<u16> = list.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                let paths: Vec<PathBuf> = String::from_utf16_lossy(&units)
                    .split('\0')
                    .take_while(|s| !s.is_empty())
                    .map(PathBuf::from)
                    .collect();
                if !paths.is_empty() {
                    return paths;
                }
            }
        }
    }
    read_any(entry, &["public.file-url"])
        .and_then(|b| path_from_file_url(&String::from_utf8_lossy(&b)))
        .into_iter()
        .collect()
}

fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| IMAGE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// The one copied image file, if the clip is exactly that.
fn copied_image_file(entry: &ClipEntry) -> Option<PathBuf> {
    let paths = copied_paths(entry);
    match paths.as_slice() {
        [only] if is_image_path(only) => {
            let meta = std::fs::metadata(only).ok()?;
            (meta.is_file() && meta.len() <= MAX_FILE_BYTES).then(|| only.clone())
        }
        _ => None,
    }
}

fn source(entry: &ClipEntry) -> Result<Source, PluginError> {
    if let Some(path) = copied_image_file(entry) {
        let bytes = std::fs::read(&path).map_err(|e| PluginError(format!("could not read {}: {}", path.display(), e)))?;
        return Ok(Source { bytes, file: Some(path) });
    }
    // Prefer the encoding the image arrived in: a JPEG on the clipboard is
    // the original file's bytes, metadata included, while a TIFF or PNG next
    // to it is usually a re-rendering the copying app added.
    for names in [JPEG_FORMATS, HEIF_FORMATS, PNG_FORMATS, TIFF_FORMATS] {
        if let Some(bytes) = read_any(entry, names) {
            if !bytes.is_empty() {
                return Ok(Source { bytes, file: None });
            }
        }
    }
    Err(PluginError::from("this clip has no image"))
}

// --- operations ---------------------------------------------------------------

/// Hand the bytes to loupe as a file, which is what its API takes.
fn with_temp_file<T>(bytes: &[u8], f: impl FnOnce(&Path) -> T) -> Result<T, PluginError> {
    use std::io::Write;
    let mut file = tempfile::NamedTempFile::new().map_err(|e| PluginError(format!("could not create a temporary file: {e}")))?;
    file.write_all(bytes).and_then(|_| file.flush()).map_err(|e| PluginError(format!("could not write a temporary file: {e}")))?;
    Ok(f(file.path()))
}

fn from_pixels(px: loupe_rs::Pixels) -> Result<image::DynamicImage, PluginError> {
    image::RgbaImage::from_raw(px.width, px.height, px.rgba)
        .map(image::DynamicImage::ImageRgba8)
        .ok_or_else(|| PluginError::from("the system returned a malformed image"))
}

/// Decode with the operating system. Reads what the `image` crate cannot,
/// HEIC above all, and returns the picture already upright.
fn decode_with_system(bytes: &[u8]) -> Result<image::DynamicImage, PluginError> {
    let px = with_temp_file(bytes, loupe_rs::load_pixels)?.map_err(|e| PluginError(format!("could not read this image: {e}")))?;
    let img = from_pixels(px)?;
    // Photos have no transparency; drop the unused alpha channel so they
    // are treated as photos from here on.
    Ok(if img.as_rgba8().is_some_and(|p| p.pixels().all(|p| p[3] == 255)) {
        image::DynamicImage::ImageRgb8(img.to_rgb8())
    } else {
        img
    })
}

fn decode(bytes: &[u8]) -> Result<image::DynamicImage, PluginError> {
    if metadata::is_heif(bytes) {
        return decode_with_system(bytes).map_err(|e| {
            if cfg!(target_os = "macos") {
                e
            } else {
                PluginError::from("HEIC photos can only be read on macOS")
            }
        });
    }
    let img = match image::load_from_memory(bytes) {
        Ok(img) => img,
        // Something the `image` crate does not know; the system may.
        Err(e) => return decode_with_system(bytes).map_err(|_| PluginError(format!("could not read this image: {e}"))),
    };
    // Draw the picture the way it is meant to be seen. After this the
    // orientation tag is no longer needed (and re-encoding drops it).
    Ok(match metadata::inspect(bytes).orientation {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.rotate90().fliph(),
        6 => img.rotate90(),
        7 => img.rotate270().fliph(),
        8 => img.rotate270(),
        _ => img,
    })
}

fn encode_png(img: &image::DynamicImage) -> Result<Vec<u8>, PluginError> {
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).map_err(|e| PluginError(format!("could not write PNG: {e}")))?;
    Ok(out.into_inner())
}

fn encode_jpeg(img: &image::DynamicImage, quality: u8) -> Result<Vec<u8>, PluginError> {
    // JPEG has no transparency; put transparent areas on white rather than
    // letting them turn black.
    let rgba = img.to_rgba8();
    let mut rgb = image::RgbImage::new(rgba.width(), rgba.height());
    for (x, y, p) in rgba.enumerate_pixels() {
        let a = p[3] as u32;
        let over = |c: u8| ((c as u32 * a + 255 * (255 - a) + 127) / 255) as u8;
        rgb.put_pixel(x, y, image::Rgb([over(p[0]), over(p[1]), over(p[2])]));
    }
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
        .encode_image(&rgb)
        .map_err(|e| PluginError(format!("could not write JPEG: {e}")))?;
    Ok(out)
}

pub struct Settings {
    pub max_size: u32,
    pub jpeg_quality: u8,
    pub trim_cutout: bool,
}

impl Settings {
    fn from(config: &serde_json::Value) -> Self {
        let max_size = config.get("maxSize").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(1600);
        let jpeg_quality = match config.get("jpegQuality").and_then(|v| v.as_str()).unwrap_or("High") {
            "Low" => 60,
            "Medium" => 75,
            "Maximum" => 95,
            _ => 85,
        };
        let trim_cutout = config.get("cutoutSize").and_then(|v| v.as_str()) != Some("Keep the original size");
        Settings { max_size, jpeg_quality, trim_cutout }
    }
}

/// Crop to the part of the image that is not transparent.
fn trim_transparent(img: image::RgbaImage) -> image::RgbaImage {
    // Ignore the faintest pixels: a soft mask leaves stray near-zero alpha
    // far from the subject.
    const VISIBLE: u8 = 8;
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    for (x, y, p) in img.enumerate_pixels() {
        if p[3] > VISIBLE {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    if x0 > x1 {
        return img;
    }
    image::imageops::crop_imm(&img, x0, y0, x1 - x0 + 1, y1 - y0 + 1).to_image()
}

fn cutout(bytes: &[u8], settings: &Settings) -> Result<Vec<u8>, PluginError> {
    use loupe_rs::CutoutError;
    let px = with_temp_file(bytes, loupe_rs::remove_background)?.map_err(|e| match e {
        CutoutError::NoSubject => PluginError::from("No subject found to separate from the background"),
        CutoutError::Unsupported => PluginError::from("Removing the background needs macOS 14 or later"),
        CutoutError::Failed(why) => PluginError(format!("could not remove the background: {why}")),
    })?;
    let img = from_pixels(px)?.into_rgba8();
    let img = if settings.trim_cutout { trim_transparent(img) } else { img };
    encode_png(&image::DynamicImage::ImageRgba8(img))
}

/// Run one mode. Returns the new image, its kind, and a suffix describing
/// what was done (used in the file name).
pub fn process(bytes: &[u8], mode: &str, settings: &Settings) -> Result<(Vec<u8>, Kind, String), PluginError> {
    let is_jpeg = metadata::is_jpeg(bytes);
    let is_png = metadata::is_png(bytes);
    let is_heif = metadata::is_heif(bytes);
    match mode {
        "cutout" => Ok((cutout(bytes, settings)?, Kind::Png, "-cutout".into())),
        "png" => Ok((encode_png(&decode(bytes)?)?, Kind::Png, String::new())),
        "jpeg" => Ok((encode_jpeg(&decode(bytes)?, settings.jpeg_quality)?, Kind::Jpeg, String::new())),
        "resize" => {
            let img = decode(bytes)?;
            let longest = img.width().max(img.height());
            // A HEIC photo has no PNG-like use; it comes back as a JPEG.
            let as_jpeg = is_jpeg || is_heif;
            let kind = if as_jpeg { Kind::Jpeg } else { Kind::Png };
            if longest <= settings.max_size && (is_jpeg || is_png) && !metadata::inspect(bytes).needs_rotation() {
                // Already small enough: don't re-compress for nothing.
                return Ok((bytes.to_vec(), kind, String::new()));
            }
            let img = if longest > settings.max_size {
                img.resize(settings.max_size, settings.max_size, image::imageops::FilterType::Lanczos3)
            } else {
                img
            };
            let out = if as_jpeg { encode_jpeg(&img, settings.jpeg_quality)? } else { encode_png(&img)? };
            Ok((out, kind, format!("-{}", settings.max_size)))
        }
        // "strip" and anything unrecognized
        _ => {
            let found = metadata::inspect(bytes);
            if is_jpeg {
                // A photo stored sideways depends on its orientation tag, so
                // it has to be redrawn upright; that costs one high-quality
                // re-encode. Every other JPEG is cleaned without touching
                // the image data.
                if found.needs_rotation() {
                    return Ok((encode_jpeg(&decode(bytes)?, settings.jpeg_quality.max(92))?, Kind::Jpeg, "-clean".into()));
                }
                if let Some(clean) = metadata::strip_jpeg(bytes) {
                    return Ok((clean, Kind::Jpeg, "-clean".into()));
                }
                return Ok((encode_jpeg(&decode(bytes)?, settings.jpeg_quality.max(92))?, Kind::Jpeg, "-clean".into()));
            }
            if is_png {
                if let Some(clean) = metadata::strip_png(bytes) {
                    return Ok((clean, Kind::Png, "-clean".into()));
                }
            }
            if is_heif {
                // HEIC cannot be cleaned in place. Decoding takes only the
                // picture, so a high-quality JPEG of it carries nothing over.
                return Ok((encode_jpeg(&decode(bytes)?, settings.jpeg_quality.max(92))?, Kind::Jpeg, "-clean".into()));
            }
            // TIFF, WebP and the rest: re-encoding as PNG keeps every pixel
            // and carries no metadata over.
            Ok((encode_png(&decode(bytes)?)?, Kind::Png, "-clean".into()))
        }
    }
}

// --- output -------------------------------------------------------------------

fn platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    }
}

fn file_url(path: &Path) -> String {
    let mut out = String::from("file://");
    for b in path.to_string_lossy().as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => out.push(*b as char),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn file_reference(path: &Path) -> Output {
    match platform() {
        "macos" => Output::new("public.file-url").data(file_url(path).into_bytes()),
        "windows" => {
            let mut buf = Vec::new();
            for v in [20u32, 0, 0, 0, 1] {
                buf.extend_from_slice(&v.to_le_bytes());
            }
            for unit in path.to_string_lossy().encode_utf16() {
                buf.extend_from_slice(&unit.to_le_bytes());
            }
            buf.extend_from_slice(&[0, 0, 0, 0]);
            Output::new("CF_HDROP").data(buf)
        }
        _ => Output::new("text/uri-list").data(format!("{}\r\n", file_url(path)).into_bytes()),
    }
}

fn output_dir(change_count: i64) -> Result<PathBuf, PluginError> {
    let root = std::env::temp_dir().join("schrodinger-images");
    // Remove results older than an hour so the directory doesn't grow.
    if let Ok(entries) = std::fs::read_dir(&root) {
        let now = std::time::SystemTime::now();
        for e in entries.flatten() {
            let old = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| now.duration_since(t).ok())
                .map(|age| age.as_secs() > 3600)
                .unwrap_or(false);
            if old {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    let dir = root.join(change_count.to_string());
    std::fs::create_dir_all(&dir).map_err(|e| PluginError(format!("could not create {}: {}", dir.display(), e)))?;
    Ok(dir)
}

fn write_file(change_count: i64, stem: &str, suffix: &str, kind: Kind, bytes: &[u8]) -> Result<PathBuf, PluginError> {
    let path = output_dir(change_count)?.join(format!("{stem}{suffix}.{}", kind.ext()));
    std::fs::write(&path, bytes).map_err(|e| PluginError(format!("could not write {}: {}", path.display(), e)))?;
    Ok(path)
}

impl NativePlugin for ImagePlugin {
    fn analyze(&self, entry: &ClipEntry) -> Result<AnalysisData, PluginError> {
        let formats = &entry.meta.formats;
        let pixels = has_any(formats, PNG_FORMATS)
            || has_any(formats, JPEG_FORMATS)
            || has_any(formats, TIFF_FORMATS)
            || has_any(formats, HEIF_FORMATS);
        let applies = pixels || copied_image_file(entry).is_some();
        Ok(if applies { AnalysisData::new().set("is_image", true) } else { AnalysisData::empty() })
    }

    fn applies_to(&self, entry: &ClipEntry) -> bool {
        entry.analysis().has("is_image")
    }

    fn applies_to_mode(&self, _entry: &ClipEntry, mode: &str) -> bool {
        // Background removal uses the operating system, and only macOS has it.
        mode != "cutout" || cfg!(target_os = "macos")
    }

    fn apply(&self, entry: &ClipEntry, mode: Option<&str>) -> Result<Vec<Output>, PluginError> {
        let src = source(entry)?;
        let settings = Settings::from(&entry.config());
        let (bytes, kind, suffix) = process(&src.bytes, mode.unwrap_or("strip"), &settings)?;

        // A copied file comes back as a file, named after the original.
        if let Some(original) = &src.file {
            let stem = original.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "image".into());
            let path = write_file(entry.meta.change_count, &stem, &suffix, kind, &bytes)?;
            return Ok(vec![file_reference(&path)]);
        }

        // Pixels come back as pixels.
        let format = match (platform(), kind) {
            ("macos", Kind::Png) => "public.png",
            ("macos", Kind::Jpeg) => "public.jpeg",
            ("windows", Kind::Png) => "PNG",
            ("linux", Kind::Png) => "image/png",
            ("linux", Kind::Jpeg) => "image/jpeg",
            // Windows has no standard clipboard format for JPEG data.
            _ => {
                let path = write_file(entry.meta.change_count, "image", &suffix, kind, &bytes)?;
                return Ok(vec![file_reference(&path)]);
            }
        };
        Ok(vec![Output::new(format).data(bytes)])
    }
}

schrodinger_plugin!(ImagePlugin);

#[cfg(test)]
mod tests {
    use super::*;
    use metadata::tests::{plain_jpeg, tagged_jpeg, tagged_png};

    const SETTINGS: Settings = Settings { max_size: 1600, jpeg_quality: 85, trim_cutout: true };

    fn dims(bytes: &[u8]) -> (u32, u32) {
        let img = image::load_from_memory(bytes).unwrap();
        (img.width(), img.height())
    }

    #[test]
    fn strip_is_lossless_for_an_upright_jpeg() {
        let tagged = tagged_jpeg(1, true);
        let (out, kind, suffix) = process(&tagged, "strip", &SETTINGS).unwrap();
        assert_eq!((kind, suffix.as_str()), (Kind::Jpeg, "-clean"));
        assert!(!metadata::inspect(&out).anything());
        // Same compressed image data as a direct strip: nothing re-encoded.
        assert_eq!(out, metadata::strip_jpeg(&tagged).unwrap());
    }

    #[test]
    fn strip_redraws_a_sideways_photo_upright() {
        // 40x20 stored with orientation 6 (rotate 90 degrees to view).
        let tagged = tagged_jpeg(6, true);
        assert_eq!(dims(&tagged), (40, 20));
        let (out, _, _) = process(&tagged, "strip", &SETTINGS).unwrap();
        assert!(!metadata::inspect(&out).anything());
        assert_eq!(dims(&out), (20, 40), "pixels rotated, tag gone");
    }

    #[test]
    fn strip_cleans_png_and_converts_other_formats() {
        let (out, kind, _) = process(&tagged_png(), "strip", &SETTINGS).unwrap();
        assert_eq!(kind, Kind::Png);
        assert!(!metadata::inspect(&out).anything());

        let mut tiff = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(6, 3).write_to(&mut tiff, image::ImageFormat::Tiff).unwrap();
        let (out, kind, _) = process(&tiff.into_inner(), "strip", &SETTINGS).unwrap();
        assert_eq!(kind, Kind::Png);
        assert!(metadata::is_png(&out));
        assert_eq!(dims(&out), (6, 3));
    }

    #[test]
    fn converts_between_formats_and_drops_metadata() {
        let (png, kind, _) = process(&tagged_jpeg(6, true), "png", &SETTINGS).unwrap();
        assert_eq!(kind, Kind::Png);
        assert!(metadata::is_png(&png));
        assert_eq!(dims(&png), (20, 40), "orientation applied on conversion");
        assert!(!metadata::inspect(&png).anything());

        let (jpeg, kind, _) = process(&tagged_png(), "jpeg", &SETTINGS).unwrap();
        assert_eq!(kind, Kind::Jpeg);
        assert!(metadata::is_jpeg(&jpeg));
    }

    #[test]
    fn jpeg_conversion_puts_transparency_on_white() {
        let img = image::RgbaImage::from_pixel(8, 8, image::Rgba([0, 0, 0, 0]));
        let png = encode_png(&image::DynamicImage::ImageRgba8(img)).unwrap();
        let (jpeg, _, _) = process(&png, "jpeg", &SETTINGS).unwrap();
        let p = image::load_from_memory(&jpeg).unwrap().to_rgb8().get_pixel(4, 4).0;
        assert!(p.iter().all(|c| *c > 245), "expected white, got {p:?}");
    }

    #[test]
    fn resize_fits_within_the_limit_and_keeps_the_format() {
        let small = Settings { max_size: 100, jpeg_quality: 85, trim_cutout: true };
        let (out, kind, suffix) = process(&plain_jpeg(400, 200), "resize", &small).unwrap();
        assert_eq!((kind, suffix.as_str()), (Kind::Jpeg, "-100"));
        assert_eq!(dims(&out), (100, 50));

        let png = encode_png(&image::DynamicImage::new_rgba8(50, 300)).unwrap();
        let (out, kind, _) = process(&png, "resize", &small).unwrap();
        assert_eq!(kind, Kind::Png);
        assert_eq!(dims(&out), (17, 100));
    }

    #[test]
    fn resize_leaves_small_images_untouched() {
        let original = plain_jpeg(40, 20);
        let (out, _, suffix) = process(&original, "resize", &SETTINGS).unwrap();
        assert_eq!(out, original);
        assert_eq!(suffix, "");
    }

    #[test]
    fn unreadable_data_is_an_error_not_a_panic() {
        assert!(process(b"definitely not an image", "png", &SETTINGS).is_err());
        assert!(process(b"definitely not an image", "strip", &SETTINGS).is_err());
    }

    #[test]
    fn reads_settings() {
        let s = Settings::from(&serde_json::json!({ "maxSize": "640", "jpegQuality": "Maximum" }));
        assert_eq!((s.max_size, s.jpeg_quality), (640, 95));
        let d = Settings::from(&serde_json::json!({}));
        assert_eq!((d.max_size, d.jpeg_quality), (1600, 85));
        assert!(d.trim_cutout);
        let keep = Settings::from(&serde_json::json!({ "cutoutSize": "Keep the original size" }));
        assert!(!keep.trim_cutout);
    }

    #[test]
    fn trims_to_the_visible_part() {
        let mut img = image::RgbaImage::new(20, 10);
        img.put_pixel(19, 0, image::Rgba([0, 0, 0, 3])); // too faint to count
        for (x, y) in [(4, 2), (9, 6)] {
            img.put_pixel(x, y, image::Rgba([10, 20, 30, 255]));
        }
        let out = trim_transparent(img);
        assert_eq!((out.width(), out.height()), (6, 5));
        assert_eq!(out.get_pixel(0, 0).0, [10, 20, 30, 255]);

        let empty = trim_transparent(image::RgbaImage::new(4, 4));
        assert_eq!((empty.width(), empty.height()), (4, 4), "nothing visible: left as is");
    }

    #[test]
    fn recognizes_heic_by_its_header() {
        let mut heic = vec![0, 0, 0, 24];
        heic.extend_from_slice(b"ftypheic\0\0\0\0mif1heic");
        assert!(metadata::is_heif(&heic));
        let mut mp4 = vec![0, 0, 0, 24];
        mp4.extend_from_slice(b"ftypisom\0\0\0\0isomiso2");
        assert!(!metadata::is_heif(&mp4));
        assert!(!metadata::is_heif(&plain_jpeg(4, 4)));
        assert!(is_image_path(Path::new("/x/IMG_0001.HEIC")));
    }

    #[test]
    fn decodes_file_urls() {
        assert_eq!(path_from_file_url("file:///tmp/my%20photo.jpg"), Some(PathBuf::from("/tmp/my photo.jpg")));
        assert!(is_image_path(Path::new("/x/IMG_0001.JPG")));
        assert!(!is_image_path(Path::new("/x/notes.txt")));
    }
}

#[cfg(test)]
mod real_file {
    /// Run every mode on a real file: IMAGE_FILE=/path/to.jpg cargo test real_file -- --ignored --nocapture
    #[test]
    #[ignore]
    fn process_a_real_file() {
        let path = std::env::var("IMAGE_FILE").expect("set IMAGE_FILE");
        let bytes = std::fs::read(&path).unwrap();
        let before = super::metadata::inspect(&bytes);
        let img = super::decode(&bytes).unwrap();
        println!("input: {} bytes, {}x{} upright, {:?}", bytes.len(), img.width(), img.height(), before);
        let settings = super::Settings { max_size: 1024, jpeg_quality: 85, trim_cutout: true };
        let out_dir = std::env::var("OUT_DIR_IMAGES").ok();
        for mode in ["strip", "png", "jpeg", "resize", "cutout"] {
            let started = std::time::Instant::now();
            let (out, kind, suffix) = match super::process(&bytes, mode, &settings) {
                Ok(r) => r,
                Err(e) => {
                    println!("{mode:7} -> error: {}", e.0);
                    continue;
                }
            };
            let o = image::load_from_memory(&out).unwrap();
            println!("{mode:7} -> {:?}{suffix} {} bytes, {}x{}, metadata left: {}, {:?}", kind, out.len(), o.width(), o.height(), super::metadata::inspect(&out).anything(), started.elapsed());
            if let Some(dir) = &out_dir {
                std::fs::write(format!("{dir}/{mode}.{}", kind.ext()), &out).unwrap();
            }
        }
    }
}
