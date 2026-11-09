//! `pe2-cli`: the tool crates behind one multi-call binary.
//! It dispatches on its subcommand (`pe2-cli project extract`) or, run under a tool's name (a link in the editor's
//! own `bin/`), on that name (`project extract`).

mod host;

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
    /// Print, once done, what the run spent its time on: each span's count, total, mean and longest (summed over
    /// threads, so work run in parallel totals more than it took)
    #[arg(long, global = true)]
    profile: bool,
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
    /// Projects: made, set up, and their generated layers filled by their plans
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
    /// Serve a project to agents over the Model Context Protocol, on stdio: through the editor while it has the
    /// project open, else under the project lock, taken for each request alone
    Mcp {
        /// The project's directory
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// The editor, started and waited on, as scripts and checks drive it
    Editor {
        #[command(subcommand)]
        command: EditorCommand,
    },
    /// Extraction plans, apart from any project
    Plan {
        #[command(subcommand)]
        command: PlanCommand,
    },
}

#[derive(Subcommand)]
enum PlanCommand {
    /// Run every operation of a plan and say, a JSON line each, what it read: its bytes' sha1 and size, or why it read
    /// nothing. Nothing is written but what is kept: a release's discs surveyed
    Observe {
        /// The plan
        plan: PathBuf,
        /// Where every release's discs are looked for, whatever the plan says
        #[arg(long)]
        roms: PathBuf,
        /// Keep what each operation read here, by its sha1
        #[arg(long)]
        keep: Option<PathBuf>,
        /// Keep what operations of these kinds read only (`inflate`, `slice`)
        #[arg(long, value_delimiter = ',')]
        keep_kinds: Vec<String>,
    },
}

