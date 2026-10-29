use std::collections::{BTreeMap, HashSet};
use std::cell::Cell;
use std::sync::Mutex;

use anyhow::{Context, Result};

use super::exec::{Engine, Node, evaluate};
use super::store::Store;
use super::{File, OpDef, Ops, Plan, Port, Retain, Type, Value};

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

fn bytes(value: &Value) -> &[u8] {
    match value {
        Value::File(f) => &f.bytes,
        _ => panic!("not a file"),
    }
}

fn file(bytes: Vec<u8>) -> Value {
    Value::File(File { name: "made".into(), bytes })
}

fn ops() -> Ops {
    let mut ops = Ops::default();
    ops.add(OpDef::new("t.upper", "Upper", "1").input(Port::one("in", Type::File)).makes(Type::File, |call| {
        ran(0);
        let input = call.take(0)?;
        if input.is_shared() {
            SHARED.with(|c| c.set(c.get() + 1));
        }
        let Value::File(mut f) = input.take() else { anyhow::bail!("not a file") };
        f.bytes.make_ascii_uppercase();
        Ok(Value::File(f))
    }));
    ops.add(OpDef::new("t.count", "Count", "1").input(Port::one("in", Type::File)).makes(Type::Table, |call| {
        ran(1);
        let len = bytes(call.take(0)?.value()).len();
        let mut table = toml::Table::new();
        table.insert("len".into(), i64::try_from(len)?.into());
        Ok(Value::Table(table))
    }));
    ops.add(OpDef::new("t.join", "Join", "1").input(Port::many("parts", Type::File)).makes(Type::File, |call| {
        ran(2);
        let parts: Vec<u8> = call.take_all(0).iter().flat_map(|i| bytes(i.value()).to_vec()).collect();
        Ok(file(parts))
    }));
    ops.add(OpDef::new("t.repeat", "Repeat", "1").input(Port::one("in", Type::File)).makes(Type::File, |call| {
        ran(3);
        let n: usize = call.parameter().unwrap_or("1").parse()?;
        Ok(file(bytes(call.take(0)?.value()).repeat(n)))
    }));
    ops
}

fn problems(declaration: &str) -> Vec<String> {
    let (_, problems) = Plan::of(&toml::from_str(declaration).unwrap(), &ops());
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
    let (plan, problems) = Plan::of(&toml::from_str(STACK).unwrap(), &ops());
    assert!(problems.is_empty(), "{problems:?}");
    let steps: Vec<_> = plan.steps.iter().map(|s| (s.op.as_str(), s.inputs.clone())).collect();
    assert_eq!(steps, [("file", vec![]), ("t.upper", vec![vec![0]]), ("t.count", vec![vec![1]])]);
    let outputs: Vec<_> = plan.outputs.iter().map(|o| (o.name.as_str(), o.from, o.retain, o.eager)).collect();
    assert_eq!(outputs, [("source", 0, Retain::Cache, false), ("upper", 1, Retain::Transient, false), ("count", 2, Retain::Transient, true)]);
}

