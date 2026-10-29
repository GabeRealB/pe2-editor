//! The extension host: loads the extensions, registers what they register, and answers what a kind has - its icon,
//! fields, picture, thumbnail, views - for the editor, `pe2-cli` and the agents' server alike.
//!
//! Loading checks what the SDK cannot: one extension an id (a second is refused), each one's requirements met by the
//! version loaded (`pe2.images@^0.1`), registration in dependency order, cycles refused; an extension whose
//! requirement is missing, too old or itself disabled is disabled, and so are its dependents, each with the reason. What
//! one registers is checked as well: a role only on its own kinds or its dependencies', an alias only from a built-in,
//! a name only once. The extensions are linked in for now (`extensions::builtin`), so loading is calling their entry
//! points; a component's would be instantiated first.

use anyhow::{Context, Result};
use pe2_ext::{Asset, Extension, Field, Icon, Image, KindDef, Registry, RoleDef, Source, Tint, ViewDef};
use std::collections::{HashMap, HashSet};

use crate::index;

/// A loaded extension.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Loaded {
    pub id: String,
    pub version: String,
}

/// The extensions loaded, and what they registered.
#[derive(Default)]
pub struct Extensions {
    pub loaded: Vec<Loaded>,
    /// What was refused or disabled, and why.
    pub problems: Vec<String>,
    kinds: HashMap<String, (KindDef, usize)>,
    roles: HashMap<String, RoleDef>,
    /// Bare names declarations write, to the qualified names of the built-ins registering them.
    kind_aliases: HashMap<String, String>,
    role_aliases: HashMap<String, String>,
}

/// A version's major, minor and patch numbers.
fn semver(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.split('.').map(|p| p.parse::<u64>().ok());
    Some((parts.next()??, parts.next().unwrap_or(Some(0))?, parts.next().unwrap_or(Some(0))?))
}

/// Whether `version` meets a caret requirement (`^0.1`, `0.1.2`): at least it, and the same major (the same minor
/// below 1.0).
fn satisfies(version: &str, requirement: &str) -> bool {
    let (Some(have), Some(want)) = (semver(version), semver(requirement.trim_start_matches('^'))) else {
        return false;
    };
    let compatible = if want.0 == 0 { have.0 == 0 && have.1 == want.1 } else { have.0 == want.0 };
    compatible && have >= want
}

/// A requirement's id and the versions it accepts.
fn requirement(text: &str) -> (&str, Option<&str>) {
    match text.split_once('@') {
        Some((id, version)) => (id, Some(version)),
        None => (text, None),
    }
}

impl Extensions {
    /// Load `extensions`, built-ins (`builtin`) or a project's own.
    #[must_use]
    pub fn load(extensions: Vec<Extension>, builtin: bool) -> Extensions {
        let mut out = Extensions::default();
        let mut pending: Vec<Extension> = vec![];
        for extension in extensions {
            if pending.iter().any(|e| e.id == extension.id) {
                out.problems.push(format!("{}: another extension has this id; the second is not loaded", extension.id));
            } else {
                pending.push(extension);
            }
        }
        let ids: HashSet<&str> = pending.iter().map(|e| e.id).collect();
        let mut disabled: HashSet<&'static str> = HashSet::new();
        loop {
            let mut progress = false;
            for extension in &pending {
                if disabled.contains(extension.id) || out.loaded.iter().any(|l| l.id == extension.id) {
                    continue;
                }
                let mut ready = true;
                for text in extension.requires {
                    let (id, version) = requirement(text);
                    let blocked = if !ids.contains(id) {
                        Some(format!("it needs {text}, which is not installed"))
                    } else if disabled.contains(id) {
                        Some(format!("it needs {id}, which is disabled"))
                    } else if let Some(loaded) = out.loaded.iter().find(|l| l.id == id) {
                        version.filter(|v| !satisfies(&loaded.version, v)).map(|v| {
                            format!("it needs {id} {v}, and {} is installed", loaded.version)
                        })
                    } else {
                        ready = false;
                        None
                    };
                    if let Some(why) = blocked {
                        out.problems.push(format!("{} is disabled: {why}", extension.id));
                        disabled.insert(extension.id);
                        ready = false;
                        progress = true;
                        break;
                    }
                }
                if !ready || disabled.contains(extension.id) {
                    continue;
                }
                progress = true;
                if let Err(e) = out.register(extension, builtin) {
                    out.problems.push(format!("{} is disabled: {e:#}", extension.id));
                    disabled.insert(extension.id);
                }
            }
            if !progress {
                break;
            }
        }
        for extension in &pending {
            if !disabled.contains(extension.id) && !out.loaded.iter().any(|l| l.id == extension.id) {
                out.problems.push(format!("{} is disabled: its requirements form a cycle", extension.id));
            }
        }
        out
    }

