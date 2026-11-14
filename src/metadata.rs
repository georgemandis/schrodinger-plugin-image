//! Reading and removing image metadata without touching the pixels.
//!
//! A photo's file carries more than the picture: where it was taken (GPS),
//! when, on what device, sometimes the owner's name, an embedded thumbnail of
//! the uncropped original, and editing history. For JPEG and PNG that data
//! sits in its own segments or chunks beside the compressed image, so it can
//! be dropped by copying the file and leaving those out. The image data is
//! not decoded or re-compressed, so there is no quality loss.
//!
//! What is deliberately kept: the color profile (removing it changes how the
//! colors look) and the markers a decoder needs.

/// What was found in an image's metadata.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Found {
    /// EXIF orientation (1 = upright, 2..=8 = stored rotated or mirrored).
    pub orientation: u8,
    pub has_exif: bool,
    pub has_gps: bool,
    pub has_xmp: bool,
    pub has_iptc: bool,
    pub has_comment: bool,
}

impl Found {
    pub fn anything(&self) -> bool {
        self.has_exif || self.has_gps || self.has_xmp || self.has_iptc || self.has_comment
    }

    /// The picture is stored sideways or mirrored and relies on metadata to
    /// be shown upright. Removing the metadata without redrawing it would
    /// leave it rotated.
    pub fn needs_rotation(&self) -> bool {
        (2..=8).contains(&self.orientation)
    }
}

// --- EXIF -------------------------------------------------------------------

/// Read orientation and whether a GPS block exists from an EXIF payload
/// (the bytes after the "Exif\0\0" header: a TIFF structure).
fn read_exif(tiff: &[u8], found: &mut Found) {
    let le = match tiff.get(0..2) {
        Some(b"II") => true,
        Some(b"MM") => false,
        _ => return,
    };
    let u16_at = |o: usize| -> Option<u16> {
        let b = tiff.get(o..o + 2)?;
        Some(if le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) })
    };
    let u32_at = |o: usize| -> Option<u32> {
        let b = tiff.get(o..o + 4)?;
        Some(if le { u32::from_le_bytes([b[0], b[1], b[2], b[3]]) } else { u32::from_be_bytes([b[0], b[1], b[2], b[3]]) })
    };
    let Some(ifd0) = u32_at(4).map(|v| v as usize) else { return };
    let Some(count) = u16_at(ifd0) else { return };
    for i in 0..count.min(512) as usize {
        let entry = ifd0 + 2 + i * 12;
        let Some(tag) = u16_at(entry) else { return };
        match tag {
            0x0112 => {
                if let Some(v) = u16_at(entry + 8) {
                    if (1..=8).contains(&v) {
                        found.orientation = v as u8;
                    }
                }
            }
            0x8825 => found.has_gps = true,
            _ => {}
        }
    }
}

// --- JPEG -------------------------------------------------------------------

const SOI: u8 = 0xD8;
const SOS: u8 = 0xDA;
const EOI: u8 = 0xD9;

pub fn is_jpeg(bytes: &[u8]) -> bool {
    bytes.len() > 3 && bytes[0] == 0xFF && bytes[1] == SOI && bytes[2] == 0xFF
}

/// One header segment of a JPEG: its marker and its full bytes (FF, marker,
/// length, payload).
struct Segment<'a> {
    marker: u8,
    bytes: &'a [u8],
    payload: &'a [u8],
}

/// Split a JPEG into its header segments and the remainder that starts at
/// the image data. None if the structure is not what we expect.
fn jpeg_segments(bytes: &[u8]) -> Option<(Vec<Segment<'_>>, &[u8])> {
    if !is_jpeg(bytes) {
        return None;
    }
    let mut segments = Vec::new();
    let mut pos = 2;
    loop {
        // Markers may be preceded by fill bytes (FF FF ...).
        while bytes.get(pos) == Some(&0xFF) && bytes.get(pos + 1) == Some(&0xFF) {
            pos += 1;
        }
        if bytes.get(pos) != Some(&0xFF) {
            return None;
        }
        let marker = *bytes.get(pos + 1)?;
        if marker == SOS || marker == EOI {
            // Everything from here is image data: copied verbatim.
            return Some((segments, &bytes[pos..]));
        }
        // Standalone markers have no length.
        if (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            segments.push(Segment { marker, bytes: &bytes[pos..pos + 2], payload: &[] });
            pos += 2;
            continue;
        }
        let len = u16::from_be_bytes([*bytes.get(pos + 2)?, *bytes.get(pos + 3)?]) as usize;
        if len < 2 {
            return None;
        }
        let end = pos + 2 + len;
        let whole = bytes.get(pos..end)?;
        segments.push(Segment { marker, bytes: whole, payload: &whole[4..] });
        pos = end;
    }
}

