# Installing

## What you get

The editor comes as one folder. It holds:

- `pe2-editor`, the editor;
- `pe2-cli`, the command line;
- `docs/`, this manual;
- `extensions/`, the extensions that ship beside the editor;
- `LICENSES.txt`, the licenses of what the editor is built with.

Put the folder anywhere and run `pe2-editor`. Nothing is installed elsewhere. Your settings, your window layout
and the places you trust are kept in your user profile, not in the folder.

## What your system needs

**Windows.** Nothing. If you install [Dokany](https://github.com/dokan-dev/dokany), the editor uses it for
[editing in other programs](../working/other-programs.md).

**Linux.** To edit in other programs, the system needs FUSE, which most distributions have. Without it, everything
else works. Under WSL, the editor tells you if one more setting is needed for Windows programs to reach the
project.

**macOS.** Not tested.

## Downloads

Some extensions need a program to do their work, such as a compiler. They download it the first time a project
uses them, and only after asking you. See [Extensions](../extending/extensions.md).
