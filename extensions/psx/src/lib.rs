//! pe2.psx, an unsandboxed extension: the PS1 target, as a toolchain file is `CMake`'s, and one step of a package. It provides
//! `package-target`, given the files `pe2.packages` gathered: it writes them out, compiles the units with the tools it ships and links them by
//! the package's `link_script` at the address of its slot. Slots are named by packages and mapped here, as what a target calls a place in
//! memory is no package's to say.

use std::fs;
use std::path::{Path, PathBuf};

use pe2_ext::events::Build;
use pe2_ext::process::spawn;
use pe2_ext::ty::{self, Files, Registered, SourceTree};
use pe2_ext::{Call, File, Registry, Result, Table, Typed, Value, anyhow, bail};

/// The target this is, as a config names it.
const TARGET: &str = "psx";

pe2_ext::extension!("pe2.psx", "0.1.0", register, requires = ["pe2.packages@^0.1"], provides = ["package-target@1.0.0"], unsandboxed = true, hashed = ["linux-x86_64"]);

/// The places a package loads into, by the names packages give them.
const SLOTS: [(&str, u32); 12] = [
    ("resident", 0x8009_3800),
    ("weapon", 0x8011_D1C0),
    ("aya", 0x8011_5770),
    ("kyle", 0x8016_7A70),
    ("stage", 0x8017_9950),
    ("overlay", 0x8012_EF30),
    ("options", 0x801D_4000),
    ("debug", 0x801D_6000),
    ("room", 0x8017_D5C0),
    ("actor_1", 0x8013_1E20),
    ("actor_2", 0x8014_9E20),
    ("actor_3", 0x8016_1E20),
];

const CPP: [&str; 7] = ["-undef", "-Wall", "-lang-c", "-nostdinc", "-D_LANGUAGE_C", "-DUSE_INCLUDE_ASM", "-DVER_USA"];
const CC1: [&str; 16] = [
    "-O2", "-mips1", "-mcpu=3000", "-w", "-funsigned-char", "-fpeephole", "-ffunction-cse", "-fpcc-struct-return", "-fcommon",
    "-fverbose-asm", "-msoft-float", "-mgas", "-fgnu-linker", "-quiet", "-gcoff", "-G0",
];
const MASPSX: [&str; 8] = ["--run-assembler", "--aspsx-version=2.77", "--expand-div", "-EL", "-O2", "-march=r3000", "-mtune=r3000", "-G0"];

/// An image the game loads.
pub struct Package;

impl Registered for Package {
    const NAME: &str = "pe2.psx@image";
    const LABEL: &str = "Package image";
}

impl Files for Package {}

#[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
fn register(r: &mut Registry) -> Result<()> {
    let _ = r.value_type::<Package>();
    let _ = r.op("preprocess").label("First pass").input::<SourceTree>("sources").output::<SourceTree>().run(first_pass);
    let _ = r.op("compile").label("Compile and link").input::<SourceTree>("sources").output::<ty::File<Package>>().run(build);
    let _ = r.op("flags").label("What a language server is told").takes("package", &["pe2.packages@package"]).output::<ty::Table>().provides("flags").run(flags);
    let _ = r.extend("pe2.packages@package").of(Package::NAME).register();
    let _ = r.on::<Build>(check_tools);
    Ok(())
}

/// The programs a build needs that it does not ship, looked for before anything is compiled: without them each package fails on its own, saying less.
fn check_tools(_: &mut Call) -> Result<()> {
    for tool in ["mips-linux-gnu-cpp", "mips-linux-gnu-ld", "mips-linux-gnu-objcopy", "python3"] {
        run(tool, &["--version".to_string()]).map_err(|e| anyhow!("building for {TARGET} needs {tool}: {e:#}"))?;
    }
    Ok(())
}

/// What a language server needs of this target to read a package's sources.
#[expect(clippy::unnecessary_wraps, reason = "an operation may fail; this one does not")]
fn flags(_: &mut Call) -> Result<Table> {
    let defines = CPP.iter().filter_map(|flag| flag.strip_prefix("-D")).map(|define| Value::String(define.to_string()));
    let mut table = Table::new();
    table.insert("triple".into(), "mips-linux-gnu".into());
    table.insert("defines".into(), Value::Array(defines.collect()));
    Ok(table)
}

