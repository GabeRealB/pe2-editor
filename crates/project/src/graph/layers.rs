//! A stack's layers edited, as the stack editor writes them: a declaration's text to its text with a layer added, removed
//! or moved. The layers are edited as a list and the section written back whole, before the originals as extraction
//! writes it, the rest of the declaration left as it is. An edit is not checked: what it makes of the stack is lowering's
//! to say, and a stack may be wrong while it is being made.

use anyhow::{Context, Result};
use toml::{Table, Value};

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

#[cfg(test)]
mod tests {
    use super::{add, remove, shift};
    use toml::Table;

    const TEXT: &str = r#"# An asset bundle, written by extraction.

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
        assert!(added.starts_with("# An asset bundle"), "{added}");
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

    #[test]
    fn a_place_the_stack_has_not_is_refused() {
        assert!(add(TEXT, 3, Table::new()).is_err());
        assert!(remove(TEXT, 2).is_err());
        assert!(shift(TEXT, 0, 2).is_err());
    }
}
