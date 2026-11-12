# Undo and history

Every change is a step you can undo: edits, saves, new assets, renames, deletes, and changes made by the command
line, by other programs and by AI agents.

## Undo and redo

| To | Do |
|---|---|
| Undo in the current document | **Edit ▸ Undo** (`Ctrl+Z`) |
| Redo in the current document | **Edit ▸ Redo** (`Ctrl+Shift+Z` or `Ctrl+Y`) |
| Undo the last change anywhere in the project | **Edit ▸ Undo in the project** (`Ctrl+Alt+Z`) |
| Redo it | **Edit ▸ Redo in the project** (`Ctrl+Alt+Shift+Z`) |

The Edit menu, and the undo and redo buttons beside the menus, say which change each would take back.

## The History pane

The **History** pane lists every step of the session, oldest first, with when it was made and by whom. Undone
steps are grey; click one to redo up to it.

- Show **All** steps, or only those of **This document**.
- Where several people or agents made changes, show only one author's.
- **Checkpoint** names the current moment, so you can find it again.

Choose a step to see what it changed, field by field.

Right-click a step for more:

| Choice | What it does |
|---|---|
| **Show as after this** | Shows the whole project as it was just after the step. |
| **Undo back to here** | Undoes every step after it. |
| **Take this change back** | Undoes that one step alone, keeping everything after it. Offered where nothing since changed the same things. |

## Looking at an earlier moment

While an earlier moment is shown, every preview, player and view shows the project as it was then, read-only.
From there:

- **Show then / Show now** (or `F` over the History pane) flips between that moment and now, to compare;
- **Restore here** brings your documents back to that moment. This is a new step, so nothing after it is lost;
- **Back to now** returns.
