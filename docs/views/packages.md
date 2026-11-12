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

Building packages needs the `pe2.psx` extension, which runs the game's compiler on your computer. It ships beside
the editor; see its own guide in the **Extensions** pane, and [trust](../extending/extensions.md#trust).
