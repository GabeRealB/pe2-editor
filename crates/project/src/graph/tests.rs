use std::collections::{BTreeMap, HashSet};
use std::cell::Cell;
use std::sync::Mutex;

use anyhow::{Context, Result};

use super::exec::{Call, Engine, Node, evaluate};
use super::store::Store;
use super::{Content, File, OpDef, Ops, Plan, Retain, Tail, Type, Typed, Value, port, ports};

struct Bundle {
    path: &'static str,
    declaration: String,
    fields: toml::Table,
    files: Mutex<BTreeMap<String, Vec<u8>>>,
}

impl Bundle {
    fn new(declaration: &str, files: &[(&str, &str)]) -> Bundle {
        let files = files.iter().map(|(n, b)| (n.to_string(), b.as_bytes().to_vec())).collect();
        Bundle { path: "test/bundle", declaration: declaration.to_string(), fields: toml::from_str(declaration).unwrap(), files: Mutex::new(files) }
    }

}

impl Node for Bundle {
    fn path(&self) -> &'static str {
        self.path
    }

    fn uid(&self) -> Option<&str> {
        None
    }

    fn declaration(&self) -> &str {
        &self.declaration
    }

    fn fields(&self) -> &toml::Table {
        &self.fields
    }

    fn file_sha1(&self, name: &str) -> Option<String> {
        self.files.lock().unwrap().get(name).map(|b| crate::extract::sha1_hex(b))
    }

    fn read(&self, name: &str) -> Result<Vec<u8>> {
        self.files.lock().unwrap().get(name).cloned().context("no such file")
    }

    fn file_names(&self) -> Vec<String> {
        self.files.lock().unwrap().keys().cloned().collect()
    }

    fn read_content(&self, sha1: &str) -> Result<Vec<u8>> {
        let files = self.files.lock().unwrap();
        files.values().find(|b| crate::extract::sha1_hex(b) == sha1).cloned().context("no such content")
    }
}

/// A file value's bytes, as a node holds them where they are stored.
fn data(node: &Bundle, value: &Value) -> Vec<u8> {
    let Value::Typed(typed) = value else { panic!("not a file") };
    match &typed.file().unwrap().content {
        Content::Inline(bytes) => bytes.clone(),
        Content::Stored { sha1, .. } => node.read_content(sha1).unwrap(),
    }
}

thread_local! {
    /// How many times each test operation ran on this test's thread, which runs its evaluations.
    static RUNS: [Cell<usize>; 4] = const { [Cell::new(0), Cell::new(0), Cell::new(0), Cell::new(0)] };
    /// How many times `upper` was given its input shared.
    static SHARED: Cell<usize> = const { Cell::new(0) };
}

fn ran(op: usize) {
    RUNS.with(|r| r[op].set(r[op].get() + 1));
}

fn runs() -> [usize; 4] {
    RUNS.with(|r| r.each_ref().map(Cell::get))
}

fn shared() -> usize {
    SHARED.with(Cell::get)
}

/// A file value's bytes, as an operation reads them.
fn bytes(call: &Call<'_>, value: &Value) -> Result<Vec<u8>> {
    match value {
        Value::Typed(typed) => call.read(&typed.file()?.content),
        _ => anyhow::bail!("not a file"),
    }
}

fn file(bytes: Vec<u8>) -> Value {
    Value::Typed(Typed::new(pe2_ext::data::BYTES, vec![File::new("made", bytes)]))
}

/// A moment from now: a sweep then takes what was just written.
fn later() -> std::time::SystemTime {
    std::time::SystemTime::now() + std::time::Duration::from_secs(60)
}

fn ops() -> Ops {
    let mut ops = Ops::default();
    let file_type = || Type::File(Box::new(Type::Any));
    ops.add(OpDef::new("t.upper", "Upper", "1").input(port("in", file_type())).makes(file_type(), |call| {
        ran(0);
        let input = call.take(0)?;
        if input.is_shared() {
            SHARED.with(|c| c.set(c.get() + 1));
        }
        let Value::Typed(mut typed) = input.take() else { anyhow::bail!("not a file") };
        let mut bytes = call.read(&typed.file()?.content)?;
        bytes.make_ascii_uppercase();
        typed.files[0].content = Content::Inline(bytes);
        Ok(Value::Typed(typed))
    }));
    ops.add(OpDef::new("t.tree", "Files of the project", "1").makes(file_type(), |call| {
        ran(3);
        let mut joined = Vec::new();
        for path in call.entries("src")? {
            joined.extend(call.read_entry(&path)?);
        }
        for (path, declaration) in call.assets_of("package")? {
            joined.extend(format!("{path}{declaration}").into_bytes());
        }
        Ok(file(joined))
    }));
    ops.add(OpDef::new("t.count", "Count", "1").input(port("in", file_type())).makes(Type::Table, |call| {
        ran(1);
        let input = call.take(0)?;
        let len = bytes(call, input.value())?.len();
        let mut table = toml::Table::new();
        table.insert("len".into(), i64::try_from(len)?.into());
        Ok(Value::Table(table))
    }));
    ops.add(OpDef::new("t.join", "Join", "1").input(ports("parts", file_type())).makes(file_type(), |call| {
        ran(2);
        let mut parts = Vec::new();
        for input in call.take_all(0) {
            parts.extend(bytes(call, input.value())?);
        }
        Ok(file(parts))
    }));
    ops.add(OpDef::new("t.repeat", "Repeat", "1").input(port("in", file_type())).parameter("times").makes(file_type(), |call| {
        ran(3);
        let n: usize = call.parameter().unwrap_or("1").parse()?;
        let input = call.take(0)?;
        Ok(file(bytes(call, input.value())?.repeat(n)))
    }));
    ops
}

