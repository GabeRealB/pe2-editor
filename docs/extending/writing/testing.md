# Testing

## Unit tests

An extension is an ordinary Rust crate: its own tests run on your computer with `cargo test`.

## Tests that drive the editor

To test what a user sees, write a test extension. It is an extension with the id `<id>.tests` that requires yours
and the editor's test support:

```toml
id = "me.notes.tests"
version = "0.1.0"
requires = ["me.notes@^0.1", "pe2.test@^0.1"]
```

It registers tests with `Registry::test(name, description, run)`. Each test drives a real editor on a project of
its own, as a user would, through `pe2_ext::test::Editor`:

| To | Use |
|---|---|
| Act | `drop_file`, `click`, `click_label`, `menu`, `typing`, `press`, `undo`, `show`, `node`, `declare`, `override_entry` |
| Check | `expect`, `expect_not`, `expect_no_problems`, `problems`, `assets`, `selected`, `pictured`, `made` |
| Use test files | `data(name)`: a file from the test's `tests/data/` |

A test passes unless it returns an error.

A test extension's folder holds `tests/tests.wasm`, its data in `tests/data/`, and a `tests/test.toml` saying what
project it runs in: its layers and configurations, which extensions to hold, which to leave off and which to trust.

## Running them

Tests run in `pe2-test-runner`, a build of the editor for testing, which a release does not include:

```
pe2-test-runner --suites <folder> [--filter <text>] [--list]
```

The editor's **Tests** pane runs them too, where `pe2-test-runner` is beside the editor: **Run all**, **Run failed**,
or one test, with what failed, a picture of the window, and what it said.
