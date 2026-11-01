//! pe2.render: meshes, materials, models and prefabs, and the game's renderer. A mesh is read from a run of a package (`extract`) and decoded from there (`model`). What is drawn is the extension's: its 3D view
//! renders on the editor's device through `wasi:webgpu`, in the document a model opens in, at the size that document is; what is in a model shows in the panes as any asset's parts.

use pe2_ext::tokens::{VALUE, link};
use pe2_ext::ty::{self, Files, Registered};
use pe2_ext::{Asset, Cell, Field, Icon, Registry, Result, Signature, Style, Table, Tint, Ui, Value, View};

pe2_ext::extension!("pe2.render", "0.1.0", register);

pub(crate) use pe2_model::{material, mesh, model, skeleton};

mod assembly;
#[cfg(any(target_family = "wasm", test))]
#[cfg_attr(not(target_family = "wasm"), expect(dead_code, reason = "tested here, used by the view, which is the component's"))]
mod camera;
mod extract;
#[cfg(any(target_family = "wasm", test))]
#[cfg_attr(not(target_family = "wasm"), expect(dead_code, reason = "tested here, used by the view, which is the component's"))]
mod gizmo;
#[cfg(any(target_family = "wasm", test))]
#[cfg_attr(not(target_family = "wasm"), expect(dead_code, reason = "tested here, used by the view, which is the component's"))]
mod math;
mod material_panel;
mod materials;
mod parts;
mod prefab;
#[cfg(target_family = "wasm")]
mod render;
#[cfg(any(target_family = "wasm", test))]
#[cfg_attr(not(target_family = "wasm"), expect(dead_code, reason = "tested here, used by the view, which is the component's"))]
mod scene;
mod uv;
#[cfg(target_family = "wasm")]
mod view;

/// What is set on a model, and on a prefab.
const MODEL: &str = "pe2.render@model";
const PREFAB: &str = "pe2.render@prefab";

/// A mesh, in USD's text form.
pub struct Usd;

impl Registered for Usd {
    const NAME: &str = "pe2.render@usd";
    const LABEL: &str = "USD text";

    fn signatures() -> Vec<Signature> {
        vec![Signature::Name(mesh::FILE.into())]
    }
}

impl Files for Usd {}

/// What a viewer of a model shows: a mesh, a model with its materials filled or a prefab's models in one tree, with
/// where each transform is kept (`assembly::Assembled`).
pub struct Scene;

impl Registered for Scene {
    const NAME: &str = "pe2.render@scene";
    const LABEL: &str = "Scene";
}

impl Files for Scene {}

/// A mesh's stack: its file.
const MESH: &str = r#"[[stack]]
id = "mesh"
op = "file"
file = "mesh.usda"

[[stack]]
op = "output"
name = "native"
retain = "cache"

[[stack]]
op = "pe2.render@skeleton_of"

[[stack]]
op = "output"
name = "skeleton"

[[stack]]
op = "pe2.render@assemble"
node = { from = "mesh" }

[[stack]]
op = "preview"
name = "scene"
retain = "cache"
"#;

/// A material: what is set on it, shown on a ball.
const MATERIAL: &str = r#"[[stack]]
id = "material"
op = "pe2.render@material"

[[stack]]
op = "pe2.render@assemble"

[[stack]]
op = "preview"
name = "scene"
retain = "cache"
"#;

/// A skeleton: its joints, each a place.
const SKELETON: &str = r#"[[stack]]
id = "skeleton"
op = "pe2.render@skeleton"

[[stack]]
op = "pe2.render@joint_at"

[[stack]]
op = "output"
name = "joints"

[[stack]]
op = "pe2.render@assemble"
node = { from = "skeleton" }

[[stack]]
op = "preview"
name = "scene"
retain = "cache"
"#;

/// A model: a mesh, and what fills its slots.
const MODEL_STACK: &str = r#"[[stack]]
id = "model"
op = "pe2.render@model"

[[stack]]
op = "pe2.render@mesh_of"

[[stack]]
op = "output"
name = "mesh"

[[stack]]
op = "pe2.render@material_of"
model = { from = "model" }

