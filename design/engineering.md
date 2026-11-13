# Building, testing and measuring

## Building

The workspace builds with the nightly toolchain in `rust-toolchain.toml`, which also brings the `wasm32-wasip3`
target the extensions are built for. A normal `cargo build` builds the editor, the command line, and - through
`crates/extensions/build.rs` - the built-in extensions as components, which it embeds.

Every warning is an error, Clippy's pedantic group included (`[workspace.lints]`). `cargo xtask` lays out and
archives releases, and helps extension authors pin their downloads (see the README).

## The packages the extensions download

What the game's extensions download - compilers, assemblers, Python, FFmpeg - is built from its sources, not taken
from whoever built it, so that it lasts as long as the editor does. Each package is a zig project of its own,
`packages/<package>/` (gcc-psx, binutils-mipsel, aro-cpp, python, ffmpeg), whose `build.zig.zon` pins what it is built from
- by URL and content hash - and whose `build.zig` installs what the package holds. What has no zig build upstream is
ported in `packages/ports/`: a build of its own, with what the library's own build decides of each host written down,
and patches where the source needs them. What a source generates with scripts is generated with the programs in
`packages/tools/` (a patch tool, and a small POSIX shell with sed, enough for binutils' scripts), so that building needs
nothing of the machine but zig.

Most of FFmpeg's libraries are ported by harvesting: `packages/harvest/harvest.sh` runs each library's own build -
configure, CMake or meson - for each host, with zig's compilers behind wrappers that record every compile, and writes
the port's `hosts/<host>/`: `sources.zon`, the library's sources in groups compiled alike, and `build/`, the files its
build generated that the sources read; the `harvested` port builds a library from them. A recipe a library
(`harvest/recipes/`) says how its own build is run. binutils' and CPython's ports keep what their own builds decide of
each host the same way, from recipes of their own (their parts' configure; a reference build of CPython for each). Harvesting again, after a library's version changes, writes its
tables anew; it runs on Linux, with the tools it sets up for itself (nasm, meson, the autotools) in
`target/packages/harvest`.

`packages/components.toml` describes every source the packages are built from, by the exact URL a manifest fetches it
from: its name and version, its license and the files carrying it, whether it is linked into what ships or only used to
build it, and on which platforms. `cargo xtask packages` fetches the pinned zig (from a community mirror, its signature
checked, as ziglang.org asks of tools) and, for each package, cross-builds it for every platform from one machine with
nothing fetched before, so that what its project fetched is what it is built from. Every fetched source must be a
component (a URL changed and not described fails the task by name), and a release is laid out of it: an archive a
platform, packed deterministically - two runs give the same bytes - with `licenses/`, the license of every component
linked into it there; `sources/`, each component's source named by its name and version, the `packages/` folder and the
zig that built them (what the licenses ask of whoever ships the result); notes listing what it is made of;
`SHA256SUMS`; and the `[[setup]]` entries the extensions' manifests take. A release is a package version,
`<package>-<version>-<release>` (`release` in `packages.toml` counts its builds), so packages change apart; `--publish`
makes it a release of the editor's repository, never over one published already.

The projects are built in a copy of `packages/` (`target/packages/projects/`, kept between runs with zig's cache in
it), so that nothing zig makes - what it fetches for a project, its cache - is made in `packages/`; what is built
names its sources by paths relative to its project, the same in the copy as anywhere.

The same bytes from every build, wherever it is made, ask two things of the ports. No library holds two objects of
one name: zig writes an archive's members ordered by their names alone, keeping those of one name in the order they
happened to be compiled in, and on macOS what links the archive follows that order. A port adds its sources through
`harvested`'s `Archives`, which compiles a source whose name its library has already into a further library, made
alike and linked by the first (FFmpeg's `parser.c` of several of its libraries, x264's sources compiled once for each
depth). And no source is compiled from a folder a step wrote: zig names it by a hash of the step, which takes the
files it reads by their absolute paths, and an assertion's message carries a source's path into the program. What a
step writes is compiled as the source it was made from where it can be (CPython's `Python/` outside the one file
patched), and otherwise names itself with `#line` (ld's emulations). The task fails on what is built naming zig's
cache or the folders it is built in.

A package replaces what an extension downloaded only once it builds the same: GCC 2.8.1's `cc1` compiles every C file
of a project to the same assembly as the build it replaced, on each platform that can run it; FFmpeg encodes the same
bytes as FFmpeg's own build of the same configuration. The preprocessor is aro's, as zig carries it, behind the
arguments of GCC's; each release of aro-cpp holds it to clang's on `packages/aro-cpp/tests`, so that a zig whose aro
changed does not ship unnoticed.

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
