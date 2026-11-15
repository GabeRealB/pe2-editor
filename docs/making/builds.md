# Builds

A package - code the project builds, such as an overlay of the game - is built by an operation: a graph of the programs
that compile and link it, and of a few steps that need no program. Which programs, with which arguments, in how many
steps, is the project's to say; the editor knows no compiler. One project can build the same packages several ways,
one a [configuration](configurations.md).

## What builds a package

A package's stack gathers what it reaches (**Sources**), then names the operation that builds it, then makes its
outputs of what that gives:

```toml
[[stack]]
op = "pe2.packages@package"
# ... what the package is ...

[[stack]]
id = "sources"
op = "pe2.packages@gather"

[[stack]]
id = "built"
op = "uid://<the build>/package"

[[stack]]
op = "output"
name = "native"
value = { from = "built.image" }
deploy = "build/{config}/"
```

The build operation takes the gathered sources and makes the package's image (`image`), its compiled objects
(`objects`), what other packages link against (`symbols`) and its sources with their values resolved (`resolved`).
A library's makes its objects alone.

## Running a program

Each program a build runs is a **Run a program** box (`run`):

| Setting | What it is |
|---|---|
| `program` | The program, as an extension offers it: `pe2.gcc-psx:cc1`. |
| `args` | Its arguments, with placeholders. An argument that is itself a list is a group: `["-iquote", "{tree}/{given.quote}"]` gives `-iquote` before each folder. |
| `outputs` | The files it makes, by name. |
| `stdout` | A file holding what it prints, where that is what it makes. |
| `rewrite` | Outputs that are text, in which the folder it ran in is written `tree`. |
| `makes` | The kind of value it makes (files where unsaid). |
| `env` | What it runs with besides what the editor has, as a table: `{ INCLUDE = "{tree}/include" }`. |
| `cwd` | A folder within what it takes to run in (where unsaid, the folder it all is laid out in). |

What it takes is laid out in a folder of its own. The placeholders are `{tree}` (that folder), `{in}` (the first file
it takes), `{inputs}`, `{files.<folder>}` (the files under a folder), `{out}`, `{out.<n>}`, `{given.<key>}` (a setting
of what it takes: a unit's `object`, its `quote` and `angle` folders), `{values.<key>}` and `{config.release}`,
`{config.target}`, `{config.name}`, `{config.properties.<p>}`. One that is a list makes one argument for each item.

Only programs an extension offers run, and only once you trust the project as a whole (**Trust**, or
`pe2-cli project trust --all`). Tools are extensions of their own - a compiler, a set of binutils - offering their
programs and downloading them when trusted; see [Managing extensions](../extending/extensions.md).

## The pieces of a build

Besides `run` boxes, a build wires what `pe2.packages` gives: **Units of a language** (`units`: a unit each, its whole
tree with it first), **Files put in the tree** (`with`), **Slots and values** (`resolve`), **Objects** (`collect`),
**What links it** (`link_inputs`: its objects and its dependencies', its link script, what it links against) and
**Published symbols** (`published`, from a symbol listing). **For each** runs an operation for each unit. A platform's
extension gives the last step: `pe2.psx@finish` makes the image the PlayStation loads.

## Generated builds

A build is long to write by hand. A [generator](own-operations.md#generators) makes it from a description instead:
`pe2.packages@generate` makes the parts of a build - a unit's steps for each language, a first pass, a package's and a
library's build - from what to run for each:

```toml
first_pass = { program = "pe2.aro-cpp:cpp", args = ["-E", "-P", "-DPE2_FIRST_PASS", "-o", "{out}", "{in}"] }

[languages.c]
steps = [
    { program = "pe2.aro-cpp:cpp", args = ["-E", "-P", "-o", "{out}", "{in}"], outputs = ["unit.i"] },
    { program = "pe2.gcc-psx:cc1", args = ["-O2", "-o", "{out}", "{in}"], outputs = ["unit.s"] },
    { program = "pe2.binutils-mipsel-maspsx:as", args = ["-o", "{out}", "{in}"], inputs = ["1", "unit"] },
]

[link]
steps = [
    { program = "pe2.binutils-mipsel:ld", args = ["-T", "{files.script/}", "-o", "{out}", "{files.objects/}"], outputs = ["image.elf"] },
    { program = "pe2.binutils-mipsel:objcopy", args = ["-O", "binary", "-j", ".image", "{in}", "{out}"], outputs = ["image.bin"] },
]
finish = "pe2.psx@finish"

[publish]
steps = [
    { program = "pe2.binutils-mipsel:ld", args = ["--unresolved-symbols=ignore-all", "-T", "{files.script/}", "-o", "{out}", "{files.own/}"], outputs = ["layout.elf"] },
    { program = "pe2.binutils-mipsel:nm", args = ["-g", "--defined-only", "{in}"], stdout = "listing.txt" },
]
```

Each step takes the step before it, or what `inputs` names: `unit`, `linked` (what links the package), an earlier step
by its place. The last step of a language makes the unit's object.

## A build for each configuration

A generator's description may say more for a configuration: `[configs.<name>]` is laid over it when that one is built.
So one configuration builds with one compiler, another with another, from the same packages:

```toml
[configs.usa_clang.languages.c]
steps = [
    { program = "pe2.clang:clang", args = ["-target", "mipsel-none-elf", "-S", "-o", "{out}", "{in}"], outputs = ["unit.s"] },
    { program = "pe2.binutils-mipsel:as", args = ["-o", "{out}", "{in}"], inputs = ["0", "unit"] },
]
```

What each configuration makes is kept apart; building one leaves the other's as it was.
