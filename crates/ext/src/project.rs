//! The project as an extension asks about it: assets by path, by reference, by type; what the user selected; the
//! members of an asset's families it is being looked at in. The assets are lent, like any: for the frame or call that asked.
//!
//! It is also how extensions use one another, through the host: what the host understands of any asset - its picture,
//! its outputs, its facts - is answered by the extension its type is, whichever that is; and anything else by a service
//! an extension offers (`Registry::service`), called by name. Each runs in the other extension's own instance, its
//! memory and failures its own.

use crate::{Asset, Image, Result, Value};

/// An output asked for without waiting (`Project::request`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Requested {
    Ready(Vec<u8>),
    /// On its way, for so many milliseconds.
    Pending { waited: u32 },
    /// Why it cannot be.
    Failed(String),
}

/// What the host answers about the project.
pub trait Project {
    /// The asset at a resolved path.
    fn asset(&self, path: &str) -> Option<Asset<'_>>;
    /// The asset a reference names (`"uid://..."`, or `{ uid, path }`).
    fn resolve(&self, reference: &Value) -> Option<Asset<'_>>;
    /// The member of a family of an asset's outputs it is being looked at in (`picture`: its look), where one was chosen.
    fn chosen(&self, path: &str, family: &str) -> Option<String> {
        let _ = (path, family);
        None
    }
    /// Every asset of a type, by its qualified name or a built-in's bare one.
    fn find(&self, node_type: &str) -> Vec<Asset<'_>>;
    /// The asset the user selected.
    fn selection(&self) -> Option<Asset<'_>>;
    /// An asset's picture in a look (its first where none is named), drawn by the extension its type is; none where it
    /// has none.
    fn picture(&self, path: &str, look: Option<&str>) -> Option<Result<Image>>;
    /// An output of an asset that is a picture (`palettes/default`): none where it has no such output.
    fn image(&self, path: &str, address: &str) -> Option<Result<Image>>;
    /// The target of the config in use.
    fn target(&self) -> Option<String> {
        None
    }
    /// What changes each time changes are taken in: what is read of plain files may be out of date once it has.
    fn revision(&self) -> u64 {
        0
    }
    /// An output of an asset that is a table: none where it has no such output, or why it could not be made.
    fn table(&self, path: &str, address: &str) -> Option<Result<crate::Table>> {
        let _ = (path, address);
        None
    }
    /// An output of an asset that is one file, whole, made from its settings as they are written: what a viewer shows
    /// of what is being edited. None where it has no such output, or why it could not be made.
    fn bytes(&self, path: &str, address: &str) -> Option<Result<Vec<u8>>> {
        let _ = (path, address);
        None
    }
    /// What `bytes` gives, without waiting for it: for a frame, as how long an output takes is what it reads. What is on its
    /// way is shown again when it is there. Where nothing can make it on the side, it is made here.
    fn request(&self, path: &str, address: &str) -> Requested {
        match self.bytes(path, address) {
            Some(Ok(bytes)) => Requested::Ready(bytes),
            Some(Err(why)) => Requested::Failed(format!("{why:#}")),
            None => Requested::Failed(format!("{path} has no {address}")),
        }
    }
    /// A service an extension offers (`pe2.images@palette`): its answer to these arguments. A component calls only
    /// the services of the extensions it requires (or its own).
    ///
    /// # Errors
    /// No such service, it is not the caller's to call, or it failed.
    fn call(&self, service: &str, arguments: &Value) -> Result<Value>;
    /// Whether the extension `caller` may call `service`: its own, or one of an extension it requires. The host's
    /// check of a component's call; an extension asks nothing of it.
    fn may_call(&self, caller: &str, service: &str) -> bool {
        let _ = (caller, service);
        true
    }
}
