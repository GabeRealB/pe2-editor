//! A model as the game stores it: the arrays a `TmdSource` record points at and the record, read into what it authors - parts (bones and their vertices), normals, materials and faces
//! described by how they draw. What is specific to the console, the opcodes, their runs and the seam pass that transforms faces spanning parts, is read through and left behind.
//!
//! The way into the game's data is `decode`, from a model's bytes and the address of its first byte; `encode` is the way back. Nothing here trusts them: what does not read is an error, never a panic.

use std::collections::HashMap;

use pe2_ext::{Result, anyhow, bail};

const MODIFIERS: u32 = 0x4000 | 0x8000 | 0x10000 | 0x20000;
const GROUP_END: u32 = 0xFFFF_FFFE;
const STREAM_END: u32 = 0xFFFF_FFFF;
const RECORD_SIZE: usize = 0x24;
const BONE_SIZE: usize = 36;

/// How a face takes light.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Shading {
    Unlit,
    Flat,
    Smooth,
}

/// Where a face's color is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Colored {
    Not,
    Face,
    Corner,
}

/// What an opcode draws.
#[derive(Clone, Copy)]
struct Kind {
    corners: usize,
    normals: usize,
    colors: usize,
    textured: bool,
    /// Its vertices come from what the seam pass transformed.
    seam: bool,
    shading: Shading,
    colored: Colored,
    semi: bool,
}

/// What each base opcode draws: what the engine's stream handlers do with it.
fn kind(base: u32) -> Option<Kind> {
    use Colored::{Corner, Face, Not};
    use Shading::{Flat, Smooth, Unlit};
    let k = |corners, normals, colors, textured, seam, shading, colored, semi| Some(Kind { corners, normals, colors, textured, seam, shading, colored, semi });
    match base {
        0x04 => k(3, 0, 1, false, false, Unlit, Face, false),
        0x44 => k(4, 0, 1, false, false, Unlit, Face, false),
        0x20 => k(3, 3, 1, false, false, Smooth, Face, false),
        0x22 => k(3, 3, 1, false, false, Smooth, Face, true),
        0x40 => k(4, 1, 1, false, false, Flat, Face, false),
        0x60 => k(4, 4, 1, false, false, Smooth, Face, false),
        0x62 => k(4, 4, 1, false, false, Smooth, Face, true),
        0x18 => k(3, 1, 0, true, false, Flat, Not, false),
        0x1A => k(3, 1, 0, true, false, Flat, Not, true),
        0x1C => k(3, 0, 0, true, false, Unlit, Not, false),
        0x1E => k(3, 0, 0, true, false, Unlit, Not, true),
        0x38 => k(3, 3, 0, true, false, Smooth, Not, false),
        0x3A => k(3, 3, 0, true, false, Smooth, Not, true),
        0x30 => k(3, 3, 1, true, false, Smooth, Face, false),
        0x58 => k(4, 1, 0, true, false, Flat, Not, false),
        0x5A => k(4, 1, 0, true, false, Flat, Not, true),
        0x5C => k(4, 0, 0, true, false, Unlit, Not, false),
        0x5E => k(4, 0, 0, true, false, Unlit, Not, true),
        0x78 => k(4, 4, 0, true, false, Smooth, Not, false),
        0x7A => k(4, 4, 0, true, false, Smooth, Not, true),
        0x70 => k(4, 4, 1, true, false, Smooth, Face, false),
        0x156 => k(4, 0, 4, true, false, Unlit, Corner, true),
        0x21 => k(3, 0, 0, false, true, Smooth, Corner, false),
        0x31 => k(3, 0, 0, true, true, Smooth, Face, false),
        0x39 => k(3, 0, 0, true, true, Smooth, Not, false),
        0x3B => k(3, 0, 0, true, true, Smooth, Not, true),
        0x79 => k(4, 0, 0, true, true, Smooth, Not, false),
        0x7B => k(4, 0, 0, true, true, Smooth, Not, true),
        _ => None,
    }
}

/// A transform pass's element: whether it carries a color before its destinations.
fn pass_color(base: u32) -> Option<bool> {
    match base {
        0xC8 => Some(false),
        0xC0 => Some(true),
        _ => None,
    }
}

/// The bytes of the primitive a seam face writes, and from one corner to the next: by corners, then textured.
fn packet(corners: usize, textured: bool) -> (usize, usize) {
    match (corners, textured) {
        (3, false) => (28, 8),
        (3, true) => (40, 12),
        (_, false) => (36, 8),
        (_, true) => (52, 12),
    }
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Part {
    pub parent: usize,
    /// Fixed point: 4096 is one.
    pub rotation: [[i16; 3]; 3],
    pub translation: [i32; 3],
    /// Local to the part.
    pub vertices: Vec<[i16; 3]>,
}

/// How a transparent face mixes with what is behind it: the GPU's four.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Blend {
    Mix,
    Add,
    Subtract,
    AddQuarter,
}

