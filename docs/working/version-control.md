# Version control

A project is made to be kept in git and shared: what it holds of yours is small text files, which git compares and
merges well. What is yours alone - the files filled from your discs, what the editor works out, your history - stays
in the project's `.local` folder, which git leaves out.

## Setting up

A project made in a git repository is set up for it as it is made. For a project you put in a repository later,
run once, in a terminal:

```
pe2-cli project git-setup --project <folder>
```

It writes `.gitattributes` beside `project.toml`, and registers with your git how the project's files are merged.
Git keeps that registration in each clone's own configuration, so whoever clones the project runs it once too. While
a project in a repository is not set up, it says so in [Problems](problems.md).

## What git shares

- `project.toml`: the project's layers, settings and configurations.
- `layers/`: what each layer does, one small file for each entry it makes, changes or removes.
- `store/`: what those files keep that is no text - images, sounds, models - each kept once.

An asset you change from a layer below keeps only what you changed of it: what git shares of an edit follows the
edit, not the size of the asset.

## Merging

When you merge a branch, the project's files are merged by what they say: two people changing different settings
of one asset, or different lines of one script, are merged without a conflict.

When both changed the same thing in different ways, the merge keeps yours, keeps theirs beside it, and the project
says so in [Problems](problems.md). There, choose **Keep this one** or **Take the other**; from a terminal,
`pe2-cli project settle <layer> <uid>` keeps yours and `--theirs` takes theirs. Then commit as for any merge.
