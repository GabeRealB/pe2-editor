//! Gathers what the editor ships, by its license, for `src/lib.rs` to keep in the programs: every crate each program
//! links - asked of `cargo metadata` for the platform built for, and for the extensions' components - with the
//! license texts its package holds, what the workspace's `licenses/licenses.toml` adds by hand, and what that says is
//! bundled or used where installed.
//!
//! It fails, naming each, on a crate under a license the policy does not allow and on one with no text to show: a
//! dependency added is settled when it is added, not found out later.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

/// The platform the extensions' components are built for.
const COMPONENTS: &str = "wasm32-wasip3";
/// What the built-in extensions are listed as: they are inside both programs.
const BUILT_IN: &str = "extensions";
/// Set to a platform's triple, the listing is gathered for it in place of the one built for.
const FOR: &str = "PE2_LICENSES_FOR";

fn main() {
    let here = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    let workspace = here.parent().and_then(Path::parent).expect("the crate is in the workspace's crates").to_path_buf();
    let hand = workspace.join("licenses");
    println!("cargo:rerun-if-changed={}", hand.display());
    println!("cargo:rerun-if-changed={}", workspace.join("Cargo.lock").display());
    let table = hand.join("licenses.toml");
    let said: toml::Table =
        read(&table).parse().unwrap_or_else(|e| panic!("{} does not read: {e}", table.display()));

    // The platform built for - or another, to hold what it links to the policy from this machine
    // (`PE2_LICENSES_FOR=x86_64-pc-windows-msvc cargo check -p licenses`): what is linked differs by platform.
    println!("cargo:rerun-if-env-changed={FOR}");
    let (target, os, arch) = match std::env::var(FOR) {
        Ok(triple) => {
            let os = ["windows", "linux"].into_iter().find(|os| triple.contains(os)).unwrap_or("macos").to_string();
            let arch = triple.split('-').next().unwrap_or_default().to_string();
            (triple, os, arch)
        }
        Err(_) => (
            std::env::var("TARGET").expect("cargo sets it"),
            std::env::var("CARGO_CFG_TARGET_OS").expect("cargo sets it"),
            std::env::var("CARGO_CFG_TARGET_ARCH").expect("cargo sets it"),
        ),
    };
    let native = Graph::of(&workspace, &target);
    let components = Graph::of(&workspace, COMPONENTS);
    for manifest in native.members.values().map(|id| &native.packages[id].manifest) {
        println!("cargo:rerun-if-changed={}", manifest.display());
    }

    let programs = programs(&native, &components);

    let mut listing = Listing::default();
    let mut wrong = Vec::new();
    let policy = Policy::of(&said, &hand);
    let mut linked: BTreeMap<(String, String), (Package, BTreeSet<String>)> = BTreeMap::new();
    for (program, ids) in &programs {
        let graph = if program == "pe2-editor" || program == "pe2-cli" { &native } else { &components };
        for id in ids {
            let package = &graph.packages[id];
            let entry = linked.entry((package.name.clone(), package.version.clone()));
            entry.or_insert_with(|| (package.clone(), BTreeSet::new())).1.insert(program.clone());
        }
    }
    let mut crates = Vec::new();
    for (package, within) in linked.into_values() {
        match policy.texts(&package) {
            Ok(texts) => {
                let texts: Vec<Value> = texts.into_iter().map(|(file, text)| listing.text(&file, &text)).collect();
                crates.push(json!({
                    "name": package.name,
                    "version": package.version,
                    "license": package.license,
                    "source": package.source,
                    "repository": package.repository,
                    "within": within,
                    "texts": texts,
                }));
            }
            Err(why) => wrong.push(format!("{} {}: {why}", package.name, package.version)),
        }
    }
    assert!(
        wrong.is_empty(),
        "\n\nwhat the editor links is not all settled ({}):\n  {}\n\n",
        table.display(),
        wrong.join("\n  ")
    );

    let (bundled, installed) = by_hand(&said, &hand, &os, &mut listing);
    let own = said.get("own").and_then(toml::Value::as_table).expect("licenses.toml says the editor's own license");
    let own_file = hand.join(text_of(own, "text").expect("its text"));
    println!("cargo:rerun-if-changed={}", own_file.display());
    let own_text = read(&own_file);
    let own = json!({
        "name": text_of(own, "name"),
        "version": std::env::var("CARGO_PKG_VERSION").expect("cargo sets it"),
        "license": text_of(own, "license"),
        "texts": [listing.text("LICENSE", &own_text)],
    });

    let whole = json!({
        "target": format!("{os}-{arch}"),
        "own": own,
        "crates": crates,
        "bundled": bundled,
        "installed": installed,
        "texts": listing.texts,
    });
    let packed = miniz_oxide::deflate::compress_to_vec(whole.to_string().as_bytes(), 9);
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets it")).join("listing.json.deflate");
    std::fs::write(&out, packed).unwrap_or_else(|e| panic!("{}: {e}", out.display()));
}

