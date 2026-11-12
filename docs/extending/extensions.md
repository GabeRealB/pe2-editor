# Extensions

Everything the editor knows about a kind of asset - how to show it, edit it, import, export and build it - comes
from an extension. Most are built into the editor. Others ship beside it, such as `pe2.psx`, which compiles the
game's code, and `pe2.ffmpeg`, which writes video files; and you can [write your own](writing/index.md).

## The extensions a project uses

A project keeps its own copy of each extension it uses, beyond the built-in ones. Everyone who opens the project
therefore uses the same extensions, whatever editor they have.

The **Extensions** pane has three views:

- **List**: every extension, with its state: loaded, left off by the project, not trusted, available to add, or not
  loaded and why. **Filter extensions** finds one.
- **Graph**: which extensions need which.
- **Fetched**: what extensions have downloaded onto this computer (see [Downloads](#downloads)).

Choose an extension to see what it adds to the editor, what it needs and what needs it, its settings, its guides
and installation notes, and the licenses of what it ships and downloads.

| To | Do |
|---|---|
| Add an extension to the project | **Add it to the project** |
| Update it to the version beside your editor, or in your extensions folder | **Update it from there** |
| Turn it off for this project | Clear **Enabled for this project** |
| Remove it from the project | **Remove it from the project** |

Adding, updating and removing an extension are changes to the project, and can be undone.

From a terminal: `pe2-cli project extension add <id, folder or archive>`, `update`, `remove`, `list`.

## Settings

An extension can have settings, listed with it in the Extensions pane. Each says whether it is the project's, shared
with everyone on it, or yours alone; **Back to** *default* resets one.

## Trust

Most extensions run in a sandbox: they see only what the editor gives them, and cannot touch your computer.

A few need to run programs on your computer. These run only once you trust them. The project asks the first time
you open it, and the Extensions pane lets you **Trust** or **Stop trusting** one at any time. Trust is yours, on this
computer, and is never stored in the project. If an extension you trusted changes what it runs, you are asked again.

From a terminal: `pe2-cli project trust`.

## Downloads

An extension that runs programs may need to download them first. Before you trust it, it lists every download:
what it is, where it comes from, and its license. Optional downloads have a switch, so you can leave them out.

Downloads are checked and kept once on your computer, shared by all your projects:

- **Fetch now** downloads ahead of time; **Try again** retries one that failed.
- **Fetch again** repairs a download that was changed or damaged.
- What no project has used for 30 days is removed by itself. **Clean up now** removes it sooner, and **Remove what
  this project does not use** keeps only this project's.
