//! pe2.events: an event a project defines for itself, an asset anything can name by its uid, as it names the built-in ones. Signaled from the Events pane, by a graph's `signal`
//! box or from the command line; what it sets off is whatever is wired to it. What a stack holds, it holds as any asset: a graph layer's gates, to signal it from others.

use pe2_ext::{Field, Icon, Registry, Result, Tint};

pe2_ext::extension!(register);

#[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
fn register(r: &mut Registry) -> Result<()> {
    r.named_type("event")
        .alias("event")
        .label("Event")
        .data()
        .icon(Icon::Terminal, Tint::Code)
        .field(Field::new("label").label("Label"))
        .register();
    Ok(())
}
