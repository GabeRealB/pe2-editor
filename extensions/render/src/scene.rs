//! A model as a 3D view holds it: its parts posed, its vertices where that puts them, what to draw and what a ray hits. The game's Y points down, and a model shows upright turned half a
//! turn about X, which is where everything here is: `upright` is that turn. A pose is the view's own, to look at a model bent: nothing here changes the asset.

use crate::math::{IDENTITY, M3, V3, add, compose, cross, dot, length, normalized, rotate, scale, sub, transposed};
use crate::model::{Blend, Face, Model, Shading, Source};

/// Bytes of a vertex: position, normal and color, three floats each.
pub const VERTEX_SIZE: u64 = 36;

/// Bytes of a face's vertex: a vertex, and where in the atlas with how much of it, three floats more.
pub const TRIANGLE_SIZE: u64 = 48;

/// Where a material's picture is in the atlas: its corner, and what a texel of it measures there.
pub type Region = [f32; 4];

/// Pictures one above the other: an atlas's pixels, its size, and where each one went.
#[must_use]
pub fn pack(pictures: &[(usize, usize, &[u8])]) -> (usize, usize, Vec<u8>, Vec<Region>) {
    let width = pictures.iter().map(|picture| picture.0).max().unwrap_or(1).max(1);
    let height = pictures.iter().map(|picture| picture.1).sum::<usize>().max(1);
    let mut pixels = vec![0u8; width * height * 4];
    let mut regions = Vec::with_capacity(pictures.len());
    let mut top = 0;
    for &(w, h, rgba) in pictures {
        for row in 0..h {
            let from = row * w * 4;
            let to = (top + row) * width * 4;
            if let Some(line) = rgba.get(from..from + w * 4) {
                pixels[to..to + w * 4].copy_from_slice(line);
            }
        }
        #[expect(clippy::cast_precision_loss, reason = "an atlas is far smaller than floats lose pixels at")]
        regions.push([0.0, top as f32 / height as f32, 1.0 / width as f32, 1.0 / height as f32]);
        top += h;
    }
    (width, height, pixels, regions)
}

/// What a 3D view paints faces by, in place of what they draw.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Paint {
    #[default]
    Off,
    Shading,
    Source,
}

const SMOOTH: [f32; 3] = [0.30, 0.55, 0.95];
const FLAT: [f32; 3] = [0.95, 0.60, 0.20];
const UNLIT: [f32; 3] = [0.82, 0.82, 0.82];
const TEXTURED: [f32; 3] = [0.25, 0.75, 0.65];
const VERTEX: [f32; 3] = [0.85, 0.35, 0.75];
const FLAT_COLOR: [f32; 3] = [0.90, 0.80, 0.25];
const PLAIN: [f32; 3] = [0.55, 0.55, 0.58];

impl Paint {
    /// What a view offers.
    pub const ALL: [(Paint, &'static str); 3] =
        [(Paint::Off, "Textures"), (Paint::Shading, "Shading"), (Paint::Source, "Color source")];

    /// How a view's note keeps it.
    #[must_use]
    pub fn written(self) -> Option<char> {
        match self {
            Paint::Off => None,
            Paint::Shading => Some('s'),
            Paint::Source => Some('c'),
        }
    }

    /// A face's color.
    #[must_use]
    pub fn color(self, face: &Face) -> Option<[f32; 3]> {
        match self {
            Paint::Off => None,
            Paint::Shading => Some(match face.shading {
                Shading::Smooth => SMOOTH,
                Shading::Flat => FLAT,
                Shading::Unlit => UNLIT,
            }),
            Paint::Source => Some(match face.source() {
                Source::Textured => TEXTURED,
                Source::Vertex => VERTEX,
                Source::Flat => FLAT_COLOR,
                Source::Plain => PLAIN,
            }),
        }
    }

    /// What its colors say, and how many of a model's faces each is.
    #[must_use]
    pub fn legend(self, model: &Model) -> Vec<([f32; 3], &'static str, usize)> {
        let by_shading = |shading: Shading| model.faces.iter().filter(|face| face.shading == shading).count();
        let by_source = |source: Source| model.faces.iter().filter(|face| face.source() == source).count();
        let all = match self {
            Paint::Off => Vec::new(),
            Paint::Shading => vec![
                (SMOOTH, "Smooth", by_shading(Shading::Smooth)),
                (FLAT, "Flat", by_shading(Shading::Flat)),
                (UNLIT, "Unlit", by_shading(Shading::Unlit)),
            ],
            Paint::Source => vec![
                (TEXTURED, "Textured", by_source(Source::Textured)),
                (VERTEX, "Vertex colors", by_source(Source::Vertex)),
                (FLAT_COLOR, "Flat color", by_source(Source::Flat)),
                (PLAIN, "Plain", by_source(Source::Plain)),
            ],
        };
        all.into_iter().filter(|(.., count)| *count > 0).collect()
    }
}

/// Half a turn about X.
pub fn upright(v: V3) -> V3 {
    [v[0], -v[1], -v[2]]
}

/// A part's local transform, as the model has it or as it was posed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Local {
    pub rotation: M3,
    pub translation: V3,
}

/// Where a part is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct World {
    pub rotation: M3,
    pub position: V3,
}

