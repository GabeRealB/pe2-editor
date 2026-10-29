//! The project as an extension asks about it: assets by path, by reference, by kind; what the user selected; the look
//! an asset is shown in. The assets are lent, like any: for the frame or call that asked.

use crate::{Asset, Value};

/// What the host answers about the project.
pub trait Project {
    /// The asset at a resolved path.
    fn asset(&self, path: &str) -> Option<Asset<'_>>;
    /// The asset a reference names (`"uid://..."`, or `{ uid, path }`).
    fn resolve(&self, reference: &Value) -> Option<Asset<'_>>;
    /// Every asset of a kind, by its qualified name or a built-in's bare one.
    fn find(&self, kind: &str) -> Vec<Asset<'_>>;
    /// The asset the user selected.
    fn selection(&self) -> Option<Asset<'_>>;
    /// The look an asset is shown in, as the host's look picker chose it (an index into its picture's looks).
    fn look(&self, path: &str) -> usize;
}
