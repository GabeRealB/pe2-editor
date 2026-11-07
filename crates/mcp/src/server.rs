//! `pe2-cli mcp`: the protocol over stdio. Each request goes to the editor when one serves the project, and is
//! otherwise answered here under the project lock, taken for that request alone.

use project::Project;
use project::cache::Cache;
use project::index::Index;
use project::lock::{Lock, LockError};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, Implementation, InitializeResult, ListResourceTemplatesResult,
    ListToolsResult, PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse, ResourceTemplate,
    ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::{Reply, Request, View, answer, link};

/// How long a request waits for an editor opening the project to listen.
const EDITOR_STARTING: Duration = Duration::from_secs(10);

/// How long a request waits for another brief holder of the project (another agent's request) to let go.
const PATIENCE: Duration = Duration::from_secs(10);

const INSTRUCTIONS: &str = "A Parasite Eve II project: its assets are bundles in layers stacked bottom to top (the game's \
nodes at the bottom, read-only, their files in a generated layer its plan fills from the discs), resolved into one tree by path, each asset with a uid \
(uid://...) that references name it by. Find assets with find_assets or list_folder, read one with get_asset (its \
declaration is its node.toml), follow references both ways with references, see images with preview. The project \
is read-only here. Where the editor has the project open, its window is served too: editor_ui reads its widgets (ids \
to act on), editor_screenshot shows it, and - where the user lets agents use the editor - editor_act clicks, drags \
and scrolls, editor_type types and presses keys, editor_commands lists and editor_run runs its commands, the edits these make being the editor's own, undoable in its \
history. editors lists the editors running on this computer and what each is doing, who holds the project now, and \
whether this server's program was rebuilt since it started (then it runs older code, and should be restarted).";

/// Answer a request for the project in `root`: through the editor when it serves the project, else here.
#[must_use]
pub fn execute(root: &Path, request: &Request) -> Reply {
    // The editors and who holds the project: answered here, never through an editor nor under the project's lock.
    if matches!(request, Request::Call { name, .. } if name == "editors") {
        let text = crate::presence::report(root).to_string();
        return Reply::Call(rmcp::model::CallToolResult::success(vec![rmcp::model::ContentBlock::text(text)]));
    }
    if let Some(reply) = link::forward(root, request) {
        return reply;
    }
    // An editor opening the project, or holding it with its link not up yet: its, as soon as it listens - never taken
    // from under it.
    if let Some(pid) = crate::presence::claimed_by(root) {
        let started = std::time::Instant::now();
        while started.elapsed() < EDITOR_STARTING {
            std::thread::sleep(std::time::Duration::from_millis(200));
            if let Some(reply) = link::forward(root, request) {
                return reply;
            }
        }
        return Reply::Error(format!(
            "the editor (process {pid}) is opening the project: ask again once it is open - `editors` says how far it is"
        ));
    }
    // The window is the editor's alone: asked of none, it is no reason to take the project.
    if request.uses_window() {
        return Reply::Error("no editor has the project open".to_string());
    }
    let lock = match Lock::acquire_within(root, "pe2-cli mcp", PATIENCE) {
        Ok(lock) if lock.unprotected().is_some() => {
            static WARNED: std::sync::Once = std::sync::Once::new();
            WARNED.call_once(|| eprintln!("warning: {}", lock.unprotected().unwrap_or_default()));
            lock
        }
        Ok(lock) => lock,
        Err(LockError::Held(owner)) => {
            return Reply::Error(format!("the project is in use by {owner}; ask again once it is done"));
        }
        Err(e) => return Reply::Error(e.to_string()),
    };
    let opened = Project::load(root).and_then(|project| {
        let cache = Cache::open(&project, &lock)?;
        let mut index = Index::open(&project, &cache)?;
        // The extensions as the project has them: those it holds and the built-ins, without those it leaves off.
        let extensions = extensions::for_project(&project);
        let _ = project::imports::Imports::open(&cache)?.attach(&mut index, &extensions);
        let store = project::graph::store::Store::open(&cache)?;
        extensions.keep_in(store.clone());
        Ok((project, index, extensions, store))
    });
    match opened {
        Ok((project, index, extensions, store)) => {
            let reply =
                answer(&mut View { project: &project, index: &index, extensions: &extensions, editor: None }, request);
            // The server has no frames, and waits for the next request: its extensions' readings give their memory back.
            extensions.release();
            // What it kept answering, swept where a sweep is due - while it holds the project.
            let _ = project::imports::sweep_if_due(&store, &index);
            reply
        }
        Err(e) => Reply::Error(format!("the project cannot be opened: {e:#}")),
    }
}

/// The protocol's side of the server.
#[derive(Clone)]
pub struct Server {
    root: PathBuf,
    tools: Vec<Tool>,
}

impl Server {
    #[must_use]
    pub fn new(root: PathBuf) -> Server {
        Server { root, tools: crate::definitions() }
    }

    async fn execute(&self, request: Request) -> Result<Reply, ErrorData> {
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || execute(&root, &request))
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))
    }
}

impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().enable_resources().build())
            .with_server_info(Implementation::new("pe2", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + Send + '_ {
        std::future::ready(Ok(ListToolsResult::with_all_items(self.tools.clone())))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tools.iter().find(|t| t.name == name).cloned()
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let call = Request::Call { name: request.name.to_string(), arguments: request.arguments.unwrap_or_default() };
        match self.execute(call).await? {
            Reply::Call(result) => Ok(result.into()),
            Reply::Read(_) => Err(ErrorData::internal_error("a read answered a call", None)),
            Reply::Error(e) => Err(ErrorData::internal_error(e, None)),
        }
    }

    fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourceTemplatesResult, ErrorData>> + Send + '_ {
        let mut template = ResourceTemplate::new("uid://{uid}", "asset");
        template.description = Some("An asset's declaration (its node.toml), by its uid.".into());
        template.mime_type = Some("application/toml".into());
        std::future::ready(Ok(ListResourceTemplatesResult::with_all_items(vec![template])))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        match self.execute(Request::Read { uri: request.uri }).await? {
            Reply::Read(result) => Ok(result.into()),
            Reply::Call(_) => Err(ErrorData::internal_error("a call answered a read", None)),
            Reply::Error(e) => Err(ErrorData::resource_not_found(e, None)),
        }
    }
}

/// Serve the project in `root` over stdio until the client goes.
///
/// # Errors
/// The runtime cannot start or the connection fails.
pub fn serve_stdio(root: PathBuf) -> anyhow::Result<()> {
    crate::presence::note_start();
    // Made before the runtime: listing the extensions' tools loads them, and a component's call blocks its thread.
    let server = Server::new(root);
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(async move {
        let service = server.serve(rmcp::transport::stdio()).await?;
        service.waiting().await?;
        Ok(())
    })
}
