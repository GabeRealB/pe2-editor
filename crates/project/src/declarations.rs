//! The declarations: where extraction finds each asset on a release's discs. They are extraction's map from the discs
//! to the assets; packing builds from the project's assets, never from them. Extraction fails where a disc differs from
//! what they say - a reference that names no declaration, a disc whose files differ from its layout, a file or chunk
//! not where they place it, bytes that differ from their sha1.
//!
//! - Group files, `<group>.toml` outside `disc/`: one table an asset - its uid, kind, its own fields (a folder's or
//!   file's slots, a stream's descriptor fields, an image's palettes, ...), and per release where extraction finds it
//!   (`from.<release>.at`), with the original bytes' sha1 where they are read whole. A file's chunks' payload sha1s are
//!   the file's (`from.<release>.chunks`).
//! - The disc layout, `disc/<release>/*.toml`: one asset a file, a disc (`disk<N>`) or a stage (`stage<N>`).
//!
//! Every slot names the asset that fills it, `asset = { uid, path = "res://<group>/<name>.asset" }`; loading holds
//! every such reference to a declaration with that uid. Uids are drawn once, when an asset is first declared (`uid`).
//!
//! # The layout
//!
//! A chain of assets, each a container whose slots name the assets that fill them:
//!
//! ```text
//! disc     disc/<release>/disk<N>    the license data and the ISO's files, in order (type: XA mode)
//! stage    disc/<release>/stage<N>   bound to the disc's STAGE<N>.CDF slot (stage 0: STAGE0.HED + STAGE0.CDF) -
//!                                    stage 0's stream slots and file groups, stages 1-5's folders, by id
//! group    stage0/<group>            a stage 0 file group, the loader's category or an actor slot family; its file
//!                                    slots keyed within it, the file id `group * 10000 + key`
//! folder   <room>/folder             a stage folder: its stream slots and its file slots, by id
//! file     .../files/<name>          a file's slots in order, typed - package, image, palette, bundle, bs,
//!                                    sound_bank, text; a `sequence` (a whole-file hMPK music image); a `trailer`
//! stream   .../streams/<name>        a `movie` or `scene_audio`: its `StreamSlot` fields by their C names
//! ```
//!
//! A slot's id is the slot's - a folder's or file's place - never part of the asset, which another slot may also hold
//! (a stage 0 file sharing another's bytes is one file asset in two slots). What follows from the layout is not
//! written: slots are laid out in id order, so offsets and sizes follow. Nor what follows from a bound asset: a
//! package's load address, a chunk's size and end flag, a stream's location and what its bytes determine (a movie's
//! `width` and `height`, a scene's `timing_bytes` and `resume_sector_offset`). `frame_limit` is the stream's own, an
//! authored stop. The HED marker (control bit 15) is the container's. A stage's rooms are its folders, under
//! `stages/<stage>/rooms/<room>`; Dryfield's day and night stages hold one room each as two instances, `<room>/day`
//! and `<room>/night`, as `src/stages/dryfield` builds them.
//!
//! # VRAM and images
//!
//! VRAM is 1024 x 512 halfwords. A `vram_slot` (`engine/vram`) is a rectangle of it, `rect = [x, y, w, h]`, that
//! uploads are placed in: a slot `within` another is part of it (the item preview's 96 drawn rows of its 128-row
//! sheet); one that `borrows` another overwrites it while it is up, and whoever owns the other restores it (the item
//! menu's previews over the room's sprite pages). Any other overlap is two uses of the same VRAM at different times.
//!
//! Textures and atlases are images (`kind = "image"`) by `role`, as backgrounds are: a slot that takes an image takes
//! any of them. A texture is pixels in a `mode` - `indexed_4bpp`, `indexed_8bpp` or `direct_16bpp` - and a `size` in
//! texels (its kind settings; its `[import]` section names the importer of its stored texels, `pe2.images@texels`, a
//! background's `pe2.images@bs`), uploaded to a `page` - the
//! VRAM slot and the halfword `at` in it - filling as many 64-halfword VRAM pages, side by side, as its width needs;
//! and its `palettes`, each named, placed where its colors are uploaded (`slot`, `at`), or uploaded by an executable
//! (`executable`), or none known (`no_palette`). A texture is drawn whole, with its palette. Its location lists where
//! its first page is read, and the sha1s of its texels (a byte a texel at 4 and 8 bits, a halfword at 16, row by row)
//! and of each palette's colors. A texture at its page's corner is as large as its non-zero texels and its cells -
//! exactly, for a picture a menu draws whole; rounded up to 8 for a page that is sampled.
//!
//! An atlas is a texture whose regions are drawn separately - a sprite, a polygon, a glyph sampling a rectangle of it
//! with a palette of its own - with `cells`, each a named `rect` in texels and the `palette` it is drawn with; a cell
//! of another depth than the image's states its `mode`, its rectangle in texels of that depth (the UI sheet's 8-bit
//! portrait among 4-bit icons). A palette holds the colors of the deepest cell drawn with it. The cells are those its
//! users name: a room's view sprites (`view<N>_sprite_<i>`), the code's own tables (a clock's digits, the HUD's
//! icons), a model's faces. A multi-page image whose pages are drawn with palettes of their own, where nothing names
//! finer regions yet, is an atlas of `page_<n>` cells.
//!
//! An image's `variants` are its other looks: a texture's each a `palette`, the first its default; an atlas's each the
//! `palettes` some of its cells take instead (`{ <cell> = <palette> }` - the UI sheet's `ammo_9mm` recolors three
//! icons).
//!
//! A file's image slot lists its `pages` - an `image`, or a place where the page holds palettes only - each image
//! filling the pages its width needs; where a file holds an image in two chunks, each entry says which of its pages
//! that chunk holds (`first`, `count`). A palette slot (or a palette resource of a bundle) lists its `palettes`: an
//! image's palette by name (`image`, `palette`), or a `name`d run of `entries` colors placed by `slot` and `at`, which
//! no image owns: an image palette no slot names takes its colors from the run uploaded at its place (the system's
//! text palettes, which every dialogue atlas is drawn with). An image chunk stores each page from its row to the
//! bottom of its slot, within the 256-row column, every bit around its images zero; a palette chunk stores the
//! rectangle its palettes span, or its `upload` (a whole slot, or `at` and `size` in one), every color outside its
//! palettes zero.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::uid;

