# Licenses

The editor is free software under CC0-1.0: no rights reserved.

It is built with other people's libraries, each under its own license, all of them permissive. To see them:

- in the editor, **Settings ▸ Licenses**: every library, its version and license, and the license's text;
- on the command line, `pe2-cli licenses` (add `--texts` for every license text);
- beside the programs, in `LICENSES.txt`.

## What is not part of the editor

- **Programs your system provides**, such as the filesystem support the editor uses on Linux and Windows. They are
  under their own licenses.
- **Programs extensions download**, such as a compiler. Each extension lists them, with their licenses, before
  downloading anything, and in the **Extensions** pane. `pe2-cli licenses --project <folder>` lists them for a
  project.

Everything built into the editor is under a license that places no conditions on extensions, so an extension can
be under any license its author chooses.
