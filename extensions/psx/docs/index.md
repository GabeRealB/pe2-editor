# pe2.psx

The PlayStation, as the build of a package ends: what is the platform's and no compiler's.

A package's build - an operation of the project's, its programs offered by the tool extensions - compiles the package,
links it by its link script, which places it in the game's memory, and cuts its image out of what it linked
(`objcopy -j .image`). **The image the game loads** (`pe2.psx@finish`) is its last step: it makes that image the
package's `.pe2pkg`, named as the release names its files (`aya/mist`: `aya_mist.pe2pkg`), or, for a package marked an
executable (`target.psx.executable = true`), the executable the PlayStation's BIOS loads: the PS-X EXE header - its
entry point, where it loads and how much, the stack, the region's license string of the release built - before the
image, padded to whole sectors.

A configuration builds for it by naming it as its target (`target = "psx"`), which also picks what packages say of
this target alone (`target.psx` in a package: its link script, its prebuilt objects, what it publishes).

It also tells a language server what a package's sources are read as (`flags`).

It runs no program, and runs in the sandbox.
