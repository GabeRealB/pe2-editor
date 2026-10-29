//! What agents are told of an image besides its declaration, and the palette tool.

use pe2_ext::{Asset, Project, Result, Table, Value, anyhow};

use crate::{cells, palette_colors, palette_names, rgba};

/// An image's palettes by name and its cells' count.
pub(crate) fn facts(asset: &Asset) -> Table {
    let mut facts = Table::new();
    let palettes = palette_names(asset).into_iter().map(Value::String).collect();
    facts.insert("palettes".into(), Value::Array(palettes));
    facts.insert("cells".into(), Value::Integer(i64::try_from(cells(asset).len()).unwrap_or(i64::MAX)));
    facts
}

pub(crate) const PALETTE: &str = "A palette's colors: each as stored (ABGR1555) and as RGBA.";

pub(crate) const PALETTE_ARGUMENTS: &str = r#"{
    "type": "object",
    "properties": {
        "asset": { "type": "string", "description": "The image: its path or its uid." },
        "palette": { "type": "string", "description": "The palette's name." }
    },
    "required": ["asset", "palette"]
}"#;

/// A palette's colors, each as stored and as RGBA.
pub(crate) fn palette(project: &dyn Project, arguments: &Value) -> Result<Value> {
    let name = arguments.get("asset").and_then(Value::as_str).ok_or_else(|| anyhow!("no asset given"))?;
    let wanted = arguments.get("palette").and_then(Value::as_str).ok_or_else(|| anyhow!("no palette given"))?;
    let asset = project
        .asset(name)
        .or_else(|| project.resolve(&Value::String(name.to_string())))
        .ok_or_else(|| anyhow!("no asset is named {name}"))?;
    let names = palette_names(&asset);
    let chosen = names.iter().position(|n| n == wanted).ok_or_else(|| {
        anyhow!("{}: no palette is named {wanted}; its palettes: {}", asset.path(), names.join(", "))
    })?;
    let colors = palette_colors(&asset, chosen)
        .ok_or_else(|| anyhow!("{}: palette {wanted} has no stored colors", asset.path()))??;
    let colors = colors
        .iter()
        .map(|&c| {
            let [red, green, blue, alpha] = rgba(c);
            let mut color = Table::new();
            color.insert("abgr1555".into(), Value::String(format!("{c:#06x}")));
            color.insert("rgba".into(), Value::String(format!("#{red:02x}{green:02x}{blue:02x}{alpha:02x}")));
            Value::Table(color)
        })
        .collect();
    let mut answer = Table::new();
    answer.insert("path".into(), Value::String(asset.path().to_string()));
    answer.insert("palette".into(), Value::String(wanted.to_string()));
    answer.insert("colors".into(), Value::Array(colors));
    Ok(Value::Table(answer))
}
