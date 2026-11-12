//! pe2.flags: the game flag bank's flags by name. The game keeps 504 four-bit values, read and written by ID; a flag
//! registry names some of them, each with what it is and what its values mean, so that whatever reads a flag - a
//! dialogue's command, an event, a package - names it.
//!
//! A registry is a data node, written by hand, as many as are convenient (the game's own mechanisms, a stage's): its
//! `flag` table holds each flag by name - its `id`, its `kind` (`marker`, `state`, `counter`, `bits`, `packed` two IDs
//! read as one byte, `reserved` a run the bank keeps something else in), its `values` or `bits` by name, a `count` of
//! consecutive IDs where it is a run of them, its `description`, and whether it is `verified` against the code that
//! reads and writes it. An ID has one owner across every registry of the project: its `ids` output fails, naming both,
//! where one overlaps another's, or passes the bank's end.

use std::collections::BTreeMap;

use pe2_ext::{Asset, Call, Field, Icon, Registry, Result, Table, Tint, Value, anyhow, bail, ty};

mod view;

pe2_ext::extension!(register);

/// The bank's size: IDs are 0..`BANK`.
pub const BANK: i64 = 504;

const STACK: &str = r#"[[stack]]
id = "registry"
op = "pe2.flags@flag_registry"

[[stack]]
op = "pe2.flags@ids"
registry = { from = "registry" }

[[stack]]
op = "output"
name = "ids"
"#;

/// A flag of a registry: its name and the IDs it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Flag {
    pub name: String,
    pub first: i64,
    pub count: i64,
}

/// The flags a registry's settings hold, each with its IDs: a `packed` flag two, a run its `count`.
///
/// # Errors
/// A flag has no ID.
pub fn flags_of(settings: &Table) -> Result<Vec<Flag>> {
    let flags = settings.get("flag").and_then(Value::as_table).into_iter().flatten();
    flags
        .map(|(name, flag)| {
            let id = flag.get("id").and_then(Value::as_integer).ok_or_else(|| anyhow!("flag {name}: no id"))?;
            let kind = flag.get("kind").and_then(Value::as_str).unwrap_or("marker");
            let count = flag.get("count").and_then(Value::as_integer).unwrap_or(if kind == "packed" { 2 } else { 1 });
            Ok(Flag { name: name.clone(), first: id, count })
        })
        .collect()
}

/// What overlaps in a registry and the others of the project, by IDs: each problem naming both owners.
#[must_use]
pub fn overlaps(own: &str, flags: &[Flag], others: &[(String, Vec<Flag>)]) -> Vec<String> {
    let mut owners: BTreeMap<i64, (String, String)> = BTreeMap::new();
    let mut problems = Vec::new();
    let all = std::iter::once((own.to_string(), flags.to_vec())).chain(others.iter().cloned());
    for (registry, flags) in all {
        for flag in &flags {
            for id in flag.first..flag.first + flag.count.max(1) {
                if !(0..BANK).contains(&id) {
                    if registry == own {
                        problems.push(format!("{}: ID {id:#05x} is past the bank's end ({BANK:#05x})", flag.name));
                    }
                    continue;
                }
                match owners.get(&id) {
                    Some((other, name)) if registry == own || *other == own => {
                        let (mine, (theirs, their)) =
                            if registry == own { (&flag.name, (other, name)) } else { (name, (&registry, &flag.name)) };
                        problems.push(format!("ID {id:#05x}: {mine} here, and {theirs}'s {their}"));
                    }
                    Some(_) => {}
                    None => {
                        owners.insert(id, (registry.clone(), flag.name.clone()));
                    }
                }
            }
        }
    }
    problems
}

/// A registry's IDs by flag, held to every registry of the project: no ID owned twice, none past the bank's end.
fn ids(call: &mut Call) -> Result<Table> {
    let given = call.given();
    let own = call.host.path().to_string();
    let flags = flags_of(&given)?;
    let mut others = Vec::new();
    for (path, declaration) in call.assets_of("pe2.flags@flag_registry")? {
        if path == own {
            continue;
        }
        let table: Table = declaration.parse()?;
        let layers = table.get("stack").and_then(Value::as_array).into_iter().flatten();
        let settings = layers.filter_map(Value::as_table).find(|layer| layer.contains_key("flag"));
        if let Some(settings) = settings {
            others.push((path, flags_of(settings)?));
        }
    }
    let problems = overlaps(&own, &flags, &others);
    if !problems.is_empty() {
        bail!("{}", problems.join("; "));
    }
    Ok(flags.into_iter().map(|f| (f.name, Value::Integer(f.first))).collect())
}

/// The flags of a registry, by name.
fn flag_names(asset: &Asset) -> Vec<(String, String)> {
    let flags = asset.field("flag").and_then(Value::as_table).into_iter().flatten();
    flags.map(|(name, _)| (name.clone(), name.clone())).collect()
}

#[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
fn register(r: &mut Registry) -> Result<()> {
    let _ = r
        .op("ids")
        .label("Its IDs, owned once")
        .takes("registry", &["pe2.flags@flag_registry"])
        .output::<ty::Table>()
        .run(ids);
    // A flag of a registry, as what reads it by name: its first ID.
    let _ = r
        .op("flag")
        .label("A flag's ID")
        .takes("registry", &["pe2.flags@flag_registry"])
        .output::<ty::Table>()
        .parameter::<String>("name")
        .members(flag_names)
        .run(|call| {
            let name: String = call.parameter()?;
            let given = call.given();
            let id = given.get("flag").and_then(|f| f.get(&name)).and_then(|f| f.get("id")).and_then(Value::as_integer);
            let id = id.ok_or_else(|| anyhow!("it has no flag {name}"))?;
            Ok(Table::from_iter([(pe2_ext::tokens::VALUE.to_string(), Value::Integer(id))]))
        });
    r.named_type("flag_registry")
        .alias("flag_registry")
        .label("Flag registry")
        .icon(Icon::Grid, Tint::Code)
        .data()
        .stack(STACK)
        .view("Flags", |_| view::Flags::of_registry())
        .field(Field::new("description").label("Description"))
        .field(Field::new("flag").label("Flags"))
        .register();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Flag, overlaps};

    fn flag(name: &str, first: i64, count: i64) -> Flag {
        Flag { name: name.into(), first, count }
    }

    #[test]
    fn an_id_has_one_owner() {
        let own = [flag("route", 0, 1), flag("objective", 0xA2, 2), flag("late", 0x1F8, 1)];
        let others = vec![("flags/game".to_string(), vec![flag("chapter", 0x7A, 1), flag("half", 0xA3, 1)])];
        let found = overlaps("flags/akropolis", &own, &others);
        assert_eq!(found, vec!["late: ID 0x1f8 is past the bank's end (0x1f8)", "ID 0x0a3: objective here, and flags/game's half"]);
        assert_eq!(overlaps("flags/akropolis", &own[..1], &others), Vec::<String>::new());
    }
}
