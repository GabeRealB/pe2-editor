//! A project's records merged by git through the project's driver, tried with the programs themselves: two branches'
//! edits of different fields of one node merged by what they change; the same field changed both ways a conflict the
//! project says, settled by keeping one side.

#![allow(
    clippy::disallowed_methods,
    reason = "a test's fixtures: the machine's time, places and environment, which nothing tested reads"
)]
use sfs::HostPath;
use sfs::testing::At as _;
use std::process::Command;

/// A project made in a git repository of its own, with a data folder of its own for what runs on it.
struct Repo {
    root: HostPath,
}

impl Repo {
    fn new(name: &str) -> Repo {
        let root = sfs::testing::scratch(&format!("merges-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.at("data")).unwrap();
        std::fs::create_dir_all(root.at("project")).unwrap();
        let repo = Repo { root };
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.name", "a test"],
            &["config", "user.email", "test@example.invalid"],
        ] {
            repo.git(args);
        }
        let (made, said) = repo.run(&["project", "new", &repo.project().to_string_lossy()]);
        assert!(made, "{said}");
        repo
    }

    fn project(&self) -> HostPath {
        self.root.at("project")
    }

    /// Run the program: whether it succeeded, and what it said.
    fn run(&self, args: &[&str]) -> (bool, String) {
        let data = self.root.at("data");
        let ran = Command::new(env!("CARGO_BIN_EXE_pe2-cli"))
            .args(args)
            .envs([
                ("XDG_DATA_HOME", data.as_os_str()),
                ("APPDATA", data.as_os_str()),
                ("LOCALAPPDATA", data.as_os_str()),
            ])
            .env_remove("PE2_PROJECT_LOCKED")
            .output()
            .expect("the program runs");
        let said = [ran.stdout, ran.stderr].map(|said| String::from_utf8_lossy(&said).into_owned()).concat();
        (ran.status.success(), said)
    }

    /// Run a command of `pe2-cli project` on the project, and stop what it started holding it: git changes its
    /// files next, as another program's.
    fn ask(&self, command: &[&str]) -> (bool, String) {
        let project = self.project();
        let mut args = vec!["project"];
        args.extend(command);
        args.extend(["--project", project.to_str().unwrap()]);
        let said = self.run(&args);
        self.stop();
        said
    }

    fn stop(&self) {
        let project = self.project();
        let _ = self.run(&["project", "serve", "--stop", "--project", project.to_str().unwrap()]);
        while project::host::running(&project) || project::server::running(&project) {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// Run git in the project's folder: whether it succeeded, and what it said.
    fn git(&self, args: &[&str]) -> (bool, String) {
        let ran = Command::new("git").args(args).current_dir(self.project()).output().expect("git runs");
        let said = [ran.stdout, ran.stderr].map(|said| String::from_utf8_lossy(&said).into_owned()).concat();
        (ran.status.success(), said)
    }

    /// Write the node `d`'s declaration through the program.
    fn declare(&self, text: &str) {
        let file = self.root.at("node.toml");
        std::fs::write(&file, text).unwrap();
        let (wrote, said) = self.ask(&["write", "d/node.toml", "--from", file.to_str().unwrap()]);
        assert!(wrote, "{said}");
    }

    /// Commit everything as it is.
    fn commit(&self, message: &str) {
        assert!(self.git(&["add", "-A"]).0);
        let (done, said) = self.git(&["commit", "-q", "-m", message]);
        assert!(done, "{said}");
    }

    /// The hash of the node's declaration as the project resolves it, and its problems.
    fn resolved(&self) -> (String, String) {
        let (dumped, said) = self.ask(&["dump"]);
        assert!(dumped, "{said}");
        let dump: serde_json::Value = serde_json::from_str(said.trim()).unwrap_or_else(|e| panic!("{e}: {said}"));
        let node = dump["entries"].as_array().unwrap().iter().find(|e| e["path"] == "d.node").expect("the node");
        let declaration = node["files"].as_array().unwrap().iter().find(|f| f["name"] == "node.toml").unwrap();
        (declaration["sha1"].as_str().unwrap().to_string(), dump["problems"].to_string())
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        self.stop();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn sha1(text: &str) -> String {
    project::extract::sha1_of(text.as_bytes()).to_string()
}

#[test]
fn two_branches_edits_of_one_node_merge_by_what_they_change_and_a_conflict_is_settled() {
    let repo = Repo::new("node");
    // Registered as the project was made, in its repository.
    let (_, driver) = repo.git(&["config", "--get", "merge.pe2.driver"]);
    assert!(driver.contains("project merge %O %A %B %P"), "{driver}");
    repo.declare("size = 2\nmode = \"a\"\n");
    repo.commit("a node");
    // Each branch a field of it.
    assert!(repo.git(&["checkout", "-q", "-b", "ours"]).0);
    repo.declare("size = 3\nmode = \"a\"\n");
    repo.commit("its size");
    assert!(repo.git(&["checkout", "-q", "-b", "theirs", "main"]).0);
    repo.declare("size = 2\nmode = \"b\"\n");
    repo.commit("its mode");
    assert!(repo.git(&["checkout", "-q", "ours"]).0);
    let (merged, said) = repo.git(&["merge", "-q", "--no-edit", "theirs"]);
    assert!(merged, "{said}");
    let (declared, problems) = repo.resolved();
    assert_eq!(declared, sha1("size = 3\nmode = \"b\"\n"), "{problems}");
    assert_eq!(problems, "[]");
    // The same field both ways: a conflict, the project says it, settled by taking the other side.
    assert!(repo.git(&["checkout", "-q", "-b", "other"]).0);
    repo.declare("size = 5\nmode = \"b\"\n");
    repo.commit("another size");
    assert!(repo.git(&["checkout", "-q", "ours"]).0);
    repo.declare("size = 4\nmode = \"b\"\n");
    repo.commit("a size");
    let (merged, _) = repo.git(&["merge", "-q", "--no-edit", "other"]);
    assert!(!merged, "a conflict");
    let (declared, problems) = repo.resolved();
    assert_eq!(declared, sha1("size = 4\nmode = \"b\"\n"), "ours, as it was");
    assert!(problems.contains("a merge left"), "{problems}");
    // Its uid: the record holding both sides.
    let records = repo.project().at("layers/project");
    let uid = std::fs::read_dir(&records)
        .unwrap()
        .flatten()
        .filter(|shard| shard.path().is_dir())
        .flat_map(|shard| std::fs::read_dir(shard.path()).unwrap().flatten())
        .find(|record| std::fs::read_to_string(record.path()).unwrap().contains("[merge]"))
        .map(|record| format!("uid://{}", record.file_name().to_string_lossy().trim_end_matches(".toml")))
        .expect("the record holding both sides");
    let (settled, said) = repo.ask(&["settle", "project", &uid, "--theirs"]);
    assert!(settled, "{said}");
    let (declared, problems) = repo.resolved();
    assert_eq!((declared, problems), (sha1("size = 5\nmode = \"b\"\n"), "[]".to_string()));
    repo.commit("settled");
}
