# The extension runtime

The host side: `crates/project/src/ext` (registry, components, trust), `crates/extensions` (finding and loading,
the built-ins). The guest side is the SDK, `crates/ext`, documented for authors in the manual's "Writing extensions".

## The contract

The boundary is a WIT package, `pe2:extension` (`crates/ext/wit`). The extension exports `guest` (describe yourself,
register, and calls by handle); the host provides `host` (assets, the project, the UI frame, writers),
`unsandboxed` (processes) and `gpu` with `wasi:webgpu`.

Closures cannot cross the boundary, so `register` returns a description, and every callback - an operation, a view, a
field's formatter, a tool - is a handle the host calls back with.

The SDK is the same Rust API whether built for `wasm32-wasip3` or linked natively into the editor. Built-ins can
therefore be linked for debugging and profiling (`PE2_EXTENSIONS=linked`); everything else always runs as a
component.

The contract has a version, checked before anything is compiled. Below 1.0 a minor version is a line; within a line
versions only add host functions, and any change to the guest or to a type starts a new line. The error says whether
to update the editor or rebuild the extension. Until the first release the version stays at 0.1.0 and changes
freely.

## Finding and loading

Extensions are found in three places: held by the project (sealed in a layer), the user's extensions folder, and
beside the editor. Only the project's run; the others are offered to add, and adding copies them into the project.
The built-ins are embedded in the programs.

Loading resolves what is found into the registry, `Extensions`:

1. Of several copies of an id, the nearest wins: the project's over a file's over a built-in.
2. Dependencies (`requires`, `weak`, `optional`, and services through `provides`) are resolved to a fixpoint. An
   extension whose requirement is missing, off or too old is disabled with its dependents, and the reason is kept.
   Cycles are refused.
3. Each registration is checked: everything must be in the extension's namespace, once.

Every extension found is kept with its status (loaded, available, not trusted, replaced, refused and why), which is
what the Extensions pane shows. Results are cached per process by what was found.

An extension whose manifest is embedded is compiled and instantiated only when it actually registers, so a disabled
or unmet extension never runs. Compiled code is cached per user, keyed by the bytes and the engine's settings.

An extension may also be only its manifest - a folder with `extension.toml` and no component: a tool, the programs it
offers and what they are fetched from. It registers nothing; it is checked as one with a component is, its manifest
by the same checks the SDK's macro runs (`pe2-manifest`, which both use), and trusted by its folder, its manifest
counting, as that is what it is.

## Running a component

- **One engine per component**, deterministic (canonical NaNs, deterministic SIMD), interrupted by epochs that one
  thread ticks for every engine.
- **Deadlines** by kind of call: about a second for a question, two for a view frame, ten for long work, an hour
  outside the sandbox.
- **Memory** is capped per instance, raised by what the extension declares, within a hard limit.
- **Instances follow the job, never a timer**, because a guest's memory never shrinks: a fresh instance per
  operation (handed on along a chain while it stays small), one shared instance for reads that is reclaimed when it
  goes unused or grows, one for views. An instance that traps is dropped.
- **Lending.** Host objects (an asset, the project, a UI frame) are put in the resource table for the length of one
  call and taken out before it returns, so a guest can never keep one.
- **A content crosses as what it is:** a few bytes in itself (`inline`, a `list<u8, 30>` and how many), bytes the
  host holds by a handle (`memory`, a `buffer`), or a stored blob by its SHA-1 (`stored`, a `list<u8, 20>` and its
  size) - fixed-length lists, which the engine is set to take (`Config::wasm_component_model_fixed_length_lists`). A
  list is copied whole into whoever receives it, so bytes cross by handle: an operation is given its inputs' contents
  without their bytes and copies in what it reads (`buffer.read`, `read-stored-range`); what a component makes it
  hands over once (`buffer`'s constructor). One `buffer` serves contents and media alike (`HostBuffer`, an
  `Arc<[u8]>` shared by every handle). A hash is a `Sha1` on both sides and in the project's records, the store and the
  model: text only where it is written out.
