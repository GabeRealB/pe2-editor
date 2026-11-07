//! A clip in USD's text form: a `SkelAnimation` whose per-frame joint samples are there to look at - another tool plays
//! them on the skeleton the file carries - and the exact keys beside them, which are what is read back:
//!
//! ```text
//! keyCounts int[]         keys per track
//! keyRotations int3[]     each key's Euler angles, 4096 units per turn (tracks in order, keys in order)
//! keyTranslations int3[]  each key's translation, (0, 0, 0) on a track without one
//! keyFrames int[]         frames to blend into each key
//! keyCues int[]           each key's cue bits
//! keyVariants int[]       each key's pose variant (`clip`)
//! translated bool[]       per track: stored with translation
//! loops int[]             per track: the key a finished track jumps back to, -1 when it ends
//! ```
//!
//! Samples: 30 a second, as `clip::sample` blends them, turned into quaternions in the game's order (M = Rx Ry Rz); a
//! track without translation keeps its joint's rest translation. Joints are named by their skeleton's paths. The
//! skeleton itself is written under `/Preview`, playing `/Clip`, so the file opens as the skeleton moving.

use std::f64::consts::TAU;
use std::fmt::Write as _;

use pe2_ext::{Result, anyhow, bail};

use crate::clip::{Clip, Key, TURN, Track, sample};
use crate::skeleton::Joint;
use crate::usda::{self, Prim, Value};

/// The name a clip's file has in its bundle.
pub const FILE: &str = "clip.usda";
pub const FPS: u32 = 30;

type Rows = [[f64; 3]; 3];
/// A transform as USD writes one: rows multiplying a row vector, translation last.
type Matrix = [[f64; 4]; 4];

fn product(a: &Rows, b: &Rows) -> Rows {
    [0, 1, 2].map(|r| [0, 1, 2].map(|c| (0..3).map(|k| a[r][k] * b[k][c]).sum()))
}

/// A rotation's rows, M = Rx Ry Rz, angles in radians.
fn rows(angles: [f64; 3]) -> Rows {
    let [(sx, cx), (sy, cy), (sz, cz)] = angles.map(f64::sin_cos);
    let x = [[1.0, 0.0, 0.0], [0.0, cx, -sx], [0.0, sx, cx]];
    let y = [[cy, 0.0, sy], [0.0, 1.0, 0.0], [-sy, 0.0, cy]];
    let z = [[cz, -sz, 0.0], [sz, cz, 0.0], [0.0, 0.0, 1.0]];
    product(&product(&x, &y), &z)
}

/// A quaternion (w, x, y, z) of M = Rx Ry Rz: qx qy qz.
fn quaternion(angles: [f64; 3]) -> [f64; 4] {
    let half = angles.map(|a| a / 2.0);
    let axis = |at: usize| {
        let mut q = [half[at].cos(), 0.0, 0.0, 0.0];
        q[at + 1] = half[at].sin();
        q
    };
    let times = |a: [f64; 4], b: [f64; 4]| {
        [
            a[0] * b[0] - a[1] * b[1] - a[2] * b[2] - a[3] * b[3],
            a[0] * b[1] + a[1] * b[0] + a[2] * b[3] - a[3] * b[2],
            a[0] * b[2] - a[1] * b[3] + a[2] * b[0] + a[3] * b[1],
            a[0] * b[3] + a[1] * b[2] - a[2] * b[1] + a[3] * b[0],
        ]
    };
    times(times(axis(0), axis(1)), axis(2))
}

/// Angles in the game's units, as radians.
#[must_use]
pub fn radians(units: [f64; 3]) -> [f64; 3] {
    units.map(|a| a * TAU / TURN)
}

fn join<T: std::fmt::Display>(items: impl IntoIterator<Item = T>) -> String {
    items.into_iter().map(|item| item.to_string()).collect::<Vec<_>>().join(", ")
}

fn tuple<T: std::fmt::Display>(items: impl IntoIterator<Item = T>) -> String {
    format!("({})", join(items))
}

