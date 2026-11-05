//! pe2.psx, an unsandboxed extension: the PS1 target, as a toolchain file is `CMake`'s, and the last steps of a package. It provides
//! `package-target`, given the files `pe2.packages` gathered: `compile` makes the package's objects - its C units with the tools it ships, its
//! assembly as written, its own prebuilt objects as they are - each named by its package and its place in it (`title/src/package.o`), as a
//! link script names it; `symbols` lays an image out alone and says what it exports; `link` makes its image by the package's `link_script`,
//! which places it in memory, with what its dependencies' objects add and against what the images it binds publish. A target that links
//! packages otherwise takes their objects and leaves the last two out.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use pe2_ext::events::Build;
use pe2_ext::process::spawn;
use pe2_ext::ty::{self, Files, Registered, SourceTree};
use pe2_ext::{Call, File, Micro, MicroInput, Prepared, Registry, Result, Table, Typed, Value, anyhow, bail};

/// The target this is, as a config names it.
const TARGET: &str = "psx";

pe2_ext::extension!(
    "pe2.psx",
    "0.1.0",
    register,
    requires = ["pe2.packages@^0.1"],
    provides = ["package-target@1.0.0"],
    unsandboxed = true,
    hashed = ["linux-x86_64"]
);

/// A sector of the disc, which the executable's header and image fill whole.
const SECTOR: usize = 0x800;

/// Where the BIOS puts the stack: the top of the 2 MB of RAM, less a few words.
const STACK: u32 = 0x801F_FFF0;

/// The license string a release's region carries in its executable's header, at 0x4C.
const REGIONS: [(&str, &str); 3] = [
    ("USA", "Sony Computer Entertainment Inc. for North America area"),
    ("JAP", "Sony Computer Entertainment Inc. for Japan area"),
    ("PAL", "Sony Computer Entertainment Inc. for Europe area"),
];

/// Where an ELF's first loaded segment goes: the executable's load address, which its link script gives.
fn load_address(elf: &[u8]) -> Result<u32> {
    let word = |at: usize| -> Result<u32> {
        Ok(u32::from_le_bytes(elf.get(at..at + 4).ok_or_else(|| anyhow!("the ELF is cut short"))?.try_into()?))
    };
    let half = |at: usize| -> Result<usize> {
        Ok(usize::from(u16::from_le_bytes(
            elf.get(at..at + 2).ok_or_else(|| anyhow!("the ELF is cut short"))?.try_into()?,
        )))
    };
    let (table, size, count) = (usize::try_from(word(0x1C)?)?, half(0x2A)?, half(0x2C)?);
    for index in 0..count {
        let header = table + index * size;
        if word(header)? == 1 {
            return word(header + 8);
        }
    }
    bail!("the ELF loads nothing")
}

/// The executable: its header, every field of which follows from the link or this target - the entry point (the ELF's),
/// where the image loads and how much of it, padded to whole sectors, the stack, the region's license string - then the
/// image, padded. `tools/buildsteps/psx_exe.py` writes the same bytes.
fn exe_header(image: &[u8], elf: &[u8], address: u32, release: &str) -> Result<Vec<u8>> {
    if elf.get(..6) != Some(b"\x7fELF\x01\x01".as_slice()) {
        bail!("the linked image is no 32-bit little-endian ELF");
    }
    let entry = u32::from_le_bytes(elf[0x18..0x1C].try_into()?);
    let (_, region) = REGIONS
        .iter()
        .find(|(known, _)| *known == release)
        .ok_or_else(|| anyhow!("no region for the release {release}"))?;
    let size = image.len().div_ceil(SECTOR) * SECTOR;
    let mut out = b"PS-X EXE".to_vec();
    for field in [0, 0, entry, 0, address, u32::try_from(size)?, 0, 0, 0, 0, STACK, 0, 0, 0, 0, 0, 0] {
        out.extend(field.to_le_bytes());
    }
    out.extend(region.as_bytes());
    out.resize(SECTOR, 0);
    out.extend(image);
    out.resize(SECTOR + size, 0);
    Ok(out)
}