/// How a run of faces draws.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Material {
    pub transparent: bool,
    /// The texture slot, for one that is textured.
    pub slot: Option<usize>,
    pub blend: Option<Blend>,
}

/// Where a texture slot's faces look: its depth, page and palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Placement {
    pub bits: u8,
    pub page: [u16; 2],
    pub palette: [u16; 2],
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Corner {
    /// Among all vertices, part by part.
    pub vertex: usize,
    pub normal: Option<usize>,
    pub color: Option<[u8; 3]>,
    pub uv: Option<[u8; 2]>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Face {
    pub material: usize,
    pub shading: Shading,
    /// None: a seam face, spanning parts.
    pub part: Option<usize>,
    pub normal: Option<usize>,
    pub color: Option<[u8; 3]>,
    /// In rim order.
    pub corners: Vec<Corner>,
}

/// Where a face's color comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Textured,
    /// One at each corner.
    Vertex,
    Flat,
    Plain,
}

impl Face {
    /// The part it belongs to, by `Model::owners`: its own, or for one spanning parts its first corner's.
    #[must_use]
    pub fn part_in(&self, owners: &[Option<usize>]) -> Option<usize> {
        self.part.or_else(|| self.corners.first().and_then(|corner| owners.get(corner.vertex).copied().flatten()))
    }

    #[must_use]
    pub fn source(&self) -> Source {
        if self.corners.iter().any(|corner| corner.uv.is_some()) {
            Source::Textured
        } else if self.corners.iter().any(|corner| corner.color.is_some()) {
            Source::Vertex
        } else if self.color.is_some() {
            Source::Flat
        } else {
            Source::Plain
        }
    }
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Model {
    pub parts: Vec<Part>,
    /// Past what the parts count. One model of 790 has any: three zeros no face uses, in its vertex array as stored.
    pub loose_vertices: Vec<[i16; 3]>,
    pub normals: Vec<[i16; 3]>,
    pub slots: Vec<Placement>,
    pub materials: Vec<Material>,
    pub faces: Vec<Face>,
    pub effect: u32,
    pub reflection: bool,
}

impl Model {
    /// Each vertex's part; none for the points no part owns.
    #[must_use]
    pub fn owners(&self) -> Vec<Option<usize>> {
        let mut owners = Vec::new();
        for (index, part) in self.parts.iter().enumerate() {
            owners.resize(owners.len() + part.vertices.len(), Some(index));
        }
        owners.resize(owners.len() + self.loose_vertices.len(), None);
        owners
    }
}

/// What the bytes are read through: every read in bounds.
struct Bytes<'a> {
    data: &'a [u8],
    load: u32,
}

