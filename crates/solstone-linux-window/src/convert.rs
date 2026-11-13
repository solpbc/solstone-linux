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
}
