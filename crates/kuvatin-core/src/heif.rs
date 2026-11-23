//! HEIC/HEIF stills — what an iPhone photo is when it arrives untouched
//! (AirDrop, a cable import, the still half of a Live Photo).
//!
//! The pixels are HEVC, and nothing Kuvatin bundles decodes a HEIF image:
//! the `image` crate has no HEIF codec, and GStreamer's HEVC decoder sees
//! video, not the grid of tiles a HEIF photo is cut into. The decode goes
//! through Windows' own codec (WIC) instead, which means no new binary in the
//! installer and no codec licence of ours — but it also means the photo opens
//! only where Windows can open it: with "HEIF Image Extensions" (preinstalled
//! on Windows 10 1809+ and 11) AND "HEVC Video Extensions" from the Microsoft
//! Store. [`support`] tells which one is missing, so the error can say so.
//!
//! The container itself is read here, in Rust, for the parts WIC does not
//! hand over in a usable form: the EXIF block (capture date, camera —
//! carried to the output like a JPEG's), the colour profile, and the size,
//! which the batch's memory budget needs before anything is decoded. WIC
//! does apply the rotation and mirroring (`irot`/`imir`) itself; the parse
//! reads them only to know which way round the size is.

use crate::metadata::{neutralise_orientation, Metadata};
use crate::{CoreError, CoreResult};
use image::metadata::Orientation;
use image::DynamicImage;
use std::path::Path;

/// File extensions that are HEIF stills (lower-case, no dot).
pub const HEIF_EXTENSIONS: &[&str] = &["heic", "heif"];

/// Whether `path` names a HEIF still: by its extension, or, for a file whose
/// extension says nothing useful, by the brand in its first bytes.
pub fn is_heif_path(path: &Path) -> bool {
    let by_ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| HEIF_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false);
    by_ext || sniff_file(path)
}

/// Whether the first bytes of `path` are a HEIF image's `ftyp`.
fn sniff_file(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 64];
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut n = 0;
    while n < head.len() {
        match f.read(&mut head[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(_) => return false,
        }
    }
    sniff(&head[..n])
}

/// Whether `bytes` start with an `ftyp` box naming an HEVC-coded HEIF image
/// brand. `mif1` alone is not enough: AVIF files carry it too, and those are
/// AV1, which this module does not promise to read.
pub fn sniff(bytes: &[u8]) -> bool {
    const HEVC_BRANDS: &[&[u8; 4]] = &[b"heic", b"heix", b"heim", b"heis", b"hevc", b"hevx"];
    if bytes.len() < 12 || &bytes[4..8] != b"ftyp" {
        return false;
    }
    let size = (u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize)
        .clamp(8, bytes.len());
    // major brand, minor version, then the compatible brands.
    let major = &bytes[8..12];
    if HEVC_BRANDS.iter().any(|b| major == b.as_slice()) {
        return true;
    }
    bytes[16.min(size)..size]
        .chunks_exact(4)
        .any(|c| HEVC_BRANDS.iter().any(|b| c == b.as_slice()))
}

// ---------------------------------------------------------------------------
// Container
// ---------------------------------------------------------------------------

/// What the container says about the primary image.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Container {
    /// Size before rotation: `ispe`, cut to `clap` when there is one (the
    /// coded size is padded to whole blocks; the clean aperture is the
    /// picture).
    pub size: Option<(u32, u32)>,
    /// The `irot`/`imir` transforms, as the EXIF-style orientation that
    /// performs them. They are part of the image (a reader must apply them;
    /// WIC does), unlike EXIF's orientation tag, which in HEIF is a copy.
    pub orientation: Option<Orientation>,
    /// Raw EXIF, starting at the TIFF header.
    pub exif: Option<Vec<u8>>,
    /// ICC profile from a `colr` box of type `prof`/`rICC`.
    pub icc: Option<Vec<u8>>,
}

/// One box: its type, and its payload as a range of the outer buffer.
struct BoxRef<'a> {
    kind: [u8; 4],
    body: &'a [u8],
    /// Offset of `body` in the whole file (for `iloc` file offsets).
    at: usize,
}

/// The boxes laid end to end in `buf` (which starts at file offset `at`).
/// Stops quietly at the first malformed header: what came before is still
/// good, and a truncated tail must not fail a photo that decodes.
fn boxes(buf: &[u8], at: usize) -> Vec<BoxRef<'_>> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 8 <= buf.len() {
        let size32 = u32::from_be_bytes([buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]) as u64;
        let kind = [buf[i + 4], buf[i + 5], buf[i + 6], buf[i + 7]];
        let (header, size) = match size32 {
            0 => (8u64, (buf.len() - i) as u64),
            1 => {
                if i + 16 > buf.len() {
                    break;
                }
                let mut b = [0u8; 8];
                b.copy_from_slice(&buf[i + 8..i + 16]);
                (16, u64::from_be_bytes(b))
            }
            n => (8, n),
        };
        if size < header {
            break;
        }
        // A box that claims more than is there: keep what is there (the
        // meta box read from a file's first bytes ends early this way).
        let end = (i as u64).saturating_add(size).min(buf.len() as u64) as usize;
        let start = i + header as usize;
        if start > end {
            break;
        }
        out.push(BoxRef {
            kind,
            body: &buf[start..end],
            at: at + start,
        });
        i = end;
    }
    out
}

fn find<'a>(list: &'a [BoxRef<'a>], kind: &[u8; 4]) -> Option<&'a BoxRef<'a>> {
    list.iter().find(|b| &b.kind == kind)
}

/// A big-endian reader that answers `None` past the end instead of panicking.
struct Rd<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Rd<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b, i: 0 }
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.i.checked_add(n)?;
        let s = self.b.get(self.i..end)?;
        self.i = end;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|s| s[0])
    }
    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|s| u16::from_be_bytes([s[0], s[1]]))
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4)
            .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }
    /// An unsigned integer `n` bytes wide (0, 4 or 8 in `iloc`).
    fn uint(&mut self, n: usize) -> Option<u64> {
        let s = self.take(n)?;
        Some(s.iter().fold(0u64, |acc, &x| (acc << 8) | u64::from(x)))
    }
    fn rest(&self) -> &'a [u8] {
        self.b.get(self.i..).unwrap_or(&[])
    }
}