    /// Call an extension's entry point and take what it registered, checked.
    fn register(&mut self, extension: &Extension, builtin: bool) -> Result<()> {
        let mut registry = Registry::new(extension.id);
        (extension.register)(&mut registry).context("its registration failed")?;
        let (kinds, roles) = registry.into_parts();
        let dependencies: Vec<&str> = extension.requires.iter().map(|r| requirement(r).0).collect();
        let mut seen = HashSet::new();
        for name in kinds.iter().map(|k| &k.name).chain(roles.iter().map(|r| &r.name)) {
            anyhow::ensure!(name.extension() == extension.id, "{name} is not in its namespace");
            anyhow::ensure!(seen.insert(name.clone()), "it registers {name} twice");
        }
        for role in &roles {
            let owner = role.kind.extension();
            let reachable = owner == extension.id && kinds.iter().any(|k| k.name == role.kind)
                || dependencies.contains(&owner) && self.kinds.contains_key(role.kind.as_str());
            anyhow::ensure!(reachable, "{} is on {}, which neither it nor what it requires registers", role.name, role.kind);
        }
        let aliases = kinds.iter().filter_map(|k| k.alias.as_ref()).chain(roles.iter().filter_map(|r| r.alias.as_ref()));
        for alias in aliases {
            anyhow::ensure!(builtin, "only a built-in extension names a kind or role bare ({alias})");
        }
        let index = self.loaded.len();
        self.loaded.push(Loaded { id: extension.id.to_string(), version: extension.version.to_string() });
        for kind in kinds {
            if let Some(alias) = &kind.alias {
                self.kind_aliases.insert(alias.clone(), kind.name.to_string());
            }
            self.kinds.insert(kind.name.to_string(), (kind, index));
        }
        for role in roles {
            if let Some(alias) = &role.alias {
                self.role_aliases.insert(alias.clone(), role.name.to_string());
            }
            self.roles.insert(role.name.to_string(), role);
        }
        Ok(())
    }

    /// The kind a declaration names, qualified or by a built-in's bare alias.
    fn kind_named(&self, declared: &str) -> Option<&(KindDef, usize)> {
        self.kinds.get(self.kind_aliases.get(declared).map_or(declared, String::as_str))
    }

    /// An asset's kind, where an extension registers it.
    #[must_use]
    pub fn kind_of(&self, asset: &index::Asset) -> Option<&KindDef> {
        self.kind_named(asset.kind.as_deref()?).map(|(kind, _)| kind)
    }

    /// An asset's role, where one is registered on its kind.
    #[must_use]
    pub fn role_of(&self, asset: &index::Asset) -> Option<&RoleDef> {
        let kind = self.kind_of(asset)?;
        let declared = asset.role.as_deref()?;
        let role = self.roles.get(self.role_aliases.get(declared).map_or(declared, String::as_str))?;
        (role.kind == kind.name).then_some(role)
    }

    /// An asset as its extensions see it: lent, with its kind and role qualified.
    #[must_use]
    pub fn lend<'a>(&'a self, asset: &'a index::Asset) -> Asset<'a> {
        let kind = self.kind_of(asset).map(|k| k.name.as_str());
        let role = self.role_of(asset).map(|r| r.name.as_str());
        Asset::new(asset, kind, role)
    }

    /// A kind's icon and its tint, by the name a declaration writes.
    #[must_use]
    pub fn icon(&self, declared: &str) -> Option<(Icon, Tint)> {
        self.kind_named(declared)?.0.icon
    }

    /// Whether an asset has a picture.
    #[must_use]
    pub fn has_picture(&self, asset: &index::Asset) -> bool {
        self.kind_of(asset).and_then(|k| k.picture.as_ref()).is_some_and(|p| p.has(&self.lend(asset)))
    }

