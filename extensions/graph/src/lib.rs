//! pe2.graph: what a project defines of its own work graph. An operation - a graph of others, with the inputs and
//! settings it takes - any stack uses by its uid as it uses an extension's, made once and used everywhere: the host
//! lays its graph into each stack using it. A template, a node's stack to start from, offered beside the extensions'.

use pe2_ext::{Field, Icon, Registry, Result, Tint};

pe2_ext::extension!(register);

#[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
fn register(r: &mut Registry) -> Result<()> {
    r.named_type("operation")
        .alias("operation")
        .label("Operation")
        .data()
        .icon(Icon::Stack, Tint::Code)
        .field(Field::new("label").label("Label"))
        .register();
    r.named_type("template")
        .alias("template")
        .label("Template")
        .data()
        .icon(Icon::File, Tint::Code)
        .field(Field::new("label").label("Label"))
        .field(Field::new("node_type").label("Makes"))
        .register();
    Ok(())
}