/// Read what [`Container`] holds out of a HEIF file's bytes. `file` may be
/// only the start of the file: whatever lies beyond it (an EXIF block in the
/// media data) is then simply absent.
pub fn parse(file: &[u8]) -> Container {
    parse_inner(file).unwrap_or_default()
}

fn parse_inner(file: &[u8]) -> Option<Container> {
    let top = boxes(file, 0);
    let meta = find(&top, b"meta")?;
    // `meta` is a full box: version and flags first.
    let children = boxes(meta.body.get(4..)?, meta.at + 4);

    let primary = {
        let pitm = find(&children, b"pitm")?;
        let mut r = Rd::new(pitm.body);
        let version = r.u8()?;
        r.take(3)?;
        if version == 0 {
            u32::from(r.u16()?)
        } else {
            r.u32()?
        }
    };

    let mut out = Container::default();

    // Properties of the primary item, in association order.
    if let Some(iprp) = find(&children, b"iprp") {
        let inner = boxes(iprp.body, iprp.at);
        if let Some(ipco) = find(&inner, b"ipco") {
            let props = boxes(ipco.body, ipco.at);
            let mut assoc: Vec<usize> = Vec::new();
            for ipma in inner.iter().filter(|b| &b.kind == b"ipma") {
                assoc.extend(associations(ipma.body, primary).unwrap_or_default());
            }
            let mut clap: Option<(u32, u32)> = None;
            let mut rotate_ccw = 0u8; // quarter turns, anticlockwise
            let mut mirror: Option<u8> = None; // axis: 0 vertical, 1 horizontal
            let mut mirror_first = false;
            for index in assoc {
                let Some(p) = index.checked_sub(1).and_then(|i| props.get(i)) else {
                    continue;
                };
                match &p.kind {
                    b"ispe" => {
                        let mut r = Rd::new(p.body);
                        r.take(4);
                        if let (Some(w), Some(h)) = (r.u32(), r.u32()) {
                            out.size = Some((w, h));
                        }
                    }
                    b"clap" => {
                        // Width N/D and height N/D, then the offsets.
                        let mut r = Rd::new(p.body);
                        if let (Some(wn), Some(wd), Some(hn), Some(hd)) =
                            (r.u32(), r.u32(), r.u32(), r.u32())
                        {
                            if wd > 0 && hd > 0 && wn / wd > 0 && hn / hd > 0 {
                                clap = Some((wn / wd, hn / hd));
                            }
                        }
                    }
                    b"irot" => {
                        rotate_ccw = p.body.first().map(|b| b & 3).unwrap_or(0);
                    }
                    b"imir" => {
                        mirror = p.body.first().map(|b| b & 1);
                        mirror_first = rotate_ccw == 0;
                    }
                    b"colr" => {
                        let kind = p.body.get(0..4);
                        if kind == Some(b"prof") || kind == Some(b"rICC") {
                            let icc = p.body[4..].to_vec();
                            if !icc.is_empty() {
                                out.icc = Some(icc);
                            }
                        }
                    }
                    _ => {}
                }
            }
            if clap.is_some() {
                out.size = clap;
            }
            out.orientation = orientation_of(rotate_ccw, mirror, mirror_first);
        }
    }

    out.exif = exif_item(file, &children);
    Some(out)
}

/// The 1-based property indices `ipma` associates with `item`.
fn associations(body: &[u8], item: u32) -> Option<Vec<usize>> {
    let mut r = Rd::new(body);
    let version = r.u8()?;
    let flags = r.take(3)?;
    let wide = flags[2] & 1 == 1;
    let count = r.u32()?;
    for _ in 0..count {
        let id = if version < 1 {
            u32::from(r.u16()?)
        } else {
            r.u32()?
        };
        let n = r.u8()?;
        let mut list = Vec::with_capacity(n as usize);
        for _ in 0..n {
            let index = if wide {
                usize::from(r.u16()? & 0x7fff)
            } else {
                usize::from(r.u8()? & 0x7f)
            };
            list.push(index);
        }
        if id == item {
            return Some(list);
        }
    }
    Some(Vec::new())
}

/// The EXIF-style orientation that applies HEIF's `irot` (quarter turns
/// anticlockwise) and `imir`. They apply in the order they are associated;
/// `mirror_first` says the mirror came first.
///
/// `imir`'s axis is read the way Windows' encoder writes it, and libheif
/// reads it: 0 flips top↔bottom, 1 flips left↔right (measured: WIC writes
/// axis 1 for EXIF orientation 2, "mirror horizontal"). The standard's
/// wording reads the other way round, and nothing in the field follows it.
fn orientation_of(rotate_ccw: u8, mirror: Option<u8>, mirror_first: bool) -> Option<Orientation> {
    // Clockwise quarter turns, which is what `Orientation` speaks.
    let mut cw = (4 - rotate_ccw % 4) % 4;
    // Mirror-then-rotate equals rotate-the-other-way-then-mirror.
    if mirror.is_some() && mirror_first {
        cw = (4 - cw) % 4;
    }
    // Turn every mirror into "rotate cw, then flip left↔right".
    let (cw, flip) = match mirror {
        None => (cw, false),
        Some(1) => (cw, true),
        // top↔bottom = half turn, then left↔right.
        Some(_) => ((cw + 2) % 4, true),
    };
    Some(match (cw, flip) {
        (0, false) => return None,
        (1, false) => Orientation::Rotate90,
        (2, false) => Orientation::Rotate180,
        (3, false) => Orientation::Rotate270,
        (0, true) => Orientation::FlipHorizontal,
        (1, true) => Orientation::Rotate90FlipH,
        (2, true) => Orientation::FlipVertical,
        (_, _) => Orientation::Rotate270FlipH,
    })
}

