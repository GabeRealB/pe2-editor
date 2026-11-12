# Building, testing and measuring

## Building

The workspace builds with the nightly toolchain in `rust-toolchain.toml`, which also brings the `wasm32-wasip3`
target the extensions are built for. A normal `cargo build` builds the editor, the command line, and - through
`crates/extensions/build.rs` - the built-in extensions as components, which it embeds.

Every warning is an error, Clippy's pedantic group included (`[workspace.lints]`). `cargo xtask` lays out and
archives releases, and helps extension authors pin their downloads (see the README).

## Testing

```
cargo clippy --workspace --all-targets
cargo test --workspace
```

on Linux and on Windows. Three kinds of test matter most:

- **The edit harness** (the tests of `project::edit`). Every edit a test makes is checked against the truth: the
  served tree updated by delta against the whole, the shared filesystem against a follower of its commits, the model
  against the index, and the index changed in place against one opened afresh. Every new kind of edit goes through
  it. A test names project files as the project does, by their real folder (`project::testing::real`).
- **Editor scenarios** (`crates/editor/src/scenario`). The real editor, driven headlessly through its accessibility
  tree with real pointer and keys, asserting on what is written, the problems and the labels - never on pixels. Each
  scenario runs in a project and data folder of its own, with no network.
- **Extension test suites.** A test extension drives the editor through `pe2.test`, a host extension present only in
  test mode, on its own thread while the editor keeps drawing. Suites run in `pe2-test-runner` (built with the
  `tests` feature, never shipped), which the Tests pane starts and reads, so a hanging test cannot take the editor
  with it.

The concurrency-heavy parts have harnesses of their own: the shared filesystem's tests kill writers and readers at
every step of a transaction, and run a second process against the same space.

## Measuring

Coarse work carries `tracing` spans (an extension instantiated, a job, a plan operation, the window's own work).
`pe2-cli --profile` prints what a run spent its time on; the editor reports the same to agents; the `tracy` feature
streams spans to Tracy.

Benchmarks that need a real project or folder are ignored tests, run by hand with `--release -- --ignored`
(`vfs/tests/bench.rs` and a few in the shared filesystem and the model).

## Platforms

Linux and Windows are built and tested; macOS builds are not. Platform-specific code is mostly in the filesystem
backends, in how paths are handed to extensions (Windows drives become roots in an extension's filesystem), and in
how processes are started (`CREATE_NO_WINDOW`). `PE2_TEST_SCOPES` runs the filesystem tests through a chosen backend,
and `PE2_LICENSES_FOR` checks another platform's dependencies against the license policy.