#[derive(Subcommand)]
enum EditorCommand {
    /// Start the editor on a project, returning once it is up - waiting for an agent (`--wait-for-agent`), or the project
    /// open - and failing where it does not get there: the project held by another editor, a question the project asks
    /// whoever opens it, an error, the editor gone, or the time up. Prints its process
    Start {
        /// The project's directory
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Hold its opening once agents can reach it, until the first asks it something
        #[arg(long)]
        wait_for_agent: bool,
        /// How long it may take, in seconds
        #[arg(long, default_value_t = 120)]
        timeout: u64,
        /// What else the editor is given (`-- --select characters/aya/aya`)
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Wait for the editor that has a project open: until it is idle - the project open, nothing taken in or run -
    /// or until it exits; failing on an error, a question unanswered, or the time up
    Wait {
        /// The project's directory
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Until it is idle, or until it exits
        #[arg(long, value_enum, default_value_t = Until::Idle)]
        until: Until,
        /// How long it may take, in seconds
        #[arg(long, default_value_t = 600)]
        timeout: u64,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum Until {
    Idle,
    Exit,
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
enum LayerCommand {
    /// Add an empty layer at the top of the stack (`.layers/<name>`)
    Add {
        name: String,
        /// A layer of plain files only (code), not assets
        #[arg(long)]
        files: bool,
        /// Below every other layer, as one the rest build on (the game's)
        #[arg(long)]
        bottom: bool,
        /// A generated layer, filled by its plan (`.layers/<name>.plan.jsonl`, written beside it by whoever makes it)
        /// with the files of the nodes of the layer it fills
        #[arg(long, requires = "fills", conflicts_with_all = ["files", "bottom"])]
        generated: bool,
        /// The layer a generated layer fills
        #[arg(long, requires = "generated")]
        fills: Option<String>,
        /// The project's directory
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// Make a layer's records those of a tree of files and its changeset (`<folder>.toml` beside it), in place of all it
    /// had: what a tool writes as files (`tools/editor_project/init.py`)
    Import {
        name: String,
        /// The tree
        #[arg(long)]
        from: PathBuf,
        /// The project's directory
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// Write a records layer's entries out as a tree of files, its changeset beside it (`<folder>.toml`)
    Export {
        name: String,
        /// Where
        #[arg(long)]
        to: PathBuf,
        /// The project's directory
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
}

#[derive(Subcommand)]
enum MountCommand {
    /// Mount a tree at a name: a files layer at the bottom of the stack, or, unmanaged, a place read by path alone
    Add {
        at: String,
        /// The tree, relative to the project or absolute; an unmanaged mount's is each machine's (`locate`)
        #[arg(required_unless_present = "unmanaged")]
        path: Option<String>,
        /// Nothing is written into it
        #[arg(long)]
        read_only: bool,
        /// Not indexed: read by path alone (`res://<at>/...`), as a plan reads its discs
        #[arg(long)]
        unmanaged: bool,
        /// The project's directory
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// Say where an unmanaged mount is on this machine (`project.local.toml`)
    Locate {
        at: String,
        /// Where it is, relative to the project or absolute
        path: String,
        /// The project's directory
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
}

#[derive(Subcommand)]
enum LinkCommand {
    /// Link a file to one the project's builds deploy, on this machine alone, made once that is deployed: where a
    /// language server looks for it, the project not being the root of what it is edited in
    Add {
        /// The link, relative to the project (`../compile_commands.json`)
        at: String,
        /// The deployed file, relative to the project (`build/compile_commands.json`)
        deployed: String,
        /// The project's directory
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// Add a build the project can be: a release, and the target building it
    Add {
        name: String,
        #[arg(long)]
        release: Option<String>,
        #[arg(long)]
        target: Option<String>,
        /// The project's directory
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
}

/// Which project an edit is of, and which of its layers it goes to.
#[derive(clap::Args)]
struct EditAt {
    /// The project's directory
    #[arg(long, default_value = ".")]
    project: PathBuf,
    /// The layer it goes to: by default, the highest the project lets be written
    #[arg(long)]
    layer: Option<String>,
}

#[derive(Subcommand)]
enum StoreCommand {
    /// Store the project's large files as chunks: each file of its layers' assets, and each plain file, of 1 MiB or
    /// more and held whole until now, put in the store and replaced by its manifest, its content the same
    Migrate {
        /// The project's directory
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// Remove the chunks nothing names any more: no layer's manifest or listing, nor the history
    Gc {
        /// The project's directory
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// Pack a generated layer: its files kept in one listing (`.pack`), their chunks in the store, `project.toml`
    /// saying so (`storage = "packed"`)
    Pack {
        /// The project's directory
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// The layer
        #[arg(long)]
        layer: String,
    },
    /// Unpack a packed layer: its files written back as files
    Unpack {
        /// The project's directory
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// The layer
        #[arg(long)]
        layer: String,
    },
    /// Store a layer stored as a tree of files as records (one a entry, by uid): kept only where the project shows the
    /// same after, else the tree is put back
    Convert {
        /// The project's directory
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// The layer
        #[arg(long)]
        layer: String,
    },
}

#[derive(Subcommand)]
enum ProjectCommand {
    /// Fill a project's generated layers by their plans (`.layers/<layer>.plan.jsonl`), reading the discs where the
    /// plans say: only what changed or is gone is made, and what a plan no longer gives is removed
    Extract {
        /// The project's directory
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// This generated layer alone
        #[arg(long)]
        layer: Option<String>,
        /// The releases to read: `available` - those whose discs are all found - `all`, failing by name on a disc not
        /// found, or a list (`USA,EUR`)
        #[arg(long, default_value = "available")]
        releases: String,
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
    /// Make an empty project: a `project.toml` naming a layer of its own
    New {
        /// The directory to make it in (made where missing)
        directory: PathBuf,
    },
    /// The project's layers
    Layer {
        #[command(subcommand)]
        command: LayerCommand,
    },
    /// The trees mounted into the project
    Mount {
        #[command(subcommand)]
        command: MountCommand,
    },
    /// The builds the project can be
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Links this machine wants to what the project's builds deploy (`project.local.toml`)
    Link {
        #[command(subcommand)]
        command: LinkCommand,
    },
    /// Write a file at a path of the project (`packages/aya/node.toml`), from a file or standard input: a node's file, a
    /// plain one, or a `node.toml` that makes its folder a node
    Write {
        /// The path in the project, as the served folder shows it
        path: String,
        /// The file whose content to write; standard input where none
        #[arg(long)]
        from: Option<PathBuf>,
        #[command(flatten)]
        at: EditAt,
    },
    /// Make a folder
    Mkdir {
        path: String,
        #[command(flatten)]
        at: EditAt,
    },
    /// Delete an entry (a node with what is inside it), or a file of a node
    Rm {
        path: String,
        #[command(flatten)]
        at: EditAt,
    },
    /// Rename or move an entry, or a file within its node
    Mv {
        from: String,
        to: String,
        #[command(flatten)]
        at: EditAt,
    },
    /// Make the layer's own version of an entry from below
    Override {
        path: String,
        #[command(flatten)]
        at: EditAt,
    },
    /// Drop the layer's operation on an entry - an override, a move, a delete - and what it holds of it
    Drop {
        path: String,
        #[command(flatten)]
        at: EditAt,
    },
    /// Keep the layer's version of a stale override: its base becomes what is below now
    KeepMine {
        path: String,
        #[command(flatten)]
        at: EditAt,
    },
    /// Give up the layer's version of an entry for the one below
    TakeBase {
        path: String,
        #[command(flatten)]
        at: EditAt,
    },
    /// Take an entry a layer lists and no longer holds out of its list (a problem's layer and path)
    Forget {
        /// The layer holding it, as the problem names it
        #[arg(value_name = "LAYER")]
        of: String,
        path: String,
        #[command(flatten)]
        at: EditAt,
    },
    /// List what a layer holds unlisted at and under a path of it (a problem's layer and path)
    Adopt {
        /// The layer holding it, as the problem names it
        #[arg(value_name = "LAYER")]
        of: String,
        path: String,
        #[command(flatten)]
        at: EditAt,
    },
    /// Remove what a layer holds unlisted at a path of it from its folder (a problem's layer and path)
    RemoveUnlisted {
        /// The layer holding it, as the problem names it
        #[arg(value_name = "LAYER")]
        of: String,
        path: String,
        #[command(flatten)]
        at: EditAt,
    },
    /// Take back the last step of whoever holds the project (an editor, or the host serving it)
    Undo {
        #[command(flatten)]
        at: EditAt,
    },
    /// Do again the last step taken back
    Redo {
        #[command(flatten)]
        at: EditAt,
    },
    /// The project's problems: what does not add up, each with the paths it is about
    Problems {
        /// The project's directory
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// The project's chunk store, where its large files' content is kept
    Store {
        #[command(subcommand)]
        command: StoreCommand,
    },
    /// Serve a project's merged layers as a filesystem: the tree the editor shows - every asset a `<path>.node/` folder
    /// holding its files - at the project's `assets` folder, until stopped (Ctrl-C), what programs write there a step
    /// of the highest writable layer. It holds the project while it serves
    Serve {
        /// The project's directory
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// How to serve it - `fuse` (mounted at `assets`), or `webdav` (for the system's own client to mount) - in
        /// place of what `project.local.toml` says (`[filesystem] serve`)
        #[arg(long)]
        backend: Option<String>,
        /// WebDAV's address (`127.0.0.1:4918`; port 0 for any), in place of `[filesystem] address`
        #[arg(long)]
        address: Option<String>,
        /// On Windows, unregister the project's folder as a sync root, and serve nothing
        #[arg(long)]
        forget: bool,
        /// Stop the host serving it, whoever keeps it
        #[arg(long)]
        stop: bool,
        /// Where its folder holds files that are not the project's, move them beside it (`assets.set-aside`)
        #[arg(long, conflicts_with = "take_in")]
        set_aside: bool,
        /// Where its folder holds files that are not the project's, write them into the project
        #[arg(long)]
        take_in: bool,
    },
    /// Serve a project as a filesystem, holding it while nothing else does: what `serve` and the editor start, and keep
    #[command(hide = true)]
    Host {
        #[arg(long, default_value = ".")]
        project: PathBuf,
        #[arg(long)]
        backend: Option<String>,
        #[arg(long)]
        address: Option<String>,
        #[arg(long, value_enum, default_value = "refuse")]
        strays: host::Strays,
    },
    /// How a project's generated layers are, by their plans: none, empty, filled, or not as the plan says now - known
    /// without the discs and without reading what was made
    Status {
        /// The project's directory
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// This generated layer alone
        #[arg(long)]
        layer: Option<String>,
    },
    /// Check a project's generated layers against their plans, without the discs: every file there, made by what the
    /// plan says now, unchanged since it was written
    Verify {
        /// The project's directory
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// This generated layer alone
        #[arg(long)]
        layer: Option<String>,
    },
    /// What a project's extensions offer whoever writes its nodes, as JSON: each template - its node type, the files it
    /// takes, its stack with `{file}` for the node's stored file - each operation a stack may name - its ports, output,
    /// version, the settings it derives and what undoes it - and each extension's version
    Registry {
        /// The project's directory
        #[arg(long, default_value = ".")]
        project: PathBuf,
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
        /// The layer to hold it (the one holding it already, else the highest the project writes)
        #[arg(long)]
        layer: Option<String>,
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
/// The editor's program: beside this one, else as the system finds it.
fn editor_program() -> PathBuf {
    let beside = std::env::current_exe().ok().and_then(|exe| Some(exe.parent()?.join("pe2-editor")));
    beside.filter(|program| program.exists()).unwrap_or_else(|| PathBuf::from("pe2-editor"))
}

/// What keeps an editor from going on, where something does: an error it shows, a question the project asks.
fn blocked(presence: &mcp::presence::Presence) -> Option<String> {
    if let Some(error) = &presence.error {
        return Some(format!("the editor says: {error}"));
    }
    presence.asking.then(|| {
        "the project asks which of its extensions to trust: answer it in the editor, or trust them first \
         (`pe2-cli project trust`)"
            .to_string()
    })
}

/// Start the editor on a project, and wait until it is up: waiting for an agent, or the project open.
fn start_editor(project: &Path, wait_for_agent: bool, timeout: u64, args: &[String]) -> anyhow::Result<()> {
    let root = std::fs::canonicalize(project).with_context(|| project.display().to_string())?;
    if let Some(pid) = mcp::presence::claimed_by(&root) {
        anyhow::bail!("{}: the editor in process {pid} has it open", root.display());
    }
    let mut command = std::process::Command::new(editor_program());
    // What it says goes where this says what goes wrong: this says its process alone.
    command
        .arg(&root)
        .args(wait_for_agent.then_some("--wait-for-agent"))
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::io::stderr());
    let mut child = command.spawn().context("the editor could not be started")?;
    let pid = child.id();
    let started = std::time::Instant::now();
    let mut said = String::new();
    loop {
        if let Some(status) = child.try_wait()? {
            anyhow::bail!("the editor ended before it was up ({status}){said}");
        }
        if let Some(presence) = mcp::presence::running().remove(&pid) {
            if let Some(why) = blocked(&presence) {
                anyhow::bail!("{why}");
            }
            said = presence.stage.as_deref().map_or_else(String::new, |stage| format!("; it was at: {stage}"));
            let waiting = wait_for_agent && presence.stage.as_deref() == Some(WAITING_FOR_AGENT);
            let open = !wait_for_agent && presence.opening.is_none() && presence.project.is_some();
            if waiting || open {
                println!("{pid}");
                return Ok(());
            }
        }
        if started.elapsed() > std::time::Duration::from_secs(timeout) {
            anyhow::bail!("the editor in process {pid} was not up within {timeout} s{said}");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// The stage an editor holds its opening at for an agent (`pe2-editor --wait-for-agent`).
const WAITING_FOR_AGENT: &str = "Waiting for an agent";

/// Wait for the editor that has a project open: until it is idle, or until it exits.
fn wait_editor(project: &Path, until: Until, timeout: u64) -> anyhow::Result<()> {
    let root = std::fs::canonicalize(project).with_context(|| project.display().to_string())?;
    let Some(pid) = mcp::presence::claimed_by(&root) else {
        // Waited for to exit, an editor that is not there has.
        if until == Until::Exit {
            return Ok(());
        }
        anyhow::bail!("{}: no editor has it open", root.display());
    };
    let started = std::time::Instant::now();
    loop {
        match mcp::presence::running().remove(&pid) {
            None if until == Until::Exit => return Ok(()),
            None => anyhow::bail!("the editor in process {pid} ended"),
            Some(presence) => {
                if let Some(why) = blocked(&presence) {
                    anyhow::bail!("{why}");
                }
                if until == Until::Idle && presence.opening.is_none() && presence.busy.is_empty() {
                    return Ok(());
                }
            }
        }
        if started.elapsed() > std::time::Duration::from_secs(timeout) {
            anyhow::bail!("the editor in process {pid} did not get there within {timeout} s");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

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
    into: Option<&str>,
    history: &mut project::history::History,
    label: String,
) -> Result<project::ext::component::About> {
    if extensions::archive::is_archive(from) {
        let unpacked = extensions::archive::unpack(from).with_context(|| from.display().to_string())?;
        return project::switches::add_extension(p, &unpacked.folder, into, history, label);
    }
    project::switches::add_extension(p, from, into, history, label)
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
    // Its page read from what the project holds: a held extension's files are on no disk to point at.
    let install = about.described.install.as_ref().zip(held.as_ref());
    if let Some((page, text)) =
        install.and_then(|(page, held)| Some((page, project::content::read_to_string(&held.join(page)).ok()?)))
    {
        println!("What it needs on this machine, fetched or installed ({page}):\n");
        for line in text.lines() {
            println!("    {line}");
        }
        println!();
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

/// `pe2-cli project extension list`: each extension, how it stands, and where it is from.
fn list_extensions(p: &project::Project) {
    use project::ext::Status;
    let extensions = extensions::for_project(p);
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

/// `pe2-cli project extension`.
fn extension(command: ExtensionCommand) -> Result<()> {
    let (ExtensionCommand::List { project }
    | ExtensionCommand::Add { project, .. }
    | ExtensionCommand::Update { project, .. }
    | ExtensionCommand::Setup { project, .. }
    | ExtensionCommand::Remove { project, .. }) = &command;
    let project = project.clone();
    let p = project::Project::load(&project)?;
    // Listing them reads what the project holds and changes nothing: it does not wait for the project.
    if let ExtensionCommand::List { .. } = command {
        list_extensions(&p);
        return Ok(());
    }
    let lock = project::lock::Lock::acquire(&p.root, "pe2-cli project extension", |owner| {
        eprintln!("waiting: the project is in use by {owner}");
    })?;
    let (mut history, _) = project::history::History::open(&project::cache::Cache::open(&p, &lock)?)?;
    match command {
        ExtensionCommand::List { .. } => list_extensions(&p),
        ExtensionCommand::Add { what, layer, .. } => {
            let from = kept_folder(&p, &what)?;
            let about = add_from(&p, &from, layer.as_deref(), &mut history, format!("Add {what}"))?;
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
            let about = add_from(&p, &from, None, &mut history, format!("Update {id}"))?;
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
    project::profile::install();
    // A large file a layer holds as chunks is given to a program in a scoped filesystem, mounted when first needed.
    let _scopes = vfs::scoped::Scopes::install();
    let profile = cli.profile.then(std::time::Instant::now);
    // What the command line asks is a command: its work weighed as one, over the host's own.
    let done = project::schedule::Group::new(project::schedule::Class::Commanded, "pe2-cli").enter(|| run(cli));
    if let Some(started) = profile {
        eprintln!("\n{}", project::profile::table(&project::profile::totals()));
        eprintln!("{:.2} s in all", started.elapsed().as_secs_f64());
    }
    done
}

/// A project's setup edited - made, its layers, mounts and configs - or its generated layers' states said.
fn setup_command(command: ProjectCommand) -> Result<()> {
    match command {
        ProjectCommand::New { directory } => {
            project::create(&directory)?;
            println!("an empty project in {}", directory.display());
        }
        ProjectCommand::Layer { command: LayerCommand::Add { name, files, bottom, generated: _, fills, project } } => {
            if let Some(fills) = fills {
                edit_project(&project, |p, history| project::setup::add_generated(p, &name, &fills, history))?;
            } else {
                let kind = if files { project::LayerKind::Files } else { project::LayerKind::Assets };
                edit_project(&project, |p, history| project::stack::add_at(p, &name, kind, bottom, history))?;
            }
        }
        ProjectCommand::Layer { command: LayerCommand::Import { name, from, project } } => {
            let p = project::Project::load(&project)?;
            let lock = project::lock::Lock::acquire(&p.root, "pe2-cli project layer import", |owner| {
                eprintln!("waiting: the project is in use by {owner}");
            })?;
            let cache = project::cache::Cache::open(&p, &lock)?;
            let count = project::records::import(&p, &cache, &name, &std::path::absolute(&from)?)?;
            println!("{name}: {count} records");
        }
        ProjectCommand::Layer { command: LayerCommand::Export { name, to, project } } => {
            let p = project::Project::load(&project)?;
            let count = project::records::export(&p, &name, &std::path::absolute(&to)?)?;
            println!("{name}: {count} entries written to {}", to.display());
        }
        ProjectCommand::Mount { command: MountCommand::Add { at, path, read_only, unmanaged, project } } => {
            edit_project(&project, |p, history| {
                project::setup::add_mount(p, &at, path.as_deref(), read_only, unmanaged, history)
            })?;
        }
        ProjectCommand::Mount { command: MountCommand::Locate { at, path, project } } => {
            project::setup::locate(&project::Project::load(&project)?, &at, &path)?;
        }
        ProjectCommand::Link { command: LinkCommand::Add { at, deployed, project } } => {
            project::setup::add_link(&project::Project::load(&project)?, &at, &deployed)?;
        }
        ProjectCommand::Config { command: ConfigCommand::Add { name, release, target, project } } => {
            edit_project(&project, |p, history| {
                project::setup::add_config(p, &name, release.as_deref(), target.as_deref(), history)
            })?;
        }
        ProjectCommand::Status { project, layer } => {
            let p = project::Project::load(&project)?;
            let extensions = project_extensions(&p);
            for l in generated_layers(&p, layer.as_deref())? {
                let name = &l.name;
                match project::plan::state(&l.plan(), &l.path, &extensions)? {
                    project::plan::State::NoPlan => println!("{name} has no plan ({})", l.plan().display()),
                    project::plan::State::Empty => println!("{name} is empty: its plan has not run"),
                    project::plan::State::Filled => println!("{name} is filled, as its plan says"),
                    project::plan::State::Partial(not) => {
                        println!("{name} is filled of some releases: {}'s discs were not read", not.join(", "));
                    }
                    project::plan::State::Stale(why) => println!("{name} is stale: {}", why.join("; ")),
                }
            }
        }
        _ => unreachable!("not a setup command"),
    }
    Ok(())
}

/// Edit a project's setup, holding its lock, as a step of its history.
fn edit_project(
    project: &Path,
    edit: impl FnOnce(&project::Project, &mut project::history::History) -> Result<()>,
) -> Result<()> {
    let p = project::Project::load(project)?;
    let lock = project::lock::Lock::acquire(&p.root, "pe2-cli project", |owner| {
        eprintln!("waiting: the project is in use by {owner}");
    })?;
    let (mut history, _) = project::history::History::open(&project::cache::Cache::open(&p, &lock)?)?;
    edit(&p, &mut history)
}

/// A project's generated layers, or the one named: none is an error.
fn generated_layers<'p>(p: &'p project::Project, only: Option<&str>) -> Result<Vec<&'p project::Layer>> {
    let layers: Vec<_> = p.generated().filter(|l| only.is_none_or(|name| l.name == name)).collect();
    if layers.is_empty() {
        match only {
            Some(name) => anyhow::bail!("the project has no generated layer {name}"),
            None => anyhow::bail!("the project has no generated layer: nothing says what to extract"),
        }
    }
    Ok(layers)
}

/// Run a generated layer's plan.
fn run_plan(
    p: &project::Project,
    lock: &project::lock::Lock,
    layer: &project::Layer,
    releases: &project::plan::Releases,
) -> Result<()> {
    let plan = layer.plan();
    anyhow::ensure!(plan.is_file(), "{} has no plan ({}): nothing says what to extract", layer.name, plan.display());
    let extensions = project_extensions(p);
    let cache = project::cache::Cache::open(p, lock)?;
    let engine = project::imports::Imports::open(&cache)?.engine(&extensions);
    let ran = project::plan::run(p, layer, &engine, &extensions, &project::plan::Progress::default(), releases)?;
    println!(
        "{} ({}): {} files written and {} kept as they were ({} operations run), {} removed, {} of releases not read left, \
         {} releases' own the same as what they share",
        layer.name,
        ran.releases.join(", "),
        ran.written,
        ran.kept,
        ran.ran,
        ran.removed,
        ran.skipped,
        ran.shared
    );
    Ok(())
}

/// The extensions a project holds, as many as load: what a plan runs of one that did not fails by name, as it is run.
fn project_extensions(p: &project::Project) -> std::sync::Arc<project::ext::Extensions> {
    let extensions = extensions::for_project(p);
    for problem in &extensions.problems {
        eprintln!("warning: {problem}");
    }
    extensions
}

/// Check generated layers against their plans, exiting with failure where one is not what its plan makes.
/// A project command serving it, editing it, or saying its problems.
fn project_command(command: &ProjectCommand) -> Result<()> {
    match command {
        ProjectCommand::Problems { project } => return problems(project),
        ProjectCommand::Serve { project, backend, address, forget, stop, set_aside, take_in } => {
            let strays = match (set_aside, take_in) {
                (true, _) => host::Strays::SetAside,
                (_, true) => host::Strays::TakeIn,
                _ => host::Strays::Refuse,
            };
            return if *forget {
                forget_served(project)
            } else {
                serve_project(project, backend.as_deref(), address.as_deref(), *stop, strays)
            };
        }
        ProjectCommand::Host { project, backend, address, strays } => {
            return host::run(project, backend.as_deref(), address.as_deref(), *strays);
        }
        _ => {}
    }
    let (op, at) = edit_of(command).context("no such command")??;
    let done = edit(&op, at);
    // What standard input was kept in is the edit's alone.
    if let (ProjectCommand::Write { from: None, .. }, vfs::ops::Op::Write { from, .. }) = (command, &op) {
        let _ = std::fs::remove_file(from);
    }
    done
}

/// The edit a project command is, and where it goes; none for another command.
fn edit_of(command: &ProjectCommand) -> Option<Result<(vfs::ops::Op, &EditAt)>> {
    use vfs::ops::Op;
    let path = |p: &String| p.trim_matches('/').to_string();
    Some(Ok(match command {
        ProjectCommand::Write { path: at_path, from, at } => {
            let from = match from {
                Some(from) => std::path::absolute(from).map_err(anyhow::Error::from),
                None => read_stdin_to_file(),
            };
            match from {
                Ok(from) => (Op::Write { path: path(at_path), from }, at),
                Err(e) => return Some(Err(e)),
            }
        }
        ProjectCommand::Mkdir { path: p, at } => (Op::Folder { path: path(p) }, at),
        ProjectCommand::Rm { path: p, at } => (Op::Remove { path: path(p) }, at),
        ProjectCommand::Mv { from, to, at } => (Op::Rename { from: path(from), to: path(to) }, at),
        ProjectCommand::Override { path: p, at } => (Op::Override { path: path(p) }, at),
        ProjectCommand::Drop { path: p, at } => (Op::Drop { path: path(p) }, at),
        ProjectCommand::KeepMine { path: p, at } => (Op::KeepMine { path: path(p) }, at),
        ProjectCommand::TakeBase { path: p, at } => (Op::TakeBase { path: path(p) }, at),
        ProjectCommand::Forget { of, path: p, at } => (Op::Forget { layer: of.clone(), path: path(p) }, at),
        ProjectCommand::Adopt { of, path: p, at } => (Op::Adopt { layer: of.clone(), path: path(p) }, at),
        ProjectCommand::RemoveUnlisted { of, path: p, at } => {
            (Op::RemoveUnlisted { layer: of.clone(), path: path(p) }, at)
        }
        ProjectCommand::Undo { at } => (Op::Undo, at),
        ProjectCommand::Redo { at } => (Op::Redo, at),
        _ => return None,
    }))
}

/// Standard input kept in a file, for what applies a write to read: the file's path.
fn read_stdin_to_file() -> Result<PathBuf> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    std::io::stdin().read_to_end(&mut bytes)?;
    let file = std::env::temp_dir().join(format!("pe2-write-{}", std::process::id()));
    std::fs::write(&file, bytes)?;
    Ok(file)
}

/// Make an edit of a project: by whoever holds it, or - where nothing does - here, holding it for the while.
fn edit(op: &vfs::ops::Op, at: &EditAt) -> Result<()> {
    let root = project::Project::load(&at.project)?.root;
    let request = mcp::Request::Edit { op: op.clone(), layer: at.layer.clone() };
    let reply = mcp::server::execute(&root, &request);
    // Made by whoever holds the project: what is served follows at once, not when the host's watch sees it.
    if let mcp::Reply::Edited { touched, .. } = &reply {
        project::host::changed(&root, touched.clone());
    }
    println!("{}", reply.told().map_err(anyhow::Error::msg)?);
    Ok(())
}

/// `pe2-cli project problems`: asked of whoever holds the project, or read here.
fn problems(project: &Path) -> Result<()> {
    let root = project::Project::load(project)?.root;
    let request = mcp::Request::Call { name: "problems".into(), arguments: serde_json::Map::new() };
    let told = mcp::server::execute(&root, &request).told().map_err(anyhow::Error::msg)?;
    let told: serde_json::Value = serde_json::from_str(&told)?;
    let listed = |key: &str| told[key].as_array().cloned().unwrap_or_default();
    for problem in listed("problems") {
        let paths: Vec<&str> = problem["paths"].as_array().into_iter().flatten().filter_map(|p| p.as_str()).collect();
        println!("{}: {}", paths.join(", "), problem["problem"].as_str().unwrap_or_default());
    }
    for reference in listed("dangling_references") {
        println!("dangling: {reference}");
    }
    if listed("problems").is_empty() && listed("dangling_references").is_empty() {
        println!("no problems");
    }
    Ok(())
}

/// `pe2-cli project store`.
fn store(command: &StoreCommand) -> Result<()> {
    let (StoreCommand::Migrate { project }
    | StoreCommand::Gc { project }
    | StoreCommand::Pack { project, .. }
    | StoreCommand::Unpack { project, .. }
    | StoreCommand::Convert { project, .. }) = command;
    let p = project::Project::load(project)?;
    let lock = project::lock::Lock::acquire(&p.root, "pe2-cli project store", |owner| {
        eprintln!("waiting: the project is in use by {owner}");
    })?;
    // Whole MiB with a tenth: what a person reads of a size.
    let mib = |bytes: u64| format!("{}.{}", bytes >> 20, ((bytes % (1 << 20)) * 10) >> 20);
    match command {
        StoreCommand::Migrate { .. } => {
            let cache = project::cache::Cache::open(&p, &lock)?;
            let index = project::index::Index::open(&p, &cache)?;
            let pass = project::chunks::migrate(&p, &index)?;
            println!(
                "{} files stored as chunks: {} MiB whole, {} MiB in the store and their manifests",
                pass.files,
                mib(pass.before),
                mib(pass.after)
            );
        }
        StoreCommand::Pack { layer, .. } => {
            let count = project::pack::pack_layer(&p, layer)?;
            println!("{layer}: {count} files packed");
        }
        StoreCommand::Unpack { layer, .. } => {
            let count = project::pack::unpack_layer(&p, layer)?;
            println!("{layer}: {count} files unpacked");
        }
        StoreCommand::Convert { layer, .. } => {
            let cache = project::cache::Cache::open(&p, &lock)?;
            let started = std::time::Instant::now();
            let converted = project::records::convert(&p, &cache, layer)?;
            let took = started.elapsed().as_secs_f64();
            if converted.records > 0 {
                println!("{layer}: {} records, the project showing the same ({took:.1}s)", converted.records);
            }
            if converted.extensions > 0 {
                println!("{layer}: {} extensions held as records", converted.extensions);
            }
        }
        StoreCommand::Gc { .. } => {
            let (gone, freed) = project::chunks::sweep(&p)?;
            println!("{gone} chunks no longer named removed: {} MiB freed", mib(freed));
        }
    }
    Ok(())
}

/// `pe2-cli project serve`: the host keeps serving the project while this runs - started where none runs - or, with
/// `--stop`, stops.
fn serve_project(
    project: &Path,
    backend: Option<&str>,
    address: Option<&str>,
    stop: bool,
    strays: host::Strays,
) -> Result<()> {
    use project::host::{Connection, FromHost, ToHost};
    let p = project::Project::load(project)?;
    if stop {
        match Connection::connect(&p.root) {
            Some(mut host) => {
                host.ask(&ToHost::Stop)?;
                println!("the host serving {} stops", project::host::shown(&p.root));
            }
            None => println!("nothing serves {}", project::host::shown(&p.root)),
        }
        return Ok(());
    }
    let found = Connection::connect(&p.root);
    let started = found.is_none();
    let mut host = if let Some(host) = found {
        if backend.is_some() || address.is_some() {
            eprintln!("a host serves it already, as it does: --backend and --address are for one started");
        }
        host
    } else {
        let mut args = Vec::new();
        for (flag, value) in [("--backend", backend), ("--address", address)] {
            if let Some(value) = value {
                args.extend([flag.to_string(), value.to_string()]);
            }
        }
        args.extend(host::Strays::args(strays));
        project::host::start(&p.root, &std::env::current_exe()?, &args)?
    };
    let FromHost::Kept { at, note } = host.ask(&ToHost::Keep { who: "pe2-cli project serve".into() })? else {
        anyhow::bail!("the host would not keep serving");
    };
    let how = if started { "a host started for it" } else { "by the host serving it already" };
    println!("{} served at {at}, {how}: Ctrl-C lets go of it", project::host::shown(&p.root));
    if let Some(note) = note {
        println!("note: {note}");
    }
    // Until Ctrl-C, or the host stops.
    let (stop, stopped) = std::sync::mpsc::channel();
    let interrupted = stop.clone();
    ctrlc::set_handler(move || {
        let _ = interrupted.send(false);
    })?;
    std::thread::spawn(move || {
        host.closed();
        let _ = stop.send(true);
    });
    if stopped.recv().unwrap_or(true) {
        println!("the host stopped");
    } else if p.filesystem.keep {
        println!("let go: the host serves on, as this machine says (`[filesystem] keep`)");
    } else {
        println!("let go: unless something else keeps it, the host stops in 30s, once no file is open in it");
    }
    Ok(())
}

/// Unregister a project's folder as a sync root (Windows).
fn forget_served(project: &Path) -> Result<()> {
    let p = project::Project::load(project)?;
    #[cfg(windows)]
    {
        vfs::cloud::forget(&host::served_name(&p))?;
        println!("{} is no longer a sync root", project::host::shown(&p.filesystem.at));
    }
    #[cfg(not(windows))]
    println!("{}: only Windows registers a project's folder", project::host::shown(&p.root));
    Ok(())
}

fn verify_layer(project: &Path, only: Option<&str>) -> Result<()> {
    let p = project::Project::load(project)?;
    let extensions = project_extensions(&p);
    let mut whole = true;
    for layer in generated_layers(&p, only)? {
        let checked = project::plan::verify(&layer.plan(), &layer.path, &|id| extensions.version_of(id))?;
        for (what, files) in [("missing", &checked.missing), ("stale", &checked.stale), ("changed", &checked.changed)] {
            for file in files.iter().take(20) {
                println!("{}: {what}: {file}", layer.name);
            }
        }
        println!(
            "{}: {} files: {} missing, {} made by another plan, {} changed",
            layer.name,
            checked.files,
            checked.missing.len(),
            checked.stale.len(),
            checked.changed.len()
        );
        if !checked.not_read.is_empty() {
            println!(
                "{}: {}'s discs were not read: what only they make is not there",
                layer.name,
                checked.not_read.join(", ")
            );
        }
        whole &= checked.whole();
    }
    if !whole {
        std::process::exit(1);
    }
    Ok(())
}

/// The project's extensions' registry, as JSON: of those that load, the rest said.
fn print_registry(project: &Path) -> Result<()> {
    let extensions = project_extensions(&project::Project::load(project)?);
    println!("{:#}", extensions.registry());
    Ok(())
}

/// `pe2-cli plan observe`: what each operation of a plan read, as JSON lines.
fn observe_plan(plan: &Path, roms: &Path, keep: Option<&Path>, keep_kinds: &[String]) -> Result<()> {
    let extensions = extensions::shared();
    let engine = extensions.engine();
    let roms = roms.to_path_buf();
    let seen = project::plan::observe(plan, &|_| Ok(roms.clone()), &engine, &extensions, keep, keep_kinds)?;
    let lines: Vec<String> = seen.iter().map(serde_json::to_string).collect::<Result<_, _>>()?;
    println!("{}", lines.join("\n"));
    Ok(())
}

fn run(cli: Cli) -> Result<()> {
    if cli.trust_everything {
        extensions::trust::trust_everything();
        eprintln!("warning: trusting every extension in this run");
    }
    match cli.tool {
        Tool::Plan { command: PlanCommand::Observe { plan, roms, keep, keep_kinds } } => {
            observe_plan(&plan, &roms, keep.as_deref(), &keep_kinds)?;
        }
        Tool::Convert { command: ConvertCommand::Bs { input, output } } => bs_to_png(&input, &output)?,
        Tool::Convert { command: ConvertCommand::Texture { input, output, bpp, palette, row, column } } => {
            texture_to_png(&input, &output, bpp, palette.as_deref().map(|p| (p, row, column)))?;
        }
        Tool::Convert { command: ConvertCommand::Palette { input, output } } => palette_to_png(&input, &output)?,
        Tool::Editor { command: EditorCommand::Start { project, wait_for_agent, timeout, args } } => {
            start_editor(&project, wait_for_agent, timeout, &args)?;
        }
        Tool::Editor { command: EditorCommand::Wait { project, until, timeout } } => {
            wait_editor(&project, until, timeout)?;
        }
        Tool::Project { command: ProjectCommand::Build { project, config, clean } } => {
            let events: &[&str] = if clean { &[Clean::NAME, Build::NAME] } else { &[Build::NAME] };
            signal(&project, config.as_deref(), events)?;
        }
        Tool::Project {
            command:
                command @ (ProjectCommand::New { .. }
                | ProjectCommand::Layer { .. }
                | ProjectCommand::Mount { .. }
                | ProjectCommand::Config { .. }
                | ProjectCommand::Link { .. }
                | ProjectCommand::Status { .. }),
        } => setup_command(command)?,
        Tool::Project { command: ProjectCommand::Verify { project, layer } } => {
            verify_layer(&project, layer.as_deref())?;
        }
        Tool::Project { command: ProjectCommand::Registry { project } } => print_registry(&project)?,
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
        Tool::Project { command: ProjectCommand::Store { command } } => store(&command)?,
        Tool::Project { command: ProjectCommand::Extract { project, layer, releases } } => {
            let releases = match releases.as_str() {
                "available" => project::plan::Releases::Available,
                "all" => project::plan::Releases::All,
                list => project::plan::Releases::Only(list.split(',').map(|r| r.trim().to_string()).collect()),
            };
            let p = project::Project::load(&project)?;
            let lock = project::lock::Lock::acquire(&p.root, "pe2-cli project extract", |owner| {
                eprintln!("waiting: the project is in use by {owner}");
            })?;
            if let Some(why) = lock.unprotected() {
                eprintln!("warning: {why}");
            }
            for layer in generated_layers(&p, layer.as_deref())? {
                run_plan(&p, &lock, layer, &releases)?;
            }
            // What checking its sources kept, swept on its way out where a sweep is due, by the project as extracted:
            // indexed only then.
            let _ = project::cache::Cache::open(&p, &lock).and_then(|cache| {
                let store = project::graph::store::Store::open(&cache)?;
                if !store.sweep_due(project::imports::SWEEP_EVERY) {
                    return Ok(None);
                }
                let index = project::index::Index::open(&p, &cache)?;
                Ok(project::imports::sweep_if_due(&store, &index))
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
        // What is left of the project's commands serve it, edit it, and say its problems.
        Tool::Project { command } => project_command(&command)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    /// Every command parses: names unique, arguments well formed (clap's own checks, which otherwise run only when
    /// a command is used).
    #[test]
    fn the_commands_are_well_formed() {
        use clap::CommandFactory as _;
        super::Cli::command().debug_assert();
    }
}
