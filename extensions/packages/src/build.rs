//! What a package's build is wired from, beside the `run` boxes that run its programs: none of these runs anything.
//!
//! A build - a graph a project defines, by hand or by a generator - takes what `gather` made of a package and wires:
//! `units`, the units of a language, each a tree of its own with it first (what a `run` box takes, one for each unit);
//! `with`, files made of the units put back in the tree (a first pass's, which `resolve` reads); `collect`, what was
//! compiled of the units with what the package links as it is, each by its name; `link_inputs`, what links it - its
//! objects and its dependencies' public ones, its script, what the libraries and the images it binds publish - laid out
//! for a linker; and `published`, a symbol listing (`nm`) as what binding it links against.
//!
//! What a target holds of a package is under `target/<target>/<package>/` of the tree, `<target>` the configuration's.

use std::fmt::Write as _;
use std::path::Path;

use pe2_ext::ty::{self, Files, Registered, SourceTree};
use pe2_ext::{Call, File, Registry, Result, Table, Typed, Value, anyhow, bail};

/// A package's objects: its units compiled and what it links as it is, each named by its package and its place in it;
/// which of them are public (`public`, for what links it).
pub struct Objects;

impl Registered for Objects {
    const NAME: &str = "pe2.packages@objects";
    const LABEL: &str = "Objects";
}

impl Files for Objects {}

/// What links a package, laid out for a linker: `objects/` (its own, then its dependencies' public ones), `own/` (its own
/// alone: what its layout is made of), `script/` (its link script), `published/` (what the libraries and the images it
/// binds publish).
pub struct LinkInputs;

impl Registered for LinkInputs {
    const NAME: &str = "pe2.packages@link";
    const LABEL: &str = "Link inputs";
}

impl Files for LinkInputs {}

/// What an image publishes, at the addresses its layout alone gives them: what the images binding its package link
/// against.
pub struct Symbols;

impl Registered for Symbols {
    const NAME: &str = "pe2.packages@symbols";
    const LABEL: &str = "Published symbols";
}

impl Files for Symbols {}

/// What a build is wired from, registered: its value types and its operations.
pub fn register(r: &mut Registry) {
    let _ = r
        .op("generate")
        .label("A build, generated")
        .input::<ty::Any>("description")
        .output::<ty::Table>()
        .run(super::generate::generate);
    let _ = r.value_type::<Objects>();
    let _ = r.value_type::<LinkInputs>();
    let _ = r.value_type::<Symbols>();
    let _ = r
        .op("units")
        .label("Units of a language")
        .input::<SourceTree>("sources")
        .output::<ty::List<SourceTree>>()
        .run(units);
    let _ = r
        .op("with")
        .label("Files put in the tree")
        .input::<SourceTree>("sources")
        .inputs::<ty::Any>("files")
        .output::<SourceTree>()
        .run(with);
    let _ = r
        .op("collect")
        .label("Objects")
        .input::<SourceTree>("sources")
        .inputs::<ty::Any>("compiled")
        .output::<Objects>()
        .run(objects);
    let _ = r
        .op("link_inputs")
        .label("What links it")
        .input::<SourceTree>("sources")
        .input::<Objects>("objects")
        .output::<LinkInputs>()
        .steps(link_inputs());
    let _ = r
        .op("published")
        .label("Published symbols")
        .input::<ty::Any>("listing")
        .output::<ty::File<Symbols>>()
        .run(published);
}

/// A file of the gathered tree, as text.
fn text_in(call: &Call, tree: &Typed, name: &str) -> Result<String> {
    let file = tree.files.iter().find(|file| file.name == name).ok_or_else(|| anyhow!("the tree has no {name}"))?;
    Ok(String::from_utf8_lossy(&call.read(file)?).into_owned())
}

/// The units a gathered tree lists, each with its visibility (`public self/lib/libsn/snmain.s`).
fn units_of(listed: &str) -> Vec<(String, String)> {
    listed
        .lines()
        .filter_map(|line| line.split_once(' '))
        .map(|(visibility, unit)| (visibility.to_string(), unit.to_string()))
        .collect()
}

/// What a unit's object is named: its package and its place in it (`title/src/package.o`), as a link script names it.
#[must_use]
pub fn object_name(package: &str, unit: &str) -> String {
    let inside = unit.strip_prefix("self/").unwrap_or(unit);
    format!("{package}/{}", Path::new(inside).with_extension("o").display())
}

/// The configuration's target: where what a target holds of a package is in its tree.
fn target(call: &Call) -> Result<String> {
    call.config().and_then(|config| config.target).ok_or_else(|| anyhow!("the configuration built names no target"))
}

