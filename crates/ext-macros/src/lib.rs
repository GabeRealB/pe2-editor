//! The extension SDK's macros. `pe2_ext` re-exports them: an extension uses `pe2_ext::extension!`.

#![allow(
    clippy::disallowed_methods,
    reason = "it reads the crate being built where cargo has it: a place on disk, no storage's"
)]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use proc_macro::TokenStream;

mod stepping;

/// The file beside an extension's `Cargo.toml` that says what it is.
const MANIFEST: &str = "extension.toml";

/// Declares an extension from its manifest, `extension.toml` beside its `Cargo.toml`, and names its entry point:
///
/// ```ignore
/// pe2_ext::extension!(register);
/// ```
///
/// The manifest is what the extension is, written once: its id and version, how it depends on others, whether it runs
/// unsandboxed, and what is said of it. The component carries it whole, in the section the host reads without running
/// it, and an extension's folder (`<id>.extension/`) holds the same file beside the component, for what reads it
/// without opening the component. Defines `ID`, the id, and `extension()`, which a host linking it in loads it by;
/// built as a component, it exports it.
///
/// ```toml
/// id = "acme.fonts"
/// version = "0.1.0"
/// requires = ["pe2.images@^0.1"]    # also weak, optional: ids and the versions accepted
/// provides = ["font-source@1.0.0"]  # services, by name and version
/// platforms = ["linux-x86_64"]      # where it runs; none, anywhere
/// unsandboxed = true                # all of WASI, and programs: the user trusts it first
/// description = "Fonts from the Acme foundry."
/// license = "MIT"
/// repository = "https://example.com/acme/fonts"
/// docs = "docs/index.md"            # its guides' first page
/// install = "docs/install.md"       # what to set up on the machine first, where there is anything
/// icon = "icon.png"
///
/// [[bundled]]                       # what it ships of others', each with its license
/// name = "a tool"
/// license = "GPL-2.0-or-later"
/// text = "licenses/tool.txt"
///
/// [[setup]]                         # what a machine fetches before it runs (unsandboxed only)
/// name = "a compiler"
/// license = "GPL-3.0-or-later"      # shown before it is fetched
/// copyright = "Free Software Foundation"  # who holds its rights, and where it comes from, shown beside it
/// platform = "linux-x86_64"         # none, every platform
/// url = "https://example.com/compiler.tar.gz"
/// mirrors = ["https://elsewhere.example.org/compiler.tar.gz"]  # tried in order where it fails: the same bytes
/// sha256 = "..."                    # what was downloaded is these bytes, or nothing is taken
/// # An archive: each path in it, where it goes, and the SHA-256 of what it holds there, patched (a folder's, of its
/// # files' paths and hashes); a single file: `take = { file = "cc1", sha256 = "..." }`. `pe2-cli extension hash
/// # <manifest> --write` finds them.
/// take = { "compiler/bin/cc1" = { to = "cc1", sha256 = "..." } }
/// lazy = true                       # optional: fetched when it asks for it (`pe2_ext::process::set_up`), as the user
///                                   # lets it; without, required, and fetched once it runs
/// patches = [{ file = "patches/fix.patch", sha256 = "..." }]  # unified diffs it ships, applied to what is taken
///
/// [[program]]                       # a program it offers, by name: it and others start it (unsandboxed only)
/// name = "tool"                     # what it is started as; another extension names it `acme.fonts:tool`
/// platform = "linux-x86_64"         # none, every platform
/// program = "python/bin/python3"    # a place in its tree (shipped, or fetched), or `@<extension>:<program>`
/// args = ["-B", "@tool/tool.py"]    # `@`: a place in its tree, or another extension's program; what is passed follows
/// ```
///
/// Each path is relative to the manifest, and has to be there. Each place `take` names is relative to the extension's
/// tree, where its programs are looked for.
#[proc_macro]
pub fn extension(input: TokenStream) -> TokenStream {
    let register = input.to_string();
    let expanded = if register.trim().is_empty() {
        Err("extension!(register): name the extension's entry point".to_string())
    } else {
        manifest().and_then(|(path, table)| expand(&path, &table, &register))
    };
    let code = expanded.unwrap_or_else(|why| format!("compile_error!({why:?});"));
    code.parse::<TokenStream>().unwrap_or_else(|e: proc_macro::LexError| {
        format!("compile_error!({:?});", e.to_string()).parse().unwrap_or_default()
    })
}

