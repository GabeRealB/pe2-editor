//! A mesh as an asset holds it: geometry and the names of its material slots, and nothing of where it ends up. What a
//! package stores puts a VRAM page and palette in every face and an address on the whole; those are a material's and a
//! package's. As USD, so that anything that reads USD reads it: one mesh bound rigidly to a skeleton, each part a joint
//! and each vertex following one. Exact values sit beside what is shown: `texel` UVs, `normalIndex`, part-local
//! `sourceNormals`, `shading`, `part`, `colorSource`. How a face draws is its material's: here a material only names a slot.
//!
//! The file carries the skeleton it was bound to, so that it opens alone. A mesh on a skeleton asset takes its pose from
//! that (`pose`): these bind transforms then only say how its vertices are found again.

use std::fmt::{Display, Write};

use pe2_ext::{Result, anyhow, bail};

use crate::model::{Corner, Face, Material, Model, Part, Shading};
use crate::skeleton::{self, Joint};
use crate::usda::{self, Prim};

pub const FILE: &str = "mesh.usda";

/// What a missing index is written as, and a texel with no UV.
const NONE: i64 = -1;
/// A rotation's and a normal's one.
const ONE: f64 = 4096.0;
/// What a UV is shown over, with no texture to say: its bytes' range.
const UV_RANGE: f32 = 256.0;

type Rows = [[f64; 3]; 3];

/// Where a part is in the bind pose.
#[derive(Clone, Copy)]
struct Bind {
    rotation: Rows,
    at: [f64; 3],
}

fn times(rows: &Rows, v: [f64; 3]) -> [f64; 3] {
    rows.map(|row| row[0] * v[0] + row[1] * v[1] + row[2] * v[2])
}

fn composed(a: &Rows, b: &Rows) -> Rows {
    std::array::from_fn(|row| std::array::from_fn(|column| (0..3).map(|k| a[row][k] * b[k][column]).sum()))
}

