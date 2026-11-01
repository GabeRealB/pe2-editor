//! Tests of `pe2.images`, in an extension of their own: what ships knows nothing of them.

use pe2_ext::test::Editor;
use pe2_ext::{Registry, Result, Value, bail};

pe2_ext::extension!("pe2.images.tests", "0.1.0", register, requires = ["pe2.images@^0.1", "pe2.test@^0.1"]);

#[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
fn register(r: &mut Registry) -> Result<()> {
    let _ = r.test("a_file_makes_a_node_that_shows_and_undo_takes_it_away", "A file dropped on the window makes a node", |project, _| {
        let editor = Editor::of(project);
        editor.drop_file("casual.ora", &pe2_test_data::painting())?;
        editor.click_label("New node: Texture, from layers")?;
        let path = editor.selected()?;
        if !editor.pictured(&path, "picture/default")? {
            bail!("{path} has no picture");
        }
        editor.expect("page_0")?;
        editor.expect_no_problems()?;
        editor.undo()?;
        if editor.assets()?.contains(&path) {
            bail!("{path} is still there");
        }
        Ok(Value::Boolean(true))
    });
    Ok(())
}
