//! Tests of `pe2.images`, in an extension of their own: what ships knows nothing of them.

use pe2_ext::test::Editor;
use pe2_ext::{Call, Need, Prepared, Project, Registry, Result, Table, Value, bail, ty};

pe2_ext::extension!(register);

#[expect(clippy::unnecessary_wraps, reason = "what an operation's step is")]
fn next_read(call: &mut Call<'_>) -> Result<Prepared> {
    let next = call.settings.get("next").and_then(Value::as_str);
    Ok(Prepared { needs: next.iter().map(|next| Need::new(next, "native")).collect(), micros: Vec::new() })
}

#[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
fn register(r: &mut Registry) -> Result<()> {
    let _ = r.test(
        "a_file_makes_a_node_that_shows_and_undo_takes_it_away",
        "A file dropped on the window makes a node",
        |project, _| {
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
        },
    );
    // Each reads the next, holding its instance while that one runs.
    let _ = r.op("chained").label("Reads the next").output::<ty::Table>().prepare(next_read).run(|call| {
        if let Some(next) = call.settings.get("next").and_then(Value::as_str) {
            call.output_of(next, "native")?;
        }
        Ok(Table::new())
    });
    // Reads what it never said it reads.
    let _ = r.op("unsaid").label("Reads what it did not say").output::<ty::Table>().run(|call| {
        call.output_of("chain/0", "native")?;
        Ok(Table::new())
    });
    let _ = r.test("a_long_chain_of_nodes_is_not_too_deep", "Nodes of one extension reading each other, nothing made first", |project: &dyn Project, _: &Value| {
        const LONGEST: usize = 40;
        let editor = Editor::of(project);
        for at in 0..LONGEST {
            let next = at.checked_sub(1).map_or_else(String::new, |next| format!("next = \"chain/{next}\"\n"));
            let declaration = format!("uid = \"uid://chain{at}\"\n\n[[stack]]\nop = \"pe2.images.tests@chained\"\n{next}\n[[stack]]\nop = \"output\"\nname = \"native\"\n");
            editor.declare(&format!("chain/{at}"), &format!("uid://chain{at}"), &declaration)?;
        }
        if let Some(why) = editor.made(&format!("chain/{}", LONGEST - 1), "native")? {
            bail!("{why}");
        }
        let declaration = "uid = \"uid://unsaid\"\n\n[[stack]]\nop = \"pe2.images.tests@unsaid\"\n\n[[stack]]\nop = \"output\"\nname = \"native\"\n";
        editor.declare("unsaid", "uid://unsaid", declaration)?;
        let why = editor.made("unsaid", "native")?.unwrap_or_default();
        if !why.contains("it did not say it reads this") {
            bail!("reading what it did not say came to: {why:?}");
        }
        Ok(Value::Boolean(true))
    });
    let _ = r.test(
        "what_is_undone_while_the_project_is_read_waits_for_it",
        "Undoing while the project is read waits, and the window goes on",
        |project: &dyn Project, _: &Value| {
            let editor = Editor::of(project);
            editor.drop_file("casual.ora", &pe2_test_data::painting())?;
            editor.click_label("New node: Texture, from layers")?;
            let path = editor.selected()?;
            editor.hold_reading()?;
            editor.undo()?;
            if !editor.assets()?.contains(&path) {
                bail!("{path} went before the project was read");
            }
            editor.release_reading()?;
            if editor.assets()?.contains(&path) {
                bail!("{path} is still there");
            }
            Ok(Value::Boolean(true))
        },
    );
    Ok(())
}
