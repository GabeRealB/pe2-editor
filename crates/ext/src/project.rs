//! The project as an extension asks about it: assets by path, by reference, by kind; what the user selected; the
//! members of an asset's families it is being looked at in. The assets are lent, like any: for the frame or call that asked.
//!
//! It is also how extensions use one another, through the host: what the host understands of any asset - its picture,
//! its outputs, its facts - is answered by the extension its kind is, whichever that is; and anything else by a service
//! an extension offers (`Registry::service`), called by name. Each runs in the other extension's own instance, its
//! memory and failures its own.

use crate::{Asset, Image, Result, Value};

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
    /// Every asset of a kind, by its qualified name or a built-in's bare one.
    fn find(&self, kind: &str) -> Vec<Asset<'_>>;
    /// The asset the user selected.
    fn selection(&self) -> Option<Asset<'_>>;
    /// An asset's picture in a look (its first where none is named), drawn by the extension its kind is; none where it
    /// has none.
    fn picture(&self, path: &str, look: Option<&str>) -> Option<Result<Image>>;
    /// An output of an asset that is a picture (`palettes/default`): none where it has no such output.
    fn image(&self, path: &str, address: &str) -> Option<Result<Image>>;
    /// An output of an asset that is a table: none where it has no such output, or why it could not be made.
    fn table(&self, path: &str, address: &str) -> Option<Result<crate::Table>> {
        let _ = (path, address);
        None
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
