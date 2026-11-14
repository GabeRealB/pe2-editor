//! An extension's manifest, `extension.toml`, checked: what the SDK's `extension!` checks as an extension is built, and
//! what the host checks of one that has no component - an extension that is only its manifest and what it names, such
//! as the programs a tool's downloads hold.
//!
//! The manifest's keys, each as what it is; its downloads (`[[setup]]`), every place taken held to a hash; the programs
//! it offers (`[[program]]`), each a place in its tree or another extension's program (`@<extension>:<name>`), its
//! arguments likewise; and that only what runs unsandboxed fetches or starts programs. Files it names are read through
//! what is given (`Read`), relative to the manifest: from the disk as an extension is built, from a project's store as
//! one it holds is loaded.

use std::fmt::Write as _;
use std::path::Path;

/// A file the manifest names, read by its path relative to the manifest: its bytes, or why it cannot be.
pub type Read<'a> = &'a dyn Fn(&str) -> Result<Vec<u8>, String>;

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
    ("program", Kind::Program),
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
    /// Tables of the programs it offers: `name`, `platform`, `program`, `args`.
    Program,
}

/// A place within an extension's tree: relative, going nowhere outside it.
#[must_use]
pub fn within(place: &str) -> bool {
    let path = Path::new(place);
    !place.is_empty()
        && !place.contains('\\')
        && !place.contains(':')
        && path.components().all(|part| matches!(part, std::path::Component::Normal(_)))
}

/// Another extension's program, as a manifest names it after `@`: its id and the program's name.
#[must_use]
pub fn reference(text: &str) -> Option<(&str, &str)> {
    let (id, name) = text.split_once(':')?;
    let named = |part: &str| !part.is_empty() && !part.contains(['/', '\\', ':', ' ']);
    (named(id) && named(name)).then_some((id, name))
}

/// A platform as an extension names it: `<os>-<arch>`, of Linux, macOS or Windows.
#[must_use]
pub fn platform_named(platform: &str) -> bool {
    matches!(platform.split_once('-').map(|(os, _)| os), Some("linux" | "macos" | "windows"))
}

/// Bytes' SHA-256, in hexadecimal digits.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes).iter().fold(String::new(), |mut hex, b| {
        let _ = write!(hex, "{b:02x}");
        hex
    })
}

/// Whether it is a SHA-256 in hexadecimal digits.
fn hex64(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A `[[setup]]` entry's patches checked: each a file beside the manifest, held to the hash it declares.
fn patches(name: &str, item: &toml::Table, read: Read) -> Result<(), String> {
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
        let got = sha256_hex(&read(file).map_err(|e| format!("{name}: {file}: {e}"))?);
        if !got.eq_ignore_ascii_case(declared) {
            return Err(format!("{name}: {file} is sha256 {got}, and the manifest says {declared}"));
        }
    }
    Ok(())
}

/// A place a program or an argument names after `@`: one in the extension's tree, or another extension's program.
fn place_or_reference(text: &str) -> bool {
    within(text) || reference(text).is_some()
}

/// One `[[program]]` entry checked: what is wrong with it.
fn program(item: &toml::Table) -> Result<(), String> {
    const KEYS: [&str; 4] = ["name", "platform", "program", "args"];
    if let Some(extra) = item.keys().find(|key| !KEYS.contains(&key.as_str())) {
        return Err(format!("no key {extra}; each has {}", KEYS.join(", ")));
    }
    let name = item.get("name").and_then(toml::Value::as_str).ok_or("each has a name, a string")?;
    if name.is_empty() || name.contains(['/', '\\', ':', ' ']) {
        return Err(format!("{name}: a program's name is a word others name it by (`<extension>:{name}`)"));
    }
    if let Some(platform) = item.get("platform") {
        let platform = platform.as_str().ok_or(format!("{name}: platform is a string"))?;
        if !platform_named(platform) {
            return Err(format!("{name}: platform {platform} is not <os>-<arch> of linux, macos or windows"));
        }
    }
    let program = item.get("program").and_then(toml::Value::as_str).ok_or(format!("{name}: it has no program"))?;
    let program = program.strip_prefix('@').unwrap_or(program);
    if !place_or_reference(program) {
        return Err(format!(
            "{name}: {program} is a place in the extension's tree, or another extension's program (@<extension>:<name>)"
        ));
    }
    let args = item.get("args").map_or(Some(&[][..]), |args| args.as_array().map(Vec::as_slice));
    let args = args.ok_or(format!("{name}: args is a list of strings"))?;
    for arg in args {
        let arg = arg.as_str().ok_or(format!("{name}: args is a list of strings"))?;
        if arg.strip_prefix('@').is_some_and(|place| !place_or_reference(place)) {
            return Err(format!(
                "{name}: {arg} names neither a place in the extension's tree nor another extension's program"
            ));
        }
    }
    Ok(())
}

