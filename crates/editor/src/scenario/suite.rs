//! Suites: an extension's tests, in a `tests/` folder of it, with what they need. For each test the harness makes a project
//! from what the suite declares, opens it and takes it away, so that no test sees what another did. A test can be left out
//! of what ships by removing the folder.
//!
//! ```text
//! render/
//!     extension.wasm
//!     tests/
//!         tests.wasm      the tests: a component, as any extension is
//!         test.toml       what a project for them holds
//!         data/           what they make their nodes from
//! ```

use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::{Fixture, Scenario, isolate};

/// `test.toml`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Declared {
    /// Its layers, the tests in the first; one otherwise.
    #[serde(default)]
    pub layers: Vec<String>,
    /// Its configs.
    #[serde(default)]
    pub configs: Vec<String>,
    /// Extensions built apart it holds as well, by id.
    #[serde(default)]
    pub take: Vec<String>,
    /// Extensions it leaves off, by id.
    #[serde(default)]
    pub disabled: Vec<String>,
    /// Whether what it holds is trusted.
    #[serde(default)]
    pub trust: bool,
}

/// A `tests/` folder.
pub struct Suite {
    /// What it is called: the extension it is of.
    pub name: String,
    pub folder: PathBuf,
    pub declared: Declared,
}

impl Suite {
    /// The suite in a `tests/` folder.
    ///
    /// # Errors
    /// Its `test.toml` cannot be read.
    pub fn at(folder: &Path) -> Result<Suite, String> {
        let name = folder.parent().and_then(Path::file_name).map_or_else(|| "tests".to_string(), |name| name.to_string_lossy().into_owned());
        let manifest = folder.join("test.toml");
        let text = if manifest.is_file() { std::fs::read_to_string(&manifest).map_err(|e| format!("{}: {e}", manifest.display()))? } else { String::new() };
        let declared = toml::from_str(&text).map_err(|e| format!("{}: {e}", manifest.display()))?;
        Ok(Suite { name, folder: folder.to_path_buf(), declared })
    }

    /// A project of what it declares.
    fn scratch(&self) -> Fixture {
        Fixture::for_suite(self)
    }

    /// Its tests' names: only a project they are loaded in can say.
    fn tests(&self) -> Result<Vec<String>, String> {
        let mut editor = Scenario::open_with(self.scratch(), None, true);
        let tests = editor.workspace().extensions.tests();
        if tests.is_empty() {
            let present = editor.workspace().extensions.present.iter().map(|p| format!("{}: {:?}", p.id, p.status)).collect::<Vec<_>>();
            return Err(format!("{}: it has no tests: {present:?}", self.name));
        }
        Ok(tests)
    }
}

/// What came of running suites.
#[derive(Debug, Default)]
pub struct Report {
    pub passed: Vec<String>,
    pub failed: Vec<(String, String)>,
}

impl Report {
    /// Whether anything ran and everything passed.
    #[must_use]
    pub fn passed(&self) -> bool {
        !self.passed.is_empty() && self.failed.is_empty()
    }
}

/// How a run says what happens.
#[derive(Clone, Debug, Default)]
pub struct Out {
    /// List what there is, running nothing.
    pub list: bool,
    /// Each thing as a line of JSON, for whatever is reading.
    pub json: bool,
    /// Where to keep what the window showed when a test failed.
    pub artifacts: Option<PathBuf>,
}

impl Out {
    /// A thing that happened, as what is reading wants it.
    fn say(&self, json: &serde_json::Value, text: impl FnOnce() -> String) {
        if self.json {
            println!("{json}");
        } else {
            println!("{}", text());
        }
    }
}

/// Whether a test is among those asked for.
#[must_use]
pub fn wanted(name: &str, filters: &[String]) -> bool {
    filters.is_empty() || filters.iter().any(|filter| name.contains(filter.as_str()))
}

/// Say the tests of these suites; whether every suite could say.
#[must_use]
pub fn list(suites: &[Suite], out: &Out) -> bool {
    isolate();
    let mut every = true;
    for suite in suites {
        match suite.tests() {
            Ok(tests) => {
                for name in tests {
                    out.say(&serde_json::json!({ "event": "test", "suite": suite.name, "name": name }), || name.clone());
                }
            }
            Err(why) => {
                every = false;
                eprintln!("{why}");
            }
        }
    }
    every
}

/// Run each test of these suites asked for, in a project of its own.
#[must_use]
pub fn run(suites: &[Suite], filters: &[String], out: &Out) -> Report {
    isolate();
    let mut report = Report::default();
    for suite in suites {
        let mut tests = match suite.tests() {
            Ok(tests) => tests,
            Err(why) => {
                out.say(&serde_json::json!({ "event": "failed", "suite": suite.name, "name": suite.name, "why": why }), || format!("FAILED  {}\n{why}", suite.name));
                report.failed.push((suite.name.clone(), why));
                continue;
            }
        };
        tests.retain(|name| wanted(name, filters));
        for name in tests {
            out.say(&serde_json::json!({ "event": "started", "suite": suite.name, "name": name }), String::new);
            let started = std::time::Instant::now();
            let mut editor = Scenario::open_with(suite.scratch(), None, true);
            let result = editor.run_test(&name);
            let kept = result.as_ref().err().zip(out.artifacts.as_ref()).map(|(_, folder)| editor.keep_failure(folder, &name));
            drop(editor);
            let seconds = started.elapsed().as_secs_f64();
            match result {
                Ok(()) => {
                    out.say(&serde_json::json!({ "event": "passed", "suite": suite.name, "name": name, "seconds": seconds }), || format!("ok      {name}"));
                    report.passed.push(name);
                }
                Err(why) => {
                    let (said, picture) = kept.unwrap_or_default();
                    let event = serde_json::json!({ "event": "failed", "suite": suite.name, "name": name, "seconds": seconds, "why": why, "said": said, "picture": picture });
                    out.say(&event, || format!("FAILED  {name}\n{why}"));
                    report.failed.push((name, why));
                }
            }
        }
    }
    let (passed, failed) = (report.passed.len(), report.failed.len());
    out.say(&serde_json::json!({ "event": "done", "passed": passed, "failed": failed }), || format!("{passed} passed, {failed} failed"));
    report
}

/// The suites built with the editor.
#[must_use]
pub fn built() -> Vec<Suite> {
    let folders = extensions::built_suites();
    folders.iter().map(|folder| Suite::at(folder).unwrap_or_else(|why| panic!("{why}"))).collect()
}

/// The suites in these folders: each a `tests/` folder, or an extension holding one.
///
/// # Errors
/// One holds no tests, or its `test.toml` cannot be read.
pub fn in_folders(folders: &[PathBuf]) -> Result<Vec<Suite>, String> {
    let found = |folder: &PathBuf| {
        let inside = folder.join("tests");
        let folder = if inside.is_dir() { inside } else { folder.clone() };
        if folder.join("tests.wasm").is_file() { Suite::at(&folder) } else { Err(format!("{}: no tests.wasm", folder.display())) }
    };
    folders.iter().map(found).collect()
}