/// A rotation's inverse: stored in fixed point, they are not quite orthonormal, so the transpose will not do.
fn inverse(m: &Rows) -> Option<Rows> {
    let cofactor = |row: usize, column: usize| {
        let (r1, r2, c1, c2) = ((row + 1) % 3, (row + 2) % 3, (column + 1) % 3, (column + 2) % 3);
        m[r1][c1] * m[r2][c2] - m[r1][c2] * m[r2][c1]
    };
    let determinant: f64 = (0..3).map(|column| m[0][column] * cofactor(0, column)).sum();
    (determinant.abs() > 1e-12).then(|| std::array::from_fn(|row| std::array::from_fn(|column| cofactor(column, row) / determinant)))
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

impl Bind {
    fn of(part: &Part, above: Option<&Bind>) -> Bind {
        let rotation = part.rotation.map(|row| row.map(|x| f64::from(x) / ONE));
        let at = part.translation.map(f64::from);
        match above {
            Some(above) => {
                let moved = times(&above.rotation, at);
                Bind { rotation: composed(&above.rotation, &rotation), at: [moved[0] + above.at[0], moved[1] + above.at[1], moved[2] + above.at[2]] }
            }
            None => Bind { rotation, at },
        }
    }

    fn apply(&self, v: [f64; 3]) -> [f64; 3] {
        let turned = times(&self.rotation, v);
        [turned[0] + self.at[0], turned[1] + self.at[1], turned[2] + self.at[2]]
    }

    /// USD's row-vector matrix.
    fn matrix(&self) -> [[f64; 4]; 4] {
        let r = &self.rotation;
        [[r[0][0], r[1][0], r[2][0], 0.0], [r[0][1], r[1][1], r[2][1], 0.0], [r[0][2], r[1][2], r[2][2], 0.0], [self.at[0], self.at[1], self.at[2], 1.0]]
    }

    fn from_matrix(m: &[f64; 16]) -> Bind {
        Bind { rotation: std::array::from_fn(|row| std::array::from_fn(|column| m[column * 4 + row])), at: [m[12], m[13], m[14]] }
    }
}

/// Each part's place, parents first however they are ordered; a chain as long as the parts can only be a loop.
fn binds(parts: &[Part]) -> Vec<Bind> {
    fn bind(parts: &[Part], at: usize, out: &mut Vec<Option<Bind>>, depth: usize) -> Bind {
        if let Some(known) = out[at] {
            return known;
        }
        let parent = parts[at].parent;
        let above = (parent != at && parent < parts.len() && depth < parts.len()).then(|| bind(parts, parent, out, depth + 1));
        let found = Bind::of(&parts[at], above.as_ref());
        out[at] = Some(found);
        found
    }
    let mut out = vec![None; parts.len()];
    (0..parts.len()).map(|at| bind(parts, at, &mut out, 0)).collect()
}

fn joint_paths(parts: &[Part], names: Option<&[String]>) -> Vec<String> {
    let own = |at: usize| names.and_then(|names| names.get(at).cloned()).unwrap_or_else(|| format!("part{at}"));
    (0..parts.len())
        .map(|at| {
            let mut path = own(at);
            let (mut here, mut depth) = (at, 0);
            while parts[here].parent != here && parts[here].parent < parts.len() && depth < parts.len() {
                here = parts[here].parent;
                depth += 1;
                path = format!("{}/{path}", own(here));
            }
            path
        })
        .collect()
}

fn join<T: Display>(items: impl IntoIterator<Item = T>) -> String {
    let mut out = String::new();
    for (at, item) in items.into_iter().enumerate() {
        let _ = write!(out, "{}{item}", if at == 0 { "" } else { ", " });
    }
    out
}

fn tuple<T: Display>(items: impl IntoIterator<Item = T>) -> String {
    format!("({})", join(items))
}

fn quoted(items: impl IntoIterator<Item = impl Display>) -> String {
    join(items.into_iter().map(|item| format!("\"{item}\"")))
}

#[expect(clippy::cast_possible_truncation, reason = "stored as USD's floats, as what reads the file expects")]
fn narrow(v: [f64; 3]) -> [f32; 3] {
    v.map(|x| x as f32)
}

/// A model's corners, as a mesh's arrays run: one entry each, face after face.
#[derive(Default)]
struct Corners {
    counts: Vec<usize>,
    indices: Vec<usize>,
    normal_index: Vec<i64>,
    normals: Vec<[f32; 3]>,
    colors: Vec<[f32; 3]>,
    texels: Vec<[i64; 2]>,
    st: Vec<[f32; 2]>,
    shading: Vec<&'static str>,
    part: Vec<i64>,
    source: Vec<&'static str>,
}

fn shading_name(shading: Shading) -> &'static str {
    match shading {
        Shading::Smooth => "smooth",
        Shading::Flat => "flat",
        Shading::Unlit => "unlit",
    }
}

fn number_of(index: Option<usize>) -> i64 {
    index.and_then(|index| i64::try_from(index).ok()).unwrap_or(NONE)
}

