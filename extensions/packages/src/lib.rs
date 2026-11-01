//! pe2.packages: a package, the game's code as it is built, declared as `CMake` declares a target: the files it compiles (`sources`, globs
//! and paths, a leading `!` leaving out what a glob took), what its dependents may include (`public`), and the slot it loads into, a
//! name, whose address is the target's. A target adds what it alone needs under `target.<name>`.
//!
//! Everything about what a package is lives here. `gather` finds its sources and follows what they request, reading through the host so that
//! what is read is the stack as resolved and is recorded: a package is made again when what it reaches changes, and only then. A target
//! is one step after that, `package-target@compile`, given the files reached; which target it is, a package does not know.

mod database;
mod gather;
mod scan;
mod tokenizers;
mod views;

use pe2_ext::ty::{self, SourceTree};
use pe2_ext::{Field, Icon, Registry, Result, Retain, Tint};

pe2_ext::extension!("pe2.packages", "0.1.0", register, requires = ["pe2.images@^0.1", "pe2.render@^0.1"]);

/// A package's stack: what it reaches gathered, then built by whichever target is there, which it does not name.
const STACK: &str = r#"[[stack]]
op = "pe2.packages@gather"

[[stack]]
op = "package-target@preprocess"

[[stack]]
op = "pe2.packages@resolve"

[[stack]]
op = "output"
name = "resolved"
retain = "transient"

[[stack]]
op = "package-target@compile"

[[stack]]
op = "output"
name = "native"
retain = "cache"
pull = ["pe2.events@build", "pe2.events@files-changed"]
listen = ["pe2.events@clean"]
deploy = "build/{config}/"
"#;

/// What a language server reads, for the config in use, wherever any of it may have changed.
const DATABASE_STACK: &str = r#"[[stack]]
op = "graph"
result = "above"

[stack.ops.database]
op = "pe2.packages@database"

[stack.ops.overlay]
op = "pe2.packages@overlay"

[stack.ops.server_values]
op = "pe2.packages@server_values"

[stack.ops.pe2_server]
op = "output"
name = "pe2_server"
retain = "cache"
pull = ["pe2.events@build", "pe2.events@files-changed", "pe2.events@config-changed", "pe2.events@project-opened"]
listen = ["pe2.events@clean"]
deploy = "build/{config}/virtual/pe2_server.h"
from = { value = "server_values" }

[stack.ops.compile_commands]
op = "output"
name = "compile_commands"
retain = "cache"
pull = ["pe2.events@build", "pe2.events@files-changed", "pe2.events@config-changed", "pe2.events@project-opened"]
listen = ["pe2.events@clean"]
deploy = ["build/{config}/compile_commands.json", "build/compile_commands.json"]
from = { value = "database" }

[stack.ops.overlay_file]
op = "output"
name = "overlay"
retain = "cache"
pull = ["pe2.events@build", "pe2.events@config-changed", "pe2.events@project-opened"]
listen = ["pe2.events@clean"]
deploy = "build/{config}/overlay.yaml"
from = { value = "overlay" }
"#;

#[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
fn register(r: &mut Registry) -> Result<()> {
    let _ = r.value_type::<SourceTree>();
    let _ = r.value_type::<ty::Tokens>();
    let _ = r.op("gather").label("Sources").output::<ty::File<SourceTree>>().run(gather::gather);
    let _ = r.op("resolve").label("Slots and values").input::<SourceTree>("sources").output::<SourceTree>().run(gather::resolve);
    tokenizers::register(r);
    let reflection = r.op("reflect").label("What it asks for").output::<ty::Table>().run(gather::reflection);
    let _ = r.op("database").label("Compile database").output::<ty::Bytes>().run(database::database);
    let _ = r.op("overlay").label("Include overlay").output::<ty::Bytes>().run(database::overlay);
    let _ = r.op("server_values").label("Values for a language server").output::<ty::Bytes>().run(database::server_values);
    r.kind("compile-database").alias("compile-database").label("Compile database").icon(Icon::File, Tint::Code).stack(DATABASE_STACK).register();
    r.kind("package")
        .alias("package")
        .label("Package")
        .icon(Icon::Stack, Tint::Code)
        .field(Field::new("slot").label("Slot"))
        .field(Field::new("sources").label("Sources"))
        .field(Field::new("public").label("Public"))
        .field(Field::new("target").label("Targets"))
        .field(Field::new("bind").custom())
        .field(Field::new("option").custom())
        .stack(STACK)
        .output("reflection", &reflection, Retain::Cache)
        .contents_view("Slots", |_| Box::new(views::Slots::default()))
        .inspector_tab("Slots", Icon::Grid, &[], |_| Box::new(views::Slots::sheet()))
        .register();
    Ok(())
}
