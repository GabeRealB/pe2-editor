# PE2 Editor manual

The PE2 Editor is an editor for Parasite Eve II projects: you browse the game's content, change it, and build the
game's code from it. It is incomplete and experimental.

The editor ships with no projects and no game data. Make a new project, or open one made for the game, which the
editor fills from discs you own.

## Contents

**1. Getting started**

- [Installing](getting-started/install.md)
- [Your first project](getting-started/first-project.md)

**2. Basics**

- [Key ideas](basics/key-ideas.md): projects, assets, layers, stacks, outputs, events, configurations, extensions
- [The window](basics/window.md): menus, panes, tabs, modes, the status bar and the command palette

**3. Working with a project**

- [Finding and viewing assets](working/finding.md)
- [Editing](working/editing.md)
- [Layers and overrides](working/layers.md)
- [Undo and history](working/history.md)
- [Editing in other programs](working/other-programs.md)
- [Importing and exporting](working/import-export.md)
- [Building](working/building.md)
- [Problems](working/problems.md)

**4. How assets are made**

- [Stacks](making/stacks.md): what an asset is made of, and its outputs
- [The graph editor](making/graphs.md): operations wired together
- [Events](making/events.md): what makes outputs, and when
- [Configurations](making/configurations.md): building the project more than one way
- [Your own operations and templates](making/own-operations.md)

**5. Built-in views**

- [Pictures](views/pictures.md)
- [Sound](views/sound.md)
- [Models](views/models.md)
- [Movies](views/movies.md)
- [Fonts](views/fonts.md)
- [Dialogues, text and flags](views/text.md)
- [Packages of code](views/packages.md)
- [The game's files](views/game-files.md)

**6. Extending the editor**

- [Extensions](extending/extensions.md): adding, trusting and managing them
- [AI agents](extending/agents.md)
- [Writing extensions](extending/writing/index.md)
  - [The manifest](extending/writing/manifest.md)
  - [What an extension can add](extending/writing/registering.md)
  - [Calling the editor](extending/writing/host.md)
  - [Running programs](extending/writing/unsandboxed.md)
  - [Packaging and sharing](extending/writing/packaging.md)
  - [Testing](extending/writing/testing.md)

**7. Reference**

- [Settings](reference/settings.md)
- [Keyboard and mouse](reference/shortcuts.md)
- [Command line](reference/command-line.md)
- [Licenses](reference/licenses.md)