/// The preprocessor: GCC 14.2.0's `cc1`, which the extension ships for each platform at this path, run without a driver
/// (`tools/<os>/cpp/README.md`). A package is preprocessed with `-Werror`, so a macro defined twice stops it.
const PREPROCESSOR: &str = "cpp/libexec/gcc/host/14.2.0/cc1";
const CPP: [&str; 9] =
    ["-E", "-quiet", "-undef", "-Wall", "-Werror", "-nostdinc", "-D_LANGUAGE_C", "-DUSE_INCLUDE_ASM", "-DVER_USA"];
const CC1: [&str; 16] = [
    "-O2",
    "-mips1",
    "-mcpu=3000",
    "-w",
    "-funsigned-char",
    "-fpeephole",
    "-ffunction-cse",
    "-fpcc-struct-return",
    "-fcommon",
    "-fverbose-asm",
    "-msoft-float",
    "-mgas",
    "-fgnu-linker",
    "-quiet",
    "-gcoff",
    "-G0",
];
/// Assembly, as the build assembles it: little-endian R3000, no small data.
const ASSEMBLER: [&str; 6] = ["-EL", "-O2", "-march=r3000", "-mtune=r3000", "-no-pad-sections", "-G0"];
const MASPSX: [&str; 8] =
    ["--run-assembler", "--aspsx-version=2.77", "--expand-div", "-EL", "-O2", "-march=r3000", "-mtune=r3000", "-G0"];

/// An image the game loads.
pub struct Package;

impl Registered for Package {
    const NAME: &str = "pe2.psx@image";
    const LABEL: &str = "Package image";
}

impl Files for Package {}

/// A compiled unit, on its way to being linked.
pub struct Object;

impl Registered for Object {
    const NAME: &str = "pe2.psx@compiled_unit";
    const LABEL: &str = "Compiled unit";
}

impl Files for Object {}

/// A package's objects: its units compiled and what it links as it is, each named by its package and its place in it.
pub struct Objects;

impl Registered for Objects {
    const NAME: &str = "pe2.psx@objects";
    const LABEL: &str = "Objects";
}

impl Files for Objects {}

/// What an image exports, at the addresses its layout alone gives them: what the images binding its package link against.
pub struct Symbols;

impl Registered for Symbols {
    const NAME: &str = "pe2.psx@exports";
    const LABEL: &str = "Published symbols";
}

impl Files for Symbols {}

#[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
fn register(r: &mut Registry) -> Result<()> {
    let _ = r.value_type::<Package>();
    let _ = r.value_type::<Object>();
    let _ = r.value_type::<Objects>();
    let _ = r.value_type::<Symbols>();
    let _ =
        r.op("preprocess").label("First pass").input::<SourceTree>("sources").output::<SourceTree>().run(first_pass);
    let _ = r.op("expand").label("Expand the units").input::<SourceTree>("sources").output::<SourceTree>().run(expand);
    let _ = r
        .op("object")
        .label("Compile a unit")
        .input::<SourceTree>("expanded")
        .input::<SourceTree>("core")
        .output::<ty::File<Object>>()
        .run(object);
    let _ = r
        .op("assemble")
        .label("Assemble a unit")
        .input::<SourceTree>("unit")
        .output::<ty::File<Object>>()
        .run(assemble);
    let _ = r
        .op("collect")
        .label("The package's objects")
        .inputs::<ty::File<Object>>("units")
        .input::<SourceTree>("prebuilt")
        .output::<Objects>()
        .run(collect);
    // Its work is its micro ops', each unit kept by what it comes to expanded, so that a file changed compiles what it reaches.
    let _ = r
        .op("compile")
        .label("Compile")
        .input::<SourceTree>("sources")
        .output::<Objects>()
        .prepare(split_build)
        .run(|_| bail!("it is done by its micro ops"));
    let _ = r
        .op("symbols")
        .label("Published symbols")
        .input::<SourceTree>("sources")
        .input::<Objects>("objects")
        .output::<Symbols>()
        .run(symbols);
    // What it links of other packages - their objects, what they publish - it reads as it runs: a first run says which.
    let _ = r
        .op("link")
        .label("Link")
        .input::<SourceTree>("sources")
        .input::<Objects>("objects")
        .output::<ty::File<Package>>()
        .prepared_by_running()
        .run(link_image);
    let _ = r
        .op("flags")
        .label("What a language server is told")
        .takes("package", &["pe2.packages@package"])
        .output::<ty::Table>()
        .provides("flags")
        .run(flags);
    let _ = r.extend("pe2.packages@package").of(Package::NAME).register();
    let _ = r.on::<Build>(check_tools);
    Ok(())
}