/// A reference to a declared asset: its uid and its bundle's `res://` path.
#[derive(Clone, Debug, Deserialize)]
pub struct Reference {
    pub uid: String,
    pub path: String,
}

impl Reference {
    /// The declaration path (`group/name`) it names.
    #[must_use]
    pub fn target(&self) -> &str {
        self.path.trim_start_matches("res://").trim_end_matches(".asset")
    }
}

/// Where extraction finds an asset: a disc's slots (`disc`, `slot` or `slots`), or a place in a stage - a folder, a
/// group, a file (`stage`, `folder` but in stage 0, `file`), a file's chunk slot (and `slot`), a stream.
#[derive(Clone, Debug, Deserialize)]
pub struct At {
    pub disc: Option<String>,
    pub slot: Option<String>,
    #[serde(default)]
    pub slots: Vec<String>,
    pub stage: Option<u32>,
    pub folder: Option<u32>,
    pub group: Option<u32>,
    pub file: Option<u32>,
    pub stream: Option<String>,
    /// An image chunk slot's page.
    pub page: Option<usize>,
}

impl At {
    pub fn slots(&self) -> Vec<&str> {
        self.slot.iter().map(String::as_str).chain(self.slots.iter().map(String::as_str)).collect()
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Location {
    #[serde(default)]
    pub at: Vec<At>,
    pub sha1: Option<String>,
    /// An image's palettes' colors, by sha1.
    #[serde(default)]
    pub palettes: BTreeMap<String, String>,
}

/// An asset of a group file.
#[derive(Clone, Debug, Deserialize)]
pub struct Asset {
    pub uid: String,
    pub kind: String,
    pub role: Option<String>,
    pub format: Option<String>,
    #[serde(default)]
    pub from: BTreeMap<String, Location>,
    /// A file's chunk slots, in order.
    #[serde(default)]
    pub slots: Vec<ChunkSlot>,
    /// The declaration's own fields as TOML, without `from`: what its bundle's `asset.toml` starts with.
    #[serde(skip)]
    pub body: String,
}

impl Asset {
    /// The declaration's own fields read as a kind's view (a texture, an atlas, a VRAM slot).
    ///
    /// # Errors
    /// The fields do not have the view's shape.
    pub fn view<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        Ok(toml::from_str(&self.body)?)
    }
}

/// A file's chunk slot; an image or palette slot says what it uploads (the module's "VRAM and images").
#[derive(Clone, Debug, Deserialize)]
pub struct ChunkSlot {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(default)]
    pub pages: Vec<PageEntry>,
    #[serde(default)]
    pub palettes: Vec<PaletteEntry>,
    /// A bundle's resources.
    #[serde(default)]
    pub resources: Vec<Resource>,
}

