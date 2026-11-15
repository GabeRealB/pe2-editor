//! Programs started: a [`Program`] says what is run, with what and where its streams go; a [`Process`] is one running.
//! The system's starts them as `std::process` does - on Windows never with a window of its own (`CREATE_NO_WINDOW`):
//! started by a process with no console, a console program would be given a terminal window each, a build's hundreds
//! of them. [`Scripted`] answers what a test says.

use crate::HostPath;
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;

/// Where one of a program's streams goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stream {
    /// Nowhere: read, it ends at once; written, it is dropped.
    Null,
    /// To this process's own.
    Inherit,
    /// To this process, through a pipe taken from the [`Process`].
    Piped,
    /// To what this process says as it goes wrong (its standard error).
    Errors,
    /// To a file, made anew; where its output and its errors name the same one, both go there.
    Into(HostPath),
}

/// A program to run: by default given nothing to read, saying what it says where this process does.
#[derive(Clone, Debug)]
pub struct Program {
    /// What is run: a path, or a name the system finds.
    pub program: OsString,
    pub args: Vec<OsString>,
    /// What its environment holds besides this process's.
    pub env: Vec<(OsString, OsString)>,
    /// The folder it starts in; this process's where none.
    pub folder: Option<PathBuf>,
    pub stdin: Stream,
    pub stdout: Stream,
    pub stderr: Stream,
    /// A process group of its own: a Ctrl-C in the terminal that started this process is not its, nor is that
    /// terminal's closing.
    pub own_group: bool,
}

impl Program {
    /// `program`, with nothing else said.
    #[must_use]
    pub fn new(program: impl Into<OsString>) -> Program {
        Program {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
            folder: None,
            stdin: Stream::Null,
            stdout: Stream::Inherit,
            stderr: Stream::Inherit,
            own_group: false,
        }
    }

    #[must_use]
    pub fn arg(mut self, arg: impl Into<OsString>) -> Program {
        self.args.push(arg.into());
        self
    }

    #[must_use]
    pub fn args<A: Into<OsString>>(mut self, args: impl IntoIterator<Item = A>) -> Program {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    #[must_use]
    pub fn env(mut self, name: impl Into<OsString>, value: impl Into<OsString>) -> Program {
        self.env.push((name.into(), value.into()));
        self
    }

    #[must_use]
    pub fn folder(mut self, folder: impl Into<PathBuf>) -> Program {
        self.folder = Some(folder.into());
        self
    }

    /// Where its input, output and errors go.
    #[must_use]
    pub fn streams(mut self, stdin: Stream, stdout: Stream, stderr: Stream) -> Program {
        (self.stdin, self.stdout, self.stderr) = (stdin, stdout, stderr);
        self
    }

    #[must_use]
    pub fn own_group(mut self) -> Program {
        self.own_group = true;
        self
    }
}

/// How a process ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exit {
    code: Option<i32>,
    said: String,
}

impl Exit {
    /// Ended with this code.
    #[must_use]
    pub fn code(code: i32) -> Exit {
        Exit { code: Some(code), said: format!("exit code: {code}") }
    }

    /// Whether it says it did what it was asked (its code is 0).
    #[must_use]
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    /// Its code; none where a signal ended it.
    #[must_use]
    pub fn exit_code(&self) -> Option<i32> {
        self.code
    }
}

impl From<std::process::ExitStatus> for Exit {
    fn from(status: std::process::ExitStatus) -> Exit {
        Exit { code: status.code(), said: status.to_string() }
    }
}

impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.said)
    }
}

/// What a program run to its end said, and how it ended.
#[derive(Clone, Debug)]
pub struct Output {
    pub exit: Exit,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// A program running.
pub trait Process: Send {
    /// Its process's number.
    fn id(&self) -> u32;

    /// How it ended, where it has.
    ///
    /// # Errors
    /// The system cannot say.
    fn try_wait(&mut self) -> io::Result<Option<Exit>>;

    /// Wait for it to end.
    ///
    /// # Errors
    /// The system cannot say.
    fn wait(&mut self) -> io::Result<Exit>;

    /// End it.
    ///
    /// # Errors
    /// It cannot be ended.
    fn kill(&mut self) -> io::Result<()>;

