# Editing

## The Inspector

The **Inspector** shows what is selected. At the top are its picture or icon, its name, path and type, and its uid
with **Copy**. Down the left side are its tabs:

| Tab | What it shows |
|---|---|
| **Properties** | Its fields, to edit. |
| **Stack** | How it is made. See [Stacks](../making/stacks.md). |
| **Files** | Its files and its declaration. |
| **References** | What it uses, and what uses it. |

Extensions add tabs for their own kinds of asset, such as **Palettes** for a picture.

## Changing fields

- Click a field's pencil to edit it; `Enter` keeps the change, `Esc` cancels.
- A field with choices has a list. Choosing its empty entry resets the field.
- A field the asset leaves out shows the value it has then, marked **default**. Setting it writes it into the asset.
- A field made by importing a file is marked **from the source**. Change the source file instead.
- A field that refers to another asset shows that asset; click it to go there. To change it, drag an asset onto it
  from the Assets pane. A reference to an asset that is gone is shown in red.

Hover a field's pencil to see where the change goes: the active layer, or an override in it. Your edits go to the
[active layer](layers.md); if the asset comes from a layer below, the editor makes an override of it there first.

Some kinds of asset have editors of their own in their document, such as a sound script's timeline. See
[Built-in views](../index.md#contents).

## Saving

An asset you are editing becomes a document with unsaved changes. Every preview, player and view shows your
changes straight away, but nothing is written into the project until you save.

| To | Do |
|---|---|
| Save the current document | **File ▸ Save** (`Ctrl+S`), or **Save** in the Inspector |
| Save every document | **File ▸ Save all** (`Ctrl+Alt+S`) |
| Throw away your unsaved changes | **File ▸ Revert to saved**, or **Revert** in the Inspector |

The status bar lists unsaved documents, with Save and Revert for each. If you close the editor with unsaved
documents, it asks what to do with them. If the editor stops unexpectedly, the next time you open the project it
offers your unsaved work back.

If another program changes an asset's files while you have unsaved changes to it, the Inspector says so and asks
which to keep: **Keep mine** or **Take the disk's**.

## Making, renaming, moving and deleting

| To | Do |
|---|---|
| Make a new asset | **File ▸ New node**, and choose what kind. It is made beside what you are looking at, and you name it. |
| Make an asset from a file | Drop the file on the window, or **File ▸ Import…**. See [Importing and exporting](import-export.md). |
| Rename an asset | Right-click it in the Assets pane, **Rename…** |
| Move an asset | Drag it onto a folder in the Assets pane. |
| Delete an asset | Right-click it in the Assets pane, **Delete**, or select it and press `Delete`. |

These changes take effect at once, without saving. Like every change, they can be undone.

Deleting an asset that comes from a layer below hides it in the active layer (**Delete in** *layer*); the layer
below keeps it.

## The declaration

Everything an asset is - its type, fields and stack - is written in its declaration, a short text file. To read or
edit it directly, open the Inspector's **Files** tab and unfold **Declaration**. **Save** is offered once the text is
valid; until then the first mistake is shown in red.

The same tab lists the asset's files, with their size and where each comes from: a layer, the file it was imported
from, or made by its stack. Each has a button to open it in another program; see
[Opening in another program](import-export.md#opening-in-another-program).

## Leaving an asset out of a configuration

Where an asset is built, the Inspector's header has a checkbox **In** *configuration*. Clear it to leave the asset
out of that configuration: nothing builds it there, and nothing finds it. See
[Configurations](../making/configurations.md).
