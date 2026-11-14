# pe2.binutils-mipsel

GNU binutils 2.47 for little-endian MIPS, as the editor's releases build them from their sources.

It offers these programs, which a project's operations and other extensions start as `pe2.binutils-mipsel:<program>`:

| Program | What it is |
|---|---|
| `as` | the assembler |
| `ld` | the linker |
| `nm` | lists an object's symbols |
| `objcopy` | copies sections out of an object |

It runs outside the sandbox, as what offers programs does, and fetches what it offers once the project trusts it.
