//! A clip played in the 3D view: where it is, how it moves on, what it does to the joints it moves, and the bar along
//! the bottom of the view it is scrubbed on. Native, so that it is tested where the view, the component's, is not.

use crate::clip::{Clip, TURN, sample};
use crate::gizmo::{ACCENT, Overlay};
use crate::scene::Local;

/// The game's normal rate: frames of a clip each second.
pub const FPS: f32 = 30.0;

/// Where a clip is as it plays.
#[derive(Clone, Debug, PartialEq)]
pub struct Playback {
    /// In frames, between keys while it plays.
    pub frame: f32,
    pub playing: bool,
    /// Whether it starts again at its end, rather than stopping there.
    pub looped: bool,
    /// Its length in frames.
    pub length: u32,
}

impl Playback {
    #[must_use]
    pub fn new(clip: &Clip) -> Playback {
        Playback { frame: 0.0, playing: true, looped: true, length: clip.length() }
    }

    /// On by `seconds`, where it plays: round to its start again, or held at its end.
    pub fn advance(&mut self, seconds: f32) {
        if !self.playing {
            return;
        }
        #[expect(clippy::cast_precision_loss, reason = "a clip's frames")]
        let length = self.length as f32;
        self.frame += seconds * FPS;
        if self.frame <= length {
            return;
        }
        if self.looped && length > 0.0 {
            self.frame %= length;
        } else {
            self.frame = length;
            self.playing = false;
        }
    }

    /// To a frame of its own, stopped there.
    pub fn seek(&mut self, frame: f32) {
        #[expect(clippy::cast_precision_loss, reason = "a clip's frames")]
        let length = self.length as f32;
        self.frame = frame.clamp(0.0, length);
        self.playing = false;
    }

    /// A whole frame on or back.
    pub fn step(&mut self, by: i32) {
        #[expect(clippy::cast_precision_loss, reason = "a step of frames")]
        let to = self.frame.round() + by as f32;
        self.seek(to);
    }

    /// Play from where it is, from the start where it had stopped at its end.
    pub fn toggle(&mut self) {
        #[expect(clippy::cast_precision_loss, reason = "a clip's frames")]
        if !self.playing && self.frame >= self.length as f32 {
            self.frame = 0.0;
        }
        self.playing = !self.playing;
    }
}

/// A rotation's rows, as the game composes one: M = Rx Ry Rz, angles in its units.
#[must_use]
pub fn rows(angles: [f64; 3]) -> [[f32; 3]; 3] {
    let [(sx, cx), (sy, cy), (sz, cz)] = angles.map(|a| (a * std::f64::consts::TAU / TURN).sin_cos());
    let rows = [
        [cy * cz, -cy * sz, sy],
        [sx * sy * cz + cx * sz, -sx * sy * sz + cx * cz, -sx * cy],
        [-cx * sy * cz + sx * sz, cx * sy * sz + sx * cz, cx * cy],
    ];
    #[expect(clippy::cast_possible_truncation, reason = "a rotation's entries, as the view draws them")]
    rows.map(|row| row.map(|x| x as f32))
}

/// The pose a clip puts its joints in at a frame: each track's joint (`parts`, by track) its rotation, and its
/// translation where the track has one, its rest translation (`rest`, by part) where it has none.
#[must_use]
pub fn pose(clip: &Clip, parts: &[Option<usize>], rest: &[Local], frame: f32) -> Vec<(usize, Local)> {
    clip.tracks
        .iter()
        .zip(parts)
        .filter_map(|(track, part)| {
            let part = (*part)?;
            let (rotation, translation) = sample(track, f64::from(frame));
            #[expect(clippy::cast_possible_truncation, reason = "a translation, as the view draws it")]
            let translation = translation.map(|t| t.map(|x| x as f32)).or_else(|| Some(rest.get(part)?.translation))?;
            Some((part, Local { rotation: rows(rotation), translation }))
        })
        .collect()
}

/// The bar a clip is scrubbed on, along the bottom of a view of a size, in pixels.
pub struct Bar {
    left: f32,
    right: f32,
    y: f32,
}

const MARGIN: f32 = 16.0;
const HEIGHT: f32 = 28.0;
/// How far from its line a press still takes it.
const GRAB: f32 = 12.0;

impl Bar {
    #[must_use]
    pub fn new(size: (f32, f32)) -> Bar {
        Bar { left: MARGIN, right: (size.0 - MARGIN).max(MARGIN + 1.0), y: size.1 - HEIGHT }
    }

    fn x_of(&self, frame: f32, length: u32) -> f32 {
        #[expect(clippy::cast_precision_loss, reason = "a clip's frames")]
        let length = (length as f32).max(1.0);
        self.left + (self.right - self.left) * (frame / length).clamp(0.0, 1.0)
    }

