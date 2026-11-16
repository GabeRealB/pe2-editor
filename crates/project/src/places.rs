//! Where a project keeps what other programs reach of it, by its own rules: the folder of this machine's state, the
//! lock and the file naming who holds it, the channel its host listens at, and each layer's folder and plan. What
//! works with a project from outside the editor asks here (`pe2-cli project paths`) rather than building the paths,
//! so that a change of where the project keeps them is made in one place.

use pe2_io::ChannelName;
use serde::Serialize;
use sfs::HostPath;

/// Where a project keeps what other programs reach of it.
#[derive(Debug, Serialize)]
pub struct Places {
    /// The project's folder.
    pub root: String,
    /// The folder of this machine's state of it.
    pub local: String,
    /// The lock whoever holds the project holds, and the file naming who does.
    pub lock: String,
    pub owner: String,
    /// Where its host listens: a socket file, or a pipe's name.
    pub host: Channel,
    /// Each layer, in the stack's order, bottom to top.
    pub layers: Vec<LayerPlaces>,
}

/// A channel's name, as a program connecting to it gives it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Unix(String),
    Pipe(String),
}

/// Where a layer keeps itself.
#[derive(Debug, Serialize)]
pub struct LayerPlaces {
    pub name: String,
    /// Its folder.
    pub path: String,
    /// Its plan, where a plan fills it (whether or not it is written yet).
    pub plan: Option<String>,
    /// Whether it is kept as records, written and read through the editor's commands alone.
    pub records: bool,
}

/// Where the project keeps what other programs reach of it.
///
/// # Errors
/// The name of the channel its host listens at cannot be made.
pub fn places(project: &crate::Project) -> std::io::Result<Places> {
    let text = |path: &HostPath| path.to_string();
    let (lock, owner) = crate::lock::paths(&project.root);
    let host = match crate::host::ROLE.name(&project.root)? {
        ChannelName::File(path) => Channel::Unix(text(&path)),
        ChannelName::Named(name) => Channel::Pipe(name),
    };
    let fs = project.io.filesystem();
    let layers = project
        .layers
        .iter()
        .map(|layer| LayerPlaces {
            name: layer.name.to_string(),
            path: text(&layer.path),
            plan: (layer.kind == crate::LayerKind::Generated).then(|| text(&layer.plan())),
            records: crate::records::is_records(fs, &layer.path),
        })
        .collect();
    Ok(Places {
        root: text(&project.root),
        local: text(&project.root.join(&crate::cache::DIR)),
        lock: text(&lock),
        owner: text(&owner),
        host,
        layers,
    })
}
