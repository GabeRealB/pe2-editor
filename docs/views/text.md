# Dialogues, text and flags

## Dialogues

A room's **Dialogue** is its script: lines of text and the scene steps between them. Its **Script** view lists what
happens in order. **Edit its text** takes you to the line's text in its string table.

## String tables

A **String table** holds text by key. Its **Strings** view lists every entry; filter it, choose one, **Edit** it and
**Apply**.

A string table can be translated: see [Configurations](../making/configurations.md) for how the language chooses
which text is used.

## Casts

A **Cast** names the speakers of a dialogue (**Speakers**).

## Flag registries

The game remembers the story's progress in flags. A **Flag registry** names them, so that dialogues, events and code
can refer to a flag by name instead of by number. Its **Flags** view lists each flag with its kind and values; **Add
flag** by name and number, **Rename** or **Remove** one. A number can belong to only one flag across all registries.