/// A place in a VRAM slot, in halfwords from its corner.
#[derive(Clone, Debug, Deserialize)]
pub struct Placement {
    pub slot: Reference,
    pub at: [u32; 2],
}

/// An image slot's page: an image (a texture or an atlas), or (palettes only) a placement.
#[derive(Clone, Debug, Deserialize)]
pub struct PageEntry {
    pub image: Option<Reference>,
    /// The first of the image's pages this chunk holds, where it holds not its first.
    pub first: Option<usize>,
    /// How many of them, where it holds not the rest.
    pub count: Option<usize>,
    pub slot: Option<Reference>,
    pub at: Option<[u32; 2]>,
}

/// A palette a slot uploads: an image's, by name, or a named run of colors no image owns.
#[derive(Clone, Debug, Deserialize)]
pub struct PaletteEntry {
    pub image: Option<Reference>,
    pub palette: Option<String>,
    pub name: Option<String>,
    pub entries: Option<u32>,
    pub slot: Option<Reference>,
    pub at: Option<[u32; 2]>,
}

/// A bundle's resource slot.
#[derive(Clone, Debug, Deserialize)]
pub struct Resource {
    pub index: usize,
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(default)]
    pub palettes: Vec<PaletteEntry>,
}

/// What a texel is: an index into a palette of 16 or 256 colors, or a color.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub enum Mode {
    #[serde(rename = "indexed_4bpp")]
    Indexed4,
    #[serde(rename = "indexed_8bpp")]
    Indexed8,
    #[serde(rename = "direct_16bpp")]
    Direct16,
}

impl Mode {
    /// Its depth: bits a texel.
    #[must_use]
    pub fn bpp(self) -> u8 {
        match self {
            Mode::Indexed4 => 4,
            Mode::Indexed8 => 8,
            Mode::Direct16 => 16,
        }
    }
}

/// A texture or an atlas - its kind settings: its mode and size, its page, its palettes (where each is uploaded) and an
/// atlas's cells, the parts keyed by name in the order declared.
#[derive(Clone, Debug, Deserialize)]
pub struct Texture {
    pub mode: Mode,
    pub size: [u32; 2],
    pub page: Option<Placement>,
    #[serde(default, deserialize_with = "keyed")]
    pub palettes: Vec<Palette>,
    #[serde(default, deserialize_with = "keyed")]
    pub cells: Vec<Cell>,
    #[serde(default)]
    pub no_palette: bool,
}

/// Parts keyed by name (`cells.disc_1 = { ... }`), in the order declared, each given its key as its `name`.
fn keyed<'de, D, T>(deserializer: D) -> std::result::Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    use serde::de::Error;
    let table = toml::Table::deserialize(deserializer)?;
    table
        .into_iter()
        .map(|(name, value)| {
            let mut value = value;
            if let Some(fields) = value.as_table_mut() {
                fields.insert("name".into(), toml::Value::String(name.clone()));
            }
            value.try_into().map_err(|e| D::Error::custom(format!("{name}: {e}")))
        })
        .collect()
}