    /// Its input, where it is piped: taken once.
    fn take_stdin(&mut self) -> Option<Box<dyn Write + Send>>;

    /// Its output, where it is piped: taken once.
    fn take_stdout(&mut self) -> Option<Box<dyn Read + Send>>;

    /// Its errors, where they are piped: taken once.
    fn take_stderr(&mut self) -> Option<Box<dyn Read + Send>>;
}

/// What starts programs.
pub trait Processes: Send + Sync {
    /// Start a program.
    ///
    /// # Errors
    /// It cannot be started: not found, not a program, or a file its streams go to cannot be made.
    fn spawn(&self, program: &Program) -> io::Result<Box<dyn Process>>;

    /// Run a program to its end, what it says and its errors piped and kept.
    ///
    /// # Errors
    /// It cannot be started, or what it says cannot be read.
    fn output(&self, program: &Program) -> io::Result<Output> {
        let piped = Program { stdout: Stream::Piped, stderr: Stream::Piped, ..program.clone() };
        let mut process = self.spawn(&piped)?;
        let (stdout, stderr) = (process.take_stdout(), process.take_stderr());
        // Its errors read beside what it says, so that neither pipe fills while the other is read.
        let errors = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut stderr) = stderr {
                let _ = stderr.read_to_end(&mut bytes);
            }
            bytes
        });
        let mut said = Vec::new();
        if let Some(mut stdout) = stdout {
            stdout.read_to_end(&mut said)?;
        }
        let stderr = errors.join().unwrap_or_default();
        Ok(Output { exit: process.wait()?, stdout: said, stderr })
    }
}

/// The system's, by `std::process`.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemProcesses;

struct SystemProcess(std::process::Child);

impl Process for SystemProcess {
    fn id(&self) -> u32 {
        self.0.id()
    }

    fn try_wait(&mut self) -> io::Result<Option<Exit>> {
        Ok(self.0.try_wait()?.map(Exit::from))
    }

    fn wait(&mut self) -> io::Result<Exit> {
        Ok(self.0.wait()?.into())
    }

    fn kill(&mut self) -> io::Result<()> {
        self.0.kill()
    }

    fn take_stdin(&mut self) -> Option<Box<dyn Write + Send>> {
        self.0.stdin.take().map(|stdin| Box::new(stdin) as Box<dyn Write + Send>)
    }

    fn take_stdout(&mut self) -> Option<Box<dyn Read + Send>> {
        self.0.stdout.take().map(|stdout| Box::new(stdout) as Box<dyn Read + Send>)
    }

    fn take_stderr(&mut self) -> Option<Box<dyn Read + Send>> {
        self.0.stderr.take().map(|stderr| Box::new(stderr) as Box<dyn Read + Send>)
    }
}

impl SystemProcesses {
    #[allow(
        clippy::disallowed_methods,
        reason = "the system's programs, and the files they write: what every other starts through"
    )]
    fn command(program: &Program) -> io::Result<std::process::Command> {
        let mut command = std::process::Command::new(&program.program);
        command.args(&program.args).envs(program.env.iter().map(|(name, value)| (name, value)));
        if let Some(folder) = &program.folder {
            command.current_dir(folder);
        }
        // A file both its output and its errors go to is made once.
        let shared = match (&program.stdout, &program.stderr) {
            (Stream::Into(out), Stream::Into(err)) if out == err => Some(std::fs::File::create(out)?),
            _ => None,
        };
        let stdio = |stream: &Stream| -> io::Result<std::process::Stdio> {
            Ok(match stream {
                Stream::Null => std::process::Stdio::null(),
                Stream::Inherit => std::process::Stdio::inherit(),
                Stream::Piped => std::process::Stdio::piped(),
                Stream::Errors => io::stderr().into(),
                Stream::Into(file) => match &shared {
                    Some(shared) => shared.try_clone()?.into(),
                    None => std::fs::File::create(file)?.into(),
                },
            })
        };
        command.stdin(stdio(&program.stdin)?).stdout(stdio(&program.stdout)?).stderr(stdio(&program.stderr)?);
        #[cfg(unix)]
        if program.own_group {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x200;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            // A console of its own, with no window - never none at all (`DETACHED_PROCESS`, beside which Windows
            // ignores the other), or every console program it came to start would be given a terminal window.
            let group = if program.own_group { CREATE_NEW_PROCESS_GROUP } else { 0 };
            command.creation_flags(group | CREATE_NO_WINDOW);
        }
        Ok(command)
    }
}

