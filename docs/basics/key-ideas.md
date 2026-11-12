# Key ideas

**Project.** A folder holding everything you work on. Make one with **File ▸ New project…**, open one with
**File ▸ Open project…**.

**Asset.** One thing in the project: a picture, a sound, a model, a room's dialogue, a package of the game's code.
Assets sit in folders, like files. Every asset has a type, fields you edit in the Inspector, and files of its own.
It also has a permanent identity, its uid, so assets that refer to it still find it after you rename or move it.
The editor also calls an asset a *node*.

**Layers.** A project is a stack of layers, like an image editor's. What you see is all of them combined, top over
bottom.

- The game's own content is a layer at the bottom. It is read-only.
- Your work is in layers above it.
- The **active layer** is the one your edits go to.

When you change something that comes from a layer below, the editor makes an **override** in the active layer: your
version of that one asset. The layer below is never touched. See [Layers and overrides](../working/layers.md).

**Generated layer.** A layer filled from your discs rather than by hand. Filling it again remakes only what has
changed.

**Documents.** An asset you are editing is a document. Your changes are visible everywhere at once, but are only
written into the project when you save. See [Editing](../working/editing.md).

**Stack and outputs.** How an asset becomes what the game uses is its **stack**: a list of steps, from its stored
files through operations to its **outputs**, such as a picture, a sound as played, or a compiled file. A step can be
a whole **graph** of operations wired together. See [Stacks](../making/stacks.md) and
[The graph editor](../making/graphs.md).

**Events.** Outputs are made when something happens: you build, you save a file, you open the project, or anything
you name yourself. See [Events](../making/events.md).

**Configuration.** One way of building the project: which release of the game, and settings such as the language.
You choose the one you work in at the top right of the window. See [Configurations](../making/configurations.md).

**Extension.** Everything the editor knows about a kind of asset - how to show it, edit it and build it - comes
from an extension. Most are built in; you can add others and [write your own](../extending/writing/index.md).