#[test]
fn what_is_wrong_with_a_stack() {
    assert_eq!(problems("[[stack]]\nop = \"t.upper\""), ["stack[0]: `t.upper` takes a value, and nothing is above it"]);
    assert_eq!(
        problems("[[stack]]\nop = \"file\"\nfile = \"a\"\n[[stack]]\nop = \"file\"\nfile = \"b\""),
        ["stack[1]: `file` is a source below the top: it would drop what is above it"]
    );
    assert_eq!(
        problems("[[stack]]\nop = \"file\"\nfile = \"a\"\n[[stack]]\nop = \"t.count\"\n[[stack]]\nop = \"t.upper\""),
        ["stack[2]: its `in` takes file, and is given table"]
    );
    assert_eq!(problems("[[stack]]\nop = \"x.nothing\""), ["stack[0]: `x.nothing`: no extension provides it"]);
    assert_eq!(
        problems("[[stack]]\nop = \"file\"\nfile = \"a\"\n[[stack]]\nop = \"output\"\nname = \"a\"\n[[stack]]\nop = \"output\"\nname = \"a\""),
        ["stack[2]: a second output named `a`"]
    );
    assert_eq!(problems("[[stack]]\nop = \"t.join\""), ["stack[0]: `t.join` takes a value, and nothing is above it"]);
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
name = "repeated(times)"
"#;

#[test]
fn a_graph_layer_runs_what_its_outputs_reach() {
    let ops = ops();
    let (plan, problems) = Plan::of(&toml::from_str(GRAPH).unwrap(), &ops);
    assert!(problems.is_empty(), "{problems:?}");
    let node = Bundle::new(GRAPH, &[("a.txt", "a"), ("b.txt", "b")]);
    let before = runs();
    let (joined, _) = plan.output("joined").unwrap();
    let (made, reads) = evaluate(&plan, &ops, &node, &[(joined, None)]);
    assert_eq!(bytes(made[0].as_ref().unwrap()), b"aBa");
    // `unused` is pruned; `t.repeat` is below the output asked for.
    let ran = runs();
    assert_eq!([ran[0] - before[0], ran[2] - before[2], ran[3] - before[3]], [1, 1, 0]);
    assert_eq!(reads.files.keys().collect::<Vec<_>>(), ["a.txt", "b.txt"]);
    // A family's output, by its parameter's value; the tap inside the graph.
    let (repeated, times) = plan.output("repeated/3").unwrap();
    let (b, _) = plan.output("b").unwrap();
    let (made, _) = evaluate(&plan, &ops, &node, &[(repeated, times.as_deref()), (b, None)]);
    assert_eq!(bytes(made[0].as_ref().unwrap()), b"aBaaBaaBa");
    assert_eq!(bytes(made[1].as_ref().unwrap()), b"B");
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
fn a_value_taken_once_is_moved() {
    let ops = ops();
    let (plan, _) = Plan::of(&toml::from_str(STACK).unwrap(), &ops);
    let node = Bundle::new(STACK, &[("a.txt", "abc")]);
    let before = shared();
    // `upper` alone: the file's bytes are taken by it alone, and moved.
    let (count, _) = plan.output("count").unwrap();
    let (made, _) = evaluate(&plan, &ops, &node, &[(count, None)]);
    assert_eq!(made[0].as_ref().unwrap().as_ref(), &Value::Table(toml::from_str("len = 3").unwrap()));
    assert_eq!(shared(), before);
    // Asked for with `source`, the bytes are shared: upper copies them, the source is unchanged.
    let (source, _) = plan.output("source").unwrap();
    let (upper, _) = plan.output("upper").unwrap();
    let (made, _) = evaluate(&plan, &ops, &node, &[(source, None), (upper, None)]);
    assert_eq!((bytes(made[0].as_ref().unwrap()), bytes(made[1].as_ref().unwrap())), (&b"abc"[..], &b"ABC"[..]));
    assert!(shared() > before);
}

#[test]
fn a_failure_reaches_what_takes_from_it() {
    let ops = ops();
    let (plan, _) = Plan::of(&toml::from_str(STACK).unwrap(), &ops);
    let node = Bundle::new(STACK, &[]);
    let (count, _) = plan.output("count").unwrap();
    let (made, _) = evaluate(&plan, &ops, &node, &[(count, None)]);
    assert_eq!(made[0].as_ref().unwrap_err().as_ref(), "stack[0]: no such file");
}

#[test]
fn a_kept_output_holds_while_what_it_read_does() {
    let dir = std::env::temp_dir().join(format!("pe2-graph-store-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let engine = Engine::new(ops(), Some(Store::at(dir.clone()).unwrap()));
    let node = Bundle::new(STACK, &[("a.txt", "abc")]);
    let (plan, _) = Plan::of(&node.fields, &engine.ops);
    let first = engine.output(&plan, &node, "source", false).unwrap();
    assert!(!first.kept);
    let again = engine.output(&plan, &node, "source", true).unwrap();
    assert!(again.kept);
    assert_eq!(bytes(again.value.as_deref().unwrap()), b"abc");
    // A transient output is never kept.
    assert!(!engine.output(&plan, &node, "upper", false).unwrap().kept);
    // The file changed: made again.
    node.files.lock().unwrap().insert("a.txt".into(), b"abcd".to_vec());
    let changed = engine.output(&plan, &node, "source", true).unwrap();
    assert!(!changed.kept);
    assert_eq!(bytes(changed.value.as_deref().unwrap()), b"abcd");
    // Swept: the trace no longer live, its value and blob go.
    let swept = engine.store().unwrap().sweep(&HashSet::new());
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
    let (plan, _) = Plan::of(&one.fields, &engine.ops);
    assert!(!engine.output(&plan, &one, "source", false).unwrap().kept);
    assert!(!engine.output(&plan, &two, "source", false).unwrap().kept);
    // Each found kept by its own trace; the other's file changing leaves it.
    two.files.lock().unwrap().insert("a.txt".into(), b"xyz".to_vec());
    assert!(engine.output(&plan, &one, "source", false).unwrap().kept);
    let count = |sub: &str| std::fs::read_dir(dir.join(sub)).unwrap().count();
    assert_eq!((count("traces"), count("values")), (2, 1));
    let _ = std::fs::remove_dir_all(&dir);
}