impl Texture {
    /// How many colors a palette holds: the deepest of the cells drawn with it, or the image's depth.
    #[must_use]
    pub fn entries(&self, palette: &str) -> usize {
        let cells = self.cells.iter().filter(|c| c.palette.as_deref() == Some(palette));
        cells.map(|c| entries_at(c.mode.unwrap_or(self.mode).bpp())).max().unwrap_or(entries_at(self.mode.bpp()))
    }
}

fn entries_at(bpp: u8) -> usize {
    match bpp {
        4 => 16,
        8 => 256,
        _ => 0,
    }
}

/// A named palette: where its colors are uploaded, or the executable that uploads them.
#[derive(Clone, Debug, Deserialize)]
pub struct Palette {
    pub name: String,
    pub slot: Option<Reference>,
    pub at: Option<[u32; 2]>,
    pub executable: Option<String>,
}

/// A region of an atlas drawn on its own: a rectangle in texels (of its own mode, where it states one) and its
/// palette.
#[derive(Clone, Debug, Deserialize)]
pub struct Cell {
    pub name: String,
    pub rect: [u32; 4],
    pub palette: Option<String>,
    pub mode: Option<Mode>,
}

/// A rectangle of VRAM uploads are placed in.
#[derive(Clone, Debug, Deserialize)]
pub struct VramSlot {
    pub rect: [u32; 4],
    pub within: Option<Reference>,
    #[serde(default)]
    pub borrows: Vec<Reference>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DiscImageDecl {
    pub name: String,
    pub cue: String,
    pub size: u64,
    pub sha1: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DiscSlot {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
    pub asset: Option<Reference>,
}

/// A disc of the layout.
#[derive(Clone, Debug, Deserialize)]
pub struct Disc {
    pub uid: String,
    pub image: DiscImageDecl,
    pub slots: Vec<DiscSlot>,
    #[serde(default)]
    pub dummy_sectors: u32,
}

/// One file of the layout: a disc or a container, parsed where extraction needs it, and its text.
pub struct Layout {
    pub uid: String,
    pub kind: String,
    pub disc: Option<Disc>,
    pub at: Vec<At>,
    pub text: String,
}

#[derive(Deserialize)]
struct LayoutHead {
    uid: String,
    kind: String,
    #[serde(default)]
    at: Vec<At>,
}

pub struct Declarations {
    pub root: PathBuf,
    /// Group assets by declaration path, `group/name`.
    pub assets: BTreeMap<String, Asset>,
    /// Per release, the layout's assets by name (`disk1`, `stage0`, ...).
    pub layouts: BTreeMap<String, BTreeMap<String, Layout>>,
}

fn toml_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("{}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            toml_files(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "toml") {
            out.push(path);
        }
    }
    Ok(())
}

impl Declarations {
    /// The declarations under `root`.
    ///
    /// # Errors
    /// A file cannot be read or parsed, a layout file is not `disc/<release>/<name>.toml`, or a uid is malformed or
    /// taken twice.
    pub fn load(root: &Path) -> Result<Declarations> {
        let layout_dir = root.join("disc");
        let mut files = Vec::new();
        toml_files(root, &mut files)?;
        files.sort();
        let (mut assets, mut layouts) = (BTreeMap::new(), BTreeMap::<String, BTreeMap<String, Layout>>::new());
        for path in files {
            let text = std::fs::read_to_string(&path)?;
            let rel = path.strip_prefix(root)?.with_extension("");
            let rel = rel.to_string_lossy().replace('\\', "/");
            if path.starts_with(&layout_dir) {
                let (release, name) = rel
                    .strip_prefix("disc/")
                    .and_then(|r| r.split_once('/'))
                    .with_context(|| format!("{}: not disc/<release>/<name>.toml", path.display()))?;
                let head: LayoutHead = toml::from_str(&text).with_context(|| format!("{}", path.display()))?;
                let disc = if head.kind == "disc" {
                    Some(toml::from_str::<Disc>(&text).with_context(|| format!("{}", path.display()))?)
                } else {
                    None
                };
                layouts
                    .entry(release.to_string())
                    .or_default()
                    .insert(name.to_string(), Layout { uid: head.uid, kind: head.kind, disc, at: head.at, text });
                continue;
            }
            let doc: toml_edit::DocumentMut = text.parse().with_context(|| format!("{}", path.display()))?;
            let group: toml::Table = toml::from_str(&text).with_context(|| format!("{}", path.display()))?;
            for (name, value) in group {
                let mut asset: Asset = value.try_into().with_context(|| format!("{}: [{name}]", path.display()))?;
                let mut body = toml_edit::DocumentMut::new();
                if let Some(table) = doc.get(&name).and_then(toml_edit::Item::as_table) {
                    for (key, item) in table.iter().filter(|(key, _)| *key != "from") {
                        body.insert(key, item.clone());
                    }
                }
                asset.body = body.to_string();
                assets.insert(format!("{rel}/{name}"), asset);
            }
        }
        let decl = Declarations { root: root.to_path_buf(), assets, layouts };
        decl.validate()?;
        Ok(decl)
    }

