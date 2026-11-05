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
    /// What a project takes in from outside itself
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
    /// Every extension present, and what became of it
    List {
        /// The project's directory
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// Take one in: by id, from your own extensions or those beside the editor, or from a folder or component
    Add {
        /// Its id, or where it is, relative to the project or `res://`
        what: String,
        /// Without it, the project is not whole
        #[arg(long)]
        required: bool,
        /// The project's directory
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// Let one go
    Remove {
        /// Its id
        id: String,
        /// The project's directory
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// Keep what each is now, as what the project took in
    Lock {
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
    let extensions = extensions::for_project(&p, Some(&index));
    for problem in &extensions.problems {
        eprintln!("warning: {problem}");
    }
    whole(&extensions)?;
    extensions.keep_in(project::graph::store::Store::open(&cache)?);
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
    anyhow::ensure!(!failed, "it failed");
    Ok(())
}

/// What was said of a project's extensions, and whether it can go on without what it requires.
fn whole(extensions: &project::ext::Extensions) -> Result<()> {
    untrusted(extensions);
    for id in &extensions.unlocked {
        eprintln!(
            "warning: {id} is not what the project took in: `pe2-cli project extension lock` keeps what it is now"
        );
    }
    let said: Vec<String> = extensions.unmet.iter().map(|(id, why)| format!("{id}: {why}")).collect();
    anyhow::ensure!(said.is_empty(), "the project requires what it does not have:\n  {}", said.join("\n  "));
    Ok(())
}

/// A project's extensions as it says, with what its index holds.
fn extensions_of(
    p: &project::Project,
) -> Result<(project::lock::Lock, project::index::Index, std::sync::Arc<project::ext::Extensions>)> {
    let lock = project::lock::Lock::acquire(&p.root, "pe2-cli project extension", |owner| {
        eprintln!("waiting: the project is in use by {owner}");
    })?;
    let cache = project::cache::Cache::open(p, &lock)?;
    let index = project::index::Index::open(p, &cache)?;
    let extensions = extensions::for_project(p, Some(&index));
    Ok((lock, index, extensions))
}

/// `pe2-cli project extension`.
fn extension(command: ExtensionCommand) -> Result<()> {
    use project::ext::Status;
    let (ExtensionCommand::List { project }
    | ExtensionCommand::Add { project, .. }
    | ExtensionCommand::Remove { project, .. }
    | ExtensionCommand::Lock { project }) = &command;
    let project = project.clone();
    let p = project::Project::load(&project)?;
    let (lock, index, extensions) = extensions_of(&p)?;
    let (mut history, _) = project::history::History::open(&project::cache::Cache::open(&p, &lock)?)?;
    match command {
        ExtensionCommand::List { .. } => {
            for present in &extensions.present {
                let status = match &present.status {
                    Status::Loaded => "loaded".to_string(),
                    Status::Disabled => "left off".to_string(),
                    Status::Refused(why) => format!("not loaded: {why}"),
                    Status::NotEnabled { .. } => "not trusted".to_string(),
                    Status::Available => "available".to_string(),
                    Status::Replaced(by) => format!("replaced by {by}"),
                };
                let required =
                    p.manifest.imported.iter().any(|imported| imported.id == present.id && imported.required);
                println!(
                    "{} {}  {}{status}  ({})",
                    present.id,
                    present.version,
                    if required { "required, " } else { "" },
                    present.origin.said()
                );
            }
        }
        ExtensionCommand::Add { what, required, .. } => {
            // A place, or who is available.
            let place = p.imported_from(&what);
            let at = (place.exists() || what.starts_with("res://")).then_some(&place);
            let about = if let Some(place) = at {
                let file = if place.is_dir() { place.join(project::ext::EXTENSION_COMPONENT) } else { place.clone() };
                project::ext::component::about(&std::fs::read(&file).with_context(|| file.display().to_string())?)
                    .context("it is no extension")?
            } else {
                let found = extensions.present.iter().find(|present| present.id == what);
                let found = found.with_context(|| format!("{what} is not among those found: give where it is"))?;
                project::ext::component::About {
                    id: found.id.clone(),
                    version: found.version.clone(),
                    ..Default::default()
                }
            };
            let line = about.version.rsplit_once('.').map_or(about.version.as_str(), |(line, _)| line);
            let imported = project::Imported {
                id: about.id.clone(),
                version: Some(format!("^{line}")),
                required,
                from: at.map(|_| what.clone()),
            };
            project::switches::import(&p, &imported, &mut history, format!("Take {} in", about.id))?;
            let p = project::Project::load(&project)?;
            project::switches::write_lock(&p, &extensions::locked(&p, Some(&index)))?;
            println!("{} {}", about.id, about.version);
        }
        ExtensionCommand::Remove { id, .. } => {
            project::switches::remove(&p, &id, &mut history, format!("Let {id} go"))?;
            let p = project::Project::load(&project)?;
            project::switches::write_lock(&p, &extensions::locked(&p, Some(&index)))?;
        }
        ExtensionCommand::Lock { .. } => project::switches::write_lock(&p, &extensions::locked(&p, Some(&index)))?,
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
            // Its own extensions (`extension` assets, in its layers above the game's) and the user's.
            let index =
                project::cache::Cache::open(&p, &lock).and_then(|cache| project::index::Index::open(&p, &cache));
            let extensions = extensions::for_project(&p, index.as_ref().ok());
            drop(index);
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
