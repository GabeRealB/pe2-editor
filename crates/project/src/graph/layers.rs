//! A stack's layers edited, as the stack editor writes them: a declaration's text to its text with a layer added, removed
//! or moved. The layers are edited as a list and the section written back whole, before the originals as extraction
//! writes it, the rest of the declaration left as it is. An edit is not checked: what it makes of the stack is lowering's
//! to say, and a stack may be wrong while it is being made.

use anyhow::{Context, Result};
use toml::{Table, Value};

/// Where an operation sits on its graph's canvas: no setting of it, so moving it makes nothing again.
pub const POS: &str = "pos";

/// A declaration with its stack's layers changed.
fn edited(text: &str, change: impl FnOnce(&mut Vec<Value>) -> Result<()>) -> Result<String> {
    let fields: Table = toml::from_str(text).context("its declaration")?;
    let mut layers = fields.get("stack").and_then(Value::as_array).cloned().unwrap_or_default();
    change(&mut layers)?;
    let rest = without_stack(text)?;
    if layers.is_empty() {
        return Ok(rest);
    }
    let mut section = Table::new();
    section.insert("stack".into(), Value::Array(layers));
    Ok(crate::extract::with_stack(&rest, &toml::to_string(&section)?))
}

/// The file a stack stores, by the name it gives it.
#[must_use]
pub fn stored_file(stack: &str) -> Option<String> {
    let parsed: Table = stack.parse().ok()?;
    let layers = parsed.get("stack")?.as_array()?;
    let stored = layers.iter().find(|layer| layer.get("op").and_then(Value::as_str) == Some(super::FILE))?;
    stored.get("file")?.as_str().map(str::to_string)
}

/// A declaration without its stack. What is written over the first layer is the file's, kept: with no field above it, a
/// file's opening comment is held by that layer.
fn without_stack(text: &str) -> Result<String> {
    let mut document: toml_edit::DocumentMut = text.parse().context("its declaration")?;
    let opening = document.get("stack").and_then(toml_edit::Item::as_array_of_tables).and_then(|layers| layers.iter().next());
    let opening = opening.and_then(|layer| layer.decor().prefix()).and_then(toml_edit::RawString::as_str).unwrap_or_default().to_string();
    document.remove("stack");
    let rest = document.to_string();
    Ok(if opening.trim().is_empty() || !document.is_empty() { rest } else { format!("{opening}{rest}") })
}

/// The layer a setting belongs on: the first operation of the node's own before anything is taken from it, else its stored
/// file. What hangs off further down reads it there.
fn home(layers: &toml_edit::ArrayOfTables) -> Option<usize> {
    let op = |layer: &toml_edit::Table| layer.get("op").and_then(toml_edit::Item::as_str).map(str::to_string);
    let taken = layers.iter().position(|layer| matches!(op(layer).as_deref(), Some(super::OUTPUT | super::PREVIEW)));
    let own = layers.iter().take(taken.unwrap_or(usize::MAX)).position(|layer| op(layer).as_deref() != Some(super::FILE));
    own.or_else(|| (!layers.is_empty()).then_some(0))
}

/// Where a declaration holds a setting: on the layer it is on, else the one it belongs on, else - there being no stack -
/// among the declaration's own fields.
pub fn settings_table<'a>(document: &'a mut toml_edit::DocumentMut, key: &str) -> &'a mut toml_edit::Table {
    use toml_edit::Item;
    let layers = document.get("stack").and_then(Item::as_array_of_tables);
    let at = layers.and_then(|layers| {
        layers.iter().position(|layer| layer.contains_key(key) && !matches!(key, "op" | "name" | "file")).or_else(|| home(layers))
    });
    let Some(at) = at else { return document.as_table_mut() };
    let Some(layer) = document["stack"].as_array_of_tables_mut().and_then(|layers| layers.get_mut(at)) else {
        unreachable!("it was found there")
    };
    layer
}