fn problems(declaration: &str) -> Vec<String> {
    let (_, problems) = Plan::of(&toml::from_str(declaration).unwrap(), &ops(), &[]);
    problems.into_iter().map(|p| format!("{}: {}", p.at, p.what)).collect()
}

const STACK: &str = r#"
[[stack]]
op = "file"
file = "a.txt"

[[stack]]
op = "output"
name = "source"
retain = "cache"

[[stack]]
op = "t.upper"

[[stack]]
op = "output"
name = "upper"

[[stack]]
op = "t.count"

[[stack]]
op = "output"
name = "count"
when = "eager"
"#;

#[test]
fn a_stack_lowers_to_a_chain_with_taps() {
    let (plan, problems) = Plan::of(&toml::from_str(STACK).unwrap(), &ops(), &[]);
    assert!(problems.is_empty(), "{problems:?}");
    let steps: Vec<_> = plan.steps.iter().map(|s| (s.op.as_str(), s.inputs.clone())).collect();
    assert_eq!(steps, [("file", vec![]), ("t.upper", vec![vec![0]]), ("t.count", vec![vec![1]])]);
    let outputs: Vec<_> = plan.outputs.iter().map(|o| (o.name.as_str(), o.from, o.retain, o.eager)).collect();
    // Its result (the count) kept by no output of its own: the host keeps it.
    let kept = [("count", 2, Retain::Transient, true), (super::RESULT, 2, Retain::Cache, true)];
    assert_eq!(outputs[..2], [("source", 0, Retain::Cache, false), ("upper", 1, Retain::Transient, false)]);
    assert_eq!(outputs[2..], kept);
}

#[test]
fn a_graph_layer_holds_a_preview() {
    let declaration = "[[stack]]\nop = \"graph\"\nresult = \"a\"\nops.a = { op = \"file\", file = \"a.txt\" }\n\
                       ops.size = { op = \"t.count\", from = { in = \"a\" } }\n\
                       ops.shown = { op = \"preview\", name = \"look\", from = { value = \"size\" } }\n";
    let fields: toml::Table = declaration.parse().unwrap();
    let mut ops = ops();
    ops.viewers = std::sync::Arc::new(vec![Type::Table]);
    let (plan, wrong) = Plan::of(&fields, &ops, &[]);
    assert_eq!(wrong, []);
    assert!(plan.outputs.iter().any(|output| output.name == "look" && output.preview));
}

#[test]
fn what_is_wrong_with_a_stack() {
    assert_eq!(problems("[[stack]]\nop = \"t.upper\""), ["stack[0]: its `in` is connected to nothing"]);
    assert_eq!(
        problems("[[stack]]\nop = \"file\"\nfile = \"a\"\n[[stack]]\nop = \"t.count\"\n[[stack]]\nop = \"t.upper\""),
        ["stack[2]: its `in` takes file, and is given table"]
    );
    assert_eq!(problems("[[stack]]\nop = \"x.nothing\""), ["stack[0]: `x.nothing`: no extension provides it"]);
    assert_eq!(
        problems("[[stack]]\nop = \"file\"\nfile = \"a\"\n[[stack]]\nop = \"output\"\nname = \"a\"\n[[stack]]\nop = \"output\"\nname = \"a\""),
        ["stack[2]: a second output named `a`"]
    );
    assert_eq!(problems("[[stack]]\nop = \"t.join\""), ["stack[0]: its `parts` is connected to nothing"]);
}

const GRAPH: &str = r#"
[[stack]]
op = "file"
file = "a.txt"

[[stack]]
op = "graph"
result = "both"
ops.b = { op = "file", file = "b.txt" }
ops.up = { op = "t.upper", from = { in = "b" } }
ops.both = { op = "t.join", from = { parts = ["above", "up", "above"] } }
ops.unused = { op = "t.upper", from = { in = "above" } }
ops.tap = { op = "output", name = "b", from = { value = "up" } }

[[stack]]
op = "output"
name = "joined"

[[stack]]
op = "t.repeat"

