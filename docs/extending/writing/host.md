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

An output is asked for by an `Asked`: its path and, after `?`, its query - `pe2_ext::NATIVE` and `pe2_ext::SETTINGS`,
which every asset has, `Asked::constant("page")` for a name written in the code (checked as it is compiled), or
`Asked::new(text)?` for one read from somewhere. A reference held in a field is read with `target_of` (the asset it
names), `asked_of` (what of it, and how), `address_of`, `query_of(key)` and `fragment_of` (the part read), whether it
is written as text or as a table.

Only outside the sandbox: `temp()` gives a place a program can write to, `keep(place)` takes that file back into the
project, `path_of(file)` says where a stored file is on disk, and `serve(files)` hands a program a read-only folder
of files. A place is a `HostPath`: `guest()` is how the extension opens it, `native()` how a program is told it, and
`join(path)` a place within it. See [Running programs](unsandboxed.md).

## In views, tools and services

Views, tools and services reach the project through `Project`:

| To | Use |
|---|---|
| Find assets | `asset`, `resolve`, `find`, `selection`, `chosen` |
| Read what an asset makes | `picture`, `image`, `table`, `bytes`; `request` to ask without waiting |
| Know the project | `target`, `revision` |
| Call a service | `call(service, arguments)`: one the extension provides, or of an extension it requires |

An `Asset` gives its `path`, `uid`, kind, fields, parts and files.

## Limits

In the sandbox an extension has no files, network or environment of its own, and its clock does not move. Each
call has a time limit: a second to answer a question, two to draw a view, ten for long work.

When a call fails, what the extension printed to standard error is shown with the error. There is no progress
reporting yet.
