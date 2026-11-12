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
| Add a layer | **New layer**, then name it and choose **Assets** (assets and plain files) or **Files** (plain files only, such as code) |
| Remove a layer from the project | Right-click it, **Take out of the stack**. Its files stay on disk. |

The eye, the lock and solo are your view of the project: they are kept for you, and change nothing for anyone else.

A layer can be locked for other reasons, which its lock explains when you hover it: a generated layer is written
only by filling it, and a folder brought in read-only cannot be edited. A layer cannot be moved below one it
depends on.

## Overrides

When you edit an asset from a layer below, the editor copies it into the active layer first: an **override**. You
can also do this yourself: right-click the asset in the Assets pane and choose **Override in** *layer*. The override
holds only the files you change; everything else still comes from below.

To give up your version, right-click the asset and choose **Take the version below**.

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