/// The bytes of the first `Exif` item, from the TIFF header on.
fn exif_item(file: &[u8], meta: &[BoxRef<'_>]) -> Option<Vec<u8>> {
    let id = {
        let iinf = find(meta, b"iinf")?;
        let mut r = Rd::new(iinf.body);
        let version = r.u8()?;
        r.take(3)?;
        if version == 0 {
            r.u16()?;
        } else {
            r.u32()?;
        }
        let entries = boxes(r.rest(), 0);
        entries
            .iter()
            .filter(|b| &b.kind == b"infe")
            .find_map(|infe| {
                let mut r = Rd::new(infe.body);
                let version = r.u8()?;
                r.take(3)?;
                if version < 2 {
                    return None;
                }
                let id = if version == 2 {
                    u32::from(r.u16()?)
                } else {
                    r.u32()?
                };
                r.u16()?; // protection index
                (r.take(4)? == b"Exif").then_some(id)
            })?
    };

    let iloc = find(meta, b"iloc")?;
    let mut r = Rd::new(iloc.body);
    let version = r.u8()?;
    r.take(3)?;
    let sizes = r.u8()?;
    let (offset_size, length_size) = (usize::from(sizes >> 4), usize::from(sizes & 15));
    let sizes = r.u8()?;
    let base_size = usize::from(sizes >> 4);
    let index_size = if version >= 1 {
        usize::from(sizes & 15)
    } else {
        0
    };
    let count = if version < 2 {
        u32::from(r.u16()?)
    } else {
        r.u32()?
    };
    for _ in 0..count {
        let item = if version < 2 {
            u32::from(r.u16()?)
        } else {
            r.u32()?
        };
        let method = if version >= 1 { r.u16()? & 15 } else { 0 };
        r.u16()?; // data reference index
        let base = r.uint(base_size)?;
        let extents = r.u16()?;
        let mut data = Vec::new();
        for _ in 0..extents {
            r.uint(index_size)?;
            let off = r.uint(offset_size)?;
            let len = r.uint(length_size)?;
            if item != id {
                continue;
            }
            let source: &[u8] = match method {
                0 => file,
                1 => find(meta, b"idat")?.body,
                _ => return None,
            };
            let start = usize::try_from(base.checked_add(off)?).ok()?;
            let end = if len == 0 {
                source.len()
            } else {
                start.checked_add(usize::try_from(len).ok()?)?
            };
            data.extend_from_slice(source.get(start..end)?);
        }
        if item == id {
            // The payload opens with the offset of the TIFF header within
            // what follows (usually past an `Exif\0\0`).
            let skip = u32::from_be_bytes(data.get(0..4)?.try_into().ok()?) as usize;
            let tiff = data.get(4usize.checked_add(skip)?..)?;
            let ok = tiff.starts_with(b"II*\0") || tiff.starts_with(b"MM\0*");
            return ok.then(|| tiff.to_vec());
        }
    }
    None
}

/// Stored size of a HEIF file's primary image, rotated as it will be shown,
/// from the first bytes of the file — no pixel is decoded. `None` when the
/// container can't be read.
pub fn dimensions(path: &Path) -> Option<(u32, u32)> {
    use std::io::Read;
    // The meta box sits at the front; a 48-tile grid's is a few KiB.
    let mut head = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(1 << 20)
        .read_to_end(&mut head)
        .ok()?;
    let c = parse(&head);
    let (w, h) = c.size?;
    Some(if swaps_axes(c.orientation) {
        (h, w)
    } else {
        (w, h)
    })
}

fn swaps_axes(o: Option<Orientation>) -> bool {
    matches!(
        o,
        Some(
            Orientation::Rotate90
                | Orientation::Rotate270
                | Orientation::Rotate90FlipH
                | Orientation::Rotate270FlipH
        )
    )
}

// ---------------------------------------------------------------------------
// Decode
// ---------------------------------------------------------------------------

/// Whether Windows can decode HEIC here, and if not, what is missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    /// Both codecs are present.
    Ready,
    /// No WIC HEIF decoder: "HEIF Image Extensions" is missing.
    NoHeif,
    /// The HEIF decoder is there but cannot decode HEVC: "HEVC Video
    /// Extensions" is missing.
    NoHevc,
    /// Not Windows.
    Unsupported,
}

/// What to tell the user when a HEIC won't decode for want of a codec.
pub fn missing_codec_hint(s: Support) -> Option<&'static str> {
    match s {
        Support::Ready => None,
        Support::NoHeif => Some(
            "Windows has no HEIF decoder. Install \"HEIF Image Extensions\" \
             from the Microsoft Store, then try again.",
        ),
        Support::NoHevc => Some(
            "Windows can read the HEIF container but not the HEVC pictures \
             inside it. Install \"HEVC Video Extensions\" from the Microsoft \
             Store (or the free \"HEVC Video Extensions from Device \
             Manufacturer\"), then try again.",
        ),
        Support::Unsupported => Some("HEIC photos can only be read on Windows."),
    }
}

/// Check, once per process, whether HEIC photos can be decoded here.
pub fn support() -> Support {
    static CACHED: std::sync::OnceLock<Support> = std::sync::OnceLock::new();
    *CACHED.get_or_init(wic::probe)
}

/// The colour a decode should hand back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colour {
    /// The stored pixels, with the profile beside them in [`Metadata`] —
    /// what Images mode wants, so the output carries the same profile.
    AsStored,
    /// Converted to sRGB, with no profile — for consumers that ignore one
    /// (GStreamer, the video editor), on which a Display P3 photo would
    /// otherwise look washed out.
    Srgb,
}

