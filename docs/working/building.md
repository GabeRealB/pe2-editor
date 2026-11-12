# Building

Building turns the project's assets into what the game uses, such as the game's code compiled into the files the
game loads. What is built, and when, is up to each asset's [stack](../making/stacks.md) and the
[events](../making/events.md) it is wired to.

## Choosing a configuration

A configuration is one way of building the project: which release of the game, and settings such as the language.
The configuration you work in is shown at the top right of the window; choose another there. Previews and builds
follow it. See [Configurations](../making/configurations.md).

## Building

| To | Do |
|---|---|
| Build the project | **Build** in the configuration menu (`Ctrl+B`) |
| Remove what a build made, so the next build starts afresh | **Clean** in the configuration menu |
| Stop a build | **Cancel** in the status bar, or **Cancel build** in the command palette |

What is built is what is saved: if you have unsaved documents, the editor first asks whether to save them.

The project is also built as you work: saving a file rebuilds what depends on it, and nothing else.

## Following a build

While a build runs, the status bar says so, with **Cancel**. When it ends, it says how many things were made, or
what failed. Failures, and outputs that are out of date because what they read changed, are listed in
[Problems](problems.md) under **Build** · *configuration*, each linking to its asset.

## Where the results go

A build writes its results into the project's `build` folder, in a folder per configuration. What goes there is up
to the assets: each output says where it goes.

Another program may expect a built file elsewhere, such as a code editor looking for its compile database at the
top of your source folder. Give it a link, kept on your computer only:

```
pe2-cli project link add <where the program looks> <the built file>
```
