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
    let mut document: toml_edit::DocumentMut = text.parse().context("its declaration")?;
    document.remove("stack");
    let rest = document.to_string();
    if layers.is_empty() {
        return Ok(rest);
    }
    let mut section = Table::new();
    section.insert("stack".into(), Value::Array(layers));
    Ok(crate::extract::with_stack(&rest, &toml::to_string(&section)?))
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

kind = "movie"

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

    const GRAPH: &str = "kind = \"movie\"\n\n[[stack]]\nop = \"graph\"\n";

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