impl Bytes<'_> {
    fn u32(&self, at: usize) -> Result<u32> {
        let bytes = self.data.get(at..at + 4).ok_or_else(|| anyhow!("past the end, at {at:#X}"))?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn i16(&self, at: usize) -> Result<i16> {
        let bytes = self.data.get(at..at + 2).ok_or_else(|| anyhow!("past the end, at {at:#X}"))?;
        Ok(i16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn i32(&self, at: usize) -> Result<i32> {
        self.u32(at).map(u32::cast_signed)
    }

    fn offset(&self, address: u32) -> Result<usize> {
        address.checked_sub(self.load).map(|at| at as usize).ok_or_else(|| anyhow!("{address:#X} is before the model"))
    }

    fn triple(&self, at: usize) -> Result<[i16; 3]> {
        Ok([self.i16(at)?, self.i16(at + 2)?, self.i16(at + 4)?])
    }
}

/// One command of the stream.
struct Command {
    op: u32,
    elements: Vec<Vec<u32>>,
}

/// One element of a drawing command, read as what its opcode says.
#[derive(Default)]
struct Element {
    vertices: Vec<usize>,
    normals: Vec<usize>,
    cache: Vec<usize>,
    colors: Vec<u32>,
    uv: Vec<u32>,
    clut: u32,
    tpage: u32,
}

fn halves(words: &[u32]) -> Vec<u32> {
    words.iter().flat_map(|word| [word & 0xFFFF, word >> 16]).collect()
}

fn element(kind: Kind, words: &[u32]) -> Result<Element> {
    let references = kind.corners + if kind.seam { 0 } else { kind.normals };
    let reference_words = references.div_ceil(2);
    let short = || anyhow!("an element is too short");
    let half = halves(words.get(..reference_words).ok_or_else(short)?);
    let mut out = Element::default();
    if kind.seam {
        out.cache = half.iter().take(references).map(|h| (h / 4) as usize).collect();
    } else {
        out.vertices = half[..kind.corners].iter().map(|h| (h / 8) as usize).collect();
        out.normals = half[kind.corners..references].iter().map(|h| (h / 8) as usize).collect();
    }
    out.colors = words.get(reference_words..reference_words + kind.colors).ok_or_else(short)?.to_vec();
    if kind.textured {
        let texture = words.get(reference_words + kind.colors..).ok_or_else(short)?;
        let &[first, second, third, ..] = texture else { return Err(short()) };
        out.uv = vec![first & 0xFFFF, second & 0xFFFF, third & 0xFFFF];
        if kind.corners == 4 {
            out.uv.push(third >> 16);
        }
        (out.clut, out.tpage) = (first >> 16, second >> 16);
    }
    Ok(out)
}

fn groups(bytes: &Bytes, start: usize) -> Result<Vec<Vec<Command>>> {
    let (mut groups, mut current, mut at) = (Vec::new(), Vec::new(), start);
    loop {
        let word = bytes.u32(at)?;
        if word == STREAM_END {
            groups.push(current);
            return Ok(groups);
        }
        if word == GROUP_END {
            groups.push(std::mem::take(&mut current));
            at += 4;
            continue;
        }
        let dims = bytes.u32(at + 8)?;
        let (count, stride) = ((dims >> 16) as usize, (dims & 0xFFFF) as usize);
        let mut elements = Vec::with_capacity(count);
        for e in 0..count {
            let mut words = Vec::with_capacity(stride);
            for w in 0..stride {
                words.push(bytes.u32(at + 12 + 4 * stride * e + 4 * w)?);
            }
            elements.push(words);
        }
        current.push(Command { op: word, elements });
        at += 12 + 4 * count * stride;
    }
}

/// The corner a seam face's cache reference stands for: what the pass said of it.
struct Passed {
    normal: usize,
    color: Option<u32>,
}

fn passes(groups: &[Vec<Command>]) -> Result<HashMap<(usize, u32), Passed>> {
    let mut passes = HashMap::new();
    for command in groups.iter().flatten() {
        let Some(colored) = pass_color(command.op & !MODIFIERS) else { continue };
        for words in &command.elements {
            let short = || anyhow!("a pass element is too short");
            let first = *words.first().ok_or_else(short)?;
            let (vertex, normal) = (((first & 0xFFFF) / 8) as usize, ((first >> 16) / 8) as usize);
            let (color, destinations) = if colored { (Some(*words.get(1).ok_or_else(short)?), words.get(2)) } else { (None, words.get(1)) };
            passes.insert((vertex, destinations.ok_or_else(short)? & 0xFFFF), Passed { normal, color });
        }
    }
    Ok(passes)
}

/// The game stores colors as 0xBBGGRR.
#[expect(clippy::cast_possible_truncation, reason = "each is masked to a byte")]
fn rgb(color: u32) -> [u8; 3] {
    [color as u8, (color >> 8) as u8, (color >> 16) as u8]
}

/// What is known of the model as its faces are read: slots and materials, each made the first time a face names it.
#[derive(Default)]
struct Reading {
    slots: HashMap<(u32, u32), usize>,
    materials: HashMap<(Option<usize>, bool, Option<u32>), usize>,
    effects: Vec<u32>,
    layers: Vec<bool>,
}

impl Reading {
    fn material(&mut self, model: &mut Model, kind: Kind, element: &Element) -> usize {
        let mut abr = None;
        let mut slot = None;
        if kind.textured {
            let (tp, cl) = (element.tpage, element.clut);
            let next = model.slots.len();
            let known = *self.slots.entry((tp & !0x60, cl)).or_insert(next);
            if known == next {
                #[expect(clippy::cast_possible_truncation, reason = "masked to what a texture page and a palette have")]
                let placement = Placement {
                    bits: if (tp >> 7) & 3 == 1 { 8 } else { 4 },
                    page: [(tp & 0xF) as u16 * 64, ((tp >> 4) & 1) as u16 * 256],
                    palette: [(cl & 0x3F) as u16 * 16, (cl >> 6) as u16],
                };
                model.slots.push(placement);
            }
            slot = Some(known);
            abr = Some((tp >> 5) & 3);
        }
        let next = model.materials.len();
        let known = *self.materials.entry((slot, kind.semi, abr)).or_insert(next);
        if known == next {
            let blend = abr.map(|abr| [Blend::Mix, Blend::Add, Blend::Subtract, Blend::AddQuarter][abr as usize]);
            model.materials.push(Material { transparent: kind.semi, slot, blend });
        }
        known
    }
}

/// Read the faces of a command.
fn read_command(model: &mut Model, reading: &mut Reading, command: &Command, passed: &HashMap<(usize, u32), Passed>, group: usize, written: &mut usize) -> Result<()> {
    let base = command.op & !MODIFIERS;
    if pass_color(base).is_some() {
        return Ok(());
    }
    let kind = kind(base).ok_or_else(|| anyhow!("opcode {:#X}", command.op))?;
    let layered = command.op & 0x4000 != 0;
    let routine = match command.op & 0x38000 {
        0 => 0,
        0x8000 => 1,
        0x10000 => 2,
        _ => 3,
    };
    for words in &command.elements {
        let read = element(kind, words)?;
        let material = reading.material(model, kind, &read);
        let vertices = if kind.seam { &read.cache } else { &read.vertices };
        let mut corners = Vec::with_capacity(vertices.len());
        for (k, vertex) in vertices.iter().copied().enumerate() {
            let mut corner = Corner { vertex, ..Corner::default() };
            if kind.textured {
                let uv = read.uv[k];
                #[expect(clippy::cast_possible_truncation, reason = "a byte each")]
                let bytes = [uv as u8, (uv >> 8) as u8];
                corner.uv = Some(bytes);
            }
            if kind.seam {
                let (_, stride) = packet(kind.corners, kind.textured);
                let destination = *written + if layered { 4 } else { 8 } + stride * k;
                let pass = u32::try_from(destination).ok().and_then(|d| passed.get(&(vertex, d))).ok_or_else(|| anyhow!("a seam corner no pass wrote"))?;
                corner.normal = Some(pass.normal);
                corner.color = pass.color.map(|color| rgb(color & 0xFF_FFFF));
            } else if kind.shading == Shading::Smooth {
                corner.normal = Some(*read.normals.get(k).ok_or_else(|| anyhow!("a corner without a normal"))?);
            }
            if kind.colored == Colored::Corner && !kind.seam {
                corner.color = Some(rgb(read.colors.get(k).ok_or_else(|| anyhow!("a corner without a color"))? & 0xFF_FFFF));
            }
            corners.push(corner);
        }
        if corners.len() == 4 {
            // Stored in Z order, kept as the rim.
            corners.swap(2, 3);
        }
        if kind.seam {
            *written += packet(kind.corners, kind.textured).0 * if layered { 2 } else { 1 };
        } else {
            reading.effects.push(routine);
        }
        reading.layers.push(layered);
        model.faces.push(Face {
            material,
            shading: kind.shading,
            part: (!kind.seam).then_some(group),
            normal: (kind.shading == Shading::Flat).then(|| read.normals[0]),
            color: (kind.colored == Colored::Face).then(|| read.colors.first().map(|color| rgb(color & 0xFF_FFFF))).flatten(),
            corners,
        });
    }
    Ok(())
}

/// A model from its bytes, arrays through record, and the address of its first byte.
///
/// # Errors
/// They are not a model.
pub fn decode(blob: &[u8], address: u32) -> Result<Model> {
    let bytes = Bytes { data: blob, load: address };
    let record = blob.len().checked_sub(RECORD_SIZE).ok_or_else(|| anyhow!("too short for a record"))?;
    let (parts, counts) = (bytes.u32(record + 12)? as usize, bytes.u32(record + 16)?);
    let (verts, norms, skeleton, stream) = (bytes.u32(record + 20)?, bytes.u32(record + 24)?, bytes.u32(record + 28)?, bytes.u32(record + 32)?);
    if counts == 0 {
        bail!("no counts of each part's vertices");
    }
    let (counts, verts_at, norms_at, skeleton_at, stream_at) =
        (bytes.offset(counts)?, bytes.offset(verts)?, bytes.offset(norms)?, bytes.offset(skeleton)?, bytes.offset(stream)?);
    let vertex_count = norms_at.saturating_sub(verts_at) / 8;
    let normal_count = stream_at.saturating_sub(norms_at) / 8;
    let mut model = Model::default();
    let mut next = 0;
    for i in 0..parts {
        let bone = skeleton_at + BONE_SIZE * i;
        let mut rotation = [[0; 3]; 3];
        for (r, row) in rotation.iter_mut().enumerate() {
            *row = bytes.triple(bone + 6 * r)?;
        }
        let translation = [bytes.i32(bone + 20)?, bytes.i32(bone + 24)?, bytes.i32(bone + 28)?];
        let parent = usize::try_from(bytes.i32(bone + 32)?).map_err(|_| anyhow!("part {i}: no parent"))?;
        let count = bytes.u32(counts + 4 * i)? as usize;
        let mut vertices = Vec::with_capacity(count);
        for v in next..next + count {
            vertices.push(bytes.triple(verts_at + 8 * v)?);
        }
        next += count;
        model.parts.push(Part { parent, rotation, translation, vertices });
    }
    for v in next..vertex_count {
        model.loose_vertices.push(bytes.triple(verts_at + 8 * v)?);
    }
    for n in 0..normal_count {
        model.normals.push(bytes.triple(norms_at + 8 * n)?);
    }
    let groups = groups(&bytes, stream_at)?;
    let passed = passes(&groups)?;
    let mut reading = Reading::default();
    for (index, group) in groups.iter().enumerate() {
        let mut written = 0;
        for command in group {
            read_command(&mut model, &mut reading, command, &passed, index, &mut written)?;
        }
    }
    model.effect = reading.effects.first().copied().unwrap_or(0);
    model.reflection = reading.layers.first().copied().unwrap_or(false);
    Ok(model)
}
/// What a slot's faces are drawn with, in the terms the game stores them in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    pub texture: Option<Placement>,
    pub transparent: bool,
    /// Stored in every texture page, an opaque face's too, where the GPU does not read it.
    pub blend: Blend,
    /// The second, additive layer: all of a model has it, or none.
    pub reflective: bool,
    /// The draw routine the package gives its faces: all of a model has one, or none.
    pub effect: u32,
}

impl Placement {
    /// Its palette, as the GPU is told it.
    #[must_use]
    pub fn clut(&self) -> u32 {
        u32::from(self.palette[0] / 16) | u32::from(self.palette[1]) << 6
    }
}

impl Binding {
    /// Its texture page, as the GPU is told it.
    #[must_use]
    pub fn tpage(&self) -> Option<u32> {
        let placement = self.texture?;
        let [x, y] = placement.page;
        Some(u32::from(x / 64) | u32::from(y / 256) << 4 | (self.blend as u32) << 5 | u32::from(placement.bits == 8) << 7)
    }
}

/// A run of one opcode, as it is being written.
struct Run {
    op: u32,
    elements: Vec<Vec<u32>>,
    open: bool,
}

fn add(group: &mut Vec<Run>, op: u32, words: Vec<u32>) {
    match group.last_mut() {
        Some(run) if run.op == op && run.open => run.elements.push(words),
        _ => group.push(Run { op, elements: vec![words], open: true }),
    }
}

/// A corner of a seam face, for the part its vertex is in to transform.
struct Pass {
    op: u32,
    vertex: u32,
    destinations: (u32, u32),
    normal: u32,
    color: Option<u32>,
}

fn color_word(color: [u8; 3]) -> u32 {
    u32::from(color[0]) | u32::from(color[1]) << 8 | u32::from(color[2]) << 16
}

/// The GPU code of a colored element's first word.
fn code(base: u32) -> Option<u32> {
    Some(match base {
        0x04 => 0x20,
        0x44 => 0x28,
        0x20 => 0x30,
        0x22 => 0x32,
        0x40 | 0x60 => 0x38,
        0x62 => 0x3A,
        0x30 => 0x34,
        0x70 | 0x156 => 0x3C,
        _ => return None,
    })
}

/// The bytes of the primitive a base opcode builds in the object's buffer.
fn primitive(base: u32) -> usize {
    match base {
        0x04 => 20,
        0x44 => 24,
        0x20..=0x22 => 28,
        0x1C | 0x1E => 32,
        0x40 | 0x60 | 0x62 => 36,
        0x18 | 0x1A | 0x30 | 0x38 | 0x3A | 0x5C | 0x5E | 0x39 | 0x3B => 40,
        _ => 52,
    }
}

/// The opcode that draws a face, by what it says of itself.
fn opcode(want: Kind) -> Option<u32> {
    (0..0x200).find(|&base| {
        kind(base).is_some_and(|k| {
            (k.corners, k.textured, k.seam, k.shading, k.colored, k.semi) == (want.corners, want.textured, want.seam, want.shading, want.colored, want.semi)
        })
    })
}

fn packed(values: &[u32]) -> Vec<u32> {
    values.chunks(2).map(|pair| pair[0] | pair.get(1).copied().unwrap_or(0) << 16).collect()
}

fn index_word(index: usize, scale: usize) -> Result<u32> {
    u32::try_from(index * scale).map_err(|_| anyhow!("an index out of range"))
}

/// What a face's element holds. Its corners are as stored.
fn element_words(base: u32, kind: Kind, face: &Face, corners: &[Corner], binding: Binding) -> Result<Vec<u32>> {
    let mut references = Vec::new();
    if kind.seam {
        for corner in corners {
            references.push(index_word(corner.vertex, 4)?);
        }
    } else {
        for corner in corners {
            references.push(index_word(corner.vertex, 8)?);
        }
        for corner in corners {
            if face.shading == Shading::Smooth {
                references.push(index_word(corner.normal.ok_or_else(|| anyhow!("a smooth corner without a normal"))?, 8)?);
            }
        }
        if face.shading == Shading::Flat {
            references.push(index_word(face.normal.ok_or_else(|| anyhow!("a flat face without a normal"))?, 8)?);
        }
    }
    let mut words = packed(&references);
    let first = code(base).unwrap_or(0) << 24;
    if kind.colored == Colored::Face {
        words.push(first | color_word(face.color.ok_or_else(|| anyhow!("a face without its color"))?));
    }
    if kind.colored == Colored::Corner && !kind.seam {
        for (at, corner) in corners.iter().enumerate() {
            let color = color_word(corner.color.ok_or_else(|| anyhow!("a corner without its color"))?);
            words.push(if at == 0 { first | color } else { color });
        }
    }
    if kind.textured {
        let placement = binding.texture.ok_or_else(|| anyhow!("a face with coordinates, in a slot with no texture"))?;
        let (page, clut) = (binding.tpage().unwrap_or(0), placement.clut());
        let uv = |at: usize| corners.get(at).and_then(|corner| corner.uv).map_or(0, |[u, v]| u32::from(u) | u32::from(v) << 8);
        words.extend([uv(0) | clut << 16, uv(1) | page << 16, uv(2) | uv(3) << 16]);
    }
    Ok(words)
}

/// The commands of a model, a group each part, the seam's and an empty last: faces in their order, then each part's
/// pass over the seam corners it holds. With what the seam's primitives and all of them take of the buffer.
fn commands(model: &Model, bindings: &[Binding], layered: bool, effect: u32) -> Result<(Vec<Vec<Run>>, usize, usize)> {
    let mut starts = vec![0];
    for part in &model.parts {
        starts.push(starts[starts.len() - 1] + part.vertices.len());
    }
    let part_of = |vertex: usize| starts.windows(2).position(|span| span[0] <= vertex && vertex < span[1]);
    let seam_group = model.parts.len();
    let mut groups: Vec<Vec<Run>> = (0..model.parts.len() + 2).map(|_| Vec::new()).collect();
    let mut passes: Vec<Vec<Pass>> = (0..model.parts.len()).map(|_| Vec::new()).collect();
    let mut written = 0usize;
    for face in &model.faces {
        let mut corners = face.corners.clone();
        if corners.len() == 4 {
            corners.swap(2, 3);
        }
        let binding = *bindings.get(face.material).ok_or_else(|| anyhow!("a face in slot {} of {}", face.material, bindings.len()))?;
        let seam = face.part.is_none();
        let colored = if corners.iter().any(|corner| corner.color.is_some()) {
            Colored::Corner
        } else if face.color.is_some() {
            Colored::Face
        } else {
            Colored::Not
        };
        let textured = corners.iter().any(|corner| corner.uv.is_some());
        let wanted = Kind { corners: corners.len(), normals: 0, colors: 0, textured, seam, shading: face.shading, colored, semi: binding.transparent };
        let base = opcode(wanted).ok_or_else(|| anyhow!("no opcode draws a {}-cornered {:?} face", corners.len(), face.shading))?;
        let routine = if !seam || effect == 1 { effect } else { 0 };
        let op = base | if layered { 0x4000 } else { 0 } | [0, 0x8000, 0x10000, 0x20000][routine as usize & 3];
        let words = element_words(base, wanted, face, &corners, binding)?;
        let Some(part) = face.part else {
            add(&mut groups[seam_group], op, words);
            let (size, stride) = packet(corners.len(), textured);
            for (k, corner) in corners.iter().enumerate() {
                let at = u32::try_from(written + stride * k).map_err(|_| anyhow!("a seam past the buffer"))?;
                let size = u32::try_from(size).map_err(|_| anyhow!("a seam past the buffer"))?;
                let (destinations, op) = if layered {
                    ((at + 4, at + 4 + size), 0x40C8)
                } else if effect == 3 {
                    ((at + 8, at + 4), 0x2_00C8)
                } else {
                    ((at + 8, at + 4), if corner.color.is_some() { 0xC0 } else { 0xC8 })
                };
                let normal = index_word(corner.normal.ok_or_else(|| anyhow!("a seam corner without a normal"))?, 1)?;
                let vertex = index_word(corner.vertex, 1)?;
                let owner = part_of(corner.vertex).ok_or_else(|| anyhow!("a seam corner on no part"))?;
                passes[owner].push(Pass { op, vertex, destinations, normal, color: corner.color.map(color_word) });
            }
            written += size * if layered { 2 } else { 1 };
            continue;
        };
        add(groups.get_mut(part).ok_or_else(|| anyhow!("a face on no part"))?, op, words);
    }
    for (part, mut pass) in passes.into_iter().enumerate() {
        for run in &mut groups[part] {
            run.open = false;
        }
        pass.sort_by_key(|p| (p.op, p.vertex, p.destinations));
        for p in pass {
            let mut words = vec![(p.vertex * 8) | ((p.normal * 8) << 16)];
            words.extend(p.color);
            words.push(p.destinations.0 | p.destinations.1 << 16);
            add(&mut groups[part], p.op, words);
        }
    }
    let size = |groups: &[Vec<Run>]| -> usize {
        let runs = groups.iter().flatten();
        let drawn = runs.filter(|run| pass_color(run.op & !MODIFIERS).is_none());
        drawn.map(|run| primitive(run.op & !MODIFIERS) * if run.op & 0x4000 != 0 { 2 } else { 1 } * run.elements.len()).sum()
    };
    let (seam, all) = (size(&groups[seam_group..=seam_group]), size(&groups));
    Ok((groups, seam, all))
}

/// A part as the game's skeleton stores it.
pub struct Bone<'a> {
    pub rotation: &'a [[i16; 3]; 3],
    pub translation: [i32; 3],
    pub parent: i32,
}

