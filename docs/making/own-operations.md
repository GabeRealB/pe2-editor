# Your own operations and templates

A project can define operations and templates of its own, without writing an extension:

- an **operation** is a graph you can use as one step, in any stack or graph, as often as you like;
- a **template** is a recipe for new assets, offered under **File ▸ New node** or when importing files.

Both are assets, made by writing their declaration: make a new folder in the
[project folder](../working/other-programs.md) and save a `node.toml` in it with the text below. From a terminal:
`pe2-cli project mkdir <folder>`, then `pe2-cli project write <folder>/node.toml <file>`.

## Operations

Start from an empty operation:

```toml
[[stack]]
op = "pe2.graph@operation"
label = "My operation"
ops = {}
```

Open it in the [graph editor](graphs.md) (`Ctrl+3`) and build it as any graph. Inside an operation, three more boxes
say what it is from outside:

| Box | What it is |
|---|---|
| **Input** | Something the operation takes: its name, its kind, and whether it takes several. |
| **Setting** | A setting users of the operation fill in: its kind or values, its default, and its label. Boxes read it where their settings say `{ setting = "name" }`. |
| **Output** | What it makes. The first is its result; the others are extra outputs. |

Once saved, the operation is offered by its label wherever an operation is: in a graph's right-click menu, and under
**Add layer** in a stack where its input fits. Where it is used, the Inspector shows its settings as fields.

**For each** runs an operation once for each item of a list, which makes it the way to repeat work.

## Templates

A template says what kind of asset it makes and the stack it starts with. For example, this one makes a string table
that follows the project's language:

```toml
[[stack]]
op = "pe2.graph@template"
label = "Translated string table"
node_type = "pe2.dialogue@string_table"
makes = [
    { op = "pe2.dialogue@string_table", strings = {} },
    { op = "uid://<your translation operation>" },
    { op = "output", name = "strings" },
]
```

| Key | What it means |
|---|---|
| `label` | Its name in menus. |
| `node_type` | The kind of asset it makes. |
| `makes` | The stack a new asset starts with. Use `{file}` for the file it was made from. |
| `files` | The files it can be made from, such as `["*.png"]`. Without it, it makes assets from nothing. |
| `offered` | `false` to keep it out of menus. |

A template that takes no files is listed under **File ▸ New node**. One that takes files is offered in the
[Import dialog](../working/import-export.md) when you import a file it matches.
