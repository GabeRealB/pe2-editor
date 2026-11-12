# Packaging and sharing

## The extension's folder

An extension is shared as a folder named after its id, `<id>.extension`:

| File | What it is |
|---|---|
| `extension.toml` | Its manifest. |
| `extension.wasm` | Its component. |
| `docs/` | Its guides, named by `docs` and `install` in the manifest. |
| `LICENSE`, `licenses/` | Its license, and the licenses of what it ships. |
| `CHANGELOG.md` | What changed between versions. |
| `<platform>/` | Programs it ships, for each system, if it runs outside the sandbox. |
| `tests/` | Its tests. See [Testing](testing.md). |

## The archive

The same folder can be zipped as `<id>-<version>.extension.zip`, keeping each file's permissions. Users add an
extension from either.

## Where users put it

- To add it to a project: **Add it to the project** in the Extensions pane, or
  `pe2-cli project extension add <folder or archive>`. The project keeps its own copy, so everyone who opens the
  project uses the same.
- To have it offered to every project: put it in your extensions folder, in your user data (`pe2-editor/extensions`).
  Extensions beside the editor, in its `extensions` folder, are offered the same way.

Nothing runs from those folders: an extension runs only from a project that holds it.

## Docs and licenses

Give users a guide: `docs/index.md`, named by `docs` in the manifest, is shown in the Extensions pane. Where the
extension needs something set up on the user's computer, say so in `docs/install.md`, named by `install`; the
editor points to it when a program cannot be started. List what you ship that is not your own under `[[bundled]]`,
with the text of its license in `licenses/`.
