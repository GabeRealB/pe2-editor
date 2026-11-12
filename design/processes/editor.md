# The editor

`crates/editor`: `app.rs` (`Editor`), `workspace.rs` (`Workspace`), `takein.rs` (the worker), `panes.rs` and
`windows.rs` (layout), `commands.rs`, `ext_ui.rs` (extension views).

## Two threads, two jobs

The editor's rule is that **the window displays and dispatches**. Everything that changes or reads the project's
files happens on the **worker**, the thread holding the project (a `holder::core` [queue](holding.md)). The window
never waits for it:

- the window **asks** the worker for work (`Workspace::ask`) with a closure to run when the answer comes, and keeps
  drawing;
- the worker does the work, takes in what it changed, and **hands over a snapshot**: the index, the asset tree, the
  layers, and a view of the history;
- on the next frame the window swaps the snapshot in, drops what it cached of changed assets, and runs the closures
  of what was answered (show the new node, say the step's label...).

The snapshot's maps are copies made only for what changed since the last one, after the edit has been answered, so
an edit never waits for a copy and the window never sees a half-made change.

Where the window must decide something first - nothing is editable while a past point is shown, which layer an edit
goes to - it decides from what it last saw, and the worker checks again.

## A frame

Each frame, in order:

1. swap in a project that finished opening;
2. take in changes from outside, answer agents that need a frame, swap in snapshots;
3. run the closures of answered work, and edits deferred while the project was being read again;
4. let the agents' driver inject input;
5. handle shortcuts, then draw the menus, the status bar and every layout.

**Panes never act while they draw.** They return what they want (navigate, pick, an edit, a history request), and
the editor carries it out after everything is drawn. So drawing has no side effects, and a layout can be drawn in
several windows.

## Layout

Panes live in tile trees (`egui_tiles`): one for the main window and one per extra window (each a native window).
A pane is a kind and an instance; a document pane shows an entry in one of several **editors** - Preview, Files,
Graph, or one an extension registers. **Modes** are saved layouts; switching carries the open documents over.

Layouts are saved through eframe's storage; preferences in the user's `settings.toml`; the user's view of a
project's layers (eyes, locks, active layer) in the project's cache, so toggling a layer never changes a tracked
file.

## Commands

Every action with a name is a `Command`: menus, shortcuts, mouse buttons, the command palette and agents all go
through it, so each action is defined once. Bindings have defaults and can be overridden in the settings.

## Extension views

An extension's view is drawn with the editor's own widgets: the SDK's `Ui` is implemented by the editor
(`ext_ui::Host`). What a view changes in a frame is collected and becomes an ordinary edit, so it is undoable like
any other. GPU views share the window's wgpu device: an extension publishes a texture, the editor shows it with no
copy. Views that need outputs ask without blocking and are redrawn when they arrive.

## Off the window's thread

Besides the worker, the editor has small pools for pictures and thumbnails (thumbnails are kept in the project's
store, so a later session reuses them), a thread per player stream (picture and sound read ahead, never skipping),
and threads for exports and plan states. None of them blocks a frame.

## Opening and leaving

Opening runs off the window's thread: take the lock, start listening for agents (so they can reach the window while
the project opens), read the index over the model, then start the worker. Leaving lets go in a fixed order - worker
and builds, then the lock, the server and the host's lease - and frees the index's memory last, after a host
standing by has taken over.

## Where to look

`app.rs` (`Editor::ui`, `apply`), `workspace.rs` (`Workspace`, `ask`, `edit`, `prepare`, `leave`), `takein.rs`
(`TakeIn`, `Worker`, `Snapshot`, `Outside`), `panes.rs`, `windows.rs`, `commands.rs`, `ext_ui.rs`, `gpu_view.rs`,
`player.rs`, `pictures.rs`, `thumbs.rs`.
