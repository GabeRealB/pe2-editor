# Holding a project

`crates/holder`, the host in `crates/cli/src/host.rs`, `project::lock`, `project::role`.

## One holder at a time

A project is **held** by one process at a time: it has the lock, the only index that changes, and the history.
Everything that changes the project goes through it. The holder is either:

- **the editor**, while its window has the project open; or
- **a host**, a background process (`pe2-cli project host`) that holds the project while no editor does.

A host is started on demand: by `pe2-cli` commands, by agents, or to keep the folder served. It leaves by itself a
couple of minutes after nothing uses it, unless told to keep serving.

Beside the holder runs the **server**, which serves the folder and nothing else (see [Serving](../filesystem/serving.md)).
Because the server holds nothing, the folder stays served while the project changes hands.

## The core

What holding a project means is written once, in `holder::core`, and used by both the host and the editor:

- `Queue` is a thread that owns something and does what it is given, in order. Everything that changes the project
  is work on that thread, so edits from the window, from commands, from agents and from the folder are made one after
  another, each over the project as the previous one left it.
- `Core` is a project held: the index, what is said in the shared space, the feed to the server. It makes edits
  (`edits`), takes what programs did through the folder (`take_due`), takes in files changed by other means
  (`take_in`), and after each says what changed in one commit.

The editor and the host differ only in what they do around the core: the host answers requests and waits for edits
to be on disk before answering, so the next command sees them; the editor answers at once and hands snapshots to its
window.

## Changing hands

The lock can be borrowed. A process that needs the project while a host holds it - the editor opening it, an
extraction, a script - takes the lock; the host lets go of everything and takes the project back when the borrower is
done. When the editor closes, a host standing by takes over the same server, so closing the window never interrupts
serving.

## Asking the holder

Processes talk over a local socket (a named pipe on Windows), one CBOR message per frame. Whoever asks first asks who
answers - an editor or a host, which build, trusting what - so a stale host from an older build is replaced rather
than talked to. `project::role` knows how each kind of process is found, started and connected to.

On Windows, background processes are started with `CREATE_NO_WINDOW`, never detached: a process with no console at all
makes Windows open a terminal for every console program it starts.

## Watching the layers

One watch (`holder::watch`) follows what changes under a project's layers by other means than the holder: git, an
editor in a code layer, a script. Changes are batched until the layers are quiet for a moment, and taken in like any
change. What the holder wrote itself it recognizes by the stamp the file will have, so it never takes in its own
writes.

## Where to look

`holder::core`, `holder::files` (the feed to the server), `holder::watch`, `holder::runner` (events, see
[Events](../building/events.md)), `cli/src/host.rs`, `project::lock`, `project::role`, `project::wire`.
