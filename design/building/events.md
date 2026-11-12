# Events and epochs

`project::events`, `project::build`, `holder::runner`.

## The idea

Nothing is built on a schedule of its own. Outputs are wired to **events**, and when an event fires, everything
wired to it is made in one **epoch**. Building, cleaning and building as you save are all just events.

The built-in events are `build`, `clean`, `files-changed`, `config-changed` and `project-opened`. Extensions and
projects can declare more.

## Wiring

An output layer says:

- `pull`: the events that make it;
- `listen`: the events that make it stale (and remove what it deployed);
- `deploy`: where under the project's `build/` folder a build copies it;
- `ran`, `changed`, `failed`: events it signals in turn.

Inside graphs, events are values too: `event` and `signal` operations, and gates (`and`, `or`, `not`...), so an
output can wait for others or set off more work when done. Extensions can add **hooks**, which run before what an
event pulls; a failing hook stops the event's work, which is how an extension checks its tools before a build.

## An epoch

When an event fires, every output wired to it is found kept or made (see [The work graph](graph.md)), and copied to
where it deploys; outputs that go stale are removed from where they went. The result says what was made, what
failed, what is stale, and whether it was cancelled.

- **One at a time.** Epochs run one after another; a burst of saves becomes one `files-changed`.
- **Off the holder's thread.** The runner works beside the thread holding the project, so the project can still be
  read and edited during a build. Whoever asks for the project during an epoch waits.
- **Cancellable** between steps.

Every change of files therefore pulls everything wired to `files-changed`, so what keeps that fast is that finding
an output kept is cheap.

## Where to look

`project::events` (declarations, wiring checks), `project::build` (an epoch), `holder::runner` (running epochs for a
holder), the `pe2.events` extension (project-defined events).
