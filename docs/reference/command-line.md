# Command line

`pe2-cli` does from a terminal what the editor does in its window. Run it in the project's folder, or name the
folder with `--project`. `pe2-cli <command> --help` describes any command.

While the editor has the project open, changes made with `pe2-cli` go through the editor and appear in its
[history](../working/history.md). Otherwise `undo` and `redo` still take back what earlier commands did.

## Projects

| Command | What it does |
|---|---|
| `pe2-cli project new <folder>` | Makes an empty project. |
| `pe2-cli project extract` | Fills the project from your discs. `--releases` chooses which. |
| `pe2-cli project status` | Says whether the project is filled. |
| `pe2-cli project verify` | Checks every filled file, without the discs. |
| `pe2-cli project problems` | Lists the project's problems. |
| `pe2-cli project trust` | Shows and changes which places of the project you trust. |
| `pe2-cli project mount locate <name> <folder>` | Says where your discs are on this computer. |

## Editing

Paths are as the project folder shows them. Edits go to the highest layer you can edit, or to the one named with
`--layer`.

| Command | What it does |
|---|---|
| `pe2-cli project write <path> [<file>]` | Writes a file, from a file or from standard input. A `node.toml` makes its folder an asset. |
| `pe2-cli project mkdir <path>` | Makes a folder. |
| `pe2-cli project rm <path>` | Deletes an asset or a file. |
| `pe2-cli project mv <from> <to>` | Renames or moves. |
| `pe2-cli project override <path>` | Makes the layer's own version of an asset from below. |
| `pe2-cli project drop <path>` | Gives up the layer's version of an asset. |
| `pe2-cli project keep-mine <path>`, `take-base <path>` | Settles an out-of-date override. |
| `pe2-cli project undo`, `redo` | Takes back the last change, or does it again. |

## Building and events

| Command | What it does |
|---|---|
| `pe2-cli project build` | Builds the project. `--config` chooses the configuration; `--clean` cleans first. |
| `pe2-cli project clean` | Removes what a build made. |
| `pe2-cli project signal <event>` | Signals an event, and waits for what it sets off. |
| `pe2-cli project output <asset>` | Makes an asset's outputs, and lists each file with its size. |
| `pe2-cli project link add <where> <built file>` | Links a built file to where another program looks for it, on this computer. |

## Layers, configurations and extensions

| Command | What it does |
|---|---|
| `pe2-cli project layer add` | Adds a layer. |
| `pe2-cli project config add <name>` | Adds a configuration. `--release` and `--target` say what it builds. |
| `pe2-cli project config declare <property>` | Declares a property: `--type` or `--values`, and `--default`. |
| `pe2-cli project config set <configuration> <property> <value>`, `unset` | Sets a property, or puts it back to its default. |
| `pe2-cli project config list` | Lists the configurations and their properties. |
| `pe2-cli project extension list`, `add`, `update`, `remove` | Manages the project's extensions. |
| `pe2-cli project extension setup` | Shows what the project's extensions download, and downloads it ahead of time. |
| `pe2-cli fetched list`, `clean` | Shows and removes what extensions downloaded onto this computer. |

## Serving and agents

| Command | What it does |
|---|---|
| `pe2-cli project serve` | Keeps the project folder available to other programs without the editor. `--stop` stops it. |
| `pe2-cli mcp` | Serves the project to an AI agent. See [AI agents](../extending/agents.md). |

## For extension authors

| Command | What it does |
|---|---|
| `pe2-cli extension hash <manifest>` | Fills in the checksums of what an extension downloads. `--write` writes them. |
| `pe2-cli extension pins <manifest>` | Checks that everything an extension downloads is still there. |

## Other

| Command | What it does |
|---|---|
| `pe2-cli licenses` | Lists what the editor is built with, and the licenses. See [Licenses](licenses.md). |