/// Decode a HEIF still, upright, with its metadata. `max_dim`/`max_bytes`
/// bound what a lying header can make it allocate, as for every other decode.
pub fn decode(
    input: &Path,
    colour: Colour,
    max_dim: u32,
    max_bytes: u64,
) -> CoreResult<(DynamicImage, Metadata)> {
    let io_err = |e: std::io::Error| CoreError::Io {
        path: input.to_path_buf(),
        source: e,
    };
    let bytes = std::fs::read(input).map_err(io_err)?;
    let container = parse(&bytes);
    // Upright already: WIC applies `irot`/`imir` itself (measured, see
    // `windows_applies_the_container_transforms`), so unlike a JPEG's EXIF
    // tag there is nothing left to rotate here.
    let img =
        wic::decode(&bytes, colour, container.icc.as_deref(), max_dim, max_bytes).map_err(|e| {
            let hint = match support() {
                Support::Ready => String::new(),
                s => format!(" {}", missing_codec_hint(s).unwrap_or_default()),
            };
            CoreError::Unreadable(format!(
                "could not decode the HEIC photo {}: {e}.{hint}",
                input.display()
            ))
        })?;
    let mut exif = container.exif;
    if let Some(e) = exif.as_mut() {
        neutralise_orientation(e);
    }
    let icc = match colour {
        Colour::AsStored => container.icc,
        Colour::Srgb => None,
    };
    Ok((img, Metadata { icc, exif }))
}

/// Decode a HEIF still and write it as an sRGB PNG at `out` — the form the
/// video editor can use, since GStreamer can't read HEIF.
pub fn convert_to_png(input: &Path, out: &Path) -> CoreResult<()> {
    let (img, _) = decode(
        input,
        Colour::Srgb,
        crate::pipeline::MAX_DECODE_DIM,
        crate::pipeline::MAX_DECODE_BYTES,
    )?;
    let img = match img {
        DynamicImage::ImageRgba16(_) | DynamicImage::ImageRgb16(_) => {
            DynamicImage::ImageRgba8(img.into_rgba8())
        }
        other => other,
    };
    let tmp = out.with_extension("png.part");
    let io_err = |p: &Path, e: std::io::Error| CoreError::Io {
        path: p.to_path_buf(),
        source: e,
    };
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
    }
    {
        let file = std::fs::File::create(&tmp).map_err(|e| io_err(&tmp, e))?;
        let mut w = std::io::BufWriter::new(file);
        // Fast compression: this is a working copy, read once per import.
        let enc = image::codecs::png::PngEncoder::new_with_quality(
            &mut w,
            image::codecs::png::CompressionType::Fast,
            image::codecs::png::FilterType::Adaptive,
        );
        img.write_with_encoder(enc)
            .map_err(|e| CoreError::Encode(e.to_string()))?;
        use std::io::Write;
        w.flush().map_err(|e| io_err(&tmp, e))?;
    }
    std::fs::rename(&tmp, out).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        io_err(out, e)
    })
}

#[cfg(windows)]
mod wic {
    use super::{Colour, Support};
    use image::DynamicImage;
    use windows::core::{Interface, GUID};
    use windows::Win32::Graphics::Imaging::*;
    use windows::Win32::System::Com::*;

    /// COM for the length of one call. A thread that already has COM keeps
    /// its apartment (the UI thread is STA for drag and drop); WIC works in
    /// either, so a mismatch is fine and is not ours to undo.
    struct Com(bool);
    impl Com {
        fn enter() -> Self {
            let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            Com(hr.is_ok())
        }
    }
    impl Drop for Com {
        fn drop(&mut self) {
            if self.0 {
                unsafe { CoUninitialize() };
            }
        }
    }

    fn factory() -> windows::core::Result<IWICImagingFactory> {
        unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
    }

    /// A HEIC small enough to sit in the binary: 16x16, encoded by Windows'
    /// own HEIF encoder. Decoding it is the only reliable test of the HEVC
    /// half — the Store package's presence says nothing about whether this
    /// process may use it.
    const PROBE: &[u8] = include_bytes!("heif_probe.heic");

    pub(super) fn probe() -> Support {
        let _com = Com::enter();
        let Ok(f) = factory() else {
            return Support::NoHeif;
        };
        if !has_heif_decoder(&f) {
            return Support::NoHeif;
        }
        match decode_with(&f, PROBE, Colour::AsStored, None, 64, 1 << 20) {
            Ok(_) => Support::Ready,
            Err(_) => Support::NoHevc,
        }
    }

    fn has_heif_decoder(f: &IWICImagingFactory) -> bool {
        let Ok(list) = (unsafe {
            f.CreateComponentEnumerator(WICDecoder.0 as u32, WICComponentEnumerateDefault.0 as u32)
        }) else {
            return false;
        };
        loop {
            let mut item = [None];
            let mut got = 0u32;
            if unsafe { list.Next(&mut item, Some(&mut got)) }.is_err() || got == 0 {
                return false;
            }
            let Some(unknown) = item[0].take() else {
                return false;
            };
            let Ok(info) = unknown.cast::<IWICBitmapDecoderInfo>() else {
                continue;
            };
            if unsafe { info.GetContainerFormat() }.ok() == Some(GUID_ContainerFormatHeif) {
                return true;
            }
        }
    }

    pub(super) fn decode(
        bytes: &[u8],
        colour: Colour,
        icc: Option<&[u8]>,
        max_dim: u32,
        max_bytes: u64,
    ) -> Result<DynamicImage, String> {
        let _com = Com::enter();
        let f = factory().map_err(|e| format!("Windows Imaging Component unavailable ({e})"))?;
        decode_with(&f, bytes, colour, icc, max_dim, max_bytes)
    }