/// Each program, and the third-party crates it links: the two programs, and each extension's component - a built-in
/// one's as inside both, one shipped beside them by its id.
fn programs(native: &Graph, components: &Graph) -> Vec<(String, BTreeSet<String>)> {
    let mut programs = vec![("pe2-editor".into(), native.linked("editor")), ("pe2-cli".into(), native.linked("cli"))];
    let built_in = native.depends_on(BUILT_IN);
    for (name, id) in &components.members {
        let folder = components.packages[id].manifest.parent().expect("a manifest is in a folder").to_path_buf();
        let manifest = folder.join("extension.toml");
        // A crate with a manifest beside it is an extension; one in a `tests` folder is an extension's tests.
        if !manifest.is_file() || folder.file_name().is_some_and(|n| n == "tests") {
            continue;
        }
        println!("cargo:rerun-if-changed={}", manifest.display());
        let about: toml::Table = read(&manifest).parse().unwrap_or_else(|e| panic!("{}: {e}", manifest.display()));
        let id = about.get("id").and_then(toml::Value::as_str).unwrap_or(name).to_string();
        let within = if built_in.contains(name) { BUILT_IN.to_string() } else { id };
        programs.push((within, components.linked(name)));
    }
    programs
}

/// What `licenses.toml` says is bundled, and what it says is used where installed: those of the system built for.
fn by_hand(said: &toml::Table, hand: &Path, os: &str, listing: &mut Listing) -> (Vec<Value>, Vec<Value>) {
    let here_only = |item: &&toml::Table| {
        item.get("on").and_then(toml::Value::as_array).is_none_or(|on| on.iter().any(|o| o.as_str() == Some(os)))
    };
    let items = |key: &str| -> Vec<toml::Table> {
        let all = said.get(key).and_then(toml::Value::as_array).cloned().unwrap_or_default();
        all.iter().filter_map(toml::Value::as_table).filter(here_only).cloned().collect()
    };
    let mut bundled = Vec::new();
    for item in items("bundled") {
        let mut texts = Vec::new();
        for file in strings(item.get("texts")) {
            let path = hand.join(&file);
            let name = path.file_name().expect("a file has a name").to_string_lossy().into_owned();
            texts.push(listing.text(&name, &read(&path)));
        }
        bundled.push(json!({
            "name": text_of(&item, "name"),
            "version": text_of(&item, "version"),
            "what": text_of(&item, "what"),
            "license": text_of(&item, "license"),
            "copyright": text_of(&item, "copyright"),
            "from": text_of(&item, "from"),
            "within": strings(item.get("in")),
            "texts": texts,
        }));
    }
    let installed = items("installed")
        .iter()
        .map(|item| {
            json!({
                "name": text_of(item, "name"),
                "what": text_of(item, "what"),
                "license": text_of(item, "license"),
                "from": text_of(item, "from"),
            })
        })
        .collect();
    (bundled, installed)
}

/// A table's text at a key.
fn text_of(item: &toml::Table, key: &str) -> Option<String> {
    item.get(key).and_then(toml::Value::as_str).map(str::to_string)
}

