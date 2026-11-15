//! `generate`: a package's build made from its description, as a build system makes its build from what a project says
//! (a generator's operation: what it makes is an operation the project uses, `uid://<generator>/<part>`). One `part` a
//! layer: `first_pass`, `unit/<extension>` (what a unit of a language is made into), `package`, `library`.
//!
//! The description is the generator node's own settings, those of the configuration built laid over them
//! (`configs.<name>`):
//!
//! ```toml
//! first_pass = { program = "pe2.aro-cpp:cpp", args = ["-E", "-P", "-DPE2_FIRST_PASS", "-o", "{out}", "{in}"] }
//! [languages.c]                 # each step a `run` box: its program, its args, what it makes (`outputs`, `stdout`), what it
//! steps = [                     # takes (`inputs`: `unit`, or an earlier step by its place) - the last makes the unit's object
//!     { program = "pe2.aro-cpp:cpp", args = ["-E", "-P", "-o", "{out}", "{in}"], outputs = ["unit.i"] },
//!     { program = "pe2.gcc-psx:cc1", args = ["-o", "{out}", "{in}"], outputs = ["unit.s"] },
//!     { program = "pe2.binutils-mipsel-maspsx:as", args = ["-o", "{out}", "{in}"], inputs = ["1", "unit"] },
//! ]
//! [link]                        # what links a package (`linked`: pe2.packages@link_inputs), then what is made of it
//! steps = [{ program = "pe2.binutils-mipsel:ld", args = ["-T", "{files.script/}", "-o", "{out}", "{files.objects/}"], outputs = ["image.elf"] }]
//! finish = "pe2.psx@finish"     # an operation taking the image, the linked file and the tree; none: the last step's is the image
//! [publish]                     # what binding the package links against: the last step a listing of its symbols (`nm`)
//! steps = [...]
//! ```
//!
//! A generator's operations name one another as `/<part>`, which the host makes `uid://<generator>/<part>`.

use pe2_ext::{Call, Result, Table, Value, anyhow, bail, ty};

/// What a generator's operation is: a node declaring an operation of the project's.
fn operation(label: &str, ops: Table) -> Table {
    let mut defined = Table::new();
    defined.insert("op".into(), Value::String("pe2.graph@operation".into()));
    defined.insert("label".into(), Value::String(label.into()));
    defined.insert("ops".into(), Value::Table(ops));
    Table::from_iter([("stack".to_string(), Value::Array(vec![Value::Table(defined)]))])
}

fn text(value: &str) -> Value {
    Value::String(value.to_string())
}

fn texts(items: &[&str]) -> Value {
    Value::Array(items.iter().map(|item| text(item)).collect())
}

/// A box of the operation: its operation, its settings, what it takes from (`from`).
fn op(name: &str, settings: &[(&str, Value)], from: &[(&str, Value)]) -> Value {
    let mut table = Table::from_iter([("op".to_string(), text(name))]);
    table.extend(settings.iter().map(|(k, v)| ((*k).to_string(), v.clone())));
    if !from.is_empty() {
        table.insert("from".into(), Value::Table(from.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect()));
    }
    Value::Table(table)
}

fn input(name: &str) -> Value {
    op("input", &[("name", text(name))], &[])
}

fn output(name: &str, from: &str) -> Value {
    op("output", &[("name", text(name))], &[("value", text(from))])
}

/// One table laid over another, key by key, tables by theirs.
fn laid_over(under: &mut Table, over: &Table) {
    for (key, value) in over {
        match (under.get_mut(key), value) {
            (Some(Value::Table(below)), Value::Table(above)) => laid_over(below, above),
            _ => {
                under.insert(key.clone(), value.clone());
            }
        }
    }
}

