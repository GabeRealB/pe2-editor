//! What a session leaves behind for the next.

use super::{Fixture, Scenario};

#[test]
fn a_second_project_keeps_what_it_makes_in_its_own_store() {
    // Opened and closed, a project leaves the extensions behind it: the next must not keep its work in a folder gone with it.
    let mut first = Scenario::open(Fixture::rig());
    first.show("rig/skeleton");
    drop(first);
    let mut editor = Scenario::open(Fixture::rig());
    editor.show("rig/skeleton");
    editor.expect("3 joints");
}