/// What a game stores for a model, as the record points at it.
pub struct Arrays<'a> {
    pub bones: Vec<Bone<'a>>,
    pub part_vertices: Vec<u32>,
    pub vertices: Vec<[i16; 3]>,
    pub normals: Vec<[i16; 3]>,
    pub stream: Vec<u32>,
    /// What its commands take of the primitive buffer, and the part of that before the seam.
    pub buffer: u32,
    pub seam: u32,
}

/// A model's arrays.
///
/// # Errors
/// A face cannot be drawn with its slot's binding, or there is no opcode for it.
pub fn arrays<'a>(model: &'a Model, bindings: &[Binding]) -> Result<Arrays<'a>> {
    let layered = bindings.first().is_some_and(|binding| binding.reflective);
    if bindings.iter().any(|binding| binding.reflective != layered) {
        bail!("the second layer is the whole model's: every slot is reflective, or none");
    }
    let effect = bindings.first().map_or(0, |binding| binding.effect);
    if bindings.iter().any(|binding| binding.effect != effect) {
        bail!("an effect is the whole model's: every slot has the same one");
    }
    let (groups, seam, all) = commands(model, bindings, layered, effect)?;
    let mut stream = Vec::new();
    let last = groups.len() - 1;
    for (at, group) in groups.iter().enumerate() {
        for run in group {
            let stride = run.elements.first().map_or(0, Vec::len);
            stream.extend([run.op, 0, u32::try_from(run.elements.len() << 16 | stride)?]);
            stream.extend(run.elements.iter().flatten());
        }
        stream.push(if at == last { STREAM_END } else { GROUP_END });
    }
    let bones = model.parts.iter().map(|part| Ok(Bone { rotation: &part.rotation, translation: part.translation, parent: i32::try_from(part.parent)? }));
    Ok(Arrays {
        bones: bones.collect::<Result<_>>()?,
        part_vertices: model.parts.iter().map(|part| u32::try_from(part.vertices.len())).collect::<Result<_, _>>()?,
        vertices: model.parts.iter().flat_map(|part| &part.vertices).chain(&model.loose_vertices).copied().collect(),
        normals: model.normals.clone(),
        stream,
        buffer: u32::try_from(all)?,
        seam: u32::try_from(seam)?,
    })
}

