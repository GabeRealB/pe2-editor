//! Models as the editor authors them and as the game stores them: meshes in USD's text form, skeletons, materials, and the
//! encoder between those and the arrays a `TmdSource` record points at; and the clips that move them, both ways. A crate of its own, as what shows a model and what
//! builds one both need it and neither should own it.

pub mod clip;
pub mod clip_usd;
pub mod material;
pub mod mesh;
pub mod model;
pub mod skeleton;
pub mod usda;
