# The shared filesystem

`crates/sfs`, and `project::storage` and `project::shared` on top of it.

## The idea

A project's processes - the holder, the server, a command - share their state through memory-mapped files in the
project's cache rather than by sending it to each other. One library, linked by all of them, provides three things:

- a **write-ahead log**, which makes every change durable;
- **pages and trees**, a copy-on-write store holding the served tree and the project's model;
- a **ring of pages read**, a cache of file content shared between processes.

Nothing in the pages or the ring is the only copy of anything: either can be thrown away and rebuilt from the
project's files and the log.

## The log

An append-only log of records with their bytes, in segment files, mapped by every process. Any process appends; a
record becomes visible only once whole, so no reader sees part of one, and where the log really ends is found by
checking records, never trusted from a header.

Two kinds of writer use it:

- **The server** logs what programs do through the folder, with the file bytes, so a file in transit is just ranges
  of the log and a sync is just flushing it. A new server replays what the previous one left.
- **The holder** logs each transaction of the history before touching any file: the whole set of changes, then
  that it is whole, then the log is flushed once, and only then are files written. A set that never became whole
  touched nothing; one that is whole is written again by whoever opens the project next.

Sets that change only records are **written behind**: logged, kept in the shared space as waiting, and written to
their files by a background thread. Readers in any process look at what waits before the disk. A file changed by
someone else meanwhile is not overwritten: the conflict is kept aside and reported.

Each side says how far it still needs the log, and what neither needs is dropped.

## Pages and trees

Chunk files of fixed-size pages, mapped by every process, holding B-trees:

- **One writer at a time**, by a lock the system releases if the holder dies. A writer never changes a page a reader
  can reach: it copies it, and publishes a commit by flipping a header.
- **Readers take no lock.** A reader pins the generation it reads at; a freed page is reused only once no pinned
  reader can reach it.
- **Damage is detected, never read.** A page says which generation wrote it; following a pointer to the wrong page
  marks the space damaged for every process.
- **Trusted only when clean.** A space is trusted on reopening only if the last process left it in order; otherwise
  it starts empty and is refilled, and what it held is kept aside for what can still be salvaged.

On the trees sits a small filesystem that knows nothing of projects (`sfs::fs`): names, nodes and a log of changes
per commit. The served tree and the [project's model](../project/index-and-model.md) live there.

## The ring

A lock-free cache of 4 KiB pages of file content, keyed by file and version, in memory every process maps. The
server reads through it; content assembled from records, chunks or the log is read once and served many times.

## Where to look

`sfs::log`, `sfs::space` and `sfs::tree` (with their test harness, which kills writers and readers at every step),
`sfs::fs`, `sfs::ring`, `project::storage` (transactions through the log), `project::shared` (what is said there).
