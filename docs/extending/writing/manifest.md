# The manifest

Every extension has an `extension.toml` beside its `Cargo.toml`. It is checked as the extension builds: an unknown
key, a missing file or a file outside the extension fails the build. The component carries the manifest inside it,
and the copy in the extension's folder must match.

## About the extension

| Key | Meaning |
|---|---|
| `id` | Its id, such as `me.notes`. Everything it registers is named `<id>@<name>`. |
| `version` | Its version. |
| `description` | One line on what it does. |
| `license` | Its license, as an SPDX expression. |
| `repository` | Where its source is. |
| `docs` | Its guide's first page, such as `docs/index.md`. Shown in the Extensions pane. |
| `install` | What to set up on the computer first, such as `docs/install.md`. Shown when a program it needs cannot be started. |
| `icon` | Its icon. |

## What it needs and offers

| Key | Meaning |
|---|---|
| `requires` | Extensions or services it cannot work without, with the versions it accepts: `["pe2.images@^0.1"]`. |
| `optional` | Ones it uses where they are present. |
| `weak` | Ones it uses where present, and is turned off with. |
| `provides` | Services it offers others, by name and version: `["video-container@1.0.0"]`. |
| `platforms` | The systems it runs on, as `linux-x86_64`, `windows-x86_64`, `macos-aarch64`...; all, if left out. |

## Licenses of what it ships

```toml
[[bundled]]
name = "A font it draws with"
license = "OFL-1.1"
text = "licenses/font.txt"
```

Each `[[bundled]]` names something it ships that is not its own code, with its license's text.

## Running programs

An extension that runs programs sets `unsandboxed = true`, and may declare what it downloads (`[[setup]]`) and the
programs it starts (`[[launch]]`). A sandboxed extension that declares either is refused. See
[Running programs](unsandboxed.md).