/// One `[[setup]]` entry checked: what is wrong with it.
fn setup(item: &toml::Table, read: Read) -> Result<(), String> {
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
    patches(name, item, read)?;
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

/// A manifest checked: its keys, each as what it is, the files it names there, and that only what runs unsandboxed
/// fetches or offers programs.
///
/// # Errors
/// What is wrong with it, said by key.
pub fn check(table: &toml::Table, read: Read) -> Result<(), String> {
    let file = |key: &str, value: &str| match read(value) {
        Ok(_) => Ok(()),
        Err(e) => Err(format!("{key}: there is no {value} ({e})")),
    };
    for (key, value) in table {
        let Some((_, kind)) = KEYS.iter().find(|(known, _)| known == key) else {
            let known: Vec<&str> = KEYS.iter().map(|(key, _)| *key).collect();
            return Err(format!("no key {key}; a manifest has {}", known.join(", ")));
        };
        match (kind, value) {
            (Kind::Text, toml::Value::String(_)) | (Kind::Flag, toml::Value::Boolean(_)) => {}
            (Kind::File, toml::Value::String(text)) => file(key, text)?,
            (Kind::Texts, toml::Value::Array(items)) if items.iter().all(toml::Value::is_str) => {}
            (Kind::Bundled, toml::Value::Array(items)) => {
                for item in items {
                    let item = item.as_table().ok_or_else(|| format!("{key}: each is a table"))?;
                    for part in ["name", "license", "text"] {
                        item.get(part)
                            .and_then(toml::Value::as_str)
                            .ok_or_else(|| format!("{key}: each has {part}, a string"))?;
                    }
                    if let Some(extra) = item.keys().find(|k| !["name", "license", "text"].contains(&k.as_str())) {
                        return Err(format!("{key}: no key {extra}; each has name, license and text"));
                    }
                    file("bundled text", item.get("text").and_then(toml::Value::as_str).unwrap_or_default())?;
                }
            }
            (Kind::Setup, toml::Value::Array(items)) => {
                for item in items {
                    let item = item.as_table().ok_or_else(|| format!("{key}: each is a table"))?;
                    setup(item, read).map_err(|why| format!("{key}: {why}"))?;
                }
            }
            (Kind::Program, toml::Value::Array(items)) => {
                for item in items {
                    let item = item.as_table().ok_or_else(|| format!("{key}: each is a table"))?;
                    program(item).map_err(|why| format!("{key}: {why}"))?;
                }
            }
            _ => {
                let what = match kind {
                    Kind::Text | Kind::File => "a string",
                    Kind::Texts => "a list of strings",
                    Kind::Flag => "true or false",
                    Kind::Bundled | Kind::Setup | Kind::Program => "a list of tables",
                };
                return Err(format!("{key} is {what}"));
            }
        }
    }
    let unsandboxed = table.get("unsandboxed").and_then(toml::Value::as_bool) == Some(true);
    for (key, why) in
        [("setup", "what a machine fetches is for the programs it starts"), ("program", "what it offers are programs")]
    {
        let any = table.get(key).and_then(toml::Value::as_array).is_some_and(|items| !items.is_empty());
        if any && !unsandboxed {
            return Err(format!("{key}: {why}, so it is unsandboxed"));
        }
    }
    for key in ["id", "version"] {
        if table.get(key).and_then(toml::Value::as_str).is_none() {
            return Err(format!("it has no {key}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{check, reference, setup, within};

    fn none(_: &str) -> Result<Vec<u8>, String> {
        Err("no file".to_string())
    }

    /// A `[[setup]]` entry of a manifest, with `take` as given.
    fn entry(take: &str) -> Result<(), String> {
        let sha = "a".repeat(64);
        let text = format!("name = \"a tool\"\nurl = \"https://example.com/t\"\nsha256 = \"{sha}\"\n{take}\n");
        setup(&text.parse::<toml::Table>().expect("toml"), &none)
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

    #[test]
    fn a_program_is_a_place_or_another_extensions_program() {
        assert!(within("python/bin/python3") && !within("../x") && !within("a:b") && !within(""));
        assert_eq!(reference("pe2.python:python3"), Some(("pe2.python", "python3")));
        assert_eq!(reference("python/bin:x"), None);
        assert_eq!(reference(":x"), None);
        let manifest = |programs: &str| {
            let text = format!("id = \"a\"\nversion = \"0.1.0\"\nunsandboxed = true\n{programs}");
            check(&text.parse::<toml::Table>().unwrap(), &none)
        };
        manifest("[[program]]\nname = \"cc1\"\nprogram = \"cc1\"\n").unwrap();
        manifest(
            "[[program]]\nname = \"as\"\nprogram = \"@pe2.python:python3\"\n\
             args = [\"-B\", \"@maspsx/maspsx.py\", \"--gnu-as-path\", \"@pe2.binutils-mipsel:as\"]\n",
        )
        .unwrap();
        let outside = manifest("[[program]]\nname = \"x\"\nprogram = \"../x\"\n").unwrap_err();
        assert!(outside.contains("place in the extension's tree"), "{outside}");
        let named = manifest("[[program]]\nname = \"a/b\"\nprogram = \"x\"\n").unwrap_err();
        assert!(named.contains("a word others name it by"), "{named}");
        let sandboxed = "id = \"a\"\nversion = \"0.1.0\"\n[[program]]\nname = \"x\"\nprogram = \"x\"\n";
        let why = check(&sandboxed.parse::<toml::Table>().unwrap(), &none).unwrap_err();
        assert!(why.contains("so it is unsandboxed"), "{why}");
    }
}