impl Processes for SystemProcesses {
    fn spawn(&self, program: &Program) -> io::Result<Box<dyn Process>> {
        Ok(Box::new(SystemProcess(Self::command(program)?.spawn()?)))
    }

    fn output(&self, program: &Program) -> io::Result<Output> {
        let piped = Program { stdout: Stream::Piped, stderr: Stream::Piped, ..program.clone() };
        let done = Self::command(&piped)?.output()?;
        Ok(Output { exit: done.status.into(), stdout: done.stdout, stderr: done.stderr })
    }
}

/// What a test answers a program run with.
type Answer = dyn Fn(&Program) -> io::Result<Output> + Send + Sync;

/// Programs a test answers: each run is what `answer` says of it, ended as it is started.
#[derive(Clone)]
pub struct Scripted {
    answer: Arc<Answer>,
}

impl std::fmt::Debug for Scripted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("programs a test answers")
    }
}

impl Scripted {
    #[must_use]
    pub fn new(answer: impl Fn(&Program) -> io::Result<Output> + Send + Sync + 'static) -> Scripted {
        Scripted { answer: Arc::new(answer) }
    }
}

struct Answered {
    exit: Exit,
    stdout: Option<Vec<u8>>,
    stderr: Option<Vec<u8>>,
}

impl Process for Answered {
    fn id(&self) -> u32 {
        0
    }

    fn try_wait(&mut self) -> io::Result<Option<Exit>> {
        Ok(Some(self.exit.clone()))
    }

    fn wait(&mut self) -> io::Result<Exit> {
        Ok(self.exit.clone())
    }

    fn kill(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn take_stdin(&mut self) -> Option<Box<dyn Write + Send>> {
        Some(Box::new(io::sink()))
    }

    fn take_stdout(&mut self) -> Option<Box<dyn Read + Send>> {
        self.stdout.take().map(|bytes| Box::new(io::Cursor::new(bytes)) as Box<dyn Read + Send>)
    }

    fn take_stderr(&mut self) -> Option<Box<dyn Read + Send>> {
        self.stderr.take().map(|bytes| Box::new(io::Cursor::new(bytes)) as Box<dyn Read + Send>)
    }
}

impl Processes for Scripted {
    fn spawn(&self, program: &Program) -> io::Result<Box<dyn Process>> {
        let Output { exit, stdout, stderr } = (self.answer)(program)?;
        let piped = |stream: &Stream, bytes: Vec<u8>| (*stream == Stream::Piped).then_some(bytes);
        let (stdout, stderr) = (piped(&program.stdout, stdout), piped(&program.stderr, stderr));
        Ok(Box::new(Answered { exit, stdout, stderr }))
    }
}

#[cfg(test)]
mod tests {
    use super::{Exit, Output, Processes, Program, Scripted, Stream};
    use std::io::Read;

    #[test]
    fn a_scripted_program_says_what_the_test_answers() {
        let processes = Scripted::new(|program| {
            Ok(Output {
                exit: Exit::code(3),
                stdout: program.args.iter().flat_map(|arg| arg.as_encoded_bytes()).copied().collect(),
                stderr: b"no".to_vec(),
            })
        });
        let run = Program::new("tool").args(["a", "b"]);
        let done = processes.output(&run).unwrap();
        assert_eq!((done.exit.success(), done.exit.exit_code()), (false, Some(3)));
        assert_eq!((done.stdout.as_slice(), done.stderr.as_slice()), (&b"ab"[..], &b"no"[..]));
        // What is not piped is not given.
        let mut started = processes.spawn(&run.streams(Stream::Null, Stream::Piped, Stream::Inherit)).unwrap();
        assert!(started.take_stderr().is_none());
        let mut said = String::new();
        started.take_stdout().unwrap().read_to_string(&mut said).unwrap();
        assert_eq!(said, "ab");
    }
}
