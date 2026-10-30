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
//! The editor's window is served too, where the editor answers: its widgets read from its accessibility tree, and -
//! where the user lets agents - clicks, keys and text sent to it as the user's own would be, its edits the editor's
//! undoable ones. The rest is read-only: writes wait for the write path, where they become operations in the active
//! layer.

pub mod link;
pub mod server;
mod tools;

use rmcp::model::{CallToolResult, ReadResourceResult};
use serde::{Deserialize, Serialize};

pub use tools::{ActArgs, EDITOR_UI, RunArgs, ScreenshotArgs, TypeArgs, UiArgs, definitions};

/// A request to the project.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Request {
    /// A tool called by its name.
    Call { name: String, arguments: serde_json::Map<String, serde_json::Value> },
    /// A resource read by its URI (`uid://...`).
    Read { uri: String },
    /// Events signaled, one epoch each: answered once they have ended, only by an editor.
    Signal { events: Vec<String>, config: Option<String> },
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

impl Reply {
    /// What a tool said, or why it failed, as text.
    ///
    /// # Errors
    /// It failed, or it was no tool's answer.
    pub fn told(&self) -> Result<String, String> {
        match self {
            Reply::Call(result) => {
                let texts = result.content.iter().filter_map(|block| block.as_text().map(|text| text.text.clone()));
                let said = texts.collect::<Vec<_>>().join("\n");
                if result.is_error == Some(true) { Err(said) } else { Ok(said) }
            }
            Reply::Read(_) => Err("it answered with something else".to_string()),
            Reply::Error(why) => Err(why.clone()),
        }
    }

    /// A tool's answer as text.
    #[must_use]
    pub fn text(text: String) -> Reply {
        Reply::Call(CallToolResult::success(vec![rmcp::model::ContentBlock::text(text)]))
    }

    /// A tool's failure.
    #[must_use]
    pub fn failed(message: String) -> Reply {
        Reply::Call(CallToolResult::error(vec![rmcp::model::ContentBlock::text(message)]))
    }

    /// A tool's answer as a picture with a caption.
    #[must_use]
    pub fn picture(caption: String, png: &[u8]) -> Reply {
        use base64::Engine;
        let png = base64::engine::general_purpose::STANDARD.encode(png);
        let content = vec![rmcp::model::ContentBlock::text(caption), rmcp::model::ContentBlock::image(png, "image/png")];
        Reply::Call(CallToolResult::success(content))
    }
}

impl Request {
    /// A call of one of the tools that use the editor's window, which the editor answers across frames.
    #[must_use]
    pub fn uses_window(&self) -> bool {
        matches!(self, Request::Call { name, .. } if EDITOR_UI.contains(&name.as_str()))
    }
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
    /// What the extensions say of the assets: their pictures, their looks.
    pub extensions: &'a project::ext::Extensions,
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
        Request::Signal { .. } => Reply::Error("events are signaled to an editor".to_string()),
    }
}
