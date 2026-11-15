//! The roles the programs of a project play, tried with the program itself: what commands ask of a project nothing
//! holds is answered by a host started for it - one, which the next command finds, with the history the last one
//! left; its folder is served by a server of its own, which whoever holds the project feeds, and which serves on
//! as the project changes hands.

#![allow(
    clippy::disallowed_methods,
    reason = "a test's fixtures: the machine's time, places and environment, which nothing tested reads"
)]
use sfs::HostPath;
use sfs::testing::At as _;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use project::host::{Connection, FromHost, ToHost};
use project::server::{FromServer, ToServer};

/// A project of its own, with a data folder of its own for what runs on it; its host stopped when dropped.
struct Tried {
    root: HostPath,
}

impl Tried {
    fn new(name: &str) -> Tried {
        let root = sfs::testing::scratch(&format!("roles-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.at("data")).unwrap();
        let tried = Tried { root };
        let (made, said) = tried.run(&["project", "new", &tried.project().to_string_lossy()]);
        assert!(made, "{said}");
        tried
    }

    fn project(&self) -> HostPath {
        self.root.at("project")
    }

    /// Run the program on the project: whether it succeeded, and what it said.
    fn run(&self, args: &[&str]) -> (bool, String) {
        self.run_with("data", args)
    }

    /// Run the program with a data folder of this name, of the project's own.
    fn run_with(&self, data: &str, args: &[&str]) -> (bool, String) {
        let data = self.root.at(data);
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

    /// Run a command of `pe2-cli project` on the project.
    fn ask(&self, command: &[&str]) -> (bool, String) {
        self.ask_with("data", command)
    }

    /// Run a command of `pe2-cli project` on the project, with a data folder of this name.
    fn ask_with(&self, data: &str, command: &[&str]) -> (bool, String) {
        let project = self.project();
        let mut args = vec!["project"];
        args.extend(command);
        args.extend(["--project", project.to_str().unwrap()]);
        self.run_with(data, &args)
    }

    /// The process hosting the project, where one does.
    fn host(&self) -> Option<u32> {
        match Connection::connect(&self.project(), &pe2_io::System)?.ask(&ToHost::Status).ok()? {
            FromHost::Status { pid, .. } => Some(pid),
            _ => None,
        }
    }

    /// Who holds the project, as its host says.
    fn holder(&self) -> Option<String> {
        match Connection::connect(&self.project(), &pe2_io::System)?.ask(&ToHost::Status).ok()? {
            FromHost::Status { holder, .. } => Some(holder),
            _ => None,
        }
    }

    /// The process serving the project's folder, and who feeds it, where one serves.
    fn server(&self) -> Option<(u32, Option<String>)> {
        match project::server::status(&self.project(), &pe2_io::System)? {
            FromServer::Status { pid, fed_by, .. } => Some((pid, fed_by)),
            _ => None,
        }
    }

    /// What the server and the host said, in their logs.
    fn said(&self) -> (String, String) {
        let log = |role: &project::role::Role| {
            std::fs::read_to_string(self.project().join(&project::cache::DIR).join(&role.log)).unwrap_or_default()
        };
        (log(&project::server::ROLE), log(&project::host::ROLE))
    }

    /// Whether the host said this of what it did, in its log.
    fn host_said(&self, what: &str) -> bool {
        let log = self.project().join(&project::cache::DIR).join(&project::host::ROLE.log);
        std::fs::read_to_string(log).is_ok_and(|said| said.lines().any(|line| line == what))
    }

    /// Keep the project's folder served (`project serve`) over WebDAV, at an address the system picks, until what
    /// this returns is dropped: nothing is mounted, and what programs do there is asked over HTTP.
    fn serve(&self) -> Served {
        use std::io::BufRead as _;
        let data = self.root.at("data");
        let mut keeping = Command::new(env!("CARGO_BIN_EXE_pe2-cli"))
            .args(["project", "serve", "--backend", "webdav", "--address", "127.0.0.1:0", "--project"])
            .arg(self.project().as_os_str())
            .envs([
                ("XDG_DATA_HOME", data.as_os_str()),
                ("APPDATA", data.as_os_str()),
                ("LOCALAPPDATA", data.as_os_str()),
            ])
            .env_remove("PE2_PROJECT_LOCKED")
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("the program runs");
        let mut said = String::new();
        let mut says = std::io::BufReader::new(keeping.stdout.take().unwrap());
        says.read_line(&mut said).unwrap();
        let at = said.split("served at http://").nth(1).and_then(|rest| rest.split('/').next());
        let at = at.unwrap_or_else(|| panic!("it says where it is served: {said}")).to_string();
        Served { keeping, _says: says, at }
    }
}

/// A project's folder kept served over WebDAV, at this address, while this is held.
struct Served {
    keeping: std::process::Child,
    /// What it goes on to say, taken for as long as it runs.
    _says: std::io::BufReader<std::process::ChildStdout>,
    at: String,
}

impl Drop for Served {
    fn drop(&mut self) {
        let _ = self.keeping.kill();
        let _ = self.keeping.wait();
    }
}

impl Served {
    /// Ask something of what serves the folder, as a program would of a file there: the status answered, and the
    /// content.
    fn http(&self, method: &str, path: &str, body: &[u8]) -> (u16, Vec<u8>) {
        use std::io::{Read as _, Write as _};
        let mut stream = std::net::TcpStream::connect(&self.at).expect("it listens");
        let head = format!(
            "{method} /{path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
            self.at,
            body.len()
        );
        stream.write_all(head.as_bytes()).unwrap();
        stream.write_all(body).unwrap();
        let mut answer = Vec::new();
        let _ = stream.read_to_end(&mut answer);
        let split = answer.windows(4).position(|w| w == b"\r\n\r\n").expect("an answer");
        let head = String::from_utf8_lossy(&answer[..split]).to_lowercase();
        let status = head.split(' ').nth(1).and_then(|s| s.parse().ok()).expect("a status");
        let mut content = answer[split + 4..].to_vec();
        if head.contains("transfer-encoding: chunked") {
            let (mut whole, mut rest) = (Vec::new(), content.as_slice());
            while let Some(end) = rest.windows(2).position(|w| w == b"\r\n") {
                let size = usize::from_str_radix(String::from_utf8_lossy(&rest[..end]).trim(), 16).unwrap_or(0);
                if size == 0 {
                    break;
                }
                whole.extend_from_slice(&rest[end + 2..end + 2 + size]);
                rest = &rest[end + 2 + size + 2..];
            }
            content = whole;
        }
        (status, content)
    }

    /// A file of the folder, read; none where it is not there.
    fn read(&self, path: &str) -> Option<String> {
        let (status, content) = self.http("GET", path, b"");
        (status == 200).then(|| String::from_utf8_lossy(&content).into_owned())
    }

    /// A file of the folder, written.
    fn write(&self, path: &str, text: &str) {
        let (status, _) = self.http("PUT", path, text.as_bytes());
        assert!((200..300).contains(&status), "{path} is written: {status}");
    }
}

/// A stand-in for an editor: it takes the project as one does - borrowed from the host - holds it as any holder
/// does (`holder::core::Core`), answers the edits sent on the holder's link as steps of a history of its own, and
/// feeds the server what is served, until dropped.
struct StandIn {
    _link: mcp::link::Listener,
    files: Arc<holder::files::Files>,
    /// The edits it made, by their labels.
    made: Arc<Mutex<Vec<String>>>,
    _held: Arc<Mutex<(holder::core::Core, project::history::History)>>,
    _lock: project::lock::Lock,
}

impl StandIn {
    fn take(root: &HostPath) -> StandIn {
        let p = project::Project::load(root).unwrap();
        let lock = project::lock::Lock::acquire(root, "a stand-in", &pe2_io::System, |_| {}).unwrap();
        let cache = project::cache::Cache::open(&p, &lock).unwrap();
        let (history, _) = project::history::History::open(&cache).unwrap();
        let index = project::index::Index::open(&p, &cache).unwrap();
        let says = Arc::new(holder::says::Says::of(&p));
        let mut core = holder::core::Core::new(p.clone(), index, says.clone());
        core.took(|| {});
        let files = Arc::new(holder::files::Files::feed(&p, Vec::new(), says));
        core.feed_whole(files.clone());
        let made = Arc::new(Mutex::new(Vec::new()));
        let noting = made.clone();
        core.made = Box::new(move |label| noting.lock().unwrap().push(label.to_string()));
        let held = Arc::new(Mutex::new((core, history)));
        let holding = held.clone();
        let answer = move |request: mcp::Request| {
            let mut held = holding.lock().unwrap();
            let (core, history) = &mut *held;
            match request {
                // A command's edit comes as it is; what is due is taken where it is said.
                mcp::Request::Edit { op, layer } => {
                    core.edits(history, &[op], layer.as_deref()).0.pop().unwrap_or(mcp::Reply::Declined)
                }
                mcp::Request::Due => core.take_due(history, Ok(None), None).0,
                _ => mcp::Reply::Declined,
            }
        };
        let link = mcp::link::Listener::start(root, pe2_io::system(), answer).unwrap();
        StandIn { _link: link, files, made, _held: held, _lock: lock }
    }

    fn made(&self, label: &str) -> bool {
        self.made.lock().unwrap().iter().any(|made| made == label)
    }
}

impl Drop for Tried {
    fn drop(&mut self) {
        let _ = self.ask(&["serve", "--stop"]);
        let project = self.project();
        until(|| !project::host::running(&project) && !project::server::running(&project));
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Wait until something holds, as long as that takes: nothing here promises when, and what never comes is a test that
/// does not end.
fn until(holds: impl Fn() -> bool) {
    while !holds() {
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn written(at: &HostPath, text: &str) -> String {
    std::fs::write(at, text).unwrap();
    at.to_string_lossy().into_owned()
}

#[test]
fn what_commands_ask_is_answered_by_one_host_started_for_it() {
    let tried = Tried::new("one-host");
    let from = written(&tried.root.at("from.txt"), "one");
    assert!(!project::host::running(&tried.project()), "nothing hosts a project nothing was asked of");
    // The first edit starts a host, which holds the project and serves its folder nowhere.
    let (made, said) = tried.ask(&["write", "notes/a.txt", "--from", &from]);
    assert!(made, "{said}");
    let host = tried.host().expect("a host was started");
    assert!(!tried.project().join(&project::SERVED).exists(), "its folder is served nowhere");
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
    until(|| !project::host::running(&tried.project())); // the host stops
}

#[test]
fn a_host_started_under_other_settings_is_replaced_by_who_asks_next() {
    let tried = Tried::new("identity");
    let from = written(&tried.root.at("from.txt"), "one");
    let (made, said) = tried.ask(&["write", "a.txt", "--from", &from]);
    assert!(made, "{said}");
    let first = tried.host().expect("a host was started");
    // Asked again as it was started: the same host answers.
    let (made, said) = tried.ask(&["write", "b.txt", "--from", &from]);
    assert!(made && tried.host() == Some(first), "{said}");
    // Asked with another data folder - other settings, other trust: that host is stopped, and another answers.
    std::fs::create_dir_all(tried.root.at("other")).unwrap();
    let (made, said) = tried.ask_with("other", &["write", "c.txt", "--from", &from]);
    assert!(made && said.contains("was stopped"), "{said}");
    let second = tried.host().expect("another host was started");
    assert_ne!(second, first, "a host of the asker's own");
}

#[test]
fn what_events_set_off_is_run_by_the_host_which_lends_the_project_after() {
    let tried = Tried::new("epochs");
    let (configured, said) = tried.ask(&["config", "add", "test"]);
    assert!(configured, "{said}");
    let from = written(&tried.root.at("from.txt"), "one");
    let (made, said) = tried.ask(&["write", "a.txt", "--from", &from]);
    assert!(made, "{said}");
    let host = tried.host().expect("a host was started");
    // A build is the host's to run: the command asks it, and takes the project for nothing.
    let (built, said) = tried.ask(&["build"]);
    assert!(built, "{said}");
    assert_eq!(tried.host(), Some(host), "the same host, which ran it");
    // What works on the storage itself borrows the project, and the host takes it back.
    let (collected, said) = tried.ask(&["store", "gc"]);
    assert!(collected, "{said}");
    let (made, said) = tried.ask(&["write", "b.txt", "--from", &from]);
    assert!(made && tried.host() == Some(host), "{said}");
}

#[test]
fn a_folder_served_is_a_servers_which_the_host_feeds() {
    let tried = Tried::new("served");
    let from = written(&tried.root.at("from.txt"), "one");
    let folder = tried.serve();
    // Two processes: the host holding the project, and the server serving its folder, which the host feeds.
    let host = tried.host().expect("a host holds it");
    let (server, fed_by) = tried.server().expect("a server serves it");
    assert_ne!(host, server, "the server is a process of its own");
    assert_eq!(fed_by.as_deref(), Some("pe2-cli"), "fed by the host");
    // What a command changes is in the folder once the command returns.
    let (made, said) = tried.ask(&["write", "notes/a.txt", "--from", &from]);
    assert!(made, "{said}");
    assert_eq!(folder.read("notes/a.txt").as_deref(), Some("one"));
    // What a program writes there is the host's step, as the command's was: the next undo takes it back.
    folder.write("b.txt", "two");
    until(|| tried.host_said("New b.txt")); // what is written is the project's
    assert_eq!(folder.read("b.txt").as_deref(), Some("two"));
    let (undone, said) = tried.ask(&["undo"]);
    assert!(undone && said.contains("b.txt"), "{said}");
    assert_eq!(folder.read("b.txt"), None, "taken back, it is gone from the folder");
    // And the filesystem the project's processes share says the same of what is served, to whoever attaches.
    let shared = project::shared::space(&tried.project(), &pe2_io::SystemFilesystem).expect("the shared space");
    let said = project::shared::listed(&shared.read().expect("read")).expect("listed");
    assert!(
        matches!(
            said.get(project::TreePath::new("notes/a.txt").unwrap()),
            Some(project::server::Shown::File { size: 3, .. })
        ),
        "{:?}\n{:?}",
        said.keys(),
        tried.said()
    );
    assert!(
        matches!(said.get(project::TreePath::new("notes").unwrap()), Some(project::server::Shown::Folder))
            && !said.contains_key(project::TreePath::new("b.txt").unwrap())
    );
    drop(shared);
    // One feeder at a time: what another process lists while the host feeds it is not taken.
    let mut other = project::server::ROLE.connect(&tried.project(), &pe2_io::System).expect("the server listens");
    let listed: FromServer = other.ask(&ToServer::Shown).unwrap();
    assert_eq!(listed, FromServer::Fed { by: "pe2-cli".into() });
    assert_eq!(folder.read("notes/a.txt").as_deref(), Some("one"));
    // Stopped, both are gone.
    let (stopped, said) = tried.ask(&["serve", "--stop"]);
    assert!(stopped, "{said}");
    let gone = || !project::host::running(&tried.project()) && !project::server::running(&tried.project());
    until(gone); // the host and the server stop
}

#[test]
fn the_project_changes_hands_and_its_folder_is_served_throughout() {
    let tried = Tried::new("hands");
    let from = written(&tried.root.at("from.txt"), "one");
    let folder = tried.serve();
    let (server, _) = tried.server().expect("a server serves it");
    let (made, said) = tried.ask(&["write", "a.txt", "--from", &from]);
    assert!(made, "{said}");
    // Another takes the project, as an editor does: the host lends it, and feeds the server no more - who holds the
    // project does, on the same server.
    let stand_in = StandIn::take(&tried.project());
    assert_eq!(tried.holder().as_deref(), Some("a stand-in"));
    let fed_by_it = || tried.server().is_some_and(|(_, by)| by.is_some_and(|by| by != "pe2-cli"));
    until(fed_by_it); // whoever holds the project feeds the server
    // The server says who feeds it a moment before that one has its answer.
    let knows = || matches!(stand_in.files.state(), holder::files::State::Served(..));
    until(knows); // and knows it is served
    assert_eq!(folder.read("a.txt").as_deref(), Some("one"), "served on");
    // What a program writes now is made by who holds the project.
    folder.write("c.txt", "three");
    until(|| stand_in.made("New c.txt")); // the holder makes the edit
    assert_eq!(folder.read("c.txt").as_deref(), Some("three"));
    // Let go of - said first, as an editor closing says it - the project is the host's again without it looking for
    // it: it feeds the server what is served now, the other's edit in it.
    let handing = project::host::leaving(&tried.project(), "a stand-in", &pe2_io::System).expect("the host hears it");
    drop(stand_in);
    drop(handing);
    assert!(tried.host_said("a stand-in is letting go of the project"));
    let back = || {
        tried.holder().as_deref() == Some("the host")
            && tried.server().is_some_and(|(_, by)| by.as_deref() == Some("pe2-cli"))
    };
    until(back); // the host takes the project back, and feeds the server
    assert_eq!(folder.read("c.txt").as_deref(), Some("three"));
    folder.write("d.txt", "four");
    until(|| tried.host_said("New d.txt")); // what is written is the host's step
    assert_eq!(tried.server().map(|(pid, _)| pid), Some(server), "one server, throughout");
}

#[test]
fn what_is_written_while_nothing_holds_the_project_starts_a_host() {
    let tried = Tried::new("orphan");
    let folder = tried.serve();
    let host = tried.host().expect("a host holds it");
    // Its host gone - told to stop, as one that differs is - the server serves on what it was listed last.
    let stopping =
        Connection::connect(&tried.project(), &pe2_io::System).and_then(|mut host| host.ask(&ToHost::Stop).ok());
    assert_eq!(stopping, Some(FromHost::Stopping));
    until(|| !project::host::running(&tried.project())); // the host stops
    assert!(tried.server().is_some(), "the server serves on for a while");
    // What a program writes now has a host started to hold the project, which feeds the server from then on.
    folder.write("e.txt", "five");
    // A host is started, and makes it.
    until(|| tried.host_said("New e.txt"));
    assert_ne!(tried.host(), Some(host), "another host");
    assert_eq!(folder.read("e.txt").as_deref(), Some("five"));
}

#[test]
fn what_is_written_while_a_tool_has_the_project_is_taken_in_once_it_is_done() {
    let tried = Tried::new("busy");
    let folder = tried.serve();
    // A tool takes the project, as an extraction does: it answers no edits.
    let tool = project::lock::Lock::acquire(&tried.project(), "a tool", &pe2_io::System, |_| {}).unwrap();
    assert_eq!(tried.holder().as_deref(), Some("a tool"));
    // What a program writes meanwhile is kept, shown as written, and is nobody's step yet.
    folder.write("meanwhile.txt", "kept");
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(folder.read("meanwhile.txt").as_deref(), Some("kept"), "shown as written");
    assert!(!tried.host_said("New meanwhile.txt"));
    // The tool done, the host has the project again, and what was kept is its step.
    drop(tool);
    until(|| tried.host_said("New meanwhile.txt")); // taken in once it is done
    assert_eq!(folder.read("meanwhile.txt").as_deref(), Some("kept"));
    let (undone, said) = tried.ask(&["undo"]);
    assert!(undone && said.contains("meanwhile.txt"), "{said}");
}
