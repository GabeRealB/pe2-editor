# Pictures

Textures, backgrounds and atlases are **Images**. An image is drawn through a palette, can have several looks, and
is cut into cells that the game draws separately.

## Viewing

An image's **Preview** shows it on a canvas: the mouse wheel zooms, dragging pans, double-click fits it, and hovering
shows the position and what is there.

| Control | What it does |
|---|---|
| **Upscaling** | How the picture is drawn larger: **Nearest (exact)** shows each pixel as it is; **Bilinear**, **Sharp bilinear**, **Scale2x** and **Scale3x** smooth it for viewing. |
| **Grid** | A pixel grid, which follows the zoom. |
| **Variants** | Where an image has several looks, choose one, or step with **Previous variant** and **Next variant**. |
| **Cells and pages** | Draws the outlines of the image's cells over it. Click one to select it. |

The default upscaling is set in **Settings ▸ Appearance**.

## Palettes and variants

The Inspector's **Palettes** tab lists the image's variants - the same picture through different palettes. Reorder or
remove them, **Add variant**, or **Recolor a cell…** to give one cell different colors. In the Contents pane, drop a
cell on a palette to see it drawn through that palette.

## Cells

The **Cells** view and the Inspector's **Cells** tab show the image's cells. Select one to see it on its own.

## Color modes

An image's **Mode** is how its colors are stored: **Indexed, 4 bpp** (16 colors), **Indexed, 8 bpp** (256 colors) or
**Direct color, 16 bpp**.

## Video memory

The game loads pictures into the console's video memory. A **VRAM slot** is a place there; its **Place** view shows
where it lies.

The **VRAM** pane (**Window ▸ VRAM**) shows the whole of video memory: the slots, and what the selected asset loads
into it. Hover to see what is where; click to open an image. A setting of the images extension, in the
**Extensions** pane, draws a page grid over it.

## Importing and exporting

- **Texture, from layers** makes an image from a layered OpenRaster file (`.ora`), which most painting programs can
  save.
- **Background picture** makes one from a background file (`.bs`).
- **File ▸ Export…** saves any picture as a PNG image.
