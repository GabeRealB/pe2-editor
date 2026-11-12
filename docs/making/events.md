# Events

An event is something that happens: you build, you save a file, you open the project. Outputs are wired to events,
and when an event happens, everything wired to it is made.

## The events

| Event | When it happens |
|---|---|
| **Build** | You choose **Build** (`Ctrl+B`). |
| **Clean** | You choose **Clean**. |
| **Files changed** | Files of the project changed: you saved, or another program did. This is what builds the project as you work. |
| **Config changed** | You chose another configuration. |
| **Project opened** | The project finished opening. |

Extensions can add events of their own. A project can too: in the **Events** pane, type a name in **A new event**
and choose **Add**. The event is an asset of its own, in the `events` folder of the active layer.

## Wiring an output to events

An [Output step](stacks.md#outputs) says, in its settings:

| Setting | What it means |
|---|---|
| **Made on** | The events that make it. |
| **Stale on** | The events that make it out of date, and remove it from where builds put it. |
| **Goes to** | Where a build puts it, inside the project's `build` folder. `{config}` stands for the configuration's name. |
| **Ran**, **Changed**, **Failed** | Events it signals: after it is made, when it came out different, or when it failed. |

In the [graph editor](graphs.md), events are boxes too: **Event** gives an event as a value, **Signal** signals one
when its input says so, and gates such as **And**, **Or** and **Not** combine them. This way one output can wait
for others, or set off further work when it is done.

## Signalling an event

The **Events** pane lists every event - the project's, the extensions' and the built-in ones - with how much is
wired to each. **Signal** sets one off, in the configuration you work in.

From a terminal: `pe2-cli project signal <event>`.

## What happens when an event fires

Everything wired to the event runs once. Each output it makes is either found already made and unchanged, or made
again; then it is put where it goes. Outputs that go stale are removed from where they went.

Only one event runs at a time; many saves in a row are taken together. While one runs, the status bar says what it
is doing, with **Cancel**. What failed is listed in [Problems](../working/problems.md).

Before an event runs, extensions can check what it needs. If such a check fails, nothing wired to the event runs,
and the problem says why.
