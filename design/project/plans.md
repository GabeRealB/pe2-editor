# Plans and generated layers

`project::plan`, `project::discs`, `project::extract`, `project::pack`.

## The idea

The editor knows nothing of how a project was made. A project for the game arrives with its nodes and a **plan**
for each generated layer: what to read from the discs, and what to make of it. The editor runs the plan; it never
decides what to extract.

A plan holds locations, sizes and hashes - never the discs' bytes - so it travels with the project, and the user
provides the discs.

## What a plan is

A JSON-lines file beside the layer (`<layer>.plan.jsonl`):

- a **header**: the extensions it needs, and for each release the discs it reads, by name, size and hash;
- **operations**: each with an id, taking earlier ids as inputs, with arguments and the hash expected of its result.
  Host operations read discs, containers, chunks and streams; any extension's operation can be used too. Alternatives
  across releases are written as `any`;
- **emits**: which operations' results become which files of which node.

## Running it

Each operation has a key made from what it is, its arguments, what it expects and its inputs' keys, rooted at the
discs' hashes - all computable without the discs. A run:

1. reads the record of the last run;
2. finds the files whose key changed, or that are missing;
3. runs each operation they need once, in parallel, checking each result against its expected hash;
4. keeps what it makes in the local store (`.local/store`), by hash;
5. writes the generated layer's records - an override of each node it gives files, holding them by hash, and none of
   what the plan no longer emits - as one set, only what differs;
6. writes the new record beside them (`plan.record.jsonl`).

What a plan makes is of the user's own discs: the layer's records and what they hold are this machine's alone
(`.local/layers/<layer>/`, `.local/store/`), never shipped with the project; the plan, which holds no bytes of the
discs, is (`layers/<layer>.plan.jsonl`).

So running a plan again makes only what changed, a stopped run resumes where it stopped, and a disc that is not the
expected one is refused by its hash, whatever its name. A plan never writes declarations: the nodes belong to
whoever made the plan.

## Releases

Discs are found by size and hash in the folder each release says, which each machine sets for itself. A run reads
the releases whose discs are all found (or all, or a chosen list). A layer filled for some releases only is
*partial*; files that are identical across releases are kept once.

A plan's state - none, empty, filled, partial, stale and why - and a check that every file is as the plan says (in
the local store, still of its hash) are both worked out without the discs (`pe2-cli project status`, `verify`).

## Generated layers in the index

A generated layer takes no part in the stack of layers: it fills the nodes of another layer, and what it holds at a
path is that node's too. A file it writes that is all data (`settings.toml`) is read beneath the node's own
declaration, so extracted values show as fields without being copied into the user's layers.

## In the editor

The editor works out each plan's state off the window's thread, and offers to fill a layer that is empty or stale.
A plan runs as the only work: after the running epoch, with events that come meanwhile held until what it wrote is
taken in.

## Where to look

`project::plan` (representation, keys, run, state, verify), `project::discs` (finding and reading discs),
`project::extract`, `project::pack`, the editor's `plans.rs` and `fill.rs`, `pe2-cli project extract`.
