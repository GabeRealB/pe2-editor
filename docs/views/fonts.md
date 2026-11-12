# Fonts

The game draws text from small pictures of each character. The editor keeps them as fonts:

| Kind | What it is |
|---|---|
| **Font face** | A set of characters in one style, such as the dialogue font or its bold. |
| **Font atlas** | The picture the game reads characters from, and which characters go where. |
| **Encoding** | Which character each of the game's codes is. |

## Font faces

A face previews as a **sample** line of text and as the full set of its **glyphs**. Its fields set its spacing:
**Kerning pixels**, **Em, in texels** and **Outline width**.

**Font face, from UFO** makes a face from a UFO font, the format font editors such as FontForge and Glyphs save. Edit
the characters in your font editor and import them again.

## Font atlases

An atlas lists its faces in the Contents pane. Its **Placement** is either **Placed** (each character where the atlas
says) or **Generated** (laid out by the editor). **Pack unplaced** places the characters that have no place yet.
