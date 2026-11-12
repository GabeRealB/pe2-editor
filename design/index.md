# How the editor is built

These pages are for developers working on the editor: how it is put together, how its parts work, and why. They are
about the editor as it is now; what it was before is in the commit history. Each page ends with where to look in the
code, which is the reference for the details.

What a user or an extension author needs is in the manual, `docs/`.

## Contents

- [Architecture](architecture.md): programs, processes, crates, the life of an edit and of a read, principles
- [Building, testing and measuring](engineering.md)

**The project**

- [Layers and storage](project/layers.md): records, the store, overrides and merges, sealed extensions
- [The index and the model](project/index-and-model.md): resolving layers, refreshing, opening fast
- [History and documents](project/history.md): working copies, transactions, steps, sessions
- [Plans and generated layers](project/plans.md): filling a project from discs

**Processes**

- [Holding a project](processes/holding.md): the holder, the host, the lock, changing hands
- [The editor](processes/editor.md): the window, the worker, frames, layouts, extension views
- [Agents](processes/agents.md): requests, the holder's link, driving the window

**The project as a folder**

- [Serving a project as a folder](filesystem/serving.md): the server, backends, scopes
- [Writes through the folder](filesystem/writes.md): from a program's save to an edit
- [The shared filesystem](filesystem/shared.md): the log, pages and trees, the ring

**Building**

- [The work graph](building/graph.md): stacks, plans, evaluation, caching by what was read
- [Events and epochs](building/events.md): what makes outputs, and when

**Extensions**

- [The extension runtime](extensions/runtime.md): the contract, loading, sandboxing, trust
- [The game's extensions](extensions/game.md): formats, mirrors of the game's code

**Other**

- [Licenses](licenses.md): the listing and the dependency policy

## Keeping these pages true

A page says what is, in the present tense. When you change a part, change its page with it. Keep pages at the level
of how and why; names in them are there to help find the code, not to list it.
