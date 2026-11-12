# Licenses

`crates/licenses`, `licenses/licenses.toml`.

## The idea

The programs say what they are built with: every crate they link, with its version, license and license texts, plus
what they bundle that is no crate and what they use where the system has it. The listing is gathered as the programs
build and compiled into them, so a copy of a program still says it. It is shown by `pe2-cli licenses`, in the
editor's settings, and written beside a release.

## How it is gathered

The crate's build script asks `cargo metadata` what each program links on the platform being built - and what each
extension's component links - and reads each crate's license files. What no tool can find is written by hand in
`licenses/licenses.toml`: the policy, standard texts, texts copied from upstream for crates that ship none, and what
is bundled or used where installed.

## The policy

Every linked crate's license expression must be met by the allowed licenses alone, all of them permissive and none
asking anything of what is loaded beside the program, since the editor loads extensions of any license. A crate that
is not allowed, or has no text to show, **fails the build by name**: a dependency is settled when it is added.

What is linked differs by platform. Another platform's set can be checked from any machine with
`PE2_LICENSES_FOR=<target triple> cargo check -p licenses`.

## Where to look

`crates/licenses/build.rs` (gathering and the policy), `crates/licenses/src/lib.rs` (the listing, as a table and as a
document), `licenses/licenses.toml`.