/// The bit of a layer's page the game sets: an average or an add comes out added, the other two a quarter added.
#[must_use]
pub fn layer_blend(blend: Blend) -> Blend {
    match blend {
        Blend::Mix | Blend::Add => Blend::Add,
        Blend::Subtract | Blend::AddQuarter => Blend::AddQuarter,
    }
}

/// What `face_bytes` asks of a face that does not change with what is drawn of it.
struct FaceShape<'a> {
    geometry: (&'a [V3], &'a [World]),
    geometric: V3,
    transparent: bool,
    tint: bool,
    /// What it is painted in place of what it draws.
    paint: Option<V3>,
}

/// A face's blended texels: where it is, to be drawn in its turn.
pub struct BlendedFace {
    center: V3,
    order: usize,
    /// The packet added over a face's base.
    layer: bool,
    pub blend: Blend,
    bytes: Vec<u8>,
}

/// A run of faces of one blend, in vertices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Run {
    pub blend: Blend,
    pub first: u32,
    pub count: u32,
}

/// Faces as vertices: what is solid, the opaque faces then the solid texels of the others, each counted, and what
/// blends.
pub struct Triangles {
    pub bytes: Vec<u8>,
    pub vertices: [u32; 2],
    pub blended: Vec<BlendedFace>,
}

/// What blends, in the order the game draws it for an eye looking down `-back`: every face by its own average depth,
/// farthest first, the ordering table linking a later face before one of the same depth, and a layer after its base.
#[must_use]
pub fn in_depth_order(faces: &[BlendedFace], back: V3) -> (Vec<u8>, Vec<Run>) {
    let mut drawn: Vec<(f32, &BlendedFace)> = faces.iter().map(|face| (dot(face.center, back), face)).collect();
    drawn.sort_by(|a, b| a.0.total_cmp(&b.0).then(b.1.order.cmp(&a.1.order)).then(a.1.layer.cmp(&b.1.layer)));
    let (mut bytes, mut runs) = (Vec::new(), Vec::<Run>::new());
    let mut first = 0;
    for (_, face) in drawn {
        let count = u32::try_from(face.bytes.len() as u64 / TRIANGLE_SIZE).unwrap_or(0);
        bytes.extend_from_slice(&face.bytes);
        match runs.last_mut() {
            Some(run) if run.blend == face.blend => run.count += count,
            _ => runs.push(Run { blend: face.blend, first, count }),
        }
        first += count;
    }
    (bytes, runs)
}

pub struct Scene {
    pub model: Model,
    /// By material, where one has a picture.
    pub regions: Vec<Option<Region>>,
    /// By part, whether it shows, where only some do.
    pub only: Option<Vec<bool>>,
    /// By material, the layer a reflective one adds, and its share.
    pub layers: Vec<Option<(Region, f32)>>,
    pub rest: Vec<Local>,
    pub pose: Vec<Local>,
    /// What faces are painted by.
    pub paint: Paint,
    /// Each vertex's part; none for the points no part owns.
    owners: Vec<Option<usize>>,
    pub center: V3,
    pub radius: f32,
}

