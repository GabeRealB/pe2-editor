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
use std::sync::Arc;

use crate::View;

/// An asset named by its path (`stages/akropolis/rooms/cafeteria/props/puff`, with or without `res://` and `.node`)
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
    /// The output, by its address: `stream`, `native`, a family's member by its parameter's value (`picture/night`),
    /// and how it is asked for after a `?` where it takes a query (`cells/body?palette=night`).
    output: String,
}

#[derive(Deserialize, JsonSchema)]
struct FindArgs {
    /// Text the asset's path contains (case-insensitive).
    #[serde(default)]
    text: Option<String>,
    /// Only assets of this type (`image`, `file`, `folder`, `vram_slot`, ...).
    #[serde(default, rename = "type")]
    node_type: Option<String>,
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
    /// The image: its path or its uid, and one of its outputs - a reference to it (`uid://.../cells/cursor`,
    /// `uid://.../cells/body?palette=night`), or after a path's `#` (`#picture/night`, `#palettes/default`;
    /// `get_asset` lists them).
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

/// A command to run.
#[derive(Deserialize, JsonSchema)]
pub struct RunArgs {
    /// Its id, from `editor_commands` (`edit.undo`, `pane.assets`).
    pub command: String,
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
pub const EDITOR_UI: [&str; 6] =
    ["editor_ui", "editor_act", "editor_type", "editor_screenshot", "editor_commands", "editor_run"];

/// An extension's tool as agents see it: by its name within its extension (`palette`), its arguments as it declares
/// them.
fn extension_tool(tool: &pe2_ext::ToolDef) -> Tool {
    let schema: Map<String, Value> = serde_json::from_str(&tool.schema).unwrap_or_default();
    let annotations = ToolAnnotations::new().read_only(true);
    Tool::new(tool.name.local().to_string(), tool.description.clone(), schema).annotate(annotations)
}

/// Every tool: the server's own, then the extensions' - those every project has, as this program has them.
#[must_use]
pub fn definitions() -> Vec<Tool> {
    with(extensions::shared().tools().iter().map(extension_tool).collect())
}

/// Every tool, the extensions' being these: the server's own, then theirs.
#[must_use]
pub fn with(theirs: Vec<Tool>) -> Vec<Tool> {
    let mut tools = vec![editors()];
    tools.extend(own());
    tools.extend(theirs);
    tools
}

/// The tools of a project's extensions, as loaded for it.
pub(crate) fn of(extensions: &project::ext::Extensions) -> Vec<Tool> {
    extensions.tools().iter().map(extension_tool).collect()
}

/// The tool listing the editors on this computer, which needs no project (`presence`).
fn editors() -> Tool {
    tool::<NoArgs>(
        "editors",
        "The editors running on this computer and what each is doing: its process, the project it has open or is \
             opening, what it is busy with (taking in, an epoch, requests), whether the project asks something \
             unanswered, its error, what it fetches, its last epoch, its problems, its window (focused, in the \
             background, minimized), whether agents may use it, and whether its program was rebuilt since it started. \
             And of this server: who holds its project now, and whether its own program was rebuilt since it started \
             - a server that was runs older code, and should be restarted. Answered without the project: it never \
             waits for it or takes it.",
    )
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
            "Find assets by text in their path, type or folder. Returns each one's path, uid and type, \
             and how many matched in all.",
        ),
        tool::<FolderArgs>(
            "list_folder",
            "The entries of a folder of the resolved project: its folders, its assets (with their types) and its \
             plain files.",
        ),
        tool::<AssetArgs>(
            "get_asset",
            "An asset as resolved: path, uid, type, the layers creating and overriding it, its files (size, \
             sha1, layer, the operation that made it), its looks where it has a picture, its outputs (what its stack \
             and its type make of it, each read with get_output), its import and the imports that fit its source, \
             and its declaration (node.toml) as written - with the settings as resolved where its import derives \
             some from its source - and its addresses (what references name after a `#`\
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
             `picture/default`, `palettes/default`.",
        ),
        tool::<PreviewArgs>(
            "preview",
            "An image's picture as PNG - a background, a texture through a palette, an atlas with each cell through \
             its own - in one of its looks; or one of its outputs (asset#picture/night).",
        ),
        tool::<NoArgs>(
            "editor_selection",
            "The asset selected in the editor and the look it is shown in. Only when the editor has the project open.",
        ),
        tool::<NoArgs>(
            "editor_performance",
            "What the editor costs, each second: frames and their time, CPU (100 is one core), memory, the GPU's use \
             and memory, the window, what draws it; and its time since it started by span (operations, instances, the \
             index, the store), costliest first, summed over threads. Only when the editor has the project open.",
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
        tool::<NoArgs>(
            "editor_commands",
            "Every command of the editor: its id, its name, what it is bound to and whether it can run now - everything the \
             menus and the command palette offer, what extensions add among them. Only when the editor has the project open.",
        ),
        acting::<RunArgs>(
            "editor_run",
            "Run a command of the editor by its id, as the user would from its palette; what it edits is undoable in its \
             history. Only when the editor has the project open and the user lets agents use it (its settings, Agents).",
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
        "editors" => Ok(crate::presence::report(&view.project.root)),
        "project" => Ok(project(view)),
        "find_assets" => parse(arguments).and_then(|a| find_assets(view.index, view.extensions, &a)),
        "list_folder" => {
            parse(arguments).and_then(|a: FolderArgs| list_folder(view.index, view.extensions, a.path.as_deref()))
        }
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
        "editor_performance" => {
            view.editor.as_ref().map(|editor| editor.performance.clone()).ok_or_else(|| NO_EDITOR.to_string())
        }
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

/// What an agent names, read once as it comes in: an asset - by its uid, its path, or its path as a place (`res://`,
/// `.node` allowed) - and the output of it named, where one is (`uid://.../cells/cursor`, `<path>#cells/cursor`).
struct Named<'t> {
    text: &'t str,
    asset: pe2_path::Reference,
    asked: Option<Arc<pe2_path::Asked>>,
}

fn named(text: &str) -> Result<Named<'_>, String> {
    let text = text.trim();
    let refused = |e: pe2_path::PathError| format!("{text}: {e}");
    if text.starts_with("uid://") {
        let named = pe2_path::ResourcePath::new(text).map_err(refused)?;
        return Ok(Named { text, asset: pe2_path::Reference::Uid(named.uid), asked: named.asked });
    }
    let (path, asked) = match text.split_once('#') {
        Some((path, asked)) => (path, Some(Arc::from(pe2_path::Asked::new(asked).map_err(refused)?))),
        None => (text, None),
    };
    let path = if path.starts_with(pe2_path::RES_PREFIX) {
        project::TreePath::from_res(path)
    } else {
        project::TreePath::new(path.strip_suffix('/').unwrap_or(path))
    };
    let path = project::index::asset_named(path.map_err(refused)?).map_err(refused)?;
    Ok(Named { text, asset: pe2_path::Reference::Path(path), asked })
}

/// The asset an agent names, where it names no output of it.
fn asset<'a>(index: &'a Index, name: &str) -> Result<&'a Asset, String> {
    let named = named(name)?;
    if named.asked.is_some() {
        return Err(format!("{}: names an output of an asset, where the asset is asked for", named.text));
    }
    asset_of(index, &named)
}