/// What a game stores for a model: its arrays and its record, as `decode` reads them.
///
/// # Errors
/// As `arrays`.
pub fn encode(model: &Model, bindings: &[Binding], address: u32) -> Result<Vec<u8>> {
    let arrays = arrays(model, bindings)?;
    let mut out = Vec::new();
    let u32s = |out: &mut Vec<u8>, values: &[u32]| {
        for value in values {
            out.extend_from_slice(&value.to_le_bytes());
        }
    };
    for bone in &arrays.bones {
        for value in bone.rotation.iter().flatten() {
            out.extend_from_slice(&value.to_le_bytes());
        }
        out.extend_from_slice(&[0; 2]);
        for value in bone.translation {
            out.extend_from_slice(&value.to_le_bytes());
        }
        out.extend_from_slice(&bone.parent.to_le_bytes());
    }
    let counts_at = out.len();
    u32s(&mut out, &arrays.part_vertices);
    let verts_at = out.len();
    for point in &arrays.vertices {
        for value in point.iter().chain(&[0]) {
            out.extend_from_slice(&value.to_le_bytes());
        }
    }
    let norms_at = out.len();
    for normal in &arrays.normals {
        for value in normal.iter().chain(&[0]) {
            out.extend_from_slice(&value.to_le_bytes());
        }
    }
    let stream_at = out.len();
    u32s(&mut out, &arrays.stream);
    let address_of = |at: usize| Ok::<u32, std::num::TryFromIntError>(address.wrapping_add(u32::try_from(at)?));
    let record = [
        0,
        arrays.buffer,
        arrays.seam,
        u32::try_from(arrays.bones.len())?,
        address_of(counts_at)?,
        address_of(verts_at)?,
        address_of(norms_at)?,
        address,
        address_of(stream_at)?,
    ];
    u32s(&mut out, &record);
    Ok(out)
}

