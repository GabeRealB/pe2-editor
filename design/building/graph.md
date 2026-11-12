# The work graph

`project::graph` (with `graph::exec` and `graph::store`), `project::imports`.

## From stacks to plans

Every asset has a **stack**: a list of layers - stored files, importers, operations, graphs, outputs - each taking
what the one above passes down. A graph layer is several operations wired together. Before anything runs, a stack is
lowered to a **plan**: a graph of steps, each an operation with its inputs, settings and outputs. Operations come from
extensions (see [The extension runtime](../extensions/runtime.md)) or from the project itself (`pe2.graph`
operations, inlined into the plan where they are used).

## Pulled, not pushed

A plan is evaluated by pulling: a step is made when something asks for its output, and asks for its own inputs
first. So only what is needed is made.

Choice is part of the graph rather than of the code around it:

- any step can have an `active` input; where it is false, the step makes nothing, nor does anything taking from it,
  except `merge`, which takes the first of its inputs that is made;
- a step's `active` is evaluated before its inputs, so a branch not taken is never made, and only the taken branch's
  reads are recorded;
- `config`, `equals`, `select`, `overlay`, the boolean operations and `for_each` let a graph depend on the
  configuration being built, choose among other assets, and repeat work.

## Kept by what was read

Every step records what it read: its node's files, other assets' outputs, plain files of the project, the
configuration. Its result is kept under a key made from the plan and those reads, in the cache or with the build.
Finding a result kept means holding its trace to what it read now - recursively, since each output read is itself
kept by its reads - so an unchanged result is never remade, and a change remakes exactly what read it.

What makes that cheap: file hashes are remembered by time and length, and a run remembers what it already found of
other outputs.

## Imports

An imported asset's settings come from its source file. The **imports** layer runs each asset's importer and
attaches what it derived to the asset in the index. After a change, only the changed assets' imports, and the
definitions a project provides, are attached again.

## Configurations

A configuration is a release, a target and properties declared in `project.toml`. What reads the configuration
records the part it read, so two configurations that say the same thing share their results. A value that does not
fit its property is a problem, not an error: the default holds.

## Where to look

`project::graph` (stacks, graph layers, lowering, the built-in operations and their labels), `graph::exec` (the
evaluator, traces, keys), `graph::store` (what is kept), `project::imports`, `Project::configs` in `project`.
