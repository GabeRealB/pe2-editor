//! Extracting fonts (`pe2.fonts@faces`, `pe2.fonts@atlases`): an atlas's page and its faces' glyph tables, read off the
//! discs, are read into each face's UFO (`<name>.ufo/`) and the atlas's layout (`layout.toml`), which are checked to
//! make the page and the tables again exactly - through the UFOs, as the project will read them - before anything is
//! written.
//!
//! An atlas declares where its page is read (`from.<release>.at`, an image chunk slot's page, and the texels' `sha1`);
//! a face where its table is (`from.<release>.at`, the executable's disc slot, with the table's `address` and its
//! `sha1`), the table's `count` records from `first`.

use pe2_ext::{Declared, Discs, Extraction, Made, Result, Table, Value, anyhow, bail};
use sha1::{Digest, Sha1};
use std::collections::BTreeMap;

use crate::atlas::{self, FaceSpec, Glyph, Layout};
use crate::encoding::Encoding;
use crate::face;

/// Where a PS-EXE's text starts in its file, and where its load address is in its header.
const EXE_TEXT: usize = 0x800;
const EXE_LOAD: usize = 0x18;

/// A face as an atlas holds it, by its declaration's path.
struct Held {
    path: String,
    entry: Table,
}

/// An atlas as read in a release: its faces' UFOs' files, its layout, and what lies under its glyphs, as its PNG.
type Read = (Vec<BTreeMap<String, Vec<u8>>>, Layout, Option<Vec<u8>>);

struct Fonts {
    atlases: BTreeMap<String, (Table, Vec<Held>)>,
    faces: BTreeMap<String, Table>,
    encodings: BTreeMap<String, Table>,
    /// What an atlas was read into, per release: its faces' UFOs' files and its layout.
    read: BTreeMap<(String, String), Read>,
}

/// The declaration a reference names, by its path: `{ uid, path = "res://<group>/<name>.node" }`.
fn declared_path(reference: &Value) -> Option<&str> {
    let path = reference.get("path")?.as_str()?;
    Some(path.trim_start_matches("res://").trim_end_matches(".node"))
}

pub fn begin(declared: &dyn Declared) -> Result<Box<dyn Extraction>> {
    let mut atlases = BTreeMap::new();
    for (path, table) in declared.declarations("font_atlas") {
        let mut held = Vec::new();
        for entry in table.get("faces").and_then(Value::as_array).into_iter().flatten() {
            let entry = entry.as_table().ok_or_else(|| anyhow!("{path}: a face is not a table"))?;
            let face =
                entry.get("font").and_then(declared_path).ok_or_else(|| anyhow!("{path}: a face names no font"))?;
            held.push(Held { path: face.to_string(), entry: entry.clone() });
        }
        atlases.insert(path, (table, held));
    }
    let faces = declared.declarations("font").into_iter().collect();
    let encodings = declared.declarations("encoding").into_iter().collect();
    Ok(Box::new(Fonts { atlases, faces, encodings, read: BTreeMap::new() }))
}

