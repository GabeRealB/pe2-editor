# The work graph

`project::graph` (with `graph::exec` and `graph::store`), `project::imports`.

## From stacks to plans

Every asset has a **stack**: a list of layers - stored files, importers, operations, graphs, outputs - each taking
what the one above passes down. A graph layer is several operations wired together. Before anything runs, a stack is
lowered to a **plan**: a graph of steps, each an operation with its inputs, settings and outputs. Operations come from
extensions (see [The extension runtime](../extensions/runtime.md)) or from the project itself (`pe2.graph`
operations, inlined into the plan where they are used).

## Outputs and how they are asked for

An output is named by its path - any groups, then its own name (`unit/c`) - which `pe2_path::Address` checks where the
plan is laid out. A reference names one as a URL names a page, `uid://<uid>/<path>[?<query>][#<fragment>]`
(`pe2_path::ResourcePath`), and what reaches the engine is an `Asked`: the path and the query.

- A **family** is an output whose operation takes a parameter: `picture/night` is the member `night` of the family
  `picture` (`Plan::output`); the family asked for as it is gives its operation no parameter.
- A **query** says how the output is made (`palette=night`). An operation declares the keys it takes
  (`OpDef::query`); `Engine::output_in` refuses any other by name before anything runs, and the operation reads it as
  `Call::query`. The member and the query go together as `exec::Asking`, into the key an output is kept under, so two
  ways of making one output are two results.
- A **fragment** is the part of what is made that a reader takes. It never reaches the engine and is never part of a
  key: what is made is the same whichever part is read.

A read of another node's output is recorded by the node and what was asked (`<node>#<path>?<query>`), and checked again
by that.

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

## Running programs

A build is a graph like any: its actions are `run` boxes (`project::ext::run`), each a program on files. What it takes is
laid out in a folder of its own, the program is started there with arguments whose placeholders name that folder, its
files, its outputs, the configuration, a setting of what it takes, or its `values`; what it writes under the names it
gives, or prints, is what it makes. Only programs extensions offer run (`<extension>:<program>`), and only for a project
trusted as a whole. Its version names the extensions offering programs by their hashes, so a tool changed remakes what
it made; outputs it names in `rewrite` have the folder written `tree`, so what is made of them is the same wherever it
ran. Which programs, which arguments, how many steps a unit takes: all of it is the graph's, none of it the editor's - a
package is built by whatever operation its node names (`pe2.packages`: what it gives a build).

## Generators

A generator (`pe2.graph@generator`) is a node whose outputs are operations - declarations, each defining one - as a
build system makes its build from a description. When definitions are taken, the generators are collected; what they
make is made when the operations are next asked for, for the configuration built, and kept until it or they change,
each output defining `uid://<generator>/<output>` (`/<output>` naming another of its own). A generator is laid out
with the project's own definitions alone and made of its own settings and the configuration, no other node, so it
cannot depend on what it makes; a thread making them is given those definitions alone when it asks again. Plans are
laid out afresh with the definitions of the configuration built, so two configurations may build a package with two
graphs, their results kept apart by what each read.

## Configurations

A configuration is a release, a target and properties declared in `project.toml`. What reads the configuration
records the part it read, so two configurations that say the same thing share their results. A value that does not
fit its property is a problem, not an error: the default holds.

## Where to look

`project::graph` (stacks, graph layers, lowering, the built-in operations and their labels), `project::ext::run` (the
`run` box), `Extensions::generated` (generators), `graph::exec` (the
evaluator, traces, keys), `graph::store` (what is kept), `project::imports`, `Project::configs` in `project`.
