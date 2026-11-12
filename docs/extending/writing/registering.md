# What an extension can add

An extension's `register` function receives a `Registry` and adds to it. Everything it adds is named after the
extension (`<id>@<name>`), and is offered to the user as soon as the extension is loaded.

## Kinds of asset

`Registry::value_type::<T>()` registers a Rust type as a kind of value or asset; `Registry::named_type(name)`
starts one by name. A kind of asset is described by a `TypeBuilder`:

| Method | What the user gets |
|---|---|
| `label`, `icon` | Its name and icon in the Assets pane and the Inspector. |
| `bytes`, `named`, `text`, `sniff`, `media_type` | Files recognized as this kind: by their first bytes, their name, or their text. |
| `field` | A field in the Inspector (below). |
| `stack` | The stack a new asset of this kind starts with. |
| `takes` | The files it is made from. |
| `data` | An asset made only of its fields, with no files. |
| `view`, `editor` | A view of the asset in its document, such as a timeline or a 3D view. |
| `inspector`, `inspector_tab` | A section or a tab of its own in the Inspector. |
| `contents`, `content_act`, `contents_view`, `part_sheet` | Its parts in the Contents pane, what can be done to them, and their fields. |
| `register` | Registers it. |

`Registry::extend(name)` adds views and fields to another extension's kind of asset.

## Fields

`Field::new(key)` describes one field: `label`, `default_value`, `format` (how it reads, such as hexadecimal),
`brief`, `choices`, `required`, `read_only`, `custom`.

## Pictures and thumbnails

An asset's picture is its output named `picture`. Where it has several looks, the output is a family with one member
per look. The editor makes the thumbnail from the first look by itself. `Marks` draw boxes over a picture, such as
cells, that the user can hover and click.

## Operations

`Registry::op(name)` adds an operation to stacks and graphs, with an `OpBuilder`:

| Method | What it says |
|---|---|
| `input`, `inputs`, `takes`, `port` | What it takes: one value, any number, or named ports. |
| `output`, `also`, `output_same_as`, `output_list_of` | What it makes, and extra outputs. |
| `field`, `derives`, `inverse` | Its settings; settings it works out from its input; how to undo it, for importers. |
| `parameter`, `members` | A family of outputs, such as one picture per look. |
| `run` | The work itself. |
| `prepare`, `steps`, `streams` | Work in stages: declaring what it reads first, steps written with `#[pe2_ext::steps]`, or a stream such as video frames. |
| `keeps` | How long its result is kept: `Retain::Cache`, `Build` or `Transient`. |
| `provides` | Offers it as a conversion to every asset making a value it takes. |
| `exports(label, extension)` | Offers it as an exporter in **File ▸ Export…**. |

The SDK provides kinds of value to take and make: `Image`, `Table`, `Pcm`, `Sound`, `Playback`, `Bytes`, `File<T>`,
`List<T>`, `Stream<T>`, `Marks`, `Any`, `OneOf`.

## Importers and templates

`Registry::template(name, kind, stack)` adds a template: with `.label(...)` it is offered under **File ▸ New node**;
with `.files(&["*.ora"])` it is offered for those files in the Import dialog instead. `{file}` in its stack stands
for the imported file.

## Viewers and panes

| Function | What the user gets |
|---|---|
| `Registry::viewer(kind, name, icon, make)` | A view of the document for outputs of that kind, with a button at the right of the tab strip, such as **3D**. |
| `Registry::panel(name, label, icon, make)` | A pane of its own, listed in the **Window** menu, such as **VRAM**. |

A view implements the `View` trait: `show(&mut self, ui: &mut Ui)`, called each time it is drawn. `Ui` offers
labels, buttons, fields, tables, a canvas to paint on, pictures, reference fields that take a dropped asset, a player
transport, and a GPU viewport. Edits a view makes through `Ui` (`set`, `set_step`) are steps of the user's history.

3D views draw with WebGPU, through `wasi:webgpu`, on the editor's own device, and show the result with
`Ui::gpu_viewport`. There is no GPU on the command line. The SDK has no wrapper for WebGPU yet; the built-in 3D view
is the example.

## Commands, agent tools and services

| Function | What it adds |
|---|---|
| `Registry::command(label, tool)` | A command in the command palette, which the user can bind to a key. It runs one of the extension's tools. |
| `Registry::tool(name, description, schema, run)` | A tool AI agents can call. |
| `Registry::service(name, description, schema, run)` | A service other extensions can call, declared with `provides` in the manifest. |

## Events and hooks

`Registry::event::<E>()` declares an event; `Registry::on::<E>(run)` adds a hook that runs before what an event
makes, such as a check that the tools a build needs are there. A hook that fails stops the event's work, and the
user sees why. The built-in events are `Build`, `Clean`, `FilesChanged`, `ConfigChanged` and `ProjectOpened`.

## Settings and resources

- `Registry::setting(Setting::new(key, default))` adds a setting, shown with the extension in the Extensions pane:
  the project's, or the user's (`.user()`). A changed setting reloads the extension.
- `Registry::memory(name, bytes)` asks for more memory than the default 256 MiB, up to 4 GiB.
- `Registry::test(name, description, run)` adds a test. See [Testing](testing.md).
