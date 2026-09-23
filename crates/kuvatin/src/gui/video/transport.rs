//! Frame stepping and the J / K / L shuttle. Neither is an edit: nothing is
//! recorded and nothing is marked unsaved. `J` is not reverse play, as it is
//! in other editors: the engine has no reverse (see the clip-edits design),
//! so `J` steps the speed down and then pauses.

use super::VideoState;
use crate::gui::AppWindow;
use slint::ComponentHandle;
use std::cell::Cell;
use std::time::Duration;

/// The forward speeds the shuttle climbs through.
const LADDER: [f64; 4] = [1.0, 2.0, 4.0, 8.0];

/// What a shuttle key asks the transport to do.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Shuttle {
    /// Play forward at this rate.
    Play(f64),
    /// Stop, at normal speed.
    Pause,
    /// Step one frame back (-1) or on (+1), paused.
    Step(i32),
}

/// `key` is -1 for J, 0 for K and +1 for L; `rate` is what the preview plays
/// at now. L plays, then climbs the ladder; J climbs down it and pauses at
/// normal speed, or steps a frame back when paused; K pauses, or plays.
fn shuttle(key: i32, playing: bool, rate: f64) -> Shuttle {
    match (key.signum(), playing) {
        (1, true) => Shuttle::Play(
            LADDER
                .iter()
                .copied()
                .find(|&r| r > rate)
                .unwrap_or(LADDER[LADDER.len() - 1]),
        ),
        (-1, true) => LADDER
            .iter()
            .copied()
            .rev()
            .find(|&r| r < rate)
            .map_or(Shuttle::Pause, Shuttle::Play),
        (0, true) => Shuttle::Pause,
        (-1, false) => Shuttle::Step(-1),
        // L or K while paused: play, at normal speed.
        _ => Shuttle::Play(1.0),
    }
}

/// Where a one-frame step from `at` lands: `frame` seconds back or on, never
/// before the start or past `duration`.
fn step_target(at: f64, frame: f64, dir: i32, duration: f64) -> f64 {
    (at + frame * f64::from(dir.signum())).clamp(0.0, duration.max(0.0))
}

