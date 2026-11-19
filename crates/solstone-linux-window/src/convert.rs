// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Turn a screenshot the journal can't keep (AVIF, HEIF, JPEG XL, BMP, TIFF and the
//! rest) into a PNG with the desktop's own imaging, gdk-pixbuf and the loaders
//! installed on this computer. The colour profile is carried over, and so is the
//! image's XMP packet when it has one. The original is never touched.

use gtk::gdk_pixbuf::Pixbuf;
use std::path::Path;

/// It converted.
pub const OK: i32 = 0;
/// The arguments were wrong.
pub const USAGE: i32 = 2;
/// The image can't be decoded here.
pub const UNREADABLE: i32 = 3;
/// The PNG couldn't be written.
pub const UNWRITABLE: i32 = 4;

pub fn run(args: &[String]) -> i32 {
    let [source, target] = args else {
        return USAGE;
    };
    convert(Path::new(source), Path::new(target))
}

/// Turn a HEIC/HEIF photo into a JPEG using gdk-pixbuf and the desktop's own
/// loaders, preserving its colour profile and EXIF capture metadata (with IFD0
/// Orientation stripped because `apply_embedded_orientation` rotates upright).
pub fn run_photo(args: &[String]) -> i32 {
    let [source, target] = args else {
        return USAGE;
    };
    convert_photo(Path::new(source), Path::new(target))
}

fn convert_photo(source: &Path, target: &Path) -> i32 {
    let pixbuf = match Pixbuf::from_file(source) {
        Ok(pixbuf) => pixbuf,
        Err(error) => {
            eprintln!("{error}");
            return UNREADABLE;
        }
    };
    let pixbuf = pixbuf.apply_embedded_orientation().unwrap_or(pixbuf);
    let icc = pixbuf.option("icc-profile");
    let mut options: Vec<(&str, &str)> = vec![("quality", "92")];
    if let Some(icc) = icc.as_deref() {
        options.push(("icc-profile", icc));
    }
    let bytes = match pixbuf.save_to_bufferv("jpeg", &options) {
        Ok(bytes) => bytes,
        Err(_) if icc.is_some() => match pixbuf.save_to_bufferv("jpeg", &[("quality", "92")]) {
            Ok(bytes) => bytes,
            Err(error) => {
                eprintln!("{error}");
                return UNWRITABLE;
            }
        },
        Err(error) => {
            eprintln!("{error}");
            return UNWRITABLE;
        }
    };
    let original = std::fs::read(source).unwrap_or_default();
    let bytes = carry_exif_to_jpeg(&original, bytes);
    match std::fs::write(target, bytes) {
        Ok(()) => OK,
        Err(error) => {
            eprintln!("{error}");
            UNWRITABLE
        }
    }
}

fn convert(source: &Path, target: &Path) -> i32 {
    let pixbuf = match Pixbuf::from_file(source) {
        Ok(pixbuf) => pixbuf,
        Err(error) => {
            eprintln!("{error}");
            return UNREADABLE;
        }
    };
    let pixbuf = pixbuf.apply_embedded_orientation().unwrap_or(pixbuf);
    let icc = pixbuf.option("icc-profile");
    let mut options: Vec<(&str, &str)> = Vec::new();
    if let Some(icc) = icc.as_deref() {
        options.push(("icc-profile", icc));
    }
    let bytes = match pixbuf.save_to_bufferv("png", &options) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("{error}");
            return UNWRITABLE;
        }
    };
    let original = std::fs::read(source).unwrap_or_default();
    let bytes = match xmp_packet(&original) {
        Some(xmp) => with_xmp(&bytes, xmp),
        None => bytes,
    };
    match std::fs::write(target, bytes) {
        Ok(()) => OK,
        Err(error) => {
            eprintln!("{error}");
            UNWRITABLE
        }
    }
}