fn sha1_hex(bytes: &[u8]) -> String {
    Sha1::digest(bytes).iter().fold(String::new(), |mut s, b| {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Where a declaration is read in a release: its location table, the first it lists.
fn located<'a>(table: &'a Table, release: &str) -> Result<(&'a Table, &'a Table)> {
    let from = table
        .get("from")
        .and_then(|f| f.get(release))
        .and_then(Value::as_table)
        .ok_or_else(|| anyhow!("not in {release}"))?;
    let at = from
        .get("at")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_table)
        .ok_or_else(|| anyhow!("read from nowhere"))?;
    Ok((from, at))
}

fn check(bytes: &[u8], from: &Table, what: &str) -> Result<()> {
    let want = from.get("sha1").and_then(Value::as_str).ok_or_else(|| anyhow!("its {what} has no sha1"))?;
    let got = sha1_hex(bytes);
    if got != want {
        bail!("its {what} has sha1 {got}, the declaration {want}");
    }
    Ok(())
}

impl Fonts {
    /// A face's spec on an atlas and its table, read from the release's executable.
    fn face(&self, held: &Held, release: &str, discs: &mut dyn Discs) -> Result<(FaceSpec, Vec<u8>, Table)> {
        let path = &held.path;
        let table = self.faces.get(path).ok_or_else(|| anyhow!("{path}: no such font"))?;
        let name = path.rsplit('/').next().unwrap_or(path).to_string();
        let integer =
            |t: &Table, key: &str| t.get(key).and_then(Value::as_integer).ok_or_else(|| anyhow!("{path}: no {key}"));
        let encoding =
            table.get("encoding").and_then(declared_path).ok_or_else(|| anyhow!("{path}: it names no encoding"))?;
        let encoding_table = self.encodings.get(encoding).ok_or_else(|| anyhow!("{path}: no encoding {encoding}"))?;
        let mut encoding = Encoding::of(encoding_table).map_err(|e| anyhow!("{path}: {e}"))?;
        crate::apply_characters(&mut encoding, table).map_err(|e| anyhow!("{path}: {e}"))?;
        let (missing, nothing) = crate::binding(table).map_err(|e| anyhow!("{path}: {e}"))?;
        let format = crate::format_of(table).map_err(|e| anyhow!("{path}: {e}"))?;
        let band =
            held.entry.get("band").and_then(Value::as_array).ok_or_else(|| anyhow!("{path}: its band is not set"))?;
        let row = |i: usize| {
            band.get(i)
                .and_then(Value::as_integer)
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| anyhow!("{path}: its band is two rows"))
        };
        let spec = FaceSpec {
            name,
            first: u32::try_from(integer(table, "first")?)?,
            count: usize::try_from(integer(table, "count")?)?,
            bias: u8::try_from(integer(&held.entry, "bias")?.rem_euclid(256))?,
            band: (row(0)?, row(1)?),
            encoding,
            missing,
            nothing,
            only: None,
            format,
        };
        let (from, at) = located(table, release).map_err(|e| anyhow!("{path}: {e}"))?;
        let length = spec.count * spec.format.size();
        // A credits document's glyph table, from its `start`-th record: a resource of a bundle; else a table of the
        // executable's, by its address.
        let bytes = if let Some(resource) = from.get("resource") {
            let resource = resource
                .as_integer()
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| anyhow!("{path}: its resource is not a number"))?;
            let bundle = discs.read(at)?;
            let resources = formats::bundle::resources(&bundle);
            let (_, document) =
                resources.get(&resource).ok_or_else(|| anyhow!("{path}: its bundle has no resource {resource}"))?;
            if document.get(..4) != Some(b"STF1".as_slice()) {
                bail!("{path}: its resource {resource} is no credits document");
            }
            let glyphs = document
                .get(0xC..0x10)
                .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .ok_or_else(|| anyhow!("{path}: a short document"))?;
            let first =
                from.get("start").and_then(Value::as_integer).and_then(|n| usize::try_from(n).ok()).unwrap_or(0);
            let start = usize::try_from(glyphs)? + first * spec.format.size();
            document.get(start..start + length).ok_or_else(|| anyhow!("{path}: past its document's end"))?.to_vec()
        } else {
            let exe = discs.read(at)?;
            let load = exe
                .get(EXE_LOAD..EXE_LOAD + 4)
                .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .ok_or_else(|| anyhow!("{path}: not an executable"))?;
            let address = from
                .get("address")
                .and_then(Value::as_integer)
                .and_then(|a| u32::try_from(a).ok())
                .ok_or_else(|| anyhow!("{path}: no address"))?;
            let start = usize::try_from(
                address.checked_sub(load).ok_or_else(|| anyhow!("{path}: its address is before the executable"))?,
            )? + EXE_TEXT;
            exe.get(start..start + length).ok_or_else(|| anyhow!("{path}: past the executable's end"))?.to_vec()
        };
        check(&bytes, from, "table").map_err(|e| anyhow!("{path}: {e}"))?;
        Ok((spec, bytes, table.clone()))
    }

    /// An atlas read into its faces' UFOs and its layout, held to make its page and tables again.
    fn read_atlas(&mut self, path: &str, release: &str, discs: &mut dyn Discs) -> Result<()> {
        let key = (path.to_string(), release.to_string());
        if self.read.contains_key(&key) {
            return Ok(());
        }
        let (table, held) = self.atlases.get(path).ok_or_else(|| anyhow!("{path}: no such atlas"))?;
        let mut page = crate::page_spec(table)?;
        let rule = crate::flow_of(table, &page)?;
        // Its pages, one below another: each where `from.<release>.pages` says, or the one where `from` does.
        let from = table
            .get("from")
            .and_then(|f| f.get(release))
            .and_then(Value::as_table)
            .ok_or_else(|| anyhow!("not in {release}"))?;
        let sheets: Vec<&Table> = match from.get("pages").and_then(Value::as_array) {
            Some(sheets) => sheets.iter().filter_map(Value::as_table).collect(),
            None => vec![from],
        };
        if sheets.len() != page.pages {
            bail!("it is {} pages, and is read from {}", page.pages, sheets.len());
        }
        let mut texels: Vec<u8> = Vec::with_capacity(page.width * page.rows());
        for (i, sheet) in sheets.iter().enumerate() {
            let at = sheet
                .get("at")
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .and_then(Value::as_table)
                .ok_or_else(|| anyhow!("its page {i} is read from nowhere"))?;
            let payload = discs.read(at)?;
            let pages = codecs::texture::pages(&payload).map_err(|e| anyhow!("its page {i}: {e}"))?;
            let index = at.get("page").and_then(Value::as_integer).and_then(|p| usize::try_from(p).ok()).unwrap_or(0);
            let stored = pages.get(index).ok_or_else(|| anyhow!("its page {i}'s chunk has no page {index}"))?;
            let one: Vec<u8> = codecs::texture::texels(stored, 4, (0, 0, page.width, page.height))
                .into_iter()
                .map(|t| u8::try_from(t).unwrap_or(0))
                .collect();
            check(&one, sheet, &format!("page {i}"))?;
            texels.extend(one);
        }
        let mut specs = Vec::new();
        let mut tables = Vec::new();
        let mut faces = Vec::new();
        for face in held {
            let (spec, bytes, settings) = self.face(face, release, discs)?;
            specs.push(spec);
            tables.push(bytes);
            faces.push(settings);
        }
        let views: Vec<&[u8]> = tables.iter().map(Vec::as_slice).collect();
        let (glyphs, placed, under) = atlas::decode(&page, &texels, &specs, &views).map_err(|e| anyhow!(e))?;
        page.under.clone_from(&under);
        // Generated, it holds its text's characters alone, pinned where its rule does not place them as the disc has them.
        let layout = match &rule {
            Some(rule) => {
                if let Some(text) = &rule.text {
                    let used: std::collections::BTreeSet<char> = atlas::first_use(text).into_iter().collect();
                    for spec in &mut specs {
                        spec.only = Some(used.clone());
                    }
                }
                atlas::pins_for(&page, &specs, &glyphs, rule, &placed, &views).map_err(|e| anyhow!(e))?
            }
            None => placed,
        };
        // Through the UFOs, as the project reads them: what they make must be the disc's.
        let mut ufos = Vec::new();
        let mut again: Vec<Vec<Glyph>> = Vec::new();
        for ((spec, glyphs), settings) in specs.iter().zip(&glyphs).zip(&faces) {
            let text = |key: &str| settings.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
            let kerning =
                settings.get("kerning").and_then(Value::as_integer).and_then(|k| u8::try_from(k).ok()).unwrap_or(0);
            let files = face::ufo_files(glyphs, &text("family"), &text("style"), kerning);
            let font = codecs::ufo::read(&files).map_err(|e| anyhow!("{}: {e}", spec.name))?;
            again.push(
                face::from_ufo(&font, &crate::raster::Raster::default()).map_err(|e| anyhow!("{}: {e}", spec.name))?,
            );
            ufos.push(files);
        }
        let layout = crate::layout_of(&crate::layout_text(&layout))?;
        let under_png =
            under.map(|texels| face::art_png(&atlas::Art { width: page.width, height: page.rows(), texels }));
        if let Some(png) = &under_png {
            page.under = Some(face::art_of(png).map_err(|e| anyhow!("under.png: {e}"))?.texels);
        }
        let placing = match &rule {
            Some(rule) => atlas::flow(&page, &specs, &again, rule, &layout).map_err(|e| anyhow!(e))?,
            None => layout.clone(),
        };
        let (made, made_tables) = atlas::generate(&page, &specs, &again, &placing).map_err(|e| anyhow!(e))?;
        let differ = made.iter().zip(&texels).filter(|(a, b)| a != b).count();
        if differ != 0 {
            bail!("its faces and layout make {differ} texels of its page otherwise");
        }
        for ((spec, made), stored) in specs.iter().zip(&made_tables).zip(&tables) {
            if made != stored {
                let size = spec.format.size();
                let bad = made.chunks(size).zip(stored.chunks(size)).filter(|(a, b)| a != b).count();
                bail!("{}: its glyphs make {bad} records of its table otherwise", spec.name);
            }
        }
        self.read.insert(key, (ufos, layout, under_png));
        Ok(())
    }

    /// The atlas holding a face, and the face's place in it.
    fn atlas_of(&self, face: &str) -> Option<(String, usize)> {
        self.atlases
            .iter()
            .find_map(|(path, (_, held))| Some((path.clone(), held.iter().position(|h| h.path == face)?)))
    }
}

impl Extraction for Fonts {
    fn extract(&mut self, path: &str, release: &str, discs: &mut dyn Discs) -> Result<Made> {
        if self.atlases.contains_key(path) {
            self.read_atlas(path, release, discs)?;
            let (_, layout, under) = &self.read[&(path.to_string(), release.to_string())];
            let mut made = vec![("layout.toml".to_string(), crate::layout_text(layout).into_bytes())];
            made.extend(under.iter().map(|png| ("under.png".to_string(), png.clone())));
            return Ok(made);
        }
        let (atlas, at) = self.atlas_of(path).ok_or_else(|| anyhow!("{path}: no atlas holds it"))?;
        self.read_atlas(&atlas, release, discs)?;
        let (ufos, _, _) = &self.read[&(atlas, release.to_string())];
        let name = path.rsplit('/').next().unwrap_or(path);
        Ok(ufos[at].iter().map(|(file, bytes)| (format!("{name}.ufo/{file}"), bytes.clone())).collect())
    }
}