/// The programs a build needs that it does not ship, looked for before anything is compiled: without them each package fails on its own, saying less.
fn check_tools(_: &mut Call) -> Result<()> {
    for tool in [
        PREPROCESSOR,
        "mips-linux-gnu-as",
        "mips-linux-gnu-ld",
        "mips-linux-gnu-nm",
        "mips-linux-gnu-objcopy",
        "python3",
    ] {
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

/// Where expanded units say they come from: the same wherever they were expanded, so that a unit is known by what it says.
const TREE: &str = "tree";

/// What a package is compiled by: its C units expanded, each compiled by what that came to, its assembly assembled, and
/// those with its prebuilt objects.
fn split_build(call: &mut Call) -> Result<Prepared> {
    let target = call.config().and_then(|config| config.target);
    if target.as_ref().is_some_and(|target| target != TARGET) {
        bail!("the config builds for {}, and this is {TARGET}", target.unwrap_or_default());
    }
    let sources = call.take::<SourceTree>(0)?;
    let units = sources.files.iter().find(|file| file.name == "units").ok_or_else(|| anyhow!("it names no units"))?;
    let units = String::from_utf8(call.read(units)?)?;
    let name =
        sources.files.iter().find(|file| file.name == "package").ok_or_else(|| anyhow!("it names no package"))?;
    let name = Value::String(String::from_utf8(call.read(name)?)?);
    let mut micros = vec![Micro {
        op: "expand".into(),
        label: "expand".into(),
        ports: vec![vec![MicroInput::port(0)]],
        ..Micro::default()
    }];
    let mut objects = vec![];
    // The objects what links the package takes: its public units'.
    let mut public = vec![];
    for (visibility, unit) in units_of(&units) {
        let unit = unit.as_str();
        if visibility == "public" {
            public.push(Value::String(object_name(name.as_str().unwrap_or_default(), unit)));
        }
        objects.push(MicroInput::micro(micros.len()));
        if is_assembly(unit) {
            micros.push(Micro {
                op: "assemble".into(),
                label: unit.to_string(),
                settings: Table::from_iter([
                    ("unit".to_string(), Value::String(unit.to_string())),
                    ("package".to_string(), name.clone()),
                ]),
                ports: vec![vec![MicroInput::port(0).only(&[unit])]],
                keep: true,
            });
            continue;
        }
        micros.push(Micro {
            op: "object".into(),
            label: unit.to_string(),
            settings: Table::from_iter([
                ("unit".to_string(), Value::String(unit.to_string())),
                ("package".to_string(), name.clone()),
            ]),
            ports: vec![
                vec![MicroInput::micro(0).only(&[&expanded_name(unit)])],
                vec![MicroInput::port(0).only(&["dep/core/"])],
            ],
            keep: true,
        });
    }
    let prebuilt = format!("target/{TARGET}/");
    let settings = Table::from_iter([("package".to_string(), name), ("public".to_string(), Value::Array(public))]);
    micros.push(Micro {
        op: "collect".into(),
        label: "objects".into(),
        settings,
        ports: vec![objects, vec![MicroInput::port(0).only(&[&prebuilt])]],
        ..Micro::default()
    });
    Ok(Prepared { needs: Vec::new(), micros })
}

/// The units a gathered tree lists, each with its visibility (`public self/lib/libsn/snmain.s`).
fn units_of(listed: &str) -> Vec<(String, String)> {
    listed
        .lines()
        .filter_map(|line| line.split_once(' '))
        .map(|(visibility, unit)| (visibility.to_string(), unit.to_string()))
        .collect()
}

/// Whether a unit is assembly, assembled as it is written.
fn is_assembly(unit: &str) -> bool {
    Path::new(unit).extension().is_some_and(|extension| extension == "s")
}

/// What a unit's object is named: its package and its place in it (`title/src/package.o`), as a link script names it.
fn object_name(package: &str, unit: &str) -> String {
    let inside = unit.strip_prefix("self/").unwrap_or(unit);
    format!("{package}/{}", Path::new(inside).with_extension("o").display())
}

/// A package's name, as the gathered tree says it (`package`), handed to each step of its build.
fn package_name(call: &Call) -> Result<String> {
    call.settings
        .get("package")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("it is told no package's name"))
}

/// A package's name, in the gathered tree written out.
fn name_in(tree: &Path) -> Result<String> {
    fs::read_to_string(tree.join("package")).map_err(|_| anyhow!("the tree names no package"))
}

/// The file an image is written to: its package's name as the release names its files, the path's steps joined by `_` (`aya/mist`:
/// `aya_mist`).
fn file_name(name: &str) -> String {
    name.replace('/', "_")
}

fn expanded_name(unit: &str) -> String {
    format!("expanded/{unit}.i")
}

/// Every unit run through the preprocessor, the place it was done at taken out of what it says.
fn expand(call: &mut Call) -> Result<Typed> {
    let path = call.host.path().to_string();
    let sources = call.take::<SourceTree>(0)?;
    let scratch = PathBuf::from(format!("{}.expand", call.temp()?));
    fs::create_dir_all(&scratch)?;
    let made = write_out(call, &sources, &scratch).and_then(|tree| {
        let mut made = vec![];
        for (_, unit) in
            units_of(&fs::read_to_string(tree.join("units"))?).iter().filter(|(_, unit)| !is_assembly(unit))
        {
            let expanded = scratch.join("expanded.i");
            let mut args: Vec<String> = CPP.iter().map(ToString::to_string).collect();
            args.extend(searched(&tree));
            args.extend([format!("-I{}", text(&tree)), "-o".to_string(), text(&expanded), text(&tree.join(unit))]);
            run(PREPROCESSOR, &args)?;
            let said = replaced(&fs::read(&expanded)?, text(&tree).as_bytes(), TREE.as_bytes());
            made.push(File::new(&expanded_name(unit), said));
        }
        Ok(made)
    });
    let _ = fs::remove_dir_all(&scratch);
    Ok(Typed::new(SourceTree::NAME, in_project(made, &scratch, &path)?))
}

/// These bytes with one run of them put for another.
fn replaced(bytes: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at..].starts_with(from) {
            out.extend_from_slice(to);
            at += from.len();
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    out
}

/// A unit compiled, from what it came to expanded and what the assembler includes.
fn object(call: &mut Call) -> Result<Typed> {
    let path = call.host.path().to_string();
    let unit =
        call.settings.get("unit").and_then(Value::as_str).ok_or_else(|| anyhow!("it names no unit"))?.to_string();
    let expanded = call.take::<SourceTree>(0)?;
    let core = call.take::<SourceTree>(1)?;
    let scratch = PathBuf::from(format!("{}.object", call.temp()?));
    fs::create_dir_all(&scratch)?;
    let made = write_out(call, &core, &scratch).and_then(|tree| {
        let source = expanded.files.first().ok_or_else(|| anyhow!("{unit} was not expanded"))?;
        let at = scratch.join("unit.i");
        fs::write(&at, call.read(source)?)?;
        compile(&at, &tree, &scratch)
    });
    let kept = made.and_then(|object| call.keep(&text(&object), &object_name(&package_name(call)?, &unit)));
    let _ = fs::remove_dir_all(&scratch);
    Ok(Typed::new(Object::NAME, vec![said_of(kept, &path)?]))
}

/// What a tool said of a unit expanded, said of the files it came from.
fn said_of<T>(result: Result<T>, path: &str) -> Result<T> {
    let tree = format!("{TREE}/");
    result.map_err(|e| {
        anyhow!(
            "{}",
            format!("{e:#}")
                .replace(&format!("{tree}self/"), &format!("{path}/"))
                .replace(&format!("{tree}dep/"), "packages/")
                .replace(&tree, "")
        )
    })
}

/// A unit of assembly assembled, as it is written: no preprocessor, no maspsx.
fn assemble(call: &mut Call) -> Result<Typed> {
    let path = call.host.path().to_string();
    let unit =
        call.settings.get("unit").and_then(Value::as_str).ok_or_else(|| anyhow!("it names no unit"))?.to_string();
    let sources = call.take::<SourceTree>(0)?;
    let scratch = PathBuf::from(format!("{}.assemble", call.temp()?));
    fs::create_dir_all(&scratch)?;
    let made = write_out(call, &sources, &scratch).and_then(|tree| {
        let object = scratch.join("unit.o");
        let mut args: Vec<String> = ASSEMBLER.iter().map(ToString::to_string).collect();
        args.extend(["-o".to_string(), text(&object), text(&tree.join(&unit))]);
        run("mips-linux-gnu-as", &args)?;
        call.keep(&text(&object), &object_name(&package_name(call)?, &unit))
    });
    let _ = fs::remove_dir_all(&scratch);
    Ok(Typed::new(Object::NAME, vec![in_project(made, &scratch, &path)?]))
}

/// The package's objects: its units', and its prebuilt ones (`target/psx/<package>/objects/<visibility>/<place>`) by their place in
/// it - all of them, for its own image, and which are public (`public`), for what links it.
fn collect(call: &mut Call) -> Result<Typed> {
    let name = package_name(call)?;
    let mut public: Vec<Value> = call.settings.get("public").and_then(Value::as_array).cloned().unwrap_or_default();
    let compiled = call.take_all::<ty::File<Object>>(0)?;
    let prebuilt = call.take::<SourceTree>(1)?;
    let mut files = vec![];
    for object in &compiled {
        files.push(object.file()?.clone());
    }
    for visibility in ["private", "public"] {
        let own = format!("target/{TARGET}/{name}/objects/{visibility}/");
        for file in prebuilt.files.iter().filter(|file| file.name.starts_with(&own)) {
            let mut file = file.clone();
            file.name = format!("{name}/{}", &file.name[own.len()..]);
            if visibility == "public" {
                public.push(Value::String(file.name.clone()));
            }
            files.push(file);
        }
    }
    let settings = Table::from_iter([("public".to_string(), Value::Array(public))]);
    Ok(Typed { settings, ..Typed::new(Objects::NAME, files) })
}

/// Objects on disk under `objects/`, each by its name, as a link script names them.
fn write_objects(call: &Call, objects: &[File], scratch: &Path) -> Result<Vec<PathBuf>> {
    let mut written = vec![];
    for file in objects {
        let at = scratch.join("objects").join(&file.name);
        if let Some(parent) = at.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&at, call.read(file)?)?;
        written.push(at);
    }
    Ok(written)
}

