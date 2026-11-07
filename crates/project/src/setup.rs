//! What a project is set up with, edited in its `project.toml` in place as the stack is (`stack`), each edit a
//! transaction of its history: its generated layers, each filling another's nodes by its plan, the trees mounted into
//! it - unmanaged ones read by path alone, each where this machine says (`project.local.toml`) - and the builds it can
//! be.

use anyhow::{Context, Result, bail};
use std::path::Path;
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};

use crate::history::History;
use crate::stack::{read, write};
use crate::{FILE, Project};

fn table<'a>(document: &'a mut DocumentMut, key: &str) -> Result<&'a mut Table> {
    let item = document.entry(key).or_insert(Item::Table(Table::new()));
    item.as_table_mut().with_context(|| format!("`{key}` in {FILE} is not a table"))
}

/// Add a generated layer, right above the layer it `fills`: what its plan (`<path>.plan.jsonl`, beside its folder)
/// writes, the files of that layer's nodes. Its folder is what the plan made, this machine's, so the project's
/// `.gitignore` leaves it out.
///
/// # Errors
/// The name is taken, the layer it fills is not one of the project's, or the files cannot be written.
pub fn add_generated(project: &Project, name: &str, fills: &str, history: &mut History) -> Result<()> {
    crate::stack::check_name(project, name)?;
    anyhow::ensure!(
        project.layers.iter().any(|l| l.name == fills && l.kind == crate::LayerKind::Assets),
        "the project has no asset layer {fills} to fill"
    );
    let mut document = read(&project.root)?;
    let item = document.entry("layer").or_insert(Item::ArrayOfTables(ArrayOfTables::new()));
    let layers = item.as_array_of_tables_mut().with_context(|| format!("`layer` in {FILE} is no list of tables"))?;
    let path = format!("{}/{name}", crate::LAYERS);
    let mut layer = Table::new();
    layer.insert("name", value(name));
    layer.insert("path", value(&path));
    layer.insert("kind", value("generated"));
    layer.insert("fills", value(fills));
    // An array of tables takes no insert: rebuilt with it after the layer it fills.
    let mut all: Vec<Table> = layers.iter().cloned().collect();
    let at = all.iter().position(|t| t.get("name").and_then(Item::as_str) == Some(fills)).map_or(all.len(), |i| i + 1);
    all.insert(at, layer);
    layers.clear();
    for table in all {
        layers.push(table);
    }
    let mut tx = history.begin(format!("Add the layer {name}, filling {fills}"));
    tx.make_dir(&project.root.join(&path))?;
    tx.write(&project.root.join(FILE), document.to_string().as_bytes())?;
    ignored(&mut tx, &project.root, &format!("/{path}/"))?;
    tx.commit().map(drop)
}

/// Have the project's `.gitignore` leave out a path, where it does not already (a transaction's writes are made as
/// they come, so a second entry reads the first).
fn ignored(tx: &mut crate::history::Transaction<'_>, root: &Path, entry: &str) -> Result<()> {
    let file = root.join(".gitignore");
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    if text.lines().any(|line| line.trim() == entry) {
        return Ok(());
    }
    let separator = if text.is_empty() || text.ends_with('\n') { "" } else { "\n" };
    tx.write(&file, format!("{text}{separator}{entry}\n").as_bytes())
}

/// Where an unmanaged mount is on this machine: written to `project.local.toml` (`[mounts] <at> = "<path>"`), which is
/// this machine's alone and no step of the project's history.
///
/// # Errors
/// The project has no such unmanaged mount, or the file cannot be written.
pub fn locate(project: &Project, at: &str, path: &str) -> Result<()> {
    anyhow::ensure!(project.unmanaged.iter().any(|m| m.at == at), "the project has no unmanaged mount {at}");
    let file = project.root.join(crate::LOCAL);
    let mut document: DocumentMut = std::fs::read_to_string(&file).unwrap_or_default().parse()?;
    table(&mut document, "mounts")?.insert(at, value(path));
    std::fs::write(&file, document.to_string()).with_context(|| file.display().to_string())
}