fn face_normal(points: &[[f64; 3]], face: &Face) -> [f32; 3] {
    let at = |corner: usize| face.corners.get(corner).and_then(|corner| points.get(corner.vertex)).copied().unwrap_or_default();
    let (a, b) = (sub(at(1), at(0)), sub(at(2), at(0)));
    let cross = [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    let length = cross.iter().map(|x| x * x).sum::<f64>().sqrt();
    if length > 0.0 { narrow(cross.map(|x| x / length)) } else { [0.0, 1.0, 0.0] }
}

fn gather(model: &Model, points: &[[f64; 3]], joint: &[usize], worlds: &[Bind]) -> Corners {
    let mut out = Corners::default();
    for face in &model.faces {
        out.counts.push(face.corners.len());
        out.shading.push(shading_name(face.shading));
        out.part.push(number_of(face.part));
        let by_corner = face.corners.iter().any(|corner| corner.color.is_some());
        out.source.push(if by_corner { "corner" } else if face.color.is_some() { "face" } else { "none" });
        let plain = face_normal(points, face);
        for corner in &face.corners {
            out.indices.push(corner.vertex);
            let index = corner.normal.or(face.normal);
            out.normal_index.push(number_of(index));
            let stored = index.and_then(|index| model.normals.get(index));
            let turned = stored.zip(joint.get(corner.vertex)).map(|(normal, part)| narrow(times(&worlds[*part].rotation, normal.map(|x| f64::from(x) / ONE))));
            out.normals.push(turned.unwrap_or(plain));
            let color = corner.color.or(face.color).unwrap_or([255; 3]);
            out.colors.push(color.map(|x| f32::from(x) / 255.0));
            out.texels.push(corner.uv.map_or([NONE; 2], |uv| uv.map(i64::from)));
            let [u, v] = corner.uv.unwrap_or_default().map(f32::from);
            out.st.push([(u + 0.5) / UV_RANGE, 1.0 - (v + 0.5) / UV_RANGE]);
        }
    }
    out
}

fn face_varying(out: &mut String, kind: &str, name: &str, items: impl IntoIterator<Item = impl Display>) {
    let _ = writeln!(out, "            {kind}[] primvars:{name} = [{}] (\n                interpolation = \"faceVarying\"\n            )", join(items));
}

fn uniform(out: &mut String, kind: &str, name: &str, items: &str) {
    let _ = writeln!(out, "            {kind}[] primvars:{name} = [{items}] (\n                interpolation = \"uniform\"\n            )");
}

fn skeleton_prim(out: &mut String, model: &Model, worlds: &[Bind], names: Option<&[String]>) {
    let matrices = |binds: &[Bind]| join(binds.iter().map(|bind| tuple(bind.matrix().map(tuple))));
    let rests: Vec<Bind> = model.parts.iter().map(|part| Bind::of(part, None)).collect();
    let _ = writeln!(out, "        def Skeleton \"Skeleton\"\n        {{");
    let _ = writeln!(out, "            uniform token[] joints = [{}]", quoted(joint_paths(&model.parts, names)));
    let _ = writeln!(out, "            uniform matrix4d[] bindTransforms = [{}]", matrices(worlds));
    let _ = writeln!(out, "            uniform matrix4d[] restTransforms = [{}]\n        }}\n", matrices(&rests));
}

fn subset_prims(out: &mut String, model: &Model, slots: &[String]) {
    for at in 0..slots.len() {
        let faces = model.faces.iter().enumerate().filter(|(_, face)| face.material == at).map(|(index, _)| index);
        let _ = writeln!(out, "\n            def GeomSubset \"material{at}\" (\n                prepend apiSchemas = [\"MaterialBindingAPI\"]\n            )\n            {{");
        let _ = writeln!(out, "                uniform token elementType = \"face\"\n                uniform token familyName = \"materialBind\"");
        let _ = writeln!(out, "                int[] indices = [{}]", join(faces));
        let _ = writeln!(out, "                rel material:binding = </Model/Materials/material{at}>\n            }}");
    }
}

fn mesh_prim(out: &mut String, model: &Model, slots: &[String], worlds: &[Bind]) {
    let mut points: Vec<[f64; 3]> = Vec::new();
    let mut joint: Vec<usize> = Vec::new();
    for (at, part) in model.parts.iter().enumerate() {
        points.extend(part.vertices.iter().map(|v| worlds[at].apply(v.map(f64::from))));
        joint.resize(points.len(), at);
    }
    let owned = points.len();
    points.extend(model.loose_vertices.iter().map(|v| v.map(f64::from)));
    joint.resize(points.len(), 0);
    let corners = gather(model, &points, &joint, worlds);
    let _ = writeln!(out, "        def Mesh \"Mesh\" (\n            prepend apiSchemas = [\"SkelBindingAPI\", \"MaterialBindingAPI\"]\n        )\n        {{");
    let _ = writeln!(out, "            uniform bool doubleSided = 1\n            uniform token orientation = \"leftHanded\"");
    let _ = writeln!(out, "            point3f[] points = [{}]", join(points.iter().map(|p| tuple(narrow(*p)))));
    let _ = writeln!(out, "            int[] faceVertexCounts = [{}]", join(&corners.counts));
    let _ = writeln!(out, "            int[] faceVertexIndices = [{}]", join(&corners.indices));
    let _ = writeln!(out, "            custom float3[] sourceNormals = [{}]", join(model.normals.iter().map(|n| tuple(n.map(|x| f32::from(x) / 4096.0)))));
    face_varying(out, "normal3f", "normals", corners.normals.iter().map(|n| tuple(*n)));
    face_varying(out, "int", "normalIndex", &corners.normal_index);
    face_varying(out, "color3f", "displayColor", corners.colors.iter().map(|c| tuple(*c)));
    face_varying(out, "int2", "texel", corners.texels.iter().map(|t| tuple(*t)));
    face_varying(out, "texCoord2f", "st", corners.st.iter().map(|st| tuple(*st)));
    uniform(out, "token", "shading", &quoted(&corners.shading));
    uniform(out, "int", "part", &join(&corners.part));
    uniform(out, "token", "colorSource", &quoted(&corners.source));
    let _ = writeln!(out, "            int[] primvars:skel:jointIndices = [{}] (\n                elementSize = 1\n                interpolation = \"vertex\"\n            )", join(&joint));
    let weights = (0..joint.len()).map(|at| u8::from(at < owned));
    let _ = writeln!(out, "            float[] primvars:skel:jointWeights = [{}] (\n                elementSize = 1\n                interpolation = \"vertex\"\n            )", join(weights));
    let _ = writeln!(out, "            matrix4d primvars:skel:geomBindTransform = ((1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 0, 0, 1))");
    let _ = writeln!(out, "            rel skel:skeleton = </Model/Rig/Skeleton>");
    subset_prims(out, model, slots);
    let _ = writeln!(out, "        }}");
}

fn material_prims(out: &mut String, slots: &[String]) {
    let _ = writeln!(out, "\n    def \"Materials\"\n    {{");
    for (at, slot) in slots.iter().enumerate() {
        let _ = writeln!(out, "        def Material \"material{at}\"\n        {{");
        let _ = writeln!(out, "            token inputs:slot = \"{slot}\"");
        let _ = writeln!(out, "            token outputs:surface.connect = </Model/Materials/material{at}/Surface.outputs:surface>\n");
        let _ = writeln!(out, "            def Shader \"Surface\"\n            {{\n                uniform token info:id = \"UsdPreviewSurface\"");
        let _ = writeln!(out, "                float inputs:roughness = 1\n                token outputs:surface\n            }}\n        }}");
    }
    let _ = writeln!(out, "    }}");
}

/// The file of a mesh; `slots` names what its faces' materials are filled by, and `joints` its parts, on a skeleton.
#[must_use]
pub fn write(model: &Model, slots: &[String], joints: Option<&[String]>) -> String {
    let worlds = binds(&model.parts);
    let mut out = String::from("#usda 1.0\n(\n    defaultPrim = \"Model\"\n    metersPerUnit = 0.001\n    upAxis = \"Y\"\n)\n\n");
    // The game's Y points down: it shows upright turned half about X, with its numbers left as they are.
    let _ = writeln!(out, "def Xform \"Model\"\n{{\n    custom token[] slots = [{}]", quoted(slots));
    let _ = writeln!(out, "    float xformOp:rotateX = 180\n    uniform token[] xformOpOrder = [\"xformOp:rotateX\"]\n");
    let _ = writeln!(out, "    def SkelRoot \"Rig\" (\n        prepend apiSchemas = [\"SkelBindingAPI\"]\n    )\n    {{\n        rel skel:skeleton = </Model/Rig/Skeleton>\n");
    skeleton_prim(&mut out, model, &worlds, joints);
    mesh_prim(&mut out, model, slots, &worlds);
    let _ = writeln!(out, "    }}");
    material_prims(&mut out, slots);
    let _ = writeln!(out, "}}");
    out
}

#[expect(clippy::cast_possible_truncation, reason = "what is rounded is checked against the range by the caller's conversion")]
fn whole(x: f64) -> i64 {
    x.round() as i64
}

fn shorts(values: &[f64]) -> Result<Vec<i16>> {
    values.iter().map(|x| i16::try_from(whole(*x)).map_err(|_| anyhow!("{x} is out of range"))).collect()
}

fn triples(values: &[f64]) -> Vec<[f64; 3]> {
    values.as_chunks::<3>().0.to_vec()
}

fn indices(values: &[f64]) -> Vec<Option<usize>> {
    values.iter().map(|x| usize::try_from(whole(*x)).ok()).collect()
}

/// The parts a skeleton's bind transforms give: each one's rest transform is its place relative to its parent's.
fn read_parts(skeleton: &Prim) -> Result<Vec<Part>> {
    let paths = skeleton.attribute("joints")?.words();
    let matrices = skeleton.numbers("bindTransforms")?;
    if paths.is_empty() || matrices.len() != paths.len() * 16 {
        bail!("the skeleton needs its joints and a bind transform for each");
    }
    let worlds: Vec<Bind> = matrices.as_chunks::<16>().0.iter().map(Bind::from_matrix).collect();
    paths
        .iter()
        .enumerate()
        .map(|(at, path)| {
            let parent = path.rsplit_once('/').map(|(above, _)| paths.iter().position(|other| other == above).ok_or_else(|| anyhow!("{path} has no parent")));
            let parent = parent.transpose()?.unwrap_or(at);
            let (rotation, translation) = if parent == at {
                (worlds[at].rotation, worlds[at].at)
            } else {
                let back = inverse(&worlds[parent].rotation).ok_or_else(|| anyhow!("{path}'s parent has no inverse"))?;
                (composed(&back, &worlds[at].rotation), times(&back, sub(worlds[at].at, worlds[parent].at)))
            };
            let fixed = |x: f64| i16::try_from(whole(x * ONE)).map_err(|_| anyhow!("{path}: its rotation is out of range"));
            let wide = |x: f64| i32::try_from(whole(x)).map_err(|_| anyhow!("{path}: its translation is out of range"));
            let mut rows = [[0; 3]; 3];
            for (cell, value) in rows.iter_mut().flatten().zip(rotation.iter().flatten()) {
                *cell = fixed(*value)?;
            }
            Ok(Part { parent, rotation: rows, translation: [wide(translation[0])?, wide(translation[1])?, wide(translation[2])?], vertices: Vec::new() })
        })
        .collect()
}

/// Each point taken back through its part's bind, to the numbers it was stored as.
fn read_vertices(mesh: &Prim, parts: &mut [Part]) -> Result<Vec<[i16; 3]>> {
    let points = triples(&mesh.numbers("points")?);
    let joints = indices(&mesh.numbers("primvars:skel:jointIndices")?);
    let weights = mesh.numbers("primvars:skel:jointWeights")?;
    if joints.len() != points.len() || weights.len() != points.len() {
        bail!("{} points, {} joint indices, {} weights", points.len(), joints.len(), weights.len());
    }
    // A point no joint moves belongs to no part: those come last, and nothing has to say how many.
    let owned = weights.iter().take_while(|weight| **weight > 0.0).count();
    if weights[owned..].iter().any(|weight| *weight > 0.0) {
        bail!("points with no joint have to come after those with one");
    }
    let worlds = binds(parts);
    let back: Vec<Option<Rows>> = worlds.iter().map(|world| inverse(&world.rotation)).collect();
    let mut last = 0;
    for (point, joint) in points[..owned].iter().zip(&joints) {
        let joint = joint.filter(|joint| *joint < parts.len() && *joint >= last).ok_or_else(|| anyhow!("points are not grouped by part, in part order"))?;
        last = joint;
        let rows = back[joint].as_ref().ok_or_else(|| anyhow!("a part with vertices has no inverse"))?;
        let local = shorts(&times(rows, sub(*point, worlds[joint].at)))?;
        parts[joint].vertices.push([local[0], local[1], local[2]]);
    }
    points[owned..].iter().map(|point| shorts(point).map(|p| [p[0], p[1], p[2]])).collect()
}

/// Each face's material, from the subsets binding them.
fn read_materials(mesh: &Prim, faces: usize) -> Result<Vec<usize>> {
    let mut materials = vec![None; faces];
    for subset in mesh.children.iter().filter(|child| child.kind == "GeomSubset") {
        let at: usize = subset.name.strip_prefix("material").and_then(|at| at.parse().ok()).ok_or_else(|| anyhow!("a subset is called {}", subset.name))?;
        for face in indices(&subset.numbers("indices")?) {
            *face.and_then(|face| materials.get_mut(face)).ok_or_else(|| anyhow!("{} names no face", subset.name))? = Some(at);
        }
    }
    materials.into_iter().map(|material| material.ok_or_else(|| anyhow!("a face has no material"))).collect()
}

fn byte(x: f64) -> Option<u8> {
    u8::try_from(whole(x * 255.0)).ok()
}

fn read_faces(mesh: &Prim) -> Result<Vec<Face>> {
    let counts = indices(&mesh.numbers("faceVertexCounts")?);
    let vertices = indices(&mesh.numbers("faceVertexIndices")?);
    let normals = indices(&mesh.numbers("primvars:normalIndex")?);
    let colors = triples(&mesh.numbers("primvars:displayColor")?);
    let texels = mesh.numbers("primvars:texel")?;
    let shading = mesh.attribute("primvars:shading")?.words();
    let parts = indices(&mesh.numbers("primvars:part")?);
    let source = mesh.attribute("primvars:colorSource")?.words();
    let materials = read_materials(mesh, counts.len())?;
    if [shading.len(), parts.len(), source.len()].iter().any(|len| *len != counts.len()) {
        bail!("the faces' arrays differ in length");
    }
    if [normals.len(), colors.len(), texels.len() / 2].iter().any(|len| *len != vertices.len()) {
        bail!("the corners' arrays differ in length");
    }
    let rgb = |at: usize| colors[at].iter().map(|x| byte(*x)).collect::<Option<Vec<u8>>>().map(|c| [c[0], c[1], c[2]]);
    let mut first = 0;
    let mut faces = Vec::with_capacity(counts.len());
    for (at, count) in counts.iter().enumerate() {
        let count = count.ok_or_else(|| anyhow!("a face has no corners"))?;
        let span = first..first + count;
        let shading = match shading[at].as_str() {
            "smooth" => Shading::Smooth,
            "flat" => Shading::Flat,
            "unlit" => Shading::Unlit,
            other => bail!("a face's shading is {other}"),
        };
        let by_corner = source[at] == "corner";
        let corner = |k: usize| -> Result<Corner> {
            let texel = [whole(texels[k * 2]), whole(texels[k * 2 + 1])];
            Ok(Corner {
                vertex: vertices[k].ok_or_else(|| anyhow!("a corner has no vertex"))?,
                normal: normals[k].filter(|_| shading == Shading::Smooth),
                color: if by_corner { rgb(k) } else { None },
                uv: u8::try_from(texel[0]).ok().zip(u8::try_from(texel[1]).ok()).map(|(u, v)| [u, v]),
            })
        };
        faces.push(Face {
            material: materials[at],
            shading,
            part: parts[at],
            normal: if shading == Shading::Flat { *normals.get(first).ok_or_else(|| anyhow!("a face has no corners"))? } else { None },
            color: if source[at] == "face" { rgb(first) } else { None },
            corners: span.map(corner).collect::<Result<_>>()?,
        });
        first += count;
    }
    Ok(faces)
}

/// Give a mesh its skeleton's hierarchy and pose.
///
/// # Errors
/// They do not have the same parts.
pub fn pose(model: &mut Model, joints: &[Joint]) -> Result<()> {
    if model.parts.len() != joints.len() {
        return Err(anyhow!("{} parts on {} joints", model.parts.len(), joints.len()));
    }
    for (at, (part, joint)) in model.parts.iter_mut().zip(joints).enumerate() {
        part.parent = joint.parent.unwrap_or(at);
        part.rotation = skeleton::matrix(joint.rotation);
        part.translation = joint.translation;
    }
    Ok(())
}

/// Whether a stored model rests as its skeleton says: what lets the mesh leave that out.
#[must_use]
pub fn rests_on(model: &Model, joints: &[Joint]) -> bool {
    let mut expected = model.clone();
    pose(&mut expected, joints).is_ok() && expected.parts.iter().zip(&model.parts).all(|(want, got)| want.parent == got.parent && want.rotation == got.rotation && want.translation == got.translation)
}

/// A mesh, and its slots' names.
///
/// # Errors
/// It is not what `write` makes, however it was saved since.
pub fn read(text: &str) -> Result<(Model, Vec<String>)> {
    let root = usda::read(text)?;
    let held = root.at(&["Model"]).ok_or_else(|| anyhow!("no /Model"))?;
    let skeleton = held.at(&["Rig", "Skeleton"]).ok_or_else(|| anyhow!("no /Model/Rig/Skeleton"))?;
    let mesh = held.at(&["Rig", "Mesh"]).ok_or_else(|| anyhow!("no /Model/Rig/Mesh"))?;
    if !matches!(mesh.attribute("orientation")?, usda::Value::Text(orientation) if orientation == "leftHanded") {
        bail!("its faces have to wind as the game's do (orientation leftHanded), in their stored corner order");
    }
    let slots = held.attribute("slots")?.words();
    let mut parts = read_parts(skeleton)?;
    let loose_vertices = read_vertices(mesh, &mut parts)?;
    let rounded = mesh.numbers("sourceNormals")?;
    let normals = triples(&rounded).iter().map(|n| shorts(&n.map(|x| x * ONE)).map(|n| [n[0], n[1], n[2]])).collect::<Result<_>>()?;
    let faces = read_faces(mesh)?;
    if let Some(past) = faces.iter().map(|face| face.material).find(|material| *material >= slots.len()) {
        bail!("a face has material {past} of {}", slots.len());
    }
    let materials = (0..slots.len()).map(|slot| Material { transparent: false, slot: Some(slot), blend: None }).collect();
    Ok((Model { parts, loose_vertices, normals, materials, faces, ..Model::default() }, slots))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Model {
        let corner = |vertex, uv| Corner { vertex, normal: Some(vertex), color: None, uv };
        let flat = |vertex| Corner { vertex, normal: None, color: Some([1, 128, 255]), uv: None };
        let root = Part { parent: 0, rotation: [[4096, 0, 0], [0, 4096, 0], [0, 0, 4096]], translation: [1, -2, 300_000], vertices: vec![[1, 2, 3], [-4, 5, 6]] };
        // Not quite orthonormal, as they are stored.
        let child = Part { parent: 0, rotation: [[0, -4095, 3], [4096, 1, 0], [-2, 0, 4094]], translation: [0, -927, 3], vertices: vec![[7, 8, 9]] };
        let textured = Face { material: 1, shading: Shading::Smooth, part: Some(0), normal: None, color: None, corners: vec![corner(0, Some([0, 255])), corner(1, Some([12, 34])), corner(2, Some([56, 78]))] };
        let seam = Face { material: 0, shading: Shading::Flat, part: None, normal: Some(1), color: None, corners: vec![flat(2), flat(1), flat(0)] };
        let unlit = Face { material: 0, shading: Shading::Unlit, part: Some(1), normal: None, color: Some([10, 20, 30]), corners: vec![Corner { vertex: 1, ..Corner::default() }; 3] };
        Model { parts: vec![root, child], loose_vertices: vec![[-300, 40, 5]], normals: vec![[4096, 0, 0], [0, 0, -4096], [1, 2, 3]], faces: vec![textured, seam, unlit], ..Model::default() }
    }

    #[test]
    fn a_mesh_comes_back_as_it_was_written() {
        let slots = vec!["page_1".to_string(), "page_0".to_string()];
        let (back, named) = read(&write(&sample(), &slots, None)).unwrap();
        assert_eq!(named, slots);
        let geometry = |model: &Model| format!("{:?} {:?} {:?} {:?}", model.parts, model.loose_vertices, model.normals, model.faces);
        assert_eq!(geometry(&back), geometry(&sample()));
        assert_eq!(back.materials.len(), 2);
    }

    #[test]
    fn a_mesh_on_a_skeleton_takes_its_pose_from_it() {
        let joints = vec![
            Joint { name: "root".to_string(), parent: None, translation: [1, -2, 300_000], rotation: [0.0; 3] },
            Joint { name: "arm".to_string(), parent: Some(0), translation: [5, 6, 7], rotation: [0.0; 3] },
        ];
        let names = ["root".to_string(), "arm".to_string()];
        let text = write(&sample(), &["a".to_string(), "b".to_string()], Some(&names));
        assert!(text.contains("\"root/arm\""), "joints are named, by their paths");
        let (mut back, _) = read(&text).unwrap();
        pose(&mut back, &joints).unwrap();
        assert_eq!(back.parts[1].parent, 0);
        assert_eq!(back.parts[1].translation, [5, 6, 7]);
        assert_eq!(back.parts[1].vertices, sample().parts[1].vertices, "found by the file's bind, not by the pose it now takes");
        assert!(rests_on(&back, &joints));
        assert!(!rests_on(&sample(), &joints), "its second part rests elsewhere");
        assert!(pose(&mut back, &joints[..1]).is_err());
    }

    #[test]
    fn a_file_says_what_is_wrong_with_it() {
        let text = write(&sample(), &["a".to_string(), "b".to_string()], None);
        assert!(read(&text.replace("leftHanded", "rightHanded")).is_err(), "corner order would change meaning");
        assert!(read(&text.replace("faceVertexCounts", "faceCounts")).is_err());
        assert!(read("#usda 1.0\n").is_err());
        let weights = "float[] primvars:skel:jointWeights = [1, 1, 1, 0]";
        assert!(text.contains(weights), "the point no joint moves");
        assert!(read(&text.replace(weights, "float[] primvars:skel:jointWeights = [1, 0, 1, 0]")).is_err(), "out of order");
    }

    #[test]
    fn what_no_part_owns_needs_no_count() {
        let text = write(&sample(), &["a".to_string(), "b".to_string()], None);
        assert!(!text.contains("oose"));
        assert_eq!(read(&text).unwrap().0.loose_vertices, sample().loose_vertices);
    }

    #[test]
    fn nothing_of_the_console_is_written() {
        let text = write(&sample(), &["only".to_string(), "other".to_string()], None);
        for word in ["page =", "palette", "address", "symbol", "tpage", "clut", "effect", "transparent", "blend"] {
            assert!(!text.contains(word), "{word} is in a mesh");
        }
    }
}