/// `units`: the units of a language (`extension`: `c`, `s`), each the whole tree with it first, told its unit, its
/// object's name, its visibility, and the folders of the tree its includes are found in - those in quotes, those in
/// angle brackets (`unit`, `object`, `visibility`, `quote`, `angle`): what one `run` box a unit takes.
///
/// # Errors
/// The tree lists no units or names no package.
pub fn units(call: &mut Call) -> Result<Vec<Typed>> {
    let extension = call.settings.get("extension").and_then(Value::as_str).unwrap_or("c").trim_start_matches('.');
    let extension = extension.to_string();
    let tree = call.take::<SourceTree>(0)?;
    let package = text_in(call, &tree, "package")?;
    let includes = text_in(call, &tree, "includes").unwrap_or_default();
    let searched = |kind: &str| -> Value {
        let folders = includes.lines().filter_map(|line| line.strip_prefix(kind)?.strip_prefix(' '));
        Value::Array(folders.map(|folder| Value::String(folder.to_string())).collect())
    };
    let (quote, angle) = (searched("quote"), searched("angle"));
    let mut items = vec![];
    for (visibility, unit) in units_of(&text_in(call, &tree, "units")?) {
        if Path::new(&unit).extension().is_none_or(|e| e.to_string_lossy() != extension) {
            continue;
        }
        let own = tree.files.iter().find(|file| file.name == unit).ok_or_else(|| anyhow!("the tree has no {unit}"))?;
        let mut files = vec![own.clone()];
        files.extend(tree.files.iter().filter(|file| file.name != unit).cloned());
        let settings = Table::from_iter([
            ("unit".to_string(), Value::String(unit.clone())),
            ("object".to_string(), Value::String(object_name(&package, &unit))),
            ("visibility".to_string(), Value::String(visibility)),
            ("quote".to_string(), quote.clone()),
            ("angle".to_string(), angle.clone()),
        ]);
        items.push(Typed { settings, notes: tree.notes.clone(), ..Typed::new(SourceTree::NAME, files) });
    }
    Ok(items)
}

/// `with`: the tree, with the files of what else it takes put in it, each in place of one of its name.
///
/// # Errors
/// What it takes is no tree.
pub fn with(call: &mut Call) -> Result<Typed> {
    let mut tree = call.take::<SourceTree>(0)?;
    let added = typed_all(call.take_all::<ty::Any>(1)?);
    for file in added.into_iter().flat_map(|typed| typed.files) {
        tree.files.retain(|known| known.name != file.name);
        tree.files.push(file);
    }
    Ok(tree)
}

/// A value's typed form, where it has one.
fn typed(data: pe2_ext::Data) -> Option<Typed> {
    match data {
        pe2_ext::Data::Typed(typed) => Some(typed),
        _ => None,
    }
}

/// What values are made of that is typed: each typed one, each of a list's (what a for-each makes) in its order.
fn typed_all(data: Vec<pe2_ext::Data>) -> Vec<Typed> {
    data.into_iter()
        .flat_map(|data| match data {
            pe2_ext::Data::List(items) => typed_all(items),
            pe2_ext::Data::Typed(typed) => vec![typed],
            _ => vec![],
        })
        .collect()
}

/// `collect`: what was compiled of the units, each named as its unit's object, with what the package links as it is
/// (`target/<target>/<package>/objects/<visibility>/<place>`) by its place in it; which are public - its public units',
/// its public prebuilt ones - for what links it.
///
/// # Errors
/// The tree names no package, or a compiled unit is no single file.
pub fn objects(call: &mut Call) -> Result<Typed> {
    let target = target(call)?;
    let tree = call.take::<SourceTree>(0)?;
    let compiled = typed_all(call.take_all::<ty::Any>(1)?);
    let package = text_in(call, &tree, "package")?;
    let units = units_of(&text_in(call, &tree, "units")?);
    let mut public: Vec<Value> = units
        .iter()
        .filter(|(visibility, _)| visibility == "public")
        .map(|(_, unit)| Value::String(object_name(&package, unit)))
        .collect();
    let mut files: Vec<File> = vec![];
    for made in compiled {
        let [file] = made.files.as_slice() else { bail!("a compiled unit is {} files, not one", made.files.len()) };
        files.push(file.clone());
    }
    for visibility in ["private", "public"] {
        let own = format!("target/{target}/{package}/objects/{visibility}/");
        for file in tree.files.iter().filter(|file| file.name.starts_with(&own)) {
            let mut file = file.clone();
            file.name = format!("{package}/{}", &file.name[own.len()..]);
            if visibility == "public" {
                public.push(Value::String(file.name.clone()));
            }
            files.push(file);
        }
    }
    let settings = Table::from_iter([("public".to_string(), Value::Array(public))]);
    Ok(Typed { settings, ..Typed::new(Objects::NAME, files) })
}

/// What the gathered tree says a package links besides its own objects (its file `linked`): its dependencies
/// (`dependency`), and the packages it binds (`bound`), by their paths.
fn linked_in(listed: &str, kind: &str) -> Vec<String> {
    listed.lines().filter_map(|line| line.strip_prefix(kind)?.strip_prefix(' ')).map(str::to_string).collect()
}