fn read(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    String::from_utf8_lossy(&bytes).replace("\r\n", "\n")
}

fn strings(value: Option<&toml::Value>) -> Vec<String> {
    let array = value.and_then(toml::Value::as_array).cloned().unwrap_or_default();
    array.iter().filter_map(toml::Value::as_str).map(str::to_string).collect()
}

/// The texts gathered, each kept once.
#[derive(Default)]
struct Listing {
    texts: Vec<String>,
    known: BTreeMap<String, usize>,
}

impl Listing {
    /// A text, by the name of the file it is in: where it is among those kept.
    fn text(&mut self, file: &str, text: &str) -> Value {
        let text = text.trim_matches('\n').to_string();
        let at = if let Some(at) = self.known.get(&text) {
            *at
        } else {
            self.texts.push(text.clone());
            self.known.insert(text, self.texts.len() - 1);
            self.texts.len() - 1
        };
        json!({ "file": file, "text": at })
    }
}

/// A package, as `cargo metadata` says it.
#[derive(Clone)]
struct Package {
    name: String,
    version: String,
    /// Its license expression; none where it names a file instead, or nothing.
    license: Option<String>,
    /// Where it comes from, unless that is crates.io; none for the workspace's own.
    source: Option<String>,
    third_party: bool,
    repository: Option<String>,
    manifest: PathBuf,
}

/// The workspace's packages resolved for a platform.
struct Graph {
    packages: BTreeMap<String, Package>,
    /// Each package's dependencies that are linked into it: not what only builds or tests it.
    links: BTreeMap<String, Vec<String>>,
    /// The workspace's own, by name.
    members: BTreeMap<String, String>,
}

impl Graph {
    fn of(workspace: &Path, target: &str) -> Graph {
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let manifest = workspace.join("Cargo.toml");
        let ran = Command::new(cargo)
            .args(["metadata", "--format-version", "1", "--locked", "--filter-platform", target, "--manifest-path"])
            .arg(&manifest)
            .output()
            .expect("cargo runs");
        assert!(ran.status.success(), "cargo metadata for {target}:\n{}", String::from_utf8_lossy(&ran.stderr));
        let said: Value = serde_json::from_slice(&ran.stdout).expect("cargo metadata says JSON");
        let text = |value: &Value| value.as_str().map(str::to_string);
        let mut packages = BTreeMap::new();
        for package in said["packages"].as_array().expect("its packages") {
            let source = text(&package["source"]);
            packages.insert(
                text(&package["id"]).expect("a package has an id"),
                Package {
                    name: text(&package["name"]).expect("a package has a name"),
                    version: text(&package["version"]).expect("a package has a version"),
                    license: text(&package["license"]),
                    third_party: source.is_some(),
                    source: source.filter(|s| !s.starts_with("registry+https://github.com/rust-lang/crates.io-index")),
                    repository: text(&package["repository"]),
                    manifest: PathBuf::from(text(&package["manifest_path"]).expect("a package has a manifest")),
                },
            );
        }
        let mut links = BTreeMap::new();
        for node in said["resolve"]["nodes"].as_array().expect("its resolved packages") {
            let linked = node["deps"].as_array().expect("a package's dependencies").iter().filter(|dep| {
                dep["dep_kinds"].as_array().is_some_and(|kinds| kinds.iter().any(|kind| kind["kind"].is_null()))
            });
            let linked = linked.filter_map(|dep| text(&dep["pkg"])).collect();
            links.insert(text(&node["id"]).expect("a package has an id"), linked);
        }
        let members = said["workspace_members"].as_array().expect("its members").iter().filter_map(text);
        let members = members.map(|id| (packages[&id].name.clone(), id)).collect();
        Graph { packages, links, members }
    }

    /// The names of the workspace's own packages one of them links directly.
    fn depends_on(&self, member: &str) -> BTreeSet<String> {
        let id = self.members.get(member).unwrap_or_else(|| panic!("the workspace has no {member}"));
        self.links[id].iter().map(|id| self.packages[id].name.clone()).collect()
    }

