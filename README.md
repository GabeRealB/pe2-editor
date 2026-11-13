# PE2 Editor

An incomplete, experimental editor for Parasite Eve II: for working with a game project's assets - its pictures,
sounds, models, dialogues and the code it is built from - and, in time, for producing releases of it.

**It ships with no projects and no game data.** Start an empty project, or open one made for the game, which the
editor fills from discs you own.

**It is experimental.** Expect rough edges, and formats that change without notice.

## Features

- **Non-destructive editing** in layers.
- **Full history**: undo anything, and look back at any earlier point.
- **Edit in any program**: the project is an ordinary folder.
- **Incremental builds**: only what changed is rebuilt.
- **Extensible** with sandboxed WebAssembly extensions.
- **Works with AI agents.**
- **Fast**, even on large projects.
- **Scriptable** from the command line.

## Platforms

Built and tested on Linux (x86_64, including WSL) and Windows (x86_64). macOS builds are not tested. See
[installing](docs/getting-started/install.md) for what each system needs.

## Getting started

Build it (below), then:

```bash
pe2-editor                          # then File ▸ New project… for an empty one
pe2-editor path/to/project          # or open a project you have
```

[The manual](docs/index.md) covers the rest. It ships beside the programs.

## Building

The workspace uses the nightly Rust toolchain named in `rust-toolchain.toml`; `rustup` installs it on first use.

```bash
cargo build --release               # pe2-editor and pe2-cli, in target/release/
cargo xtask dist                    # laid out as a release, in target/dist/pe2-<version>-<platform>/
cargo xtask package                 # the same, as archives with their SHA-256 sums
```

A release holds the two programs, the user documentation, `LICENSES.txt`, and the extensions that ship beside
the editor rather than inside it (`pe2.psx`, `pe2.ffmpeg`).

## Extensions

Most extensions are built in. The two that run programs on your machine, `pe2.psx` (the game's compiler) and
`pe2.ffmpeg` (video), ship beside the editor and run only if you trust them. See [extensions](docs/extending/extensions.md).

For extension authors: `cargo xtask hash <manifest> --write` fills in the checksums of what a manifest downloads,
and `cargo xtask pins` checks that every pinned download is still there. What `pe2.psx` and `pe2.ffmpeg` download is
built from source by `cargo xtask packages` (the zig projects in `packages/`) and published as this repository's
releases, one a package version.

## Working on the editor

Before a change is done:

```bash
cargo clippy --workspace --all-targets
cargo test --workspace
```

Every warning is an error, Clippy's pedantic lints included.

The workspace:

```text
crates/
  editor/         pe2-editor: the window, its panes, and the worker that holds the project
  cli/            pe2-cli: the same from a terminal
  project/        projects: layers, the index, the history, filling by plans
  holder/         holding a project, with a window or without: the host process, epochs, watching layers
  vfs/            the project served as a folder, by whatever the system offers
  sfs/            the state the project's processes share: a write-ahead log, shared pages and trees
  mcp/            the project served to agents
  ext/, ext-macros/  the extension SDK and its WIT contract
  setup/          downloading what an extension needs: checked, unpacked, patched
  formats/        the game's file formats, read and written byte for byte
  codecs/         the game's encodings to editable forms and back
  model/          models, as edited and as the game stores them
  typeface/       the game's fonts, as UFO fonts
  value/          node fields in memory
  webgpu/         WebGPU for extensions
  licenses/       what the programs are built with, gathered as they build
extensions/       the extensions, built in and shipped apart
xtask/            dist, package, hash, pins, packages
packages/         what the extensions download, built from source with zig
docs/             the user documentation
design/           how each part is built, for whoever changes it
licenses/         license policy, and license texts no package ships
```

Start with [`CLAUDE.md`](CLAUDE.md), which maps [`design/`](design/) - how each part is built - and states the
rules every change is held to.

## License

CC0-1.0 ([`LICENSE`](LICENSE)): no rights reserved. The libraries it is built with are under their own licenses,
all permissive; `pe2-cli licenses` lists each with its text, as does Settings ▸ Licenses in the editor
([details](docs/reference/licenses.md)).