fn rest(model: &Model) -> Vec<Local> {
    let fixed = |x: i16| f32::from(x) / 4096.0;
    #[expect(clippy::cast_precision_loss, reason = "a model's coordinates are far from where floats lose them")]
    model
        .parts
        .iter()
        .map(|p| Local { rotation: p.rotation.map(|row| row.map(fixed)), translation: p.translation.map(|x| x as f32) })
        .collect()
}

impl Scene {
    #[must_use]
    pub fn new(model: Model) -> Scene {
        let rest = rest(&model);
        let owners = model.owners();
        let mut scene = Scene {
            model,
            regions: Vec::new(),
            layers: Vec::new(),
            only: None,
            pose: rest.clone(),
            rest,
            paint: Paint::Off,
            owners,
            center: [0.0; 3],
            radius: 1.0,
        };
        // A skeleton has no vertices: its joints are what there is to frame.
        let mut positions = scene.positions();
        if positions.is_empty() {
            positions = scene.worlds().iter().map(|world| upright(world.position)).collect();
        }
        // Its bounds' middle, and the sphere around them: a tall model framed by its height, not by where most of its
        // vertices are (a head has more than legs).
        let mut low = [f32::MAX; 3];
        let mut high = [f32::MIN; 3];
        for v in &positions {
            for axis in 0..3 {
                low[axis] = low[axis].min(v[axis]);
                high[axis] = high[axis].max(v[axis]);
            }
        }
        if positions.is_empty() {
            (low, high) = ([0.0; 3], [0.0; 3]);
        }
        let center = scale(add(low, high), 0.5);
        scene.center = center;
        scene.radius = (length(sub(high, low)) / 2.0).max(1.0);
        scene
    }

    /// Each part's place, parents first however they are ordered.
    #[must_use]
    pub fn worlds(&self) -> Vec<World> {
        let mut out: Vec<Option<World>> = vec![None; self.pose.len()];
        for index in 0..self.pose.len() {
            self.world(index, &mut out, 0);
        }
        out.into_iter().map(|world| world.unwrap_or(World { rotation: IDENTITY, position: [0.0; 3] })).collect()
    }

    fn world(&self, index: usize, out: &mut [Option<World>], depth: usize) -> World {
        if let Some(known) = out[index] {
            return known;
        }
        let local = self.pose[index];
        let parent = self.model.parts[index].parent;
        // A part is a root where its parent is itself; a chain deeper than the parts could only be a loop.
        let world = if parent == index || parent >= self.pose.len() || depth > self.pose.len() {
            World { rotation: local.rotation, position: local.translation }
        } else {
            let above = self.world(parent, out, depth + 1);
            World {
                rotation: compose(&above.rotation, &local.rotation),
                position: add(rotate(&above.rotation, local.translation), above.position),
            }
        };
        out[index] = Some(world);
        world
    }

    /// Every vertex, upright, in the order faces name them.
    #[must_use]
    pub fn positions(&self) -> Vec<V3> {
        let worlds = self.worlds();
        let parts = self.model.parts.iter().enumerate().flat_map(|(index, part)| {
            let world = worlds[index];
            part.vertices.iter().map(move |v| upright(add(rotate(&world.rotation, v.map(f32::from)), world.position)))
        });
        parts.chain(self.model.loose_vertices.iter().map(|v| upright(v.map(f32::from)))).collect()
    }

    /// The part a face belongs to: its own, or for one spanning parts its first corner's.
    #[must_use]
    fn shows(&self, face: &Face) -> bool {
        self.only
            .as_ref()
            .is_none_or(|only| self.part_of(face).is_some_and(|part| only.get(part).copied().unwrap_or(false)))
    }

