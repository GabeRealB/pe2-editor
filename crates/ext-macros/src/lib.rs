//! The extension SDK's macros. `pe2_ext` re-exports them: an extension uses `pe2_ext::extension!`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use proc_macro::TokenStream;

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
/// [[launch]]                        # what it starts by name that is another program (unsandboxed only)
/// name = "tool"
/// platform = "linux-x86_64"         # none, every platform
/// program = "python/bin/python3.13" # a place in its tree: shipped, or fetched
/// args = ["-B", "@tool/tool.py"]    # `@`: a place in its tree; what the extension passes follows
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

/// The keys a manifest may have, each with what it is.
const KEYS: &[(&str, Kind)] = &[
    ("id", Kind::Text),
    ("version", Kind::Text),
    ("requires", Kind::Texts),
    ("weak", Kind::Texts),
    ("optional", Kind::Texts),
    ("provides", Kind::Texts),
    ("platforms", Kind::Texts),
    ("unsandboxed", Kind::Flag),
    ("description", Kind::Text),
    ("license", Kind::Text),
    ("repository", Kind::Text),
    ("docs", Kind::File),
    ("install", Kind::File),
    ("icon", Kind::File),
    ("bundled", Kind::Bundled),
    ("setup", Kind::Setup),
    ("launch", Kind::Launch),
];

#[derive(Clone, Copy)]
enum Kind {
    Text,
    Texts,
    Flag,
    /// A path to a file beside the manifest.
    File,
    /// Tables of `name`, `license` and `text`, the last a file.
    Bundled,
    /// Tables of what a machine fetches: `name`, `license`, `platform`, `url`, `sha256`, `take`.
    Setup,
    /// Tables of what it starts by name that is another program: `name`, `platform`, `program`, `args`.
    Launch,
}

/// A place within the extension's tree: relative, going nowhere outside it.
fn within(place: &str) -> bool {
    let path = Path::new(place);
    !place.is_empty()
        && !place.contains('\\')
        && path.components().all(|part| matches!(part, std::path::Component::Normal(_)))
}

/// A platform as an extension names it: `<os>-<arch>`, of Linux, macOS or Windows.
fn platform_named(platform: &str) -> bool {
    matches!(platform.split_once('-').map(|(os, _)| os), Some("linux" | "macos" | "windows"))
}

/// A file's SHA-256, in hexadecimal digits.
fn sha256_of(file: &Path) -> Result<String, String> {
    use sha2::Digest as _;
    let bytes = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
    Ok(sha2::Sha256::digest(&bytes).iter().fold(String::new(), |mut hex, b| {
        let _ = write!(hex, "{b:02x}");
        hex
    }))
}

/// A `[[setup]]` entry's patches checked: each a file beside the manifest, held to the hash it declares.
fn patches(name: &str, item: &toml::Table, folder: &Path) -> Result<(), String> {
    let Some(patches) = item.get("patches") else { return Ok(()) };
    let patches = patches.as_array().ok_or(format!("{name}: patches is a list of {{ file, sha256 }}"))?;
    for patch in patches {
        let patch = patch.as_table().ok_or(format!("{name}: each patch is {{ file, sha256 }}"))?;
        if let Some(extra) = patch.keys().find(|key| !["file", "sha256"].contains(&key.as_str())) {
            return Err(format!("{name}: a patch has no key {extra}; each has file and sha256"));
        }
        let file = patch.get("file").and_then(toml::Value::as_str).ok_or(format!("{name}: a patch has no file"))?;
        let declared =
            patch.get("sha256").and_then(toml::Value::as_str).ok_or(format!("{name}: {file} has no sha256"))?;
        if !within(file) {
            return Err(format!("{name}: {file} is a path within the extension, going nowhere outside"));
        }
        let got = sha256_of(&folder.join(file))?;
        if !got.eq_ignore_ascii_case(declared) {
            return Err(format!("{name}: {file} is sha256 {got}, and the manifest says {declared}"));
        }
    }
    Ok(())
}

/// One `[[launch]]` entry checked: what is wrong with it.
fn launch(item: &toml::Table) -> Result<(), String> {
    const KEYS: [&str; 4] = ["name", "platform", "program", "args"];
    if let Some(extra) = item.keys().find(|key| !KEYS.contains(&key.as_str())) {
        return Err(format!("no key {extra}; each has {}", KEYS.join(", ")));
    }
    let name = item.get("name").and_then(toml::Value::as_str).ok_or("each has a name, a string")?;
    if let Some(platform) = item.get("platform") {
        let platform = platform.as_str().ok_or(format!("{name}: platform is a string"))?;
        if !platform_named(platform) {
            return Err(format!("{name}: platform {platform} is not <os>-<arch> of linux, macos or windows"));
        }
    }
    let program = item.get("program").and_then(toml::Value::as_str).ok_or(format!("{name}: it has no program"))?;
    if !within(program) {
        return Err(format!("{name}: {program} is a place in the extension's tree"));
    }
    let args = item.get("args").map_or(Some(&[][..]), |args| args.as_array().map(Vec::as_slice));
    let args = args.ok_or(format!("{name}: args is a list of strings"))?;
    for arg in args {
        let arg = arg.as_str().ok_or(format!("{name}: args is a list of strings"))?;
        if arg.strip_prefix('@').is_some_and(|place| !within(place)) {
            return Err(format!("{name}: {arg} names a place outside the extension's tree"));
        }
    }
    Ok(())
}

