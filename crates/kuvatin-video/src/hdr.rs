//! HDR video on an SDR canvas.
//!
//! An iPhone records HDR by default (HLG in BT.2020, 10-bit HEVC), and any
//! copy that arrives untouched (AirDrop, a cable import, iCloud's "original")
//! keeps it. GStreamer converts such a picture to the editor's 8-bit RGB by
//! the numbers alone, with no transfer or primaries conversion, so it came out
//! dim and washed out: reference white at half brightness, colours greyed.
//!
//! `kuvatinhdr` maps it the way an SDR display expects. On 16-bit RGB it
//! linearises the signal (HLG: inverse OETF and the 1.2 system gamma; PQ: the
//! ST 2084 EOTF) relative to HDR reference white (203 cd/m², BT.2408), turns
//! BT.2020 primaries into BT.709, rolls highlights off above a knee instead of
//! clipping them, and encodes with the BT.709 OETF. Everything per pixel is a
//! table lookup, a 3×3 matrix and a square root, spread over threads.
//!
//! The engine puts the element on a clip as a GES effect when discovery says
//! the clip's video is HLG or PQ (see `project::apply_hdr`), and the thumbnailer
//! runs frames through it the same way.

use gst::glib;
use gst::prelude::*;
use gstreamer as gst;
use gstreamer_video as gst_video;

/// The element's factory name.
pub const ELEMENT: &str = "kuvatinhdr";

/// The HDR transfer a stream is in, from its caps' colorimetry.
pub fn transfer_of(caps: &gst::CapsRef) -> Option<&'static str> {
    let s = caps.structure(0)?;
    let c: gst_video::VideoColorimetry = s.get::<&str>("colorimetry").ok()?.parse().ok()?;
    match c.transfer() {
        gst_video::VideoTransferFunction::AribStdB67 => Some("hlg"),
        gst_video::VideoTransferFunction::Smpte2084 => Some("pq"),
        _ => None,
    }
}

/// Register `kuvatinhdr` with this process's registry. Idempotent.
pub fn register() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if let Err(e) =
            gst::Element::register(None, ELEMENT, gst::Rank::NONE, HdrToSdr::static_type())
        {
            eprintln!("kuvatin: could not register {ELEMENT}: {e}");
        }
    });
}

glib::wrapper! {
    pub struct HdrToSdr(ObjectSubclass<imp::HdrToSdr>)
        @extends gst_video::VideoFilter, gstreamer_base::BaseTransform, gst::Element, gst::Object;
}

/// The pure mapping, kept apart from the element so it can be tested alone.
pub(crate) mod curve {
    /// HDR reference white, cd/m² (ITU-R BT.2408).
    const REF_WHITE: f32 = 203.0;
    /// An HLG display's nominal peak, cd/m², the scale its system gamma of
    /// 1.2 is defined for.
    const HLG_PEAK: f32 = 1000.0;
    /// Linear SDR values up to here pass straight through; above it they roll
    /// off towards 1.0 instead of clipping. Reference white lands near 0.93.
    const KNEE: f32 = 0.8;
    /// The output table covers linear values up to this many times reference
    /// white; past it everything is white already.
    pub(crate) const X_MAX: f32 = 16.0;
    /// Steps in the output table, spaced by square root so the shadows, where
    /// the OETF is steep, get most of them.
    pub(crate) const OUT_STEPS: usize = 16384;

    /// HLG signal (0..1) → display light relative to reference white.
    pub(crate) fn hlg(e: f32) -> f32 {
        const A: f32 = 0.178_832_77;
        const B: f32 = 0.284_668_92;
        const C: f32 = 0.559_910_7;
        let scene = if e <= 0.5 {
            e * e / 3.0
        } else {
            (((e - C) / A).exp() + B) / 12.0
        };
        // The OOTF, applied per channel rather than through luminance: close
        // to the standard for the moderate saturation of real footage.
        scene.max(0.0).powf(1.2) * HLG_PEAK / REF_WHITE
    }

