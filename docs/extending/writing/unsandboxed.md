# Running programs

Some work needs a program: a compiler, a video encoder. An extension that runs programs declares
`unsandboxed = true` in its manifest. It then has the whole system - files, network, environment - and may start
programs; and nothing of it runs until the user trusts it.

## Trust

The user trusts an extension by where it is: a layer of the project, or the whole project. Trust covers everything
in the extension's folder that runs, so if its component or a program it ships changes, the user is asked again.
Its manifest, docs, licenses and tests do not count. Nothing runs from the user's own extensions folder or from
beside the editor, only from a project.

## Programs

`pe2_ext::process::spawn(tool, args)` starts a program and returns a `Process` to write to, read from and wait for.
The program is looked for in the extension's folder for the user's platform, then in the extension's folder, then
among its downloads, then on the system.

`[[launch]]` names a program the extension starts in a particular way:

```toml
[[launch]]
name = "maspsx"
platform = "linux-x86_64"
program = "python/bin/python3.13"
args = ["-B", "@maspsx/maspsx.py"]
```

`@` marks a place inside the extension's tree.

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