/// What the gathered tree says the image links besides its own objects (`linked`): its dependencies, and the packages it
/// binds, by their paths.
fn linked(tree: &Path, kind: &str) -> Vec<String> {
    let listed = fs::read_to_string(tree.join("linked")).unwrap_or_default();
    listed.lines().filter_map(|line| line.strip_prefix(kind)?.strip_prefix(' ')).map(str::to_string).collect()
}

/// The package's own link script, from the gathered tree.
fn script_of(tree: &Path, name: &str) -> Result<PathBuf> {
    only_file(&tree.join("target").join(TARGET).join(name).join("link_script"))
        .map_err(|_| anyhow!("{name} has no link_script"))
}

/// What an image exports: its layout alone, its references left open - a jump to 0 does not reach, which ld says and goes
/// on from, the addresses being what this is for - and of its names those its sections hold.
fn symbols(call: &mut Call) -> Result<Typed> {
    let path = call.host.path().to_string();
    let sources = call.take::<SourceTree>(0)?;
    let objects = call.take::<Objects>(1)?;
    let scratch = PathBuf::from(format!("{}.symbols", call.temp()?));
    fs::create_dir_all(&scratch)?;
    let made = write_out(call, &sources, &scratch).and_then(|tree| {
        let name = name_in(&tree)?;
        let objects = write_objects(call, &objects.files, &scratch)?;
        let layout = scratch.join("layout.elf");
        let mut args: Vec<String> =
            ["-EL", "-nostdlib", "--no-check-sections", "--unresolved-symbols=ignore-all", "--noinhibit-exec"]
                .map(ToString::to_string)
                .to_vec();
        args.extend(["-T".to_string(), text(&script_of(&tree, &name)?), "-o".to_string(), text(&layout)]);
        args.extend(objects.iter().map(|object| text(object)));
        output("mips-linux-gnu-ld", &args)?;
        let listed = output("mips-linux-gnu-nm", &["-g".to_string(), "--defined-only".to_string(), text(&layout)])?;
        let mut published = String::new();
        for line in String::from_utf8_lossy(&listed).lines() {
            // What its sections hold: an absolute name is its link's (SLOT, a script's PROVIDE).
            if let [address, kind, symbol] = line.split_whitespace().collect::<Vec<_>>().as_slice()
                && *kind != "A"
            {
                let _ = writeln!(published, "PROVIDE({symbol} = 0x{address});");
            }
        }
        let at = scratch.join("symbols.ld");
        fs::write(&at, published)?;
        call.keep(&text(&at), "symbols.ld")
    });
    let _ = fs::remove_dir_all(&scratch);
    Ok(Typed::new(Symbols::NAME, vec![in_project(made, &scratch, &path)?]))
}

