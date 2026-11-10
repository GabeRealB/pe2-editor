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
pub mod presence;
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
    /// An edit of the project (the command line's, a program's through the served folder), made in a layer - by default
    /// the one edits go to - by whoever holds the project, as a step of its history: answered with the step's label.
    /// Never an agent's: no tool makes one.
    Edit { op: vfs::ops::Op, layer: Option<String> },
    /// Several edits made together, each a step of its own: what programs wrote at once through the served folder.
    /// Answered with each one's label, or why it was not made.
    Edits { ops: Vec<vfs::ops::Op>, layer: Option<String> },
    /// Who answers, and how it runs (`Holder`): what whoever asks compares with itself before relying on it.
    Hello,
    /// The tools of the project's extensions, as its holder has them loaded: those it holds among them, which a
    /// program that only asks does not have.
    Tools,
}

/// Who holds a project and answers what is asked of it: an editor, or a host with no window; the program it runs,
/// as it was built when it started; and what it was started under - the user's data folder, and whether it trusts
/// every extension. A host that differs in these from whoever asks would answer with other code, other settings or
/// other trust than its own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holder {
    pub editor: bool,
    pub pid: u32,
    pub program: String,
    pub built: Option<u64>,
    pub data: Option<std::path::PathBuf>,
    pub trusts_everything: bool,
}

impl Holder {
    /// This process, as a holder: an editor or not.
    #[must_use]
    pub fn this(editor: bool) -> Holder {
        let (program, built) = presence::started();
        Holder {
            editor,
            pid: std::process::id(),
            program,
            built,
            data: extensions::data_folder(),
            trusts_everything: extensions::trust::trusts_everything(),
        }
    }

    /// Why a host is not what this process would have started, where it is not: another program or build of it,
    /// another data folder, another trust.
    #[must_use]
    pub fn differs(&self) -> Option<String> {
        let mine = Holder::this(false);
        let (my_program, now) = presence::this_program();
        if !presence::same(std::path::Path::new(&self.program), std::path::Path::new(&my_program)) {
            return Some(format!("it runs {}, not {my_program}", self.program));
        }
        if self.built != now {
            return Some("its program was built again since it started".into());
        }
        if self.data != mine.data {
            return Some("it was started with another data folder".into());
        }
        (self.trusts_everything != mine.trusts_everything).then(|| "it trusts extensions otherwise".into())
    }
}

/// Its answer.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reply {
    Call(CallToolResult),
    Read(ReadResourceResult),
    /// The request could not be answered at all (a tool's own failures are results marked as errors).
    Error(String),
    /// An edit made: the step's label, and the paths it touched, for a process serving the layers to take in.
    Edited {
        label: String,
        touched: Vec<std::path::PathBuf>,
    },
    /// Not answered by this holder (the host answers edits alone): whoever asked takes the project for it.
    Declined,
    /// Edits made together: each one's label, or why it was not made, and every path they touched.
    EditedEach {
        made: Vec<Result<String, String>>,
        touched: Vec<std::path::PathBuf>,
    },
    Hello(Holder),
    Tools(Vec<rmcp::model::Tool>),
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
            Reply::Edited { label, .. } => Ok(label.clone()),
            Reply::EditedEach { made, .. } => {
                let labels: Result<Vec<String>, String> = made.iter().cloned().collect();
                labels.map(|labels| labels.join("\n"))
            }
            Reply::Read(_) | Reply::Declined | Reply::Hello(_) | Reply::Tools(_) => {
                Err("it answered with something else".to_string())
            }
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
        let content =
            vec![rmcp::model::ContentBlock::text(caption), rmcp::model::ContentBlock::image(png, "image/png")];
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
    /// What the editor is costing now.
    pub performance: serde_json::Value,
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
        Request::Edit { .. } | Request::Edits { .. } => {
            Reply::Error("an edit is made by whoever holds the project, with its history".into())
        }
        Request::Hello => Reply::Hello(Holder::this(view.editor.is_some())),
        Request::Tools => Reply::Tools(tools::of(view.extensions)),
    }
}
