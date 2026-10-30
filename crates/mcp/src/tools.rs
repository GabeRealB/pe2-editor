//! The tools: their definitions (names, descriptions, input schemas from the argument types) and their answers.

use base64::Engine;
use pe2_ext::Image;
use project::ext::{Extensions, Lens};
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
struct OutputArgs {
    /// The asset: its path or its uid.
    asset: String,
    /// The output, by its address: `stream`, `native`, a family's member by its parameter's value (`picture/night`).
    output: String,
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
    /// The image: its path or its uid; with a fragment, one of its parts (`#variants/night`, `#cells/cursor`,
    /// `#palettes/default`; `get_asset` lists them).
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
struct NoArgs {}

/// What of the editor's window to read.
#[derive(Deserialize, JsonSchema)]
pub struct UiArgs {
    /// Only this pane (its tab's title: `Inspector`, `Document 2`); the whole window when absent.
    #[serde(default)]
    pub pane: Option<String>,
    /// Only widgets whose label or value contains this text (case-insensitive), each with the labels it is within.
    #[serde(default)]
    pub text: Option<String>,
    /// Give each widget's rectangle, in the window's points (`@x,y wxh`), for `editor_act`'s `at`.
    #[serde(default)]
    pub bounds: bool,
}

/// Something to do to a widget, or at a point of the window.
#[derive(Deserialize, JsonSchema)]
pub struct ActArgs {
    /// The widget, by the id `editor_ui` gives it (`#3fa2c4d1`, with or without the `#`).
    #[serde(default)]
    pub node: Option<String>,
    /// A point of the window in points, instead of a widget: `[x, y]`.
    #[serde(default)]
    pub at: Option<[f32; 2]>,
    /// `click` (the default), `double_click`, `right_click`, `hover`, `focus`, `scroll_into_view`, `drag` (to `to`)
    /// or `scroll` (by `by`).
    #[serde(default)]
    pub action: Option<String>,
    /// Where a drag ends: `[x, y]` in points.
    #[serde(default)]
    pub to: Option<[f32; 2]>,
    /// How far to scroll, in points: `[dx, dy]`, positive moving the content right and down.
    #[serde(default)]
    pub by: Option<[f32; 2]>,
}

/// Text or keys to send to the editor.
#[derive(Deserialize, JsonSchema)]
pub struct TypeArgs {
    /// Text to type into the focused widget.
    #[serde(default)]
    pub text: Option<String>,
    /// Keys to press after it, in order: a key's name (`Enter`, `Escape`, `Tab`, `ArrowDown`, `A`, `F2`), with
    /// modifiers before it (`ctrl+A`, `shift+Tab`, `ctrl+shift+Z`; `cmd` is Ctrl outside macOS).
    #[serde(default)]
    pub keys: Vec<String>,
    /// Focus this widget first (an id from `editor_ui`).
    #[serde(default)]
    pub node: Option<String>,
}

/// What of the window to capture.
#[derive(Deserialize, JsonSchema)]
pub struct ScreenshotArgs {
    /// Only this pane (its tab's title); the whole window when absent.
    #[serde(default)]
    pub pane: Option<String>,
}

fn tool<T: JsonSchema + 'static>(name: &'static str, description: &'static str) -> Tool {
    let annotations = ToolAnnotations::new().read_only(true);
    Tool::new(name, description, Map::new()).with_input_schema::<T>().annotate(annotations)
}

/// A tool acting in the user's editor: what it does may change the project, through the editor's undoable edits.
fn acting<T: JsonSchema + 'static>(name: &'static str, description: &'static str) -> Tool {
    let annotations = ToolAnnotations::new().read_only(false).destructive(true).idempotent(false);
    Tool::new(name, description, Map::new()).with_input_schema::<T>().annotate(annotations)
}

/// The tools that use the editor's window: only the editor answers them, across frames.
pub const EDITOR_UI: [&str; 4] = ["editor_ui", "editor_act", "editor_type", "editor_screenshot"];

