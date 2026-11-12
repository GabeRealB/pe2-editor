# pe2.psx

The PlayStation target. It provides `package-target`: given the files `pe2.packages` gathered for a package, it
compiles the package's C units, assembles its assembly, and links its image by the package's link script, which places
it in the game's memory.

A config builds for it by naming it as its target (`target = "psx"` in `[config.<name>]` of `project.toml`).

## What it runs

| Program | What for | Where it comes from |
|---|---|---|
| `cpp/libexec/gcc/host/14.2.0/cc1` | preprocessing, as a modern GCC's `cc1 -E` | fetched: musl-cross's GCC 16.2.0 on Linux, macos-cross-toolchains' GCC 15.2.0 on macOS, PCSX-Redux's GCC 16.2.0 on Windows |
| `cc1` | compiling, as GCC 2.8.1 for the PlayStation | fetched: old-gcc 0.17 on Linux and macOS, SN Systems' build from the Psy-Q SDK 4.4 on Windows |
| `maspsx` | the compiler's assembly made what the original assembler made | fetched: maspsx at commit `4ffa8b5`, with three patches of this extension's (`patches/`), run by Python 3.13 from python-build-standalone, fetched too |
| `mips-linux-gnu-as`, `-ld`, `-nm`, `-objcopy` | assembling, linking, reading symbols, making images | fetched: musl-cross's binutils 2.47 on Linux, macos-cross-toolchains' 2.45 on macOS, PCSX-Redux's 2.47 on Windows |

Each compiler and the binutils for Linux and Windows were checked: built with them, every package's image is the game's own, byte for byte. Those for macOS have not been run yet; binutils 2.45 built for Linux matched too.

What it fetches is downloaded once a machine, from where its manifest says and checked against the hashes it gives:
trusting it consents to that, and the editor fetches it once it runs (`pe2-cli project extension setup pe2.psx` does so
ahead of time). The trust prompt lists each download, with its license and who holds its rights. A program is looked for in its
folder, then in what was fetched, then on the system. What the system has to provide is in [installing](install.md).

It runs outside the sandbox: you trust it before anything of it runs.