/// One `[[setup]]` entry checked: what is wrong with it.
fn setup(item: &toml::Table, folder: &Path) -> Result<(), String> {
    const KEYS: [&str; 10] =
        ["name", "license", "copyright", "platform", "url", "mirrors", "sha256", "take", "lazy", "patches"];
    if let Some(extra) = item.keys().find(|key| !KEYS.contains(&key.as_str())) {
        return Err(format!("no key {extra}; each has {}", KEYS.join(", ")));
    }
    let text = |key: &str| item.get(key).map(|value| value.as_str().ok_or(format!("{key} is a string"))).transpose();
    let name = text("name")?.ok_or("each has a name")?;
    if item.get("lazy").is_some_and(|lazy| !lazy.is_bool()) {
        return Err(format!("{name}: lazy is true or false"));
    }
    text("license")?;
    text("copyright")?;
    if let Some(platform) = text("platform")?.filter(|platform| !platform_named(platform)) {
        return Err(format!("{name}: platform {platform} is not <os>-<arch> of linux, macos or windows"));
    }
    patches(name, item, folder)?;
    let url = text("url")?.ok_or(format!("{name}: it has no url"))?;
    if !url.starts_with("https://") {
        return Err(format!("{name}: {url} is fetched only over HTTPS"));
    }
    if let Some(mirrors) = item.get("mirrors") {
        let mirrors = mirrors.as_array().ok_or(format!("{name}: mirrors is a list of URLs"))?;
        for mirror in mirrors {
            match mirror.as_str() {
                Some(mirror) if mirror.starts_with("https://") => {}
                _ => return Err(format!("{name}: each mirror is a URL fetched over HTTPS")),
            }
        }
    }
    let sha256 = text("sha256")?.ok_or(format!("{name}: it has no sha256, which what is downloaded is held to"))?;
    if !hex64(sha256) {
        return Err(format!("{name}: sha256 is 64 hexadecimal digits"));
    }
    let hashed = |place: &str, table: &toml::Table, key: &str| -> Result<(), String> {
        let to = table.get(key).and_then(toml::Value::as_str).ok_or(format!("{name}: take: {place} has no {key}"))?;
        if !within(to) {
            return Err(format!("{name}: take: {to} is a path within, going nowhere outside"));
        }
        if let Some(extra) = table.keys().find(|k| *k != key && *k != "sha256") {
            return Err(format!("{name}: take: {place} has no key {extra}; it has {key} and sha256"));
        }
        match table.get("sha256").map(toml::Value::as_str) {
            Some(Some(sha256)) if hex64(sha256) => Ok(()),
            Some(_) => Err(format!("{name}: take: {place}: sha256 is 64 hexadecimal digits")),
            None => Err(format!(
                "{name}: take: {place} has no sha256, which what is taken is held to: `pe2-cli extension hash \
                 <manifest> --write` finds it"
            )),
        }
    };
    match item.get("take") {
        Some(toml::Value::Table(one)) if one.get("file").is_some_and(toml::Value::is_str) => {
            hashed("the file", one, "file")
        }
        Some(toml::Value::Table(members)) if !members.is_empty() => {
            for (from, to) in members {
                if !within(from) {
                    return Err(format!("{name}: take: {from} is a path within, going nowhere outside"));
                }
                let to = to.as_table().ok_or(format!("{name}: take: {from} = {{ to, sha256 }}"))?;
                hashed(from, to, "to")?;
            }
            Ok(())
        }
        Some(_) => Err(format!(
            "{name}: take is {{ file, sha256 }} (a single file), or a table of paths in an archive, each {{ to, sha256 }}"
        )),
        None => Err(format!("{name}: it has no take, what of it goes where")),
    }
}