/// Is this header segment metadata about the photo (as opposed to something
/// needed to decode or correctly color the image)?
fn jpeg_segment_is_metadata(seg: &Segment<'_>) -> bool {
    match seg.marker {
        // APP0: JFIF/JFXX, part of the format.
        0xE0 => false,
        // APP2: the ICC color profile is kept; other APP2 uses (multi-picture
        // index, FlashPix) are not.
        0xE2 => !seg.payload.starts_with(b"ICC_PROFILE\0"),
        // APP14: Adobe's color-transform flag, which decoders need.
        0xEE => false,
        // Every other application segment: EXIF and XMP (APP1), IPTC and
        // Photoshop (APP13), maker and vendor blocks.
        0xE1..=0xEF => true,
        // Free-text comment.
        0xFE => true,
        _ => false,
    }
}

pub fn inspect_jpeg(bytes: &[u8]) -> Found {
    let mut found = Found { orientation: 1, ..Default::default() };
    let Some((segments, _)) = jpeg_segments(bytes) else { return found };
    for seg in &segments {
        match seg.marker {
            0xE1 if seg.payload.starts_with(b"Exif\0\0") => {
                found.has_exif = true;
                read_exif(&seg.payload[6..], &mut found);
            }
            0xE1 if seg.payload.starts_with(b"http://ns.adobe.com/") => found.has_xmp = true,
            0xED => found.has_iptc = true,
            0xFE => found.has_comment = true,
            _ => {}
        }
    }
    found
}

/// A copy of the JPEG without its metadata segments. The compressed image
/// data is untouched. None if the file's structure could not be followed.
///
/// The caller must check `Found::needs_rotation` first: this removes the
/// orientation tag along with everything else.
pub fn strip_jpeg(bytes: &[u8]) -> Option<Vec<u8>> {
    let (segments, rest) = jpeg_segments(bytes)?;
    let mut out = Vec::with_capacity(bytes.len());
    out.extend_from_slice(&[0xFF, SOI]);
    for seg in &segments {
        if !jpeg_segment_is_metadata(seg) {
            out.extend_from_slice(seg.bytes);
        }
    }
    out.extend_from_slice(rest);
    Some(out)
}

// --- PNG --------------------------------------------------------------------

const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

pub fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(PNG_SIGNATURE)
}

/// PNG chunks that describe the picture rather than being part of it: text
/// fields (title, author, software, and XMP inside iTXt), EXIF, and the
/// modification time.
fn png_chunk_is_metadata(kind: &[u8]) -> bool {
    matches!(kind, b"tEXt" | b"zTXt" | b"iTXt" | b"eXIf" | b"tIME")
}

fn png_chunks(bytes: &[u8]) -> Option<Vec<(&[u8], &[u8])>> {
    if !is_png(bytes) {
        return None;
    }
    let mut chunks = Vec::new();
    let mut pos = 8;
    while pos < bytes.len() {
        let len = u32::from_be_bytes(bytes.get(pos..pos + 4)?.try_into().ok()?) as usize;
        let kind = bytes.get(pos + 4..pos + 8)?;
        // length + type + data + CRC
        let whole = bytes.get(pos..pos + 12 + len)?;
        chunks.push((kind, whole));
        pos += 12 + len;
        if kind == b"IEND" {
            break;
        }
    }
    Some(chunks)
}

pub fn inspect_png(bytes: &[u8]) -> Found {
    let mut found = Found { orientation: 1, ..Default::default() };
    let Some(chunks) = png_chunks(bytes) else { return found };
    for (kind, whole) in chunks {
        match kind {
            b"eXIf" => {
                found.has_exif = true;
                read_exif(&whole[8..whole.len() - 4], &mut found);
            }
            b"iTXt" if whole[8..].starts_with(b"XML:com.adobe.xmp") => found.has_xmp = true,
            b"tEXt" | b"zTXt" | b"iTXt" => found.has_comment = true,
            _ => {}
        }
    }
    found
}

/// A copy of the PNG without its metadata chunks. Pixel data untouched.
pub fn strip_png(bytes: &[u8]) -> Option<Vec<u8>> {
    let chunks = png_chunks(bytes)?;
    let mut out = Vec::with_capacity(bytes.len());
    out.extend_from_slice(PNG_SIGNATURE);
    for (kind, whole) in chunks {
        if !png_chunk_is_metadata(kind) {
            out.extend_from_slice(whole);
        }
    }
    Some(out)
}

