//! Tests of `pe2.render`, in an extension of their own: what ships knows nothing of them. They act as a person does, through
//! `pe2.test`, which only exists in test mode, and make what they test of nodes through the real templates, undone after.

use pe2_ext::test::{Editor, NewNode};
use pe2_ext::{Registry, Result, Value, bail};

pe2_ext::extension!("pe2.render.tests", "0.1.0", register, requires = ["pe2.render@^0.1", "pe2.test@^0.1"]);

/// A square and a triangle, one on each joint, with one slot.
const MESH: &str = include_str!("doll.usda");

const JOINTS: &str = r#"joints = [
    { name = "root", translation = [0, 0, 0], rotation = [0, 0, 0] },
    { name = "hip", parent = "root", translation = [0, -100, 0], rotation = [0, 0, 0] },
]"#;

const PREFAB: &str = r#"skeleton = { slot = "uid://bones" }
parts.body = {}
groups.outfit.plain.body = { model = { slot = "uid://body" } }"#;

/// A doll: a texture from a file, a skeleton, a mesh on it, a material, a model and a prefab holding it.
fn doll(editor: &Editor) -> Result<()> {
    let painting = pe2_test_data::painting();
    editor.node(&NewNode { path: "doll/skin", uid: "uid://skin", template: "texture", file: "skin.ora", settings: "", files: &[("skin.ora", &painting)] })?;
    editor.node(&NewNode { path: "doll/skeleton", uid: "uid://bones", template: "skeleton", file: "", settings: JOINTS, files: &[] })?;
    let on_it = "skeleton = { slot = \"uid://bones\" }";
    editor.node(&NewNode { path: "doll/shape", uid: "uid://shape", template: "mesh", file: "", settings: on_it, files: &[("mesh.usda", MESH.as_bytes())] })?;
    let paint = "shader = \"textured\"\nsurface = \"opaque\"\nblend = \"add\"\nmain = \"uid://skin#cells/page_0@default\"";
    editor.node(&NewNode { path: "doll/paint", uid: "uid://paint", template: "material", file: "", settings: paint, files: &[] })?;
    let body = "mesh = { slot = \"uid://shape\" }\nmaterials.page_0 = { slot = \"uid://paint\" }";
    editor.node(&NewNode { path: "doll/body", uid: "uid://body", template: "model", file: "", settings: body, files: &[] })?;
    editor.node(&NewNode { path: "doll/doll", uid: "uid://doll", template: "prefab", file: "", settings: PREFAB, files: &[] })
}

#[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
fn register(r: &mut Registry) -> Result<()> {
    let _ = r.test("a_joint_selected_shows_its_translation", "Selecting a joint shows what it holds", |project, _| {
        let editor = Editor::of(project);
        editor.show("rig/skeleton")?;
        editor.click("pelvis")?;
        editor.expect("Translation")?;
        editor.expect("bones/1")?;
        editor.expect_not("bones/2")?;
        Ok(Value::Boolean(true))
    });
    // A byte-identical package says nothing of this: a mesh that lost its skeleton still builds.
    let _ = r.test("a_prefab_shows_what_its_parts_hold", "A prefab draws the model filling its part", |project, _| {
        let editor = Editor::of(project);
        doll(&editor)?;
        editor.expect_no_problems()?;
        editor.show("doll/doll")?;
        editor.expect("2 faces")?;
        Ok(Value::Boolean(true))
    });
    let _ = r.test("every_type_of_node_shows", "What every template makes opens with nothing failing", |project, _| {
        let editor = Editor::of(project);
        doll(&editor)?;
        let made = editor.make_every_template()?;
        if made < 5 {
            bail!("only {made} templates made a node");
        }
        for path in editor.assets()? {
            editor.show(&path)?;
            editor.expect_not("could not")?;
            // What was just made may be short of a setting, and says so; the doll is whole.
            if path.starts_with("doll/") {
                editor.expect_not("stack[")?;
            }
        }
        Ok(Value::Boolean(true))
    });
    Ok(())
}
