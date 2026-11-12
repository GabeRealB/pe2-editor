# Finding and viewing assets

## The Assets pane

The **Assets** pane shows the project as folders and assets. When it is wide, it has a folder tree on the left and
the open folder on the right, with **Back**, **Forward**, **Up a folder** and the folder's path above. Narrow, it is a
single tree.

- Click a folder to open it, and an asset to open it as a document.
- Assets with pictures show a thumbnail; others show their type's icon.
- `*` marks an asset with unsaved changes. A dot marks what a layer you can edit holds.
- The **files** switch also shows plain files, such as source code, beside the assets.

To find something, type in **Filter by path or type**: part of a name or path, or a kind of asset.

## Previewing

An open asset is shown in its **Preview**:

- pictures are shown on a canvas: the wheel zooms, dragging pans, double-click fits. Hover to see the position and
  what is there; click a marked area to select that part. See [Pictures](../views/pictures.md);
- sounds, music and movies play in the player: **Play**/**Pause**, a slider to seek, the time, and the volume with
  a mute switch. See [Sound](../views/sound.md) and [Movies](../views/movies.md);
- models are shown in 3D. See [Models](../views/models.md).

An asset with nothing to preview shows its files instead. **Files** (`Ctrl+2`) always does: text with line numbers
and colors for code, other files as bytes. It is read-only, and follows the file when it changes.

Drag an asset onto the document area to open it in a tab of its own.

## Inside an asset

The **Contents** pane shows the parts inside the open asset - a model's parts, a sound bank's slots, a picture's
palettes - and below them its [stack](../making/stacks.md). Selecting a row selects it everywhere; double-click
follows it where it leads. Rows can be folded.

Some parts can be rearranged here: drag a row onto another to put it before, after or inside it, or drag an asset
from the Assets pane onto a row to attach it. While you drag, a label by the pointer says what the drop would do,
or, in red, why it cannot. Right-click a row to **Rename** it or for what its kind offers.

## Following references

The **References** pane, and the Inspector's **References** tab, list what the selected asset uses (**References**)
and what uses it (**Referenced by**). Click one to go to it. **Back** (`Alt+←`) returns.