fn build(call: &mut Call) -> Result<Typed> {
    let path = call.host.path().to_string();
    let name = path.rsplit('/').next().unwrap_or(&path).to_string();
    let fields = call.given();
    let target = call.config().and_then(|config| config.target);
    if target.as_ref().is_some_and(|target| target != TARGET) {
        bail!("the config builds for {}, and this is {TARGET}", target.unwrap_or_default());
    }
    let sources = call.take::<SourceTree>(0)?;
    let notes = sources.notes.clone();
    let scratch = PathBuf::from(format!("{}.build", call.temp()?));
    fs::create_dir_all(&scratch)?;
    let made = write_out(call, &sources, &scratch).and_then(|tree| image_of(&fields, &tree, &scratch, &name));
    let result = made.and_then(|image| {
        let kept = call.keep(&image.display().to_string(), &format!("{name}.pe2pkg"))?;
        Ok(Typed { notes: notes.clone(), ..Typed::new(Package::NAME, vec![kept]) })
    });
    let _ = fs::remove_dir_all(&scratch);
    // What a tool said of the tree it was given is said of the files they are.
    in_project(result, &scratch, &path)
}

/// What a tool said of the tree it was given, said of the files they are.
fn in_project<T>(result: Result<T>, scratch: &Path, path: &str) -> Result<T> {
    let tree = scratch.join("tree").display().to_string();
    result.map_err(|e| anyhow!("{}", format!("{e:#}").replace(&format!("{tree}/self/"), &format!("{path}/")).replace(&format!("{tree}/pkg/"), "packages/").replace(&format!("{tree}/"), "")))
}

/// What the units come to with `PE2_INCLUDE` and the options expanded and the other `PE2_*` macros left as they are written, for
/// `pe2.packages` to read: each unit beside what was gathered, under `first/`.
fn first_pass(call: &mut Call) -> Result<Typed> {
    let mut sources = call.take::<SourceTree>(0)?;
    let scratch = PathBuf::from(format!("{}.first", call.temp()?));
    fs::create_dir_all(&scratch)?;
    let made = write_out(call, &sources, &scratch).and_then(|tree| {
        let mut made = vec![];
        for unit in fs::read_to_string(tree.join("units"))?.lines() {
            let expanded = scratch.join("expanded.i");
            let mut args: Vec<String> = CPP.iter().map(ToString::to_string).collect();
            args.extend(["-P".to_string(), "-DPE2_FIRST_PASS".to_string(), format!("-I{}", text(&tree)), "-o".to_string(), text(&expanded), text(&tree.join(unit))]);
            run("mips-linux-gnu-cpp", &args)?;
            made.push(File::new(&format!("first/{unit}"), fs::read(&expanded)?));
        }
        Ok(made)
    });
    let _ = fs::remove_dir_all(&scratch);
    let path = call.host.path().to_string();
    let unset = sources.settings.get("unset").and_then(Value::as_array).map_or_else(Vec::new, |names| names.iter().filter_map(Value::as_str).collect());
    sources.files.extend(in_project(made, &scratch, &path).map_err(|e| no_value(e, &unset))?);
    Ok(sources)
}

/// A failure of an option with no value, said of the line asking for it: the compiler names the header defining it.
fn no_value(error: pe2_ext::Error, unset: &[&str]) -> pe2_ext::Error {
    let said = format!("{error:#}");
    let Some(name) = unset.iter().find(|name| said.contains(&format!("pe2_unset_{name}"))) else { return error };
    let asked = said.lines().filter_map(|line| line.split_once(": note: in expansion of macro")).find(|(_, by)| by.contains("PE2_OPTION"));
    let at = asked.and_then(|(place, _)| place.rsplit_once(':')).map(|(line, _)| line);
    match at {
        Some(at) => anyhow!("{at}: {name} has no value on this package"),
        None => error.context(format!("{name} has no value on this package")),
    }
}

/// What was gathered, on disk for the tools to read.
fn write_out(call: &Call, sources: &Typed, scratch: &Path) -> Result<PathBuf> {
    let tree = scratch.join("tree");
    for file in &sources.files {
        let to = tree.join(&file.name);
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(to, call.read(file)?)?;
    }
    Ok(tree)
}