/// A table and everything in it placed by where it is now, not the document it was parsed from.
fn unplaced(table: &mut toml_edit::Table) {
    table.set_position(0);
    for (_, item) in table.iter_mut() {
        match item {
            toml_edit::Item::Table(inner) => unplaced(inner),
            toml_edit::Item::ArrayOfTables(tables) => tables.iter_mut().for_each(unplaced),
            _ => {}
        }
    }
}

/// A declaration with its fields on the layer they belong on, leaving what is about the node as stored (`original`).
///
/// # Errors
/// It is not TOML.
pub fn settings_on_layers(text: &str) -> Result<String> {
    use toml_edit::Item;
    let split = text.find("\n[original.").map_or(text.len(), |at| at + 1);
    let (declared, originals) = text.split_at(split);
    // The head - the comment over the declaration - is written back over the stack below; parsed, it would be held by
    // whatever field comes first, a table or a dotted key, and moved with it.
    let head: Vec<&str> = declared.lines().take_while(|line| line.starts_with('#') || line.is_empty()).collect();
    let fields = declared.lines().skip(head.len()).collect::<Vec<_>>().join("\n");
    let mut document: toml_edit::DocumentMut = fields.parse().context("its declaration")?;
    let Some(at) = document.get("stack").and_then(Item::as_array_of_tables).and_then(home) else { return Ok(text.to_string()) };
    let keys: Vec<String> = document.iter().map(|(key, _)| key.to_string()).filter(|key| key != "stack").collect();
    let moved: Vec<(String, Item)> = keys.iter().filter_map(|key| Some((key.clone(), document.remove(key)?))).collect();
    let Some(Ok(mut layers)) = document.remove("stack").map(Item::into_array_of_tables) else { return Ok(text.to_string()) };
    if let Some(layer) = layers.get_mut(at) {
        for (key, item) in moved {
            layer.insert(&key, item);
        }
    }
    let mut body = String::new();
    for layer in layers.iter_mut() {
        layer.decor_mut().clear();
        layer.iter_mut().for_each(|(mut key, _)| key.leaf_decor_mut().clear());
        unplaced(layer);
        let mut single = toml_edit::ArrayOfTables::new();
        single.push(layer.clone());
        let mut alone = toml_edit::DocumentMut::new();
        alone.insert("stack", Item::ArrayOfTables(single));
        body.push_str(alone.to_string().trim());
        body.push_str("\n\n");
    }
    let head = head.join("\n");
    let originals = if originals.is_empty() { String::new() } else { format!("\n{originals}") };
    Ok(format!("{}\n\n{}\n{originals}", head.trim(), body.trim_end()).trim_start().to_string())
}

/// A declaration with another stack in place of its own, what was set on the old one kept on the new.
///
/// # Errors
/// It is not TOML, or the stack is not.
pub fn with_new_stack(text: &str, stack: &str) -> Result<String> {
    let fields: Table = toml::from_str(text).context("its declaration")?;
    let settings = super::with_settings(fields.clone());
    let bare = without_stack(text)?;
    let rest: Table = toml::from_str(&bare).context("its declaration")?;
    let held = settings.iter().filter(|(key, _)| !matches!(key.as_str(), "stack" | "original") && !rest.contains_key(*key));
    let mut lines = toml::to_string(&held.map(|(key, value)| (key.clone(), value.clone())).collect::<Table>()).context("its settings")?;
    lines.push_str(&bare);
    settings_on_layers(&crate::extract::with_stack(&lines, stack))
}

/// A layer added, to be the one at `at`.
///
/// # Errors
/// The declaration cannot be read, or the stack has no such place.
pub fn add(text: &str, at: usize, layer: Table) -> Result<String> {
    edited(text, |layers| {
        anyhow::ensure!(at <= layers.len(), "the stack has no place {at}");
        layers.insert(at, Value::Table(layer));
        Ok(())
    })
}

/// The layer at `at` removed.
///
/// # Errors
/// The declaration cannot be read, or the stack has no such layer.
pub fn remove(text: &str, at: usize) -> Result<String> {
    edited(text, |layers| {
        anyhow::ensure!(at < layers.len(), "the stack has no layer {at}");
        layers.remove(at);
        Ok(())
    })
}

