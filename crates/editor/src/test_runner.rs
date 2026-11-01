//! `pe2-test-runner`: the editor with no window, running tests. Built with the `tests` feature, and not shipped.
//!
//!     pe2-test-runner [--built] [--suites FOLDER ...] [--filter TEXT ...] [--list] [--json] [--artifacts FOLDER]
//!     pe2-test-runner --project PROJECT [--filter TEXT ...] [--trust-everything]
//!
//! `--built` and `--suites` run tests in projects of their own, made from what each `tests/` folder declares: the built-ins',
//! and those of these folders, each a `tests/` folder or an extension holding one; none named, the built-ins'. `--project`
//! runs the tests of the extensions a project has, in it, undoing what each changed. `--list` says what there is and runs
//! nothing, `--json` says each thing as a line of JSON, and `--artifacts` keeps what the window showed when a test failed.

fn main() {
    std::process::exit(editor::scenario::main());
}