    fn decode_with(
        f: &IWICImagingFactory,
        bytes: &[u8],
        colour: Colour,
        icc: Option<&[u8]>,
        max_dim: u32,
        max_bytes: u64,
    ) -> Result<DynamicImage, String> {
        let e = |what: &str| {
            let what = what.to_string();
            move |err: windows::core::Error| format!("{what}: {}", err.message().trim())
        };
        unsafe {
            let stream = f.CreateStream().map_err(e("stream"))?;
            stream.InitializeFromMemory(bytes).map_err(e("stream"))?;
            let decoder = f
                .CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)
                .map_err(e("no decoder for this file"))?;
            let frame = decoder.GetFrame(0).map_err(e("first image"))?;
            let (mut w, mut h) = (0u32, 0u32);
            frame.GetSize(&mut w, &mut h).map_err(e("size"))?;
            if w == 0 || h == 0 || w > max_dim || h > max_dim {
                return Err(format!("unreasonable size {w}x{h}"));
            }

            // 16 bits a channel when the source has more than 8 (10-bit HEIF).
            let deep = bits_per_channel(f, &frame).map(|b| b > 8).unwrap_or(false);
            let bpp: u64 = if deep { 8 } else { 4 };
            if u64::from(w) * u64::from(h) * bpp > max_bytes {
                return Err(format!("{w}x{h} is over the memory limit"));
            }

            let mut source: IWICBitmapSource = frame.cast().map_err(e("frame"))?;
            if colour == Colour::Srgb {
                if let Some(converted) = to_srgb(f, &frame, &source, deep, icc) {
                    source = converted;
                }
            }

            let target = if deep {
                GUID_WICPixelFormat64bppRGBA
            } else {
                GUID_WICPixelFormat32bppRGBA
            };
            let conv = f.CreateFormatConverter().map_err(e("converter"))?;
            conv.Initialize(
                &source,
                &target,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeCustom,
            )
            .map_err(e("pixel format"))?;
            let stride = (w as usize) * (bpp as usize);
            let mut buf = vec![0u8; stride * h as usize];
            conv.CopyPixels(std::ptr::null(), stride as u32, &mut buf)
                .map_err(e("decode"))?;
            if deep {
                let px: Vec<u16> = buf
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();
                image::ImageBuffer::from_raw(w, h, px)
                    .map(DynamicImage::ImageRgba16)
                    .ok_or_else(|| "buffer size".to_string())
            } else {
                image::ImageBuffer::from_raw(w, h, buf)
                    .map(DynamicImage::ImageRgba8)
                    .ok_or_else(|| "buffer size".to_string())
            }
        }
    }

    fn bits_per_channel(f: &IWICImagingFactory, frame: &IWICBitmapFrameDecode) -> Option<u32> {
        unsafe {
            let fmt: GUID = frame.GetPixelFormat().ok()?;
            let info: IWICPixelFormatInfo = f.CreateComponentInfo(&fmt).ok()?.cast().ok()?;
            let bits = info.GetBitsPerPixel().ok()?;
            let channels = info.GetChannelCount().ok()?.max(1);
            Some(bits / channels)
        }
    }

    /// A profile the frame itself reports. WIC's HEIF decoder reports none
    /// even when the file has one (measured), which is why the caller hands
    /// in the container's; this covers a decoder that one day does.
    fn frame_profile(
        f: &IWICImagingFactory,
        frame: &IWICBitmapFrameDecode,
    ) -> Option<IWICColorContext> {
        unsafe {
            let mut count = 0u32;
            frame.GetColorContexts(&mut [], &mut count).ok()?;
            if count == 0 {
                return None;
            }
            let mut contexts: Vec<Option<IWICColorContext>> = Vec::new();
            for _ in 0..count {
                contexts.push(Some(f.CreateColorContext().ok()?));
            }
            frame.GetColorContexts(&mut contexts, &mut count).ok()?;
            contexts
                .into_iter()
                .flatten()
                .find(|c| c.GetType().ok() == Some(WICColorContextProfile))
        }
    }

    /// The pixels converted from `icc` (the container's profile) or the
    /// frame's own to sRGB, or `None` when there is no profile (already sRGB,
    /// or described by `nclx`) or the transform can't be built — the caller
    /// then keeps the original.
    fn to_srgb(
        f: &IWICImagingFactory,
        frame: &IWICBitmapFrameDecode,
        source: &IWICBitmapSource,
        deep: bool,
        icc: Option<&[u8]>,
    ) -> Option<IWICBitmapSource> {
        unsafe {
            let src_ctx = match icc {
                Some(bytes) => {
                    let ctx = f.CreateColorContext().ok()?;
                    ctx.InitializeFromMemory(bytes).ok()?;
                    ctx
                }
                None => frame_profile(f, frame)?,
            };
            let dst_ctx = f.CreateColorContext().ok()?;
            dst_ctx.InitializeFromExifColorSpace(1).ok()?; // 1 = sRGB
            let xf = f.CreateColorTransformer().ok()?;
            let fmt = if deep {
                GUID_WICPixelFormat64bppRGBA
            } else {
                GUID_WICPixelFormat32bppBGRA
            };
            xf.Initialize(source, &src_ctx, &dst_ctx, &fmt).ok()?;
            xf.cast().ok()
        }
    }

    /// Encode `rgba` (8-bit RGBA, `w` x `h`) as a HEIC with Windows' own
    /// encoder, optionally marked rotated — the tests' fixture source.
    #[cfg(test)]
    pub(super) fn encode_heic(
        rgba: &[u8],
        w: u32,
        h: u32,
        orientation: Option<u16>,
        icc: Option<&std::path::Path>,
    ) -> Result<Vec<u8>, String> {
        use std::os::windows::ffi::OsStrExt;
        let _com = Com::enter();
        let e = |what: &'static str| {
            move |err: windows::core::Error| format!("{what}: {}", err.message().trim())
        };
        unsafe {
            let f = factory().map_err(e("factory"))?;
            let encoder = f
                .CreateEncoder(&GUID_ContainerFormatHeif, std::ptr::null())
                .map_err(e("encoder"))?;
            let mem = StructuredStorage::CreateStreamOnHGlobal(None, true).map_err(e("stream"))?;
            encoder
                .Initialize(&mem, WICBitmapEncoderNoCache)
                .map_err(e("init"))?;
            let mut frame = None;
            let mut bag = None;
            encoder
                .CreateNewFrame(&mut frame, &mut bag)
                .map_err(e("frame"))?;
            let frame = frame.ok_or("no frame")?;
            frame.Initialize(bag.as_ref()).map_err(e("frame init"))?;
            frame.SetSize(w, h).map_err(e("size"))?;
            let mut fmt = GUID_WICPixelFormat32bppBGRA;
            frame.SetPixelFormat(&mut fmt).map_err(e("format"))?;
            if let Some(path) = icc {
                let ctx = f.CreateColorContext().map_err(e("colour context"))?;
                let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
                ctx.InitializeFromFilename(windows::core::PCWSTR(wide.as_ptr()))
                    .map_err(e("profile"))?;
                frame
                    .SetColorContexts(&[Some(ctx)])
                    .map_err(e("set profile"))?;
            }
            if let Some(o) = orientation {
                let writer = frame.GetMetadataQueryWriter().map_err(e("metadata"))?;
                let v = windows::core::PROPVARIANT::from(o);
                writer
                    .SetMetadataByName(windows::core::w!("System.Photo.Orientation"), &v)
                    .map_err(e("orientation"))?;
            }
            // The encoder wants BGRA.
            let mut bgra = rgba.to_vec();
            for p in bgra.chunks_exact_mut(4) {
                p.swap(0, 2);
            }
            frame.WritePixels(h, w * 4, &bgra).map_err(e("pixels"))?;
            frame.Commit().map_err(e("frame commit"))?;
            encoder.Commit().map_err(e("commit"))?;
            // Read the stream back.
            let hglobal = StructuredStorage::GetHGlobalFromStream(&mem).map_err(e("hglobal"))?;
            let len = windows::Win32::System::Memory::GlobalSize(hglobal);
            let ptr = windows::Win32::System::Memory::GlobalLock(hglobal) as *const u8;
            if ptr.is_null() {
                return Err("lock".into());
            }
            let out = std::slice::from_raw_parts(ptr, len).to_vec();
            let _ = windows::Win32::System::Memory::GlobalUnlock(hglobal);
            Ok(out)
        }
    }
}

