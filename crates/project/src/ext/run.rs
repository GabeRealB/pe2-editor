//! `run`: a box of a graph that runs a program on files - one action of a build, written by hand or by a generator.
//!
//! What it takes (`inputs`, any number of values) is laid out in a folder of its own, each file by its name; the program
//! is started with its arguments there, and what it writes into its output folder under the names `outputs` says is what
//! it makes: a value of the type `makes` names (`pe2.graph@files` where unsaid), those files its own.
//!
//! The program is one an extension offers (`<extension>:<program>`, `Extensions::program`): only what a trusted extension
//! offers runs, and only for a project the user trusts as a whole, as what runs programs does. Its arguments are text
//! with placeholders:
//!
//! | Placeholder | What it comes to |
//! |---|---|
//! | `{tree}` | the folder what it takes is laid out in |
//! | `{in}`, `{in.path}`, `{in.stem}` | the first file it takes: its path, its name, its name without its extension |
//! | `{inputs}` | every file it takes, one argument each |
//! | `{files.<folder>}` | the files it takes under a folder (`objects/`), one argument each, in the order given |
//! | `{given.<key>}` | a setting of the first value it takes (a unit's `object`) |
//! | `{out}`, `{out.<n>}` | the path of its first output, of its `n`th (from 0) |
//! | `{config.release}`, `{config.target}`, `{config.name}`, `{config.properties.<p>}` | the configuration built |
//! | `{values.<key>}` | what its `values` setting holds at the key (a table, usually wired from an operation's settings) |
//!
//! What the program prints is an output too where `stdout` names one: a listing it makes, say. It runs in the folder of
//! what it takes, or in one within it (`cwd`), with what the editor runs with and `env` besides (a table, its values'
//! placeholders filled): an `INCLUDE` or a `PATH` a toolchain needs.
//!
//! An argument holding a placeholder that comes to a list is one argument for each item, with the text around it:
//! `-I{values.includes}` is `-Ia -Ib`. An output's name may hold `{in.path}` and `{in.stem}`. Outputs named in `rewrite`
//! are text in which the folder it took is written as `tree`, so that what it makes is the same wherever it ran - a
//! preprocessor's line markers - and so is what is made of it.

use pe2_path::{Name, RelPath};
use sfs::HostPath;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};
use pe2_ext::{Content, File, Type, Typed};
use toml::{Table, Value as Toml};

use crate::graph::{self, OpDef, Value, exec};

/// What a `run` box is named.
pub const RUN: &str = "run";

/// What it makes where `makes` says nothing: files.
pub const FILES: &str = "pe2.graph@files";

/// What an input folder is written as in what is rewritten.
const TREE: &str = "tree";

/// The `run` box: programs started through `offered` (`Extensions::program`), while `trusted` says the project is.
/// `version` tells what it runs from what it ran: the extensions offering programs, by their hashes.
pub fn op(offered: super::unsandboxed::Offered, trusted: Arc<AtomicBool>, version: &str) -> OpDef {
    let makes = |settings: &Table| Some(Type::Named(settings.get("makes")?.as_str()?.to_string()));
    OpDef::new(RUN, "Run a program", version).input(graph::ports("inputs", Type::Any)).output_of(makes).makes(
        Type::Any,
        move |call| {
            anyhow::ensure!(
                trusted.load(Ordering::Relaxed),
                "it runs a program, and this project is not trusted: trust it (`pe2-cli project trust --all`) for its \
                 operations to run programs"
            );
            run(call, &offered)
        },
    )
}

/// What one run is given: its files, and where they and what it writes are laid out.
struct Laid {
    /// The folder it made for this run, removed when it is done; none where it made none.
    folder: Option<HostPath>,
    input: HostPath,
    output: HostPath,
    files: Vec<Arc<RelPath>>,
    /// The I/O it is laid out through.
    io: Arc<dyn pe2_io::Io>,
}

impl Drop for Laid {
    fn drop(&mut self) {
        if let Some(folder) = &self.folder {
            let _ = self.io.filesystem().remove_all(folder);
        }
    }
}

