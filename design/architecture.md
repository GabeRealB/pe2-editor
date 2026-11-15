# Architecture

## Programs and processes

Two programs are built from the workspace: `pe2-editor`, the window, and `pe2-cli`, the command line. At run time a
project involves up to three kinds of process:

```
        programs reading and writing assets/          agents (MCP)        pe2-cli commands
                       │                                   │                     │
                       ▼                                   ▼                     ▼
   ┌──────────── server ────────────┐            ┌────── holder: the editor or a host ──────┐
   │ serves the folder, nothing else│◀── told ───│ lock, index, history, extensions, builds │
   └───────────────┬────────────────┘            └──────────────────┬───────────────────────┘
                   │         the shared space: log, pages and trees, ring of pages            │
                   └──────────────────────────────┬───────────────────────────────────────────┘
                                                   ▼
                                    the project's files: layers, store, cache
```

- **The holder** has the project: its lock, its only index that changes, its history, its extensions, its builds.
  It is the editor while a window has the project open, otherwise a background host. See [Holding a
  project](processes/holding.md).
- **The server** serves the project as a folder and nothing else. It loads no extension and reads no index. See
  [Serving a project as a folder](filesystem/serving.md).
- **Clients** - commands, agents, the folder's programs - ask the holder, through a local socket or through the
  folder.

They share state through memory-mapped files in the project's cache rather than messages. See [The shared
filesystem](filesystem/shared.md).

## Crates

From the foundations up:

| Layer | Crates | What they are |
|---|---|---|
| Foundations | `path`, `io`, `value`, `sfs`, `formats`, `codecs`, `typeface`, `setup` | Paths as types, the host's I/O, values, shared storage, the game's formats and their editable forms, downloads. Know nothing of projects. |
| Extensions' contract | `ext`, `ext-macros`, `manifest`, `webgpu`, `model` | The SDK and its WIT contract, the manifest checked, WebGPU for extensions, and models, which are built on the SDK's types. |
| The project | `project` | Layers, records, the index, the model, history, the work graph, plans, the extension runtime. |
| Serving | `vfs` | The project as a filesystem: backends, writes, scopes. |
| Extensions | `extensions`, `extensions/*` | Finding, trusting and embedding extensions; the game's support, each an extension. |
| Holding | `mcp`, `holder` | Agents; what a holder does, with or without a window. |
| Programs | `cli`, `editor` | `pe2-cli` (with the host and server), `pe2-editor`. |
| Tooling | `xtask`, `licenses` | Releases, license listing. |

A crate depends only on crates in its own row or the rows above it. The editor knows nothing of the game: everything about Parasite Eve II is in
extensions.

## The life of an edit

1. The user changes a field. The window asks the worker, and keeps drawing ([The editor](processes/editor.md)).
2. The worker records it in the history: a document edit changes the working copy; anything else is a transaction
   ([History and documents](project/history.md)).
3. A transaction is logged and flushed, then written to the layer's files ([The shared
   filesystem](filesystem/shared.md)).
4. The index refreshes for exactly what changed ([The index and the model](project/index-and-model.md)), and one
   commit says what changed in the model and what is served.
5. The server serves the new tree; the window swaps in a snapshot; `files-changed` rebuilds what depends on the change
   ([Events and epochs](building/events.md)).

An edit through the folder starts at the server instead ([Writes through the folder](filesystem/writes.md)) and joins
at step 2; an edit from an agent or a command starts at the holder's link and joins at step 2.

## The life of a read

An asset's preview, an export or a build asks for an output. Its stack has been lowered to a plan; the evaluator pulls
the output, finding it kept if what it read is unchanged, else running the operations - each in an extension - that
make it ([The work graph](building/graph.md), [The extension runtime](extensions/runtime.md)).

## I/O

