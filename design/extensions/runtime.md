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
- **Downloads** (`[[setup]]`) are fetched by the host, not the extension: checked by hash, unpacked, patched, and
  kept in a store shared by every project. Required ones are fetched before the first call, optional ones when the
  extension asks and the user allows.

## The built-ins

`crates/extensions/build.rs` builds the built-in crates as components in a nested cargo run and embeds them. The
extensions that need trust, and the test suites, are built but not embedded: nothing that needs enabling ships inside
the editor. Adding a built-in means adding it to the build list and to the linked list; a test keeps the two in step.

## Where to look

`project::ext` (`Extensions`, loading, `may_call`, hooks), `ext::component` (wasmtime, limits, instances),
`ext::folder`, `ext::unsandboxed`, `ext::requests`, `crates/extensions` (finding, trust, settings, built-ins),
`crates/setup` (downloads).