/// The layer at `from` moved to be the one at `to`.
///
/// # Errors
/// The declaration cannot be read, or the stack has no such layer or place.
pub fn shift(text: &str, from: usize, to: usize) -> Result<String> {
    edited(text, |layers| {
        anyhow::ensure!(from < layers.len() && to < layers.len(), "the stack has no layer {from} or {to}");
        let layer = layers.remove(from);
        layers.insert(to, layer);
        Ok(())
    })
}

/// A graph layer's table, in a stack's layers.
fn graph_layer(layers: &mut [Value], at: usize) -> Result<&mut Table> {
    let layer = layers.get_mut(at).and_then(Value::as_table_mut).with_context(|| format!("the stack has no layer {at}"))?;
    anyhow::ensure!(layer.get("op").and_then(Value::as_str) == Some(super::GRAPH), "layer {at} is no graph");
    Ok(layer)
}

/// An operation of a graph layer's, by its id.
fn operation<'a>(layer: &'a mut Table, id: &str) -> Result<&'a mut Table> {
    let ops = layer.get_mut("ops").and_then(Value::as_table_mut);
    ops.and_then(|ops| ops.get_mut(id)).and_then(Value::as_table_mut).with_context(|| format!("no operation `{id}`"))
}

/// An id for an operation in a graph layer, from its name: free among those there, and readable.
#[must_use]
pub fn free_id(taken: &[&str], op: &str) -> String {
    let local = op.rsplit('@').next().unwrap_or(op);
    let base: String = local.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect();
    let names = std::iter::once(base.clone()).chain((2..=taken.len() + 2).map(|n| format!("{base}_{n}")));
    names.into_iter().find(|name| !taken.contains(&name.as_str())).unwrap_or(base)
}

/// An operation added to a graph layer, at a place on its canvas.
///
/// # Errors
/// The declaration cannot be read, or the layer is no graph.
pub fn add_operation(text: &str, at: usize, id: &str, mut op: Table, place: [f64; 2]) -> Result<String> {
    edited(text, |layers| {
        let layer = graph_layer(layers, at)?;
        op.insert(POS.into(), Value::Array(place.map(Value::Float).to_vec()));
        let ops = layer.entry("ops").or_insert_with(|| Value::Table(Table::new()));
        ops.as_table_mut().context("its `ops` is not a table")?.insert(id.to_string(), Value::Table(op));
        Ok(())
    })
}

/// An operation removed from a graph layer, with the wires to it; the layer's result, where it was that.
///
/// # Errors
/// The declaration cannot be read, or the layer has no such operation.
pub fn remove_operation(text: &str, at: usize, id: &str) -> Result<String> {
    edited(text, |layers| {
        let layer = graph_layer(layers, at)?;
        let ops = layer.get_mut("ops").and_then(Value::as_table_mut).with_context(|| format!("no operation `{id}`"))?;
        ops.remove(id).with_context(|| format!("no operation `{id}`"))?;
        for op in ops.iter_mut().filter_map(|(_, op)| op.as_table_mut()) {
            let Some(from) = op.get_mut("from").and_then(Value::as_table_mut) else { continue };
            from.retain(|_, wired| match wired {
                Value::String(source) => source != id,
                Value::Array(sources) => {
                    sources.retain(|s| s.as_str() != Some(id));
                    !sources.is_empty()
                }
                _ => true,
            });
        }
        if layer.get("result").and_then(Value::as_str) == Some(id) {
            layer.remove("result");
        }
        Ok(())
    })
}

