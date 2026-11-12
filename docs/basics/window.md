# The window

## Menus

| Menu | What it holds |
|---|---|
| **File** | New and open projects, import, new assets, save and revert, export, reload, quit |
| **Edit** | Undo and redo, settings |
| **Window** | Show or hide any pane, documents only |
| **View** | Refresh, reset the layout, zoom, the performance overlay |
| **Go** | Back, forward, up a folder, the command palette |

Beside the menus is the path of what you are looking at; click a part of it to go there. At the top right are the
**configuration** you work in, with **Build** and **Clean**, and the **mode** (below).

## Panes

The window is made of panes. Each is a tab you can drag into another group, split off, resize or close.
**Window** shows or hides any of them.

| Pane | What it shows |
|---|---|
| **Assets** | The project's folders and assets. |
| **Document** | The asset you opened, in one of its views. |
| **Contents** | The parts inside the asset you opened, and its stack. |
| **Inspector** | The asset or part that is selected: its fields, stack, files and references. |
| **References** | What the selected asset refers to, and what refers to it. |
| **Layers** | The project's layers, and which one is active. |
| **Layer** | What the active layer changes. |
| **History** | Every change made in this session. |
| **Problems** | What needs your attention. |
| **Project** | The project at a glance: its assets by type, releases, extensions, layers. |
| **Extensions** | The extensions the project uses, and those you can add. |
| **Events** | The events of the project, and a button to signal each. |
| **Tests** | Tests of the extensions, for those who write them. |
| **Settings** | Your settings and the project's. |

Extensions can add panes of their own, such as **VRAM**.

## Documents and tabs

Opening an asset shows it in a document tab. A tab in italics is a preview: the next asset you open replaces it.
Double-click the tab, or right-click it and choose **Keep open**, to keep it. A tab whose title ends in `*` has
unsaved changes.

Right-click a tab for more:

| Choice | What it does |
|---|---|
| **Another** *pane* | Opens a second pane of the same kind. |
| **Pin to** *asset* / **Follow the selection** | Keeps a pane on one asset, or lets it follow what you select. |
| **Move to a new window**, **Move to the main window** | Moves the pane into a window of its own, or back. |

Middle-click a tab to close it.

An asset can be shown in several views. Their buttons are at the right end of the tab strip:

- **Preview** (`Ctrl+1`): the asset as it looks or plays;
- **Files** (`Ctrl+2`): its files, as text or bytes;
- **Graph** (`Ctrl+3`): its [graph](../making/graphs.md), where it has one;
- and views of its kind, such as **3D** for a model. See [Built-in views](../index.md#contents).

**Documents only** (`Ctrl+Space`) hides everything but the documents. Press it again to bring the panes back.

## Modes

A mode is a layout of panes you switch to. The editor comes with **Edit** and **Changes**. Use the mode menu at the
top right to switch, make a **New mode…** from the current one, **Reset this mode** or **Delete this mode**.

## Finding your way

- **Back** and **Forward** (`Alt+←`, `Alt+→`, or the mouse's side buttons) follow where you have been.
- **Up a folder** (`Alt+↑`) shows the folder of what you are looking at.
- The **command palette** (`Ctrl+Shift+P` or `F1`) runs any command by name.

## The status bar

The bar at the bottom shows, from the left:

- the **Assets** drawer, which opens the Assets pane over the window (**Dock** keeps it in place), and the active
  layer;
- messages about what just happened;
- what is running, such as a build, with **Cancel**;
- the number of problems, unsaved documents (with Save and Revert for each), and changes on disk not yet taken in;
- whether AI agents may use the editor, and whether one is;
- where the project's folder is for [other programs](../working/other-programs.md), and the number of assets.

Click an item to go to what it is about.