/// The image: the package's objects and its dependencies', by its script at its slot, against what the libraries publish
/// and what the images it binds do.
fn link_image(call: &mut Call) -> Result<Typed> {
    let path = call.host.path().to_string();
    let fields = call.given();
    let sources = call.take::<SourceTree>(0)?;
    let notes = sources.notes.clone();
    let own = call.take::<Objects>(1)?;
    let scratch = PathBuf::from(format!("{}.link", call.temp()?));
    fs::create_dir_all(&scratch)?;
    let made = write_out(call, &sources, &scratch).and_then(|tree| {
        let name = name_in(&tree)?;
        let mut files = own.files.clone();
        for dependency in linked(&tree, "dependency") {
            let pe2_ext::Data::Typed(objects) = call.output_of(&dependency, "objects")? else {
                bail!("{dependency}: its objects are no files")
            };
            // What it gives what links it: its public objects.
            let public: Vec<&str> = objects
                .settings
                .get("public")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            files.extend(objects.files.into_iter().filter(|file| public.contains(&file.name.as_str())));
        }
        let objects = write_objects(call, &files, &scratch)?;
        let mut published = vec![];
        if let Ok(packages) = fs::read_dir(tree.join("target").join(TARGET)) {
            for package in packages {
                if let Ok(file) = only_file(&package?.path().join("symbols")) {
                    published.push(file);
                }
            }
        }
        for (at, bound) in linked(&tree, "bound").iter().enumerate() {
            let pe2_ext::Data::Typed(symbols) = call.output_of(bound, "symbols")? else {
                bail!("{bound}: what it publishes is no file")
            };
            let written = scratch.join(format!("bound_{at}.ld"));
            fs::write(&written, call.read(symbols.file()?)?)?;
            published.push(written);
        }
        let image = link(&script_of(&tree, &name)?, &published, &objects, &scratch, &name)?;
        if pe2_ext::targeted(&fields, Some(TARGET)).get("executable").and_then(Value::as_bool) != Some(true) {
            return Ok((image, format!("{}.pe2pkg", file_name(&name))));
        }
        let release = call.config().and_then(|config| config.release).ok_or_else(|| anyhow!("no release is built"))?;
        let exe = scratch.join(format!("{}.exe", file_name(&name)));
        let elf = fs::read(scratch.join("image.elf"))?;
        fs::write(&exe, exe_header(&fs::read(&image)?, &elf, load_address(&elf)?, &release)?)?;
        Ok((exe, format!("{}.exe", file_name(&name))))
    });
    let result = made.and_then(|(image, kept_as)| {
        let kept = call.keep(&image.display().to_string(), &kept_as)?;
        Ok(Typed { notes: notes.clone(), ..Typed::new(Package::NAME, vec![kept]) })
    });
    let _ = fs::remove_dir_all(&scratch);
    // What a tool said of the tree it was given is said of the files they are.
    in_project(result, &scratch, &path)
}