/// A wire made, from an operation (or `above`) to a port of another: a port taking one value has it in place of what it
/// had, one taking many has it as well.
///
/// # Errors
/// The declaration cannot be read, or the layer has no such operation.
pub fn connect(text: &str, at: usize, (to, port, many): (&str, &str, bool), source: &str) -> Result<String> {
    edited(text, |layers| {
        let op = operation(graph_layer(layers, at)?, to)?;
        let from = op.entry("from").or_insert_with(|| Value::Table(Table::new()));
        let from = from.as_table_mut().context("its `from` is not a table")?;
        let wired = Value::String(source.to_string());
        if many {
            let sources = match from.remove(port) {
                Some(Value::Array(sources)) => sources,
                Some(one) => vec![one],
                _ => Vec::new(),
            };
            let mut sources = sources;
            if !sources.contains(&wired) {
                sources.push(wired);
            }
            from.insert(port.to_string(), Value::Array(sources));
        } else {
            from.insert(port.to_string(), wired);
        }
        Ok(())
    })
}

/// A wire removed: from an operation to a port of another.
///
/// # Errors
/// The declaration cannot be read, or the layer has no such operation.
pub fn disconnect(text: &str, at: usize, (to, port): (&str, &str), source: &str) -> Result<String> {
    edited(text, |layers| {
        let op = operation(graph_layer(layers, at)?, to)?;
        let Some(from) = op.get_mut("from").and_then(Value::as_table_mut) else { return Ok(()) };
        let empty = match from.get_mut(port) {
            Some(Value::Array(sources)) => {
                sources.retain(|s| s.as_str() != Some(source));
                sources.is_empty()
            }
            Some(Value::String(wired)) => wired == source,
            _ => false,
        };
        if empty {
            from.remove(port);
        }
        if from.is_empty() {
            op.remove("from");
        }
        Ok(())
    })
}

/// What a graph layer passes on down: an operation, or none.
///
/// # Errors
/// The declaration cannot be read, or the layer is no graph.
pub fn set_result(text: &str, at: usize, id: Option<&str>) -> Result<String> {
    edited(text, |layers| {
        let layer = graph_layer(layers, at)?;
        match id {
            Some(id) => layer.insert("result".into(), id.into()),
            None => layer.remove("result"),
        };
        Ok(())
    })
}