/// The manifest of the crate being built: its path, and what it says.
fn manifest() -> Result<(PathBuf, toml::Table), String> {
    let folder = std::env::var_os("CARGO_MANIFEST_DIR").ok_or("CARGO_MANIFEST_DIR is not set: build it with cargo")?;
    let path = Path::new(&folder).join(MANIFEST);
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let table = text.parse::<toml::Table>().map_err(|e| format!("{}: {e}", path.display()))?;
    Ok((path, table))
}

/// A manifest's keys checked, each as what it is, the files it names beside it, and that only what runs unsandboxed
/// fetches or offers programs (`pe2_manifest::check`).
fn check(path: &Path, table: &toml::Table) -> Result<(), String> {
    let folder = path.parent().ok_or("a manifest in no folder")?;
    // What it names beside it is a path of the extension's tree: checked as one, then read where cargo has the crate.
    let read = |name: &str| {
        let within = pe2_path::RelPath::new(name).map_err(|e| e.to_string())?;
        let place = within.parts().fold(folder.to_path_buf(), |at, part| at.join(part.as_str()));
        std::fs::read(place).map_err(|e| e.to_string())
    };
    pe2_manifest::check(table, &read).map_err(|why| format!("{}: {why}", path.display()))
}

/// The manifest checked, and the code it declares.
fn expand(path: &Path, table: &toml::Table, register: &str) -> Result<String, String> {
    check(path, table)?;
    let said = |why: String| format!("{}: {why}", path.display());
    let text = |key: &str| table.get(key).and_then(toml::Value::as_str);
    let id = text("id").ok_or_else(|| said("it has no id".into()))?;
    let version = text("version").ok_or_else(|| said("it has no version".into()))?;
    let texts = |key: &str| {
        let items = table.get(key).and_then(toml::Value::as_array).map_or(&[][..], Vec::as_slice);
        let quoted: Vec<String> = items.iter().filter_map(toml::Value::as_str).map(|t| format!("{t:?}")).collect();
        format!("&[{}]", quoted.join(", "))
    };
    let unsandboxed = table.get("unsandboxed").and_then(toml::Value::as_bool).unwrap_or(false);
    let manifest = path.to_str().ok_or_else(|| said("its path is not UTF-8".into()))?;
    let mut code = String::new();
    let _ = write!(
        code,
        r#"
/// This extension's id.
pub const ID: &str = {id:?};

/// This extension, for the host to load.
#[must_use]
pub fn extension() -> ::pe2_ext::Extension {{
    ::pe2_ext::Extension {{
        id: {id:?},
        version: {version:?},
        requires: {requires},
        weak: {weak},
        optional: {optional},
        provides: {provides},
        platforms: {platforms},
        unsandboxed: {unsandboxed},
        register: {register},
    }}
}}

/// The manifest, which the extension is rebuilt with when it changes.
const _: &str = include_str!({manifest:?});

/// This extension, for the SDK's component exports.
#[cfg(target_family = "wasm")]
#[unsafe(no_mangle)]
fn __pe2_extension() -> ::pe2_ext::Extension {{
    extension()
}}

/// Who it is, as its component carries it in the section the host reads without running it (`pe2-extension`): its
/// manifest.
#[cfg(target_family = "wasm")]
const __PE2_ABOUT: &str = include_str!({manifest:?});
#[cfg(target_family = "wasm")]
#[unsafe(link_section = "pe2-extension")]
#[used]
static __PE2_ABOUT_SECTION: [u8; __PE2_ABOUT.len()] = ::pe2_ext::section(__PE2_ABOUT);

#[cfg(target_family = "wasm")]
use ::pe2_ext::guest::Component as __Pe2Component;
#[cfg(target_family = "wasm")]
::pe2_ext::guest::bindings::export_component!(__Pe2Component with_types_in ::pe2_ext::guest::bindings);
"#,
        requires = texts("requires"),
        weak = texts("weak"),
        optional = texts("optional"),
        provides = texts("provides"),
        platforms = texts("platforms"),
    );
    Ok(code)
}

/// An operation run in steps (`pe2_ext::stepped`): its body is cut where a `let` is marked - `#[request]`, a read of other
/// nodes made before the next step; `#[each]`, a map whose items each run as a micro op; `#[call]`, a helper in steps -
/// wherever a statement stands in its blocks, `if`s, `match`es and loops. What lives across a cut is carried, written with
/// its type. The function becomes one taking nothing and giving its steps, which `OpBuilder::steps` registers; its
/// parameters after the call are its arguments, where it is a helper.
#[proc_macro_attribute]
pub fn steps(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let function = syn::parse_macro_input!(item as syn::ItemFn);
    stepping::expand(&function).into()
}
