# Your first project

Everything you do in the editor happens in a project. You can start a new, empty one, or open one you already have.

## Making a new project

1. Choose **File ▸ New project…**.
2. Enter a folder that is empty or does not exist yet.
3. Choose **Make**.

The editor makes an empty project there and opens it. From there you can:

- bring in your own files with **File ▸ Import…** (see [Importing and exporting](../working/import-export.md));
- make new assets with **File ▸ New node** (see [Editing](../working/editing.md));
- add [extensions](../extending/extensions.md) and [layers](../working/layers.md).

From a terminal, `pe2-cli project new <folder>` makes one too.

## Opening a project

Choose **File ▸ Open project…** and enter the project's folder, or start the editor with it:

```
pe2-editor path/to/project
```

The next time you start the editor, it opens the last project again.

## What a project asks

When you open a project for the first time, it may ask you a few things:

- **What to trust.** Most extensions run in a sandbox and can do nothing to your computer. A few need to run
  programs, such as a compiler. These run only from places you trust. The editor shows what each one would
  download, and from where, before you decide.
- **What is missing**, such as extensions the project needs.
- **Which configuration to work in**, if the project has several (for example, one per release of the game).

Your answers are kept on your computer, never in the project, so a project you receive from someone else can never
arrive already trusted.

## Filling a project from your discs

A project made for the game comes with its structure but none of the game's content, which it reads from discs
you own. Once its questions are answered, the editor offers to fill it:

1. Tell it where your disc images are.
2. Let it run. The first fill takes a while.

The editor checks that the discs are the ones the project expects, and refuses others. Later fills only redo what
has changed. To fill again, open the command palette (`Ctrl+Shift+P`) and choose **Fill generated layers…**.

## A first look around

- The **Assets** pane, at the bottom, shows everything in the project as folders and assets.
- Click an asset to open it in the middle of the window, where it is previewed: pictures are shown, sounds and
  movies play.
- The **Inspector**, on the right, shows the asset's fields.

[The window](../basics/window.md) explains the rest of the layout, and [Key ideas](../basics/key-ideas.md) the
words this manual uses.

## One editor at a time

Only one editor can have a project open. While it is open, the command line and AI agents work through the editor,
and their changes appear in it.
