# Running programs

Some work needs a program: a compiler, a video encoder. An extension that runs programs declares
`unsandboxed = true` in its manifest. It then has the whole system - files, network, environment - and may start
programs; and nothing of it runs until the user trusts it.

## Trust

The user trusts an extension by where it is: a layer of the project, or the whole project. Trust covers everything
in the extension's folder that runs, so if its component or a program it ships changes, the user is asked again.
Its manifest, docs, licenses and tests do not count. Nothing runs from the user's own extensions folder or from
beside the editor, only from a project.

## What it sees

An unsandboxed extension reads and writes the disk with Rust's own `std::fs`, as a program does:

- **Files.** The whole disk, at `/` on Linux and macOS; on Windows each drive is a root of its own, `/C:`, `/D:`. A
  place the editor gives it (`HostPath`) is opened by `guest()`, written in this form. A path with `..` never reaches
  above the root it starts from, and links are followed as the system follows them.
- **The network.** Sockets to where the editor itself may reach. Where the editor runs without a network - its own
  tests - none is reached, and a download is refused.
- **Time, randomness and the environment.** The editor's: the time of day, random numbers, and the environment the
  editor was started with.

What it writes with `std::fs` is the user's disk, not the project: a project's files are changed through the
editor's calls, never by writing into its folders.

## Programs

`pe2_ext::process::spawn(tool, args)` starts a program and returns a `Process` to write to, read from and wait for.
Its arguments are `Arg`s: text, a place on disk (a `HostPath`), which the editor writes as the program reads it, or
parts joined into one (`-I` and a folder); `pe2_ext::args!["-o", &out, "-c"]` makes them. `process::temp()` gives a
place for work that belongs to no operation's run.
The program is looked for in the extension's folder for the user's platform, then in the extension's folder, then
among its downloads, then on the system.

`[[program]]` offers a program by name, started in a particular way - to the extension itself, and to others, which
start it as `<extension>:<name>` (`spawn("pe2.gcc-psx:cc1", ...)`, or a project's `run` box):

```toml
[[program]]
name = "as"
program = "@pe2.python:python3"
args = ["-B", "@maspsx/maspsx.py", "--gnu-as-path", "@pe2.binutils-mipsel:as"]
```

`program` is a place in the extension's tree, or another extension's program. In `args`, `@` marks a place in the tree,
or another extension's program, by its path; what the one starting it passes follows. Starting another extension's
program fetches what that extension requires first; it has to be in the project and trusted.

## Downloads

What the extension needs and does not ship, it declares in `[[setup]]`. The editor downloads it once per computer,
checks it, unpacks it, patches it, and shares it between projects. The user sees each download, with its license,
before trusting the extension.

```toml
[[setup]]
name = "A compiler"
license = "GPL-3.0-or-later"
copyright = "Its authors"
platform = "linux-x86_64"
url = "https://example.org/compiler-1.0.tar.gz"
mirrors = ["https://mirror.example.org/compiler-1.0.tar.gz"]
sha256 = "..."
take = { "compiler-1.0/bin/cc" = { to = "cc", sha256 = "..." } }
```

| Key | Meaning |
|---|---|
| `name`, `license`, `copyright` | What the user is shown. |
| `platform` | The system it is for; all, if left out. |
| `url`, `mirrors` | Where it is downloaded from, over HTTPS; the mirrors hold the same bytes. |
| `sha256` | The download's checksum. Other bytes are refused. |
| `take` | What is taken from it, and where it goes: an archive's members, each with the checksum of what it holds, or the file itself. |
| `patches` | Patches the extension ships, applied in order: `[{ file = "patches/fix.patch", sha256 = "..." }]`. |
| `lazy` | Downloaded only when the extension asks for it with `pe2_ext::process::set_up(name)`, and only if the user allows it. |

Downloads may be tar archives (gzip or xz), zip archives, or single files.

To fill in the checksums, write each `take` place bare (`"<path>" = "<to>"`) and run
`pe2-cli extension hash extension.toml --write`, which downloads everything for every platform and writes them. Run
`pe2-cli extension pins extension.toml` now and then to check that every download is still there.

## Limits

An extension outside the sandbox has an hour for each call instead of seconds.