/// An extension's tool as agents see it: by its name within its extension (`palette`), its arguments as it declares
/// them.
fn extension_tool(tool: &pe2_ext::ToolDef) -> Tool {
    let schema: Map<String, Value> = serde_json::from_str(&tool.schema).unwrap_or_default();
    let annotations = ToolAnnotations::new().read_only(true);
    Tool::new(tool.name.local().to_string(), tool.description.clone(), schema).annotate(annotations)
}

/// Every tool: the server's own, then the extensions'.
#[must_use]
pub fn definitions() -> Vec<Tool> {
    let mut tools = own();
    tools.extend(extensions::shared().tools().iter().map(extension_tool));
    tools
}

fn own() -> Vec<Tool> {
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
             sha1, layer, the operation that made it), its looks where it has a picture, its outputs (what its stack \
             and its kind make of it, each read with get_output), its import and the imports that fit its source, \
             and its declaration (asset.toml) as written - with the settings as resolved where its import derives \
             some from its source - and its parts (what references name by fragment, a virtual one with the parts it \
             is made of).",
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
        tool::<OutputArgs>(
            "get_output",
            "One of an asset's outputs (get_asset lists them), made now or found kept: a table (a movie's facts) as \
             JSON; a picture as PNG; a value of an extension's type as its files (name, size, sha1) and the settings \
             made with it; a stream (a movie's frames) as its header. A family's member by its parameter's value: \
             `picture/default`, `part/palettes/default`."
        ),
        tool::<PreviewArgs>(
            "preview",
            "An image's picture as PNG - a background, a texture through a palette, an atlas with each cell through \
             its own - in one of its looks; or one of its parts by fragment (asset#variants/night).",
        ),
        tool::<NoArgs>(
            "editor_selection",
            "The asset selected in the editor and the look it is shown in. Only when the editor has the project open.",
        ),
        tool::<AssetArgs>(
            "editor_show",
            "Select an asset in the editor and reveal it in its tree, for the user to look at. Changes only what the \
             editor shows. Only when the editor has the project open.",
        ),
        tool::<UiArgs>(
            "editor_ui",
            "The editor's window as its widgets: an indented tree of each widget's role, label, value and state \
             (disabled, checked, selected, focused, expanded), with an id (`#3fa2c4d1`) to act on it by; the panes \
             open. Of the main window, one pane or the widgets matching some text. Only when the editor has the \
             project open.",
        ),
        acting::<ActArgs>(
            "editor_act",
            "Act in the editor as the user would: click, double-click, right-click, hover, focus or scroll to a \
             widget by its id from editor_ui; or click, drag or scroll at a point. Returns what changed in the \
             window's widgets once the editor has drawn the result. Edits it makes are the editor's own, undoable \
             in its history. Only when the editor has the project open and the user lets agents use it (its \
             settings, Agents).",
        ),
        acting::<TypeArgs>(
            "editor_type",
            "Type text and press keys in the editor, into the focused widget or one focused first. A text field \
             commits on Enter. Returns what changed. Only when the editor has the project open and the user lets \
             agents use it.",
        ),
        tool::<ScreenshotArgs>(
            "editor_screenshot",
            "The editor's window as the user sees it, as PNG: the main window, or one pane of it. Only when the \
             editor has the project open.",
        ),
    ]
}