/// The image's XMP packet, which every container stores as plain XML.
fn xmp_packet(bytes: &[u8]) -> Option<&[u8]> {
    let start = find(bytes, b"<x:xmpmeta")?;
    let end_tag = b"</x:xmpmeta>";
    let end = start + find(&bytes[start..], end_tag)? + end_tag.len();
    Some(&bytes[start..end])
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Put an XMP packet in the PNG as its `iTXt` `XML:com.adobe.xmp` chunk, after IHDR.
fn with_xmp(png: &[u8], xmp: &[u8]) -> Vec<u8> {
    const IHDR_END: usize = 8 + 4 + 4 + 13 + 4;
    if png.len() < IHDR_END || &png[12..16] != b"IHDR" {
        return png.to_vec();
    }
    let mut body = b"XML:com.adobe.xmp\0\0\0\0\0".to_vec();
    body.extend_from_slice(xmp);
    let mut chunk = Vec::with_capacity(body.len() + 12);
    chunk.extend_from_slice(&(body.len() as u32).to_be_bytes());
    let mut typed = b"iTXt".to_vec();
    typed.extend_from_slice(&body);
    chunk.extend_from_slice(&typed);
    chunk.extend_from_slice(&crc32(&typed).to_be_bytes());
    let mut out = Vec::with_capacity(png.len() + chunk.len());
    out.extend_from_slice(&png[..IHDR_END]);
    out.extend_from_slice(&chunk);
    out.extend_from_slice(&png[IHDR_END..]);
    out
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

// ── EXIF preservation for HEIF/HEIC → JPEG ──────────────────────────────────

const MAX_EXIF_ENTRIES: usize = 1_000;
const MAX_EXIF_DEPTH: u8 = 3;
const EXIF_IFD: u16 = 34_665;
const GPS_IFD: u16 = 34_853;
const INTEROP_IFD: u16 = 40_965;
const ORIENTATION_TAG: u16 = 274;

const IMAGE_TAGS: &[u16] = &[
    254,
    255,
    256,
    257,
    258,
    259,
    262,
    263,
    266,
    273,
    ORIENTATION_TAG,
    277,
    278,
    279,
    280,
    281,
    284,
    288,
    289,
    290,
    291,
    292,
    293,
    317,
    318,
    319,
    320,
    321,
    322,
    323,
    324,
    325,
    330,
    332,
    338,
    339,
    340,
    341,
    512,
    513,
    514,
    515,
    517,
    518,
    519,
    520,
    521,
    529,
    530,
    531,
    532,
    700,
    34_675,
];

fn carry_exif_to_jpeg(source: &[u8], jpeg: Vec<u8>) -> Vec<u8> {
    if jpeg_has_exif(&jpeg) {
        return jpeg;
    }
    match exif_blob(source).and_then(|blob| insert_jpeg_app1_exif(&jpeg, &blob)) {
        Some(with) => with,
        None => jpeg,
    }
}

fn exif_blob(source: &[u8]) -> Option<Vec<u8>> {
    if source.starts_with(b"II*\0") || source.starts_with(b"MM\0*") {
        return tiff_exif(source);
    }
    if source.get(4..8) == Some(b"ftyp") {
        return heif_exif(source).and_then(|tiff| tiff_exif(&tiff));
    }
    None
}

fn jpeg_has_exif(jpeg: &[u8]) -> bool {
    if !jpeg.starts_with(&[0xff, 0xd8]) {
        return false;
    }
    let mut at = 2usize;
    while at + 1 < jpeg.len() {
        if jpeg[at] != 0xff {
            return false;
        }
        while at < jpeg.len() && jpeg[at] == 0xff {
            at += 1;
        }
        if at >= jpeg.len() {
            return false;
        }
        let marker = jpeg[at];
        at += 1;
        if marker == 0xd9 || marker == 0xda {
            return false;
        }
        if (0xd0..=0xd7).contains(&marker) || marker == 0x01 {
            continue;
        }
        let Some(len_bytes) = jpeg.get(at..at + 2) else {
            return false;
        };
        let seg_len = usize::from(u16::from_be_bytes([len_bytes[0], len_bytes[1]]));
        if seg_len < 2 || at + seg_len > jpeg.len() {
            return false;
        }
        if marker == 0xe1 && jpeg.get(at + 2..at + 8) == Some(b"Exif\0\0") {
            return true;
        }
        at += seg_len;
    }
    false
}

fn insert_jpeg_app1_exif(jpeg: &[u8], tiff_blob: &[u8]) -> Option<Vec<u8>> {
    if !jpeg.starts_with(&[0xff, 0xd8]) {
        return None;
    }
    let seg_len = u16::try_from(2usize.checked_add(6)?.checked_add(tiff_blob.len())?).ok()?;
    let mut insert_at = 2usize;
    while insert_at + 4 <= jpeg.len() && jpeg[insert_at] == 0xff && jpeg[insert_at + 1] == 0xe0 {
        let len = usize::from(u16::from_be_bytes([
            jpeg[insert_at + 2],
            jpeg[insert_at + 3],
        ]));
        if len < 2 || insert_at + 2 + len > jpeg.len() {
            break;
        }
        insert_at += 2 + len;
    }
    let mut out = Vec::with_capacity(jpeg.len() + 2 + usize::from(seg_len));
    out.extend_from_slice(&jpeg[..insert_at]);
    out.extend_from_slice(&[0xff, 0xe1]);
    out.extend_from_slice(&seg_len.to_be_bytes());
    out.extend_from_slice(b"Exif\0\0");
    out.extend_from_slice(tiff_blob);
    out.extend_from_slice(&jpeg[insert_at..]);
    Some(out)
}

struct Tiff<'a> {
    bytes: &'a [u8],
    little: bool,
}

impl Tiff<'_> {
    fn u16(&self, at: usize) -> Option<u16> {
        let b = self.bytes.get(at..at + 2)?;
        Some(if self.little {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        })
    }

    fn u32(&self, at: usize) -> Option<u32> {
        let b = self.bytes.get(at..at + 4)?;
        Some(if self.little {
            u32::from_le_bytes([b[0], b[1], b[2], b[3]])
        } else {
            u32::from_be_bytes([b[0], b[1], b[2], b[3]])
        })
    }
}

