# pe2.aro-cpp

A C preprocessor taking the arguments GCC's `cc1 -E` takes, made of aro, the C front end zig 0.17.1 carries, as the editor's releases build it.

It offers these programs, which a project's operations and other extensions start as `pe2.aro-cpp:<program>`:

| Program | What it is |
|---|---|
| `cpp` | the preprocessor |

It runs outside the sandbox, as what offers programs does, and fetches what it offers once the project trusts it.