/// The asset an agent's name leads to.
fn asset_of<'a>(index: &'a Index, named: &Named<'_>) -> Result<&'a Asset, String> {
    let text = named.text;
    let path = match &named.asset {
        pe2_path::Reference::Uid(uid) => {
            index.by_uid.get(uid).ok_or_else(|| format!("{text}: no asset has this uid"))?
        }
        pe2_path::Reference::Path(path) => path,
    };
    index
        .assets
        .get(path)
        .map(|a| &**a)
        .ok_or_else(|| format!("{text}: no asset has this path (find_assets searches by text)"))
}

/// A path of the tree an agent names, checked as it comes in.
fn tree(text: &str) -> Result<Arc<project::TreePath>, String> {
    (project::TreePath::new(text).map_err(|e| format!("{text}: {e}"))).map(ToOwned::to_owned)
}

fn summary(extensions: &Extensions, asset: &Asset) -> Value {
    json!({ "path": asset.path, "uid": asset.uid, "type": extensions.type_name(asset) })
}

fn project(view: &View) -> Value {
    let root = &view.project.root;
    let layers: Vec<Value> = view
        .index
        .layers()
        .iter()
        .zip(&view.project.layers)
        .map(|(info, layer)| {
            let path = layer.folder().strip_prefix(root).unwrap_or(layer.folder());
            let holds = match info.kind {
                project::LayerKind::Assets => "assets",
                project::LayerKind::Files => "files",
                project::LayerKind::Generated => "generated",
            };
            json!({
                "name": info.name,
                "kind": holds,
                "fills": info.fills,
                "mounted_at": (!info.at.is_root()).then(|| info.at.to_string()),
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

/// Whether an asset is of a type, named qualified or by its own name alone.
fn is_of(extensions: &Extensions, asset: &Asset, wanted: &str) -> bool {
    extensions.node_types_of(asset).iter().any(|known| known.name.to_string() == wanted || known.name.local() == wanted)
}

fn find_assets(index: &Index, extensions: &Extensions, args: &FindArgs) -> Result<Value, String> {
    let text = args.text.as_deref().map(str::to_lowercase);
    let under = args.under.as_deref().map(|under| tree(under.trim_matches('/'))).transpose()?;
    let matches: Vec<&Asset> = index
        .assets
        .values()
        .map(|a| &**a)
        .filter(|a| text.as_ref().is_none_or(|t| a.path.as_str().to_lowercase().contains(t.as_str())))
        .filter(|a| args.node_type.as_ref().is_none_or(|wanted| is_of(extensions, a, wanted)))
        .filter(|a| under.as_ref().is_none_or(|under| a.path.starts_with(under)))
        .collect();
    let (offset, limit) = (args.offset.unwrap_or(0), args.limit.unwrap_or(50).min(500));
    let page: Vec<Value> = matches.iter().skip(offset).take(limit).map(|a| summary(extensions, a)).collect();
    Ok(json!({ "total": matches.len(), "offset": offset, "assets": page }))
}

fn list_folder(index: &Index, extensions: &Extensions, path: Option<&str>) -> Result<Value, String> {
    let folder = tree(path.unwrap_or_default().trim_matches('/'))?;
    let (mut folders, mut assets, mut files) = (vec![], vec![], vec![]);
    // Paths order name by name: what is under it follows it, before anything that is not.
    let after = index
        .entries
        .range::<project::TreePath, _>(pe2_path::from(&*folder))
        .take_while(|(key, _)| key.starts_with(&folder));
    for (key, entry) in after {
        let Some(name) = key.strip_prefix(&folder) else { continue };
        if name.parts().count() != 1 {
            continue;
        }
        match entry.kind {
            Kind::Folder => folders.push(json!(name.as_str())),
            Kind::File => files.push(json!(name.as_str())),
            Kind::Asset => {
                if let Some(asset) = index.assets.get(&project::index::asset_of(key)) {
                    assets.push(summary(extensions, asset));
                }
            }
        }
    }
    Ok(json!({ "path": folder.as_str(), "folders": folders, "assets": assets, "files": files }))
}

fn get_asset(index: &Index, extensions: &Extensions, name: &str) -> Result<Value, String> {
    let asset = asset(index, name)?;
    let files: Vec<Value> = asset
        .files
        .iter()
        .map(|f| json!({ "name": f.name, "size": f.size, "sha1": f.sha1, "layer": f.layer, "made_by": f.made_by }))
        .collect();
    let mut out = summary(extensions, asset);
    let fields = out.as_object_mut().expect("an object");
    fields.insert("layers".into(), json!(asset.layers));
    fields.insert("files".into(), json!(files));
    if extensions.has_picture(asset) {
        fields.insert("looks".into(), json!(extensions.looks(asset)));
        fields.insert("has_picture".into(), json!(true));
    }
    // What it makes: its stack's outputs and its type's, each with how it is kept.
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
        fields.insert(
            "stack_problems".into(),
            json!(wrong.iter().map(|p| format!("{}: {}", p.at, p.what)).collect::<Vec<_>>()),
        );
    }
    if let Some(op) = extensions.import_op(asset) {
        fields.insert("import".into(), json!({ "name": op.name.to_string(), "label": op.label }));
    }
    let fits: Vec<Value> = extensions
        .ops_for(asset)
        .into_iter()
        .map(|op| json!({ "name": op.name.to_string(), "label": op.label }))
        .collect();
    if !fits.is_empty() {
        fields.insert("imports".into(), json!(fits));
    }
    let addresses: Vec<Value> = extensions
        .addresses(asset)
        .into_iter()
        .map(|(address, label)| json!({ "address": address, "label": label }))
        .collect();
    if !addresses.is_empty() {
        fields.insert("addresses".into(), json!(addresses));
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
            let resolved = index.resolve(&r.uid, r.target.as_deref()).map(|a| a.path.clone());
            let output = r.output.as_ref().map(ToString::to_string);
            json!({ "slot": r.slot, "uid": r.uid_text(), "output": output, "target": r.target, "resolves_to": resolved })
        })
        .collect();
    let by: Vec<Value> = index
        .referenced_by
        .get(&asset.path)
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .map(|r| json!({ "from": r.from, "slot": r.slot, "output": r.output.as_ref().map(ToString::to_string) }))
        .collect();
    Ok(json!({ "path": asset.path, "references": made, "referenced_by": by }))
}

fn reference(r: &Reference) -> Value {
    json!({ "from": r.from, "slot": r.slot, "uid": r.uid_text(), "target": r.target })
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
    let output = pe2_path::Asked::new(&args.output).map_err(|e| format!("{e}"))?;
    let engine = extensions.engine();
    // A stream is opened, not made: what it is, its header.
    if extensions.stream_output(asset).as_deref() == Some(args.output.as_str()) {
        let stream = extensions.stream(&engine, index, asset, output).map_err(|e| format!("{e:#}"))?;
        let header = serde_json::to_value(stream.header()).unwrap_or_default();
        return Ok(CallToolResult::success(vec![ContentBlock::text(json!({ "stream": header }).to_string())]));
    }
    // The thumbnail is of the first look, which it is given when asked for.
    let given = if args.output == project::graph::THUMBNAIL {
        extensions.thumbnail(&engine, asset)
    } else {
        extensions.output(&engine, index, asset, output, true)
    };
    let given = given.map_err(|e| format!("{e:#}"))?;
    let value = given.value.ok_or_else(|| format!("{}: {} made nothing", asset.path, args.output))?;
    let text = |v: Value| Ok(CallToolResult::success(vec![ContentBlock::text(v.to_string())]));
    let file = |f: &project::graph::File| {
        let (size, sha1) = match &f.content {
            Content::Stored { sha1, size } => (*size, *sha1),
            held => (held.size(), project::extract::sha1_of(held.held().unwrap_or_default())),
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
    // An output: a reference naming one (`uid://.../picture/night`), or one after a path's `#` (`path#cells/cursor`).
    let named = named(&args.asset)?;
    let (named, output) = (asset_of(index, &named)?, named.asked);
    if let Some(output) = output {
        let asset = named;
        let ids: Vec<String> = extensions.addresses(asset).into_iter().map(|(address, _)| address).collect();
        let image = extensions
            .output_image(asset, &output)
            .ok_or_else(|| {
                format!("{}: no output {output} that is a picture; its outputs: {}", asset.path, ids.join(", "))
            })?
            .map_err(|e| format!("{e:#}"))?;
        let image = upscale(image, args.scale.unwrap_or(1).clamp(1, 8));
        let png = project::ext::png(&image).map_err(|e| format!("{e:#}"))?;
        let caption = format!("{}#{output}: {} x {}", asset.path, image.width, image.height);
        return Ok(CallToolResult::success(vec![
            ContentBlock::text(caption),
            ContentBlock::image(base64::engine::general_purpose::STANDARD.encode(png), "image/png"),
        ]));
    }
    let asset = named;
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
    let tool =
        extensions.tools().iter().find(|t| t.name.local() == name).ok_or_else(|| format!("no tool is named {name}"))?;
    let arguments: toml::Value =
        serde_json::from_value(Value::Object(arguments.clone())).map_err(|e| format!("arguments: {e}"))?;
    let selection = view.editor.as_ref().and_then(|e| e.selection.as_deref());
    let lens = Lens { index: view.index, extensions, selection, chosen: None, revision: 0 };
    let answer = (tool.run)(&lens, &arguments).map_err(|e| format!("{e:#}"))?;
    serde_json::to_value(answer).map_err(|e| e.to_string())
}

const NO_EDITOR: &str = "the editor does not have the project open";

fn editor_selection(view: &View) -> Result<Value, String> {
    let editor = view.editor.as_ref().ok_or(NO_EDITOR)?;
    let asset = editor.selection.as_ref().and_then(|s| view.index.assets.get(s)).map(|a| &**a);
    Ok(json!({ "selected": asset.map(|asset| summary(view.extensions, asset)), "look": editor.variant }))
}

fn editor_show(view: &mut View, name: &str) -> Result<Value, String> {
    let path = asset(view.index, name)?.path.clone();
    let editor = view.editor.as_mut().ok_or(NO_EDITOR)?;
    editor.show = Some(path.clone());
    Ok(json!({ "shown": path.as_str() }))
}