/// `link_inputs`: what links the package, laid out for a linker - `objects/<name>` (its own, then its dependencies'
/// public ones, in that order), `script/<file>` (its link script), `published/<n>.ld` (what each library of the tree
/// publishes, then each image it binds).
#[pe2_ext::steps]
pub fn link_inputs(call: &mut Call) -> Result<Typed> {
    let listed: String = {
        let tree = call.take::<SourceTree>(0)?;
        let file = tree.files.iter().find(|file| file.name == "linked");
        file.map(|file| call.read(file))
            .transpose()?
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default()
    };
    let dependencies: Vec<String> = linked_in(&listed, "dependency");
    let bound: Vec<String> = linked_in(&listed, "bound");
    #[request]
    let read: Vec<pe2_ext::Data> = call.outputs_of(
        dependencies.iter().map(|d| (d.as_str(), "objects")).chain(bound.iter().map(|b| (b.as_str(), "symbols"))),
    )?;
    let (objects_read, symbols_read) = read.split_at(dependencies.len());
    let target = target(call)?;
    let tree = call.take::<SourceTree>(0)?;
    let own = call.take::<Objects>(1)?;
    let package = text_in(call, &tree, "package")?;
    let mut files: Vec<File> = vec![];
    let mut placed = |mut file: File, at: String| {
        file.name = at;
        files.push(file);
    };
    for file in own.files.clone() {
        placed(file.clone(), format!("own/{}", file.name));
        let at = format!("objects/{}", file.name);
        placed(file, at);
    }
    for (dependency, objects) in dependencies.iter().zip(objects_read) {
        let pe2_ext::Data::Typed(objects) = objects.clone() else { bail!("{dependency}: its objects are no files") };
        let public: Vec<String> = objects
            .settings
            .get("public")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        for file in objects.files.into_iter().filter(|file| public.contains(&file.name)) {
            let at = format!("objects/{}", file.name);
            placed(file, at);
        }
    }
    let script = format!("target/{target}/{package}/link_script/");
    let script = tree.files.iter().find(|file| file.name.starts_with(&script));
    let script = script.ok_or_else(|| anyhow!("{package} has no link_script for {target}"))?;
    let name = script.name.rsplit('/').next().unwrap_or("link.ld").to_string();
    placed(script.clone(), format!("script/{name}"));
    // What each package of the tree publishes as it is: its one file under `symbols/`.
    let mut publishing = std::collections::BTreeMap::new();
    let prefix = format!("target/{target}/");
    for file in &tree.files {
        let Some((owner, rest)) = file.name.strip_prefix(&prefix).and_then(|rest| rest.split_once('/')) else {
            continue;
        };
        if rest.starts_with("symbols/") {
            publishing.entry(owner.to_string()).or_insert_with(|| file.clone());
        }
    }
    let mut at = 0;
    for file in publishing.into_values() {
        placed(file, format!("published/{at:03}.ld"));
        at += 1;
    }
    for (bound, symbols) in bound.iter().zip(symbols_read) {
        let pe2_ext::Data::Typed(symbols) = symbols else { bail!("{bound}: what it publishes is no file") };
        placed(symbols.file()?.clone(), format!("published/{at:03}.ld"));
        at += 1;
    }
    Ok(Typed { notes: tree.notes.clone(), ..Typed::new(LinkInputs::NAME, files) })
}

/// `published`: a listing of an image's symbols (`nm -g --defined-only`), as what binding it links against - each of
/// what its sections hold (`PROVIDE(name = 0xaddress);`); an absolute name is its link's (a slot, a script's `PROVIDE`).
///
/// # Errors
/// What it takes is no single file.
pub fn published(call: &mut Call) -> Result<Typed> {
    let listing = call.take::<ty::Any>(0)?;
    let Some(listing) = typed(listing) else { bail!("it is given no listing") };
    let listed = call.read(listing.file()?)?;
    let mut published = String::new();
    for line in String::from_utf8_lossy(&listed).lines() {
        if let [address, kind, symbol] = line.split_whitespace().collect::<Vec<_>>().as_slice()
            && *kind != "A"
        {
            let _ = writeln!(published, "PROVIDE({symbol} = 0x{address});");
        }
    }
    Ok(Typed::new(Symbols::NAME, vec![File::new("symbols.ld", published.into_bytes())]))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_units_object_is_named_by_its_package_and_place() {
        assert_eq!(super::object_name("title", "self/src/package.c"), "title/src/package.o");
        assert_eq!(super::object_name("psyq", "self/lib/libsn/snmain.s"), "psyq/lib/libsn/snmain.o");
        assert_eq!(super::linked_in("dependency packages/a\nbound packages/b\n", "bound"), ["packages/b"]);
    }
}