    /// The third-party packages linked into one of the workspace's own, through whatever links them.
    fn linked(&self, member: &str) -> BTreeSet<String> {
        let root = self.members.get(member).unwrap_or_else(|| panic!("the workspace has no {member}"));
        let (mut seen, mut left) = (BTreeSet::new(), vec![root.clone()]);
        while let Some(id) = left.pop() {
            if seen.insert(id.clone()) {
                left.extend(self.links.get(&id).into_iter().flatten().cloned());
            }
        }
        seen.into_iter().filter(|id| self.packages[id].third_party).collect()
    }
}

/// What `licenses.toml` says of what may be linked, and of the texts the packages do not hold.
struct Policy {
    allow: BTreeSet<String>,
    /// Licenses whose text is the same for every work under one, with that text.
    standard: BTreeMap<String, String>,
    /// By crate: files of its package to take besides, and texts kept by hand with the name of each.
    package: BTreeMap<String, Vec<String>>,
    kept: BTreeMap<String, Vec<(String, String)>>,
}

impl Policy {
    fn of(said: &toml::Table, hand: &Path) -> Policy {
        let allow = strings(said.get("policy").and_then(|policy| policy.get("allow"))).into_iter().collect();
        let mut standard = BTreeMap::new();
        for (license, file) in said.get("standard").and_then(toml::Value::as_table).into_iter().flatten() {
            standard.insert(license.clone(), read(&hand.join(file.as_str().expect("a standard text's file"))));
        }
        let (mut package, mut kept) = (BTreeMap::<String, Vec<String>>::new(), BTreeMap::<String, Vec<_>>::new());
        for entry in said.get("texts").and_then(toml::Value::as_array).into_iter().flatten() {
            for name in strings(entry.get("crates")) {
                package.entry(name.clone()).or_default().extend(strings(entry.get("package")));
                for file in strings(entry.get("files")) {
                    let path = hand.join(&file);
                    let label = path.file_name().expect("a file has a name").to_string_lossy().into_owned();
                    kept.entry(name.clone()).or_default().push((label, read(&path)));
                }
            }
        }
        Policy { allow, standard, package, kept }
    }

    /// A package's license texts, each with the name of the file it is in: those its package holds, and those kept
    /// by hand for it; the standard text where it has none and its license's text is the same for every work.
    fn texts(&self, package: &Package) -> Result<Vec<(String, String)>, String> {
        let Some(license) = &package.license else {
            return Err("it names no license".into());
        };
        let expression = Expression::parse(license).map_err(|e| format!("its license, `{license}`, does not read: {e}"))?;
        if expression.met_by(&|name| self.allow.contains(name)).is_none() {
            return Err(format!("`{license}` is not among the licenses allowed"));
        }
        let folder = package.manifest.parent().expect("a manifest is in a folder");
        let mut texts = Vec::new();
        let mut names: Vec<PathBuf> = std::fs::read_dir(folder).into_iter().flatten().flatten().map(|e| e.path()).collect();
        names.sort();
        for path in names {
            let name = path.file_name().expect("an entry has a name").to_string_lossy().into_owned();
            let lower = name.to_lowercase();
            let is_text = ["license", "licence", "copying", "notice", "unlicense", "copyright"]
                .iter()
                .any(|start| lower.starts_with(start));
            if is_text && path.is_file() {
                texts.push((name, read(&path)));
            } else if path.is_dir() && matches!(lower.as_str(), "license" | "licenses" | "licence" | "licences") {
                let mut inside: Vec<PathBuf> =
                    std::fs::read_dir(&path).into_iter().flatten().flatten().map(|e| e.path()).collect();
                inside.sort();
                for file in inside.into_iter().filter(|file| file.is_file()) {
                    let label = format!("{name}/{}", file.file_name().expect("it has a name").to_string_lossy());
                    texts.push((label, read(&file)));
                }
            }
        }
        for file in self.package.get(&package.name).into_iter().flatten() {
            let path = folder.join(file);
            if !path.is_file() {
                return Err(format!("its package has no {file}, which licenses.toml names"));
            }
            texts.push((file.clone(), read(&path)));
        }
        texts.extend(self.kept.get(&package.name).into_iter().flatten().cloned());
        if texts.is_empty() {
            // Under a license whose text names no work: the standard text is its text.
            let Some(under) = expression.met_by(&|name| self.standard.contains_key(name)) else {
                return Err(format!(
                    "its package holds no license text, and `{license}` has none that is the same for every work: \
                     keep its repository's under licenses/texts and name it in licenses.toml"
                ));
            };
            texts.extend(under.into_iter().map(|name| (format!("{name}, the standard text"), self.standard[&name].clone())));
        }
        Ok(texts)
    }
}

