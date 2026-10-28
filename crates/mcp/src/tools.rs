//! The tools: their definitions (names, descriptions, input schemas from the argument types) and their answers.

use base64::Engine;
use project::index::{Asset, Index, Kind, Reference};
use rmcp::model::{CallToolResult, ContentBlock, ReadResourceResult, ResourceContents, Tool, ToolAnnotations};
use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use crate::View;

/// An asset named by its path (`stages/akropolis/rooms/cafeteria/props/puff`, with or without `res://` and `.asset`)
/// or its uid (`uid://...`).
#[derive(Deserialize, JsonSchema)]
struct AssetArgs {
    /// The asset: its path (`stages/akropolis/rooms/cafeteria/props/puff`) or its uid (`uid://...`).
    asset: String,
}

#[derive(Deserialize, JsonSchema)]
struct FindArgs {
    /// Text the asset's path contains (case-insensitive).
    #[serde(default)]
    text: Option<String>,
    /// Only assets of this kind (`image`, `file`, `folder`, `vram_slot`, ...).
    #[serde(default)]
    kind: Option<String>,
    /// Only assets of this role (`texture`, `atlas`, `background`, ...).
    #[serde(default)]
    role: Option<String>,
    /// Only assets under this folder (`stages/akropolis`).
    #[serde(default)]
    under: Option<String>,
    /// How many to return (default 50, at most 500).
    #[serde(default)]
    limit: Option<usize>,
    /// How many matches to skip first, to page through them.
    #[serde(default)]
    offset: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
struct FolderArgs {
    /// The folder's path; the project's top when empty or absent.
    #[serde(default)]
    path: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct ProblemsArgs {
    /// How many to return of each list (default 100).
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
struct PreviewArgs {
    /// The image: its path or its uid.
    asset: String,
    /// The look to show it in, by name (an image's variants; an atlas's `default` and its variants). Its first when
    /// absent.
    #[serde(default)]
    variant: Option<String>,
    /// Scale each pixel up this many times (1 to 8; default 1), for small images.
    #[serde(default)]
    scale: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
struct PaletteArgs {
    /// The image: its path or its uid.
    asset: String,
    /// The palette's name.
    palette: String,
}

#[derive(Deserialize, JsonSchema)]
struct NoArgs {}

fn tool<T: JsonSchema + 'static>(name: &'static str, description: &'static str) -> Tool {
    let annotations = ToolAnnotations::new().read_only(true);
    Tool::new(name, description, Map::new()).with_input_schema::<T>().annotate(annotations)
}

/// Every tool.
#[must_use]
pub fn definitions() -> Vec<Tool> {
    vec![
        tool::<NoArgs>(
            "project",
            "The project: its root, its layers bottom to top (assets or files, a mount's mount point, which are \
             read-only, their entries, operations and problems), how many assets, entries and problems it has, and \
             who answers (the editor, when it has the project open, or pe2-cli).",
        ),
        tool::<FindArgs>(
            "find_assets",
            "Find assets by text in their path, kind, role or folder. Returns each one's path, uid, kind and role, \
             and how many matched in all.",
        ),
        tool::<FolderArgs>(
            "list_folder",
            "The entries of a folder of the resolved project: its folders, its assets (with kind and role) and its \
             plain files.",
        ),
        tool::<AssetArgs>(
            "get_asset",
            "An asset as resolved: path, uid, kind, role, the layers creating and overriding it, its files (size, \
             sha1, layer), an image's palettes, looks and cell count, and its declaration (asset.toml) as written.",
        ),
        tool::<AssetArgs>(
            "references",
            "An asset's references both ways: the ones its declaration makes (slot, uid, the asset they resolve to) \
             and the assets referencing it.",
        ),
        tool::<ProblemsArgs>(
            "problems",
            "What is wrong with the project: unreadable declarations, duplicate uids, conflicting or unlisted \
             entries, stale overrides; and references naming no asset.",
        ),
        tool::<PreviewArgs>(
            "preview",
            "An image's picture as PNG - a background, a texture through a palette, an atlas with each cell through \
             its own - in one of its looks.",
        ),
        tool::<PaletteArgs>("palette", "A palette's colors: each as stored (ABGR1555) and as RGBA."),
        tool::<NoArgs>(
            "editor_selection",
            "The asset selected in the editor and the look it is shown in. Only when the editor has the project open.",
        ),
        tool::<AssetArgs>(
            "editor_show",
            "Select an asset in the editor and reveal it in its tree, for the user to look at. Changes only what the \
             editor shows. Only when the editor has the project open.",
        ),
    ]
}

/// A tool's answer.
pub fn call(view: &mut View, name: &str, arguments: &Map<String, Value>) -> CallToolResult {
    let result = match name {
        "project" => Ok(project(view)),
        "find_assets" => parse(arguments).map(|a| find_assets(view.index, &a)),
        "list_folder" => parse(arguments).map(|a: FolderArgs| list_folder(view.index, a.path.as_deref())),
        "get_asset" => parse(arguments).and_then(|a: AssetArgs| get_asset(view.index, &a.asset)),
        "references" => parse(arguments).and_then(|a: AssetArgs| references(view.index, &a.asset)),
        "problems" => parse(arguments).map(|a: ProblemsArgs| problems(view.index, a.limit.unwrap_or(100))),
        "preview" => return parse(arguments).and_then(|a| preview(view.index, &a)).unwrap_or_else(failure),
        "palette" => parse(arguments).and_then(|a| palette(view.index, &a)),
        "editor_selection" => editor_selection(view),
        "editor_show" => parse(arguments).and_then(|a: AssetArgs| editor_show(view, &a.asset)),
        _ => Err(format!("no tool is named {name}")),
    };
    match result {
        Ok(value) => {
            let text = serde_json::to_string_pretty(&value).unwrap_or_default();
            let mut result = CallToolResult::success(vec![ContentBlock::text(text)]);
            result.structured_content = Some(value);
            result
        }
        Err(e) => failure(e),
    }
}

fn failure(message: String) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message)])
}

