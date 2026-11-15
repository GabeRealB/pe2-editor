# Calling the editor

## While an operation runs

An operation's `run` receives a `Call`:

| To | Use |
|---|---|
| Read its inputs and settings | `take`, `take_all`, `parameter`, `query`, `settings`, `given` |
| Make its outputs | `make`; `writer` streams a large file to the editor's store |
| Read other assets and their outputs | `output_of`, `outputs_of`, `table_of`, `tables_of`, `node_named`, `assets_of`, `entries`, `read_entry` |
| Know what is being built | `config()`: the configuration's name, release, target and properties |

Everything an operation reads is recorded, so the editor runs it again when, and only when, what it read changes.

An asset is named by a `Reference`: its uid, or its path in the project's tree, a `TreePath`. A path or a reference
is never text: one written in the code is `TreePath::constant("packages/aya")` (checked as it is compiled), one read
from somewhere `TreePath::new(text)?` or `Reference::new(text)?`, which fail on what names nothing, and `Reference::from`
a uid or a path. What the calls give back are these types: `entries` the paths of a folder's files, `assets_of` each
asset of a type by its path with its declaration, `node_named` an asset's path, uid and types.

A file's name - within an asset (`asset.read(name)`, `has_file`), within a value (`File::name`, `File::new`), or one an
operation makes (`writer(name)`, `keep(place, name)`) - is a `RelPath`: `RelPath::constant("native.bin")` in the
code, `RelPath::new(text)?` for one made or read, so `objects/a.o` is a path, not text with slashes in it. What an
extraction makes is `Made`, its files each by that name; a reference written with its bundle's path
(`{ uid, path = "res://<path>.node" }`) is read with `declared_path`.

A file's content (`File::content`) is a few bytes held in itself (`Content::Inline`, up to 30), bytes the editor
holds in memory (`Content::Memory`, a `Buffer`), or a blob the editor keeps by its SHA-1 (`Content::Stored`, a `Sha1`:
twenty bytes, written as forty hex digits). `Content::of(bytes)` and `Content::owning(vec)` make one of bytes,
holding in itself what fits. An extension is given its inputs' contents without their bytes: `call.read(file)` reads
one whole, `call.read_range(file, offset, len)` only part of it, and `local()` gives what a content holds in itself
or in memory - none for a stored one. A `Buffer` an extension makes hands its bytes to the editor once.

An output is asked for by an `Asked`: its path and, after `?`, its query - `pe2_ext::NATIVE` and `pe2_ext::SETTINGS`,
which every asset has, `Asked::constant("page")` for a name written in the code, or `Asked::new(text)?` for one read
from somewhere. A reference held in a field is read with `target_of` (the asset it names: none where the field holds
no reference, an error where it names nothing an asset can be named by; `target_in` for a field that may be missing),
`asked_of` (what of it, and how), `address_of`, `query_of(key)` and `fragment_of` (the part read), whether it is
written as text or as a table.

Only outside the sandbox: `temp()` gives a place a program can write to, `keep(place)` takes that file back into the
project, `place_content(file)` puts a file where a program can read it - a stored one where it is kept, one held in
itself or in memory written to a file, removed when what it gives (a `PlacedContent`) is dropped, so hold it until
the program is done - and `serve(files)` hands a program a read-only folder of files. A place is a `HostPath`: `guest()` is how the extension opens it, `native()` how a program is told it, and
`join(path)` a place within it. See [Running programs](unsandboxed.md).

## In views, tools and services

Views, tools and services reach the project through `Project`:

| To | Use |
|---|---|
| Find assets | `asset`, `resolve`, `find`, `selection`, `chosen` |
| Read what an asset makes | `picture`, `image`, `table`, `bytes`; `request` to ask without waiting |
| Know the project | `target`, `revision` |
| Call a service | `call(service, arguments)`: one the extension provides, or of an extension it requires |

An `Asset` gives its `path` (a `TreePath`), `uid`, kind, fields, parts and files; `Project`'s calls take an asset's
`TreePath`, and `Ui::navigate(path, part)` opens one, a part of it selected where one is named.

## Limits

In the sandbox an extension has no files, network or environment of its own, and its clock does not move. Each
call has a time limit: a second to answer a question, two to draw a view, ten for long work.

When a call fails, what the extension printed to standard error is shown with the error. There is no progress
reporting yet.