[[stack]]
op = "output"
name = "materials"

[[stack]]
op = "pe2.render@assemble"
node = { from = "model" }

[[stack]]
op = "preview"
name = "scene"
retain = "cache"
"#;

/// A prefab: its models in one tree, and each group's members.
const PREFAB_STACK: &str = r#"[[stack]]
id = "prefab"
op = "pe2.render@prefab"

[[stack]]
op = "pe2.render@assemble"

[[stack]]
op = "preview"
name = "scene"
retain = "cache"

[[stack]]
op = "pe2.render@member"
prefab = { from = "prefab" }

[[stack]]
op = "output"
each = "groups"
"#;

/// A skeleton's joints by name, in order.
fn joint_names(asset: &Asset) -> Vec<String> {
    let joints = asset.field("joints").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    joints.iter().filter_map(|joint| joint.get("name")?.as_str()).map(str::to_string).collect()
}

/// What a sub-path goes through: a model's mesh and materials, a mesh's skeleton, a skeleton's joints, each joint its place.
fn links(r: &mut Registry) {
    let _ = r.op("mesh_of").label("Its mesh").takes("model", &[MODEL]).output::<ty::Table>().run(|call| link(call.given().get("mesh")));
    let _ = r.op("skeleton_of").label("Its skeleton").takes("mesh", &[Usd::NAME]).output::<ty::Table>().run(|call| link(call.given().get("skeleton")));
    let _ = r.op("material_of").label("What fills a slot").takes("model", &[MODEL]).output::<ty::Table>().parameter::<String>("slot").members(|asset| asset.parts("materials").into_iter().map(|(name, _)| (name.to_string(), name.to_string())).collect()).run(|call| {
        let slot: String = call.parameter()?;
        link(call.given().get("materials").and_then(|materials| materials.get(&slot)))
    });
    let _ = r.op("joint_at").label("A joint's place").takes("skeleton", &["pe2.render@skeleton"]).output::<ty::Table>().parameter::<String>("joint").members(|asset| joint_names(asset).into_iter().map(|name| (name.clone(), name)).collect()).run(|call| {
        let name: String = call.parameter()?;
        let given = call.given();
        let joints = given.get("joints").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
        let at = joints.iter().position(|joint| joint.get("name").and_then(Value::as_str) == Some(&name));
        let at = at.ok_or_else(|| pe2_ext::anyhow!("it has no joint {name}"))?;
        Ok(Table::from_iter([(VALUE.to_string(), Value::Integer(i64::try_from(at)?))]))
    });
}

