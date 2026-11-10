//! The roles the programs of a project play, tried with the program itself: what commands ask of a project nothing
//! holds is answered by a host started for it - one, which the next command finds, with the history the last one
//! left.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use project::host::{Connection, FromHost, ToHost};

/// A project of its own, with a data folder of its own for what runs on it; its host stopped when dropped.
struct Tried {
    root: PathBuf,
}

impl Tried {
    fn new(name: &str) -> Tried {
        let root = std::env::temp_dir().join(format!("pe2-roles-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("data")).unwrap();
        let tried = Tried { root };
        let (made, said) = tried.run(&["project", "new", &tried.project().to_string_lossy()]);
        assert!(made, "{said}");
        tried
    }

    fn project(&self) -> PathBuf {
        self.root.join("project")
    }

    /// Run the program on the project: whether it succeeded, and what it said.
    fn run(&self, args: &[&str]) -> (bool, String) {
        let data = self.root.join("data");
        let ran = Command::new(env!("CARGO_BIN_EXE_pe2-cli"))
            .args(args)
            .envs([("XDG_DATA_HOME", &data), ("APPDATA", &data), ("LOCALAPPDATA", &data)])
            .env_remove("PE2_PROJECT_LOCKED")
            .output()
            .expect("the program runs");
        let said = [ran.stdout, ran.stderr].map(|said| String::from_utf8_lossy(&said).into_owned()).concat();
        (ran.status.success(), said)
    }

    /// Run a command of `pe2-cli project` on the project.
    fn ask(&self, command: &[&str]) -> (bool, String) {
        let project = self.project();
        let mut args = vec!["project"];
        args.extend(command);
        args.extend(["--project", project.to_str().unwrap()]);
        self.run(&args)
    }

    /// The process hosting the project, where one does.
    fn host(&self) -> Option<u32> {
        match Connection::connect(&self.project())?.ask(&ToHost::Status).ok()? {
            FromHost::Status { pid, .. } => Some(pid),
            _ => None,
        }
    }
}

impl Drop for Tried {
    fn drop(&mut self) {
        let _ = self.ask(&["serve", "--stop"]);
        within(Duration::from_secs(20), || !project::host::running(&self.project()));
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Wait until something holds, as long as given: whether it came to.
fn within(long: Duration, holds: impl Fn() -> bool) -> bool {
    let asked = Instant::now();
    while asked.elapsed() < long {
        if holds() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    holds()
}

fn written(at: &Path, text: &str) -> String {
    std::fs::write(at, text).unwrap();
    at.to_string_lossy().into_owned()
}

#[test]
fn what_commands_ask_is_answered_by_one_host_started_for_it() {
    let tried = Tried::new("one-host");
    let from = written(&tried.root.join("from.txt"), "one");
    assert!(!project::host::running(&tried.project()), "nothing hosts a project nothing was asked of");
    // The first edit starts a host, which holds the project and serves its folder nowhere.
    let (made, said) = tried.ask(&["write", "notes/a.txt", "--from", &from]);
    assert!(made, "{said}");
    let host = tried.host().expect("a host was started");
    assert!(!tried.project().join(project::SERVED).exists(), "its folder is served nowhere");
    // The next finds it: the same one, with the history the first left - so what the first did is taken back.
    let (made, said) = tried.ask(&["write", "notes/b.txt", "--from", &from]);
    assert!(made, "{said}");
    assert_eq!(tried.host(), Some(host), "one host, kept");
    let (undone, said) = tried.ask(&["undo"]);
    assert!(undone && said.contains("b.txt"), "the last edit is taken back: {said}");
    let (undone, said) = tried.ask(&["undo"]);
    assert!(undone && said.contains("a.txt"), "and the one before it: {said}");
    // Told to stop, it is gone.
    let (stopped, said) = tried.ask(&["serve", "--stop"]);
    assert!(stopped, "{said}");
    assert!(within(Duration::from_secs(20), || !project::host::running(&tried.project())), "the host stops");
}