    /// PQ signal (0..1) → light relative to reference white.
    pub(crate) fn pq(e: f32) -> f32 {
        const M1: f32 = 2610.0 / 16384.0;
        const M2: f32 = 2523.0 / 4096.0 * 128.0;
        const C1: f32 = 3424.0 / 4096.0;
        const C2: f32 = 2413.0 / 4096.0 * 32.0;
        const C3: f32 = 2392.0 / 4096.0 * 32.0;
        let p = e.max(0.0).powf(1.0 / M2);
        let nits = 10000.0 * ((p - C1).max(0.0) / (C2 - C3 * p)).powf(1.0 / M1);
        nits / REF_WHITE
    }

    /// Linear light relative to reference white → an SDR linear value 0..1:
    /// straight up to the knee, then an exponential shoulder.
    pub(crate) fn tone(x: f32) -> f32 {
        if x <= KNEE {
            x.max(0.0)
        } else {
            KNEE + (1.0 - KNEE) * (1.0 - (-(x - KNEE) / (1.0 - KNEE)).exp())
        }
    }

    /// The BT.709 OETF.
    pub(crate) fn oetf709(l: f32) -> f32 {
        if l < 0.018 {
            4.5 * l
        } else {
            1.099 * l.powf(0.45) - 0.099
        }
    }

    /// BT.2020 → BT.709 primaries, on linear light (ITU-R BT.2087).
    pub(crate) const BT2020_TO_709: [[f32; 3]; 3] = [
        [1.660_491, -0.587_641_1, -0.072_849_9],
        [-0.124_550_5, 1.132_899_9, -0.008_349_4],
        [-0.018_150_8, -0.100_578_9, 1.118_729_7],
    ];

    /// The tables for one transfer: 16-bit code → linear, and linear (by
    /// square-root index) → 16-bit output code.
    pub(crate) struct Luts {
        pub(crate) input: Vec<f32>,
        pub(crate) output: Vec<u16>,
    }

    pub(crate) fn luts(pq_input: bool) -> Luts {
        let input = (0..=u16::MAX)
            .map(|v| {
                let e = f32::from(v) / 65535.0;
                if pq_input {
                    pq(e)
                } else {
                    hlg(e)
                }
            })
            .collect();
        let output = (0..=OUT_STEPS)
            .map(|i| {
                let r = i as f32 / OUT_STEPS as f32;
                let x = r * r * X_MAX;
                let v = oetf709(tone(x)).clamp(0.0, 1.0);
                (v * 65535.0).round() as u16
            })
            .collect();
        Luts { input, output }
    }

    impl Luts {
        /// One linear value (relative to reference white) → output code.
        #[inline]
        pub(crate) fn encode(&self, x: f32) -> u16 {
            let r = (x.max(0.0) / X_MAX).sqrt();
            let i = (r * OUT_STEPS as f32) as usize;
            self.output[i.min(OUT_STEPS)]
        }

        /// One RGB pixel, 16-bit codes in and out.
        #[inline]
        pub(crate) fn pixel(&self, rgb: [u16; 3]) -> [u16; 3] {
            let l = rgb.map(|c| self.input[c as usize]);
            let m = BT2020_TO_709;
            let mut out = [0u16; 3];
            for (k, row) in m.iter().enumerate() {
                out[k] = self.encode(row[0] * l[0] + row[1] * l[1] + row[2] * l[2]);
            }
            out
        }
    }
}

mod imp {
    use super::curve::{luts, Luts};
    use super::{gst, gst_video};
    use gst::glib;
    use gst::prelude::*;
    use gst::subclass::prelude::*;
    use gst_video::subclass::prelude::*;
    use gst_video::VideoFrameExt;
    use gstreamer_base::subclass::BaseTransformMode;
    use rayon::prelude::*;
    use std::sync::{Arc, LazyLock, Mutex};

    /// What the element does: map HLG, map PQ, or copy (a stream that turned
    /// out not to be HDR, which the thumbnailer only learns once it is linked).
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Mode {
        Hlg,
        Pq,
        Copy,
    }

