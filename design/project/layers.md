# Layers and storage

`crates/project`: `records`, `chunks`, `pack`, `content`, `merge`, `edit`.

## The model

A project is a `project.toml` and a stack of layers. The index resolves them bottom to top into one tree of
**entries**: folders, plain files, and nodes (a node is a folder with a declaration, `node.toml`, plus its files). A
node and the folder of its name beside it in a layer are one: a node with what is inside it.

A layer only ever says what it does to the layers below: create an entry, override one, move or delete it. Nothing
below is copied or changed.

## How layers are stored

| Kind | Storage | Why |
|---|---|---|
| A project's layers | **Records**: one file per entry it touches, by uid, in the layer's folder (`layers/<layer>/<aa>/<uid>.toml`, beside its `layer.toml`), its tree addressed under `layers/<layer>/tree` on no disk | Nobody edits them by hand: what they hold - assets, and plain files such as code - is edited through the served folder with any tool; renaming a folder of thousands of entries is one record; text stays text in version control. |
| Mounts | **Records** listing a plain tree of files outside the project (`[[layer]] mount = "<folder>"`): each entry's uid, where it is and what it is, none of its files; its `layer.toml` says what it leaves out (`.git/` as it is added) | Worked on with their own tools, where they are: what another program adds, moves or removes there is listed so as it is found - a file moved keeps its uid by its content - as one set said in the log. Edits write the tree's files in place. At the bottom of the stack, below what builds on them. |
| Generated layers | **Records** too - an override of each node a plan gives files - this machine's alone: in `.local/layers/<layer>/`, what they hold in `.local/store/` | Made of the user's own discs by their plan, which alone writes them: never shipped. |

A tool hands a layer over as a tree of files and its listing (`pe2-cli project layer import`): read as it is, by
no index - a `<name>.node` folder a node, another folder a folder, another file a plain file, each by the uid its
listing gives it - and written as one set said in the log, only the records that differ; `layer export` writes a
layer back so. An override of a plain file holds a copy of it at once: its content is the file.

A record says what it does to its entry, where the entry is (parent uid and name), and its files: text inline, other
bytes by hash in the store. Each layer and record says its format; a format a program does not know is refused,
never guessed at. Everything reads a layer's content through `project::content`, never by path, so callers do not
care how it is stored.

## The store

Large files (1 MiB and over) are split into content-defined chunks, kept once by hash, so an edit stores only what
changed and identical data is kept once across layers and releases. Whole objects and trees of objects (an extension)
are kept by hash too. A project has two stores: the shipped one (`store/`, at its root) holds what the shipped records
name; this machine's (`.local/store/`) holds what only it names - a generated layer's files, the version below each
override that it was made from (its base, to merge with), what the history or a conflict kept aside could put back. A
content is read from whichever holds it (`chunks::Store::other`).

The sweep (`chunks::sweep_settled`, daily while the editor holds a project, and `pe2-cli project store gc`) reads what
names each content - records, a plan's last run, the history's and the conflicts' records - parsed, never guessed at
from text; one it cannot read stops it, removing nothing. What a shipped record names that only this machine's store
has (an undo put the record back) is shared again; what the shipped store holds that only this machine names moves to
its store; then each removes what nothing names, stored longer ago than a write takes to name it. What moves is copied
whole before anything of it is removed, so a sweep cut short loses nothing still named.

## Where a project keeps itself

Beside `project.toml`, what is shipped with the project: `layers/` (each layer's folder, its changeset beside it, a
generated layer's plan) and `store/`. And `.local/`, what of it is this machine's alone, which the `.gitignore` a new
project is made with leaves out (`project::local`):

| Part | What | Lasts |
|---|---|---|
| `processes/` | the project lock and its owner, the sockets, locks and logs of the host and the server, the editor's socket for agents, the scopes and access records of what is served | as long as the processes |
| `filesystem/` | the shared filesystem's log and the space its processes share | until applied, and as long as it says its own format |
| `history/` | the session's history: steps, blobs, journal, trash | until the session closes cleanly |
| `conflicts/` | what writes that met a change made meanwhile left aside | until the user settles it |
| `layers/` | the records of the layers that are this machine's alone: a generated layer's | as long as the layer |
| `store/` | what those records hold, by hash: what a plan made of the discs | as long as something names it |
| `outputs/` | what is derived from the layers: the work graph's store, imports, merges, materialized files | until another schema or version opens it: the only part wiped |
| `set-aside/` | what the served folder held that was not the project's | until the user removes it |
| `settings.toml` | this machine's settings of the project (where its unmanaged mounts are, links to what builds deploy, how it is served) and the editor's view of it (`[editor.layers]`, `[editor.fill]`) | always |

What finds the project a file of a layer is in (its store, its outputs) asks the projects the process opened
(`project::holding`), never the names of the folders between: a folder of a tree may be named as a project's are.
Programs outside the editor ask `pe2-cli project paths` where a project keeps what they reach of it.

## Overrides and merges

An override holds only the files it writes, whole, and records the hash of the version below each was made from.
When the version below changes, the index merges three ways, by what the file is - TOML by field, other text by line,
bytes not at all. A conflict leaves the layer's side shown and the override stale until the user keeps or drops it.
Merges are cached by what they were made from; diffs are never stored.

## Sealed nodes and extensions

A node's record may hold a tree in place of files (`tree = "<sha1>"`, `Record::tree`): a sealed node, kept whole as
one tree of objects in the store, its files the tree's (`Content::Object`), never merged and never written into - an
edit writing one is refused. It is an entry like any other: placed anywhere, moved, renamed, deleted; a higher layer
replaces it whole by overriding it with another tree, removing what the one below had and its tree has not. A sealed
node whose tree holds an extension's manifest is an extension the project holds (`Project::held_extensions`), by what
it holds, wherever it is and however it is named; one is added in `extensions/`, named by its id. See [The extension
runtime](../extensions/runtime.md).

## Edits by path

`vfs::ops::Op` names edits by the paths the served folder shows: write, mkdir, rm, mv, override, drop, keep mine,
take base... The command line, agents and the folder all edit through them, so there is one set of edit operations
whoever asks.

## Values

A node's fields and settings are held in memory as `pe2_value` values: strings interned, numeric arrays packed,
tables shared and copied only where changed. They are written as TOML.

## Where to look

`project::records`, `project::chunks`, `project::pack`, `project::content`, `project::merge`, `project::edit`,
`vfs::ops`, `crates/value`.
