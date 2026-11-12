# Writing extensions

An extension adds to the editor: kinds of asset, how they are shown and edited, operations for stacks and graphs,
importers and exporters, panes, commands, and tools for AI agents. The game's own support is written this way, so
an extension can do anything the built-in ones do.

An extension is a **WebAssembly component**, written in Rust against the editor's SDK, the `pe2-ext` crate. It runs
in a sandbox: it sees only what the editor gives it. One that has to run programs on the user's computer can ask
to run outside the sandbox, and then runs only once the user trusts it.

## A first extension

An extension is a Rust library with a manifest beside its `Cargo.toml`:

```toml
# Cargo.toml
[package]
name = "my-extension"
edition = "2024"

[lib]
crate-type = ["rlib", "cdylib"]

[dependencies]
pe2-ext = { path = "<the editor's source>/crates/ext" }
```

```toml
# extension.toml
id = "me.notes"
version = "0.1.0"
description = "Notes on assets"
license = "MIT"
```

```rust
// src/lib.rs
use pe2_ext::{Registry, Result};

pe2_ext::extension!(register);

fn register(r: &mut Registry) -> Result<()> {
    // Register types, operations, views...
    Ok(())
}
```

`extension!` checks the manifest as the extension builds, so a mistake in it fails the build, and puts it into the
component.

Build it for the WebAssembly target, with the nightly toolchain the editor uses:

```
cargo build --release --target wasm32-wasip3
```

Put the component, renamed `extension.wasm`, in a folder named after the extension's id with its manifest:
`me.notes.extension/extension.toml` and `me.notes.extension/extension.wasm`. Then add it to a project:

```
pe2-cli project extension add path/to/me.notes.extension
```

## In this chapter

- [The manifest](manifest.md): every key of `extension.toml`
- [What an extension can add](registering.md): types, fields, views, operations, importers, exporters, panes,
  commands, events
- [Calling the editor](host.md): what an extension can read and ask of the project
- [Running programs](unsandboxed.md): extensions outside the sandbox, and what they download
- [Packaging and sharing](packaging.md): the folder and archive, docs and licenses
- [Testing](testing.md): tests that drive the editor

The built-in extensions are good examples: a small one such as the flags or formats extension, and, outside the
sandbox, `pe2.ffmpeg` and `pe2.psx`.

## Versions

The contract between the editor and its extensions has a version, `0.1.0` today. Until the editor's first release
it may change without notice, and an extension built for another version is refused with a message saying whether
to rebuild it or to update the editor. After that, a version will only add to what it offers.
