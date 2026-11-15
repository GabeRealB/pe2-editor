//! Starting programs, for an unsandboxed extension (`project::ext::unsandboxed`) alone: the host refuses any other, and
//! WASI has nothing for it. What uses one calls its operations, and never sees them.

use crate::Buffer;
#[cfg(target_family = "wasm")]
use crate::guest::bindings::pe2::extension::unsandboxed as host;
use crate::{Result, anyhow};

/// A program running: what is written goes to its standard input, what it writes to its standard output is read.
pub struct Process {
    #[cfg(target_family = "wasm")]
    process: host::Process,
}

/// Start a program: where the extension ships one (for this platform, in `<platform>/` of its folder, or any), else as
/// the system finds it.
///
/// # Errors
/// This extension is not unsandboxed, or it could not be started.
pub fn spawn(tool: &str, args: &[crate::Arg]) -> Result<Process> {
    #[cfg(target_family = "wasm")]
    {
        let args = args.iter().map(told).collect::<Result<Vec<_>>>()?;
        return host::spawn(tool, &args).map(|process| Process { process }).map_err(|e| anyhow!(e));
    }
    #[cfg(not(target_family = "wasm"))]
    {
        let _ = args;
        Err(anyhow!("{tool}: only an unsandboxed extension's component launches programs"))
    }
}

/// An argument as the host is told it: a place, by the handle the host gave for it.
///
/// # Errors
/// A place is none the host gave this component.
#[cfg(target_family = "wasm")]
fn told(arg: &crate::Arg) -> Result<host::Arg<'_>> {
    use crate::{Arg, ArgPart};
    fn handle(path: &crate::HostPath) -> Result<&host::HostPath> {
        crate::guest::handle(path).ok_or_else(|| anyhow!("{path}: no place the host gave"))
    }
    Ok(match arg {
        Arg::Text(text) => host::Arg::Text(text.clone()),
        Arg::Path(path) => host::Arg::Path(handle(path)?),
        Arg::Joined(parts) => host::Arg::Joined(
            parts
                .iter()
                .map(|part| match part {
                    ArgPart::Text(text) => Ok(host::ArgPart::Text(text.clone())),
                    ArgPart::Path(path) => handle(path).map(host::ArgPart::Path),
                })
                .collect::<Result<_>>()?,
        ),
    })
}

/// A place for a program to write a file to, in the system's folder of temporary files: for what is no operation's
/// run (`Call::temp` is an operation's). The extension's to remove (`HostPath::guest`).
///
/// # Errors
/// This extension is not unsandboxed, or the host has no such place.
pub fn temp() -> Result<crate::HostPath> {
    #[cfg(target_family = "wasm")]
    return host::temp().map(crate::guest::given).map_err(|e| anyhow!(e));
    #[cfg(not(target_family = "wasm"))]
    Err(anyhow!("only an unsandboxed extension's component is given temporary files"))
}

/// Have what this extension's setup names (`name` of a `[[setup]]` entry of its manifest) on this machine: what is
/// optional (`lazy`) is fetched when asked for, as the user lets it. What uses it fails where it is refused.
///
/// # Errors
/// This extension is not unsandboxed, declares nothing of that name for this platform, the user turned it off on this
/// computer, or it could not be fetched.
pub fn set_up(name: &str) -> Result<()> {
    #[cfg(target_family = "wasm")]
    return host::set_up(name).map_err(|e| anyhow!(e));
    #[cfg(not(target_family = "wasm"))]
    Err(anyhow!("{name}: only an unsandboxed extension's component fetches"))
}

impl Process {
    /// Write to its input.
    ///
    /// # Errors
    /// It stopped reading.
    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
        #[cfg(target_family = "wasm")]
        return self.process.write(bytes).map_err(|e| anyhow!(e));
        #[cfg(not(target_family = "wasm"))]
        {
            let _ = bytes;
            Err(anyhow!("no process"))
        }
    }

    /// Write a buffer's bytes to its input, straight from the host.
    ///
    /// # Errors
    /// It stopped reading.
    pub fn write_buffer(&mut self, buffer: &Buffer) -> Result<()> {
        #[cfg(target_family = "wasm")]
        return self.process.write_buffer(buffer.handle()).map_err(|e| anyhow!(e));
        #[cfg(not(target_family = "wasm"))]
        {
            let _ = buffer;
            Err(anyhow!("no process"))
        }
    }

    /// End its input.
    pub fn close_input(&mut self) {
        #[cfg(target_family = "wasm")]
        self.process.close_input();
    }

    /// Up to `len` bytes of its output: none at its end.
    ///
    /// # Errors
    /// It could not be read.
    pub fn read(&mut self, len: u32) -> Result<Vec<u8>> {
        #[cfg(target_family = "wasm")]
        return self.process.read(len).map_err(|e| anyhow!(e));
        #[cfg(not(target_family = "wasm"))]
        {
            let _ = len;
            Err(anyhow!("no process"))
        }
    }

    /// Wait for it to end.
    ///
    /// # Errors
    /// It failed: what it said on its standard error is why.
    pub fn wait(self) -> Result<()> {
        #[cfg(target_family = "wasm")]
        return self.process.wait().map_err(|e| anyhow!(e));
        #[cfg(not(target_family = "wasm"))]
        Err(anyhow!("no process"))
    }
}
