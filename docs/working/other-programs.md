# Editing in other programs

While a project is open, its **`assets`** folder shows the whole project as ordinary files and folders, exactly as
the Assets pane does. Open it with any program: an image editor, a text editor, a file manager, a script.

## What your changes do

Anything a program does in the `assets` folder is a change to the project, just as if you had made it in the
editor:

- saving a file changes that asset, in the [active layer](layers.md);
- making, renaming, moving and deleting files and folders does the same to the project;
- every change is a step in the [history](history.md), and can be undone.

Temporary files that programs leave beside a file while saving are ignored.

A change is safe as soon as the program has finished saving, even if the editor closes the moment after.

## Without the editor

The folder stays available for a little while after you close the editor. To keep it available without the
editor, run:

```
pe2-cli project serve
```

## Changes made directly in the project

Some files are not edited through the `assets` folder: code you edit in its own folder, or files a version control
tool changes. The editor takes these changes in when you come back to its window, or as they happen; choose which
in **Settings ▸ Files**. **View ▸ Refresh** (`F5`) takes them in right away.

The folders under `.layers` are the project's storage. Do not edit them by hand; if you do, the editor lists what it
finds in [Problems](problems.md).

## Files left in the folder

If you put files in the `assets` folder while nothing was serving it, the editor finds them when you next open the
project and asks what to do: **Take in** makes them part of the project, **Set aside** moves them out of the way.