- **A content is placed for a program** (`place-content`, `PlacedContent`): a stored blob where it is kept (a chunked
  one served or assembled, `content::for_program`); one held in itself or in memory written to a file of its own,
  removed when the handle is dropped - which the guard holding it does once the program is done (`exec::Written`).
- **Names are types on both sides, text between.** The contract carries an asset's path, a reference, an output's
  address and a file's name (`RelPath`, within an asset or a value) as strings; the SDK and the host check each into
  its `pe2-path` type where it arrives. An operation's lent asset holds no files by name: its values' contents are
  read by their hashes (`read-stored`). What a guest sends that is no path or reference is refused as the call's error (`component.rs`, `tree_of`, `reference_of`);
  what the host sends that is none is a broken host, and the guest traps (`guest.rs`, `told_path`).
- Calls block their thread. They must not be made from an async runtime's thread.

A sandboxed component gets WASI with nothing behind it: clocks fixed, no environment, files or network. What it
printed to standard error is kept and added to the error if it traps.

## How calls arrive

- **Operations** run in the work graph's evaluator, which gives them a `CallHost` to read content and write outputs.
  An extension that provides a service also has its operations registered under the service's name.
- **Services** are called through the registry, which allows a call only to the extension itself, or to one it
  requires.
- **Views and panels** are drawn through a component view each frame; **tools** are called for agents; **hooks**
  run as operations before an event's work.
- Views that need outputs ask without blocking (`requests`): a small pool makes them within a frame's budget, one
  making per output shared by everyone asking.

## Outside the sandbox

An unsandboxed extension gets the whole system and may start programs. It runs only once trusted:

- **Trust** is a hash of everything in its folder that runs (docs, licenses and tests excluded). It is trusted if
  that hash is recorded, if its folder lies in a place the user trusts, or for one run with `--trust-everything`.
  Trust is kept per user, never in a project, so a project cannot bring trust with it. Until trusted, an extension is
  read for its manifest and never instantiated.
- **Programs** are looked for in the extension's folder (per platform first), then its downloads, then the system.
  Programs given files from a sealed extension get them through the served filesystem.
- **Places on disk** cross the boundary as handles, never as text: the `host-path` resource, given by the calls that
  name a place (`temp`, `keep`, `place-content`, `serve`, `place`, a build's folder), offers the form the component opens
  (`guest`), the form a program is told (`native`), and a place within it (`join`). `spawn` takes its arguments as
  `arg`s - text, a place, or parts joined into one - and the host writes each place as the program reads it, so no
  extension writes a path for another system's programs.
- **Programs offered** (`[[program]]`) are named by others as `<extension>:<program>`: what every unsandboxed
  extension loaded offers is kept with the loaded set (`unsandboxed::Offered`, filled once it is known which loaded),
  and a program is started through the extension offering it - its required downloads fetched first, its tree's places
  found, other extensions' programs it names (`@<extension>:<program>`, maspsx's assembler) found in turn. So a tool is
  an extension of its own, and what builds - another extension, or a project's `run` boxes - names it.
- **Downloads** (`[[setup]]`) are fetched by the host, not the extension: checked by hash, unpacked, patched, and
  kept in a store shared by every project. Required ones are fetched before the first call, optional ones when the
  extension asks and the user allows.

## The built-ins

`crates/extensions/build.rs` builds the built-in crates as components in a nested cargo run and embeds them. The
extensions that need trust, and the test suites, are built but not embedded: nothing that needs enabling ships inside
the editor. Adding a built-in means adding it to the build list and to the linked list; a test keeps the two in step.

## Where to look

`project::ext` (`Extensions`, loading, `may_call`, hooks, `program`), `ext::component` (wasmtime, limits, instances,
`Unsandboxed::start`), `ext::folder` (`checked_alone`), `ext::unsandboxed`, `ext::requests`, `crates/extensions`
(finding, trust, settings, built-ins, `manifest_alone`), `crates/manifest` (what a manifest may say), `crates/setup`
(downloads).
