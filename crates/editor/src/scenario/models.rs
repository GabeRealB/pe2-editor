//! A model and what it is made of. A byte-identical package says nothing of these: a mesh that lost its skeleton still builds.

use std::collections::BTreeSet;

use super::{Fixture, Scenario};

#[test]
fn a_prefab_shows_what_its_parts_hold() {
    let mut editor = Scenario::open(Fixture::doll());
    assert_eq!(editor.problems(), [""; 0]);
    editor.show("doll/doll");
    editor.expect("2 faces");
}

#[test]
fn every_type_of_node_shows() {
    let mut editor = Scenario::open(Fixture::doll());
    // What needs no file, as the New node menu makes each: those a target's extension must provide for have none here.
    let ws = editor.workspace();
    let made = ws.extensions.templates().filter(|template| !template.stored() && ws.extensions.stamp(template, "").is_ok());
    let templates: Vec<String> = made.map(|template| template.name.clone()).collect();
    for template in templates {
        editor.workspace().new_node(&template, None).unwrap_or_else(|why| panic!("{template}: {why}"));
        editor.settle();
    }
    let mut shown = BTreeSet::new();
    for path in editor.assets() {
        editor.show(&path);
        let ws = editor.workspace();
        let name = ws.extensions.type_name(&ws.index.assets[&path]).map(str::to_string);
        shown.extend(name);
        editor.expect_not("could not");
        editor.expect_not("stack[");
    }
    assert!(shown.len() > 15, "{shown:?}");
}

#[test]
fn a_second_project_keeps_what_it_makes_in_its_own_store() {
    // Opened and closed, a project leaves the extensions behind it: the next must not keep its work in a folder gone with it.
    drop(Scenario::open(Fixture::doll()));
    let mut editor = Scenario::open(Fixture::doll());
    editor.show("doll/doll");
    editor.expect("2 faces");
}