/// What a tool said of the tree it was given, said of the files they are.
fn in_project<T>(result: Result<T>, scratch: &Path, path: &str) -> Result<T> {
    let tree = scratch.join("tree").display().to_string();
    result.map_err(|e| {
        anyhow!(
            "{}",
            format!("{e:#}")
                .replace(&format!("{tree}/self/"), &format!("{path}/"))
                .replace(&format!("{tree}/dep/"), "packages/")
                .replace(&format!("{tree}/"), "")
        )
    })
}

/// What the units come to with their includes and the options expanded and the other `PE2_*` macros left as they are written, for
/// `pe2.packages` to read: each unit beside what was gathered, under `first/`.
fn first_pass(call: &mut Call) -> Result<Typed> {
    let mut sources = call.take::<SourceTree>(0)?;
    let scratch = PathBuf::from(format!("{}.first", call.temp()?));
    fs::create_dir_all(&scratch)?;
    let made = write_out(call, &sources, &scratch).and_then(|tree| {
        let mut made = vec![];
        for (_, unit) in
            units_of(&fs::read_to_string(tree.join("units"))?).iter().filter(|(_, unit)| !is_assembly(unit))
        {
            let expanded = scratch.join("expanded.i");
            let mut args: Vec<String> = CPP.iter().map(ToString::to_string).collect();
            args.extend(searched(&tree));
            args.extend([
                "-P".to_string(),
                "-DPE2_FIRST_PASS".to_string(),
                format!("-I{}", text(&tree)),
                "-o".to_string(),
                text(&expanded),
                text(&tree.join(unit)),
            ]);
            run(PREPROCESSOR, &args)?;
            made.push(File::new(&format!("first/{unit}"), fs::read(&expanded)?));
        }
        Ok(made)
    });
    let _ = fs::remove_dir_all(&scratch);
    let path = call.host.path().to_string();
    let unset = sources
        .settings
        .get("unset")
        .and_then(Value::as_array)
        .map_or_else(Vec::new, |names| names.iter().filter_map(Value::as_str).collect());
    sources.files.extend(in_project(made, &scratch, &path).map_err(|e| no_value(e, &unset))?);
    Ok(sources)
}

