//! What the program was started with, shaped like `wasi:cli`'s `environment`: its environment's variables, its
//! arguments, and the places it starts from.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;

use crate::HostPath;

/// What the program was started with.
pub trait Environment: Send + Sync {
    /// A variable of its environment, where it is set.
    fn var(&self, name: &str) -> Option<OsString>;
    /// Every variable, as WASI's `get-environment` gives them.
    fn vars(&self) -> Box<[(OsString, OsString)]>;

    /// Its arguments, its own name first.
    fn args(&self) -> Box<[OsString]>;

    /// The folder it was started in.
    ///
    /// # Errors
    /// It is gone, or cannot be named.
    fn current_dir(&self) -> io::Result<HostPath>;

    /// Where temporary files go.
    ///
    /// # Errors
    /// It cannot be named.
    fn temp_dir(&self) -> io::Result<HostPath>;

    /// The program itself.
    ///
    /// # Errors
    /// It cannot be named.
    fn current_exe(&self) -> io::Result<HostPath>;
}

/// The program's own, as the system gives them.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemEnvironment;

#[allow(clippy::disallowed_methods, reason = "the system's environment: what every other reads through")]
impl Environment for SystemEnvironment {
    fn var(&self, name: &str) -> Option<OsString> {
        std::env::var_os(name)
    }

    fn vars(&self) -> Box<[(OsString, OsString)]> {
        std::env::vars_os().collect()
    }

    fn args(&self) -> Box<[OsString]> {
        std::env::args_os().collect()
    }

    fn current_dir(&self) -> io::Result<HostPath> {
        HostPath::new(&std::env::current_dir()?)
    }

    fn temp_dir(&self) -> io::Result<HostPath> {
        HostPath::new(&std::env::temp_dir())
    }

    fn current_exe(&self) -> io::Result<HostPath> {
        HostPath::new(&std::env::current_exe()?)
    }
}

/// An environment a test sets.
#[derive(Clone, Debug)]
pub struct Fixed {
    pub vars: BTreeMap<String, OsString>,
    pub args: Box<[OsString]>,
    pub current_dir: HostPath,
    pub temp_dir: HostPath,
    pub current_exe: HostPath,
}

impl Environment for Fixed {
    fn var(&self, name: &str) -> Option<OsString> {
        self.vars.get(name).cloned()
    }

    fn vars(&self) -> Box<[(OsString, OsString)]> {
        self.vars.iter().map(|(name, value)| (OsString::from(name), value.clone())).collect()
    }

    fn args(&self) -> Box<[OsString]> {
        self.args.clone()
    }

    fn current_dir(&self) -> io::Result<HostPath> {
        Ok(self.current_dir.clone())
    }

    fn temp_dir(&self) -> io::Result<HostPath> {
        Ok(self.temp_dir.clone())
    }

    fn current_exe(&self) -> io::Result<HostPath> {
        Ok(self.current_exe.clone())
    }
}