    /// Whether a point is on it.
    #[must_use]
    pub fn holds(&self, at: (f32, f32)) -> bool {
        (self.left - GRAB..=self.right + GRAB).contains(&at.0) && (at.1 - self.y).abs() <= GRAB
    }

    /// The frame a point along it is at.
    #[must_use]
    pub fn frame_at(&self, x: f32, length: u32) -> f32 {
        #[expect(clippy::cast_precision_loss, reason = "a clip's frames")]
        let length = length as f32;
        ((x - self.left) / (self.right - self.left)).clamp(0.0, 1.0) * length
    }

    /// The bar, what has played of it, a tick at each key's frame (`keys`), and where it is.
    pub fn draw(&self, overlay: &mut Overlay, playback: &Playback, keys: &[u32], hovered: bool) {
        let (left, right, y) = (self.left, self.right, self.y);
        let at = self.x_of(playback.frame, playback.length);
        overlay.line((left, y), (right, y), 4.0, [0.5, 0.52, 0.56, 0.55]);
        overlay.line((left, y), (at, y), 4.0, [ACCENT[0], ACCENT[1], ACCENT[2], 0.9]);
        for key in keys {
            #[expect(clippy::cast_precision_loss, reason = "a clip's frames")]
            let x = self.x_of(*key as f32, playback.length);
            overlay.line((x, y - 5.0), (x, y + 5.0), 1.0, [0.85, 0.87, 0.9, 0.6]);
        }
        overlay.disc((at, y), if hovered { 7.0 } else { 5.5 }, [1.0, 1.0, 1.0, 0.95]);
    }
}

/// The frames a clip has a key at, any track's, each once: where something it says starts to hold.
#[must_use]
pub fn key_frames(clip: &Clip) -> Vec<u32> {
    let mut frames: Vec<u32> = clip
        .tracks
        .iter()
        .flat_map(|track| {
            track.keys.iter().scan(0u32, |at, key| {
                *at += u32::from(key.frames);
                Some(*at)
            })
        })
        .collect();
    frames.sort_unstable();
    frames.dedup();
    frames
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clip::{Key, Track};

    fn clip() -> Clip {
        let key = |rotation: [i32; 3], frames: u8| Key { rotation, frames, ..Key::default() };
        Clip {
            tracks: vec![
                Track {
                    keys: vec![
                        Key { translation: Some([0, -900, 0]), ..key([0, 0, 0], 0) },
                        Key { translation: Some([0, -800, 0]), ..key([1024, 0, 0], 10) },
                    ],
                    looped: None,
                },
                Track { keys: vec![key([0, 0, 0], 4), key([0, 512, 0], 6)], looped: None },
            ],
        }
    }

    #[test]
    fn it_plays_round_and_stops_at_its_end_unlooped() {
        let mut playback = Playback::new(&clip());
        assert_eq!(playback.length, 10);
        playback.advance(0.2);
        assert!((playback.frame - 6.0).abs() < 1e-4);
        playback.advance(0.2);
        assert!((playback.frame - 2.0).abs() < 1e-4);
        playback.looped = false;
        playback.advance(1.0);
        assert!((playback.frame - 10.0).abs() < 1e-4 && !playback.playing);
        playback.toggle();
        assert!(playback.playing && playback.frame == 0.0);
        playback.step(3);
        assert!(!playback.playing && (playback.frame - 3.0).abs() < 1e-4);
    }

    #[test]
    fn a_pose_turns_its_joints_and_keeps_their_rest_where_nothing_moves_them() {
        let rest = vec![
            Local { rotation: rows([0.0; 3]), translation: [0.0, -927.0, 0.0] },
            Local { rotation: rows([0.0; 3]), translation: [5.0, 6.0, 7.0] },
        ];
        let posed = pose(&clip(), &[Some(0), Some(1)], &rest, 10.0);
        assert_eq!(posed[0].1.translation, [0.0, -800.0, 0.0]);
        // A quarter turn about X: Y goes to Z.
        assert!((posed[0].1.rotation[2][1] - 1.0).abs() < 1e-6);
        assert_eq!(posed[1].1.translation, [5.0, 6.0, 7.0]);
        assert!(pose(&clip(), &[None, Some(1)], &rest, 0.0).iter().all(|(part, _)| *part == 1));
    }

    #[test]
    fn the_bar_takes_a_frame_from_where_it_is_pressed() {
        let bar = Bar::new((232.0, 100.0));
        assert!(bar.holds((116.0, 72.0)) && !bar.holds((116.0, 40.0)));
        assert!((bar.frame_at(116.0, 30) - 15.0).abs() < 1e-4);
        assert_eq!(bar.frame_at(-50.0, 30), 0.0);
        assert_eq!(key_frames(&clip()), vec![0, 4, 10]);
    }
}