#[cfg(test)]
mod encoding {
    use super::*;

    const ADDRESS: u32 = 0x8010_0000;

    fn binding(transparent: bool, blend: Blend) -> Binding {
        Binding { texture: Some(Placement { bits: 8, page: [448, 256], palette: [0, 246] }), transparent, blend, reflective: false, effect: 0 }
    }

    /// Two parts, a textured smooth quad on the first and a triangle spanning both.
    fn model() -> Model {
        let identity = [[4096, 0, 0], [0, 4096, 0], [0, 0, 4096]];
        let corner = |vertex: usize, u: u8| Corner { vertex, normal: Some(vertex), color: None, uv: Some([u, u + 1]) };
        let part = |parent: usize, vertices: Vec<[i16; 3]>| Part { parent, rotation: identity, translation: [0, -100, 0], vertices };
        Model {
            parts: vec![part(0, vec![[0, 0, 0], [10, 0, 0], [10, 10, 0], [0, 10, 0]]), part(0, vec![[5, 5, 5]])],
            normals: vec![[0, 0, 4096]; 5],
            faces: vec![
                Face { material: 0, shading: Shading::Smooth, part: Some(0), normal: None, color: None, corners: vec![corner(0, 1), corner(1, 2), corner(2, 3), corner(3, 4)] },
                Face { material: 0, shading: Shading::Smooth, part: None, normal: None, color: None, corners: vec![corner(0, 5), corner(1, 6), corner(4, 7)] },
            ],
            ..Model::default()
        }
    }

