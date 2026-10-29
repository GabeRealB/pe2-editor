//! Builds the built-in extensions as components (`wasm32-wasip3`, release, in the target directory's `extensions`
//! folder, apart from this build's own) for `src/lib.rs` to embed: one `cargo build` makes the editor and its
//! extensions.

use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

const TARGET: &str = "wasm32-wasip3";
/// The built-ins, by package: each a crate in a folder of this one.
const BUILTINS: &[(&str, &str)] = &[("pe2-formats", "formats"), ("pe2-game", "game"), ("pe2-images", "images"), ("pe2-movies", "movies")];

fn main() {
    let here = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    let workspace = here.parent().expect("the extensions are in the workspace").to_path_buf();
    let target = std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| workspace.join("target"), PathBuf::from);
    let built = target.join("extensions");
    // What a component is built from: its crate, the SDK and its contract, the lock file, the cargo configuration.
    let mut inputs: Vec<PathBuf> = vec![
        workspace.join("crates/ext/src"),
        workspace.join("crates/ext/wit"),
        workspace.join("crates/ext/Cargo.toml"),
        workspace.join("crates/codecs"),
        workspace.join("crates/formats"),
        workspace.join("Cargo.lock"),
        workspace.join(".cargo/config.toml"),
    ];
    inputs.extend(BUILTINS.iter().map(|(_, folder)| here.join(folder)));
    for input in &inputs {
        println!("cargo::rerun-if-changed={}", input.display());
    }
    let mut cargo = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    cargo.current_dir(&workspace).args(["build", "--release", "--target", TARGET, "--target-dir"]).arg(&built);
    for (package, _) in BUILTINS {
        cargo.args(["-p", package]);
    }
    // This build's own flags and wrappers are for the host, not the components.
    for variable in ["CARGO_ENCODED_RUSTFLAGS", "RUSTFLAGS", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "CARGO_BUILD_TARGET"] {
        cargo.env_remove(variable);
    }
    let status = cargo.status().expect("cargo runs");
    assert!(
        status.success(),
        "the built-in extensions did not build for {TARGET}. Is the target installed? \
         `rustup target add {TARGET}` (rust-toolchain.toml asks for it)"
    );
    let mut out = String::from("/// The built-in extensions' components, by package.\npub static COMPONENTS: &[(&str, &[u8])] = &[\n");
    for (package, _) in BUILTINS {
        let file = built.join(TARGET).join("release").join(format!("{}.wasm", package.replace('-', "_")));
        let _ = writeln!(out, "    ({package:?}, include_bytes!({:?})),", file.display().to_string());
    }
    out.push_str("];\n");
    let generated = Path::new(&std::env::var_os("OUT_DIR").expect("cargo sets it")).join("components.rs");
    std::fs::write(generated, out).expect("OUT_DIR is writable");
}
