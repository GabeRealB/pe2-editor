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
| Foundations | `value`, `sfs`, `formats`, `codecs`, `typeface`, `setup` | Values, shared storage, the game's formats and their editable forms, downloads. Know nothing of projects. |
| Extensions' contract | `ext`, `ext-macros`, `webgpu`, `model` | The SDK and its WIT contract, WebGPU for extensions, and models, which are built on the SDK's types. |
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
