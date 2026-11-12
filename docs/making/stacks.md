# Stacks

An asset is made by its **stack**: a list of steps, read top to bottom. Each step takes what the one above it gives,
and passes something on down. Along the way, steps can make the asset's **outputs** - a picture, a sound as the game
plays it, a compiled file - for the editor to show, for other assets to use, and for builds.

Most assets get their stack from their kind, and you never need to touch it. You change it when you want an asset
made differently, or want something more out of it.

## Seeing a stack

- The Inspector's **Stack** tab lists the steps, numbered, with their settings. Selecting a step shows its settings
  at the top of the tab.
- The **Contents** pane lists them under **Stack**, below the asset's parts.

Problems with a step are listed under the steps, and in [Problems](../working/problems.md).

## Kinds of step

| Step | What it is |
|---|---|
| **Stored file** | One of the asset's own files. |
| **The version below** | What the layer below has of this asset, unchanged. Used by overrides that build on it. |
| **Importer** | Turns the file the asset was imported from into the asset. Where several can, you choose one. |
| **Operation** | One piece of work, such as decoding, converting or compiling, with its settings. |
| **Graph** | Several operations wired together. Its result is passed on down. See [The graph editor](graphs.md). |
| **Output** | Makes what reaches it an output of the asset, under a name. |
| **Preview** | An output shown in a document tab of its own. |

## Changing a stack

| To | Do |
|---|---|
| Add a step at the end | **Add layer**, under the steps. It offers what fits: operations that take what comes down, **Output**, **Preview**, and **Graph**. At the top of the stack it also offers the asset's stored files and **The version below**. |
| Add a step before or after another | The step's menu (**⋯**), **Add above** or **Add below** |
| Move a step | Drag its number onto another, or its menu's **Move up** and **Move down** |
| Remove a step | Its menu's **Remove**, or select it and press `Delete` |
| Change a step's settings | Select it, and edit its settings at the top of the Stack tab |
| See a graph step | **Open** on its row, or `Ctrl+3` |

Like any edit, these make the asset a document with unsaved changes, and can be undone.

## Outputs

An **Output** step has these settings:

| Setting | What it means |
|---|---|
| **Name** | What the output is called. Other assets refer to it as *asset*`#`*name*. |
| **Kept** | **Not kept**, **In the cache** (kept on this computer, made again when needed), or **With the build**. |
| **Made** | **When asked for** (when something needs it) or **As soon as it can be**. |
| **Made on**, **Stale on**, **Goes to**, **Ran**, **Changed**, **Failed** | Which events make it, which make it out of date, where builds put it, and which events it signals. See [Events](events.md). |

A field that refers to an output shows the asset's name followed by `#`*output*.

To make an asset's outputs from a terminal and see what they are, run `pe2-cli project output <asset>`.
