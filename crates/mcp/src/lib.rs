//! The project served to agents over the Model Context Protocol: the resolved project read through the index - the
//! layers' stack applied, references resolved both ways, problems, pictures - instead of an agent rebuilding it from
//! the files.
//!
//! Every request is a value (`Request`: a tool's call, a resource's read) and one function answers it against an
//! index (`answer`). Whoever holds the project answers: `pe2-cli mcp` (`server`) speaks the protocol over stdio and,
//! for each request, hands it to the editor when one is serving the project (`link`) or else takes the project lock
//! for that request alone and answers it itself - so an agent's session never keeps the editor from opening the
//! project, and one process touches it at a time.
//!
//! Read-only so far: writes wait for the write path, where they become operations in the active layer.

pub mod link;
pub mod server;
mod tools;

use rmcp::model::{CallToolResult, ReadResourceResult};
use serde::{Deserialize, Serialize};

pub use tools::definitions;

/// A request to the project.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Request {
    /// A tool called by its name.
    Call { name: String, arguments: serde_json::Map<String, serde_json::Value> },
    /// A resource read by its URI (`uid://...`).
    Read { uri: String },
}

/// Its answer.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reply {
    Call(CallToolResult),
    Read(ReadResourceResult),
    /// The request could not be answered at all (a tool's own failures are results marked as errors).
    Error(String),
}

/// What the editor shows, where the editor answers: its selection, and an asset an agent asked it to show.
#[derive(Default)]
pub struct EditorState {
    pub selection: Option<String>,
    /// The look the selection is shown in, by name.
    pub variant: Option<String>,
    /// Set by a request: the asset to show.
    pub show: Option<String>,
}

/// The project as a request sees it.
pub struct View<'a> {
    pub project: &'a project::Project,
    pub index: &'a project::index::Index,
    /// Present where the editor answers.
    pub editor: Option<EditorState>,
}

/// Answer a request against a project.
#[must_use]
pub fn answer(view: &mut View, request: &Request) -> Reply {
    match request {
        Request::Call { name, arguments } => Reply::Call(tools::call(view, name, arguments)),
        Request::Read { uri } => match tools::read(view, uri) {
            Ok(result) => Reply::Read(result),
            Err(e) => Reply::Error(e),
        },
    }
}
