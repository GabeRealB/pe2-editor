//! `pe2.test`: an extension to those that require it, built into the host, there only in test mode. Its services hand each
//! step to the thread holding the harness and wait for it, so that a test runs on a thread of its own while the editor goes
//! on drawing: what the test's own instance is blocked on is never what the editor needs.

use std::sync::Mutex;
use std::sync::mpsc::{Sender, channel};

use pe2_ext::{Extension, Project, Registry, Result, Value, anyhow};

/// A step for the thread with the harness, and where its answer goes.
pub struct Step {
    pub name: String,
    pub arguments: Value,
    pub done: Sender<std::result::Result<Value, String>>,
}

/// Where steps go, while a test runs.
static STEPS: Mutex<Option<Sender<Step>>> = Mutex::new(None);

/// While a test runs, its steps come here.
pub fn listen(steps: Option<Sender<Step>>) {
    *STEPS.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = steps;
}

fn step(name: &str, arguments: &Value) -> Result<Value> {
    let (done, answer) = channel();
    let steps = STEPS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    let steps = steps.ok_or_else(|| anyhow!("no test is running"))?;
    steps.send(Step { name: name.to_string(), arguments: arguments.clone(), done }).map_err(|_| anyhow!("the editor is gone"))?;
    answer.recv().map_err(|_| anyhow!("the editor is gone"))?.map_err(|why| anyhow!(why))
}

/// What a test may ask for.
const STEPS_OFFERED: [&str; 16] = [
    "data",
    "show",
    "click",
    "click_label",
    "typing",
    "press",
    "undo",
    "drop_file",
    "node",
    "make_every_template",
    "expect",
    "expect_not",
    "problems",
    "assets",
    "selected",
    "pictured",
];

#[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
fn register(r: &mut Registry) -> Result<()> {
    for name in STEPS_OFFERED {
        let _ = r.service(name, "A step of a test", "{}", move |_: &dyn Project, arguments: &Value| step(name, arguments));
    }
    Ok(())
}

/// The extension.
#[must_use]
pub fn extension() -> Extension {
    Extension {
        id: "pe2.test",
        version: "0.1.0",
        requires: &[],
        weak: &[],
        optional: &[],
        provides: &[],
        platforms: &[],
        unsandboxed: false,
        register,
    }
}