fn run(call: &mut exec::Call<'_>, offered: &super::unsandboxed::Offered) -> Result<Value> {
    let settings = call.settings().clone();
    let program = settings.get("program").and_then(Toml::as_str).context("it runs no `program`")?.to_string();
    let taken = call.take_all(0);
    let given: Vec<File> = taken.iter().flat_map(|input| files_of(input.value())).collect();
    // What it is told: the settings of what it takes, the first's where two say the same.
    let mut told = Table::new();
    for input in taken.iter().rev() {
        if let Value::Typed(typed) = input.value() {
            told.extend(typed.settings.clone());
        }
    }
    let laid = lay_out(call, &given)?;
    let config = config_of(call, &settings);
    let first = laid.files.first().map(|path| path.as_str());
    let outputs: Vec<Arc<RelPath>> = texts(&settings, "outputs")
        .iter()
        .map(|name| {
            let name = named(name, first, &told);
            let path =
                RelPath::new(&name).with_context(|| format!("its output {name}, which is no path within a folder"))?;
            Ok(path.to_owned())
        })
        .collect::<Result<_>>()?;
    let stdout = settings.get("stdout").and_then(Toml::as_str).map(|name| named(name, first, &told));
    let stdout = stdout
        .map(|name| {
            let path =
                RelPath::new(&name).with_context(|| format!("its stdout {name}, which is no path within a folder"))?;
            Ok::<_, anyhow::Error>(path.to_owned())
        })
        .transpose()?;
    for name in &outputs {
        if let Some(parent) = laid.output.join(name).parent() {
            call.io().filesystem().create_folder_all(&parent).with_context(|| parent.to_string())?;
        }
    }
    let values = settings.get("values").and_then(Toml::as_table).cloned().unwrap_or_default();
    let places = Places { laid: &laid, outputs: &outputs, config: &config, values: &values, told: &told };
    let mut args = Vec::new();
    for arg in settings.get("args").and_then(Toml::as_array).map_or(&[][..], Vec::as_slice) {
        match arg {
            Toml::String(arg) => args.extend(places.expand(arg)?),
            // A group: its arguments together, once for each item of the list one comes to.
            Toml::Array(group) => {
                args.extend(places.expand_group(&group.iter().filter_map(Toml::as_str).collect::<Vec<_>>())?);
            }
            other => bail!("an argument is text or a group of texts, not {other}"),
        }
    }
    let (path, mut before, _) = super::program_in(offered, &program)?;
    before.extend(args);
    let mut command = std::process::Command::new(path.as_os_str());
    let folder = match settings.get("cwd").and_then(Toml::as_str) {
        Some(within) => match RelPath::new(within) {
            Ok(within) => laid.input.join(&within),
            Err(_) => bail!("its cwd, {within}, is no folder within what it takes"),
        },
        None => laid.input.clone(),
    };
    command.args(&before).current_dir(&folder).stdin(std::process::Stdio::null());
    // What it is run with besides what the editor has: each value's placeholders filled, one value each.
    for (name, value) in settings.get("env").and_then(Toml::as_table).into_iter().flatten() {
        let value = value.as_str().with_context(|| format!("its env {name} is no text"))?;
        command.env(name, places.expand(value)?.join(" "));
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let done = command.output().with_context(|| format!("{program} ({path}) could not be started"))?;
    let said = |bytes: &[u8]| rewritten(&String::from_utf8_lossy(bytes), &laid);
    if !done.status.success() {
        let (out, err) = (said(&done.stdout), said(&done.stderr));
        bail!(
            "{program}: {}: {}{}",
            done.status,
            err.trim_end(),
            if out.trim().is_empty() { String::new() } else { format!("\n{}", out.trim_end()) }
        );
    }
    let rewrite = texts(&settings, "rewrite");
    let mut made = Vec::new();
    if let Some(name) = &stdout {
        made.push(File { name: name.clone(), content: Content::of(&done.stdout) });
    }
    for name in &outputs {
        let file = laid.output.join(name);
        let mut bytes = call.io().filesystem().read(&file).with_context(|| format!("{program} made no {name}"))?;
        if rewrite.iter().any(|r| named(r, first, &told) == name.as_str()) {
            bytes = rewritten(&String::from_utf8_lossy(&bytes), &laid).into_bytes();
        }
        made.push(File { name: name.clone(), content: Content::owning(bytes) });
    }
    let makes = settings.get("makes").and_then(Toml::as_str).unwrap_or(FILES);
    Ok(Value::Typed(Typed::new(makes, made)))
}

/// The files of a value: a typed one's, a list's items'.
fn files_of(value: &Value) -> Vec<File> {
    match value {
        Value::Typed(typed) => typed.files.clone(),
        Value::List(items) => items.iter().flat_map(files_of).collect(),
        _ => Vec::new(),
    }
}

/// The files given, laid out by their names in a folder of their own (`in`), beside the folder it writes to (`out`).
fn lay_out(call: &exec::Call<'_>, given: &[File]) -> Result<Laid> {
    use std::sync::atomic::AtomicU64;
    static RUNS: AtomicU64 = AtomicU64::new(0);
    const IN: &Name = Name::constant("in");
    const OUT: &Name = Name::constant("out");
    let run = format!("pe2-run-{}-{}", std::process::id(), RUNS.fetch_add(1, Ordering::Relaxed));
    let run = Name::new(&run)?;
    let folder = call.io().environment().temp_dir().context("the system's folder of temporary files")?.join(&run);
    let fs = call.io().filesystem();
    let _ = fs.remove_all(&folder);
    let laid = Laid {
        input: folder.join(&IN),
        output: folder.join(&OUT),
        folder: Some(folder),
        files: Vec::new(),
        io: call.io().clone(),
    };
    fs.create_folder_all(&laid.output).with_context(|| laid.output.to_string())?;
    fs.create_folder_all(&laid.input).with_context(|| laid.input.to_string())?;
    let mut laid = laid;
    for file in given {
        let name = &file.name;
        let path = laid.input.join(name);
        if let Some(parent) = path.parent() {
            fs.create_folder_all(&parent).with_context(|| parent.to_string())?;
        }
        fs.write(&path, &call.read(&file.content)?).with_context(|| path.to_string())?;
        laid.files.push(name.clone());
    }
    Ok(laid)
}

/// What of the configuration its arguments name, read only as far as they do: none, nothing read.
fn config_of(call: &exec::Call<'_>, settings: &Table) -> Table {
    let named = texts(settings, "args").iter().chain(texts(settings, "outputs").iter()).any(|a| a.contains("{config."));
    if !named {
        return Table::new();
    }
    let Some(config) = call.config() else { return Table::new() };
    let mut table = Table::new();
    table.insert("name".into(), Toml::String(config.name));
    table.extend(config.release.map(|r| ("release".to_string(), Toml::String(r))));
    table.extend(config.target.map(|t| ("target".to_string(), Toml::String(t))));
    table.insert("properties".into(), Toml::Table(config.properties));
    table
}

/// A setting that is a list of strings, or none.
fn texts(settings: &Table, key: &str) -> Vec<String> {
    settings
        .get(key)
        .and_then(Toml::as_array)
        .map_or_default(|items| items.iter().filter_map(Toml::as_str).map(ToString::to_string).collect())
}

/// An output's name, its placeholders of the first file taken (and of what it was told: `{given.<key>}`) filled.
fn named(name: &str, first: Option<&str>, told: &Table) -> String {
    let first = first.unwrap_or_default();
    // The first file's name without what follows its last dot, as a name's stem is.
    let name_of = first.rsplit('/').next().unwrap_or_default();
    let stem = match name_of.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem.to_owned(),
        _ => name_of.to_owned(),
    };
    let mut name = name.replace("{in.path}", first).replace("{in.stem}", &stem);
    for (key, value) in told {
        if let Some(text) = value.as_str() {
            name = name.replace(&format!("{{given.{key}}}"), text);
        }
    }
    name
}