    /// An asset's looks, by name.
    #[must_use]
    pub fn looks(&self, asset: &index::Asset) -> Vec<String> {
        self.kind_of(asset).and_then(|k| k.picture.as_ref()).map_or_default(|p| p.looks(&self.lend(asset)))
    }

    /// An asset's picture in one of its looks; none when it has none.
    ///
    /// # Errors
    /// Its extension could not draw it.
    pub fn picture(&self, asset: &index::Asset, look: usize) -> Option<Result<Image>> {
        let picture = self.kind_of(asset)?.picture.as_ref()?;
        let lent = self.lend(asset);
        picture.has(&lent).then(|| picture.draw(&lent, look))
    }

    /// Whether an asset's kind makes thumbnails.
    #[must_use]
    pub fn has_thumbnail(&self, asset: &index::Asset) -> bool {
        self.kind_of(asset).is_some_and(|k| k.thumbnail.is_some())
    }

    /// An asset's thumbnail, at most `side` pixels on its longer side; none when its kind makes none, or it has none.
    #[must_use]
    pub fn thumbnail(&self, asset: &index::Asset, side: usize) -> Option<Result<Image>> {
        (self.kind_of(asset)?.thumbnail.as_ref()?)(&self.lend(asset), side)
    }

    /// An artifact's generator: its extension's id and version, and its name.
    fn generator(&self, index: usize, name: &str) -> String {
        let loaded = &self.loaded[index];
        format!("{}@{name} {}", loaded.id, loaded.version)
    }

    /// The artifact name an asset's thumbnail is kept under, before it is made: its extension's.
    #[must_use]
    pub fn thumbnail_name(&self, asset: &index::Asset) -> Option<String> {
        let (_, index) = self.kind_named(asset.kind.as_deref()?)?;
        Some(format!("{}@thumbnail.png", self.loaded[*index].id))
    }

    /// The generator an asset's thumbnail is kept under as an artifact: `pe2.images@thumbnail 0.1.0`, so that a new
    /// version of its extension makes it again.
    #[must_use]
    pub fn thumbnail_generator(&self, asset: &index::Asset) -> Option<String> {
        let (_, index) = self.kind_named(asset.kind.as_deref()?)?;
        Some(self.generator(*index, "thumbnail"))
    }

    /// The fields an asset's kind and role describe, the role's first.
    #[must_use]
    pub fn fields(&self, asset: &index::Asset) -> Vec<&Field> {
        let role = self.role_of(asset).map_or(&[][..], |r| r.fields.as_slice());
        let kind = self.kind_of(asset).map_or(&[][..], |k| k.fields.as_slice());
        role.iter().chain(kind).collect()
    }

    /// The views of an asset: its kind's, then its role's.
    #[must_use]
    pub fn views(&self, asset: &index::Asset) -> Vec<&ViewDef> {
        let kind = self.kind_of(asset).map_or(&[][..], |k| k.views.as_slice());
        let role = self.role_of(asset).map_or(&[][..], |r| r.views.as_slice());
        kind.iter().chain(role).collect()
    }
}

impl Source for index::Asset {
    fn path(&self) -> &str {
        &self.path
    }

    fn uid(&self) -> Option<&str> {
        self.uid.as_deref()
    }

    fn fields(&self) -> &toml::Table {
        index::Asset::fields(self)
    }

    fn has_file(&self, name: &str) -> bool {
        self.files.iter().any(|f| f.name == name)
    }

    fn read(&self, name: &str) -> Result<Vec<u8>> {
        let file = self.files.iter().find(|f| f.name == name).with_context(|| format!("{}: no file {name}", self.path))?;
        std::fs::read(&file.path).with_context(|| file.path.display().to_string())
    }
}

/// An image as PNG.
///
/// # Errors
/// It cannot be encoded.
pub fn png(image: &Image) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let (width, height) = (u32::try_from(image.width)?, u32::try_from(image.height)?);
    let mut encoder = png::Encoder::new(&mut out, width.max(1), height.max(1));
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    if image.rgba.is_empty() {
        writer.write_image_data(&[0; 4])?;
    } else {
        writer.write_image_data(&image.rgba)?;
    }
    writer.finish()?;
    Ok(out)
}