/// A failure of an option with no value, said of the line asking for it: the compiler names the header defining it.
fn no_value(error: pe2_ext::Error, unset: &[&str]) -> pe2_ext::Error {
    let said = format!("{error:#}");
    let Some(name) = unset.iter().find(|name| said.contains(&format!("pe2_unset_{name}"))) else { return error };
    let asked = said
        .lines()
        .filter_map(|line| line.split_once(": note: in expansion of macro"))
        .find(|(_, by)| by.contains("PE2_OPTION"));
    let at = asked.and_then(|(place, _)| place.rsplit_once(':')).map(|(line, _)| line);
    match at {
        Some(at) => anyhow!("{at}: {name} has no value on this package"),
        None => error.context(format!("{name} has no value on this package")),
    }
}

/// The folders a gathered tree's includes are found in, as the preprocessor is told them, in their order: a package's own for what
/// it includes in quotes (`-iquote`), its dependencies' for what in angle brackets.
fn searched(tree: &Path) -> Vec<String> {
    let listed = fs::read_to_string(tree.join("includes")).unwrap_or_default();
    let mut args = vec![];
    for line in listed.lines() {
        match line.split_once(' ') {
            Some(("quote", dir)) => args.extend(["-iquote".to_string(), text(&tree.join(dir))]),
            Some(("angle", dir)) => args.push(format!("-I{}", text(&tree.join(dir)))),
            _ => {}
        }
    }
    args
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

/// The one file a property of a package is in.
fn only_file(folder: &Path) -> Result<PathBuf> {
    let found = fs::read_dir(folder)?.next().ok_or_else(|| anyhow!("{} is empty", folder.display()))??;
    Ok(found.path())
}

fn text(path: &Path) -> String {
    path.display().to_string()
}

/// A program, run to its end, and what it wrote on its standard output.
fn output(tool: &str, args: &[String]) -> Result<Vec<u8>> {
    let mut process = spawn(tool, args)?;
    process.close_input();
    let mut out = vec![];
    loop {
        let read = process.read(1 << 16)?;
        if read.is_empty() {
            break;
        }
        out.extend(read);
    }
    process.wait()?;
    Ok(out)
}

/// A program, run to its end. maspsx reads its standard input before its file: it gets none.
fn run(tool: &str, args: &[String]) -> Result<()> {
    let mut process = spawn(tool, args)?;
    process.close_input();
    process.wait()
}

/// A unit expanded to its object: the compiler on what it comes to, then the assembler, which includes from `dep/core`.
fn compile(expanded: &Path, tree: &Path, scratch: &Path) -> Result<PathBuf> {
    let (assembly, object) = (scratch.join("unit.s"), scratch.join("unit.o"));
    let mut args: Vec<String> = CC1.iter().map(ToString::to_string).collect();
    args.extend(["-o".to_string(), text(&assembly), text(expanded)]);
    run("cc1", &args)?;
    let mut args: Vec<String> = MASPSX.iter().map(ToString::to_string).collect();
    args.extend([
        "-no-pad-sections".to_string(),
        format!("-I{}", text(&tree.join("dep/core"))),
        "-o".to_string(),
        text(&object),
        text(&assembly),
    ]);
    run("maspsx", &args)?;
    Ok(object)
}

/// The image: the package's script for where things land, what its dependencies publish for the rest.
fn link(script: &Path, published: &[PathBuf], objects: &[PathBuf], scratch: &Path, name: &str) -> Result<PathBuf> {
    let linked = scratch.join("image.elf");
    let mut args: Vec<String> = ["-EL", "-nostdlib", "--no-check-sections"].map(ToString::to_string).to_vec();
    args.extend(["-T".to_string(), text(script)]);
    for file in published {
        args.extend(["-T".to_string(), text(file)]);
    }
    args.extend(["-o".to_string(), text(&linked)]);
    args.extend(objects.iter().map(|object| text(object)));
    run("mips-linux-gnu-ld", &args)?;
    let image = scratch.join(format!("{}.pe2pkg", file_name(name)));
    run(
        "mips-linux-gnu-objcopy",
        &["-O".into(), "binary".into(), "-j".into(), ".image".into(), text(&linked), text(&image)],
    )?;
    Ok(image)
}

#[cfg(test)]
mod tests {
    use pe2_ext::anyhow;

    #[test]
    fn an_executable_is_headed_as_the_bios_reads_it() {
        let mut elf = b"\x7fELF\x01\x01".to_vec();
        elf.resize(0x34, 0);
        elf[0x18..0x1C].copy_from_slice(&0x8001_4358u32.to_le_bytes());
        let made = super::exe_header(&[1, 2, 3], &elf, 0x8001_0000, "USA").unwrap();
        let word = |at: usize| u32::from_le_bytes(made[at..at + 4].try_into().unwrap());
        assert_eq!(&made[..8], b"PS-X EXE");
        assert_eq!((word(0x10), word(0x18), word(0x1C), word(0x30)), (0x8001_4358, 0x8001_0000, 0x800, 0x801F_FFF0));
        assert!(made[0x4C..].starts_with(b"Sony Computer Entertainment Inc. for North America area\0"));
        assert_eq!((made.len(), &made[0x800..0x803]), (0x1000, &[1, 2, 3][..]));
    }

    #[test]
    fn an_option_with_no_value_is_told_at_the_line_asking() {
        let said = "exit status: 1: In file included from pe2.h:29,\npe2_options.h:1:26: error: division by zero in #if\n\
                    1 | #define pe2_option_foo (0/pe2_unset_foo)\n\
                    packages/title/src/package.c:4:5: note: in expansion of macro \u{2018}PE2_OPTION\u{2019}\n";
        assert_eq!(
            super::no_value(anyhow!("{said}"), &["bar", "foo"]).to_string(),
            "packages/title/src/package.c:4: foo has no value on this package"
        );
        assert_eq!(super::no_value(anyhow!("other"), &["foo"]).to_string(), "other");
    }
}