fn joint_paths(joints: &[Joint]) -> Vec<String> {
    (0..joints.len())
        .map(|at| {
            let mut path = joints[at].name.clone();
            let mut here = at;
            while let Some(parent) = joints[here].parent.filter(|parent| *parent < here) {
                path = format!("{}/{path}", joints[parent].name);
                here = parent;
            }
            path
        })
        .collect()
}

/// A joint's place under its parent, and over the whole: USD's rows, translation last.
fn transforms(joints: &[Joint]) -> (Vec<Matrix>, Vec<Matrix>) {
    let matrix = |rotation: Rows, translation: [f64; 3]| {
        // USD's rows multiply a row vector: the transpose of the game's columns.
        let mut m: Matrix = [[0.0; 4]; 4];
        for r in 0..3 {
            for c in 0..3 {
                m[r][c] = rotation[c][r];
            }
        }
        m[3] = [translation[0], translation[1], translation[2], 1.0];
        m
    };
    let mut worlds: Vec<(Rows, [f64; 3])> = Vec::with_capacity(joints.len());
    let mut rests = Vec::with_capacity(joints.len());
    for joint in joints {
        let rotation = rows(joint.rotation.map(f64::to_radians));
        let translation = joint.translation.map(f64::from);
        rests.push(matrix(rotation, translation));
        let world = match joint.parent.and_then(|parent| worlds.get(parent)) {
            Some((above, at)) => {
                let moved = [0, 1, 2].map(|r| at[r] + (0..3).map(|k| above[r][k] * translation[k]).sum::<f64>());
                (product(above, &rotation), moved)
            }
            None => (rotation, translation),
        };
        worlds.push(world);
    }
    (worlds.iter().map(|(rotation, at)| matrix(*rotation, *at)).collect(), rests)
}

fn rounded(x: f64) -> String {
    let x = (x * 1e6).round() / 1e6;
    if x == 0.0 { "0".to_string() } else { x.to_string() }
}