    /// Every uid well formed and unique, and every reference naming a declaration under its uid.
    fn validate(&self) -> Result<()> {
        self.validate_uids()?;
        let texts = self.assets.iter().map(|(path, asset)| (path.clone(), asset.body.as_str())).chain(
            self.layouts
                .iter()
                .flat_map(|(r, l)| l.iter().map(move |(n, l)| (format!("disc/{r}/{n}"), l.text.as_str()))),
        );
        for (path, text) in texts {
            let value: toml::Value = toml::from_str(text).with_context(|| path.clone())?;
            let mut refs = Vec::new();
            references(&value, &mut refs);
            for r in refs {
                if self.uid_of(r.target()) != Some(r.uid.as_str()) {
                    bail!("{path}: its reference to {} ({}) names no such declaration", r.path, r.uid);
                }
            }
        }
        Ok(())
    }

    fn validate_uids(&self) -> Result<()> {
        let mut seen = BTreeMap::new();
        let layout_uids =
            self.layouts.iter().flat_map(|(r, l)| l.iter().map(move |(n, l)| (format!("disc/{r}/{n}"), &l.uid)));
        for (path, u) in self.assets.iter().map(|(p, a)| (p.clone(), &a.uid)).chain(layout_uids) {
            if uid::parse(u).is_none() {
                bail!("{path}: {u} is not a uid");
            }
            if let Some(other) = seen.insert(u.clone(), path.clone()) {
                bail!("{path} and {other} have the same uid {u}");
            }
        }
        Ok(())
    }

    /// Every uid a declaration holds, as ids.
    #[must_use]
    pub fn uids(&self) -> std::collections::HashSet<i64> {
        let layouts = self.layouts.values().flat_map(|l| l.values().map(|l| &l.uid));
        self.assets.values().map(|a| &a.uid).chain(layouts).filter_map(|u| uid::parse(u)).collect()
    }

    /// The uid of the declaration at `path` (a group asset or a release's layout asset).
    #[must_use]
    pub fn uid_of(&self, path: &str) -> Option<&str> {
        if let Some(a) = self.assets.get(path) {
            return Some(&a.uid);
        }
        let (release, name) = path.strip_prefix("disc/")?.split_once('/')?;
        Some(&self.layouts.get(release)?.get(name)?.uid)
    }
}

/// Every reference in a declaration's value: a table holding a `uid` and a `res://` `path`.
fn references(value: &toml::Value, out: &mut Vec<Reference>) {
    match value {
        toml::Value::Table(t) => {
            if let (Some(toml::Value::String(uid)), Some(toml::Value::String(path))) = (t.get("uid"), t.get("path"))
                && path.starts_with("res://")
            {
                out.push(Reference { uid: uid.clone(), path: path.clone() });
            }
            t.values().for_each(|v| references(v, out));
        }
        toml::Value::Array(a) => a.iter().for_each(|v| references(v, out)),
        _ => {}
    }
}