/// An SPDX license expression: `MIT OR Apache-2.0`, `(MIT OR Apache-2.0) AND Unicode-3.0`,
/// `Apache-2.0 WITH LLVM-exception`; `/` as older crates write `OR`.
enum Expression {
    License(String),
    Any(Vec<Expression>),
    All(Vec<Expression>),
}

impl Expression {
    fn parse(text: &str) -> Result<Expression, String> {
        let spaced = text.replace('(', " ( ").replace(')', " ) ").replace('/', " OR ");
        let tokens: Vec<&str> = spaced.split_whitespace().collect();
        let mut at = 0;
        let parsed = Expression::any(&tokens, &mut at)?;
        if at == tokens.len() { Ok(parsed) } else { Err(format!("`{}` is left over", tokens[at..].join(" "))) }
    }

    fn any(tokens: &[&str], at: &mut usize) -> Result<Expression, String> {
        let mut parts = vec![Expression::all(tokens, at)?];
        while tokens.get(*at).is_some_and(|t| t.eq_ignore_ascii_case("OR")) {
            *at += 1;
            parts.push(Expression::all(tokens, at)?);
        }
        Ok(if parts.len() == 1 { parts.remove(0) } else { Expression::Any(parts) })
    }

    fn all(tokens: &[&str], at: &mut usize) -> Result<Expression, String> {
        let mut parts = vec![Expression::one(tokens, at)?];
        while tokens.get(*at).is_some_and(|t| t.eq_ignore_ascii_case("AND")) {
            *at += 1;
            parts.push(Expression::one(tokens, at)?);
        }
        Ok(if parts.len() == 1 { parts.remove(0) } else { Expression::All(parts) })
    }

    fn one(tokens: &[&str], at: &mut usize) -> Result<Expression, String> {
        let token = *tokens.get(*at).ok_or("it ends where a license is expected")?;
        *at += 1;
        if token == "(" {
            let inside = Expression::any(tokens, at)?;
            if tokens.get(*at) != Some(&")") {
                return Err("a `(` is not closed".into());
            }
            *at += 1;
            return Ok(inside);
        }
        if matches!(token, ")" | "OR" | "AND" | "WITH") {
            return Err(format!("`{token}` where a license is expected"));
        }
        let mut name = token.to_string();
        if tokens.get(*at).is_some_and(|t| t.eq_ignore_ascii_case("WITH")) {
            let exception = tokens.get(*at + 1).ok_or("`WITH` names no exception")?;
            let _ = write!(name, " WITH {exception}");
            *at += 2;
        }
        Ok(Expression::License(name))
    }

    /// The licenses that meet it, taking of each choice the first that can be met, where those `allowed` alone can.
    fn met_by(&self, allowed: &dyn Fn(&str) -> bool) -> Option<Vec<String>> {
        match self {
            Expression::License(name) => allowed(name).then(|| vec![name.clone()]),
            Expression::Any(parts) => parts.iter().find_map(|part| part.met_by(allowed)),
            Expression::All(parts) => {
                let mut met = Vec::new();
                for part in parts {
                    met.extend(part.met_by(allowed)?);
                }
                Some(met)
            }
        }
    }
}
