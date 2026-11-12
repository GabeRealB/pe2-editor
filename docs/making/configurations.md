# Configurations

A configuration is one way of building the project: a release of the game, the target that builds it, and settings
of your own, called **properties**. A project for several releases, or several languages, has a configuration for
each.

## Choosing one

The configuration you work in is shown at the top right of the window; choose another there. Everything follows
it: previews show what that configuration makes, and **Build** builds it. Your choice is kept for you, on this
computer.

## Properties

A property is a setting every configuration has, such as the language. Each has a kind and a default:

| Kind | Edited as |
|---|---|
| A list of values | a list to choose from |
| Yes or no | a checkbox |
| A whole number | a number field |
| Text | a line of text |
| Several texts | one per line |
| A table | text |

**Settings ▸ Configurations** shows each configuration - its release, its target, and how many assets it leaves
out - and lets you set its properties. **Default** puts one back to its default. Every change can be undone.

A value that does not fit its property, such as a language the property does not list, is reported in
[Problems](../working/problems.md), and the default is used meanwhile.

To add a configuration or declare a property, use the command line:

```
pe2-cli project config add <name> --release <release> --target <target>
pe2-cli project config declare <property> --values en,fr,de --default en
pe2-cli project config set <configuration> <property> <value>
```

## How properties change what is made

A [graph](graphs.md) reads the configuration through **The config built**, and uses its properties to choose:
**Equals** for one value, **Select** to pick among several assets by a property. For example, a table of
translations, one per language, is chosen by the `language` property and laid over the original text with
**Overlay**.

## Leaving an asset out

An asset can be left out of a configuration: clear **In** *configuration* in its Inspector. Nothing builds it there,
and nothing finds it among its kind.