/// `run` boxes, one a step, each taking `unit` (the input box), an earlier step by its place, or what `start` names,
/// the first taking `start` where it says nothing; the last named `last`, the others `<prefix><place>`. The boxes, and the
/// last one's id.
fn steps(
    ops: &mut Table,
    steps: &[Value],
    prefix: &str,
    start: &[&str],
    last_outputs: Option<&[&str]>,
) -> Result<Vec<String>> {
    let mut ids: Vec<String> = vec![];
    for (at, step) in steps.iter().enumerate() {
        let step = step.as_table().ok_or_else(|| anyhow!("{prefix} step {at} is no table"))?;
        let id = format!("{prefix}{at}");
        let mut settings: Vec<(&str, Value)> = vec![];
        let program = step.get("program").cloned().ok_or_else(|| anyhow!("{prefix} step {at} runs no program"))?;
        settings.push(("program", program));
        settings.push(("args", step.get("args").cloned().unwrap_or(Value::Array(vec![]))));
        let outputs = match (step.get("outputs"), last_outputs) {
            (Some(outputs), _) => Some(outputs.clone()),
            (None, Some(last)) if at + 1 == steps.len() => Some(texts(last)),
            _ => None,
        };
        settings.extend(outputs.map(|outputs| ("outputs", outputs)));
        for key in ["stdout", "rewrite", "makes"] {
            settings.extend(step.get(key).map(|value| (key, value.clone())));
        }
        let taken: Vec<Value> = match step.get("inputs").and_then(Value::as_array) {
            Some(named) => named
                .iter()
                .map(|name| {
                    let name = name.as_str().unwrap_or_default();
                    match name.parse::<usize>() {
                        Ok(place) => ids
                            .get(place)
                            .map(|id: &String| text(id))
                            .ok_or_else(|| anyhow!("{id} takes step {place}, none before it")),
                        Err(_) => Ok(text(name)),
                    }
                })
                .collect::<Result<_>>()?,
            None => match ids.last() {
                Some(previous) => vec![text(previous)],
                None => start.iter().map(|s| text(s)).collect(),
            },
        };
        ops.insert(id.clone(), op("run", &settings, &[("inputs", Value::Array(taken))]));
        ids.push(id);
    }
    Ok(ids)
}

/// The steps a part of the description lists.
fn steps_of<'a>(description: &'a Table, part: &str) -> Result<&'a [Value]> {
    let listed = description.get(part).and_then(|p| p.get("steps")).and_then(Value::as_array);
    Ok(listed.ok_or_else(|| anyhow!("the description lists no {part}.steps"))?.as_slice())
}

/// What a unit of a language is made into: its steps, the last its object (`{given.object}` where it names no outputs).
fn unit(description: &Table, extension: &str) -> Result<Table> {
    let listed = description
        .get("languages")
        .and_then(|l| l.get(extension))
        .and_then(|l| l.get("steps"))
        .and_then(Value::as_array)
        .ok_or_else(|| {
            anyhow!("the description says nothing of units of .{extension} (languages.{extension}.steps)")
        })?;
    let mut ops = Table::from_iter([("unit".to_string(), input("unit"))]);
    let ids = steps(&mut ops, listed, "step", &["unit"], Some(&["{given.object}"]))?;
    let last = ids.last().ok_or_else(|| anyhow!("languages.{extension} has no steps"))?;
    ops.insert("object".into(), output("object", last));
    Ok(operation(&format!("Make a unit of .{extension} an object"), ops))
}

/// A C unit's first pass: what its preprocessor makes of it to say what it reads (`first/<unit>`).
fn first_pass(description: &Table) -> Result<Table> {
    let pass = description
        .get("first_pass")
        .and_then(Value::as_table)
        .ok_or_else(|| anyhow!("the description has no first_pass"))?;
    let program = pass.get("program").cloned().ok_or_else(|| anyhow!("first_pass runs no program"))?;
    let args = pass.get("args").cloned().unwrap_or(Value::Array(vec![]));
    let mut ops = Table::from_iter([("unit".to_string(), input("unit"))]);
    let settings = [("program", program), ("args", args), ("outputs", texts(&["first/{in.path}"]))];
    ops.insert("first".into(), op("run", &settings, &[("inputs", texts(&["unit"]))]));
    ops.insert("made".into(), output("first", "first"));
    Ok(operation("First pass of a C unit", ops))
}

