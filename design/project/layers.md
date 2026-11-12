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
| Asset layers | **Records**: one file per entry it touches, by uid, in `.layers/<layer>/.records/` | Nobody edits them by hand; renaming a folder of thousands of entries is one record; text stays text in version control. |
| Code layers, mounts | Plain trees of files | Worked on with their own tools. |
| Generated layers | **Packed**: one journaled listing, contents in the store | Written only by their plan; nothing to scan. |

A record says what it does to its entry, where the entry is (parent uid and name), and its files: text inline, other
bytes by hash in the store. Each layer and record says its format; a format a program does not know is refused,
never guessed at. Everything reads a layer's content through `project::content`, never by path, so callers do not
care how it is stored.

## The store

Large files (1 MiB and over) are split into content-defined chunks, kept once by hash in the project's store, so an
edit stores only what changed and identical data is kept once across layers and releases. Whole objects and trees of
objects (an extension) are kept by hash too. Garbage collection removes what no record, listing or history step
names.

## Overrides and merges

An override holds only the files it writes, whole, and records the hash of the version below each was made from.
When the version below changes, the index merges three ways, by what the file is - TOML by field, other text by line,
bytes not at all. A conflict leaves the layer's side shown and the override stale until the user keeps or drops it.
Merges are cached by what they were made from; diffs are never stored.

## Sealed extensions

A layer can hold an extension. It is sealed: kept whole as one tree of objects in the store, named by its hash,
never merged, read-only. A higher layer's copy replaces a lower one's. See [The extension
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