fn type_size(kind: u16) -> Option<usize> {
    match kind {
        1 | 2 | 6 | 7 => Some(1),
        3 | 8 => Some(2),
        4 | 9 | 11 | 13 => Some(4),
        5 | 10 | 12 => Some(8),
        _ => None,
    }
}

struct ExifEntry {
    tag: u16,
    kind: u16,
    count: u32,
    value: Vec<u8>,
    child: Option<ExifIfd>,
}

struct ExifIfd {
    entries: Vec<ExifEntry>,
}

fn child_tags(depth: u8) -> &'static [u16] {
    match depth {
        0 => &[EXIF_IFD, GPS_IFD],
        1 => &[INTEROP_IFD],
        _ => &[],
    }
}

fn read_ifd(tiff: &Tiff<'_>, offset: usize, depth: u8, seen: &mut Vec<usize>) -> Option<ExifIfd> {
    if depth > MAX_EXIF_DEPTH || seen.contains(&offset) {
        return None;
    }
    seen.push(offset);
    let count = usize::from(tiff.u16(offset)?);
    if count > MAX_EXIF_ENTRIES {
        return None;
    }
    let mut entries = Vec::new();
    for index in 0..count {
        let at = offset + 2 + index * 12;
        let (Some(tag), Some(kind), Some(n)) = (tiff.u16(at), tiff.u16(at + 2), tiff.u32(at + 4))
        else {
            return None;
        };
        if depth == 0 && IMAGE_TAGS.contains(&tag) {
            continue;
        }
        if child_tags(depth).contains(&tag) {
            if let Some(child) = tiff
                .u32(at + 8)
                .and_then(|to| read_ifd(tiff, to as usize, depth + 1, seen))
            {
                entries.push(ExifEntry {
                    tag,
                    kind: 4,
                    count: 1,
                    value: Vec::new(),
                    child: Some(child),
                });
            }
            continue;
        }
        let Some(size) = type_size(kind).and_then(|s| s.checked_mul(n as usize)) else {
            continue;
        };
        let value = if size <= 4 {
            tiff.bytes.get(at + 8..at + 8 + size)
        } else {
            tiff.u32(at + 8).and_then(|from| {
                tiff.bytes
                    .get(from as usize..(from as usize).checked_add(size)?)
            })
        };
        let Some(value) = value else { continue };
        entries.push(ExifEntry {
            tag,
            kind,
            count: n,
            value: value.to_vec(),
            child: None,
        });
    }
    entries.sort_by_key(|entry| entry.tag);
    Some(ExifIfd { entries })
}