/// A tool's answer.
pub fn call(view: &mut View, name: &str, arguments: &Map<String, Value>) -> CallToolResult {
    let result = match name {
        "project" => Ok(project(view)),
        "find_assets" => parse(arguments).map(|a| find_assets(view.index, &a)),
        "list_folder" => parse(arguments).map(|a: FolderArgs| list_folder(view.index, a.path.as_deref())),
        "get_asset" => parse(arguments).and_then(|a: AssetArgs| get_asset(view.index, view.extensions, &a.asset)),
        "references" => parse(arguments).and_then(|a: AssetArgs| references(view.index, &a.asset)),
        "problems" => parse(arguments).map(|a: ProblemsArgs| problems(view.index, a.limit.unwrap_or(100))),
        "preview" => {
            return parse(arguments).and_then(|a| preview(view.index, view.extensions, &a)).unwrap_or_else(failure);
        }
        "get_output" => {
            return parse(arguments).and_then(|a| get_output(view.index, view.extensions, &a)).unwrap_or_else(failure);
        }
        "editor_selection" => editor_selection(view),
        "editor_show" => parse(arguments).and_then(|a: AssetArgs| editor_show(view, &a.asset)),
        name if EDITOR_UI.contains(&name) => Err(NO_EDITOR.to_string()),
        _ => run_extension_tool(view, name, arguments),
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
        return index.assets.get(path).map(|a| &**a).ok_or_else(|| format!("{name}: no asset"));
    }
    let path = name.trim_start_matches("res://").trim_end_matches('/');
    let path = path.strip_suffix(project::index::BUNDLE_SUFFIX).unwrap_or(path);
    index.assets.get(path).map(|a| &**a).ok_or_else(|| {
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
        .map(|a| &**a)
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

fn get_asset(index: &Index, extensions: &Extensions, name: &str) -> Result<Value, String> {
    let asset = asset(index, name)?;
    let files: Vec<Value> = asset
        .files
        .iter()
        .map(|f| json!({ "name": f.name, "size": f.size, "sha1": f.sha1, "layer": f.layer, "made_by": f.made_by }))
        .collect();
    let mut out = summary(asset);
    let fields = out.as_object_mut().expect("an object");
    fields.insert("layers".into(), json!(asset.layers));
    fields.insert("files".into(), json!(files));
    if extensions.has_picture(asset) {
        fields.insert("looks".into(), json!(extensions.looks(asset)));
        fields.insert("has_picture".into(), json!(true));
    }
    // What it makes: its stack's outputs and its kind's, each with how it is kept.
    let (plan, wrong) = extensions.plan(asset);
    let outputs: Vec<Value> = plan
        .outputs
        .iter()
        .map(|o| {
            let name = o.parameter.as_ref().map_or_else(|| o.name.clone(), |p| format!("{}({p})", o.name));
            json!({ "name": name, "retain": project::graph::retain_name(o.retain), "eager": o.eager })
        })
        .collect();
    if !outputs.is_empty() {
        fields.insert("outputs".into(), json!(outputs));
    }
    if !wrong.is_empty() {
        fields.insert("stack_problems".into(), json!(wrong.iter().map(|p| format!("{}: {}", p.at, p.what)).collect::<Vec<_>>()));
    }
    if let Some(op) = extensions.import_op(asset) {
        fields.insert("import".into(), json!({ "name": op.name.to_string(), "label": op.label }));
    }
    let fits: Vec<Value> =
        extensions.ops_for(asset).into_iter().map(|op| json!({ "name": op.name.to_string(), "label": op.label })).collect();
    if !fits.is_empty() {
        fields.insert("imports".into(), json!(fits));
    }
    let parts: Vec<Value> = extensions
        .parts(asset)
        .into_iter()
        .map(|p| {
            let made_of = (!p.made_of.is_empty()).then_some(p.made_of);
            json!({ "id": p.id, "label": p.label, "made_of": made_of })
        })
        .collect();
    if !parts.is_empty() {
        fields.insert("parts".into(), json!(parts));
    }
    if let Some(error) = &asset.error {
        fields.insert("error".into(), json!(error));
    }
    fields.insert("declaration".into(), json!(asset.text()));
    // What readers see where an import derives some of it from the source: the declaration with those in place.
    if let Some(imported) = &asset.imported {
        fields.insert("settings".into(), json!(asset.settings_text()));
        if !imported.overridden.is_empty() {
            fields.insert("overridden_by_import".into(), json!(imported.overridden));
        }
        if !imported.orphans.is_empty() {
            fields.insert("orphaned".into(), json!(imported.orphans));
        }
    }
    Ok(out)
}

fn references(index: &Index, name: &str) -> Result<Value, String> {
    let asset = asset(index, name)?;
    let made: Vec<Value> = index
        .references_from(&asset.path)
        .iter()
        .map(|r| {
            let resolved = index.resolve(&r.uid, &r.target).map(|a| a.path.clone());
            json!({ "slot": r.slot, "uid": r.uid, "part": r.part, "target": (!r.target.is_empty()).then_some(&r.target), "resolves_to": resolved })
        })
        .collect();
    let by: Vec<Value> = index
        .referenced_by
        .get(&asset.path)
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .map(|r| json!({ "from": r.from, "slot": r.slot, "part": r.part }))
        .collect();
    Ok(json!({ "path": asset.path, "references": made, "referenced_by": by }))
}

fn reference(r: &Reference) -> Value {
    json!({ "from": r.from, "slot": r.slot, "uid": r.uid, "target": (!r.target.is_empty()).then_some(&r.target) })
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

fn get_output(index: &Index, extensions: &Extensions, args: &OutputArgs) -> Result<CallToolResult, String> {
    use project::graph::{Content, Value as Made};
    let asset = asset(index, &args.asset)?;
    let engine = extensions.engine();
    // A stream is opened, not made: what it is, its header.
    if extensions.stream_output(asset).as_deref() == Some(args.output.as_str()) {
        let stream = extensions.stream(&engine, index, asset, &args.output).map_err(|e| format!("{e:#}"))?;
        let header = serde_json::to_value(stream.header()).unwrap_or_default();
        return Ok(CallToolResult::success(vec![ContentBlock::text(json!({ "stream": header }).to_string())]));
    }
    // The thumbnail is of the first look, which it is given when asked for.
    let given = if args.output == project::graph::THUMBNAIL {
        extensions.thumbnail(&engine, asset)
    } else {
        extensions.output(&engine, index, asset, &args.output, true)
    };
    let given = given.map_err(|e| format!("{e:#}"))?;
    let value = given.value.ok_or_else(|| format!("{}: {} made nothing", asset.path, args.output))?;
    let text = |v: Value| Ok(CallToolResult::success(vec![ContentBlock::text(v.to_string())]));
    let file = |f: &project::graph::File| {
        let (size, sha1) = match &f.content {
            Content::Inline(bytes) => (bytes.len() as u64, project::extract::sha1_hex(bytes)),
            Content::Stored { sha1, size } => (*size, sha1.clone()),
        };
        json!({ "name": f.name, "size": size, "sha1": sha1 })
    };
    match value.as_ref() {
        Made::Image(image) => {
            let png = project::ext::png(image).map_err(|e| format!("{e:#}"))?;
            Ok(CallToolResult::success(vec![
                ContentBlock::text(format!("{} {}: {} x {}", asset.path, args.output, image.width, image.height)),
                ContentBlock::image(base64::engine::general_purpose::STANDARD.encode(png), "image/png"),
            ]))
        }
        Made::Table(table) => text(serde_json::to_value(table).unwrap_or_default()),
        Made::Typed(typed) => text(json!({
            "type": typed.ty,
            "also": typed.also,
            "files": typed.files.iter().map(file).collect::<Vec<_>>(),
            "settings": serde_json::to_value(&typed.settings).unwrap_or_default(),
            "notes": typed.notes,
        })),
        Made::Pcm(pcm) => text(json!({ "rate": pcm.rate, "channels": pcm.channels, "samples": pcm.samples.len() })),
        Made::List(items) => text(json!({ "items": items.len(), "type": value.ty().to_string() })),
        Made::Stream(stream) => text(json!({ "stream": serde_json::to_value(stream.header()).unwrap_or_default() })),
        Made::Marks(marks) => text(json!({
            "marks": marks.boxes.iter().map(|m| json!({
                "name": m.name, "x": m.rect.x, "y": m.rect.y, "width": m.rect.width, "height": m.rect.height,
            })).collect::<Vec<_>>(),
            "summary": marks.summary,
        })),
        Made::Video(frame) => text(json!({
            "video": frame.format.name(), "width": frame.width, "height": frame.height,
            "timestamp": frame.timestamp, "bytes": frame.buffer.size(),
        })),
        Made::Audio(sound) => text(json!({
            "audio": sound.format.name(), "sample_rate": sound.sample_rate, "channels": sound.channels,
            "timestamp": sound.timestamp, "bytes": sound.buffer.size(),
        })),
    }
}

fn preview(index: &Index, extensions: &Extensions, args: &PreviewArgs) -> Result<CallToolResult, String> {
    // A part, by its fragment: `uid://...#variants/night`, `path#cells/cursor`.
    if let Some((name, part)) = args.asset.split_once('#') {
        let asset = asset(index, name)?;
        let ids: Vec<String> = extensions.parts(asset).into_iter().map(|p| p.id).collect();
        let image = extensions
            .part(asset, part)
            .ok_or_else(|| format!("{}: no part {part} with a picture; its parts: {}", asset.path, ids.join(", ")))?
            .map_err(|e| format!("{e:#}"))?;
        let image = upscale(image, args.scale.unwrap_or(1).clamp(1, 8));
        let png = project::ext::png(&image).map_err(|e| format!("{e:#}"))?;
        let caption = format!("{}#{part}: {} x {}", asset.path, image.width, image.height);
        return Ok(CallToolResult::success(vec![
            ContentBlock::text(caption),
            ContentBlock::image(base64::engine::general_purpose::STANDARD.encode(png), "image/png"),
        ]));
    }
    let asset = asset(index, &args.asset)?;
    let looks = extensions.looks(asset);
    let look = match &args.variant {
        Some(name) => looks
            .iter()
            .position(|l| l == name)
            .ok_or_else(|| format!("{}: no look is named {name}; its looks: {}", asset.path, looks.join(", ")))?,
        None => 0,
    };
    let image = extensions
        .picture(asset, look)
        .ok_or_else(|| format!("{}: not an image with a picture (a background, a texture, an atlas)", asset.path))?
        .map_err(|e| format!("{e:#}"))?;
    let image = upscale(image, args.scale.unwrap_or(1).clamp(1, 8));
    let png = project::ext::png(&image).map_err(|e| format!("{e:#}"))?;
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
fn upscale(image: Image, scale: usize) -> Image {
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
    Image { width, height, rgba }
}

/// An extension's tool, by its name within its extension: its arguments as TOML, its answer back as JSON.
fn run_extension_tool(view: &View, name: &str, arguments: &Map<String, Value>) -> Result<Value, String> {
    let extensions = view.extensions;
    let tool = extensions.tools().iter().find(|t| t.name.local() == name).ok_or_else(|| format!("no tool is named {name}"))?;
    let arguments: toml::Value =
        serde_json::from_value(Value::Object(arguments.clone())).map_err(|e| format!("arguments: {e}"))?;
    let selection = view.editor.as_ref().and_then(|e| e.selection.as_deref());
    let lens = Lens { index: view.index, extensions, selection, looks: None };
    let answer = (tool.run)(&lens, &arguments).map_err(|e| format!("{e:#}"))?;
    serde_json::to_value(answer).map_err(|e| e.to_string())
}

const NO_EDITOR: &str = "the editor does not have the project open";

fn editor_selection(view: &View) -> Result<Value, String> {
    let editor = view.editor.as_ref().ok_or(NO_EDITOR)?;
    let asset = editor.selection.as_deref().and_then(|s| view.index.assets.get(s)).map(|a| &**a);
    Ok(json!({ "selected": asset.map(summary), "look": editor.variant }))
}

fn editor_show(view: &mut View, name: &str) -> Result<Value, String> {
    let path = asset(view.index, name)?.path.clone();
    let editor = view.editor.as_mut().ok_or(NO_EDITOR)?;
    editor.show = Some(path.clone());
    Ok(json!({ "shown": path }))
}
