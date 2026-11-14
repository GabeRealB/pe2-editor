# pe2.gcc-psx

GCC 2.8.1's C compiler proper, `cc1`, patched for the PlayStation (old-gcc's patches, and the editor's for 64-bit hosts), as the editor's releases build it from its sources.

It offers these programs, which a project's operations and other extensions start as `pe2.gcc-psx:<program>`:

| Program | What it is |
|---|---|
| `cc1` | the compiler proper: preprocessed C in, MIPS assembly out |

It runs outside the sandbox, as what offers programs does, and fetches what it offers once the project trusts it.