fn put_u16(out: &mut [u8], at: usize, value: u16, little: bool) {
    let b = if little {
        value.to_le_bytes()
    } else {
        value.to_be_bytes()
    };
    out[at..at + 2].copy_from_slice(&b);
}

fn put_u32(out: &mut [u8], at: usize, value: u32, little: bool) {
    let b = if little {
        value.to_le_bytes()
    } else {
        value.to_be_bytes()
    };
    out[at..at + 4].copy_from_slice(&b);
}

fn write_ifd(out: &mut Vec<u8>, ifd: &ExifIfd, little: bool) -> Option<u32> {
    if out.len() % 2 == 1 {
        out.push(0);
    }
    let start = out.len();
    out.resize(start + 2 + ifd.entries.len() * 12 + 4, 0);
    put_u16(out, start, u16::try_from(ifd.entries.len()).ok()?, little);
    for (index, entry) in ifd.entries.iter().enumerate() {
        let at = start + 2 + index * 12;
        put_u16(out, at, entry.tag, little);
        put_u16(out, at + 2, entry.kind, little);
        put_u32(out, at + 4, entry.count, little);
        if let Some(child) = &entry.child {
            let to = write_ifd(out, child, little)?;
            put_u32(out, at + 8, to, little);
        } else if entry.value.len() <= 4 {
            out[at + 8..at + 8 + entry.value.len()].copy_from_slice(&entry.value);
        } else {
            if out.len() % 2 == 1 {
                out.push(0);
            }
            let to = u32::try_from(out.len()).ok()?;
            out.extend_from_slice(&entry.value);
            put_u32(out, at + 8, to, little);
        }
    }
    u32::try_from(start).ok()
}

fn tiff_exif(source: &[u8]) -> Option<Vec<u8>> {
    let little = source.starts_with(b"II");
    let tiff = Tiff {
        bytes: source,
        little,
    };
    if tiff.u16(2)? != 42 {
        return None;
    }
    let first = tiff.u32(4)? as usize;
    let ifd = read_ifd(&tiff, first, 0, &mut Vec::new())?;
    if ifd.entries.is_empty() {
        return None;
    }
    let mut out = if little {
        b"II*\0".to_vec()
    } else {
        b"MM\0*".to_vec()
    };
    out.extend_from_slice(&[0; 4]);
    let at = write_ifd(&mut out, &ifd, little)?;
    put_u32(&mut out, 4, at, little);
    Some(out)
}

fn boxes(bytes: &[u8], from: usize, to: usize) -> Vec<([u8; 4], usize, usize)> {
    let mut out = Vec::new();
    let mut at = from;
    while at + 8 <= to {
        let size = u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
        let kind = [bytes[at + 4], bytes[at + 5], bytes[at + 6], bytes[at + 7]];
        let (header, size) = match size {
            0 => (8, to - at),
            1 => {
                let Some(large) = bytes.get(at + 8..at + 16) else {
                    break;
                };
                let large = u64::from_be_bytes(large.try_into().unwrap_or([0; 8]));
                (16, usize::try_from(large).unwrap_or(usize::MAX))
            }
            n => (8, n as usize),
        };
        let Some(end) = at
            .checked_add(size)
            .filter(|end| *end <= to && size >= header)
        else {
            break;
        };
        out.push((kind, at + header, end));
        at = end;
    }
    out
}

fn be(bytes: &[u8], at: usize, width: usize) -> Option<u64> {
    let part = bytes.get(at..at.checked_add(width)?)?;
    Some(part.iter().fold(0u64, |acc, b| (acc << 8) | u64::from(*b)))
}