/// Mount a tree into the project at `at` (`path`, relative to the project or absolute): a files layer at the bottom
/// of the stack, read-only where said. An unmanaged one is not indexed - read by path alone - and is where it is on
/// each machine (`locate`; the folder of its name by default), which the project's `.gitignore` leaves out.
///
/// # Errors
/// Something of the project is called so already, a managed mount has no path, or `project.toml` cannot be rewritten.
pub fn add_mount(
    project: &Project,
    at: &str,
    path: Option<&str>,
    read_only: bool,
    unmanaged: bool,
    history: &mut History,
) -> Result<()> {
    if project.layers.iter().any(|l| l.name == at) || project.unmanaged.iter().any(|m| m.at == at) {
        bail!("the project has a layer or mount {at} already");
    }
    anyhow::ensure!(unmanaged || path.is_some(), "a mount says where its tree is, but for an unmanaged one");
    let mut document = read(&project.root)?;
    let item = document.entry("mount").or_insert(Item::ArrayOfTables(ArrayOfTables::new()));
    let mounts = item.as_array_of_tables_mut().with_context(|| format!("`mount` in {FILE} is no list of tables"))?;
    let mut mount = Table::new();
    mount.insert("at", value(at));
    if let Some(path) = path {
        mount.insert("path", value(path));
    }
    if read_only {
        mount.insert("read_only", value(true));
    }
    if unmanaged {
        mount.insert("unmanaged", value(true));
    }
    mounts.push(mount);
    let mut tx = history.begin(format!("Mount {at}"));
    tx.write(&project.root.join(FILE), document.to_string().as_bytes())?;
    if unmanaged {
        ignored(&mut tx, &project.root, &format!("/{at}/"))?;
        ignored(&mut tx, &project.root, &format!("/{}", crate::LOCAL))?;
    }
    tx.commit().map(drop)
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
        let project = || Project::load(&dir).unwrap();
        let lock = crate::lock::Lock::try_acquire(&dir, "a test").unwrap();
        let (mut history, _) = History::open(&Cache::open(&project(), &lock).unwrap()).unwrap();
        crate::stack::add_at(&project(), "game", crate::LayerKind::Assets, true, &mut history).unwrap();
        super::add_generated(&project(), "game-files", "game", &mut history).unwrap();
        super::add_mount(&project(), "roms", None, false, true, &mut history).unwrap();
        super::add_mount(&project(), "packages", Some("../packages"), true, false, &mut history).unwrap();
        super::add_config(&project(), "usa", Some("USA"), Some("psx"), &mut history).unwrap();
        let made = project();
        let files = made.layer("game-files").unwrap();
        assert_eq!(
            (files.kind, files.fills.as_deref(), files.read_only),
            (crate::LayerKind::Generated, Some("game"), true)
        );
        assert_eq!(files.plan(), dir.join(".layers/game-files.plan.jsonl"));
        assert_eq!(made.unmanaged[0].path, dir.join("roms"));
        assert_eq!(made.resolve("res://roms/a.bin").unwrap(), dir.join("roms/a.bin"));
        assert_eq!(made.configs[0].release.as_deref(), Some("USA"));
        assert!(made.layers.iter().any(|l| l.name == "packages" && l.read_only));
        let at = |name: &str| made.layers.iter().position(|l| l.name == name).unwrap();
        assert!(at("game") < at("project"), "the game's layer is below the project's own");
        assert_eq!(at("game-files"), at("game") + 1, "a generated layer is right above the layer it fills");
        let ignored = std::fs::read_to_string(dir.join(".gitignore")).unwrap();
        assert!(ignored.contains("/.layers/game-files/") && ignored.contains("/roms/"), "{ignored}");
        assert!(ignored.contains("/project.local.toml"), "{ignored}");
        // Where the discs are is this machine's.
        super::locate(&made, "roms", "/elsewhere").unwrap();
        assert_eq!(project().unmanaged[0].path, std::path::Path::new("/elsewhere"));
        assert!(super::add_generated(&project(), "more", "nothing", &mut history).is_err());
        drop(lock);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