#[cfg(not(windows))]
mod wic {
    use super::{Colour, Support};
    use image::DynamicImage;
    pub(super) fn probe() -> Support {
        Support::Unsupported
    }
    pub(super) fn decode(
        _: &[u8],
        _: Colour,
        _: Option<&[u8]>,
        _: u32,
        _: u64,
    ) -> Result<DynamicImage, String> {
        Err("HEIC photos can only be read on Windows".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- a hand-built container, for the parse alone -------------------

    fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v
    }

    fn full(kind: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
        let mut b = vec![version];
        b.extend_from_slice(&flags.to_be_bytes()[1..]);
        b.extend_from_slice(body);
        bx(kind, &b)
    }

    fn infe(id: u16, kind: &[u8; 4]) -> Vec<u8> {
        let mut b = id.to_be_bytes().to_vec();
        b.extend_from_slice(&0u16.to_be_bytes());
        b.extend_from_slice(kind);
        b.push(0);
        full(b"infe", 2, 0, &b)
    }

    /// TIFF (little-endian) with one IFD0 entry: Orientation = 6.
    fn tiff() -> Vec<u8> {
        let mut t = b"II*\0".to_vec();
        t.extend_from_slice(&8u32.to_le_bytes());
        t.extend_from_slice(&1u16.to_le_bytes());
        t.extend_from_slice(&0x0112u16.to_le_bytes());
        t.extend_from_slice(&3u16.to_le_bytes());
        t.extend_from_slice(&1u32.to_le_bytes());
        t.extend_from_slice(&6u32.to_le_bytes());
        t.extend_from_slice(&0u32.to_le_bytes());
        t
    }

    /// A HEIF with a primary item 1 (`ispe` 4032x3024 coded as 4096x3072
    /// plus `clap`, `irot` one quarter turn anticlockwise, a `prof` colour
    /// profile) and an `Exif` item 2 stored in `mdat` — the layout of an
    /// iPhone photo, minus the pictures.
    fn container(props_extra: &[Vec<u8>]) -> Vec<u8> {
        let ftyp = bx(b"ftyp", b"heic\0\0\0\0mif1heicmiaf");
        let mut exif = 6u32.to_be_bytes().to_vec();
        exif.extend_from_slice(b"Exif\0\0");
        exif.extend_from_slice(&tiff());

        let meta_for = |exif_at: u32| {
            let hdlr = full(
                b"hdlr",
                0,
                0,
                &[
                    0, 0, 0, 0, b'p', b'i', b'c', b't', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                ],
            );
            let pitm = full(b"pitm", 0, 0, &1u16.to_be_bytes());
            let mut iinf = 2u16.to_be_bytes().to_vec();
            iinf.extend(infe(1, b"hvc1"));
            iinf.extend(infe(2, b"Exif"));
            let iinf = full(b"iinf", 0, 0, &iinf);
            let mut iloc = vec![0x44, 0x00];
            iloc.extend_from_slice(&1u16.to_be_bytes()); // items
            iloc.extend_from_slice(&2u16.to_be_bytes()); // id
            iloc.extend_from_slice(&0u16.to_be_bytes()); // method 0
            iloc.extend_from_slice(&0u16.to_be_bytes()); // data ref
            iloc.extend_from_slice(&1u16.to_be_bytes()); // extents
            iloc.extend_from_slice(&exif_at.to_be_bytes());
            iloc.extend_from_slice(&(exif.len() as u32).to_be_bytes());
            let iloc = full(b"iloc", 1, 0, &iloc);
            let mut ispe = 4096u32.to_be_bytes().to_vec();
            ispe.extend_from_slice(&3072u32.to_be_bytes());
            let mut clap = Vec::new();
            for v in [4032u32, 1, 3024, 1, 0, 1, 0, 1] {
                clap.extend_from_slice(&v.to_be_bytes());
            }
            let mut colr = b"prof".to_vec();
            colr.extend_from_slice(b"fake-icc-profile");
            let mut props = vec![
                full(b"ispe", 0, 0, &ispe),
                bx(b"clap", &clap),
                bx(b"irot", &[1]),
                bx(b"colr", &colr),
            ];
            props.extend(props_extra.iter().cloned());
            let n = props.len() as u8;
            let ipco = bx(b"ipco", &props.concat());
            let mut ipma = 1u32.to_be_bytes().to_vec();
            ipma.extend_from_slice(&1u16.to_be_bytes());
            ipma.push(n);
            for i in 1..=n {
                ipma.push(if i == 3 { 0x80 | i } else { i });
            }
            let ipma = full(b"ipma", 0, 0, &ipma);
            let mut iprp = ipco;
            iprp.extend(ipma);
            let iprp = bx(b"iprp", &iprp);
            full(b"meta", 0, 0, &[hdlr, pitm, iinf, iloc, iprp].concat())
        };
        let meta_len = meta_for(0).len();
        let exif_at = (ftyp.len() + meta_len + 8) as u32;
        let mut file = ftyp;
        file.extend(meta_for(exif_at));
        file.extend(bx(b"mdat", &exif));
        file
    }

    #[test]
    fn the_container_gives_size_turn_exif_and_profile() {
        let c = parse(&container(&[]));
        assert_eq!(c.size, Some((4032, 3024)), "clap wins over the coded size");
        // One quarter turn anticlockwise = EXIF 8.
        assert_eq!(c.orientation, Some(Orientation::Rotate270));
        assert_eq!(c.exif.as_deref(), Some(tiff().as_slice()));
        assert_eq!(c.icc.as_deref(), Some(b"fake-icc-profile".as_slice()));
    }

    #[test]
    fn a_mirror_after_the_turn_is_read_as_windows_writes_it() {
        // imir axis 1 = left↔right after the anticlockwise quarter turn.
        let c = parse(&container(&[bx(b"imir", &[1])]));
        assert_eq!(c.orientation, Some(Orientation::Rotate270FlipH));
        let c = parse(&container(&[bx(b"imir", &[0])]));
        assert_eq!(c.orientation, Some(Orientation::Rotate90FlipH));
    }

    #[test]
    fn orientation_covers_every_turn_and_mirror() {
        use Orientation::*;
        assert_eq!(orientation_of(0, None, false), None);
        assert_eq!(orientation_of(1, None, false), Some(Rotate270));
        assert_eq!(orientation_of(2, None, false), Some(Rotate180));
        assert_eq!(orientation_of(3, None, false), Some(Rotate90));
        assert_eq!(orientation_of(0, Some(1), true), Some(FlipHorizontal));
        assert_eq!(orientation_of(0, Some(0), true), Some(FlipVertical));
        assert_eq!(orientation_of(2, Some(1), false), Some(FlipVertical));
        // Mirror first, then a quarter turn anticlockwise: EXIF 7's
        // "transverse" (rotate 90 cw then flip) — the same as the turn the
        // other way round followed by the mirror.
        assert_eq!(orientation_of(1, Some(1), true), Some(Rotate90FlipH));
    }

    #[test]
    fn dimensions_come_from_the_header_turned_upright() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("IMG_0001.HEIC");
        std::fs::write(&p, container(&[])).unwrap();
        assert_eq!(dimensions(&p), Some((3024, 4032)));
    }