[[stack]]
op = "output"
name = "repeated"
"#;

#[test]
fn a_graph_layer_runs_what_its_outputs_reach() {
    let ops = ops();
    let (plan, problems) = Plan::of(&toml::from_str(GRAPH).unwrap(), &ops, &[]);
    assert!(problems.is_empty(), "{problems:?}");
    let node = Bundle::new(GRAPH, &[("a.txt", "a"), ("b.txt", "b")]);
    let before = runs();
    let (joined, _) = plan.output("joined").unwrap();
    let (made, reads) = evaluate(&plan, &ops, &node, &[(joined, None)], &Vec::new(), (None, None));
    assert_eq!(data(&node, made[0].as_ref().unwrap()), b"aBa");
    // `unused` is pruned; `t.repeat` is below the output asked for.
    let ran = runs();
    assert_eq!([ran[0] - before[0], ran[2] - before[2], ran[3] - before[3]], [1, 1, 0]);
    assert_eq!(reads.files.keys().collect::<Vec<_>>(), ["a.txt", "b.txt"]);
    // A family's output, by its parameter's value; the tap inside the graph.
    let (repeated, times) = plan.output("repeated/3").unwrap();
    let (b, _) = plan.output("b").unwrap();
    let (made, _) = evaluate(&plan, &ops, &node, &[(repeated, times.as_deref()), (b, None)], &Vec::new(), (None, None));
    assert_eq!(data(&node, made[0].as_ref().unwrap()), b"aBaaBaaBa");
    assert_eq!(data(&node, made[1].as_ref().unwrap()), b"B");
}

#[test]
fn a_cycle_in_a_graph_layer() {
    let cycle = "[[stack]]\nop = \"graph\"\nresult = \"x\"\nops.x = { op = \"t.upper\", from = { in = \"y\" } }\n\
                 ops.y = { op = \"t.upper\", from = { in = \"x\" } }";
    assert_eq!(
        problems(cycle),
        ["stack[0].ops.x: is in a cycle: it takes, through others, from itself", "stack[0].ops.y: is in a cycle: it takes, through others, from itself"]
    );
}

#[test]
fn what_takes_from_an_operation_that_failed_says_nothing() {
    let failed = "[[stack]]\nop = \"graph\"\nresult = \"x\"\nops.x = { op = \"t.upper\", from = { in = \"y\" } }\n\
                  ops.y = { op = \"nobody.makes\" }";
    assert_eq!(problems(failed), ["stack[0].ops.y: `nobody.makes`: no extension provides it"]);
}

#[test]
fn a_value_taken_once_is_moved() {
    let ops = ops();
    let (plan, _) = Plan::of(&toml::from_str(STACK).unwrap(), &ops, &[]);
    let node = Bundle::new(STACK, &[("a.txt", "abc")]);
    let before = shared();
    // `upper` alone: the file's bytes are taken by it alone, and moved.
    let (count, _) = plan.output("count").unwrap();
    let (made, _) = evaluate(&plan, &ops, &node, &[(count, None)], &Vec::new(), (None, None));
    assert_eq!(made[0].as_ref().unwrap().as_ref(), &Value::Table(toml::from_str("len = 3").unwrap()));
    assert_eq!(shared(), before);
    // Asked for with `source`, the bytes are shared: upper copies them, the source is unchanged.
    let (source, _) = plan.output("source").unwrap();
    let (upper, _) = plan.output("upper").unwrap();
    let (made, _) = evaluate(&plan, &ops, &node, &[(source, None), (upper, None)], &Vec::new(), (None, None));
    assert_eq!((data(&node, made[0].as_ref().unwrap()), data(&node, made[1].as_ref().unwrap())), (b"abc".to_vec(), b"ABC".to_vec()));
    assert!(shared() > before);
}

#[test]
fn a_failure_reaches_what_takes_from_it() {
    let ops = ops();
    let (plan, _) = Plan::of(&toml::from_str(STACK).unwrap(), &ops, &[]);
    let node = Bundle::new(STACK, &[]);
    let (count, _) = plan.output("count").unwrap();
    let (made, _) = evaluate(&plan, &ops, &node, &[(count, None)], &Vec::new(), (None, None));
    assert_eq!(made[0].as_ref().unwrap_err().as_ref(), "stack[0]: test/bundle: no file or folder a.txt");
}