    /// Show a part and what hangs from it, or everything.
    pub fn isolate(&mut self, part: Option<usize>) {
        self.only = part.map(|part| {
            let parts = &self.model.parts;
            let under = |mut at: usize| loop {
                if at == part {
                    return true;
                }
                if parts[at].parent == at || parts[at].parent >= parts.len() {
                    return false;
                }
                at = parts[at].parent;
            };
            (0..parts.len()).map(under).collect()
        });
    }

    pub fn part_of(&self, face: &Face) -> Option<usize> {
        face.part_in(&self.owners)
    }

    fn normal(&self, worlds: &[World], vertex: usize, index: usize) -> Option<V3> {
        let stored = self.model.normals.get(index)?.map(|x| f32::from(x) / 4096.0);
        let rotated = match self.owners.get(vertex).copied().flatten() {
            Some(part) => rotate(&worlds[part].rotation, stored),
            None => stored,
        };
        Some(upright(rotated))
    }

    /// A face as vertices, as `triangles` writes them, its colors scaled by `weight`.
    fn face_bytes(&self, face: &Face, shape: &FaceShape, region: Option<Region>, weight: f32, out: &mut Vec<u8>) {
        let (positions, worlds) = shape.geometry;
        let corners = &face.corners;
        let at = |i: usize| positions.get(corners[i].vertex).copied().unwrap_or_default();
        for i in fan(corners.len()) {
            let corner = &corners[i];
            let stored = match face.shading {
                Shading::Smooth => corner.normal,
                Shading::Flat => face.normal,
                Shading::Unlit => None,
            };
            let normal = stored.and_then(|index| self.normal(worlds, corner.vertex, index)).unwrap_or(shape.geometric);
            let region = region.zip(corner.uv);
            let color = shade(corner.color.or(face.color), shape.transparent, shape.tint);
            // A picture shows as it is, its tint only saying that the part is selected.
            let color = if region.is_some() { if shape.tint { [1.0, 0.72, 0.42] } else { [1.0; 3] } } else { color };
            let color = match shape.paint {
                Some(paint) if shape.tint => add(scale(paint, 0.45), [0.55, 0.28, 0.05]),
                Some(paint) => paint,
                None => color,
            };
            let place = region.map_or([0.0; 3], |(region, uv)| {
                [region[0] + f32::from(uv[0]) * region[2], region[1] + f32::from(uv[1]) * region[3], 1.0]
            });
            for value in at(i).into_iter().chain(normal).chain(color.map(|x| x * weight)).chain(place) {
                out.extend_from_slice(&value.to_le_bytes());
            }
        }
    }

    /// What the faces are, as vertices, the selected part's tinted: as the GPU treats them. A transparent face is
    /// transparent only where its texel's bit is set: the rest of it is as solid as an opaque face, one with no
    /// texture blending whole. A layer is at its base's very depth, and over it.
    #[must_use]
    pub fn triangles(&self, selected: Option<usize>) -> Triangles {
        let (positions, worlds) = (self.positions(), self.worlds());
        let (mut opaque, mut solid_parts, mut blended) = (Vec::new(), Vec::new(), Vec::new());
        for (order, face) in self.model.faces.iter().enumerate() {
            if face.corners.len() < 3 || !self.shows(face) {
                continue;
            }
            let at = |i: usize| positions.get(face.corners[i].vertex).copied().unwrap_or_default();
            let material = self.model.materials.get(face.material);
            let paint = self.paint.color(face);
            // Painted, a face shows what it is: nothing shows through it.
            let transparent = paint.is_none() && material.is_some_and(|material| material.transparent);
            let blend = material.and_then(|material| material.blend).unwrap_or(Blend::Mix);
            let geometric = normalized(cross(sub(at(1), at(0)), sub(at(2), at(0))));
            let tint = self.part_of(face) == selected && selected.is_some();
            let shape = FaceShape { geometry: (&positions, &worlds), geometric, transparent, tint, paint };
            let region = self.regions.get(face.material).copied().flatten().filter(|_| paint.is_none());
            let layer = self.layers.get(face.material).copied().flatten().filter(|_| paint.is_none());
            let weight = layer.map_or(1.0, |(_, layered)| 1.0 - layered);
            let textured = region.is_some() && face.corners.iter().any(|corner| corner.uv.is_some());
            let center = face.corners.iter().enumerate().fold([0.0; 3], |sum, (i, _)| add(sum, at(i)));
            #[expect(clippy::cast_precision_loss, reason = "a face has three corners or four")]
            let center = center.map(|x| x / face.corners.len() as f32);
            let mut part = Vec::new();
            self.face_bytes(face, &shape, region, weight, &mut part);
            if transparent {
                if textured {
                    solid_parts.extend_from_slice(&part);
                }
                blended.push(BlendedFace { center, order, layer: false, blend, bytes: part });
            } else {
                opaque.append(&mut part);
            }
            if let Some((region, layered)) = layer.filter(|_| face.corners.iter().any(|corner| corner.uv.is_some())) {
                let mut part = Vec::new();
                self.face_bytes(face, &shape, Some(region), layered, &mut part);
                solid_parts.extend_from_slice(&part);
                blended.push(BlendedFace { center, order, layer: true, blend: layer_blend(blend), bytes: part });
            }
        }
        let count = |bytes: &[u8]| u32::try_from(bytes.len() as u64 / TRIANGLE_SIZE).unwrap_or(0);
        let vertices = [count(&opaque), count(&solid_parts)];
        opaque.append(&mut solid_parts);
        Triangles { bytes: opaque, vertices, blended }
    }