/// Text in which the folders a run laid out are written as `tree` (what it took) and `tree.out` (what it wrote).
fn rewritten(text: &str, laid: &Laid) -> String {
    let mut text = text.to_string();
    for (folder, said) in [(&laid.input, TREE.to_string()), (&laid.output, format!("{TREE}.out"))] {
        for form in forms(folder) {
            text = text.replace(&form, &said);
        }
    }
    text
}

/// The ways a folder's path is written: as it is, and with `/` (Windows' programs write either).
fn forms(folder: &HostPath) -> Vec<String> {
    let plain = folder.display().to_string();
    let slashed = plain.replace('\\', "/");
    if slashed == plain { vec![plain] } else { vec![plain, slashed] }
}

/// What placeholders come to in one run.
struct Places<'a> {
    laid: &'a Laid,
    outputs: &'a [Arc<RelPath>],
    config: &'a Table,
    values: &'a Table,
    told: &'a Table,
}

impl Places<'_> {
    /// An argument, its placeholders filled: one, or one for each item of the list one comes to.
    fn expand(&self, arg: &str) -> Result<Vec<String>> {
        let mut parts = Vec::new();
        let mut rest = arg;
        let mut list: Option<(String, Vec<String>, String)> = None;
        let mut before = String::new();
        while let Some(open) = rest.find('{') {
            let close = rest[open..].find('}').map(|c| open + c).with_context(|| format!("{arg}: an unclosed {{"))?;
            before.push_str(&rest[..open]);
            match self.value(&rest[open + 1..close])? {
                Filled::One(text) => before.push_str(&text),
                Filled::Many(items) => {
                    anyhow::ensure!(list.is_none(), "{arg}: two placeholders coming to lists in one argument");
                    list = Some((std::mem::take(&mut before), items, String::new()));
                }
            }
            rest = &rest[close + 1..];
        }
        before.push_str(rest);
        match list {
            None => parts.push(before),
            Some((prefix, items, _)) => parts.extend(items.into_iter().map(|item| format!("{prefix}{item}{before}"))),
        }
        Ok(parts)
    }

    /// Arguments together: once, or once for each item of the list one of them comes to (`-iquote` before each folder).
    fn expand_group(&self, group: &[&str]) -> Result<Vec<String>> {
        let expanded: Vec<Vec<String>> = group.iter().map(|arg| self.expand(arg)).collect::<Result<_>>()?;
        let times = expanded.iter().map(Vec::len).filter(|n| *n != 1).max().unwrap_or(1);
        anyhow::ensure!(
            expanded.iter().all(|e| e.len() == 1 || e.len() == times),
            "{group:?}: its lists are of different lengths"
        );
        let mut args = Vec::new();
        for at in 0..times {
            for each in &expanded {
                args.push(if each.len() == 1 { each[0].clone() } else { each[at].clone() });
            }
        }
        Ok(args)
    }

    fn value(&self, name: &str) -> Result<Filled> {
        let first = self.laid.files.first();
        Ok(match name {
            "tree" => Filled::One(self.laid.input.to_string()),
            "in" => Filled::One(self.laid.input.argument(first.context("{in}: it is given no file")?)),
            "in.path" => Filled::One(first.map_or_else(String::new, ToString::to_string)),
            "in.stem" => Filled::One(named("{in.stem}", first.map(|path| path.as_str()), &Table::new())),
            "inputs" => Filled::Many(self.laid.files.iter().map(|f| self.laid.input.argument(f)).collect()),
            "out" => Filled::One(self.laid.output.argument(self.outputs.first().context("{out}: it names no output")?)),
            _ => {
                if let Some(at) = name.strip_prefix("out.") {
                    let at: usize = at.parse().with_context(|| format!("{{{name}}}: an output by its place"))?;
                    let output =
                        self.outputs.get(at).with_context(|| format!("{{{name}}}: it names no such output"))?;
                    return Ok(Filled::One(self.laid.output.argument(output)));
                }
                if let Some(folder) = name.strip_prefix("files.") {
                    let under = self.laid.files.iter().filter(|f| f.as_str().starts_with(folder));
                    return Ok(Filled::Many(under.map(|f| self.laid.input.argument(f)).collect()));
                }
                let (table, key) = if let Some(key) = name.strip_prefix("config.") {
                    (self.config, key)
                } else if let Some(key) = name.strip_prefix("given.") {
                    (self.told, key)
                } else if let Some(key) = name.strip_prefix("values.") {
                    (self.values, key)
                } else {
                    bail!("{{{name}}} is no placeholder of a run");
                };
                let mut at = None;
                let mut within = Some(table);
                for part in key.split('.') {
                    at = within.and_then(|t| t.get(part));
                    within = at.and_then(Toml::as_table);
                }
                match at {
                    Some(Toml::String(text)) => Filled::One(text.clone()),
                    Some(Toml::Array(items)) => Filled::Many(items.iter().map(scalar).collect()),
                    Some(other) => Filled::One(scalar(other)),
                    None => Filled::Many(Vec::new()),
                }
            }
        })
    }
}