/// Whether it is a SHA-256 in hexadecimal digits.
fn hex64(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A manifest's keys checked, each as what it is; and that only what runs unsandboxed fetches.
fn check(path: &Path, table: &toml::Table) -> Result<(), String> {
    let said = |why: String| format!("{}: {why}", path.display());
    let folder = path.parent().unwrap_or(Path::new("."));
    let file = |key: &str, value: &str| {
        let at = folder.join(value);
        if at.is_file() { Ok(()) } else { Err(said(format!("{key}: there is no {}", at.display()))) }
    };
    for (key, value) in table {
        let Some((_, kind)) = KEYS.iter().find(|(known, _)| known == key) else {
            let known: Vec<&str> = KEYS.iter().map(|(key, _)| *key).collect();
            return Err(said(format!("no key {key}; a manifest has {}", known.join(", "))));
        };
        match (kind, value) {
            (Kind::Text, toml::Value::String(_)) | (Kind::Flag, toml::Value::Boolean(_)) => {}
            (Kind::File, toml::Value::String(text)) => file(key, text)?,
            (Kind::Texts, toml::Value::Array(items)) if items.iter().all(toml::Value::is_str) => {}
            (Kind::Bundled, toml::Value::Array(items)) => {
                for item in items {
                    let item = item.as_table().ok_or_else(|| said(format!("{key}: each is a table")))?;
                    for part in ["name", "license", "text"] {
                        item.get(part)
                            .and_then(toml::Value::as_str)
                            .ok_or_else(|| said(format!("{key}: each has {part}, a string")))?;
                    }
                    if let Some(extra) = item.keys().find(|k| !["name", "license", "text"].contains(&k.as_str())) {
                        return Err(said(format!("{key}: no key {extra}; each has name, license and text")));
                    }
                    file("bundled text", item.get("text").and_then(toml::Value::as_str).unwrap_or_default())?;
                }
            }
            (Kind::Setup, toml::Value::Array(items)) => {
                for item in items {
                    let item = item.as_table().ok_or_else(|| said(format!("{key}: each is a table")))?;
                    setup(item, folder).map_err(|why| said(format!("{key}: {why}")))?;
                }
            }
            (Kind::Launch, toml::Value::Array(items)) => {
                for item in items {
                    let item = item.as_table().ok_or_else(|| said(format!("{key}: each is a table")))?;
                    launch(item).map_err(|why| said(format!("{key}: {why}")))?;
                }
            }
            _ => {
                let what = match kind {
                    Kind::Text | Kind::File => "a string",
                    Kind::Texts => "a list of strings",
                    Kind::Flag => "true or false",
                    Kind::Bundled | Kind::Setup | Kind::Launch => "a list of tables",
                };
                return Err(said(format!("{key} is {what}")));
            }
        }
    }
    let unsandboxed = table.get("unsandboxed").and_then(toml::Value::as_bool) == Some(true);
    for (key, why) in
        [("setup", "what a machine fetches is for the programs it starts"), ("launch", "what it launches are programs")]
    {
        let any = table.get(key).and_then(toml::Value::as_array).is_some_and(|items| !items.is_empty());
        if any && !unsandboxed {
            return Err(said(format!("{key}: {why}, so it is unsandboxed")));
        }
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::setup;

    /// A `[[setup]]` entry of a manifest, with `take` as given.
    fn entry(take: &str) -> Result<(), String> {
        let sha = "a".repeat(64);
        let text = format!("name = \"a tool\"\nurl = \"https://example.com/t\"\nsha256 = \"{sha}\"\n{take}\n");
        setup(&text.parse::<toml::Table>().expect("toml"), std::path::Path::new("."))
    }

    #[test]
    fn every_place_taken_is_held_to_a_hash() {
        let sha = "b".repeat(64);
        entry(&format!("take = {{ file = \"cc1\", sha256 = \"{sha}\" }}")).unwrap();
        entry(&format!("take = {{ \"pkg/cc1\" = {{ to = \"cc1\", sha256 = \"{sha}\" }} }}")).unwrap();
        // A member named `file` is a member.
        entry(&format!("take = {{ \"file\" = {{ to = \"cc1\", sha256 = \"{sha}\" }} }}")).unwrap();
        let bare = entry("take = { \"pkg/cc1\" = \"cc1\" }").unwrap_err();
        assert!(bare.contains("take: pkg/cc1 = { to, sha256 }"), "{bare}");
        let unhashed = entry("take = { file = \"cc1\" }").unwrap_err();
        assert!(unhashed.contains("has no sha256") && unhashed.contains("pe2-cli extension hash"), "{unhashed}");
        let short = entry("take = { \"pkg/cc1\" = { to = \"cc1\", sha256 = \"00\" } }").unwrap_err();
        assert!(short.contains("64 hexadecimal digits"), "{short}");
        let outside =
            entry(&format!("take = {{ \"pkg/cc1\" = {{ to = \"../cc1\", sha256 = \"{sha}\" }} }}")).unwrap_err();
        assert!(outside.contains("going nowhere outside"), "{outside}");
        assert!(entry("take = \"cc1\"").is_err(), "a bare file");
        let take = format!("take = {{ file = \"cc1\", sha256 = \"{sha}\" }}");
        entry(&format!("mirrors = [\"https://elsewhere.example.org/t\"]\n{take}")).unwrap();
        let plain = entry(&format!("mirrors = [\"http://elsewhere.example.org/t\"]\n{take}")).unwrap_err();
        assert!(plain.contains("over HTTPS"), "{plain}");
    }
}
