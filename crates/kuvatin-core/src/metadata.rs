//! Sidecar metadata carried from a source image to its output: the ICC colour
//! profile and the EXIF block.
//!
//! Dropping either silently damages the file. Without the profile, a Display
//! P3 photo or an Adobe RGB scan has its numbers reinterpreted as sRGB and
//! shifts visibly; without EXIF, the capture date, camera, lens and copyright
//! are gone for good.

/// What travels alongside the pixels. Both fields are raw blocks, passed
/// through untouched apart from the orientation fix described on
/// [`neutralise_orientation`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Metadata {
    /// Raw ICC colour profile, as the decoder handed it over.
    pub icc: Option<Vec<u8>>,
    /// Raw EXIF block, starting at the TIFF header (no `Exif\0\0` prefix).
    pub exif: Option<Vec<u8>>,
}

impl Metadata {
    pub fn is_empty(&self) -> bool {
        self.icc.is_none() && self.exif.is_none()
    }
}

/// Rewrite the EXIF orientation tag to 1 ("normal") in place.
///
/// Decoding already applies the orientation to the pixels, so a copied-through
/// tag would make every viewer that honours it rotate a second time. Only
/// IFD0 is walked: orientation lives there, and a malformed or truncated block
/// is left alone rather than guessed at.
pub fn neutralise_orientation(exif: &mut [u8]) {
    const ORIENTATION: u16 = 0x0112;
    let Some((little, ifd0)) = tiff_header(exif) else {
        return;
    };
    let read_u16 = |b: &[u8]| {
        let v = [b[0], b[1]];
        if little {
            u16::from_le_bytes(v)
        } else {
            u16::from_be_bytes(v)
        }
    };
    if exif.len() < ifd0 + 2 {
        return;
    }
    let count = read_u16(&exif[ifd0..]) as usize;
    for i in 0..count {
        // Each IFD entry is 12 bytes: tag, type, count, then a 4-byte value
        // (or an offset, but a SHORT orientation always fits inline).
        let at = ifd0 + 2 + i * 12;
        if at + 12 > exif.len() {
            return;
        }
        if read_u16(&exif[at..]) != ORIENTATION {
            continue;
        }
        let value = at + 8;
        exif[value..value + 4].copy_from_slice(&[0, 0, 0, 0]);
        // A SHORT is stored in the first two bytes of the value field, in the
        // block's own byte order.
        if little {
            exif[value] = 1;
        } else {
            exif[value + 1] = 1;
        }
        return;
    }
}

/// Flags in the VP8X chunk's first byte, most significant bit first:
/// two reserved bits, then ICC, alpha, EXIF, XMP, animation, reserved.
const VP8X_ICC: u8 = 0x20;
const VP8X_ALPHA: u8 = 0x10;
const VP8X_EXIF: u8 = 0x08;

/// Rebuild a WebP file so it carries `meta`.
///
/// libwebp writes the simple form, which has nowhere to put a colour profile;
/// only the extended form (a leading `VP8X` chunk) does. This re-wraps the
/// encoder's output: `VP8X`, then `ICCP`, then the original image chunks, then
/// `EXIF`, which is the order the format requires.
///
/// `None` when there is nothing to add or the input is not a WebP this code
/// understands, in which case the caller keeps the encoder's bytes as they are.
pub fn webp_with_metadata(
    encoded: &[u8],
    width: u32,
    height: u32,
    meta: &Metadata,
) -> Option<Vec<u8>> {
    if meta.is_empty() || width == 0 || height == 0 || width > 1 << 24 || height > 1 << 24 {
        return None;
    }
    let chunks = riff_chunks(encoded)?;

    // Start from the existing VP8X when there is one: it already records the
    // canvas size and, for a lossy image with transparency, the alpha flag.
    let mut header = match chunks.iter().find(|(id, _)| *id == b"VP8X") {
        Some((_, payload)) if payload.len() == 10 => payload.to_vec(),
        _ => {
            let mut v = vec![0u8; 10];
            v[4..7].copy_from_slice(&(width - 1).to_le_bytes()[..3]);
            v[7..10].copy_from_slice(&(height - 1).to_le_bytes()[..3]);
            if chunks.iter().any(|(id, _)| *id == b"ALPH") {
                v[0] |= VP8X_ALPHA;
            }
            v
        }
    };
    if meta.icc.is_some() {
        header[0] |= VP8X_ICC;
    }
    if meta.exif.is_some() {
        header[0] |= VP8X_EXIF;
    }

    let mut body = Vec::with_capacity(encoded.len() + 256);
    push_chunk(&mut body, b"VP8X", &header);
    if let Some(icc) = &meta.icc {
        push_chunk(&mut body, b"ICCP", icc);
    }
    for (id, payload) in &chunks {
        // The blocks we are replacing; everything else is passed through in
        // its original order.
        if matches!(*id, b"VP8X" | b"ICCP" | b"EXIF") {
            continue;
        }
        push_chunk(&mut body, id, payload);
    }
    if let Some(exif) = &meta.exif {
        push_chunk(&mut body, b"EXIF", exif);
    }

    let mut out = Vec::with_capacity(body.len() + 12);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((body.len() + 4) as u32).to_le_bytes());
    out.extend_from_slice(b"WEBP");
    out.extend_from_slice(&body);
    Some(out)
}

/// Append one RIFF chunk: four-character id, little-endian payload length,
/// the payload, and a pad byte when that length is odd.
fn push_chunk(out: &mut Vec<u8>, id: &[u8; 4], payload: &[u8]) {
    out.extend_from_slice(id);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    if payload.len() % 2 == 1 {
        out.push(0);
    }
}

