//! pe2.packages: a package, the game's code as it is built, declared as `CMake` declares a target: the files it compiles (`sources`, globs
//! and paths, a leading `!` leaving out what a glob took), what its dependents may include (`public`). A target adds what it alone needs
//! under `target.<name>` (`pe2_ext::targeted`): the PS1 the slot a package loads into, a name whose address is the target's, its link
//! script, and objects it links as they are.
//!
//! Everything about what a package is lives here. `gather` finds its sources and follows what they request, reading through the host so that
//! what is read is the stack as resolved and is recorded: a package is made again when what it reaches changes, and only then. A target
//! takes over after that, given the files reached: `package-target@compile` makes its `objects`, and the last two steps, `symbols` and
//! `link`, link it as the PS1 does, one image a package; a target linking packages otherwise removes them in a layer of its own and takes
//! the `objects`. Which target it is, a package does not know.

mod database;
mod gather;
mod scan;
mod tokenizers;
mod views;

use pe2_ext::ty::{self, Registered, SourceTree};
use pe2_ext::{Call, Field, Icon, Registry, Result, Retain, Table, Tint, Value, anyhow};

pe2_ext::extension!("pe2.packages", "0.1.0", register, requires = ["pe2.game@^0.1", "pe2.images@^0.1", "pe2.render@^0.1"]);

/// A package's stack: what it reaches gathered, then built by whichever target is there, which it does not name.
const STACK: &str = r#"[[stack]]
op = "pe2.packages@package"

[[stack]]
op = "pe2.packages@gather"

[[stack]]
op = "package-target@preprocess"

[[stack]]
id = "resolved"
op = "pe2.packages@resolve"

[[stack]]
op = "output"
name = "resolved"
retain = "transient"

[[stack]]
id = "objects"
op = "package-target@compile"

[[stack]]
op = "output"
name = "objects"
retain = "cache"

[[stack]]
op = "package-target@symbols"
sources = { from = "resolved" }
objects = { from = "objects" }

[[stack]]
op = "output"
name = "symbols"
retain = "cache"

[[stack]]
op = "package-target@link"
sources = { from = "resolved" }
objects = { from = "objects" }

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
op = "pe2.packages@compile-database"

[[stack]]
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

/// What a package is, its sources and what fills its slots: what is set on it.
const PACKAGE: &str = "pe2.packages@package";

/// A list: references in order, which a package indexes. Each item is an output leading to what it names
/// (`items/<index>`), so that `PE2_VALUE(demos, items, 0, _id)` reads the first through its own tokenizer.
const LIST: &str = r#"[[stack]]
id = "list"
op = "pe2.packages@list"

[[stack]]
op = "pe2.packages@item"
list = { from = "list" }

[[stack]]
op = "output"
name = "items"
"#;

/// A list's item: what its reference names, as the reference names it - an output of a node leads on where that output does.
fn item(call: &mut Call) -> Result<Table> {
    let index: String = call.parameter()?;
    let given = call.given();
    let items = given.get("items").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    let at: usize = index.parse().map_err(|_| anyhow!("{index} is no index"))?;
    let reference = items.get(at).ok_or_else(|| anyhow!("it has {} items, and no item {at}", items.len()))?;
    match (pe2_ext::target_of(reference), pe2_ext::output_of(reference)) {
        (Some(node), Some(output)) => call.table_of(node, output),
        _ => pe2_ext::tokens::link(Some(reference)),
    }
}

#[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
fn register(r: &mut Registry) -> Result<()> {
    let _ = r.value_type::<SourceTree>();
    let _ = r.value_type::<ty::Tokens>();
    let _ = r.op("gather").label("Sources").takes("package", &[PACKAGE]).output::<ty::File<SourceTree>>().run(gather::gather);
    let _ = r.op("resolve").label("Slots and values").input::<SourceTree>("sources").output::<SourceTree>().prepared_by_running().run(gather::resolve);
    tokenizers::register(r);
    let _ = r
        .op("item")
        .label("An item of the list")
        .takes("list", &["pe2.packages@list"])
        .output::<ty::Table>()
        .parameter::<String>("index")
        .members(|asset| {
            let count = asset.field("items").and_then(Value::as_array).map_or(0, Vec::len);
            (0..count).map(|at| (at.to_string(), at.to_string())).collect()
        })
        .prepared_by_running()
        .run(item);
    r.named_type("list").alias("list").label("List").data().icon(Icon::Stack, Tint::Structure).field(Field::new("items").label("Items")).stack(LIST).register();
    let _ = r.op("reflect").label("What it asks for").takes("package", &[PACKAGE]).output::<ty::Table>().provides("reflection").keeps(Retain::Cache).run(gather::reflection);
    let _ = r.op("database").label("Compile database").output::<ty::Bytes>().prepare(database::database_reads).run(database::database);
    let _ = r.op("overlay").label("Include overlay").output::<ty::Bytes>().run(database::overlay);
    let _ = r.op("server_values").label("Values for a language server").output::<ty::Bytes>().prepare(database::server_values_reads).run(database::server_values);
    r.named_type("compile-database").alias("compile-database").label("Compile database").data().icon(Icon::File, Tint::Code).stack(DATABASE_STACK).register();
    r.named_type("package")
        .alias("package")
        .label("Package")
        .data()
        .of(SourceTree::NAME)
        .icon(Icon::Stack, Tint::Code)
        .field(Field::new("slot").label("Slot"))
        .field(Field::new("sources").label("Sources"))
        .field(Field::new("public").label("Public"))
        .field(Field::new("target").label("Targets"))
        .field(Field::new("bind").custom())
        .field(Field::new("option").custom())
        .stack(STACK)
        .contents_view("Slots", |_| Box::new(views::Slots::default()))
        .inspector_tab("Slots", Icon::Grid, &[], |_| Box::new(views::Slots::sheet()))
        .register();
    // A package compiling nothing: others' headers and libraries.
    drop(r.template("library", PACKAGE, "[[stack]]\nop = \"pe2.packages@package\"\n").label("Library"));
    Ok(())
}
