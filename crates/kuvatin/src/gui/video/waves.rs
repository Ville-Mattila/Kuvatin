//! The sound drawn in each clip block. A view, not an edit: decoded once per
//! source on a worker, kept by URI for the session, never saved, never
//! recorded, and always safe to throw away and decode again. Only video
//! files are listened to: a still or an image sequence has no sound.

use super::project_file::kind_of;
use crate::gui::{AppWindow, ClipKind};
use slint::{Image, Model, Rgba8Pixel, SharedPixelBuffer, SharedString};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// The waveform picture's size. It spans the whole source and each clip shows
/// a window of it, so it is wide: a few minutes of source still give a column
/// every few pixels of a zoomed-in clip.
const WAVE_W: u32 = 4096;
const WAVE_H: u32 = 32;

/// One source's waveform and the seconds of source it spans. The pixels, not
/// a `slint::Image`, so the worker can hand it over.
type Wave = (SharedPixelBuffer<Rgba8Pixel>, f32);

/// Clips whose waveform is known, each with it.
type Ready = Vec<(SharedString, Wave)>;

#[derive(Default)]
struct Inner {
    /// Decoded sources by URI; `None` for a source with no sound, so it is
    /// not decoded again.
    done: HashMap<String, Option<Wave>>,
    /// Sources being decoded, each with the clips waiting for it.
    waiting: HashMap<String, Vec<SharedString>>,
}

impl Inner {
    /// Sort `clips` (clip id, source URI) into those whose waveform is known
    /// and the sources to decode, each once however many clips wait on it. A
    /// clip that is not a video, or whose source has no sound, gets nothing.
    fn request(&mut self, clips: Vec<(SharedString, String)>) -> (Ready, Vec<String>) {
        let mut ready = Vec::new();
        let mut decode = Vec::new();
        for (id, uri) in clips {
            if kind_of(&uri) != ClipKind::Video {
                continue;
            }
            match self.done.get(&uri) {
                Some(Some(wave)) => ready.push((id, wave.clone())),
                Some(None) => {}
                None => {
                    let waiting = self.waiting.entry(uri.clone()).or_default();
                    if waiting.is_empty() {
                        decode.push(uri);
                    }
                    waiting.push(id);
                }
            }
        }
        (ready, decode)
    }

    /// A source finished decoding: keep it, and hand back the clips that
    /// were waiting for it.
    fn finish(&mut self, uri: &str, wave: Option<Wave>) -> Vec<SharedString> {
        self.done.insert(uri.to_string(), wave);
        self.waiting.remove(uri).unwrap_or_default()
    }
}

/// The waveform cache, shared with the worker that fills it.
#[derive(Clone, Default)]
pub(super) struct Waves(Arc<Mutex<Inner>>);

impl Waves {
    /// Give each clip its source's waveform: at once from the cache, else once
    /// a worker has decoded it. `clips` are (clip id, source URI). A failure
    /// to decode is silent: a missing picture of the sound is better than a
    /// dialog about one.
    pub(super) fn fill(&self, ui_weak: slint::Weak<AppWindow>, clips: Vec<(SharedString, String)>) {
        let Ok((ready, decode)) = self.0.lock().map(|mut inner| inner.request(clips)) else {
            return;
        };
        if let Some(ui) = ui_weak.upgrade() {
            for (id, wave) in &ready {
                set_wave(&ui, id, wave);
            }
        }
        if decode.is_empty() {
            return;
        }
        let inner = self.0.clone();
        let _ = std::thread::Builder::new()
            .name("kuvatin-waveforms".into())
            .spawn(move || {
                for uri in decode {
                    let wave =
                        kuvatin_video::waveform_uri(&uri, WAVE_W, WAVE_H).map(|(f, secs)| {
                            (
                                SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
                                    &f.rgba, f.width, f.height,
                                ),
                                secs as f32,
                            )
                        });
                    let Ok(ids) = inner.lock().map(|mut i| i.finish(&uri, wave.clone())) else {
                        continue;
                    };
                    let Some(wave) = wave else {
                        continue;
                    };
                    let ui_weak = ui_weak.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = ui_weak.upgrade() {
                            for id in &ids {
                                set_wave(&ui, id, &wave);
                            }
                        }
                    });
                }
            });
    }
}

/// Put a waveform into the timeline row of clip `id`, if it is still there.
fn set_wave(ui: &AppWindow, id: &SharedString, wave: &Wave) {
    let clips = ui.get_timeline_clips();
    for i in 0..clips.row_count() {
        if let Some(mut row) = clips.row_data(i) {
            if row.id == *id {
                row.wave = Image::from_rgba8(wave.0.clone());
                row.wave_secs = wave.1;
                clips.set_row_data(i, row);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLIP: &str = "file:///C:/media/intro.mp4";

    fn wave() -> Wave {
        (SharedPixelBuffer::new(4, 1), 2.0)
    }

    fn ids(v: &[SharedString]) -> Vec<&str> {
        v.iter().map(|s| s.as_str()).collect()
    }

    #[test]
    fn a_source_is_decoded_once_however_many_clips_wait_for_it() {
        let mut inner = Inner::default();
        let (ready, decode) =
            inner.request(vec![("a".into(), CLIP.into()), ("b".into(), CLIP.into())]);
        assert!(ready.is_empty());
        assert_eq!(decode, vec![CLIP.to_string()]);
        let (_, again) = inner.request(vec![("c".into(), CLIP.into())]);
        assert!(again.is_empty(), "already being decoded");
        assert_eq!(ids(&inner.finish(CLIP, Some(wave()))), vec!["a", "b", "c"]);
        let (ready, decode) = inner.request(vec![("d".into(), CLIP.into())]);
        let got: Vec<&str> = ready.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(got, vec!["d"], "from the cache");
        assert!(decode.is_empty());
    }

    #[test]
    fn a_source_with_no_sound_is_not_decoded_again() {
        let mut inner = Inner::default();
        let _ = inner.request(vec![("a".into(), CLIP.into())]);
        inner.finish(CLIP, None);
        let (ready, decode) = inner.request(vec![("b".into(), CLIP.into())]);
        assert!(ready.is_empty() && decode.is_empty());
    }

    #[test]
    fn stills_and_sequences_are_not_listened_to() {
        let mut inner = Inner::default();
        let (ready, decode) = inner.request(vec![
            ("a".into(), "file:///C:/media/logo.png".into()),
            (
                "b".into(),
                "imagesequence://C:/r/f_%04d.png?start-index=1&framerate=24/1".into(),
            ),
        ]);
        assert!(ready.is_empty() && decode.is_empty());
    }
}