fn parse<T: DeserializeOwned>(arguments: &Map<String, Value>) -> Result<T, String> {
    serde_json::from_value(Value::Object(arguments.clone())).map_err(|e| format!("arguments: {e}"))
}

/// A resource's contents: `uid://...` is an asset's declaration.
pub fn read(view: &View, uri: &str) -> Result<ReadResourceResult, String> {
    let asset = asset(view.index, uri)?;
    let contents = ResourceContents::TextResourceContents {
        uri: uri.to_string(),
        mime_type: Some("application/toml".into()),
        text: asset.text().to_string(),
        meta: None,
    };
    Ok(ReadResourceResult::new(vec![contents]))
}

/// An asset by its path (`res://` and `.asset` allowed) or its uid.
fn asset<'a>(index: &'a Index, name: &str) -> Result<&'a Asset, String> {
    let name = name.trim();
    if let Some(path) = index.by_uid.get(name) {
        return index.assets.get(path).ok_or_else(|| format!("{name}: no asset"));
    }
    let path = name.trim_start_matches("res://").trim_end_matches('/');
    let path = path.strip_suffix(project::index::BUNDLE_SUFFIX).unwrap_or(path);
    index.assets.get(path).ok_or_else(|| {
        if name.starts_with("uid://") {
            format!("{name}: no asset has this uid")
        } else {
            format!("{name}: no asset has this path (find_assets searches by text)")
        }
    })
}

fn summary(asset: &Asset) -> Value {
    json!({ "path": asset.path, "uid": asset.uid, "kind": asset.kind, "role": asset.role })
}

fn project(view: &View) -> Value {
    let root = &view.project.root;
    let layers: Vec<Value> = view
        .index
        .layers()
        .iter()
        .zip(&view.project.layers)
        .map(|(info, layer)| {
            let path = layer.path.strip_prefix(root).unwrap_or(&layer.path);
            let kind = match info.kind {
                project::LayerKind::Assets => "assets",
                project::LayerKind::Files => "files",
            };
            json!({
                "name": info.name,
                "kind": kind,
                "mounted_at": (!info.at.is_empty()).then_some(&info.at),
                "path": path.display().to_string(),
                "read_only": info.read_only,
                "entries": info.entries,
                "operations": info.overrides + info.moves + info.deletes,
                "problems": info.problems,
            })
        })
        .collect();
    json!({
        "root": root.display().to_string(),
        "layers": layers,
        "assets": view.index.assets.len(),
        "entries": view.index.entries.len(),
        "problems": view.index.problems.len(),
        "dangling_references": view.index.dangling.len(),
        "answered_by": if view.editor.is_some() { "pe2-editor" } else { "pe2-cli" },
    })
}