    /// The faces' edges, as pairs of positions.
    #[must_use]
    pub fn edges(&self) -> Vec<(V3, V3)> {
        let positions = self.positions();
        let mut out = Vec::new();
        for face in self.model.faces.iter().filter(|face| self.shows(face)) {
            for (i, corner) in face.corners.iter().enumerate() {
                let next = &face.corners[(i + 1) % face.corners.len()];
                if let (Some(a), Some(b)) = (positions.get(corner.vertex), positions.get(next.vertex)) {
                    out.push((*a, *b));
                }
            }
        }
        out
    }

    /// The normals faces take light by: where each is, and which way it points, each once.
    #[must_use]
    pub fn normal_lines(&self) -> Vec<(V3, V3)> {
        let (positions, worlds) = (self.positions(), self.worlds());
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for face in self.model.faces.iter().filter(|face| self.shows(face)) {
            for corner in &face.corners {
                let stored = match face.shading {
                    Shading::Smooth => corner.normal,
                    Shading::Flat => face.normal,
                    Shading::Unlit => None,
                };
                let found = stored
                    .filter(|index| seen.insert((corner.vertex, *index)))
                    .and_then(|index| self.normal(&worlds, corner.vertex, index));
                if let (Some(at), Some(normal)) = (positions.get(corner.vertex), found) {
                    out.push((*at, normal));
                }
            }
        }
        out
    }

    /// The bones: each part's place upright, and its parent's where it has one.
    #[must_use]
    pub fn bones(&self) -> Vec<(V3, Option<V3>)> {
        let worlds = self.worlds();
        let at = |index: usize| upright(worlds[index].position);
        let shown = |index: &usize| self.only.as_ref().is_none_or(|only| only.get(*index).copied().unwrap_or(false));
        let links = self.model.parts.iter().enumerate().filter(|(index, _)| shown(index));
        links
            .map(|(index, part)| {
                (at(index), (part.parent != index && part.parent < worlds.len()).then(|| at(part.parent)))
            })
            .collect()
    }

    /// The part a ray hits first.
    #[must_use]
    pub fn pick(&self, origin: V3, direction: V3) -> Option<usize> {
        let positions = self.positions();
        let mut best: Option<(f32, Option<usize>)> = None;
        for face in &self.model.faces {
            let count = face.corners.len();
            for fan_at in 1..count.saturating_sub(1) {
                let corner = |i: usize| positions.get(face.corners[i].vertex).copied();
                let (Some(a), Some(b), Some(c)) = (corner(0), corner(fan_at), corner(fan_at + 1)) else { continue };
                if let Some(along) = hit(origin, direction, [a, b, c])
                    && best.is_none_or(|(nearest, _)| along < nearest)
                {
                    best = Some((along, self.part_of(face)));
                }
            }
        }
        best.and_then(|(_, part)| part)
    }

