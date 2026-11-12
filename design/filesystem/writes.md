# Writes through the folder

`vfs::writes`, `vfs::due`, `holder::core::Core::take_due`.

## The idea

What a program does in the served folder is an edit of the project, made by whoever holds it, as a step of its
history in the active layer. The program must never wait for that: a save returns at the speed of a local disk,
however busy the holder is.

So the work is split in two:

1. **The server does it at once, in what is served.** It writes the shared filesystem itself: a new file or folder
   is a node marked as not yet the project's, a removal hides the node, a rename moves it. File content in transit
   lives in the [log](shared.md), not in a file of its own. The folder therefore shows the change immediately.
2. **The holder takes it later.** The server puts each change in a queue in the shared space (*due*), in the order
   it happened, and asks the holder to look. The holder takes everything due at once, makes each an edit, refreshes
   its index once for all of them, and writes what became of each in the same commit as the new served tree. The
   server then lets go of what was taken, and undoes what was refused.

## When a change becomes due

- A file written: when closed (without waiting) or synced (waiting). A new file waits until nothing has it open and
  its burst of writes has ended, so editors that save in several steps make one edit.
- A removal: a moment later, unless something is made at the same path, which is how save-by-rename looks.
- A folder made, a rename: at once.
- Temporary files editors leave beside a file are never taken.

## Robustness

- **Ordered.** Changes are due in the order they were made, so each path refers to the project as the changes
  before it left it.
- **Durable.** Every operation and its bytes are in the log before the program is answered; a sync is the log on
  disk. A server that dies replays its log when the next one starts.
- **Idempotent.** A change may be taken twice (a holder died after making it but before saying so). One already in
  effect - a folder that exists, a rename already done - is answered as made rather than refused.
- **Refused for good, or for now.** Where nothing can take edits (the holder is a tool, or busy opening), changes
  stay as written and due, and are asked for again. A change refused for good is reported to the program at its
  next close or sync, as a filesystem reports a delayed write error. What is still not taken when serving ends is
  set aside beside the folder, never lost.

## The model holds

The folder obeys the project's model: a `node.toml` written into a folder makes it a node, a new file in a node is
one of its files, a node's declaration is removed only with its folder.

## Where to look

`vfs::writes` (the server side), `vfs::due` (the queue in the shared space), `vfs::write` (what is applied, what is
refused), `holder::core` (`take_due`).
