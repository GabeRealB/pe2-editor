# The game's extensions

The editor knows nothing of Parasite Eve II itself: everything about the game is in its extensions. This page is
about how those extensions are designed, not what they show the user (that is the manual's "Built-in views").

## Formats first

`crates/formats` reads and writes the game's native formats exactly: every stored byte is either understood or kept,
so that what is read is written back unchanged. `crates/codecs` converts native forms to editable ones (PNG, WAV,
OpenRaster, USD, UFO) and back. Extensions build on both; a round trip to the same bytes is the test of every format.

## Native data and editable data

Extraction writes the game's own bytes into the generated layer; the user's layers hold editable forms. Where a
native file is all data (a sound script's notes, a bundle's layout), the extension reads it into fields beneath what
the node sets, so it can be edited as fields without the game's data being copied into the user's layers.

## Mirrors of the game's code

Some extensions reproduce what the game's code does, so previews behave as the game does:

- **Sound**: a port of the game's sound script interpreter and voice upkeep over a model of the console's sound
  chip, with its pitch, gain and pan curves computed rather than copied.
- **Animation cues**: a cue is only bits on a key frame; what the game plays on it is the game's code's. An animation
  bank therefore *describes* it, for the editor, rather than the editor reading it from data.

These mirrors have to be kept in step with the game's code by whoever changes either.

## Rooms and text

A room's bundle declares its resources by index and joins them as the disc stores them, byte for byte. Dialogues
are data nodes of their structure, their text in string tables, their glyphs from font faces; a translated table is
laid over the original by the configuration's language. Flags are named in registries, each ID owned by one name.

## Building the game's code

The game's packages are built by graphs of the project's, not by an extension that knows the toolchain: `pe2.packages`
gives the pieces a build is wired from (a language's units, a first pass's files, `resolve`, the objects collected, what
links a package, what it publishes), `run` boxes run the programs tool extensions offer (each a manifest alone: GCC
2.8.1's `cc1`, the binutils, maspsx as the assembler of a flavor of them, aro's preprocessor, Python), and `pe2.psx`
makes the image the PlayStation loads of what was linked and runs nothing. A generator makes the build from a
description, per configuration, so the game's matching build and any other are descriptions of the same kind; the
project's nodes say which (see [The work graph](../building/graph.md)).

## Where to look

`crates/formats`, `crates/codecs`, `crates/model` (models), `crates/typeface` (fonts), each extension under
`extensions/` (`packages/src/build.rs` and `generate.rs`, `psx`), and the tools in `extensions/tools/`.
