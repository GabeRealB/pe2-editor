# The graph editor

A **graph** step in a [stack](stacks.md) is several operations wired together. The graph editor shows it as boxes
and wires, and lets you build it by hand.

## Opening it

Select an asset whose stack has a graph step and choose **Graph** at the right of the tab strip (`Ctrl+3`), or
**Open** on the graph's row in the Inspector's Stack tab. To give an asset a graph, add a **Graph** step to its
stack first.

## What it shows

- Each operation is a **box**. Its inputs are on the left, each with its name and the kind of value it takes; its
  outputs are at the bottom right. Colors match kinds of value.
- **From above** is what the step above passes in. **Passed on down** is what the graph gives the step below.
- Most boxes have an **active** input. When it is false, the box makes nothing, and neither does anything that
  takes from it.
- A box with a problem is marked, and the problem is written below the canvas.

## Working on a graph

| To | Do |
|---|---|
| Add a box | Right-click the canvas, type to filter, and choose an operation. It is placed where you clicked. |
| Wire two boxes | Drag from an output onto an input. Inputs that fit light up as you drag. |
| Change or remove a wire | Drag it off the input onto another, or onto empty space to unplug it. |
| Choose what the graph passes on | Wire a box to **Passed on down**, or select the box and choose **Pass on down**. |
| Move a box | Drag it. |
| Change a box's settings | Select it; its settings appear in the Inspector's Stack tab. |
| Remove a box | Select it and press `Delete`, or **Remove** in the Inspector. |
| See the whole graph | **Fit** (`Home`), or double-click the canvas |
| Tidy it up | **Arrange**: puts the boxes in columns, by what feeds what |
| Pan and zoom | Drag empty space; the mouse wheel |

A wire between things that do not fit is refused, and the editor says why: what the input takes, and what the
output makes. Every change can be undone, and is kept only once you save.

## Operations that choose

Some operations decide what the graph makes, rather than making something themselves:

| Operation | What it does |
|---|---|
| **Merge** | Passes on the first of its inputs that is made. Joins branches back together. |
| **The config built** | The [configuration](configurations.md) being built: its release, target and settings. |
| **Equals** | Whether a value is a given one. |
| **All**, **Any**, **Invert** | Combine yes-or-no values. |
| **Select** | Picks one of several assets' outputs by a key, and reads only that one. |
| **Overlay** | Lays one table over another, key by key. |
| **For each** | Runs [an operation of your own](own-operations.md) once for each item of a list. |
| **Constant** | A fixed value. |

Wire a yes-or-no value into a box's **active** input to make it only in some cases. For example, `The config
built` → `Equals` → `active` makes a box only for one release.

Some of these take settings the Inspector does not list yet, such as which field **Equals** looks at. Write those
in the asset's [declaration](../working/editing.md#the-declaration).

The menu also offers the boxes of [events](events.md) - **Event**, **Signal**, **Hook** and gates such as **And**,
**Or** and **Not** - and, inside an operation of your own, **Input** and **Setting**.
