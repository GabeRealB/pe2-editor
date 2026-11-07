//! What a project is set up with, edited in its `project.toml` in place as the stack is (`stack`), each edit a
//! transaction of its history: the game it is of - the layer its game's files fill and where the disc images are - each
//! release's discs, known by their images' sizes and sha1s, the trees mounted into it, and the builds it can be.

use anyhow::{Context, Result, bail};
use std::path::Path;
use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, Value, value};

use crate::history::History;
use crate::stack::{read, write};
use crate::{FILE, Project};

fn table<'a>(document: &'a mut DocumentMut, key: &str) -> Result<&'a mut Table> {
    let item = document.entry(key).or_insert(Item::Table(Table::new()));
    item.as_table_mut().with_context(|| format!("`{key}` in {FILE} is not a table"))
}

/// Set the game the project is of: the layer its game's files fill - one of its layers - and where the disc images are
/// (`roms`, relative to the project), left as it is where not given.
///
/// # Errors
/// The project has no such layer, or `project.toml` cannot be rewritten.
pub fn set_game(project: &Project, layer: &str, roms: Option<&str>, history: &mut History) -> Result<()> {
    anyhow::ensure!(project.layers.iter().any(|l| l.name == layer), "the project has no layer {layer}");
    let mut document = read(&project.root)?;
    let game = table(&mut document, "game")?;
    game.insert("layer", value(layer));
    if let Some(roms) = roms {
        game.insert("roms", value(roms));
    }
    write(&project.root, &document, history, format!("Make {layer} the game layer"))
}

/// Add a release the project is built for, its discs known by their images - each named as its file is, its size and
/// sha1 read off it - in disc order.
///
/// # Errors
/// The project has no game, has the release already, an image cannot be read, or `project.toml` cannot be rewritten.
pub fn add_release(project: &Project, name: &str, images: &[&Path], history: &mut History) -> Result<()> {
    let game = project.game.as_ref().with_context(|| format!("the project has no game ([game] in {FILE})"))?;
    anyhow::ensure!(!game.releases.contains_key(name), "the project has a release {name} already");
    anyhow::ensure!(!images.is_empty(), "a release has discs");
    let discs = crate::parallel(images, |image| -> Result<InlineTable> {
        let file = image.file_name().and_then(|n| n.to_str()).with_context(|| image.display().to_string())?;
        let size = std::fs::metadata(image).with_context(|| image.display().to_string())?.len();
        let mut disc = InlineTable::new();
        disc.insert("name", file.into());
        disc.insert("size", i64::try_from(size)?.into());
        disc.insert("sha1", crate::extract::sha1_file(image)?.into());
        Ok(disc)
    });
    let mut array = Array::new();
    for disc in discs {
        let mut disc = Value::InlineTable(disc?);
        disc.decor_mut().set_prefix("\n    ");
        array.push_formatted(disc);
    }
    array.set_trailing("\n");
    array.set_trailing_comma(true);
    let mut document = read(&project.root)?;
    let releases = table(&mut document, "game")?.entry("releases").or_insert(Item::Table(Table::new()));
    let releases = releases.as_table_mut().context("`game.releases` is not a table")?;
    releases.set_implicit(true);
    let mut release = Table::new();
    release.insert("discs", value(array));
    releases.insert(name, Item::Table(release));
    write(&project.root, &document, history, format!("Add the release {name}"))
}

/// Mount a tree into the project at `at` (`path`, relative to the project or absolute): a files layer at the bottom
/// of the stack, read-only where said.
///
/// # Errors
/// Something of the project is called so already, or `project.toml` cannot be rewritten.
pub fn add_mount(project: &Project, at: &str, path: &str, read_only: bool, history: &mut History) -> Result<()> {
    if project.layers.iter().any(|l| l.name == at) {
        bail!("the project has a layer or mount {at} already");
    }
    let mut document = read(&project.root)?;
    let item = document.entry("mount").or_insert(Item::ArrayOfTables(ArrayOfTables::new()));
    let mounts = item.as_array_of_tables_mut().with_context(|| format!("`mount` in {FILE} is no list of tables"))?;
    let mut mount = Table::new();
    mount.insert("at", value(at));
    mount.insert("path", value(path));
    if read_only {
        mount.insert("read_only", value(true));
    }
    mounts.push(mount);
    write(&project.root, &document, history, format!("Mount {path} at {at}"))
}

/// Add a build the project can be: `[config.<name>]`, a release and the target building it.
///
/// # Errors
/// It has the config already, or `project.toml` cannot be rewritten.
pub fn add_config(
    project: &Project,
    name: &str,
    release: Option<&str>,
    target: Option<&str>,
    history: &mut History,
) -> Result<()> {
    anyhow::ensure!(project.configs.iter().all(|c| c.name != name), "the project has a config {name} already");
    let mut document = read(&project.root)?;
    let configs = table(&mut document, "config")?;
    configs.set_implicit(true);
    let mut config = Table::new();
    if let Some(release) = release {
        config.insert("release", value(release));
    }
    if let Some(target) = target {
        config.insert("target", value(target));
    }
    configs.insert(name, Item::Table(config));
    write(&project.root, &document, history, format!("Add the config {name}"))
}

#[cfg(test)]
mod tests {
    use crate::Project;
    use crate::cache::Cache;
    use crate::history::History;

    #[test]
    fn a_project_is_set_up_by_its_edits_and_reads_back() {
        let dir = std::env::temp_dir().join(format!("pe2-setup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        crate::create(&dir).unwrap();
        let image = dir.join("disc.bin");
        std::fs::write(&image, b"a disc").unwrap();
        let project = || Project::load(&dir).unwrap();
        let lock = crate::lock::Lock::try_acquire(&dir, "a test").unwrap();
        let (mut history, _) = History::open(&Cache::open(&project(), &lock).unwrap()).unwrap();
        crate::stack::add_at(&project(), "game", crate::LayerKind::Assets, true, &mut history).unwrap();
        super::set_game(&project(), "game", Some("roms"), &mut history).unwrap();
        super::add_release(&project(), "USA", &[image.as_path()], &mut history).unwrap();
        super::add_mount(&project(), "packages", "../packages", true, &mut history).unwrap();
        super::add_config(&project(), "usa", Some("USA"), Some("psx"), &mut history).unwrap();
        let made = project();
        let game = made.game.as_ref().unwrap();
        assert_eq!(game.layer, "game");
        let disc = &game.releases["USA"][0];
        assert_eq!((disc.name.as_str(), disc.size), ("disc.bin", 6));
        assert_eq!(disc.sha1, crate::extract::sha1_hex(b"a disc"));
        assert_eq!(made.configs[0].release.as_deref(), Some("USA"));
        assert!(made.layers.iter().any(|l| l.name == "packages" && l.read_only));
        let at = |name: &str| made.layers.iter().position(|l| l.name == name).unwrap();
        assert!(at("game") < at("project"), "the game's layer is below the project's own");
        assert!(super::add_release(&made, "USA", &[image.as_path()], &mut history).is_err());
        drop(lock);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