    #[test]
    fn a_broken_container_reads_as_nothing_rather_than_panicking() {
        let whole = container(&[]);
        for cut in 0..whole.len() {
            let _ = parse(&whole[..cut]);
        }
        let mut junk = whole.clone();
        for (i, b) in junk.iter_mut().enumerate().skip(40) {
            if i % 7 == 0 {
                *b = 0xff;
            }
        }
        let _ = parse(&junk);
        assert_eq!(parse(b"not a heif at all"), Container::default());
    }

    #[test]
    fn sniff_takes_heic_and_leaves_avif_and_mp4() {
        assert!(sniff(&container(&[])));
        assert!(sniff(&bx(b"ftyp", b"mif1\0\0\0\0mif1heic")));
        assert!(!sniff(&bx(b"ftyp", b"avif\0\0\0\0avifmif1miaf")));
        assert!(!sniff(&bx(b"ftyp", b"isom\0\0\0\0isomiso2mp41")));
        assert!(!sniff(b"\x89PNG\r\n\x1a\n"));
    }

    #[test]
    fn heic_is_known_by_extension_in_any_case_or_by_content() {
        let dir = tempfile::tempdir().unwrap();
        let upper = dir.path().join("IMG_0001.HEIC");
        std::fs::write(&upper, b"").unwrap();
        assert!(is_heif_path(&upper));
        let renamed = dir.path().join("photo.bin");
        std::fs::write(&renamed, container(&[])).unwrap();
        assert!(is_heif_path(&renamed));
        let png = dir.path().join("a.png");
        std::fs::write(&png, b"\x89PNG\r\n\x1a\n").unwrap();
        assert!(!is_heif_path(&png));
    }

    #[test]
    fn a_missing_codec_is_named_with_its_store_package() {
        assert!(missing_codec_hint(Support::Ready).is_none());
        assert!(missing_codec_hint(Support::NoHeif)
            .unwrap()
            .contains("HEIF Image Extensions"));
        assert!(missing_codec_hint(Support::NoHevc)
            .unwrap()
            .contains("HEVC Video Extensions"));
    }

    #[test]
    fn the_probe_photo_is_a_small_heic() {
        let probe = include_bytes!("heif_probe.heic");
        assert!(sniff(probe));
        assert!(probe.len() < 4096, "keep the embedded probe small");
        assert_eq!(parse(probe).size, Some((16, 16)));
    }

    // ---- Windows' codec --------------------------------------------------
    // Each test checks `support()` first and passes, saying so, on a machine
    // without the Store codecs (a CI runner may lack the HEVC one).

    #[cfg(windows)]
    fn codecs_present(test: &str) -> bool {
        let s = support();
        if s != Support::Ready {
            eprintln!("{test}: skipped, Windows can't decode HEIC here ({s:?})");
        }
        s == Support::Ready
    }

