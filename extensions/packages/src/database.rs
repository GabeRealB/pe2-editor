//! What a language server is told, as nodes of the graph: `compile_commands.json` for every package that compiles, and the overlay that gives `PE2_PACKAGE`'s paths,
//! which are on no disk, the files they stand for, so that going to a definition lands in the real file. Both read each package's `reflection`, and what the target says it needs.

use pe2_ext::{Call, File, Result, Typed, Value, anyhow};
use serde_json::json;

use crate::gather::{Package, packages};

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
        for place in package.public_folders() {
            if let Some(disk) = call.place(&format!("{}/{place}", package.path)) {
                roots.push(json!({ "type": "directory-remap", "name": format!("{root}/pkg/{name}"), "external-contents": disk }));
            }
        }
    }
    file("overlay.yaml", &json!({ "version": 0, "case-sensitive": "true", "fallthrough": "true", "roots": roots }))
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
    let core = packages.iter().find(|(name, _)| name == "core").and_then(|(_, package)| call.place(&package.path));
    args.extend(core.map(|core| format!("-I{core}")));
    args.extend([format!("-I{folder}/virtual"), "-ivfsoverlay".to_string(), format!("{folder}/overlay.yaml")]);
    let mut commands = vec![];
    for (_, package) in &packages {
        let Ok(reflected) = call.table_of(&package.path, "reflection") else { continue };
        let sources = reflected.get("sources").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
        for source in sources.iter().filter_map(Value::as_str) {
            let Some(place) = call.place(&format!("{}/{source}", package.path)) else { continue };
            let mut arguments = args.clone();
            arguments.extend(["-c".to_string(), place.clone()]);
            commands.push(json!({ "directory": folder, "file": place, "arguments": arguments }));
        }
    }
    file("compile_commands.json", &serde_json::Value::Array(commands))
}