    impl Mode {
        fn parse(s: &str) -> Mode {
            match s {
                "hlg" => Mode::Hlg,
                "pq" => Mode::Pq,
                _ => Mode::Copy,
            }
        }
        fn nick(self) -> &'static str {
            match self {
                Mode::Hlg => "hlg",
                Mode::Pq => "pq",
                Mode::Copy => "none",
            }
        }
    }

    pub struct HdrToSdr {
        mode: Mutex<Mode>,
        luts: Mutex<Option<Arc<Luts>>>,
    }

    impl Default for HdrToSdr {
        fn default() -> Self {
            Self {
                mode: Mutex::new(Mode::Hlg),
                luts: Mutex::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for HdrToSdr {
        const NAME: &'static str = "KuvatinHdrToSdr";
        type Type = super::HdrToSdr;
        type ParentType = gst_video::VideoFilter;
    }

    impl ObjectImpl for HdrToSdr {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPS: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
                vec![glib::ParamSpecString::builder("transfer")
                    .nick("Transfer")
                    .blurb("The source's HDR transfer: hlg, pq, or none to copy")
                    .default_value(Some("hlg"))
                    .mutable_playing()
                    .build()]
            });
            PROPS.as_ref()
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            if pspec.name() == "transfer" {
                let s: Option<String> = value.get().unwrap_or(None);
                let mode = Mode::parse(s.as_deref().unwrap_or("hlg"));
                let mut cur = self.mode.lock().unwrap();
                if *cur != mode {
                    *cur = mode;
                    *self.luts.lock().unwrap() = None;
                }
            }
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                "transfer" => self.mode.lock().unwrap().nick().to_value(),
                _ => unimplemented!(),
            }
        }
    }

    impl GstObjectImpl for HdrToSdr {}

    fn rgba64() -> gst::Caps {
        gst_video::VideoCapsBuilder::new()
            .format(gst_video::VideoFormat::Rgba64Le)
            .build()
    }

    /// What the output says it is: SDR BT.709, full-range RGB.
    fn sdr_colorimetry() -> String {
        gst_video::VideoColorimetry::new(
            gst_video::VideoColorRange::Range0_255,
            gst_video::VideoColorMatrix::Rgb,
            gst_video::VideoTransferFunction::Bt709,
            gst_video::VideoColorPrimaries::Bt709,
        )
        .to_string()
    }

    impl ElementImpl for HdrToSdr {
        fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
            static META: LazyLock<gst::subclass::ElementMetadata> = LazyLock::new(|| {
                gst::subclass::ElementMetadata::new(
                    "HDR to SDR",
                    "Filter/Converter/Video",
                    "Maps HLG or PQ BT.2020 video to SDR BT.709",
                    "Kuvatin",
                )
            });
            Some(&*META)
        }

        fn pad_templates() -> &'static [gst::PadTemplate] {
            static TEMPLATES: LazyLock<Vec<gst::PadTemplate>> = LazyLock::new(|| {
                let caps = rgba64();
                vec![
                    gst::PadTemplate::new(
                        "src",
                        gst::PadDirection::Src,
                        gst::PadPresence::Always,
                        &caps,
                    )
                    .unwrap(),
                    gst::PadTemplate::new(
                        "sink",
                        gst::PadDirection::Sink,
                        gst::PadPresence::Always,
                        &caps,
                    )
                    .unwrap(),
                ]
            });
            TEMPLATES.as_ref()
        }
    }

    impl BaseTransformImpl for HdrToSdr {
        const MODE: BaseTransformMode = BaseTransformMode::NeverInPlace;
        const PASSTHROUGH_ON_SAME_CAPS: bool = false;
        const TRANSFORM_IP_ON_PASSTHROUGH: bool = false;

        /// Same size and format both ways; only the colorimetry changes, so
        /// it is left open on the other side.
        fn transform_caps(
            &self,
            _direction: gst::PadDirection,
            caps: &gst::Caps,
            filter: Option<&gst::Caps>,
        ) -> Option<gst::Caps> {
            let mut out = caps.clone();
            for s in out.make_mut().iter_mut() {
                s.remove_field("colorimetry");
            }
            Some(match filter {
                Some(f) => f.intersect_with_mode(&out, gst::CapsIntersectMode::First),
                None => out,
            })
        }

        /// The output is SDR BT.709 whatever came in.
        fn fixate_caps(
            &self,
            direction: gst::PadDirection,
            caps: &gst::Caps,
            othercaps: gst::Caps,
        ) -> gst::Caps {
            let mut other = othercaps;
            if direction == gst::PadDirection::Sink {
                let c = sdr_colorimetry();
                for s in other.make_mut().iter_mut() {
                    s.set("colorimetry", c.as_str());
                }
            }
            self.parent_fixate_caps(direction, caps, other)
        }
    }

    impl VideoFilterImpl for HdrToSdr {
        fn transform_frame(
            &self,
            inframe: &gst_video::VideoFrameRef<&gst::BufferRef>,
            outframe: &mut gst_video::VideoFrameRef<&mut gst::BufferRef>,
        ) -> Result<gst::FlowSuccess, gst::FlowError> {
            let mode = *self.mode.lock().unwrap();
            let width = inframe.width() as usize;
            let row_bytes = width * 8;
            let in_stride = inframe.plane_stride()[0] as usize;
            let out_stride = outframe.plane_stride()[0] as usize;
            let src = inframe.plane_data(0).map_err(|_| gst::FlowError::Error)?;
            let dst = outframe
                .plane_data_mut(0)
                .map_err(|_| gst::FlowError::Error)?;
            if mode == Mode::Copy {
                dst.par_chunks_mut(out_stride)
                    .zip(src.par_chunks(in_stride))
                    .for_each(|(d, s)| d[..row_bytes].copy_from_slice(&s[..row_bytes]));
                return Ok(gst::FlowSuccess::Ok);
            }
            let luts = {
                let mut slot = self.luts.lock().unwrap();
                slot.get_or_insert_with(|| Arc::new(luts(mode == Mode::Pq)))
                    .clone()
            };
            let word = |b: &[u8], i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
            dst.par_chunks_mut(out_stride)
                .zip(src.par_chunks(in_stride))
                .for_each(|(d, s)| {
                    for px in 0..width {
                        let o = px * 8;
                        let rgb = luts.pixel([word(s, o), word(s, o + 2), word(s, o + 4)]);
                        d[o..o + 2].copy_from_slice(&rgb[0].to_le_bytes());
                        d[o + 2..o + 4].copy_from_slice(&rgb[1].to_le_bytes());
                        d[o + 4..o + 6].copy_from_slice(&rgb[2].to_le_bytes());
                        d[o + 6..o + 8].copy_from_slice(&s[o + 6..o + 8]);
                    }
                });
            Ok(gst::FlowSuccess::Ok)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::curve::*;

    /// HDR reference white (HLG 75 %, PQ 203 cd/m²) comes out as a bright
    /// SDR white, not the half-brightness grey the unmapped signal gave.
    #[test]
    fn reference_white_lands_near_sdr_white() {
        let hlg = luts(false);
        let pq_luts = luts(true);
        let hlg_white = hlg.pixel([49151; 3])[0]; // 0.75 of 65535
        let pq_code = (0.580_68_f32 * 65535.0) as u16; // 203 cd/m² in PQ
        let pq_white = pq_luts.pixel([pq_code; 3])[0];
        for (name, v) in [("hlg", hlg_white), ("pq", pq_white)] {
            let frac = f32::from(v) / 65535.0;
            assert!(
                (0.9..=1.0).contains(&frac),
                "{name} reference white → {frac}"
            );
        }
    }

    /// Black stays black, and the peak is white without wrapping.
    #[test]
    fn the_ends_stay_put() {
        for pq_input in [false, true] {
            let l = luts(pq_input);
            assert_eq!(l.pixel([0; 3]), [0; 3]);
            let top = l.pixel([u16::MAX; 3]);
            assert!(top.iter().all(|&c| c > 65000), "{top:?}");
        }
    }

    /// Brighter in, brighter out, all the way up: the shoulder rolls off but
    /// never folds back.
    #[test]
    fn the_curve_never_turns_back() {
        let l = luts(false);
        let mut last = 0u16;
        for v in (0..=u16::MAX).step_by(64) {
            let out = l.pixel([v; 3])[0];
            assert!(out >= last, "{v}: {out} after {last}");
            last = out;
        }
    }

    /// A saturated BT.2020 red stays red in BT.709 (no negative wrap into
    /// another hue), and grey stays grey.
    #[test]
    fn primaries_keep_hue_and_grey() {
        let l = luts(false);
        let red = l.pixel([45000, 0, 0]);
        assert!(red[0] > 30000 && red[1] == 0 && red[2] == 0, "{red:?}");
        let grey = l.pixel([30000; 3]);
        assert!(
            grey[0].abs_diff(grey[1]) < 64 && grey[1].abs_diff(grey[2]) < 64,
            "{grey:?}"
        );
    }
}
