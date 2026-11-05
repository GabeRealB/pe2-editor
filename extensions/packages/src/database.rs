//! What a language server is told, as nodes of the graph: `compile_commands.json` for every package that compiles, and the overlay that gives `PE2_PACKAGE`'s paths,
//! which are on no disk, the files they stand for, so that going to a definition lands in the real file. Both read each package's `reflection`, and what the target says it needs.

use std::fmt::Write as _;

use pe2_ext::{Call, Data, File, Need, Prepared, Result, Typed, Value, anyhow};
use serde_json::json;

use crate::gather::{Package, packages};

/// The name of a package as a macro's: its path's steps joined by `_`.
fn define_of(name: &str) -> String {
    name.replace('/', "_")
}

fn file(name: &str, value: &serde_json::Value) -> Result<Typed> {
    let bytes = serde_json::to_vec_pretty(value)?;
    Ok(Typed::new(pe2_ext::data::BYTES, vec![File::new(name, bytes)]))
}

fn folder(call: &Call) -> Result<String> {
    Ok(call.config().ok_or_else(|| anyhow!("no config is being built"))?.folder)
}

fn sorted(call: &Call) -> Result<Vec<(String, Package)>> {
    let mut packages: Vec<(String, Package)> = packages(call)?.into_iter().collect();
    packages.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(packages)
}

pub fn overlay(call: &mut Call) -> Result<Typed> {
    let root = format!("{}/virtual", folder(call)?);
    let mut roots = vec![];
    for (name, package) in sorted(call)? {
        if let Some(disk) = package.public_folder().and_then(|folder| call.place(&folder)) {
            roots.push(json!({ "type": "directory-remap", "name": format!("{root}/{name}"), "external-contents": disk }));
        }
    }
    file("overlay.yaml", &json!({ "version": 0, "case-sensitive": "true", "fallthrough": "true", "roots": roots }))
}

/// What `server_values` reads: what every package that compiles resolved to.
pub fn server_values_reads(call: &mut Call) -> Result<Prepared> {
    let compiling = sorted(call)?.into_iter().filter(|(_, package)| package.compiles());
    Ok(Prepared { needs: compiling.map(|(_, package)| Need::new(&package.path, "resolved")).collect(), micros: Vec::new() })
}

/// What `database` reads: of every package, what its target tells a language server and what it asks for.
pub fn database_reads(call: &mut Call) -> Result<Prepared> {
    let mut needs = Vec::new();
    for (_, package) in sorted(call)? {
        needs.push(Need::new(&package.path, "flags"));
        needs.push(Need::new(&package.path, "reflection"));
    }
    Ok(Prepared { needs, micros: Vec::new() })
}

/// What a language server reads for `PE2_VALUE`: every package's values, each behind its own define, as one server reads them all.
pub fn server_values(call: &mut Call) -> Result<Typed> {
    let mut text = String::new();
    for (name, package) in sorted(call)? {
        if !package.compiles() {
            continue;
        }
        // A package that cannot be read fails this too: leaving it out would be kept, and read again by nothing once it is mended.
        let Data::Typed(tree) = call.output_of(&package.path, "resolved")? else { continue };
        let _ = writeln!(text, "#ifdef PE2_PACKAGE_{}", define_of(&name));
        for file in tree.files.iter().filter(|file| [crate::gather::OPTIONS, crate::gather::VALUES].contains(&file.name.as_str())) {
            text.push_str(&String::from_utf8_lossy(&call.read(file)?));
        }
        let _ = writeln!(text, "#endif");
    }
    Ok(Typed::new(pe2_ext::data::BYTES, vec![File::new("pe2_server.h", text.into_bytes())]))
}

pub fn database(call: &mut Call) -> Result<Typed> {
    let folder = folder(call)?;
    let packages = sorted(call)?;
    let flags = packages.iter().find_map(|(_, package)| call.table_of(&package.path, "flags").ok());
    let Some(flags) = flags else { return Err(anyhow!("no target says what a language server needs: is one enabled?")) };
    let triple = flags.get("triple").and_then(Value::as_str).unwrap_or_default();
    let mut args = vec!["clang".to_string(), "-x".into(), "c".into(), "-target".into(), triple.to_string(), "-nostdinc".into()];
    let defines = flags.get("defines").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    args.extend(defines.iter().filter_map(Value::as_str).map(|define| format!("-D{define}")));
    args.push("-DPE2_LANGUAGE_SERVER".to_string());
    args.extend([format!("-I{folder}/virtual"), "-ivfsoverlay".to_string(), format!("{folder}/overlay.yaml")]);
    let mut commands = vec![];
    for (name, package) in &packages {
        let Ok(reflected) = call.table_of(&package.path, "reflection") else { continue };
        // What it includes in quotes is in its own folders; in angle brackets, in the virtual folder, every package under its name.
        let own = package.header_folders().into_iter().map(|folder| format!("{}/{folder}", package.path));
        let path: Vec<String> = own.filter_map(|folder| call.place(&folder)).flat_map(|place| ["-iquote".to_string(), place]).collect();
        let sources = reflected.get("sources").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
        // C, as a language server reads it: assembly is assembled as it is written, and is no C unit.
        for source in sources.iter().filter_map(Value::as_str).filter(|source| std::path::Path::new(source).extension().is_some_and(|extension| extension.eq_ignore_ascii_case("c"))) {
            let Some(place) = call.place(&format!("{}/{source}", package.path)) else { continue };
            let mut arguments = args.clone();
            let at = arguments.len() - 3;
            arguments.splice(at..at, path.iter().cloned());
            arguments.extend([format!("-DPE2_PACKAGE_{}", define_of(name)), "-c".to_string(), place.clone()]);
            commands.push(json!({ "directory": folder, "file": place, "arguments": arguments }));
        }
    }
    file("compile_commands.json", &serde_json::Value::Array(commands))
}