#[test]
fn a_stored_folder_is_every_file_in_it_named_within_it() {
    let stack = "[[stack]]\nop = \"file\"\nfile = \"face.ufo\"\n\n[[stack]]\nop = \"output\"\nname = \"source\"\n";
    let ops = ops();
    let (plan, _) = Plan::of(&toml::from_str(stack).unwrap(), &ops, &[]);
    let node = Bundle::new(stack, &[("face.ufo/metainfo.plist", "m"), ("face.ufo/glyphs/A_.glif", "a"), ("other.txt", "o")]);
    let (source, _) = plan.output("source").unwrap();
    let (made, _) = evaluate(&plan, &ops, &node, &[(source, None)], &Vec::new(), (None, None));
    let made = made[0].as_ref().unwrap();
    let Value::Typed(typed) = &**made else { panic!("not files") };
    let names: Vec<&str> = typed.files.iter().map(|file| file.name.as_str()).collect();
    assert_eq!(names, ["glyphs/A_.glif", "metainfo.plist"]);
    // What is not there is said so.
    let empty = Bundle::new(stack, &[("other.txt", "o")]);
    let (made, _) = evaluate(&plan, &ops, &empty, &[(source, None)], &Vec::new(), (None, None));
    assert!(made[0].as_ref().unwrap_err().contains("no file or folder face.ufo"), "{:?}", made[0]);
}

#[test]
fn an_optional_stored_file_is_none_where_the_node_lacks_it_and_read_once_it_is_there() {
    let stack = "[[stack]]\nop = \"file\"\nfile = \"under.png\"\noptional = true\n\n[[stack]]\nop = \"output\"\nname = \"source\"\n";
    let ops = ops();
    let (plan, _) = Plan::of(&toml::from_str(stack).unwrap(), &ops, &[]);
    let (source, _) = plan.output("source").unwrap();
    let made_of = |node: &Bundle| {
        let (made, reads) = evaluate(&plan, &ops, node, &[(source, None)], &Vec::new(), (None, None));
        let Value::Typed(typed) = &**made[0].as_ref().unwrap() else { panic!("not files") };
        (typed.files.iter().map(|file| file.name.clone()).collect::<Vec<_>>(), reads)
    };
    let (none, reads) = made_of(&Bundle::new(stack, &[("other.txt", "o")]));
    assert_eq!(none, Vec::<String>::new());
    // Its absence was read: the file come, what was made of none no longer holds.
    let there = Bundle::new(stack, &[("other.txt", "o"), ("under.png", "u")]);
    assert!(!reads.hold(&there, None));
    assert_eq!(made_of(&there).0, ["under.png"]);
    // `optional` is how the stack is put together, not a setting of the node's.
    assert!(!super::set_on(super::FILE, &toml::from_str("file = \"under.png\"\noptional = true").unwrap()).contains_key("optional"));
}

