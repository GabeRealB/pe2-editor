//! The extension SDK: what an extension of the editor is written against.
//!
//! An extension has an id (`pe2.images`), which namespaces everything it registers: its kinds and roles are named
//! `<id>@<name>` (`pe2.images@image`), so no two extensions' names collide and every name says who owns it. It
//! registers procedurally, from one entry point (`extension!`): a kind and what it attaches to it - its fields as the
//! inspector shows them, its picture, its thumbnail, its views - and roles on its own kinds or on its dependencies'.
//!
//! The extensions are linked into the editor for now, and the host (`project::ext`) calls what they register
//! directly. This API is the one a WebAssembly component's bindings will implement: what crosses is what WIT can say -
//! assets lent for a call, images, a `Ui` handle a view draws through, never the host's own types.

mod asset;
mod image;
mod registry;
mod ui;

pub use anyhow::{Error, Result, anyhow, bail};
pub use asset::{Asset, Source};
pub use image::{Image, Rect, Texture};
pub use registry::{
    Extension, Field, Format, Icon, KindBuilder, KindDef, MakeView, Name, Picture, Registry, RoleBuilder, RoleDef, Thumbnail,
    Tint, ViewDef,
};
pub use toml::{Table, Value};
pub use ui::{Canvas, Style, Ui, UiHost, View};
