# The index and the model

`project::index`, `project::model`.

## The index

The `Index` is the resolved project: every entry with the layers that make it, every asset assembled from its entry
and its files, the links between assets (references by uid), and the problems found along the way. Everything that
reads the project reads the index; only whoever holds the project has one that changes.

### Opening

Opening reads what each layer holds, applies the stack of layers, assembles assets on every core, and links them.
What a records layer holds is what its records show, taken whole from them (`records::Set::shown`): its tree is on no
disk, and is never walked (a generated layer is records too, this machine's alone); a mount's tree is walked, and what
the walk finds that its records do not list (or list and do not find) is written into them (`list_unlisted`).
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

### Names

Paths of the project are `TreePath`s (`pe2_path::VfsPath<Tree>`), files within an asset `RelPath`s, uids `Uid`s:
checked where they come in - a changeset or record read, an edit asked, a file a walk finds - and never text after. The
model keeps a uid as its number (a record's key is its layer's name and the uid's eight bytes, most significant first),
and a reference written with a uid that is none as the text written beside a 0.
What a layer holds by a name no path takes is a problem of the project's (`Problem::Refused`), it and what is
inside it left out; nothing is renamed or passed over unsaid.

Names compare as the served folder must hold them on every system: by their fold (`pe2_path::folded`), each
character's full case folding of its simple uppercase, in canonical decomposition - what full case folding takes as
alike, and what NTFS takes as alike (`ı` and `i`), are one name, and so is a name in either normal form. Equality,
hashing and order all go by it, with a path ordered name by name, so what is in a folder follows it; the spelling
written is what is kept and shown. The table is made as the crate is built from the Unicode Character Database
`cargo xtask prepare` fetches, and the model names its version (`UNICODE_VERSION`): one made with another is filled
again. An edit making or moving an entry where one is shown already is refused as there already
(`edit::shown_free`), a node compared by its asset's name, and renaming an entry to another spelling of its name
changes its spelling. Two a walk finds in one folder, two files of a record, two records placed at one name are each
refused and said - whichever the system listed first (`index::apart`, `records::placed_alike`).

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
