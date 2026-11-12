# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this folder.

## What this is

The PE2 editor: a Rust workspace with two programs, `pe2-editor` (the window) and `pe2-cli` (the same from a
terminal, scripts and agents), and the extensions they load. It works on projects - an empty one it makes, or one
made for the game and filled from the user's own discs; it ships with no projects and nothing of the game. It is
incomplete and experimental. `README.md` says what it is and how it is built; `LICENSE` is its license (CC0-1.0).

**This folder is self-sustained.** Nothing in it refers to anything outside it: no path, file, tool, document or
build of the repository around it - in code, comments, tests, examples, docs and design files alike. What the
editor needs is here; what it reads of a project comes from the project, or from the plan a project is filled by.
Where something here mirrors something elsewhere (a port of the game's code, a format another program also
writes, a lock another program also takes), it says what it is and what it does, never where the other one is;
whoever works on the other side keeps the two in step from there.

## Where things are written

How the editor is built is written in `design/`, a developer guide: `design/index.md` is its contents, starting
with `architecture.md` (programs, processes, crates, the life of an edit and of a read, principles) and
`engineering.md` (building, testing, measuring), then a folder per area:

| Folder | What it covers |
|---|---|
| `design/project/` | layers and storage, the index and the model, history and documents, plans and generated layers |
| `design/processes/` | holding a project (`holder::core`, the host, the lock), the editor (window and worker), agents |
| `design/filesystem/` | serving the project as a folder (`crates/vfs`), writes through it, the shared filesystem (`crates/sfs`) |
| `design/building/` | the work graph, events and epochs |
| `design/extensions/` | the extension runtime (contract, loading, sandbox, trust), the game's extensions |
| `design/licenses.md` | what ships, by its license (`crates/licenses`), and the dependency policy |

Read the page of what you are about to change, and change it with the code. A page says how a part works and why,
as it is now - what it was before is the commits' - at the level of a developer finding their way: names are there to
find the code, not to list it, and it carries no measurement tables.

What a user of the editor reads is the manual in `docs/`, shipped beside the programs (`cargo xtask dist`): its
contents in `docs/index.md`, then seven parts - `getting-started/`, `basics/`, `working/` (a task a page), `making/`
(stacks, the graph editor, events, configurations, a project's own operations), `views/` (what the built-in
extensions show and edit), `extending/` (managing extensions, agents, and `writing/`: the SDK, the manifest, the
API, packaging, testing) and `reference/`. It says what a user can do, how, and what it affects, in the words the
window uses; never how the editor does it - that is `design/`'s. An extension shipped apart from the editor keeps
its own guide in its folder. A change a user or an extension's author would notice - a command, a pane, a setting,
a shortcut, a flag of `pe2-cli`, something an extension registers or calls - changes its page with it.

## Rules

What holds whatever is being changed:

- **The editor is standalone.** It knows nothing of how a project was made and runs the plan it is given.
- **A layer's folder is storage, not a place to edit.** A project is edited through the editor, its served folder
  (`assets/`) or `pe2-cli`; what a tree layer holds against its changeset is a problem the index reports, never an
  edit.
- **A project is made by the editor's commands** (`pe2-cli project new`, `layer add`, `mount add`, `config add`,
  ...), never by writing its `project.toml`.
- **No reader of an old format is kept before the first release**, and the contract stays 0.1.0: a format a
  program does not know is refused, and `pe2-cli` converts one way.
- **One thread holds a project** (`holder::core`): what the host and the editor both do of holding one is written
  there once, not in either.
- **The editor's window displays and dispatches.** What changes the project is asked of its worker
  (`Workspace::ask`) and not waited for; its thread reads and writes no file of the project as it draws.
- **The server serves files and nothing else**: it reads no index and loads no extension
  (`project::ext::forbid`), so that it may load what a process running an extension's code must not be one with.
- **A backend is a module implementing `Backend` and a line of `backend::all`**; nothing else names one. One
  named in the settings that cannot do the job is refused with why, never replaced by another.
- **On Windows, a program started without a console is started with `CREATE_NO_WINDOW`**, never
  `DETACHED_PROCESS`: a process with no console at all has Windows open a terminal for every console program it
  starts.
- **What is transcribed from another project's header says so in its file** (Dokany's declarations in
  `crates/vfs/src/dokan.rs`), and is listed as bundled in `licenses/licenses.toml`.
- **A dependency is settled when it is added**: the build fails by name on a crate under a license
  `licenses/licenses.toml` does not allow, or with no license text to show. Nothing whose license asks anything of
  what is loaded beside it is allowed: the editor loads extensions of any license. A crate only another platform
  links is checked from this one with `PE2_LICENSES_FOR=<target triple> cargo check -p licenses`.
- **A test names a project's files as the project does** (`project::testing::real`): by its real folder, part by
  part, not a folder joined with `a/b`. Every edit the project crate's tests make is held to an index opened
  afresh and to the project's model; an edit added goes through the same harness.
- **A test or an example takes what it reads from what it is given**, never from a place outside the workspace:
  an example that needs a release's discs or an extraction takes its folder as an argument.

## Checking

```bash
cargo clippy --workspace --all-targets
cargo test --workspace
```

on Linux and on Windows. Every warning is an error (`[workspace.lints]`), Clippy's pedantic group included;
`cargo fmt` follows `rustfmt.toml`, by the file changed, never `--all`.

`pe2-editor` takes `[PROJECT] [--select ASSET] [--show PANE] [--screenshot FILE] [--agents-drive]
[--wait-for-agent] [--trust-everything]` and nothing else. To see a change in the window, open it on a copy of a
project with a copy of the editor's data folder (`XDG_DATA_HOME` on Linux, `APPDATA` on Windows), drive it through
`pe2-cli mcp` (`editor_ui`, `editor_act`, `editor_screenshot`), and close it with its Quit command (`file.quit`).