/// The file of a clip moving the joints of `joints`, one track each.
///
/// # Errors
/// It has a track for each joint, or the file says nothing true.
pub fn write(clip: &Clip, joints: &[Joint]) -> Result<String> {
    if clip.tracks.len() != joints.len() {
        bail!("{} tracks for a skeleton of {} joints", clip.tracks.len(), joints.len());
    }
    let length = clip.length();
    let mut out = String::from("#usda 1.0\n(\n    defaultPrim = \"Clip\"\n");
    let _ = writeln!(out, "    endTimeCode = {length}\n    framesPerSecond = {FPS}\n    metersPerUnit = 0.001");
    let _ = writeln!(out, "    startTimeCode = 0\n    timeCodesPerSecond = {FPS}\n    upAxis = \"Y\"\n)\n");
    let paths = joint_paths(joints);
    let quoted = join(paths.iter().map(|path| format!("\"{path}\"")));
    let _ = writeln!(out, "def SkelAnimation \"Clip\"\n{{\n    uniform token[] joints = [{quoted}]");
    let _ = writeln!(out, "    quatf[] rotations.timeSamples = {{");
    for frame in 0..=length {
        let samples =
            clip.tracks.iter().map(|track| tuple(quaternion(radians(sample(track, f64::from(frame)).0)).map(rounded)));
        let _ = writeln!(out, "        {frame}: [{}],", join(samples));
    }
    let _ = writeln!(out, "    }}\n    half3[] scales = [{}]", join(joints.iter().map(|_| "(1, 1, 1)")));
    let _ = writeln!(out, "    float3[] translations.timeSamples = {{");
    for frame in 0..=length {
        let samples = clip.tracks.iter().zip(joints).map(|(track, joint)| {
            let at = sample(track, f64::from(frame)).1.unwrap_or(joint.translation.map(f64::from));
            tuple(at.map(rounded))
        });
        let _ = writeln!(out, "        {frame}: [{}],", join(samples));
    }
    let _ = writeln!(out, "    }}");
    let keys: Vec<&Key> = clip.tracks.iter().flat_map(|track| &track.keys).collect();
    let triples = |values: Vec<[i32; 3]>| join(values.into_iter().map(tuple));
    let _ = writeln!(out, "    custom int[] keyCounts = [{}]", join(clip.tracks.iter().map(|track| track.keys.len())));
    let _ =
        writeln!(out, "    custom int3[] keyRotations = [{}]", triples(keys.iter().map(|key| key.rotation).collect()));
    let translations = keys.iter().map(|key| key.translation.unwrap_or_default()).collect();
    let _ = writeln!(out, "    custom int3[] keyTranslations = [{}]", triples(translations));
    let _ = writeln!(out, "    custom int[] keyFrames = [{}]", join(keys.iter().map(|key| key.frames)));
    let _ = writeln!(out, "    custom int[] keyCues = [{}]", join(keys.iter().map(|key| key.cue)));
    let _ = writeln!(out, "    custom int[] keyVariants = [{}]", join(keys.iter().map(|key| key.variant)));
    let _ = writeln!(out, "    custom bool[] translated = [{}]", join(clip.tracks.iter().map(Track::translated)));
    let loops = clip.tracks.iter().map(|track| track.looped.map_or(-1, |at| i64::try_from(at).unwrap_or(-1)));
    let _ = writeln!(out, "    custom int[] loops = [{}]\n}}\n", join(loops));
    // The skeleton it moves, so that it opens as that skeleton moving: upright, as a mesh's file shows its model.
    let (worlds, rests) = transforms(joints);
    let matrices = |all: &[Matrix]| join(all.iter().map(|m| tuple(m.map(|row| tuple(row.map(rounded))))));
    let _ = writeln!(out, "def SkelRoot \"Preview\" (\n    prepend apiSchemas = [\"SkelBindingAPI\"]\n)\n{{");
    let _ =
        writeln!(out, "    float xformOp:rotateX = 180\n    uniform token[] xformOpOrder = [\"xformOp:rotateX\"]\n");
    let _ = writeln!(
        out,
        "    def Skeleton \"Skeleton\" (\n        prepend apiSchemas = [\"SkelBindingAPI\"]\n    )\n    {{"
    );
    let _ = writeln!(out, "        uniform token[] joints = [{quoted}]");
    let _ = writeln!(out, "        uniform matrix4d[] bindTransforms = [{}]", matrices(&worlds));
    let _ = writeln!(out, "        uniform matrix4d[] restTransforms = [{}]", matrices(&rests));
    let _ = writeln!(out, "        rel skel:animationSource = </Clip>\n    }}\n}}");
    Ok(out)
}

fn integers(prim: &Prim, name: &str) -> Result<Vec<i64>> {
    prim.numbers(name)?
        .into_iter()
        .map(|x| {
            #[expect(clippy::cast_possible_truncation, reason = "checked whole")]
            let whole = x as i64;
            if x.fract() == 0.0 { Ok(whole) } else { Err(anyhow!("{name}: {x} is not a whole number")) }
        })
        .collect()
}

fn flags(prim: &Prim, name: &str) -> Result<Vec<bool>> {
    match prim.attribute(name)? {
        Value::List(items) => items
            .iter()
            .map(|item| match item {
                Value::Text(word) if word == "true" || word == "1" => Ok(true),
                Value::Text(word) if word == "false" || word == "0" => Ok(false),
                Value::Number(x) => Ok(*x != 0.0),
                other => Err(anyhow!("{name}: {other:?} is no flag")),
            })
            .collect(),
        other => bail!("{name}: {other:?} is no list"),
    }
}

