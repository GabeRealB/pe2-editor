//! `pe2-cli`: the tool crates behind one multi-call binary.
//! It dispatches on its subcommand (`pe2-cli project extract`) or, run under a tool's name (a link in the editor's
//! own `bin/`), on that name (`project extract`).

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "pe2-cli", about = "The Parasite Eve II editor's tools")]
struct Cli {
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

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().collect();
    let invoked = std::path::Path::new(&args[0]).file_stem().map(|s| s.to_string_lossy().into_owned());
    if let Some(tool) = invoked.filter(|t| TOOLS.contains(&t.as_str())) {
        args.insert(1, tool);
    }
    match Cli::parse_from(args).tool {
        Tool::Convert { command: ConvertCommand::Bs { input, output } } => bs_to_png(&input, &output)?,
        Tool::Convert { command: ConvertCommand::Texture { input, output, bpp, palette, row, column } } => {
            texture_to_png(&input, &output, bpp, palette.as_deref().map(|p| (p, row, column)))?;
        }
        Tool::Convert { command: ConvertCommand::Palette { input, output } } => palette_to_png(&input, &output)?,
        Tool::Project { command: ProjectCommand::Extract { project, release, roms, declarations } } => {
            let p = project::Project::load(&project)?;
            let lock = project::lock::Lock::acquire(&p.root, "pe2-cli project extract", |owner| {
                eprintln!("waiting: the project is in use by {owner}");
            })?;
            if let Some(why) = lock.unprotected() {
                eprintln!("warning: {why}");
            }
            let options = project::extract::Options { releases: release, roms, declarations };
            let extensions = extensions::shared();
            for problem in &extensions.problems {
                eprintln!("warning: {problem}");
            }
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
