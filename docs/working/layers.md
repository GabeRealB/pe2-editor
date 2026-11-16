# Layers and overrides

A project is a stack of layers. What you see is all of them combined; your edits go to one of them, the
**active layer**. The game's own layer, at the bottom, is read-only.

## The Layers pane

Each layer is a card, top to bottom. Hover a card to see what the layer changes and what it depends on.

| To | Do |
|---|---|
| Make a layer the one your edits go to | Click it, or right-click it and choose **Make active** |
| Hide a layer from what you see | Its **eye** |
| Keep edits out of a layer | Its **lock** |
| Work on one layer alone | Right-click it, **Solo**: the layers above it are hidden, the others locked, and it becomes active. **End the solo** puts everything back. |
| Reorder layers | Drag a card, or right-click it and choose **Move up** or **Move down** |
| Add a layer | **New layer**, then name it |
| Remove a layer from the project | Right-click it, **Take out of the stack**. Its files stay on disk. |

The eye, the lock and solo are your view of the project: they are kept for you, and change nothing for anyone else.

A layer can be locked for other reasons, which its lock explains when you hover it: a generated layer is written
only by filling it, and a folder brought in read-only cannot be edited. A layer cannot be moved below one it
depends on.

## Overrides

When you edit an asset from a layer below, the editor copies it into the active layer first: an **override**. You
can also do this yourself: right-click the asset in the Assets pane and choose **Override in** *layer*. The override
holds only the files you change, and of each only what you change - so what your layer shares follows your edits,
however large the file; everything else still comes from below. A plain file - code, for instance - is
copied whole when you override it, so that you can edit it in the `assets` folder with your own tools.

To give up your version, right-click the asset and choose **Take the version below**.

## Kept on this computer only

Some of what you make is yours alone: scratch files, notes, test assets. An entry kept **on this computer only** is
stored in the project's local folder, never shared through version control with the project. It is greyed in the
Assets pane; the **eye** beside the filter hides what is yours alone, and shows it again.

| To | Do |
|---|---|
| Keep an entry, and all in it, on this computer only | Right-click it, **Keep on this computer only** |
| Share it with the project again, with the folders above it | Right-click it, **Share it** |
| Keep what is made by some names on this computer, wherever it is made | List the names under `[local] patterns` in `project.toml`, as a `.gitignore` names them |

What is made or moved into an entry kept on this computer is kept there too. A new project lists the names editors
and systems leave beside files - swap and lock files, backups, folder settings - as yours alone; change the list as you
need. Files your editors make for themselves while you edit in the `assets` folder never reach the project at all.

From a terminal: `pe2-cli project local <path>` and `pe2-cli project share <path>`.

## When the layer below changes

If the asset under your override changes later, the editor combines the two: your changes are kept, and the other
changes are taken in. If both changed the same thing, the override is out of date and listed in
[Problems](problems.md), with a table of what changed below, what you have, and what each was made from. Make its
layer active, then choose:

- **Keep mine**: your version wins, and is now based on what is below;
- **Take below**: your version is dropped.

## The Layer pane

The **Layer** pane lists everything the active layer does: the assets it overrides, moves and deletes, each with
its state (applied, out of date, missing, in conflict), and the assets it makes. From here too you can **Keep
mine**, **Take below**, or **Drop** what the layer does to an asset.