/// An image from PNG, as `png` writes it.
///
/// # Errors
/// It is not an 8-bit RGBA PNG.
pub fn from_png(bytes: &[u8]) -> Result<Image> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes)).read_info()?;
    let mut rgba = vec![0; reader.output_buffer_size().unwrap_or(0)];
    let info = reader.next_frame(&mut rgba)?;
    anyhow::ensure!(info.color_type == png::ColorType::Rgba && info.bit_depth == png::BitDepth::Eight, "not 8-bit RGBA");
    rgba.truncate(info.buffer_size());
    Ok(Image { width: info.width as usize, height: info.height as usize, rgba })
}

#[cfg(test)]
#[expect(clippy::unnecessary_wraps, reason = "entry points may fail; these do not")]
mod tests {
    use super::{Extensions, satisfies};
    use pe2_ext::{Extension, Name, Registry, Result};

    fn images(r: &mut Registry) -> Result<()> {
        let image = r.kind("image").alias("image").register();
        r.role(&image, "atlas").alias("atlas").register();
        Ok(())
    }

    fn fonts(r: &mut Registry) -> Result<()> {
        r.role(&Name::parse("test.images@image").unwrap(), "font").register();
        Ok(())
    }

    fn stray(r: &mut Registry) -> Result<()> {
        r.role(&Name::parse("test.images@image").unwrap(), "font").register();
        Ok(())
    }

    fn nothing(_: &mut Registry) -> Result<()> {
        Ok(())
    }

    fn extension(id: &'static str, version: &'static str, requires: &'static [&'static str], register: fn(&mut Registry) -> Result<()>) -> Extension {
        Extension { id, version, requires, register }
    }

    #[test]
    fn loads_in_dependency_order_and_disables_what_cannot_load() {
        let loaded = Extensions::load(
            vec![
                // Before what it needs: loaded after it.
                extension("test.fonts", "0.1.0", &["test.images@^0.2"], fonts),
                extension("test.images", "0.2.3", &[], images),
                extension("test.images", "9.0.0", &[], nothing),
                extension("test.old", "0.1.0", &["test.images@^0.3"], nothing),
                extension("test.missing", "0.1.0", &["test.nowhere"], nothing),
                extension("test.chained", "0.1.0", &["test.missing"], nothing),
                extension("test.stray", "0.1.0", &[], stray),
                extension("test.a", "0.1.0", &["test.b"], nothing),
                extension("test.b", "0.1.0", &["test.a"], nothing),
            ],
            true,
        );
        let ids: Vec<&str> = loaded.loaded.iter().map(|l| l.id.as_str()).collect();
        assert_eq!(ids, ["test.images", "test.fonts"]);
        let problems = loaded.problems.join("\n");
        for expected in [
            "test.images: another extension has this id",
            "test.old is disabled: it needs test.images ^0.3, and 0.2.3 is installed",
            "test.missing is disabled: it needs test.nowhere, which is not installed",
            "test.chained is disabled: it needs test.missing, which is disabled",
            "test.stray is disabled: test.stray@font is on test.images@image",
            "test.a is disabled: its requirements form a cycle",
        ] {
            assert!(problems.contains(expected), "{expected}\n---\n{problems}");
        }
        assert_eq!(loaded.kind_aliases["image"], "test.images@image");
        assert!(loaded.roles.contains_key("test.fonts@font"));
    }

    #[test]
    fn only_builtins_alias() {
        let loaded = Extensions::load(vec![extension("mod.images", "0.1.0", &[], images)], false);
        assert_eq!(loaded.loaded, []);
        assert!(loaded.problems[0].contains("only a built-in extension names a kind or role bare"), "{:?}", loaded.problems);
    }

    #[test]
    fn caret_requirements() {
        assert!(satisfies("0.1.4", "^0.1"));
        assert!(satisfies("0.1.4", "0.1.2"));
        assert!(!satisfies("0.1.1", "0.1.2"));
        assert!(!satisfies("0.2.0", "^0.1"));
        assert!(satisfies("1.4.0", "^1.2"));
        assert!(!satisfies("2.0.0", "^1.2"));
    }
}
