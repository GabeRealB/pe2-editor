# Serving a project as a folder

`crates/vfs`, the server in `crates/cli/src/server.rs`, and `project::shared`.

## The idea

Only the index knows how a project's layers resolve, so other programs cannot read a project from its layer
folders. Instead the resolved tree is served as a real filesystem at the project's `assets/` folder, named as the
Assets pane names it: every node a folder of its name holding its files and the entries inside it. Programs read
and write it like any folder; what they write becomes edits ([Writes through the folder](writes.md)).

## Who does what

Serving is split between two kinds of process (see [Holding a project](../processes/holding.md)):

- **The server** serves the folder and nothing else. It reads no index and loads no extension, so it can link
  libraries a process running third-party extension code must not.
- **Whoever holds the project** - the editor or a host - says what is served, in the [shared
  filesystem](shared.md), in the same commit as the refresh that changed it, and tells the server only that it
  did. It never waits on the server.

The server reads the served tree in place from the shared space, by path, as each request comes; it keeps no copy.
A commit is served the moment it is made. The server follows the commits since it last read (the changes the shared
filesystem keeps), and tells whoever caches the tree - the kernel under FUSE, watchers of a Dokany mount, the Cloud
Files worker - only what changed.

Each served file says where its content lives (a layer's file, a record's text, the store by hash), so the server
reads content without understanding records or layers.

## Backends

A backend is a module implementing `Backend` plus one line in `backend::all`; nothing else names one. A backend
says what it can do (`Can`: write through, be served at a folder) and whether it is available at a place. A job says
what it needs; the first backend, in order of preference, that can do all of it serves it. A backend is never given
a job it can only half do, and one the user names that cannot do the job is refused with why, never swapped.

| Backend | Where | Notes |
|---|---|---|
| FUSE | Linux, macOS | The kernel caches entries and content; changes are pushed to it. |
| Dokany | Windows, if installed | A real mounted filesystem, nothing kept on disk. Its declarations are transcribed from its header. |
| Cloud Files API | Windows | Placeholders hydrated on read, kept on disk; `cloud-cache` keeps them apart where the volume (ReFS) cannot host a sync root. |
| Projected File System | Windows | Read-only, so used for scopes only. |
| WebDAV | Anywhere | Served at an address, so never for scopes. |

`auto` picks by how the tree will be used (`pe2_ext::Use`: read often or once, whole or in ranges): a backend that
keeps what is read on disk for trees read often, one that keeps nothing for trees read once. Small file sets for a
program are simply copied, which beats every backend. What a keeping backend holds is bounded, and evicted by least
recent use.

Switching backends at a folder first retires what the previous one left there. Files found in the folder that are
not the project's (*strays*) stop the server: the user takes them in or sets them aside.

## Scopes

A program given files - a compiler given a package's sources, an external tool given one large file - gets a
*scope*: a read-only filesystem of its own (`vfs::scoped`), served by the backend best suited to it, cleared when
the program is done or by the next process to sweep stale scopes.

## Where to look

`vfs::backend` (choice and the `Backend` trait), one module per backend, `vfs::scoped`, `vfs::strays`,
`cli/src/server.rs` (`Server::follow`), `project::shared` (what the holder says).
