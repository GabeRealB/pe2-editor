# Packages of code

The game's code is built from **Packages**: source files, headers and objects, and what each package's slots are
filled with. Building them is what [Build](../working/building.md) does in a project for the game.

## Slots

A package has slots: places where it takes something from the project, such as a model, a picture or another
package. The **Slots** view, at the top of the Contents pane and as an Inspector tab, lists them; fill one by
dropping an asset on it. It also shows the package's options, and what it **Depends on**.

## Tokens

Pictures, models and materials used by packages have a **Tokens** tab in the Inspector: the values the code is given
for them.

## Libraries

**Library** under **File ▸ New node** makes a new package of shared code.

## The compiler

A package is built by an operation the project names (see [Builds](../making/builds.md)), whose programs - the game's
compiler, the binutils - are tool extensions the project holds, and whose last step is `pe2.psx`'s. They ship beside
the editor; see their guides in the **Extensions** pane, and [trust](../extending/extensions.md#trust): a build runs
programs only for a project you trust.