fn find_assets(index: &Index, args: &FindArgs) -> Value {
    let text = args.text.as_deref().map(str::to_lowercase);
    let under = args.under.as_deref().map(|u| format!("{}/", u.trim_end_matches('/')));
    let matches: Vec<&Asset> = index
        .assets
        .values()
        .filter(|a| text.as_ref().is_none_or(|t| a.path.to_lowercase().contains(t.as_str())))
        .filter(|a| args.kind.is_none() || a.kind == args.kind)
        .filter(|a| args.role.is_none() || a.role == args.role)
        .filter(|a| under.as_ref().is_none_or(|u| a.path.starts_with(u.as_str())))
        .collect();
    let (offset, limit) = (args.offset.unwrap_or(0), args.limit.unwrap_or(50).min(500));
    let page: Vec<Value> = matches.iter().skip(offset).take(limit).map(|a| summary(a)).collect();
    json!({ "total": matches.len(), "offset": offset, "assets": page })
}

fn list_folder(index: &Index, path: Option<&str>) -> Value {
    let path = path.unwrap_or_default().trim_matches('/');
    let prefix = if path.is_empty() { String::new() } else { format!("{path}/") };
    let (mut folders, mut assets, mut files) = (vec![], vec![], vec![]);
    for (key, entry) in index.entries.range(prefix.clone()..) {
        let Some(name) = key.strip_prefix(&prefix) else { break };
        if name.is_empty() || name.contains('/') {
            continue;
        }
        match entry.kind {
            Kind::Folder => folders.push(json!(name)),
            Kind::File => files.push(json!(name)),
            Kind::Asset => {
                let resolved = key.strip_suffix(project::index::BUNDLE_SUFFIX).unwrap_or(key);
                if let Some(asset) = index.assets.get(resolved) {
                    assets.push(summary(asset));
                }
            }
        }
    }
    json!({ "path": path, "folders": folders, "assets": assets, "files": files })
}

fn get_asset(index: &Index, name: &str) -> Result<Value, String> {
    let asset = asset(index, name)?;
    let files: Vec<Value> = asset
        .files
        .iter()
        .map(|f| json!({ "name": f.name, "size": f.size, "sha1": f.sha1, "layer": f.layer }))
        .collect();
    let mut out = summary(asset);
    let fields = out.as_object_mut().expect("an object");
    fields.insert("layers".into(), json!(asset.layers));
    fields.insert("files".into(), json!(files));
    if preview::is_texture(asset) || preview::has_picture(asset) {
        fields.insert("palettes".into(), json!(preview::palette_names(asset)));
        fields.insert("looks".into(), json!(preview::variant_names(asset)));
        fields.insert("cells".into(), json!(preview::cells_of(asset).len()));
        fields.insert("has_picture".into(), json!(preview::has_picture(asset)));
    }
    if let Some(error) = &asset.error {
        fields.insert("error".into(), json!(error));
    }
    fields.insert("declaration".into(), json!(asset.text()));
    Ok(out)
}

fn references(index: &Index, name: &str) -> Result<Value, String> {
    let asset = asset(index, name)?;
    let made: Vec<Value> = index
        .references_from(&asset.path)
        .iter()
        .map(|r| {
            let resolved = index.resolve(&r.uid, &r.target).map(|a| a.path.clone());
            json!({ "slot": r.slot, "uid": r.uid, "target": r.target, "resolves_to": resolved })
        })
        .collect();
    let by: Vec<Value> = index
        .referenced_by
        .get(&asset.path)
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .map(|r| json!({ "from": r.from, "slot": r.slot }))
        .collect();
    Ok(json!({ "path": asset.path, "references": made, "referenced_by": by }))
}