#[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
fn register(r: &mut Registry) -> Result<()> {
    let _ = r.value_type::<Usd>();
    let scene_type = r.value_type::<Scene>();
    r.viewer(&scene_type, "3D", Icon::Cube, |_| make_view());
    r.viewer(&scene_type, "UV", Icon::Grid, |_| Box::new(uv::UvView::default()));
    let shown = [Usd::NAME, "pe2.render@material", "pe2.render@skeleton", MODEL, PREFAB];
    let _ = r.op("assemble").label("What it shows").takes("node", &shown).output::<Scene>().parameter::<String>("chosen").run(assembly::scene);
    links(r);
    let _ = r
        .op("member")
        .label("What a member of a group fills a part with")
        .takes("prefab", &[PREFAB])
        .output::<ty::Table>()
        .parameter::<String>("member")
        .members(|asset| prefab::members(asset).into_iter().map(|address| (address.clone(), address)).collect())
        .run(prefab::member);
    let mesh = r
        .named_type("mesh")
        .alias("mesh")
        .label("Mesh")
        .of(Usd::NAME)
        .icon(Icon::Polygon, Tint::Picture)
        .stack(MESH)
        .takes(&["*.usda"])
        .contents(parts::contents)
        .part_sheet(parts::part_sheet)
        .inspector_tab("Mesh", Icon::Polygon, &[], |_| Box::new(Summary::default()))
        .register();
    let _ = r.extractor("meshes", &mesh, &[], extract::begin);
    let material = r
        .named_type("material")
        .alias("material")
        .label("Material")
        .data()
        .stack(MATERIAL)
        .icon(Icon::Palette, Tint::Picture)
        .field(Field::new("shader").label("Shader").choices(material::SHADERS.iter().map(|shader| (shader.id, shader.label))))
        .field(Field::new("surface").label("Surface").choices(material::SURFACES))
        .field(Field::new("blend").label("Blend").choices(material::BLENDS.iter().map(|(id, label, _)| (*id, *label))))
        .field(Field::new(material::EFFECT).label("Effect"))
        .field(Field::new(material::MAIN).custom())
        .field(Field::new(material::ENVIRONMENT).custom())
        .contents(parts::contents)
        .inspector("Textures", |_| Box::new(material_panel::Panel::default()))
        .register();
    let _ = r.extractor("materials", &material, &[], extract::authored);
    let skeleton = r
        .named_type("skeleton")
        .alias("skeleton")
        .label("Skeleton")
        .data()
        .stack(SKELETON)
        .icon(Icon::Bone, Tint::Structure)
        .field(Field::new("joints").custom())
        .contents(parts::contents)
        .part_sheet(parts::part_sheet)
        .register();
    let _ = r.extractor("skeletons", &skeleton, &[], extract::authored);
    let model = r
        .named_type("model")
        .alias("model")
        .label("Model")
        .data()
        .stack(MODEL_STACK)
        .field(Field::new("mesh").label("Mesh").required())
        .icon(Icon::Package, Tint::Picture)
        .field(Field::new("materials").custom())
        .contents(parts::contents)
        .part_sheet(parts::part_sheet)
        .inspector("Materials", |_| Box::new(materials::Bindings::default()))
        .inspector_tab("Mesh", Icon::Polygon, &[], |_| Box::new(Summary::default()))
        .register();
    let _ = r.extractor("models", &model, &[], extract::models);
    let prefab = r
        .named_type("prefab")
        .alias("prefab")
        .label("Prefab")
        .data()
        .stack(PREFAB_STACK)
        .field(Field::new("skeleton").label("Skeleton").required())
        .icon(Icon::Cube, Tint::Picture)
        // Each is a row of the tree or of the Inspector's lists, with its fields there.
        .field(Field::new("parts").custom())
        .field(Field::new("groups").custom())
        .field(Field::new("pose").custom())
        .contents(parts::contents)
        .part_sheet(parts::part_sheet)
        .content_act(parts::act)
        .inspector("Materials", |_| Box::new(materials::Bindings::default()))
        .inspector_tab("Mesh", Icon::Polygon, &[], |_| Box::new(Summary::default()))
        .register();
    let _ = r.extractor("prefabs", &prefab, &[], extract::authored);
    Ok(())
}

#[cfg(target_family = "wasm")]
fn make_view() -> Box<dyn View> {
    Box::new(view::ModelView::default())
}

#[cfg(not(target_family = "wasm"))]
fn make_view() -> Box<dyn View> {
    Box::new(NoGpu)
}

/// A build linked into the host has no GPU to reach.
#[cfg(not(target_family = "wasm"))]
struct NoGpu;

#[cfg(not(target_family = "wasm"))]
impl View for NoGpu {
    fn show(&mut self, ui: &mut Ui) {
        ui.label("Only a component reaches the GPU");
    }
}

/// What the mesh is, counted: the inspector's.
#[derive(Default)]
struct Summary {
    lines: Option<Vec<(&'static str, String)>>,
}

impl View for Summary {
    fn show(&mut self, ui: &mut Ui) {
        if self.lines.is_none() {
            let Some(asset) = ui.asset() else { return };
            match assembly::assemble(ui.project(), &asset) {
                Ok(assembled) => self.lines = Some(parts::summary(&assembled.model)),
                Err(why) => {
                    ui.error(&format!("{why:#}"));
                    return;
                }
            }
        }
        let rows: Vec<Vec<Cell>> = self.lines.iter().flatten().map(|(label, value)| vec![Cell::Text((*label).to_string(), Style::Weak), Cell::Text(value.clone(), Style::Normal)]).collect();
        ui.section("Mesh", |ui| {
            let _ = ui.table("mesh", &rows);
        });
    }
}