pub fn inspect(bytes: &[u8]) -> Found {
    if is_jpeg(bytes) {
        inspect_jpeg(bytes)
    } else if is_png(bytes) {
        inspect_png(bytes)
    } else {
        Found { orientation: 1, ..Default::default() }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// An EXIF APP1 payload with an orientation tag and, optionally, a GPS
    /// block pointer.
    pub fn exif_payload(orientation: u16, gps: bool) -> Vec<u8> {
        let mut t = Vec::new();
        t.extend_from_slice(b"Exif\0\0");
        t.extend_from_slice(b"II*\0");
        t.extend_from_slice(&8u32.to_le_bytes());
        let entries: u16 = if gps { 2 } else { 1 };
        t.extend_from_slice(&entries.to_le_bytes());
        // Orientation: tag 0x0112, type SHORT, count 1, value
        t.extend_from_slice(&0x0112u16.to_le_bytes());
        t.extend_from_slice(&3u16.to_le_bytes());
        t.extend_from_slice(&1u32.to_le_bytes());
        t.extend_from_slice(&orientation.to_le_bytes());
        t.extend_from_slice(&[0, 0]);
        if gps {
            // GPS IFD pointer: tag 0x8825, type LONG, count 1, offset
            t.extend_from_slice(&0x8825u16.to_le_bytes());
            t.extend_from_slice(&4u16.to_le_bytes());
            t.extend_from_slice(&1u32.to_le_bytes());
            t.extend_from_slice(&200u32.to_le_bytes());
        }
        t.extend_from_slice(&0u32.to_le_bytes());
        t
    }

    fn segment(marker: u8, payload: &[u8]) -> Vec<u8> {
        let mut s = vec![0xFF, marker];
        s.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        s.extend_from_slice(payload);
        s
    }

    /// Insert segments right after SOI of an encoded JPEG.
    pub fn jpeg_with(base: &[u8], extra: &[Vec<u8>]) -> Vec<u8> {
        let mut out = vec![0xFF, SOI];
        for e in extra {
            out.extend_from_slice(e);
        }
        out.extend_from_slice(&base[2..]);
        out
    }

    pub fn plain_jpeg(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb([(x * 7 % 256) as u8, (y * 5 % 256) as u8, 120]));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img).write_to(&mut out, image::ImageFormat::Jpeg).unwrap();
        out.into_inner()
    }

    pub fn tagged_jpeg(orientation: u16, gps: bool) -> Vec<u8> {
        jpeg_with(
            &plain_jpeg(40, 20),
            &[
                segment(0xE1, &exif_payload(orientation, gps)),
                segment(0xE1, b"http://ns.adobe.com/xap/1.0/\0<x:xmpmeta>secret</x:xmpmeta>"),
                segment(0xE2, b"ICC_PROFILE\0\x01\x01fake-profile-bytes"),
                segment(0xE2, b"MPF\0multi-picture"),
                segment(0xED, b"Photoshop 3.0\08BIM caption"),
                segment(0xFE, b"shot on my phone"),
            ],
        )
    }

    #[test]
    fn finds_metadata_in_a_jpeg() {
        let found = inspect_jpeg(&tagged_jpeg(6, true));
        assert_eq!(found, Found { orientation: 6, has_exif: true, has_gps: true, has_xmp: true, has_iptc: true, has_comment: true });
        assert!(found.needs_rotation());

        let clean = inspect_jpeg(&plain_jpeg(10, 10));
        assert!(!clean.anything());
        assert!(!clean.needs_rotation());
    }

    #[test]
    fn strips_a_jpeg_without_touching_the_image_data() {
        let base = plain_jpeg(40, 20);
        let tagged = tagged_jpeg(1, true);
        let stripped = strip_jpeg(&tagged).unwrap();

        assert!(!inspect_jpeg(&stripped).anything());
        for needle in [&b"Exif"[..], b"xmpmeta", b"secret", b"Photoshop", b"shot on my phone", b"MPF"] {
            assert!(!stripped.windows(needle.len()).any(|w| w == needle), "{:?} survived", String::from_utf8_lossy(needle));
        }
        // The color profile is kept...
        assert!(stripped.windows(11).any(|w| w == b"ICC_PROFILE"));
        // ...and apart from it the result is byte-for-byte the untagged file:
        // nothing was re-compressed.
        let icc = segment(0xE2, b"ICC_PROFILE\0\x01\x01fake-profile-bytes");
        assert_eq!(stripped, jpeg_with(&base, &[icc]));

        let decoded = image::load_from_memory(&stripped).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (40, 20));
    }

    #[test]
    fn rejects_things_that_are_not_jpeg() {
        assert!(strip_jpeg(b"not an image").is_none());
        assert!(strip_jpeg(&[0xFF, 0xD8, 0xFF, 0xE1, 0xFF]).is_none(), "truncated segment");
    }

    pub fn tagged_png() -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, 4, 4);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.add_text_chunk("Author".into(), "George".into()).unwrap();
            enc.add_itxt_chunk("XML:com.adobe.xmp".into(), "<x:xmpmeta>secret</x:xmpmeta>".into()).unwrap();
            let mut w = enc.write_header().unwrap();
            w.write_image_data(&[200u8; 4 * 4 * 4]).unwrap();
        }
        out
    }

    #[test]
    fn strips_a_png_keeping_the_pixels() {
        let tagged = tagged_png();
        let found = inspect_png(&tagged);
        assert!(found.has_comment && found.has_xmp);

        let stripped = strip_png(&tagged).unwrap();
        assert!(!inspect_png(&stripped).anything());
        assert!(!stripped.windows(6).any(|w| w == b"George"));
        assert!(!stripped.windows(6).any(|w| w == b"secret"));
        let before = image::load_from_memory(&tagged).unwrap().to_rgba8();
        let after = image::load_from_memory(&stripped).unwrap().to_rgba8();
        assert_eq!(before, after);
        assert!(strip_png(b"\x89PNG\r\n\x1a\n\0\0").is_none(), "truncated");
    }
}
