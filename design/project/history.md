# History and documents

`project::history`, `project::edit`, `project::storage`.

## Two kinds of edit

- **Document edits** change an asset's *working copy* only: its fields as typed in the Inspector or a view. The index
  shows working copies, so every preview and output follows unsaved changes. Nothing is written until the document is
  saved; a document whose working copy equals what is on disk stops being one.
- **Everything else** - saving, making, renaming, moving, deleting, overriding - is a **transaction**. The operations
  in `project::edit` gather their changes into it, reading the project as their own pending changes leave it. Nothing
  touches the disk until the transaction commits; then the whole set is logged, flushed once, and applied (see [The
  shared filesystem](../filesystem/shared.md)). A set that fails midway is rolled back; one interrupted by a crash
  is completed from the log when the project is next opened.

Undo and redo restore files through the same path, so no edit operation needs undo code of its own.

## Steps

The history is a list of **steps**, each with a label, an author, a time and the selection to restore. A step holds
file changes (a write from one content to another by hash, a rename, a folder made or removed - each with its
inverse) and document edits. Contents are stored once by hash in the history's folder.

- **Merging.** Edits of the same thing within a moment of each other join one step, so typing or dragging is one
  undo. A view says when a gesture ends.
- **Undo in a document or in the project.** Undo can be scoped to one document: it takes back that document's part of
  its last step. Unscoped, it takes the last step anywhere.
- **Checked.** Undo and redo check that files and working copies are as the step left them; if something else changed
  them, the user is asked before forcing.
- **Redo is dropped only where it must be:** a new step drops only the undone steps that touch the same things.
- **Saves and checkpoints** are steps too. Undo passes over saves; a checkpoint changes nothing and only marks a
  point.

## Points of the history

A past point can be shown live: the worker's index then shows the working copies as they were, rebuilt from the
steps, while everything is read-only. Restoring that point, or taking back one step's edits, makes a new step, so
nothing is lost.

## Sessions

The editor journals its session as it goes (steps and documents, appended, rewritten whole now and then). A clean
close wipes it; after a crash, the next opening offers it back and resumes it before the new session. A host keeps
its own history, so `pe2-cli undo` works between commands.

## Where to look

`project::history` (`History`, `View`, `Step`, `Transaction`, sessions), `project::edit` (the edit operations),
`project::storage`.
