# The index and the model

`project::index`, `project::model`.

## The index

The `Index` is the resolved project: every entry with the layers that make it, every asset assembled from its entry
and its files, the links between assets (references by uid), and the problems found along the way. Everything that
reads the project reads the index; only whoever holds the project has one that changes.

### Opening

Opening reads what each layer holds, applies the stack of layers, assembles assets on every core, and links them.
That costs time proportional to the project, so the holder opens over the **model** (below): what the last holder
knew is taken as known, nothing is read, and the files are checked against it on background threads. Whatever
differs - something changed by git or by hand while nothing held the project - is taken in like any change.

### Refreshing

An edit costs what it changes, not what the project holds:

- records are read again only for the files that changed;
- where only one layer's entries came, went or moved, the stack is patched for those entries instead of applied
  again;
- an asset that came or went is linked by itself where nothing else is affected;
- what changed is reported (`Refreshed`) so that what is served, the model and the editor's views follow only that.

The index's maps are its own to change in place. The editor's window gets copies, made only for maps that changed,
after the edit has been answered (see [The editor](../processes/editor.md)).

## The model

The model is what the index reads and derives, kept in the [shared space](../filesystem/shared.md) so the next
holder opens without reading the project again: each file's record (size, time, hash, and what a declaration says),
what each layer holds, the records of record layers, and the applied stack. Assets are not kept: they are cheap to
assemble from the rest.

A refresh's changes to the model are applied in the same commit that says what is served and what became of
what was due, so the folder, the server and the model always agree.

## Holding it to the truth

Every edit the project crate's tests make is held to an index opened afresh, and the model to one built from the
files. A bug in incremental refresh therefore shows up as a test failure, not as drift.

## Limits

An opening still grows with the project (assets assembled and linked, the model read). Making it constant would
mean reading what the index derives from the model on demand, which is not done.