/// A package's build, or a library's (its objects alone): its first pass, its units of each language described, what
/// links it, its image and what it publishes.
fn package(description: &Table, library: bool) -> Result<Table> {
    let languages: Vec<String> =
        description.get("languages").and_then(Value::as_table).map_or_default(|l| l.keys().cloned().collect());
    let mut ops = Table::from_iter([("sources".to_string(), input("sources"))]);
    let units = |sources: &str, extension: &str| {
        op("pe2.packages@units", &[("extension", text(extension))], &[("sources", text(sources))])
    };
    // The units a first pass reads: C's, where the description says no other.
    let first = description.get("first_pass").and_then(|f| f.get("extension")).and_then(Value::as_str).unwrap_or("c");
    ops.insert("first_units".into(), units("sources", first));
    ops.insert("first".into(), op("for_each", &[("body", text("/first_pass"))], &[("items", text("first_units"))]));
    ops.insert(
        "firsted".into(),
        op("pe2.packages@with", &[], &[("sources", text("sources")), ("files", texts(&["first"]))]),
    );
    ops.insert("resolved".into(), op("pe2.packages@resolve", &[], &[("sources", text("firsted"))]));
    let mut compiled = vec![];
    for extension in &languages {
        ops.insert(format!("units_{extension}"), units("resolved", extension));
        let body = format!("/unit/{extension}");
        let each = op("for_each", &[("body", text(&body))], &[("items", text(&format!("units_{extension}")))]);
        ops.insert(format!("objects_{extension}"), each);
        compiled.push(text(&format!("objects_{extension}")));
    }
    ops.insert(
        "objects".into(),
        op("pe2.packages@collect", &[], &[("sources", text("resolved")), ("compiled", Value::Array(compiled))]),
    );
    ops.insert("values".into(), output("resolved", "resolved"));
    ops.insert("compiled".into(), output("objects", "objects"));
    if library {
        return Ok(operation("Compile a library", ops));
    }
    ops.insert(
        "linked".into(),
        op("pe2.packages@link_inputs", &[], &[("sources", text("resolved")), ("objects", text("objects"))]),
    );
    let link =
        description.get("link").and_then(Value::as_table).ok_or_else(|| anyhow!("the description has no link"))?;
    let linking = steps(&mut ops, steps_of(description, "link")?, "link", &["linked"], None)?;
    let at = |key: &str, default: usize| -> Result<String> {
        let place = link.get(key).and_then(Value::as_integer).and_then(|p| usize::try_from(p).ok()).unwrap_or(default);
        linking.get(place).cloned().ok_or_else(|| anyhow!("link.{key} names step {place}, which is not there"))
    };
    let (elf, last) = (at("elf", 0)?, at("image", linking.len().saturating_sub(1))?);
    let image = match link.get("finish").and_then(Value::as_str) {
        Some(finish) => {
            let from = [("image", text(&last)), ("elf", text(&elf)), ("sources", text("resolved"))];
            ops.insert("image".into(), op(finish, &[], &from));
            "image".to_string()
        }
        None => last,
    };
    ops.insert("made".into(), output("image", &image));
    let publishing = steps(&mut ops, steps_of(description, "publish")?, "publish", &["linked"], None)?;
    let listing = publishing.last().ok_or_else(|| anyhow!("publish has no steps"))?;
    ops.insert("symbols".into(), op("pe2.packages@published", &[], &[("listing", text(listing))]));
    ops.insert("published".into(), output("symbols", "symbols"));
    Ok(operation("Build a package", ops))
}

/// `generate`: the part (`part`) of the build described - the generator's settings, the configuration's laid over them.
///
/// # Errors
/// It is given no description, or the part asked for is not described.
pub fn generate(call: &mut Call) -> Result<Table> {
    let part =
        call.settings.get("part").and_then(Value::as_str).ok_or_else(|| anyhow!("it generates no `part`"))?.to_string();
    let described = call.take::<ty::Any>(0)?;
    let pe2_ext::Data::Typed(described) = described else { bail!("it is given no description") };
    let mut description = described.settings;
    let configured =
        call.config().and_then(|config| description.get("configs")?.get(&config.name)?.as_table().cloned());
    if let Some(over) = configured {
        laid_over(&mut description, &over);
    }
    match part.as_str() {
        "first_pass" => first_pass(&description),
        "package" => package(&description, false),
        "library" => package(&description, true),
        _ => match part.strip_prefix("unit/") {
            Some(extension) => unit(&description, extension),
            None => bail!("no part {part}: first_pass, unit/<extension>, package or library"),
        },
    }
}