    /// Turn a part about a unit axis through it, upright, by an angle.
    pub fn turn_part(&mut self, part: usize, axis: V3, angle: f32) {
        let worlds = self.worlds();
        let parent = self.model.parts[part].parent;
        let above = if parent == part || parent >= worlds.len() { IDENTITY } else { worlds[parent].rotation };
        // The half turn is a rotation, so a turn upright is the same angle about the axis in the game's space.
        let turned = crate::math::turn(upright(axis), angle);
        let in_parent = compose(&compose(&transposed(&above), &turned), &above);
        self.pose[part].rotation = compose(&in_parent, &self.pose[part].rotation);
    }

    /// Move a part along a vector, upright.
    pub fn move_part(&mut self, part: usize, by: V3) {
        let worlds = self.worlds();
        let parent = self.model.parts[part].parent;
        let above = if parent == part || parent >= worlds.len() { IDENTITY } else { worlds[parent].rotation };
        self.pose[part].translation = add(self.pose[part].translation, rotate(&transposed(&above), upright(by)));
    }

    pub fn reset_pose(&mut self) {
        self.pose.clone_from(&self.rest);
    }
}

/// The corners of a face as triangles.
fn fan(corners: usize) -> impl Iterator<Item = usize> {
    (1..corners.saturating_sub(1)).flat_map(|i| [0, i, i + 1])
}

/// What a corner is colored: its stored color, 128 being as lit; a textured face, until textures are bound, a clay; else a paler one. What is selected leans to the accent.
fn shade(color: Option<[u8; 3]>, textured: bool, selected: bool) -> V3 {
    let base = match color {
        Some(rgb) => rgb.map(|x| (f32::from(x) / 128.0).min(1.0)),
        None if textured => [0.62, 0.60, 0.58],
        None => [0.74, 0.74, 0.74],
    };
    if selected { add(scale(base, 0.45), [0.55, 0.28, 0.05]) } else { base }
}