/// An operation moved on its layer's canvas.
///
/// # Errors
/// The declaration cannot be read, or the layer has no such operation.
pub fn place(text: &str, at: usize, id: &str, place: [f64; 2]) -> Result<String> {
    edited(text, |layers| {
        operation(graph_layer(layers, at)?, id)?.insert(POS.into(), Value::Array(place.map(Value::Float).to_vec()));
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::{add, add_operation, connect, disconnect, free_id, place, remove, remove_operation, set_result, shift};
    use toml::Table;

    const TEXT: &str = r#"# A node, written by extraction.

format = "str"

[[stack]]
op = "file"
file = "native.str"

[[stack]]
op = "output"
name = "native"

[original.USA]
sha1 = "ab"
"#;

    fn ops(text: &str) -> Vec<String> {
        let fields: Table = toml::from_str(text).unwrap();
        let layers = fields["stack"].as_array().unwrap();
        layers.iter().map(|l| l["op"].as_str().unwrap().to_string()).collect()
    }

    #[test]
    fn the_head_is_written_once_whatever_setting_comes_first() {
        let stack = "[[stack]]\nop = \"x@package\"\n\n[[stack]]\nop = \"output\"\nname = \"native\"\n";
        for first in ["headers.public = \"include\"\n", "[[slots]]\nname = \"s\"\n\n[[slots]]\nname = \"t\"\n", "[target.psx]\nlink = 1\n"] {
            let text = format!("# A node, written by extraction.\n\n{first}\n{stack}");
            let placed = super::settings_on_layers(&text).unwrap();
            assert_eq!(placed.matches("# A node").count(), 1, "{placed}");
            assert!(placed.starts_with("# A node"), "{placed}");
            let fields: Table = toml::from_str(&placed).unwrap();
            assert_eq!(fields.keys().map(String::as_str).collect::<Vec<_>>(), ["stack"], "{placed}");
        }
    }

    #[test]
    fn settings_go_on_the_operation_that_reads_them() {
        let text = "# A node.\n\nmode = \"indexed_4bpp\"\npalettes.default = { at = [0, 0] }\n\n[cells.a]\nrect = [0, 0, 1, 1]\n\n\
                    [[stack]]\nop = \"file\"\nfile = \"source.ora\"\n\n[[stack]]\nop = \"x@ora\"\n\n\
                    [[stack]]\nop = \"output\"\nname = \"native\"\n\n[original.USA]\nsha1 = \"ab\"\n";
        let placed = super::settings_on_layers(text).unwrap();
        assert!(placed.starts_with("# A node."), "{placed}");
        let fields: Table = toml::from_str(&placed).unwrap();
        assert_eq!(fields.keys().map(String::as_str).collect::<Vec<_>>(), ["stack", "original"], "{placed}");
        let layers = fields["stack"].as_array().unwrap();
        assert_eq!(layers[1]["mode"].as_str(), Some("indexed_4bpp"));
        assert!(layers[1]["cells"]["a"]["rect"].is_array() && layers[0].get("mode").is_none(), "{placed}");
        // Read back, they are what the node is set to, and a node's plan is shared with those alike.
        let settings = crate::graph::with_settings(fields);
        assert_eq!(settings["mode"].as_str(), Some("indexed_4bpp"));
        assert!(!settings.contains_key("name") && !settings.contains_key("file"));
        assert!(!crate::graph::shape(&settings).contains("mode"));
    }

    #[test]
    fn an_opening_comment_outlives_a_stack_edit() {
        let text = "# Who made this.\n\n[[stack]]\nop = \"file\"\nfile = \"a\"\n";
        let edited = super::add(text, 1, Table::from_iter([("op".to_string(), toml::Value::from("output")), ("name".to_string(), toml::Value::from("native"))])).unwrap();
        assert!(edited.starts_with("# Who made this."), "{edited}");
        assert!(edited.contains("name = \"native\""), "{edited}");
    }

    #[test]
    fn a_setting_stays_above_what_hangs_off() {
        let text = "skeleton = \"uid://s\"\n\n[[stack]]\nop = \"file\"\nfile = \"a\"\n\n[[stack]]\nop = \"output\"\nname = \"native\"\n\n\
                    [[stack]]\nop = \"x@skeleton_of\"\n";
        let fields: Table = toml::from_str(&super::settings_on_layers(text).unwrap()).unwrap();
        assert_eq!(fields["stack"][0]["skeleton"].as_str(), Some("uid://s"));
    }

    #[test]
    fn a_stored_file_holds_what_is_said_of_it_and_a_new_stack_takes_it_over() {
        let text = "[[stack]]\nop = \"file\"\nfile = \"a.str\"\nformat = \"str\"\n\n[[stack]]\nop = \"output\"\nname = \"native\"\n";
        let mut document: toml_edit::DocumentMut = text.parse().unwrap();
        super::settings_table(&mut document, "format").insert("format", toml_edit::value("x"));
        super::settings_table(&mut document, "new").insert("new", toml_edit::value(1));
        let fields: Table = toml::from_str(&document.to_string()).unwrap();
        assert_eq!(fields["stack"][0]["format"].as_str(), Some("x"));
        assert_eq!(fields["stack"][0]["new"].as_integer(), Some(1));
        let replaced = super::with_new_stack(text, "[[stack]]\nop = \"x@import\"\n").unwrap();
        let fields: Table = toml::from_str(&replaced).unwrap();
        assert_eq!(fields["stack"][0]["op"].as_str(), Some("x@import"));
        assert_eq!(fields["stack"][0]["format"].as_str(), Some("str"), "{replaced}");
    }

    #[test]
    fn a_layer_is_added_moved_and_removed_and_the_rest_stays() {
        let mut layer = Table::new();
        layer.insert("op".into(), "pe2.str@stream".into());
        let added = add(TEXT, 1, layer).unwrap();
        assert_eq!(ops(&added), ["file", "pe2.str@stream", "output"]);
        // Above the originals, with what the declaration said.
        assert!(added.find("[[stack]]").unwrap() < added.find("[original.USA]").unwrap(), "{added}");
        assert!(added.starts_with("# A node"), "{added}");
        assert!(added.contains("sha1 = \"ab\""), "{added}");
        let moved = shift(&added, 1, 2).unwrap();
        assert_eq!(ops(&moved), ["file", "output", "pe2.str@stream"]);
        assert_eq!(ops(&remove(&moved, 2).unwrap()), ["file", "output"]);
        // Removed in turn, nothing is left of it; added again, it is back in its place.
        let none = remove(&remove(TEXT, 0).unwrap(), 0).unwrap();
        assert!(!none.contains("stack"), "{none}");
        let mut file = Table::new();
        file.insert("op".into(), "file".into());
        let back = add(&none, 0, file).unwrap();
        assert_eq!(ops(&back), ["file"]);
        assert!(back.find("[[stack]]").unwrap() < back.find("[original.USA]").unwrap(), "{back}");
    }

    const GRAPH: &str = "[[stack]]\nop = \"graph\"\n";

    fn layer(text: &str) -> Table {
        let fields: Table = toml::from_str(text).unwrap();
        fields["stack"].as_array().unwrap()[0].as_table().unwrap().clone()
    }

    fn op(name: &str) -> Table {
        let mut table = Table::new();
        table.insert("op".into(), name.into());
        table
    }

    #[test]
    fn a_graph_is_built_wired_and_taken_apart() {
        let text = add_operation(GRAPH, 0, "source", op("file"), [10.0, 20.0]).unwrap();
        let text = add_operation(&text, 0, "join", op("join"), [200.0, 20.0]).unwrap();
        // One value: the last wired. Many: all of them, once.
        let text = connect(&text, 0, ("join", "one", false), "source").unwrap();
        let text = connect(&text, 0, ("join", "one", false), "above").unwrap();
        let text = connect(&text, 0, ("join", "many", true), "source").unwrap();
        let text = connect(&text, 0, ("join", "many", true), "source").unwrap();
        let text = connect(&text, 0, ("join", "many", true), "above").unwrap();
        let wired = layer(&text);
        let from = &wired["ops"]["join"]["from"];
        assert_eq!(from["one"].as_str(), Some("above"));
        assert_eq!(from["many"].as_array().unwrap().len(), 2);
        let text = set_result(&text, 0, Some("join")).unwrap();
        assert_eq!(layer(&text)["result"].as_str(), Some("join"));
        // A place is no setting of what it moves.
        let text = place(&text, 0, "join", [5.0, 6.0]).unwrap();
        assert_eq!(layer(&text)["ops"]["join"]["pos"].as_array().unwrap()[1].as_float(), Some(6.0));
        // Unplugged, then the operation and all wires to it.
        let text = disconnect(&text, 0, ("join", "many"), "above").unwrap();
        assert_eq!(layer(&text)["ops"]["join"]["from"]["many"].as_array().unwrap().len(), 1);
        let text = remove_operation(&text, 0, "source").unwrap();
        let left = layer(&text);
        assert!(left["ops"]["join"]["from"].get("many").is_none(), "{left:?}");
        assert_eq!(left["ops"]["join"]["from"]["one"].as_str(), Some("above"));
        let text = remove_operation(&text, 0, "join").unwrap();
        assert!(layer(&text).get("result").is_none());
    }

    #[test]
    fn an_operation_gets_a_free_readable_id() {
        assert_eq!(free_id(&[], "pe2.str@encode"), "encode");
        assert_eq!(free_id(&["encode", "encode_2"], "video-container@encode"), "encode_3");
        assert_eq!(free_id(&[], "below"), "below");
    }

    #[test]
    fn what_is_no_graph_takes_no_operation() {
        assert!(add_operation(TEXT, 0, "a", op("file"), [0.0, 0.0]).is_err());
    }

    #[test]
    fn a_place_the_stack_has_not_is_refused() {
        assert!(add(TEXT, 3, Table::new()).is_err());
        assert!(remove(TEXT, 2).is_err());
        assert!(shift(TEXT, 0, 2).is_err());
    }
}
