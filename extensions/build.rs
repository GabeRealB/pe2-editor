//! Builds the built-in extensions as components (`wasm32-wasip3`, release, in the target directory's `extensions`
//! folder, apart from this build's own) for `src/lib.rs` to embed: one `cargo build` makes the editor and its
//! extensions.

use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

const TARGET: &str = "wasm32-wasip3";
/// The built-ins, by package: each a crate in a folder of this one.
const BUILTINS: &[(&str, &str)] = &[
    ("pe2-dialogue", "dialogue"),
    ("pe2-events", "events"),
    ("pe2-flags", "flags"),
    ("pe2-fonts", "fonts"),
    ("pe2-formats", "formats"),
    ("pe2-game", "game"),
    ("pe2-images", "images"),
    ("pe2-movies", "movies"),
    ("pe2-packages", "packages"),
    ("pe2-str", "str"),
    ("pe2-sounds", "sounds"),
    ("pe2-render", "render"),
];

/// Components built here too, and never embedded: what needs enabling is not shipped inside the editor.
const SEPARATE: &[(&str, &str)] = &[
    ("pe2-ffmpeg", "ffmpeg"),
    ("pe2-psx", "psx"),
    ("pe2-render-tests", "render/tests"),
    ("pe2-images-tests", "images/tests"),
];

fn main() {
    let here = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    let workspace = here.parent().expect("the extensions are in the workspace").to_path_buf();
    let target = std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| workspace.join("target"), PathBuf::from);
    let built = target.join("extensions");
    // What a component is built from: its crate, the SDK and its contract, the lock file, the cargo configuration.
    let mut inputs: Vec<PathBuf> = vec![
        workspace.join("crates/ext/src"),
        workspace.join("crates/ext-macros/src"),
        workspace.join("crates/ext/wit"),
        workspace.join("crates/ext/Cargo.toml"),
        workspace.join("crates/codecs"),
        workspace.join("crates/formats"),
        workspace.join("crates/typeface"),
        workspace.join("Cargo.lock"),
        workspace.join(".cargo/config.toml"),
    ];
    inputs.extend(BUILTINS.iter().chain(SEPARATE).map(|(_, folder)| here.join(folder)));
    inputs.push(here.join("test_data"));
    for input in &inputs {
        println!("cargo::rerun-if-changed={}", input.display());
    }
    let mut cargo = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    cargo.current_dir(&workspace).args(["build", "--release", "--target", TARGET, "--target-dir"]).arg(&built);
    for (package, _) in BUILTINS.iter().chain(SEPARATE) {
        cargo.args(["-p", package]);
    }
    // This build's own flags and wrappers are for the host, not the components.
    for variable in
        ["CARGO_ENCODED_RUSTFLAGS", "RUSTFLAGS", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "CARGO_BUILD_TARGET"]
    {
        cargo.env_remove(variable);
    }
    println!("cargo::rustc-env=PE2_BUILT={}", built.join(TARGET).join("release").display());
    let suites = built.join("suites");
    println!("cargo::rustc-env=PE2_SUITES={}", suites.display());
    let status = cargo.status().expect("cargo runs");
    assert!(
        status.success(),
        "the built-in extensions did not build for {TARGET}. Is the target installed? \
         `rustup target add {TARGET}` (rust-toolchain.toml asks for it)"
    );
    for (package, folder) in SEPARATE.iter().filter(|(_, folder)| folder.ends_with("/tests")) {
        lay_out_suite(
            &built.join(TARGET).join("release").join(format!("{}.wasm", package.replace('-', "_"))),
            &here.join(folder),
            &suites,
            folder,
        );
    }
    let mut out = String::from(
        "/// The built-in extensions' components, by package.\npub static COMPONENTS: &[(&str, &[u8])] = &[\n",
    );
    for (package, _) in BUILTINS {
        let file = built.join(TARGET).join("release").join(format!("{}.wasm", package.replace('-', "_")));
        let _ = writeln!(out, "    ({package:?}, include_bytes!({:?})),", file.display().to_string());
    }
    out.push_str("];\n");
    let generated = Path::new(&std::env::var_os("OUT_DIR").expect("cargo sets it")).join("components.rs");
    std::fs::write(generated, out).expect("OUT_DIR is writable");
}

/// A built-in's tests as a folder, as an extension of its own would carry them: `<suites>/<extension>/tests/`.
fn lay_out_suite(component: &Path, source: &Path, suites: &Path, folder: &str) {
    let to = suites.join(folder);
    let _ = std::fs::remove_dir_all(&to);
    std::fs::create_dir_all(to.join("data")).expect("a folder for its tests");
    std::fs::copy(component, to.join("tests.wasm")).expect("its tests were built");
    std::fs::copy(source.join("test.toml"), to.join("test.toml")).expect("its test.toml");
    for entry in std::fs::read_dir(source.join("data")).into_iter().flatten().flatten() {
        std::fs::copy(entry.path(), to.join("data").join(entry.file_name())).expect("its data");
    }
}