/// The clip a file holds.
///
/// # Errors
/// It is not one: no `SkelAnimation` with the exact keys, or arrays that disagree.
pub fn read(text: &str) -> Result<Clip> {
    let root = usda::read(text)?;
    let prim = root
        .children
        .iter()
        .find(|prim| prim.kind == "SkelAnimation")
        .ok_or_else(|| anyhow!("no SkelAnimation: it is not a clip"))?;
    let counts = integers(prim, "keyCounts")?;
    let rotations = integers(prim, "keyRotations")?;
    let translations = integers(prim, "keyTranslations")?;
    let frames = integers(prim, "keyFrames")?;
    let cues = integers(prim, "keyCues")?;
    let variants = integers(prim, "keyVariants")?;
    let translated = flags(prim, "translated")?;
    let loops = integers(prim, "loops")?;
    let keys = frames.len();
    if translated.len() != counts.len() || loops.len() != counts.len() {
        bail!("its per-track arrays disagree in length");
    }
    if rotations.len() != keys * 3 || translations.len() != keys * 3 || cues.len() != keys || variants.len() != keys {
        bail!("its per-key arrays disagree in length");
    }
    let total: i64 = counts.iter().sum();
    if usize::try_from(total).ok() != Some(keys) {
        bail!("keyCounts cover {total} keys, the key arrays hold {keys}");
    }
    let triple = |values: &[i64], at: usize| -> Result<[i32; 3]> {
        Ok([i32::try_from(values[at * 3])?, i32::try_from(values[at * 3 + 1])?, i32::try_from(values[at * 3 + 2])?])
    };
    let mut tracks = Vec::with_capacity(counts.len());
    let mut at = 0;
    for ((count, translated), looped) in counts.iter().zip(&translated).zip(&loops) {
        let mut track = Track { keys: Vec::new(), looped: usize::try_from(*looped).ok() };
        for _ in 0..*count {
            track.keys.push(Key {
                rotation: triple(&rotations, at)?,
                translation: if *translated { Some(triple(&translations, at)?) } else { None },
                frames: u8::try_from(frames[at]).map_err(|_| anyhow!("key {at}: {} frames", frames[at]))?,
                cue: u8::try_from(cues[at]).map_err(|_| anyhow!("key {at}: cue {}", cues[at]))?,
                variant: u32::try_from(variants[at]).map_err(|_| anyhow!("key {at}: variant {}", variants[at]))?,
            });
            at += 1;
        }
        tracks.push(track);
    }
    Ok(Clip { tracks })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn joints() -> Vec<Joint> {
        vec![
            Joint { name: "root".into(), parent: None, translation: [0, 0, 0], rotation: [0.0; 3] },
            Joint { name: "arm".into(), parent: Some(0), translation: [10, -20, 0], rotation: [0.0, 90.0, 0.0] },
        ]
    }

    fn clip() -> Clip {
        Clip {
            tracks: vec![
                Track {
                    keys: vec![
                        Key { rotation: [0, 0, 0], translation: Some([0, -928, 0]), frames: 1, ..Key::default() },
                        Key {
                            rotation: [16, -8, 4088],
                            translation: Some([5, -900, 2]),
                            frames: 12,
                            cue: 2,
                            variant: 0,
                        },
                    ],
                    looped: Some(1),
                },
                Track {
                    keys: vec![Key { rotation: [1024, 0, 0], frames: 6, variant: 1, ..Key::default() }],
                    looped: None,
                },
            ],
        }
    }

    #[test]
    fn a_clip_reads_back_as_written() {
        let text = write(&clip(), &joints()).unwrap();
        assert_eq!(read(&text).unwrap(), clip());
        assert!(text.contains("\"root/arm\""));
        assert!(text.contains("13: ["));
    }

    #[test]
    fn its_tracks_are_its_skeletons_joints() {
        assert!(write(&clip(), &joints()[..1]).is_err());
    }

    #[test]
    fn a_quarter_turn_about_x_is_a_quaternion_of_its_half() {
        let q = quaternion(radians([1024.0, 0.0, 0.0]));
        let half = std::f64::consts::FRAC_1_SQRT_2;
        assert!((q[0] - half).abs() < 1e-9 && (q[1] - half).abs() < 1e-9 && q[2] == 0.0 && q[3] == 0.0);
    }
}