    /// 64x32 (or `w` x `h`): a green 16x16 block in the top-left corner on a
    /// red left half and a blue right half, so any turn or mirror shows.
    #[cfg(windows)]
    fn marker(w: u32, h: u32) -> Vec<u8> {
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let c = if x < 16 && y < 16 {
                    [0, 255, 0, 255]
                } else if x < w / 2 {
                    [255, 0, 0, 255]
                } else {
                    [0, 0, 255, 255]
                };
                px.extend_from_slice(&c);
            }
        }
        px
    }

    #[cfg(windows)]
    fn is_green(p: &image::Rgba<u8>) -> bool {
        p[1] > 200 && p[0] < 60 && p[2] < 60
    }

    /// Regenerates `heif_probe.heic` with this machine's HEIF encoder:
    /// `cargo test -p kuvatin-core write_probe_fixture -- --ignored`.
    #[cfg(windows)]
    #[test]
    #[ignore = "writes the embedded probe fixture"]
    fn write_probe_fixture() {
        let grey = vec![128u8; 16 * 16 * 4];
        let heic = wic::encode_heic(&grey, 16, 16, None, None).expect("HEIF encoder");
        let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/heif_probe.heic");
        std::fs::write(out, heic).unwrap();
    }

    /// The finding the decode rests on: WIC applies `irot`/`imir` itself,
    /// so Kuvatin must not turn the picture a second time.
    #[cfg(windows)]
    #[test]
    fn windows_applies_the_container_transforms() {
        if !codecs_present("windows_applies_the_container_transforms") {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        // (EXIF orientation written, upright size, where the green block is)
        type Case = (Option<u16>, (u32, u32), (u32, u32));
        let cases: [Case; 4] = [
            (None, (64, 32), (2, 2)),
            (Some(6), (32, 64), (29, 2)),
            (Some(3), (64, 32), (61, 29)),
            (Some(2), (64, 32), (61, 2)),
        ];
        for (o, size, green) in cases {
            let heic = match wic::encode_heic(&marker(64, 32), 64, 32, o, None) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("skipped orientation {o:?}: no HEIF encoder ({e})");
                    continue;
                }
            };
            assert_eq!(
                parse(&heic).orientation,
                o.and_then(|o| Orientation::from_exif(o as u8)),
                "the container holds the transform for {o:?}"
            );
            let path = dir.path().join(format!("o{}.heic", o.unwrap_or(1)));
            std::fs::write(&path, &heic).unwrap();
            assert_eq!(dimensions(&path), Some(size), "header size for {o:?}");
            let (img, _) = decode(&path, Colour::AsStored, 1000, 1 << 30).unwrap();
            let rgba = img.to_rgba8();
            assert_eq!(rgba.dimensions(), size, "decoded size for {o:?}");
            assert!(
                is_green(rgba.get_pixel(green.0, green.1)),
                "green corner for {o:?} at {green:?}"
            );
        }
    }

    /// The whole Images-mode path: a .heic goes in, a JPEG comes out.
    #[cfg(windows)]
    #[test]
    fn a_heic_converts_through_the_image_pipeline() {
        if !codecs_present("a_heic_converts_through_the_image_pipeline") {
            return;
        }
        let Ok(heic) = wic::encode_heic(&marker(64, 32), 64, 32, Some(6), None) else {
            eprintln!("skipped: no HEIF encoder");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("IMG_4242.HEIC");
        std::fs::write(&input, heic).unwrap();
        let img = crate::pipeline::decode_oriented(&input).unwrap();
        assert_eq!((img.width(), img.height()), (32, 64));
        let job = crate::pipeline::Job {
            format: crate::format::OutputFormat::Jpeg,
            ..Default::default()
        };
        let out = dir.path().join("out.jpg");
        crate::pipeline::process_file_to(&input, &job, &out).unwrap();
        let back = image::open(&out).unwrap();
        assert_eq!((back.width(), back.height()), (32, 64));
        let job = crate::pipeline::Job::default();
        assert!(crate::batch::estimate_cost(&input, &job) > 0);
    }

    /// For the video editor: an sRGB PNG GStreamer can read.
    #[cfg(windows)]
    #[test]
    fn a_heic_becomes_a_png_for_the_video_editor() {
        if !codecs_present("a_heic_becomes_a_png_for_the_video_editor") {
            return;
        }
        let Ok(heic) = wic::encode_heic(&marker(64, 32), 64, 32, Some(6), None) else {
            eprintln!("skipped: no HEIF encoder");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("IMG_4242.HEIC");
        std::fs::write(&input, heic).unwrap();
        let out = dir.path().join("cache").join("IMG_4242.png");
        convert_to_png(&input, &out).unwrap();
        let png = image::open(&out).unwrap().to_rgba8();
        assert_eq!(png.dimensions(), (32, 64));
        assert!(is_green(png.get_pixel(29, 2)));
        assert!(!out.with_extension("png.part").exists());
    }

    /// A photo with a wide-gamut profile (an iPhone's is Display P3; Adobe
    /// RGB stands in, from Windows' colour folder) keeps it in Images mode
    /// and is converted to sRGB for the video editor.
    #[cfg(windows)]
    #[test]
    fn a_wide_gamut_profile_is_kept_or_converted() {
        if !codecs_present("a_wide_gamut_profile_is_kept_or_converted") {
            return;
        }
        let windir = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        let icc = Path::new(&windir).join(r"System32\spool\drivers\color\AdobeRGB1998.icc");
        if !icc.exists() {
            eprintln!("skipped: no Adobe RGB profile in the colour folder");
            return;
        }
        let colour = [40u8, 180, 60, 255];
        let px: Vec<u8> = colour.iter().copied().cycle().take(32 * 32 * 4).collect();
        let heic = match wic::encode_heic(&px, 32, 32, None, Some(&icc)) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("skipped: encoder took no profile ({e})");
                return;
            }
        };
        let profile = std::fs::read(&icc).unwrap();
        assert_eq!(parse(&heic).icc.as_deref(), Some(profile.as_slice()));
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("wide.heic");
        std::fs::write(&input, heic).unwrap();

        let (stored, meta) = decode(&input, Colour::AsStored, 1000, 1 << 30).unwrap();
        assert_eq!(meta.icc.as_deref(), Some(profile.as_slice()));
        let (srgb, meta) = decode(&input, Colour::Srgb, 1000, 1 << 30).unwrap();
        assert!(meta.icc.is_none(), "converted pixels carry no profile");
        let a = *stored.to_rgba8().get_pixel(16, 16);
        let b = *srgb.to_rgba8().get_pixel(16, 16);
        let diff: i32 = (0..3).map(|i| (a[i] as i32 - b[i] as i32).abs()).sum();
        // Adobe RGB green is far outside sRGB's: the conversion must show.
        assert!(diff > 20, "stored {a:?} vs sRGB {b:?}");
    }
}