#[test]
fn a_kept_output_holds_while_what_it_read_does() {
    let dir = std::env::temp_dir().join(format!("pe2-graph-store-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let engine = Engine::new(ops(), Some(Store::at(dir.clone()).unwrap()));
    let node = Bundle::new(STACK, &[("a.txt", "abc")]);
    let (plan, _) = Plan::of(&node.fields, &engine.ops, &[]);
    let first = engine.output(&plan, &node, "source", false).unwrap();
    assert!(!first.kept);
    let again = engine.output(&plan, &node, "source", true).unwrap();
    assert!(again.kept);
    assert_eq!(data(&node, again.value.as_deref().unwrap()), b"abc");
    // A transient output is never kept.
    assert!(!engine.output(&plan, &node, "upper", false).unwrap().kept);
    // The file changed: made again.
    node.files.lock().unwrap().insert("a.txt".into(), b"abcd".to_vec());
    let changed = engine.output(&plan, &node, "source", true).unwrap();
    assert!(!changed.kept);
    assert_eq!(data(&node, changed.value.as_deref().unwrap()), b"abcd");
    // Swept: the trace no longer live, its value and blob go.
    let swept = engine.store().unwrap().sweep_older(&HashSet::new(), &HashSet::new(), later());
    assert_eq!(swept, (1, 2, 2));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn nodes_making_alike_keep_a_trace_each_and_share_the_value() {
    let dir = std::env::temp_dir().join(format!("pe2-graph-alike-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let engine = Engine::new(ops(), Some(Store::at(dir.clone()).unwrap()));
    let one = Bundle::new(STACK, &[("a.txt", "abc")]);
    let two = Bundle { path: "test/other", ..Bundle::new(STACK, &[("a.txt", "abc")]) };
    let (plan, _) = Plan::of(&one.fields, &engine.ops, &[]);
    assert!(!engine.output(&plan, &one, "source", false).unwrap().kept);
    assert!(!engine.output(&plan, &two, "source", false).unwrap().kept);
    // Each found kept by its own trace; the other's file changing leaves it.
    two.files.lock().unwrap().insert("a.txt".into(), b"xyz".to_vec());
    assert!(engine.output(&plan, &one, "source", false).unwrap().kept);
    let count = |sub: &str| std::fs::read_dir(dir.join(sub)).unwrap().count();
    assert_eq!((count("traces"), count("values")), (2, 1));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_kinds_output_is_made_from_a_kept_one_and_swept_to_its_newest() {
    let dir = std::env::temp_dir().join(format!("pe2-graph-tail-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let engine = Engine::new(ops(), Some(Store::at(dir.clone()).unwrap()));
    let stack = "[[stack]]\nop = \"file\"\nfile = \"a.txt\"\n\n[[stack]]\nop = \"t.upper\"\n\n\
                 [[stack]]\nop = \"output\"\nname = \"native\"\nretain = \"cache\"\n";
    let node = Bundle::new(stack, &[("a.txt", "abc"), ("b.txt", "unread")]);
    let tail = [Tail { name: "counted".into(), fixed: false, op: "t.count".into(), retain: Retain::Cache, from: None, step: None, preview: false, settings: toml::Table::new() }];
    let (plan, problems) = Plan::of(&node.fields, &engine.ops, &tail);
    assert!(problems.is_empty(), "{problems:?}");
    // The kept native form is taken as made: `upper` runs once, for it, and not again for what is made of it.
    let before = runs();
    assert!(!engine.output(&plan, &node, "native", false).unwrap().kept);
    let counted = engine.output(&plan, &node, "counted", true).unwrap();
    assert_eq!((counted.kept, runs()[0] - before[0]), (false, 1));
    assert_eq!(counted.value.as_deref(), Some(&Value::Table(toml::from_str("len = 3").unwrap())));
    // It read what the native form read: what neither read changing leaves both; what they read, makes them again.
    assert_eq!(counted.reads.files.keys().collect::<Vec<_>>(), ["a.txt"]);
    node.files.lock().unwrap().insert("b.txt".into(), b"changed".to_vec());
    assert!(engine.output(&plan, &node, "counted", false).unwrap().kept);
    node.files.lock().unwrap().insert("a.txt".into(), b"abcd".to_vec());
    let again = engine.output(&plan, &node, "counted", true).unwrap();
    assert_eq!((again.kept, again.value.as_deref()), (false, Some(&Value::Table(toml::from_str("len = 4").unwrap()))));
    // An output's trace is under one key: its older value goes with the sweep (the count of three), and what was
    // written out for the count's port asking for a file (`ABCD`), which no value names; a node no longer live, all of
    // it - its two traces, their values, the native form's blob.
    let live: HashSet<String> = ["test/bundle".to_string()].into();
    assert_eq!(engine.store().unwrap().sweep_older(&live, &HashSet::new(), later()), (0, 1, 1));
    assert_eq!(engine.store().unwrap().sweep_older(&HashSet::new(), &HashSet::new(), later()), (2, 2, 1));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_nodes_result_is_kept_by_its_stack_or_the_host() {
    let names = |plan: &Plan| plan.outputs.iter().map(|o| (o.name.clone(), o.retain)).collect::<Vec<_>>();
    // A stored file is seen as it is: nothing keeps it.
    let (stored, _) = Plan::of(&toml::from_str("[[stack]]\nop = \"file\"\nfile = \"a.txt\"").unwrap(), &ops(), &[]);
    assert!(stored.result_stored() && stored.result_output().is_none() && stored.outputs.is_empty());
    // Made, and kept by nothing in the stack: the host keeps it.
    let made = "[[stack]]\nop = \"file\"\nfile = \"a.txt\"\n[[stack]]\nop = \"t.upper\"";
    let (plan, _) = Plan::of(&toml::from_str(made).unwrap(), &ops(), &[]);
    assert_eq!(names(&plan), [(super::RESULT.to_string(), Retain::Cache)]);
    assert_eq!(plan.result_output(), Some(0));
    // Kept by its stack, under any name: the host adds nothing.
    let (plan, _) = Plan::of(&toml::from_str(&format!("{made}\n[[stack]]\nop = \"output\"\nname = \"upper\"\nretain = \"cache\"")).unwrap(), &ops(), &[]);
    assert_eq!(names(&plan), [("upper".to_string(), Retain::Cache)]);
}

#[test]
fn an_output_is_a_family_of_the_parameter_it_reaches() {
    let mut ops = ops();
    ops.add(OpDef::new("t.pick", "Pick", "1").input(port("in", Type::File(Box::new(Type::Any)))).parameter("which").makes(Type::Any, |call| Ok(call.take(0)?.take())));
    let stack = |below: &str| format!("[[stack]]\nop = \"file\"\nfile = \"a\"\n[[stack]]\nop = \"t.repeat\"\n{below}[[stack]]\nop = \"output\"\nname = \"made\"");
    let (plan, problems) = Plan::of(&toml::from_str(&stack("")).unwrap(), &ops, &[]);
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(plan.outputs[0].parameter.as_deref(), Some("times"));
    assert_eq!(plan.output("made/3").map(|(o, p)| (o, p.unwrap())), Some((0, "3".to_string())));
    let (_, problems) = Plan::of(&toml::from_str(&stack("[[stack]]\nop = \"t.pick\"\n")).unwrap(), &ops, &[]);
    assert_eq!(problems.iter().map(|p| p.what.as_str()).collect::<Vec<_>>(), ["`made` reaches operations of `times` and `which`: an output is one family"]);
}

/// Nodes reaching each other's outputs, by path.
struct Nodes(Vec<Bundle>, Ops);

impl super::exec::World for Nodes {
    fn node(&self, reference: &toml::Value) -> Option<(&dyn Node, Plan)> {
        let node = self.0.iter().find(|n| Some(n.path) == reference.as_str())?;
        Some((node as &dyn Node, Plan::of(&node.fields, &self.1, &[]).0))
    }
}

/// A node reading the project's plain files, held in a folder.
struct Tree(Bundle, Ops, std::path::PathBuf);

impl super::exec::World for Tree {
    fn node(&self, _: &toml::Value) -> Option<(&dyn Node, Plan)> {
        Some((&self.0 as &dyn Node, Plan::of(&self.0.fields, &self.1, &[]).0))
    }

    fn entries(&self, folder: &str) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.2.join(folder)).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names.into_iter().map(|name| format!("{folder}/{name}")).collect()
    }

    fn entry(&self, path: &str) -> Option<std::path::PathBuf> {
        Some(self.2.join(path)).filter(|disk| disk.is_file())
    }

    fn assets_of(&self, _: &str) -> Vec<(String, String)> {
        vec![("packages/one".to_string(), std::fs::read_to_string(self.2.join("declaration")).unwrap())]
    }
}

#[test]
fn what_is_made_from_the_projects_files_is_made_again_when_they_change() {
    let dir = std::env::temp_dir().join(format!("pe2-graph-tree-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("tree/src")).unwrap();
    std::fs::write(dir.join("tree/src/a.c"), "one").unwrap();
    std::fs::write(dir.join("tree/declaration"), "public = []").unwrap();
    let engine = Engine::new(ops(), Some(Store::at(dir.join("store")).unwrap()));
    let declared = "[[stack]]\nop = \"t.tree\"\n\n[[stack]]\nop = \"output\"\nname = \"native\"\nretain = \"cache\"\n";
    let world = Tree(Bundle::new(declared, &[]), engine.ops.clone(), dir.join("tree"));
    let reach = super::exec::Reach { engine: &engine, world: &world };
    let make = || {
        let (node, plan) = super::exec::World::node(&world, &toml::Value::String(String::new())).unwrap();
        engine.output_in(Some(reach), &plan, node, "native", None, true).unwrap()
    };
    assert!(!make().kept);
    assert!(make().kept, "nothing changed");
    std::fs::write(dir.join("tree/src/a.c"), "two").unwrap();
    assert!(!make().kept, "a file read changed");
    assert!(make().kept);
    std::fs::write(dir.join("tree/src/b.c"), "new").unwrap();
    assert!(!make().kept, "a file came to the folder listed");
    std::fs::remove_file(dir.join("tree/src/b.c")).unwrap();
    assert!(!make().kept, "a file left it");
    assert!(make().kept);
    std::fs::write(dir.join("tree/declaration"), "public = [\"include\"]").unwrap();
    assert!(!make().kept, "a declaration read changed");
    let _ = std::fs::remove_dir_all(&dir);
}

const PART: &str = "[[stack]]\nop = \"file\"\nfile = \"part.bin\"\n\n[[stack]]\nop = \"output\"\nname = \"native\"\n";

fn joining(parts: &str) -> String {
    format!(
        "parts = [{parts}]\n\n[[stack]]\nop = \"references\"\nfield = \"parts\"\noutput = \"native\"\n\n\
         [[stack]]\nop = \"concatenate\"\nname = \"joined.bin\"\n\n[[stack]]\nop = \"output\"\nname = \"native\"\nretain = \"cache\"\n"
    )
}

#[test]
fn a_node_joins_other_nodes_outputs_and_holds_while_they_do() {
    let dir = std::env::temp_dir().join(format!("pe2-graph-reach-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let engine = Engine::new(ops(), Some(Store::at(dir.clone()).unwrap()));
    let mut a = Bundle::new(PART, &[("part.bin", "head-")]);
    a.path = "test/a";
    // Past what a writer holds in memory: the joined file is spilled to the store.
    let big = "x".repeat(super::exec::SPILL);
    let mut b = Bundle::new(PART, &[("part.bin", &big)]);
    b.path = "test/b";
    let mut joined = Bundle::new(&joining("\"test/a\", \"test/b\""), &[]);
    joined.path = "test/joined";
    let mut itself = Bundle::new(&joining("\"test/a\", \"test/itself\""), &[]);
    itself.path = "test/itself";
    let world = Nodes(vec![a, b, joined, itself], engine.ops.clone());
    let reach = super::exec::Reach { engine: &engine, world: &world };
    let make = |path: &str| {
        let (node, plan) = super::exec::World::node(&world, &toml::Value::String(path.into())).unwrap();
        engine.output_in(Some(reach), &plan, node, "native", None, true)
    };
    let stored = |given: &super::exec::Given| match &given.value.as_deref() {
        Some(Value::Typed(typed)) => match &typed.files[0].content {
            Content::Stored { sha1, .. } => std::fs::read(engine.store().unwrap().blob_path(sha1)).unwrap(),
            Content::Inline(_) => panic!("held in memory"),
        },
        _ => panic!("not a file"),
    };
    let first = make("test/joined").unwrap();
    assert!(!first.kept);
    assert_eq!(stored(&first), format!("head-{big}").into_bytes());
    assert_eq!(first.reads.outputs.len(), 2);
    assert!(make("test/joined").unwrap().kept);
    // A part changed: made again.
    world.0[0].files.lock().unwrap().insert("part.bin".into(), b"HEAD-".to_vec());
    let changed = make("test/joined").unwrap();
    assert!(!changed.kept);
    assert_eq!(stored(&changed)[..5], *b"HEAD-");
    // A node reading itself, through others or not, is a cycle.
    let Err(cycle) = make("test/itself") else { panic!("a cycle made") };
    assert!(format!("{cycle:#}").contains("reads, through others, itself"), "{cycle:#}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn marks_are_kept_as_they_are() {
    let dir = std::env::temp_dir().join(format!("pe2-graph-marks-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = Store::at(dir.clone()).unwrap();
    let rect = pe2_ext::Rect { x: 1.0, y: 2.0, width: 3.0, height: 4.0 };
    let marks = Value::Marks(pe2_ext::Marks {
        boxes: vec![pe2_ext::Mark { name: "a".into(), rect }],
        summary: "1 cell".into(),
        toggle: "Cells".into(),
        legend: true,
    });
    let hash = super::hash(&marks);
    store.put(&hash, &marks, &|_| None).unwrap();
    assert_eq!(store.get(&hash).unwrap(), marks);
    let _ = std::fs::remove_dir_all(&dir);
}

fn events_plan(stack: &str) -> (Plan, Vec<super::Problem>) {
    let declaration: toml::Table = stack.parse().unwrap();
    Plan::of(&declaration, &Ops::default(), &[])
}

const WIRED: &str = r#"
[[stack]]
op = "graph"
result = "text"

[stack.ops.text]
op = "constant"
value = {}

[stack.ops.saved]
op = "event"
event = "saved"

[stack.ops.busy]
op = "event"
event = "busy"

[stack.ops.not_busy]
op = "not"
from = { inputs = "busy" }

[stack.ops.ready]
op = "and"
from = { inputs = ["saved", "not_busy"] }

[stack.ops.made]
op = "output"
name = "made"
retain = "cache"
from = { value = "text", pull = "ready" }

[stack.ops.tell]
op = "signal"
event = "told"
from = { when = "ready" }
"#;

#[test]
fn gates_are_boxes_of_a_graph_and_what_they_come_to_wires_an_output() {
    let (plan, problems) = events_plan(WIRED);
    assert_eq!(problems, Vec::new());
    let made = &plan.outputs[0];
    let when = plan.expression(made.events.pull[0]);
    assert!(when.holds(&|event| event == "saved"));
    assert!(!when.holds(&|_| true));
    assert!(when.waits());
    assert_eq!(plan.rules.len(), 1);
    assert_eq!(plan.rules[0].event, "told");
}

#[test]
fn what_holds_with_nothing_signaled_is_no_trigger() {
    let refused = WIRED.replace(r#"from = { value = "text", pull = "ready" }"#, r#"from = { value = "text", pull = "not_busy" }"#);
    let (plan, problems) = events_plan(&refused);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].what.contains("every epoch"));
    assert_eq!(plan.outputs[0].events.pull, Vec::<usize>::new());
}

#[test]
fn deployed_outside_the_build_folder_is_refused() {
    let elsewhere = WIRED.replace(r#"retain = "cache"
from"#, r#"deploy = "../somewhere"
from"#);
    let (_, problems) = events_plan(&elsewhere);
    assert!(problems.iter().any(|p| p.what.contains("under `build/`")), "{problems:?}");
}

#[test]
fn a_gate_is_monotone_by_its_table() {
    let monotone: Vec<&str> = crate::graph::GATES.iter().filter(|g| g.is_monotone()).map(|g| g.name).collect();
    assert_eq!(monotone, ["and", "or"]);
}

#[test]
fn a_preview_shows_what_a_viewer_takes_or_what_turns_into_it() {
    let mut ops = ops();
    ops.viewers = std::sync::Arc::new(vec![Type::Image]);
    let raw = Type::Named("t@raw".to_string());
    ops.add(OpDef::new("t.read", "Read", "1").input(port("in", Type::Table)).makes(raw.clone(), |_| anyhow::bail!("not run")));
    ops.add(OpDef::new("t.draw", "Draw", "1").input(port("in", raw)).makes(Type::Image, |_| anyhow::bail!("not run")));
    let stack = "[[stack]]\nop = \"constant\"\nvalue = {}\n\n[[stack]]\nop = \"preview\"\nname = \"shown\"\n\n\
                 [[stack]]\nop = \"output\"\nname = \"native\"\n";
    let node = Bundle::new(stack, &[]);
    let (plan, problems) = Plan::of(&node.fields, &ops, &[]);
    assert!(problems.is_empty(), "{problems:?}");
    let shown = plan.outputs.iter().find(|output| output.name == "shown").unwrap();
    assert!(shown.preview);
    assert_eq!(plan.steps[shown.from].op, "t.draw", "turned into what a viewer takes, off to the side");
    let native = plan.outputs.iter().find(|output| output.name == "native").unwrap();
    assert_eq!(plan.steps[native.from].op, "constant", "what goes on down the stack is what came down it");
    // What nothing turns into what a viewer takes is told.
    ops.viewers = std::sync::Arc::new(vec![Type::Video]);
    let (_, problems) = Plan::of(&node.fields, &ops, &[]);
    assert!(problems.iter().any(|problem| problem.what.contains("nothing shows")), "{problems:?}");
}

#[test]
fn a_layer_says_what_it_has_to() {
    let mut ops = ops();
    let mut held = OpDef::new("t.held", "Held", "1").makes(Type::Table, |_| anyhow::bail!("not run"));
    held.required = vec!["rect".to_string()];
    ops.add(held);
    let wrong = Bundle::new("[[stack]]\nop = \"t.held\"\n", &[]);
    let (_, problems) = Plan::of(&wrong.fields, &ops, &[]);
    assert!(problems.iter().any(|problem| problem.what.contains("`rect`")), "{problems:?}");
    let right = Bundle::new("[[stack]]\nop = \"t.held\"\nrect = [0, 0, 1, 1]\n", &[]);
    assert_eq!(Plan::of(&right.fields, &ops, &[]).1, vec![]);
}

#[test]
fn a_wire_names_a_layer_and_one_of_its_outputs() {
    let mut ops = ops();
    let mut pair = OpDef::new("t.pair", "Pair", "1").makes(Type::Table, |_| {
        let one = |n: i64| Value::Table(toml::Table::from_iter([("n".to_string(), toml::Value::Integer(n))]));
        Ok(Value::List(vec![one(1), one(2)]))
    });
    pair.also = vec![("second".to_string(), Type::Table)];
    ops.add(pair);
    ops.add(OpDef::new("t.add", "Add", "1").input(port("a", Type::Table)).input(port("b", Type::Table)).makes(Type::Table, |call| {
        let read = |value: &Value| value.settings().and_then(|table| table.get("n")).and_then(toml::Value::as_integer).unwrap_or(0);
        let (a, b) = (call.take(0)?, call.take(1)?);
        Ok(Value::Table(toml::Table::from_iter([("n".to_string(), toml::Value::Integer(read(a.value()) * 10 + read(b.value())))])))
    }));
    let stack = "[[stack]]\nid = \"made\"\nop = \"t.pair\"\n\n\
                 [[stack]]\nop = \"t.add\"\nb = { from = \"made.second\" }\n\n\
                 [[stack]]\nop = \"output\"\nname = \"sum\"\n\n\
                 [[stack]]\nop = \"output\"\nname = \"second\"\nvalue = { from = \"made.second\" }\n";
    let node = Bundle::new(stack, &[]);
    let engine = Engine::new(ops, None);
    let (plan, problems) = Plan::of(&node.fields, &engine.ops, &[]);
    assert!(problems.is_empty(), "{problems:?}");
    // The first port takes what is above it, the first output of the layer; the other is wired.
    let sum = engine.output(&plan, &node, "sum", true).unwrap();
    assert_eq!(sum.value.as_deref(), Some(&Value::Table(toml::from_str("n = 12").unwrap())));
    let second = engine.output(&plan, &node, "second", true).unwrap();
    assert_eq!(second.value.as_deref(), Some(&Value::Table(toml::from_str("n = 2").unwrap())));
    // What names nothing is told, and what is left connected to nothing.
    let wrong = Bundle::new("[[stack]]\nid = \"made\"\nop = \"t.pair\"\n\n[[stack]]\nop = \"t.add\"\nb = { from = \"made.third\" }\n", &[]);
    assert!(Plan::of(&wrong.fields, &engine.ops, &[]).1.iter().any(|problem| problem.what.contains("no output `third`")));
    let open = Bundle::new("[[stack]]\nop = \"t.pair\"\n\n[[stack]]\nop = \"t.add\"\n", &[]);
    assert!(Plan::of(&open.fields, &engine.ops, &[]).1.iter().any(|problem| problem.what.contains("connected to nothing")));
}