/// Wire the frame-step and shuttle keys.
pub(super) fn wire(ui: &AppWindow, st: &VideoState) {
    {
        let ui_weak = ui.as_weak();
        let project_slot = st.project.clone();
        let pending_seek = st.pending_seek.clone();
        ui.on_video_step(move |dir| {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            let slot = project_slot.borrow();
            let Some(p) = slot.as_ref() else {
                return;
            };
            step_frame(&ui, p, &pending_seek, dir);
            sync_shuttle(&ui, p);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let project_slot = st.project.clone();
        let pending_seek = st.pending_seek.clone();
        ui.on_video_shuttle(move |key| {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            let slot = project_slot.borrow();
            let Some(p) = slot.as_ref() else {
                return;
            };
            match shuttle(key, ui.get_video_playing(), p.rate()) {
                Shuttle::Play(rate) => {
                    // A frame step still waiting for the tick lands first, so
                    // the shuttle starts from where the playhead shows.
                    if let Some((secs, _)) = pending_seek.take() {
                        let _ = p.seek_accurate(Duration::from_secs_f32(secs.max(0.0)));
                    }
                    // From the end of the timeline, start again, as Play does.
                    if let (Some(pos), Some(length)) = (p.position(), p.duration()) {
                        if pos + Duration::from_millis(120) >= length {
                            let _ = p.seek(Duration::ZERO);
                        }
                    }
                    if p.set_rate(rate).is_ok() && p.play().is_ok() {
                        ui.set_video_playing(true);
                    }
                }
                Shuttle::Pause => {
                    let _ = p.pause();
                    let _ = p.set_rate(1.0);
                    ui.set_video_playing(false);
                }
                Shuttle::Step(dir) => step_frame(&ui, p, &pending_seek, dir),
            }
            sync_shuttle(&ui, p);
        });
    }
}

/// Pause, and move the playhead one frame back (`dir` < 0) or on. The seek
/// is left for the preview tick, which lands it frame-accurately, as it does
/// the end of a scrub.
fn step_frame(
    ui: &AppWindow,
    p: &kuvatin_video::Project,
    pending_seek: &Cell<Option<(f32, bool)>>,
    dir: i32,
) {
    if ui.get_video_playing() {
        let _ = p.pause();
        ui.set_video_playing(false);
    }
    let length = p.duration().map(|d| d.as_secs_f64()).unwrap_or(0.0);
    let to = step_target(f64::from(ui.get_playhead()), p.frame_secs(), dir, length) as f32;
    pending_seek.set(Some((to, true)));
    ui.set_playhead(to);
    if length > 0.0 {
        ui.set_video_position((f64::from(to) / length).clamp(0.0, 1.0) as f32);
    }
}

/// Show the rate the preview plays at, and mute it while that is not 1×:
/// faster sound only chirps. The transport slider's value is left alone, so
/// the user's level comes back. Does nothing when the readout already
/// matches, so the preview tick calls it every time.
pub(super) fn sync_shuttle(ui: &AppWindow, project: &kuvatin_video::Project) {
    let rate = project.rate() as f32;
    if rate == ui.get_shuttle_rate() {
        return;
    }
    ui.set_shuttle_rate(rate);
    project.set_master_volume(if rate == 1.0 {
        f64::from(ui.get_video_volume())
    } else {
        0.0
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn shuttle_l_plays_then_climbs_to_eight_and_stops() {
        assert_eq!(shuttle(1, false, 1.0), Shuttle::Play(1.0), "paused: play");
        assert_eq!(shuttle(1, true, 1.0), Shuttle::Play(2.0));
        assert_eq!(shuttle(1, true, 2.0), Shuttle::Play(4.0));
        assert_eq!(shuttle(1, true, 4.0), Shuttle::Play(8.0));
        assert_eq!(shuttle(1, true, 8.0), Shuttle::Play(8.0), "8x is the top");
    }

    #[test]
    fn shuttle_j_climbs_down_and_pauses_at_normal_speed() {
        assert_eq!(shuttle(-1, true, 8.0), Shuttle::Play(4.0));
        assert_eq!(shuttle(-1, true, 4.0), Shuttle::Play(2.0));
        assert_eq!(shuttle(-1, true, 2.0), Shuttle::Play(1.0));
        assert_eq!(shuttle(-1, true, 1.0), Shuttle::Pause);
    }

    #[test]
    fn shuttle_j_while_paused_steps_back_a_frame() {
        assert_eq!(shuttle(-1, false, 1.0), Shuttle::Step(-1));
    }

    #[test]
    fn shuttle_k_pauses_from_any_speed_and_plays_when_paused() {
        for rate in [1.0, 2.0, 8.0] {
            assert_eq!(shuttle(0, true, rate), Shuttle::Pause, "{rate}");
        }
        assert_eq!(shuttle(0, false, 4.0), Shuttle::Play(1.0));
    }

    #[test]
    fn shuttle_from_a_rate_off_the_ladder_goes_to_the_next_rung() {
        assert_eq!(shuttle(1, true, 3.0), Shuttle::Play(4.0));
        assert_eq!(shuttle(-1, true, 3.0), Shuttle::Play(2.0));
    }

    #[test]
    fn a_frame_step_stays_inside_the_timeline() {
        assert!(near(step_target(1.0, 0.04, 1, 10.0), 1.04));
        assert!(near(step_target(1.0, 0.04, -1, 10.0), 0.96));
        assert_eq!(
            step_target(0.02, 0.04, -1, 10.0),
            0.0,
            "not before the start"
        );
        assert_eq!(step_target(9.99, 0.04, 1, 10.0), 10.0, "not past the end");
        assert_eq!(
            step_target(3.0, 0.04, 0, 10.0),
            3.0,
            "no direction, no step"
        );
    }
}
