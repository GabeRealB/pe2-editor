# pe2.binutils-mipsel-maspsx

GNU binutils 2.47 for little-endian MIPS whose assembler assembles as Sony's ASPSX did: `as` is maspsx, with this extension's patches (`patches/`), run by pe2.python, handing what it makes to pe2.binutils-mipsel's assembler. `ld`, `nm` and `objcopy` are pe2.binutils-mipsel's.

It offers these programs, which a project's operations and other extensions start as `pe2.binutils-mipsel-maspsx:<program>`:

| Program | What it is |
|---|---|
| `as` | maspsx, then GNU as: assembles what GCC 2.8.1 writes as ASPSX did |
| `ld` | pe2.binutils-mipsel's linker |
| `nm` | pe2.binutils-mipsel's |
| `objcopy` | pe2.binutils-mipsel's |

It runs outside the sandbox, as what offers programs does, and fetches what it offers once the project trusts it.
