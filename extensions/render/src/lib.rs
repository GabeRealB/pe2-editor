//! pe2.render: meshes, materials, models and prefabs, and the game's renderer. A mesh is read from a run of a package (`extract`) and decoded from there (`model`). What is drawn is the extension's: its 3D view
//! renders on the editor's device through `wasi:webgpu`, in the document a model opens in, at the size that document is; what is in a model shows in the panes as any asset's parts.

use pe2_ext::{Cell, Field, Icon, Registry, Result, Style, Tint, Ui, View};

pe2_ext::extension!("pe2.render", "0.1.0", register);

mod assembly;
mod build;
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
mod material;
mod material_panel;
mod materials;
mod mesh;
mod model;
mod parts;
mod prefab;
#[cfg(target_family = "wasm")]
mod render;
#[cfg(any(target_family = "wasm", test))]
#[cfg_attr(not(target_family = "wasm"), expect(dead_code, reason = "tested here, used by the view, which is the component's"))]
mod scene;
mod skeleton;
mod usda;
mod uv;
#[cfg(target_family = "wasm")]
mod view;

#[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
fn register(r: &mut Registry) -> Result<()> {
    let rested = build::register(r);
    let mesh = r
        .kind("mesh")
        .alias("mesh")
        .label("Mesh")
        .icon(Icon::Polygon, Tint::Picture)
        .stack(build::MESH)
        .output("rest", &rested, pe2_ext::Retain::Cache)
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
        .stack(build::MATERIAL)
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
        .stack(build::SKELETON)
        .field(Field::new("joints").custom())
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
        .stack(build::MODEL)
        .field(Field::new("materials").custom())
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