What the host reads of the outside world and does in it - the time and waits for it, randomness, the environment,
files, watching files, starting programs, the channels between a machine's processes, HTTP and sockets - goes through
an `Io` it is given (`pe2_io`), never the system directly. Each part is a trait: shaped like WASI's interface where WASI
has one (`Clock` like `wasi:clocks`, `Filesystem` and its `Descriptor`s like `wasi:filesystem`, `Http` like
`wasi:http`'s outgoing handler, `Sockets` like `wasi:sockets`), the host's own where it has none (`Watch`,
`Processes`, `Channels`). An `Io` is any mix of parts (`Composed`): the system's (`pe2_io::system()`), or parts a test
drives - a clock that stands until moved (`Standing`), seeded randomness, an environment of its own, changes told,
programs answered, channels within the process, a network that serves only what it is given or none (`Served`,
`Closed`). The traits are object-safe and called through `Arc<dyn Io>`: one virtual call, nothing allocated per call
on the paths that are hot.

The programs make one, as they start: the command line's `run`, the editor's `main` (handed to the window through
`Start`), the agents' server, `xtask`. Owners carry it from there: a `Project` (`Project::load_with`), and through it
the index, history, storage, the cache, the store, the extensions and what runs unsandboxed; the writes, the served
tree, a holder, the host and the server. Code given none reaches nothing: the build refuses the system's own calls
(`clippy.toml`), and where one is right - a measurement, the window's pace, what the processes of a machine find each
other by, the program's own files, what only the system's own calls reach (a backend's mount points), a test's
fixtures - it is allowed where it is made, saying why. A component is given WASI over its extensions' `Io`: a sandboxed
one only fixed clocks and seeded randomness; an unsandboxed one the `Io`'s clocks, randomness, environment and sockets,
and its files through the host's own `wasi:filesystem` (`ext::files`), over the `Io`'s `Filesystem`.

Outside it, and saying so: wasmtime's internals, egui and wgpu, the runtime inside the WebDAV server, what the system
calls into (FUSE, Dokany, ProjFS and Cloud Files callbacks), build scripts and `xtask`.

## Paths

No text is a path: text becomes one through a constructor that checks it, where it comes in, and what names something
past that is one of three kinds, each a type of `pe2_path` or `pe2_io`:

- **What a project names** - an asset by its uid, and what of it an address asks for: a `ResourcePath` (`uid://x/objects`,
  `uid://x/cells/body?palette=night`), its parts `Uid`, `Address`, `Query`, `Asked`. A reference is one; the work
  graph is asked for one.
- **Where something is in the project's tree** - a node, a file of it, a folder: a `TreePath`, which is a `VfsPath`
  typed by the scope it is in (`Tree`; `Run` and `Given` for what a program is given), so one scope's path is not
  taken for another's. Within a folder, names are `Name`s and paths `RelPath`s: portable, so a project made on one
  system opens on another.
- **A place on disk** - where the filesystem is implemented: a `HostPath` (`pe2_io`, re-exported by `sfs`), absolute,
  links resolved, in the system's plain form, so two of one place are equal; joined only of checked names, or of an
  entry the system listed by the name it gave. What is no storage's - an argument, a file the user picks, the
  program's own folders - is a plain `Path`.

A conversion that fails says why (`PathError`, `NotWithin`), and its caller refuses the operation or says the problem:
nothing is passed over, and no other place is taken for the one asked.

## Principles

- **One thread holds a project.** Edits from every source are made in order, on one thread; nothing else changes the
  project.
- **The window only displays and dispatches.** It never touches the project's files and never waits on work.
- **Durable before visible.** A change is in the log before it is answered or shown.
- **Cost follows the change.** Refreshing, serving, imports and builds are proportional to what changed, not to the
  project's size; incremental results are held to fresh ones by tests.
- **Loud, never quiet.** A format a program does not know is refused, a backend that cannot do a job is refused,
  a dependency under a license not allowed fails the build - each by name.
- **The editor is standalone.** It knows nothing of how a project was made; it runs the plans a project carries.
- **I/O is given, never reached for.** Every part of the outside world comes through an `Io`, so any of it can be
  another: a test's, a fault's, a network that is not there.
