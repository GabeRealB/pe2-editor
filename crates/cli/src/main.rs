//! `pe2-cli`: the tool crates behind one multi-call binary.
//! It dispatches on its subcommand (`pe2-cli project extract`) or, run under a tool's name (a link in the editor's
//! own `bin/`), on that name (`project extract`).

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use pe2_ext::events::{Build, Clean, Event};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "pe2-cli", about = "The Parasite Eve II editor's tools")]
struct Cli {
    /// Run every unsandboxed extension in this run, asking nothing and keeping nothing
    #[arg(long, global = true)]
    trust_everything: bool,
    #[command(subcommand)]
    tool: Tool,
}

#[derive(Subcommand)]
enum Tool {
    /// Native forms to editable ones
    Convert {
        #[command(subcommand)]
        command: ConvertCommand,
    },
    /// Projects: their game layer, extracted from the discs
    Project {
        #[command(subcommand)]
        command: ProjectCommand,
    },
    /// What extensions fetched onto this computer, kept for every project: listed, and removed. What no project uses
    /// for 30 days is removed by itself
    Fetched {
        #[command(subcommand)]
        command: FetchedCommand,
    },
    /// Writing extensions
    Extension {
        #[command(subcommand)]
        command: AuthorCommand,
    },
    /// Asset uids
    Uid {
        #[command(subcommand)]
        command: UidCommand,
    },
    /// Serve a project to agents over the Model Context Protocol, on stdio: through the editor while it has the
    /// project open, else under the project lock, taken for each request alone
    Mcp {
        /// The project's directory
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
}

#[derive(Subcommand)]
enum FetchedCommand {
    /// What the store holds: each package, its size, when it was last used, and whether the project uses it
    List {
        /// The project's directory, whose extensions' packages are marked
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// Remove from the store: with nothing named, what is due to go now - unused for 30 days and not the project's,
    /// and what fetches left unfinished; or what is named, by key (its start) or name. Each is fetched again where
    /// a project needs it
    Clean {
        /// Keys (their start) or names, as `list` says them
        what: Vec<String>,
        /// Everything the project does not use, however lately it was used
        #[arg(long, conflicts_with = "what")]
        unused: bool,
        /// The project's directory, whose extensions' packages are kept
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
}

#[derive(Subcommand)]
enum AuthorCommand {
    /// The SHA-256s a manifest's `[[setup]]` entries are held to, found by fetching each - every platform's - into a
    /// place of its own and hashing what it made, patched: its download's and each place it takes. A place may be
    /// written bare (`"<path>" = "<to>"`, `take = "<file>"`) until then
    Hash {
        /// The extension's manifest (`extension.toml`)
        manifest: PathBuf,
        /// Write them into the manifest; otherwise only what differs is said
        #[arg(long)]
        write: bool,
    },
    /// Whether every source a manifest's `[[setup]]` entries name - their URLs and mirrors, every platform's - still
    /// answers: failing where an entry has none left
    Pins {
        /// The extensions' manifests (`extension.toml`)
        #[arg(required = true)]
        manifests: Vec<PathBuf>,
    },
}

#[derive(Subcommand)]
enum UidCommand {
    /// New uids, none of them held by a declaration of the project
    New {
        /// How many
        #[arg(default_value_t = 1)]
        count: usize,
        /// The project's directory
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// The declarations, instead of the project's `[game] declarations`
        #[arg(long)]
        declarations: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum ConvertCommand {
    /// A BS picture (a background) to a PNG
    Bs { input: PathBuf, output: PathBuf },
    /// An image's first page to a PNG, at a bit depth, through a palette row (from a palette chunk) or in grey
    Texture {
        input: PathBuf,
        output: PathBuf,
        #[arg(long)]
        bpp: u8,
        #[arg(long)]
        palette: Option<PathBuf>,
        #[arg(long, default_value_t = 0)]
        row: usize,
        #[arg(long, default_value_t = 0)]
        column: usize,
    },
    /// A palette chunk to a PNG, a row of pixels a palette
    Palette { input: PathBuf, output: PathBuf },
}

#[derive(Subcommand)]
enum ProjectCommand {
    /// (Re)write a project's game layer from its discs, by its declarations
    Extract {
        /// The project's directory
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// A release to extract (every release of the project by default)
        #[arg(long)]
        release: Vec<String>,
        /// Where the disc images are, instead of the project's `roms`
        #[arg(long)]
        roms: Option<PathBuf>,
        /// The declarations, instead of the project's `[game] declarations`
        #[arg(long)]
        declarations: Option<PathBuf>,
    },
    /// Build a project's packages for a config
    Build {
        /// The project's directory
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// The config (the project's first by default)
        #[arg(long)]
        config: Option<String>,
        /// Clean first
        #[arg(long)]
        clean: bool,
    },
    /// Remove what a build deployed, and make it again next time
    Clean {
        /// The project's directory
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// The config (the project's first by default)
        #[arg(long)]
        config: Option<String>,
    },
    /// Say which places of a project you trust: what is in them runs unsandboxed. Kept with you, never in the project.
    /// With nothing to trust named, it says what is trusted
    Trust {
        /// The project's directory
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// All that is inside the project (not what is mounted from outside it)
        #[arg(long)]
        all: bool,
        /// A layer or a mount, by name
        #[arg(long)]
        layer: Vec<String>,
        /// A folder
        #[arg(long)]
        path: Vec<PathBuf>,
        /// Stop trusting those named, or all of the project's
        #[arg(long)]
        revoke: bool,
    },
    /// The extensions a project holds
    Extension {
        #[command(subcommand)]
        command: ExtensionCommand,
    },
    /// Signal an event, and wait for what it sets off
    Signal {
        /// The event's name
        event: String,
        /// The project's directory
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// The config (the project's first by default)
        #[arg(long)]
        config: Option<String>,
    },
}

#[derive(Subcommand)]
enum ExtensionCommand {
    /// Every extension present, and what became of it: the project's, the built-ins, and those kept to be added
    List {
        /// The project's directory
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// Add one to the project, copied whole: an extension's folder (`<id>.extension`) or its archive
    /// (`<id>-<version>.extension.zip`), or by id from your own extensions or those beside the editor
    Add {
        /// Its folder or archive, or its id
        what: String,
        /// The project's directory
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// Copy one the project holds again, from where it is kept or from a folder
    Update {
        /// Its id
        id: String,
        /// Its folder (`<id>.extension`) or archive, in place of where it is kept
        #[arg(long)]
        from: Option<PathBuf>,
        /// The project's directory
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// What the project's extensions fetch onto this machine, as their manifests declare: shown, and fetched ahead of
    /// time - what they require, and what is optional and on. Trusting them is consenting to it
    Setup {
        /// Only this one's
        id: Option<String>,
        /// Let it fetch this optional package, by name, on this computer
        #[arg(long)]
        enable: Vec<String>,
        /// Do not let it fetch this optional package, by name, on this computer
        #[arg(long)]
        disable: Vec<String>,
        /// Hash what was fetched, every file, and fetch again what is not as its extension declares
        #[arg(long)]
        verify: bool,
        /// The project's directory
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// Remove one from the project
    Remove {
        /// Its id
        id: String,
        /// The project's directory
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
}

const TOOLS: &[&str] = &["project", "convert", "uid"];

fn write_png(
    output: &std::path::Path,
    width: usize,
    height: usize,
    color: png::ColorType,
    pixels: &[u8],
) -> Result<()> {
    let file = std::io::BufWriter::new(std::fs::File::create(output)?);
    let mut encoder = png::Encoder::new(file, u32::try_from(width)?, u32::try_from(height)?);
    encoder.set_color(color);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(pixels)?;
    Ok(())
}

fn bs_to_png(input: &std::path::Path, output: &std::path::Path) -> Result<()> {
    let picture = codecs::bs::decode(&std::fs::read(input)?)?;
    write_png(output, picture.width, picture.height, png::ColorType::Rgb, &picture.rgb)
}

fn texture_to_png(
    input: &std::path::Path,
    output: &std::path::Path,
    bpp: u8,
    palette: Option<(&std::path::Path, usize, usize)>,
) -> Result<()> {
    let pages = codecs::texture::pages(&std::fs::read(input)?)?;
    let page = pages.first().context("an image without pages")?;
    let colors = match palette {
        Some((path, row, column)) => {
            let palette = codecs::texture::palette(&std::fs::read(path)?)?;
            Some(palette.rows.get(row).context("no such palette row")?.get(column..).unwrap_or_default().to_vec())
        }
        None => None,
    };
    let (width, rgba) = codecs::texture::render(page, bpp, colors.as_deref());
    write_png(output, width, page.rows, png::ColorType::Rgba, &rgba)
}

fn palette_to_png(input: &std::path::Path, output: &std::path::Path) -> Result<()> {
    let palette = codecs::texture::palette(&std::fs::read(input)?)?;
    let rgba: Vec<u8> = palette.rows.iter().flatten().flat_map(|&c| codecs::texture::rgba(c)).collect();
    write_png(output, palette.width.max(1), palette.rows.len().max(1), png::ColorType::Rgba, &rgba)
}

/// How long an editor is given to run an epoch.
const EPOCH_PATIENCE: std::time::Duration = std::time::Duration::from_secs(3600);

/// Run an epoch for each of these events, in turn, and say what came of them.
fn signal(project: &Path, config: Option<&str>, events: &[&str]) -> anyhow::Result<()> {
    let p = project::Project::load(project)?;
    // An editor with the project open holds it: it runs them, and says what came of them.
    let events_named: Vec<String> = events.iter().map(ToString::to_string).collect();
    let request = mcp::Request::Signal { events: events_named, config: config.map(str::to_string) };
    if let Some(reply) = mcp::link::forward_within(&p.root, &request, EPOCH_PATIENCE) {
        let said = reply.told().map_err(anyhow::Error::msg)?;
        if !said.is_empty() {
            println!("{said}");
        }
        return Ok(());
    }
    let chosen = p.configs.iter().find(|c| config.is_none_or(|name| c.name == name));
    let chosen = chosen.with_context(|| "no such config in project.toml")?.clone();
    let lock = project::lock::Lock::acquire(&p.root, "pe2-cli project", |owner| {
        eprintln!("waiting: the project is in use by {owner}");
    })?;
    let cache = project::cache::Cache::open(&p, &lock)?;
    let index = project::index::Index::open(&p, &cache)?;
    let extensions = extensions::for_project(&p);
    for problem in &extensions.problems {
        eprintln!("warning: {problem}");
    }
    whole(&extensions)?;
    let store = project::graph::store::Store::open(&cache)?;
    extensions.keep_in(store.clone());
    let wiring = project::build::wiring(&index, &extensions, &chosen);
    for (at, why) in &wiring.problems {
        eprintln!("{at}: {why}");
    }
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let mut failed = false;
    for event in events {
        let roots = [(*event).to_string()];
        let report = project::build::epoch(&p, &index, &extensions, &wiring, &chosen, &roots, &cancel);
        for (asset, output) in &report.made {
            println!("{asset}: {output}");
        }
        for (asset, note) in &report.notes {
            eprintln!("{asset}: warning: {note}");
        }
        for (asset, output) in &report.stale {
            eprintln!("{asset}: {output} is stale: what it reads changed after it ran");
        }
        for (asset, why) in &report.failed {
            eprintln!("{asset}: {why}");
            failed = true;
        }
    }
    // What it kept, swept on its way out where a sweep is due.
    let _ = project::imports::sweep_if_due(&store, &index);
    anyhow::ensure!(!failed, "it failed");
    Ok(())
}

/// What was said of a project's extensions, and whether it can go on without what it requires - what those running
/// fetch fetched first, as trusting them consents to it.
fn whole(extensions: &project::ext::Extensions) -> Result<()> {
    untrusted(extensions);
    let said: Vec<String> = extensions.unmet.iter().map(|(id, why)| format!("{id}: {why}")).collect();
    anyhow::ensure!(said.is_empty(), "the project requires what it does not have:\n  {}", said.join("\n  "));
    fetch(extensions)
}

/// Megabytes, to a tenth, as the CLI says them.
fn mb(bytes: u64) -> String {
    format!("{}.{}", bytes >> 20, ((bytes & 0xF_FFFF) * 10) >> 20)
}

/// Fetch what the extensions running require and have not fetched yet, saying what, from where, as it goes.
fn fetch(extensions: &project::ext::Extensions) -> Result<()> {
    let mut shown: Option<String> = None;
    let failed = extensions::fetch_required(extensions, &mut |id, setup, done, total| {
        let named = format!("{id}: {}", setup.name);
        if shown.as_ref() != Some(&named) {
            if shown.is_some() {
                eprintln!();
            }
            eprintln!("{named}, from {}", setup.url);
            shown = Some(named);
        }
        let of = total.map_or_default(|total| format!(" of {}", mb(total)));
        eprint!("\r  {}{of} MB   ", mb(done));
    });
    if shown.is_some() {
        eprintln!();
    }
    swept(extensions::sweep_store_if_due(extensions));
    let said: Vec<String> = failed.iter().map(|(what, why)| format!("{what}: {why}")).collect();
    anyhow::ensure!(
        said.is_empty(),
        "what the project's extensions require could not be fetched:\n  {}",
        said.join("\n  ")
    );
    Ok(())
}

/// What a sweep of the store of what extensions fetch removed, said on stderr; and what it could not.
fn swept(swept: Option<project::ext::setup::kept::Swept>) {
    let Some((removed, failed)) = swept else { return };
    for kept in &removed {
        eprintln!("removed from the store, unused: {} ({})", kept.said(), project::ext::setup::kept::size(kept.bytes));
    }
    for (kept, why) in &failed {
        eprintln!("warning: could not remove {} from the store: {why}", kept.said());
    }
}

/// `pe2-cli fetched`: what the store of what extensions fetch holds, and removing from it.
fn fetched(command: FetchedCommand) -> Result<()> {
    let store = extensions::setup_folder().context("no data folder")?;
    let (FetchedCommand::List { project } | FetchedCommand::Clean { project, .. }) = &command;
    // What the project names, where there is one: kept, and marked.
    let named =
        project::Project::load(project).map_or_default(|p| extensions::named_in_store(&extensions::for_project(&p)));
    match command {
        FetchedCommand::List { .. } => {
            let kept = project::ext::setup::kept::list(&store);
            for kept in &kept {
                let used = if named.contains(&kept.key) { ", used by this project" } else { "" };
                println!(
                    "{}  {}, used {}{used}",
                    kept.said(),
                    project::ext::setup::kept::size(kept.bytes),
                    project::ext::setup::kept::ago(kept.used)
                );
                println!("  {}", kept.key);
            }
            let total: u64 = kept.iter().map(|kept| kept.bytes).sum();
            println!("{} in {}", project::ext::setup::kept::size(total), store.display());
            println!(
                "What no project uses for {} days is removed by itself; `pe2-cli fetched clean` removes it now",
                project::ext::setup::kept::UNUSED_FOR.as_secs() / 86_400
            );
        }
        FetchedCommand::Clean { what, unused, .. } if what.is_empty() && !unused => {
            let (removed, failed) = project::ext::setup::kept::sweep(&store, &named);
            if removed.is_empty() && failed.is_empty() {
                println!("Nothing to remove: what is there was used lately, or this project uses it");
            }
            swept(Some((removed, failed)));
        }
        FetchedCommand::Clean { what, unused, .. } => {
            let kept = project::ext::setup::kept::list(&store);
            let chosen: Vec<&project::ext::setup::kept::Kept> = if unused {
                kept.iter().filter(|kept| !named.contains(&kept.key)).collect()
            } else {
                let mut chosen = vec![];
                for asked in &what {
                    let found: Vec<&project::ext::setup::kept::Kept> = kept
                        .iter()
                        .filter(|kept| {
                            kept.key.starts_with(asked.as_str())
                                || kept.name.as_ref().is_some_and(|(name, _)| name == asked)
                        })
                        .collect();
                    anyhow::ensure!(!found.is_empty(), "{asked}: nothing in the store: `pe2-cli fetched list`");
                    chosen.extend(found);
                }
                chosen
            };
            for kept in chosen {
                project::ext::setup::kept::remove(&store, &kept.key)?;
                println!("removed {} ({})", kept.said(), project::ext::setup::kept::size(kept.bytes));
            }
        }
    }
    Ok(())
}

/// Where an extension to add is: a folder or an archive given, or one of that id kept to be added.
fn kept_folder(p: &project::Project, what: &str) -> Result<PathBuf> {
    let given = Path::new(what);
    let given = if given.is_absolute() { given.to_path_buf() } else { std::env::current_dir()?.join(given) };
    if given.is_dir() || extensions::archive::is_archive(&given) {
        return Ok(given);
    }
    extensions::kept_apart(what).with_context(|| {
        let places: Vec<String> = [extensions::user_folder(), extensions::install_folder()]
            .into_iter()
            .flatten()
            .map(|f| f.display().to_string())
            .collect();
        format!(
            "{what} is no folder or archive, and no extension of that id is kept to be added (in {}); {} holds {}",
            places.join(" or "),
            p.root.display(),
            p.held_extensions().iter().filter_map(|f| project::ext::folder::id_of(f)).collect::<Vec<_>>().join(", ")
        )
    })
}

/// An extension added to a project from a folder, or from an archive unpacked first.
fn add_from(
    p: &project::Project,
    from: &Path,
    history: &mut project::history::History,
    label: String,
) -> Result<project::ext::component::About> {
    if extensions::archive::is_archive(from) {
        let unpacked = extensions::archive::unpack(from).with_context(|| from.display().to_string())?;
        return project::switches::add_extension(p, &unpacked.folder, history, label);
    }
    project::switches::add_extension(p, from, history, label)
}

/// `pe2-cli project extension setup`: what the project's extensions running fetch onto this machine, each shown with
/// how it stands - hashed whole where asked (`verify`), what is not as declared fetched again; what is optional turned
/// on or off as asked; then fetched, what is required and what is on.
fn set_up(p: &project::Project, only: Option<&str>, enable: &[String], disable: &[String], verify: bool) -> Result<()> {
    let extensions = extensions::for_project(p);
    let running = extensions.present.iter().filter(|present| present.status == project::ext::Status::Loaded);
    let chosen: Vec<&project::ext::Present> =
        running.filter(|present| only.is_none_or(|id| present.id == id)).collect();
    if let Some(id) = only.filter(|_| chosen.is_empty()) {
        anyhow::bail!("{id} does not run in this project: `pe2-cli project extension list`");
    }
    let here = |present: &&project::ext::Present| -> Vec<project::ext::setup::Setup> {
        present.setup.iter().filter(|setup| setup.here()).cloned().collect()
    };
    for (names, on) in [(enable, true), (disable, false)] {
        for name in names {
            let named: Vec<project::ext::setup::Setup> =
                chosen.iter().flat_map(here).filter(|setup| setup.name == *name).collect();
            anyhow::ensure!(!named.is_empty(), "nothing the project's extensions fetch here is called {name}");
            for setup in named {
                anyhow::ensure!(setup.lazy, "{name} is required: it is fetched whatever is chosen");
                extensions::trust::set_optional(&setup.key(), on)?;
            }
        }
    }
    let store = extensions::setup_folder().context("no data folder to keep it in")?;
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let mut failed = vec![];
    for present in &chosen {
        for setup in here(present) {
            let kind = if setup.lazy { "optional" } else { "required" };
            let on = !setup.lazy || extensions::optional_on(&setup);
            println!("{}: {} ({kind}{})\n  {}", present.id, setup.name, if on { "" } else { ", off" }, setup.url);
            for mirror in &setup.mirrors {
                println!("  or {mirror}");
            }
            let rights = [setup.license.clone(), setup.copyright.as_ref().map(|c| format!("(c) {c}"))];
            let rights: Vec<String> = rights.into_iter().flatten().collect();
            if !rights.is_empty() {
                println!("  {}", rights.join("; "));
            }
            if verify && setup.fetched(&store) {
                match project::ext::setup::verify(&setup, &store) {
                    Ok(()) => println!("  fetched, each file as declared"),
                    Err(e) => println!("  {e:#}: removed, to be fetched again"),
                }
            }
            if !on || setup.fetched(&store) {
                println!("  {}", if on { "fetched" } else { "turned off on this computer" });
                continue;
            }
            let mut shown = |done: u64, total: Option<u64>| {
                let of = total.map_or_default(|total| format!(" of {}", mb(total)));
                eprint!("\r  {}{of} MB   ", mb(done));
            };
            match project::ext::setup::fetch(&setup, &store, present.origin.folder(), &mut shown, &cancel) {
                Ok(()) => eprintln!("\r  fetched{}", " ".repeat(30)),
                Err(e) => {
                    eprintln!("\r  {e:#}");
                    failed.push(setup.name.clone());
                }
            }
        }
    }
    println!("Kept in {}, for every project on this computer", store.display());
    swept(extensions::sweep_store_if_due(&extensions));
    anyhow::ensure!(failed.is_empty(), "not fetched: {}", failed.join(", "));
    Ok(())
}

/// What to know of an extension just added: whether it has to be trusted, and what to set up first.
fn added(p: &project::Project, about: &project::ext::component::About) {
    println!("{} {}", about.id, about.version);
    let held = p.held_extensions().into_iter().find(|f| project::ext::folder::id_of(f) == Some(about.id.as_str()));
    if let Some(install) = about.described.install.as_ref().zip(held.as_ref()).map(|(page, held)| held.join(page)) {
        println!("What it needs on this machine, fetched or installed: {}", install.display());
    }
    let extensions = extensions::for_project(p);
    let present = extensions.present.iter().find(|present| present.id == about.id);
    if present.is_some_and(|present| matches!(present.status, project::ext::Status::NotEnabled { .. })) {
        println!(
            "It runs outside the sandbox: nothing of it runs until you trust it (`pe2-cli project trust`, or enable it in the editor)"
        );
    }
    if present.is_some_and(|present| !extensions::required(present).is_empty()) {
        println!(
            "It fetches what it runs onto this machine before it runs, as its manifest declares: `pe2-cli project extension setup {}` shows it, and fetches it now",
            about.id
        );
    }
}

/// `pe2-cli project extension`.
fn extension(command: ExtensionCommand) -> Result<()> {
    use project::ext::Status;
    let (ExtensionCommand::List { project }
    | ExtensionCommand::Add { project, .. }
    | ExtensionCommand::Update { project, .. }
    | ExtensionCommand::Setup { project, .. }
    | ExtensionCommand::Remove { project, .. }) = &command;
    let project = project.clone();
    let p = project::Project::load(&project)?;
    let lock = project::lock::Lock::acquire(&p.root, "pe2-cli project extension", |owner| {
        eprintln!("waiting: the project is in use by {owner}");
    })?;
    let (mut history, _) = project::history::History::open(&project::cache::Cache::open(&p, &lock)?)?;
    match command {
        ExtensionCommand::List { .. } => {
            let extensions = extensions::for_project(&p);
            for present in &extensions.present {
                let status = match &present.status {
                    Status::Loaded => "loaded".to_string(),
                    Status::Disabled => "left off".to_string(),
                    Status::Refused(why) => format!("not loaded: {why}"),
                    Status::NotEnabled { .. } => "not trusted".to_string(),
                    Status::Available => format!("to be added: `pe2-cli project extension add {}`", present.id),
                    Status::Replaced(by) => format!("replaced by {by}"),
                };
                let required = p.requires.iter().any(|required| required.id == present.id);
                println!(
                    "{} {}  {}{status}  ({})",
                    present.id,
                    present.version,
                    if required { "required, " } else { "" },
                    present.origin.said()
                );
            }
        }
        ExtensionCommand::Add { what, .. } => {
            let from = kept_folder(&p, &what)?;
            let about = add_from(&p, &from, &mut history, format!("Add {what}"))?;
            added(&project::Project::load(&project)?, &about);
        }
        ExtensionCommand::Update { id, from, .. } => {
            let held = p.held_extensions().into_iter().find(|f| project::ext::folder::id_of(f) == Some(id.as_str()));
            let held =
                held.with_context(|| format!("the project holds no {id}: `pe2-cli project extension add {id}`"))?;
            let was = project::ext::folder::manifest(&held).map_or_default(|about| about.version);
            let from = match from {
                Some(from) => from,
                None => kept_folder(&p, &id)?,
            };
            let about = add_from(&p, &from, &mut history, format!("Update {id}"))?;
            if was != about.version {
                println!("{id}: {was} to {}", about.version);
            }
            added(&project::Project::load(&project)?, &about);
        }
        ExtensionCommand::Remove { id, .. } => {
            project::switches::remove_extension(&p, &id, &mut history, format!("Remove {id}"))?;
        }
        ExtensionCommand::Setup { id, enable, disable, verify, .. } => {
            set_up(&p, id.as_deref(), &enable, &disable, verify)?;
        }
    }
    Ok(())
}

/// What runs outside the sandbox and is not trusted: what needs it fails later, saying nothing of why.
fn untrusted(extensions: &project::ext::Extensions) {
    let waiting = extensions.present.iter().filter(|p| matches!(p.status, project::ext::Status::NotEnabled { .. }));
    for present in waiting {
        eprintln!(
            "warning: {} runs outside the sandbox and is not trusted, so it does not run: `pe2-cli project trust`, or --trust-everything for this run",
            present.id
        );
    }
}

/// `pe2-cli project trust`.
fn trust(project: &Path, all: bool, layers: &[String], paths: Vec<PathBuf>, revoke: bool) -> Result<()> {
    use extensions::trust as places;
    let p = project::Project::load(project)?;
    let mut named = paths;
    named.extend(all.then(|| p.root.clone()));
    for name in layers {
        let layer = p.layers.iter().find(|layer| layer.name == *name);
        let Some(layer) = layer else {
            let known: Vec<&str> = p.layers.iter().map(|layer| layer.name.as_str()).collect();
            anyhow::bail!("the project has no layer {name}; it has {}", known.join(", "));
        };
        named.push(layer.path.clone());
    }
    match (revoke, named.is_empty()) {
        (true, _) => places::distrust(&p.root, &named)?,
        (false, false) => places::trust(&p.root, &named)?,
        (false, true) => {}
    }
    let root = p.root.canonicalize()?;
    let held = places::trusted().into_iter().find(|known| known.project == root);
    let shown = held.map_or_default(|known| known.places);
    if shown.is_empty() {
        println!("Nothing of {} is trusted", root.display());
    }
    for place in shown {
        println!("{}", place.display());
    }
    // Trusted, what its extensions require is fetched: trusting them is consenting to it.
    if !revoke && !named.is_empty() {
        fetch(&extensions::for_project(&p))?;
    }
    Ok(())
}

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().collect();
    let invoked = std::path::Path::new(&args[0]).file_stem().map(|s| s.to_string_lossy().into_owned());
    if let Some(tool) = invoked.filter(|t| TOOLS.contains(&t.as_str())) {
        args.insert(1, tool);
    }
    let cli = Cli::parse_from(args);
    if cli.trust_everything {
        extensions::trust::trust_everything();
        eprintln!("warning: trusting every extension in this run");
    }
    match cli.tool {
        Tool::Convert { command: ConvertCommand::Bs { input, output } } => bs_to_png(&input, &output)?,
        Tool::Convert { command: ConvertCommand::Texture { input, output, bpp, palette, row, column } } => {
            texture_to_png(&input, &output, bpp, palette.as_deref().map(|p| (p, row, column)))?;
        }
        Tool::Convert { command: ConvertCommand::Palette { input, output } } => palette_to_png(&input, &output)?,
        Tool::Project { command: ProjectCommand::Build { project, config, clean } } => {
            let events: &[&str] = if clean { &[Clean::NAME, Build::NAME] } else { &[Build::NAME] };
            signal(&project, config.as_deref(), events)?;
        }
        Tool::Project { command: ProjectCommand::Clean { project, config } } => {
            signal(&project, config.as_deref(), &[Clean::NAME])?;
        }
        Tool::Project { command: ProjectCommand::Trust { project, all, layer, path, revoke } } => {
            trust(&project, all, &layer, path, revoke)?;
        }
        Tool::Project { command: ProjectCommand::Extension { command } } => extension(command)?,
        Tool::Project { command: ProjectCommand::Signal { event, project, config } } => {
            signal(&project, config.as_deref(), &[event.as_str()])?;
        }
        Tool::Project { command: ProjectCommand::Extract { project, release, roms, declarations } } => {
            let p = project::Project::load(&project)?;
            let lock = project::lock::Lock::acquire(&p.root, "pe2-cli project extract", |owner| {
                eprintln!("waiting: the project is in use by {owner}");
            })?;
            if let Some(why) = lock.unprotected() {
                eprintln!("warning: {why}");
            }
            let options = project::extract::Options { releases: release, roms, declarations };
            // The extensions it holds, in its layers above the game's.
            let extensions = extensions::for_project(&p);
            for problem in &extensions.problems {
                eprintln!("warning: {problem}");
            }
            whole(&extensions)?;
            let report = project::extract::extract(&p, &options, &lock, &extensions)?;
            println!(
                "extracted {} assets ({} as their sources, each checked to import back exactly) and {} layout assets \
                 into {}",
                report.assets,
                report.sources,
                report.layouts,
                report.layer.display()
            );
            // What checking its sources kept, swept on its way out where a sweep is due, by the project as extracted.
            let _ = project::cache::Cache::open(&p, &lock).and_then(|cache| {
                let index = project::index::Index::open(&p, &cache)?;
                Ok(project::imports::sweep_if_due(&project::graph::store::Store::open(&cache)?, &index))
            });
        }
        Tool::Fetched { command } => fetched(command)?,
        Tool::Extension { command: AuthorCommand::Pins { manifests } } => {
            let mut gone = Vec::new();
            for manifest in &manifests {
                gone.extend(project::ext::setup::check_pins(manifest)?);
            }
            anyhow::ensure!(gone.is_empty(), "no source answers for:\n  {}", gone.join("\n  "));
        }
        Tool::Extension { command: AuthorCommand::Hash { manifest, write } } => {
            project::ext::setup::hash_manifest(&manifest, write)?;
        }
        Tool::Mcp { project } => {
            let root = project::Project::load(&project)?.root;
            mcp::server::serve_stdio(root)?;
        }
        Tool::Uid { command: UidCommand::New { count, project, declarations } } => {
            let root = if let Some(root) = declarations {
                root
            } else {
                let p = project::Project::load(&project)?;
                let game = p.game.as_ref().context("the project has no [game]")?;
                p.resolve(&game.declarations)?
            };
            let mut taken = project::declarations::Declarations::load(&root)?.uids();
            for _ in 0..count {
                let id = project::uid::new(|id| taken.contains(&id))
                    .map_err(|e| anyhow::anyhow!("the system's random source: {e}"))?;
                taken.insert(id);
                println!("{}", project::uid::text(id));
            }
        }
    }
    Ok(())
}