    fn drawn(bytes: &[u8]) -> Model {
        decode(bytes, ADDRESS).unwrap()
    }

    #[test]
    fn what_is_encoded_decodes_and_encodes_the_same() {
        let bindings = [binding(false, Blend::Add)];
        let bytes = encode(&model(), &bindings, ADDRESS).unwrap();
        let read = drawn(&bytes);
        assert_eq!(format!("{:?}", read.faces), format!("{:?}", model().faces));
        assert_eq!(read.slots, [bindings[0].texture.unwrap()]);
        assert_eq!(encode(&read, &bindings, ADDRESS).unwrap(), bytes);
    }

    #[test]
    fn a_material_says_how_the_faces_draw() {
        let bytes = encode(&model(), &[binding(true, Blend::Subtract)], ADDRESS).unwrap();
        assert_eq!(drawn(&bytes).materials[0], Material { transparent: true, slot: Some(0), blend: Some(Blend::Subtract) });
    }

    #[test]
    fn an_effect_comes_from_the_materials_and_is_the_whole_models() {
        let with = Binding { effect: 2, ..binding(false, Blend::Add) };
        assert_eq!(drawn(&encode(&model(), &[with], ADDRESS).unwrap()).effect, 2);
        assert!(encode(&model(), &[with, binding(false, Blend::Add)], ADDRESS).is_err());
    }

    #[test]
    fn a_reflective_slot_layers_the_whole_model() {
        let reflective = Binding { reflective: true, ..binding(false, Blend::Add) };
        assert!(drawn(&encode(&model(), &[reflective], ADDRESS).unwrap()).reflection);
        let mixed = Model { faces: model().faces.into_iter().map(|face| Face { material: usize::from(face.part.is_none()), ..face }).collect(), ..model() };
        assert!(encode(&mixed, &[reflective, binding(false, Blend::Add)], ADDRESS).is_err());
    }

    #[test]
    fn a_face_with_coordinates_needs_a_texture() {
        let bare = Binding { texture: None, ..binding(false, Blend::Add) };
        assert!(encode(&model(), &[bare], ADDRESS).is_err());
    }
}
