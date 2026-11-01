//! pe2.render: meshes, materials, models and prefabs, and the game's renderer. A mesh is read from a run of a package (`extract`) and decoded from there (`model`). What is drawn is the extension's: its 3D view
//! renders on the editor's device through `wasi:webgpu`, in the document a model opens in, at the size that document is; what is in a model shows in the panes as any asset's parts.

use pe2_ext::tokens::{VALUE, link};
use pe2_ext::ty::{self, Files, Registered};
use pe2_ext::{Asset, Cell, Field, Icon, Name, Registry, Result, Signature, Style, Table, Tint, Ui, Value, View};

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

/// A mesh's stack: its file.
const MESH: &str = "[[stack]]\nop = \"file\"\nfile = \"mesh.usda\"\n\n[[stack]]\nop = \"output\"\nname = \"native\"\nretain = \"cache\"\n";

/// A skeleton's joints by name, in order.
fn joint_names(asset: &Asset) -> Vec<String> {
    let joints = asset.field("joints").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    joints.iter().filter_map(|joint| joint.get("name")?.as_str()).map(str::to_string).collect()
}

/// What a sub-path goes through: a model's mesh and materials, a mesh's skeleton, a skeleton's joints, each joint its place.
fn links(r: &mut Registry) -> (Name, Name, Name, Name) {
    let mesh_of = r.op("mesh_of").label("Its mesh").output::<ty::Table>().run(|call| link(call.host.fields().get("mesh")));
    let skeleton_of = r.op("skeleton_of").label("Its skeleton").output::<ty::Table>().run(|call| link(call.host.fields().get("skeleton")));
    let material_of = r.op("material_of").label("What fills a slot").output::<ty::Table>().parameter::<String>("slot").run(|call| {
        let slot: String = call.parameter()?;
        link(call.host.fields().get("materials").and_then(|materials| materials.get(&slot)))
    });
    let joint_at = r.op("joint_at").label("A joint's place").output::<ty::Table>().parameter::<String>("joint").run(|call| {
        let name: String = call.parameter()?;
        let joints = call.host.fields().get("joints").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
        let at = joints.iter().position(|joint| joint.get("name").and_then(Value::as_str) == Some(&name));
        let at = at.ok_or_else(|| pe2_ext::anyhow!("it has no joint {name}"))?;
        Ok(Table::from_iter([(VALUE.to_string(), Value::Integer(i64::try_from(at)?))]))
    });
    (mesh_of, skeleton_of, material_of, joint_at)
}

#[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
fn register(r: &mut Registry) -> Result<()> {
    let _ = r.value_type::<Usd>();
    let (mesh_of, skeleton_of, material_of, joint_at) = links(r);
    let mesh = r
        .kind("mesh")
        .alias("mesh")
        .label("Mesh")
        .icon(Icon::Polygon, Tint::Picture)
        .stack(MESH)
        .output("skeleton", &skeleton_of, pe2_ext::Retain::Transient)
        .editor("3D", Icon::Polygon, &[], |_| make_view())
        .editor("UV", Icon::Grid, &[], |_| Box::new(uv::UvView::default()))
        .contents(parts::contents)
        .part_sheet(parts::part_sheet)
        .inspector_tab("Mesh", Icon::Polygon, &[], |_| Box::new(Summary::default()))
        .register();
    let _ = r.extractor("meshes", &mesh, &[], extract::begin);
    let material = r
        .kind("material")
        .alias("material")
        .label("Material")
        .icon(Icon::Palette, Tint::Picture)
        .field(Field::new("shader").label("Shader").choices(material::SHADERS.iter().map(|shader| (shader.id, shader.label))))
        .field(Field::new("surface").label("Surface").choices(material::SURFACES))
        .field(Field::new("blend").label("Blend").choices(material::BLENDS.iter().map(|(id, label, _)| (*id, *label))))
        .field(Field::new(material::EFFECT).label("Effect"))
        .field(Field::new(material::MAIN).custom())
        .field(Field::new(material::ENVIRONMENT).custom())
        .editor("Material", Icon::Palette, &[], |_| make_view())
        .contents(parts::contents)
        .inspector("Textures", |_| Box::new(material_panel::Panel::default()))
        .register();
    let _ = r.extractor("materials", &material, &[], extract::authored);
    let skeleton = r
        .kind("skeleton")
        .alias("skeleton")
        .label("Skeleton")
        .icon(Icon::Bone, Tint::Structure)
        .field(Field::new("joints").custom())
        .output("joints", &joint_at, pe2_ext::Retain::Transient)
        .members(|asset| joint_names(asset).into_iter().map(|name| (name.clone(), name)).collect())
        .editor("3D", Icon::Bone, &[], |_| make_view())
        .contents(parts::contents)
        .part_sheet(parts::part_sheet)
        .register();
    let _ = r.extractor("skeletons", &skeleton, &[], extract::authored);
    let model = r
        .kind("model")
        .alias("model")
        .label("Model")
        .icon(Icon::Package, Tint::Picture)
        .field(Field::new("materials").custom())
        .output("mesh", &mesh_of, pe2_ext::Retain::Transient)
        .output("materials", &material_of, pe2_ext::Retain::Transient)
        .members(|asset| asset.parts("materials").into_iter().map(|(name, _)| (name.to_string(), name.to_string())).collect())
        .editor("3D", Icon::Package, &[], |_| make_view())
        .editor("UV", Icon::Grid, &[], |_| Box::new(uv::UvView::default()))
        .contents(parts::contents)
        .part_sheet(parts::part_sheet)
        .inspector("Materials", |_| Box::new(materials::Bindings::default()))
        .inspector_tab("Mesh", Icon::Polygon, &[], |_| Box::new(Summary::default()))
        .register();
    let _ = r.extractor("models", &model, &[], extract::models);
    let prefab = r
        .kind("prefab")
        .alias("prefab")
        .label("Prefab")
        .icon(Icon::Cube, Tint::Picture)
        // Each is a row of the tree or of the Inspector's lists, with its fields there.
        .field(Field::new("parts").custom())
        .field(Field::new("groups").custom())
        .field(Field::new("pose").custom())
        .editor("3D", Icon::Cube, &[], |_| make_view())
        .editor("UV", Icon::Grid, &[], |_| Box::new(uv::UvView::default()))
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