/// What a placeholder comes to.
enum Filled {
    One(String),
    Many(Vec<String>),
}

fn scalar(value: &Toml) -> String {
    match value {
        Toml::String(text) => text.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
#[allow(
    clippy::disallowed_methods,
    reason = "a test's fixtures: the machine's time, places, files and environment, which nothing tested reads"
)]
mod tests {
    use super::{Laid, Places};
    use crate::testing::At as _;
    use pe2_path::RelPath;

    #[test]
    fn placeholders_fill_an_argument_or_make_one_of_each_item() {
        let run = crate::testing::scratch("run-places");
        let laid = Laid {
            io: pe2_io::system(),
            folder: None,
            input: run.at("in"),
            output: run.at("out"),
            files: vec![
                RelPath::new("self/src/a.c").unwrap().to_owned(),
                RelPath::new("self/src/b.h").unwrap().to_owned(),
            ],
        };
        let outputs = [RelPath::new("self/src/a.c.s").unwrap().to_owned()];
        let config: toml::Table = toml::from_str("release = \"USA\"\n[properties]\nlanguage = \"en\"").unwrap();
        let values: toml::Table =
            toml::from_str("flags = [\"-O2\", \"-G0\"]\nincludes = [\"self\", \"dep\"]\nmode = \"x\"").unwrap();
        let told: toml::Table = toml::from_str("object = \"title/src/a.o\"").unwrap();
        let places = Places { laid: &laid, outputs: &outputs, config: &config, values: &values, told: &told };
        let one = |arg: &str| places.expand(arg).unwrap();
        // A file of what it took, or one it makes: the folder as this platform writes it, the path within with `/`.
        let at = |folder: &str, file: &str| run.at(folder).argument(RelPath::new(file).unwrap());
        assert_eq!(one("-DVER_{config.release}"), ["-DVER_USA"]);
        assert_eq!(one("{config.properties.language}"), ["en"]);
        assert_eq!(one("{values.flags}"), ["-O2", "-G0"]);
        let tree = laid.input.to_string();
        assert_eq!(one("-I{tree}/{values.includes}"), [format!("-I{tree}/self"), format!("-I{tree}/dep")]);
        assert_eq!(one("{in}"), [at("in", "self/src/a.c")]);
        assert_eq!(one("{out}"), [at("out", "self/src/a.c.s")]);
        assert_eq!(one("{inputs}"), [at("in", "self/src/a.c"), at("in", "self/src/b.h")]);
        assert_eq!(one("{values.nothing}"), Vec::<String>::new());
        assert!(places.expand("{nonsense}").is_err());
        assert!(places.expand("{values.flags}{values.includes}").is_err());
        assert_eq!(one("{files.self/src/a}"), [at("in", "self/src/a.c")]);
        assert_eq!(one("{given.object}"), ["title/src/a.o"]);
        let grouped = places.expand_group(&["-iquote", "{tree}/{values.includes}"]).unwrap();
        assert_eq!(
            grouped,
            ["-iquote".to_string(), format!("{tree}/self"), "-iquote".to_string(), format!("{tree}/dep")]
        );
        assert_eq!(places.expand_group(&["-T", "{values.nothing}"]).unwrap(), Vec::<String>::new());
        assert_eq!(super::named("{in.path}.o", Some("self/src/a.c"), &told), "self/src/a.c.o");
        assert_eq!(super::named("{in.stem}.i", Some("self/src/a.c"), &told), "a.i");
        assert_eq!(super::named("{given.object}", Some("self/src/a.c"), &told), "title/src/a.o");
        let said = format!("# 1 \"{}/self/src/a.c\"", laid.input);
        assert_eq!(super::rewritten(&said, &laid), "# 1 \"tree/self/src/a.c\"");
    }
}