fn reference(r: &Reference) -> Value {
    json!({ "from": r.from, "slot": r.slot, "uid": r.uid, "target": r.target })
}

fn problems(index: &Index, limit: usize) -> Value {
    let problems: Vec<Value> = index
        .problems
        .iter()
        .take(limit)
        .map(|p| json!({ "problem": p.describe(), "warning": p.is_warning(), "paths": p.paths() }))
        .collect();
    let dangling: Vec<Value> = index.dangling.iter().take(limit).map(reference).collect();
    json!({
        "problems": problems,
        "problems_total": index.problems.len(),
        "dangling_references": dangling,
        "dangling_total": index.dangling.len(),
    })
}

fn preview(index: &Index, args: &PreviewArgs) -> Result<CallToolResult, String> {
    let asset = asset(index, &args.asset)?;
    let looks = preview::variant_names(asset);
    let look = match &args.variant {
        Some(name) => looks
            .iter()
            .position(|l| l == name)
            .ok_or_else(|| format!("{}: no look is named {name}; its looks: {}", asset.path, looks.join(", ")))?,
        None => 0,
    };
    let image = preview::picture(asset, look)
        .ok_or_else(|| format!("{}: not an image with a picture (a background, a texture, an atlas)", asset.path))?
        .map_err(|e| format!("{e:#}"))?;
    let image = upscale(image, args.scale.unwrap_or(1).clamp(1, 8));
    let png = image.png().map_err(|e| format!("{e:#}"))?;
    let caption = format!(
        "{}: {} x {}{}",
        asset.path,
        image.width,
        image.height,
        looks.get(look).map_or_default(|l| format!(", look {l} of {}", looks.join(", ")))
    );
    Ok(CallToolResult::success(vec![
        ContentBlock::text(caption),
        ContentBlock::image(base64::engine::general_purpose::STANDARD.encode(png), "image/png"),
    ]))
}

/// Each pixel repeated `scale` times both ways.
fn upscale(image: preview::Image, scale: usize) -> preview::Image {
    if scale == 1 {
        return image;
    }
    let (width, height) = (image.width * scale, image.height * scale);
    let mut rgba = Vec::with_capacity(width * height * 4);
    for row in image.rgba.chunks(image.width * 4) {
        let wide: Vec<u8> = row.chunks(4).flat_map(|p| p.repeat(scale)).collect();
        for _ in 0..scale {
            rgba.extend_from_slice(&wide);
        }
    }
    preview::Image { width, height, rgba }
}

fn palette(index: &Index, args: &PaletteArgs) -> Result<Value, String> {
    let asset = asset(index, &args.asset)?;
    let names = preview::palette_names(asset);
    let chosen = names.iter().position(|n| *n == args.palette).ok_or_else(|| {
        format!("{}: no palette is named {}; its palettes: {}", asset.path, args.palette, names.join(", "))
    })?;
    let colors = preview::palette_colors(asset, chosen)
        .ok_or_else(|| format!("{}: palette {} has no stored colors", asset.path, args.palette))?
        .map_err(|e| format!("{e:#}"))?;
    let colors: Vec<Value> = colors
        .iter()
        .map(|&c| {
            let [red, green, blue, alpha] = preview::rgba(c);
            json!({ "abgr1555": format!("{c:#06x}"), "rgba": format!("#{red:02x}{green:02x}{blue:02x}{alpha:02x}") })
        })
        .collect();
    Ok(json!({ "path": asset.path, "palette": args.palette, "colors": colors }))
}

const NO_EDITOR: &str = "the editor does not have the project open";

fn editor_selection(view: &View) -> Result<Value, String> {
    let editor = view.editor.as_ref().ok_or(NO_EDITOR)?;
    let asset = editor.selection.as_deref().and_then(|s| view.index.assets.get(s));
    Ok(json!({ "selected": asset.map(summary), "look": editor.variant }))
}

fn editor_show(view: &mut View, name: &str) -> Result<Value, String> {
    let path = asset(view.index, name)?.path.clone();
    let editor = view.editor.as_mut().ok_or(NO_EDITOR)?;
    editor.show = Some(path.clone());
    Ok(json!({ "shown": path }))
}