fn image_of(fields: &Table, tree: &Path, scratch: &Path, name: &str) -> Result<PathBuf> {
    let slot = fields.get("slot").and_then(Value::as_str).ok_or_else(|| anyhow!("{name} names no slot"))?;
    let (_, address) = SLOTS.iter().find(|(known, _)| *known == slot).ok_or_else(|| anyhow!("no slot {slot} on this target"))?;
    let mut objects = vec![];
    for unit in fs::read_to_string(tree.join("units"))?.lines() {
        objects.push(compile(&tree.join(unit), tree, scratch)?);
    }
    let own = tree.join("target/psx").join(name);
    let script = only_file(&own.join("link_script")).map_err(|_| anyhow!("{name} has no link_script"))?;
    let mut published = vec![];
    if let Ok(packages) = fs::read_dir(tree.join("target/psx")) {
        for package in packages {
            if let Ok(file) = only_file(&package?.path().join("symbols")) {
                published.push(file);
            }
        }
    }
    link(*address, &script, &published, &objects, scratch, name)
}

/// The one file a property of a package is in.
fn only_file(folder: &Path) -> Result<PathBuf> {
    let found = fs::read_dir(folder)?.next().ok_or_else(|| anyhow!("{} is empty", folder.display()))??;
    Ok(found.path())
}

fn text(path: &Path) -> String {
    path.display().to_string()
}

/// A program, run to its end. maspsx reads its standard input before its file: it gets none.
fn run(tool: &str, args: &[String]) -> Result<()> {
    let mut process = spawn(tool, args)?;
    process.close_input();
    process.wait()
}

fn compile(unit: &Path, tree: &Path, scratch: &Path) -> Result<PathBuf> {
    let stem = unit.file_stem().map(|s| s.to_string_lossy().into_owned()).ok_or_else(|| anyhow!("{} has no name", unit.display()))?;
    let (expanded, assembly, object) = (scratch.join(format!("{stem}.i")), scratch.join(format!("{stem}.s")), scratch.join(format!("{stem}.o")));
    let mut args: Vec<String> = CPP.iter().map(ToString::to_string).collect();
    args.extend([format!("-I{}", text(tree)), "-o".to_string(), text(&expanded), text(unit)]);
    run("mips-linux-gnu-cpp", &args)?;
    let mut args: Vec<String> = CC1.iter().map(ToString::to_string).collect();
    args.extend(["-o".to_string(), text(&assembly), text(&expanded)]);
    run("cc1", &args)?;
    let mut args: Vec<String> = MASPSX.iter().map(ToString::to_string).collect();
    args.extend(["-no-pad-sections".to_string(), format!("-I{}", text(&tree.join("pkg/core"))), "-o".to_string(), text(&object), text(&assembly)]);
    run("maspsx", &args)?;
    Ok(object)
}

/// The image: the package's script for where things land, this target for where its slot is, what its dependencies publish for the rest.
fn link(address: u32, script: &Path, published: &[PathBuf], objects: &[PathBuf], scratch: &Path, name: &str) -> Result<PathBuf> {
    let linked = scratch.join("image.elf");
    let mut args: Vec<String> = ["-EL", "-nostdlib", "--no-check-sections", "--defsym"].map(ToString::to_string).to_vec();
    args.push(format!("SLOT={address:#x}"));
    args.extend(["-T".to_string(), text(script)]);
    for file in published {
        args.extend(["-T".to_string(), text(file)]);
    }
    args.extend(["-o".to_string(), text(&linked)]);
    args.extend(objects.iter().map(|object| text(object)));
    run("mips-linux-gnu-ld", &args)?;
    let image = scratch.join(format!("{name}.pe2pkg"));
    run("mips-linux-gnu-objcopy", &["-O".into(), "binary".into(), "-j".into(), ".image".into(), text(&linked), text(&image)])?;
    Ok(image)
}

#[cfg(test)]
mod tests {
    use pe2_ext::anyhow;

    #[test]
    fn an_option_with_no_value_is_told_at_the_line_asking() {
        let said = "exit status: 1: In file included from pe2.h:29,\npe2_options.h:1:26: error: division by zero in #if\n\
                    1 | #define pe2_option_foo (0/pe2_unset_foo)\n\
                    packages/title/src/title.c:4:5: note: in expansion of macro \u{2018}PE2_OPTION\u{2019}\n";
        assert_eq!(super::no_value(anyhow!("{said}"), &["bar", "foo"]).to_string(), "packages/title/src/title.c:4: foo has no value on this package");
        assert_eq!(super::no_value(anyhow!("other"), &["foo"]).to_string(), "other");
    }
}