/// Where a ray meets a triangle, along it: Möller and Trumbore's.
fn hit(origin: V3, direction: V3, [a, b, c]: [V3; 3]) -> Option<f32> {
    let (edge, other) = (sub(b, a), sub(c, a));
    let across = cross(direction, other);
    let determinant = dot(edge, across);
    if determinant.abs() < 1e-9 {
        return None;
    }
    let inverse = 1.0 / determinant;
    let start = sub(origin, a);
    let first = dot(start, across) * inverse;
    let turned = cross(start, edge);
    let second = dot(direction, turned) * inverse;
    let along = dot(other, turned) * inverse;
    (first >= 0.0 && second >= 0.0 && first + second <= 1.0 && along > 0.0).then_some(along)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Corner, Material, Part};

    fn square() -> Model {
        let corner = |vertex| Corner { vertex, ..Corner::default() };
        let fixed = [[4096, 0, 0], [0, 4096, 0], [0, 0, 4096]];
        Model {
            parts: vec![
                Part {
                    parent: 0,
                    rotation: fixed,
                    translation: [0, 0, 0],
                    vertices: vec![[-10, 0, -10], [10, 0, -10], [10, 0, 10], [-10, 0, 10]],
                },
                Part {
                    parent: 0,
                    rotation: [[0, -4096, 0], [4096, 0, 0], [0, 0, 4096]],
                    translation: [0, -100, 0],
                    vertices: vec![[50, 0, 0]],
                },
            ],
            materials: vec![Material { transparent: false, slot: None, blend: None }],
            faces: vec![Face {
                material: 0,
                shading: Shading::Unlit,
                part: Some(0),
                normal: None,
                color: None,
                corners: vec![corner(0), corner(1), corner(2), corner(3)],
            }],
            ..Model::default()
        }
    }

    #[test]
    fn a_scene_is_framed_by_its_bounds() {
        // The square's corners at y 0 and the second part's vertex at y 50: the middle of the bounds, not of the
        // vertices (four of which are at 0).
        let scene = Scene::new(square());
        assert_eq!(scene.center, [0.0, 25.0, 0.0]);
        let half_diagonal = (20.0_f32 * 20.0 + 50.0 * 50.0 + 20.0 * 20.0).sqrt() / 2.0;
        assert!((scene.radius - half_diagonal).abs() < 1e-4, "{}", scene.radius);
    }

    #[test]
    fn a_part_is_where_its_parent_puts_it_and_shows_upright() {
        let positions = Scene::new(square()).positions();
        // Part 1 turns +X to +Y (down, in the game) and sits 100 above the root: 50 back down, then upright.
        assert_eq!(positions[4], [0.0, 50.0, 0.0]);
    }

    #[test]
    fn a_ray_finds_the_part_it_hits() {
        let scene = Scene::new(square());
        assert_eq!(scene.pick([0.0, 10.0, 0.0], [0.0, -1.0, 0.0]), Some(0));
        assert_eq!(scene.pick([50.0, 10.0, 0.0], [0.0, -1.0, 0.0]), None);
    }

    #[test]
    fn a_normal_shows_where_it_is_used_and_once() {
        let mut model = square();
        model.normals = vec![[0, 4096, 0]];
        model.faces[0].shading = Shading::Smooth;
        for corner in &mut model.faces[0].corners {
            corner.normal = Some(0);
        }
        model.faces.push(model.faces[0].clone());
        let lines = Scene::new(model).normal_lines();
        assert_eq!(lines.len(), 4, "a vertex of each corner, shared by the two faces");
        assert_eq!(lines[0].1, [0.0, -1.0, 0.0], "turned upright");
    }

    #[test]
    fn unlit_faces_have_no_normals_to_show() {
        assert_eq!(Scene::new(square()).normal_lines(), []);
    }

    #[test]
    fn a_legend_says_only_what_the_model_has() {
        let model = square();
        assert_eq!(Paint::Off.legend(&model), []);
        let legend = Paint::Shading.legend(&model);
        assert_eq!(legend.iter().map(|(_, label, count)| (*label, *count)).collect::<Vec<_>>(), [("Unlit", 1)]);
    }

    #[test]
    fn a_painted_face_shows_what_it_is_and_no_texture() {
        let mut scene = Scene::new(square());
        scene.regions = vec![Some([0.0, 0.0, 0.5, 0.5])];
        scene.model.faces[0].corners[0].uv = Some([1, 1]);
        scene.paint = Paint::Source;
        let bytes = scene.triangles(None).bytes;
        let float = |at: usize| f32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        assert!((float(24) - 0.25).abs() < 1e-6, "the color is the source's");
        assert!(float(44).abs() < 1e-6, "and no picture is read");
    }

    #[test]
    fn a_turned_part_takes_what_hangs_from_it_and_a_reset_brings_it_back() {
        let mut scene = Scene::new(square());
        let before = scene.positions();
        scene.turn_part(1, [0.0, 0.0, 1.0], std::f32::consts::FRAC_PI_2);
        assert_ne!(scene.pose, scene.rest);
        let turned = scene.positions();
        assert_ne!(turned[4], before[4]);
        assert_eq!(turned[0], before[0]);
        scene.reset_pose();
        assert_eq!(scene.positions()[4], before[4]);
    }

    #[test]
    fn a_quad_is_two_triangles_of_six_corners() {
        assert_eq!(Scene::new(square()).triangles(None).bytes.len() as u64, 6 * TRIANGLE_SIZE);
    }

    /// Three squares, one behind the other along Z.
    fn stacked(blends: [Blend; 3]) -> Scene {
        let mut scene = Scene::new(square());
        let face = scene.model.faces[0].clone();
        let model = &mut scene.model;
        model.faces = vec![face.clone(), Face { material: 1, ..face.clone() }, Face { material: 2, ..face }];
        model.materials =
            blends.iter().map(|blend| Material { transparent: true, slot: None, blend: Some(*blend) }).collect();
        scene
    }

    fn blended(blend: Blend, at: f32, order: usize, layer: bool) -> BlendedFace {
        BlendedFace { center: [0.0, 0.0, at], order, layer, blend, bytes: vec![0; 144] }
    }

    #[test]
    fn what_blends_is_drawn_from_the_back_and_not_by_blend() {
        let faces = [
            blended(Blend::Add, 5.0, 0, false),
            blended(Blend::Mix, 1.0, 1, false),
            blended(Blend::Add, 3.0, 2, false),
        ];
        let (_, runs) = in_depth_order(&faces, [0.0, 0.0, 1.0]);
        let blends: Vec<_> = runs.iter().map(|run| (run.blend, run.first)).collect();
        assert_eq!(blends, [(Blend::Mix, 0), (Blend::Add, 3)]);
        let (_, turned) = in_depth_order(&faces, [0.0, 0.0, -1.0]);
        assert_eq!(turned.iter().map(|run| run.blend).collect::<Vec<_>>(), [Blend::Add, Blend::Mix]);
    }

    #[test]
    fn of_faces_at_one_depth_the_later_is_drawn_first_and_a_layer_after_its_base() {
        let faces = [
            blended(Blend::Mix, 1.0, 0, false),
            blended(Blend::Subtract, 1.0, 1, false),
            blended(Blend::Add, 1.0, 1, true),
        ];
        let (_, runs) = in_depth_order(&faces, [0.0, 0.0, 1.0]);
        assert_eq!(runs.iter().map(|run| run.blend).collect::<Vec<_>>(), [Blend::Subtract, Blend::Add, Blend::Mix]);
    }

    #[test]
    fn a_layer_is_added_by_the_bit_the_game_sets() {
        assert_eq!(
            [Blend::Mix, Blend::Add, Blend::Subtract, Blend::AddQuarter].map(layer_blend),
            [Blend::Add, Blend::Add, Blend::AddQuarter, Blend::AddQuarter]
        );
    }

    #[test]
    fn a_face_with_no_picture_blends_whole() {
        let triangles = stacked([Blend::Add, Blend::Mix, Blend::Subtract]).triangles(None);
        assert_eq!(triangles.vertices, [0, 0]);
        assert_eq!(triangles.blended.len(), 3);
    }

    #[test]
    fn a_textured_face_has_a_solid_part_and_a_blended_one() {
        let mut scene = stacked([Blend::Add; 3]);
        for corner in scene.model.faces.iter_mut().flat_map(|face| &mut face.corners) {
            corner.uv = Some([0, 0]);
        }
        scene.regions = vec![Some([0.0; 4]); 3];
        let triangles = scene.triangles(None);
        assert_eq!(triangles.vertices, [0, 18]);
        assert_eq!(triangles.blended.len(), 3);
    }

    #[test]
    fn a_layer_and_its_base_share_the_light_between_them() {
        let mut scene = Scene::new(square());
        for corner in &mut scene.model.faces[0].corners {
            corner.uv = Some([0, 0]);
        }
        scene.regions = vec![Some([0.0; 4])];
        scene.layers = vec![Some(([0.0; 4], 0.25))];
        let triangles = scene.triangles(None);
        assert_eq!(triangles.vertices, [6, 6]);
        let color =
            |vertex: usize| f32::from_le_bytes(triangles.bytes[vertex * 48 + 24..vertex * 48 + 28].try_into().unwrap());
        assert_eq!((color(0), color(6)), (0.75, 0.25));
    }

    #[test]
    fn isolating_a_part_keeps_it_and_what_hangs_from_it() {
        let mut scene = Scene::new(square());
        let part =
            |parent, vertices| Part { parent, rotation: scene.model.parts[0].rotation, translation: [0; 3], vertices };
        let parts = vec![part(0, Vec::new()), part(0, Vec::new()), part(1, Vec::new())];
        scene.model.parts = parts;
        scene.isolate(Some(1));
        assert_eq!(scene.only, Some(vec![false, true, true]));
        scene.isolate(None);
        assert_eq!(scene.only, None);
    }
}
