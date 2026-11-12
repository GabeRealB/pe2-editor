# Importing and exporting

## Importing

Drop files on the window, or choose **File ▸ Import…** and list them. The Import dialog offers what the files can
become:

| Choice | What it does |
|---|---|
| **New node:** *kind* | Makes a new asset of that kind from the file, beside what you are working on. |
| **Replace the source of** *asset* | Makes the selected asset take the file as its new source. |
| **Add to** *asset* | Adds the files to the selected asset. |

What a file can become depends on the extensions: for example, a layered picture (OpenRaster) into a texture, a WAV
into a sound, a video into a movie, a UFO font into a font face. If nothing fits, select the asset the files go
into first.

An imported asset remembers its source file. Fields it takes from the file are marked **from the source** in the
Inspector; to change them, change the file and import it again.

## Exporting

Select an asset and choose **File ▸ Export…** (`Ctrl+E`). Choose a format, which part to export where an asset has
several, and where to save it. **Show its folder when it is written** opens the folder afterwards.

The formats on offer depend on the kind of asset: for example, a picture as PNG, a sound as WAV, a movie as a video
file.

## Opening in another program

Each file in the Inspector's **Files** tab has a button to open it in another program. **Settings ▸ External tools**
chooses which program, for each kind of file: image editor, audio editor, video player, text editor, 3D modeling,
and a hex editor for anything else. Leave one empty to use your system's default.

To edit an asset's files in another program and have the changes come back, see
[Editing in other programs](other-programs.md).