/// Split a WebP file into its chunks. `None` if it is not a RIFF/WEBP
/// container or a chunk runs past the end of the data.
fn riff_chunks(bytes: &[u8]) -> Option<Vec<(&[u8; 4], &[u8])>> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return None;
    }
    let mut out = Vec::new();
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id: &[u8; 4] = bytes[at..at + 4].try_into().ok()?;
        let len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().ok()?) as usize;
        let start = at + 8;
        let end = start.checked_add(len)?;
        if end > bytes.len() {
            return None;
        }
        out.push((id, &bytes[start..end]));
        at = end + (len % 2);
    }
    (!out.is_empty()).then_some(out)
}

/// `(little_endian, offset of IFD0)` for a well-formed TIFF header.
fn tiff_header(exif: &[u8]) -> Option<(bool, usize)> {
    if exif.len() < 8 {
        return None;
    }
    let little = match &exif[0..2] {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |i: usize| {
        let v = [exif[i], exif[i + 1]];
        if little {
            u16::from_le_bytes(v)
        } else {
            u16::from_be_bytes(v)
        }
    };
    let u32_at = |i: usize| {
        let v = [exif[i], exif[i + 1], exif[i + 2], exif[i + 3]];
        if little {
            u32::from_le_bytes(v)
        } else {
            u32::from_be_bytes(v)
        }
    };
    if u16_at(2) != 42 {
        return None;
    }
    Some((little, u32_at(4) as usize))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One IFD0 entry: tag 0x0112 (orientation), type SHORT, count 1, value.
    fn exif_with_orientation(little: bool, value: u16) -> Vec<u8> {
        let mut out = Vec::new();
        let u16b = |v: u16| {
            if little {
                v.to_le_bytes()
            } else {
                v.to_be_bytes()
            }
        };
        let u32b = |v: u32| {
            if little {
                v.to_le_bytes()
            } else {
                v.to_be_bytes()
            }
        };
        out.extend_from_slice(if little { b"II" } else { b"MM" });
        out.extend_from_slice(&u16b(42));
        out.extend_from_slice(&u32b(8)); // IFD0 starts right after the header
        out.extend_from_slice(&u16b(1)); // one entry
        out.extend_from_slice(&u16b(0x0112));
        out.extend_from_slice(&u16b(3)); // SHORT
        out.extend_from_slice(&u32b(1)); // count
        out.extend_from_slice(&u16b(value));
        out.extend_from_slice(&[0, 0]); // value field padding
        out.extend_from_slice(&u32b(0)); // no next IFD
        out
    }

    fn orientation_of(exif: &[u8]) -> Option<u16> {
        let (little, ifd0) = tiff_header(exif)?;
        let at = ifd0 + 2;
        let raw = [exif[at + 8], exif[at + 9]];
        Some(if little {
            u16::from_le_bytes(raw)
        } else {
            u16::from_be_bytes(raw)
        })
    }

    /// The rotation is baked into the pixels at decode, so the tag that asked
    /// for it must not survive: a viewer honouring it would rotate twice.
    #[test]
    fn orientation_is_reset_to_normal() {
        for little in [true, false] {
            let mut exif = exif_with_orientation(little, 6);
            assert_eq!(orientation_of(&exif), Some(6), "fixture, little={little}");
            neutralise_orientation(&mut exif);
            assert_eq!(orientation_of(&exif), Some(1), "little={little}");
        }
    }

    /// A block that is not EXIF, or is cut short, is passed through untouched
    /// rather than half-rewritten.
    #[test]
    fn malformed_blocks_are_left_alone() {
        for mut bad in [
            b"not exif at all".to_vec(),
            b"II".to_vec(),
            [
                b"II".as_slice(),
                &[42, 0, 8, 0, 0, 0],
                &[1, 0],
                &[0x12, 0x01],
            ]
            .concat(),
            // Right shape, wrong magic number.
            [b"II".as_slice(), &[43, 0, 8, 0, 0, 0]].concat(),
        ] {
            let before = bad.clone();
            neutralise_orientation(&mut bad);
            assert_eq!(bad, before);
        }
    }

    /// Orientation is not always the first entry, and entries before it must
    /// survive the walk.
    #[test]
    fn orientation_is_found_after_other_entries() {
        let mut exif: Vec<u8> = [
            b"II".as_slice(),
            &[42, 0, 8, 0, 0, 0],
            &[2, 0],                                     // two entries
            &[0x1A, 0x01, 5, 0, 1, 0, 0, 0, 8, 0, 0, 0], // XResolution, an offset
            &[0x12, 0x01, 3, 0, 1, 0, 0, 0, 3, 0, 0, 0], // orientation 3
            &[0, 0, 0, 0],
        ]
        .concat();
        neutralise_orientation(&mut exif);
        let at = 8 + 2 + 12; // second entry
        assert_eq!(
            &exif[at..at + 2],
            &[0x12, 0x01],
            "walked to the right entry"
        );
        assert_eq!(&exif[at + 8..at + 12], &[1, 0, 0, 0], "reset to normal");
        let first = 8 + 2;
        assert_eq!(
            &exif[first + 8..first + 12],
            &[8, 0, 0, 0],
            "the earlier entry is untouched"
        );
    }

    /// A file with no orientation tag at all is common; nothing should change.
    #[test]
    fn a_block_without_orientation_is_unchanged() {
        let mut exif: Vec<u8> = [
            b"MM".as_slice(),
            &[0, 42, 0, 0, 0, 8],
            &[0, 1],
            &[0x01, 0x1A, 0, 5, 0, 0, 0, 1, 0, 0, 0, 8],
            &[0, 0, 0, 0],
        ]
        .concat();
        let before = exif.clone();
        neutralise_orientation(&mut exif);
        assert_eq!(exif, before);
    }
}