fn heif_exif(bytes: &[u8]) -> Option<Vec<u8>> {
    let (_, meta_from, meta_to) = boxes(bytes, 0, bytes.len())
        .into_iter()
        .find(|(kind, _, _)| kind == b"meta")?;
    let children = boxes(bytes, meta_from + 4, meta_to);
    let (_, iinf_from, iinf_to) = *children.iter().find(|(k, _, _)| k == b"iinf")?;
    let iinf_version = *bytes.get(iinf_from)?;
    let entries_at = iinf_from + 4 + if iinf_version == 0 { 2 } else { 4 };
    let item = boxes(bytes, entries_at, iinf_to)
        .into_iter()
        .filter(|(k, _, _)| k == b"infe")
        .find_map(|(_, from, _)| {
            let version = *bytes.get(from)?;
            let (id, type_at) = match version {
                2 => (be(bytes, from + 4, 2)?, from + 8),
                3 => (be(bytes, from + 4, 4)?, from + 10),
                _ => return None,
            };
            (bytes.get(type_at..type_at + 4)? == b"Exif").then_some(id)
        })?;
    let idat = children
        .iter()
        .find(|(k, _, _)| k == b"idat")
        .map(|&(_, from, to)| &bytes[from..to]);
    let (_, iloc_from, _) = *children.iter().find(|(k, _, _)| k == b"iloc")?;
    let version = *bytes.get(iloc_from)?;
    let mut at = iloc_from + 4;
    let sizes = *bytes.get(at)?;
    let more = *bytes.get(at + 1)?;
    let (offset_size, length_size) = (usize::from(sizes >> 4), usize::from(sizes & 0xf));
    let base_size = usize::from(more >> 4);
    let index_size = if version >= 1 {
        usize::from(more & 0xf)
    } else {
        0
    };
    at += 2;
    let id_size = if version < 2 { 2 } else { 4 };
    let count = be(bytes, at, id_size)?;
    at += id_size;
    for _ in 0..count.min(10_000) {
        let id = be(bytes, at, id_size)?;
        at += id_size;
        let method = if version >= 1 {
            let m = be(bytes, at, 2)? & 0xf;
            at += 2;
            m
        } else {
            0
        };
        at += 2;
        let base = if base_size > 0 {
            be(bytes, at, base_size)?
        } else {
            0
        };
        at += base_size;
        let extents = be(bytes, at, 2)?;
        at += 2;
        let mut first = None;
        for _ in 0..extents {
            at += index_size;
            let offset = if offset_size > 0 {
                be(bytes, at, offset_size)?
            } else {
                0
            };
            at += offset_size;
            let length = be(bytes, at, length_size)?;
            at += length_size;
            first.get_or_insert((offset, length));
        }
        if id != item || (method != 0 && method != 1) {
            continue;
        }
        let (offset, length) = first?;
        let source_slice = if method == 1 { idat? } else { bytes };
        let from = usize::try_from(base.checked_add(offset)?).ok()?;
        let payload = source_slice.get(from..from.checked_add(usize::try_from(length).ok()?)?)?;
        let skip = usize::try_from(be(payload, 0, 4)?).ok()?;
        return payload.get(4usize.checked_add(skip)?..).map(<[u8]>::to_vec);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_png_chunk_crc_is_the_standard_one() {
        // The CRC of an empty IEND chunk, as every PNG ends.
        assert_eq!(crc32(b"IEND"), 0xae42_6082);
    }

    #[test]
    fn an_xmp_packet_is_found_and_placed_after_the_header() {
        let source = b"....<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><r/></x:xmpmeta>....";
        let xmp = xmp_packet(source).unwrap();
        assert!(xmp.starts_with(b"<x:xmpmeta") && xmp.ends_with(b"</x:xmpmeta>"));
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend_from_slice(&[0; 13 + 4]);
        png.extend_from_slice(b"\0\0\0\0IEND\xae\x42\x60\x82");
        let out = with_xmp(&png, xmp);
        assert_eq!(&out[37..41], b"iTXt");
        assert!(out.ends_with(b"IEND\xae\x42\x60\x82"));
        assert_eq!(xmp_packet(b"no packet here"), None);
    }

    #[test]
    fn heif_exif_carries_into_jpeg_without_orientation_tag() {
        let date = b"2026:11:12 08:30:00\0";
        let offset = b"+09:00\0";
        let ifd0_at = 8u32;
        let exif_at = ifd0_at + 2 + 2 * 12 + 4;
        let date_at = exif_at + 2 + 2 * 12 + 4;
        let offset_at = date_at + date.len() as u32;
        let mut tiff = b"II*\0".to_vec();
        tiff.extend_from_slice(&ifd0_at.to_le_bytes());
        tiff.extend_from_slice(&2u16.to_le_bytes());
        // IFD0 Orientation = 6
        tiff.extend_from_slice(&ORIENTATION_TAG.to_le_bytes());
        tiff.extend_from_slice(&3u16.to_le_bytes());
        tiff.extend_from_slice(&1u32.to_le_bytes());
        tiff.extend_from_slice(&6u16.to_le_bytes());
        tiff.extend_from_slice(&0u16.to_le_bytes());
        // IFD0 ExifIFDPointer
        tiff.extend_from_slice(&EXIF_IFD.to_le_bytes());
        tiff.extend_from_slice(&4u16.to_le_bytes());
        tiff.extend_from_slice(&1u32.to_le_bytes());
        tiff.extend_from_slice(&exif_at.to_le_bytes());
        tiff.extend_from_slice(&0u32.to_le_bytes());
        // Exif IFD
        tiff.extend_from_slice(&2u16.to_le_bytes());
        tiff.extend_from_slice(&36_867u16.to_le_bytes());
        tiff.extend_from_slice(&2u16.to_le_bytes());
        tiff.extend_from_slice(&(date.len() as u32).to_le_bytes());
        tiff.extend_from_slice(&date_at.to_le_bytes());
        tiff.extend_from_slice(&36_881u16.to_le_bytes());
        tiff.extend_from_slice(&2u16.to_le_bytes());
        tiff.extend_from_slice(&(offset.len() as u32).to_le_bytes());
        tiff.extend_from_slice(&offset_at.to_le_bytes());
        tiff.extend_from_slice(&0u32.to_le_bytes());
        tiff.extend_from_slice(date);
        tiff.extend_from_slice(offset);

        let mut payload = 6u32.to_be_bytes().to_vec();
        payload.extend_from_slice(b"Exif\0\0");
        payload.extend_from_slice(&tiff);
        let full = |kind: &[u8; 4], body: &[u8]| {
            let mut b = ((body.len() + 8) as u32).to_be_bytes().to_vec();
            b.extend_from_slice(kind);
            b.extend_from_slice(body);
            b
        };
        let mut infe = vec![2, 0, 0, 0, 0, 7, 0, 0];
        infe.extend_from_slice(b"Exif");
        infe.push(0);
        let mut iinf = vec![0, 0, 0, 0, 0, 1];
        iinf.extend(full(b"infe", &infe));
        let mut iloc = vec![1, 0, 0, 0, 0x44, 0x00, 0, 1, 0, 7, 0, 1, 0, 0, 0, 1];
        iloc.extend_from_slice(&0u32.to_be_bytes());
        iloc.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        let mut meta = vec![0, 0, 0, 0];
        meta.extend(full(b"iinf", &iinf));
        meta.extend(full(b"iloc", &iloc));
        meta.extend(full(b"idat", &payload));
        let mut heif = full(b"ftyp", b"heic\0\0\0\0mif1heic");
        heif.extend(full(b"meta", &meta));

        let mut jpeg = vec![
            0xff, 0xd8, 0xff, 0xe0, 0x00, 0x07, b'J', b'F', b'I', b'F', 0x00,
        ];
        jpeg.extend_from_slice(&[0xff, 0xda, 0x00, 0x02, 0xff, 0xd9]);

        let out = carry_exif_to_jpeg(&heif, jpeg);
        assert!(jpeg_has_exif(&out));
        assert_eq!(&out[11..13], &[0xff, 0xe1]);
        assert_eq!(&out[15..21], b"Exif\0\0");
        let seg_len = usize::from(u16::from_be_bytes([out[13], out[14]]));
        let embedded = &out[21..13 + seg_len];
        assert!(embedded.windows(19).any(|w| w == b"2026:11:12 08:30:00"));
        assert!(embedded.windows(6).any(|w| w == b"+09:00"));
        let parsed = Tiff {
            bytes: embedded,
            little: true,
        };
        let ifd0 = read_ifd(&parsed, parsed.u32(4).unwrap() as usize, 0, &mut Vec::new()).unwrap();
        assert!(!ifd0.entries.iter().any(|e| e.tag == ORIENTATION_TAG));
        assert_eq!(carry_exif_to_jpeg(&heif, out.clone()), out);
    }
}
