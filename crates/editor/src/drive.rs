//! Agents using the editor's window: the tools `editor_ui`, `editor_act`, `editor_type` and `editor_screenshot`. Its
//! widgets are read from egui's accessibility tree (AccessKit), built while agents ask for it; input goes in as the
//! user's own would - an AccessKit action where the widget takes one, pointer and key events otherwise - a batch a
//! frame, through a plugin's input hook; and each request is answered once the editor has drawn its result. Reading
//! is always allowed; acting only where the user lets agents (Settings, Agents; `--agents-drive`). The main window
//! only: other windows have input and trees of their own.

use eframe::egui;
use egui::accesskit::{self, Action, NodeId, Role, Toggled};
use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

/// A request and where its answer goes.
pub type Request = (mcp::Request, mpsc::Sender<mcp::Reply>);

/// How long a request may wait for its frames: the link gives up after 20 s.
const PATIENCE: Duration = Duration::from_secs(15);
/// How long the tree is built after the last request: building it costs every frame.
const KEEP_TREE: Duration = Duration::from_secs(120);
/// Frames drawn after the last of an action's input before its result is read: the frame handling it, and the one
/// showing what the edits it made changed.
const SETTLE: u64 = 2;
/// The most lines an answer gives.
const MOST_LINES: usize = 600;
/// The most changes an action's answer lists.
const MOST_CHANGES: usize = 80;

fn acted_id() -> egui::Id {
    egui::Id::unique("agent acted")
}

/// How long after an agent's action a click may be its own: its press and release come over a few frames.
const ACTING: Duration = Duration::from_secs(2);

/// Whether what just happened in the window may be an agent's: what only a person may answer refuses it.
pub fn agent_acting(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<Instant>(acted_id())).is_some_and(|at| at.elapsed() < ACTING)
}

/// What the plugin and the driver share.
#[derive(Default)]
struct Shared {
    /// Input to send, a batch a frame.
    batches: VecDeque<Vec<egui::Event>>,
    /// The main window's widgets as of its last frame, and that frame.
    tree: Option<Tree>,
    tree_frame: u64,
    /// The main window's frames drawn.
    frames: u64,
}

#[derive(Clone)]
struct Tree {
    nodes: HashMap<NodeId, accesskit::Node>,
    root: NodeId,
    focus: NodeId,
}

fn lock(shared: &Mutex<Shared>) -> std::sync::MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The plugin: the input hook sends the queued input, the output hook keeps the tree.
struct Plugin(Arc<Mutex<Shared>>);

impl egui::Plugin for Plugin {
    fn debug_name(&self) -> &'static str {
        "pe2 agents"
    }

    fn input_hook(&mut self, _ctx: &egui::Context, input: &mut egui::RawInput) {
        if input.viewport_id != egui::ViewportId::ROOT {
            return;
        }
        if let Some(batch) = lock(&self.0).batches.pop_front() {
            input.events.extend(batch);
        }
    }

    fn output_hook(&mut self, ctx: &egui::Context, output: &mut egui::FullOutput) {
        if ctx.viewport_id() != egui::ViewportId::ROOT {
            return;
        }
        let mut shared = lock(&self.0);
        shared.frames += 1;
        if let Some(update) = &output.platform_output.accesskit_update {
            let root = update.tree.as_ref().map_or(update.focus, |t| t.root);
            let nodes = update.nodes.iter().cloned().collect();
            shared.tree = Some(Tree { nodes, root, focus: update.focus });
            shared.tree_frame = shared.frames;
        }
    }
}

/// A pane's tab title and where it was drawn last frame, noted while drawing it.
#[derive(Clone, Default)]
struct PaneRects(Vec<(String, egui::Rect)>);

fn panes_id() -> egui::Id {
    egui::Id::unique("pe2 agents: panes")
}

/// Note a pane of the main window as drawn, for agents to name it.
pub fn note_pane(ui: &egui::Ui, title: String) {
    if ui.ctx().viewport_id() != egui::ViewportId::ROOT {
        return;
    }
    let rect = ui.max_rect();
    ui.ctx().data_mut(|d| d.get_temp_mut_or_default::<PaneRects>(panes_id()).0.push((title, rect)));
}

/// A request in progress.
enum Job {
    /// Waiting for a tree drawn after it arrived.
    Waiting { name: String, arguments: serde_json::Map<String, serde_json::Value>, after: u64 },
    /// Its input queued: answered after frame `until`, with what changed since `before`.
    Settling { before: Vec<Line>, until: u64 },
    /// A screenshot asked for: answered when it arrives, cut to `rect` (in points).
    Capturing { image: Arc<Mutex<Option<Arc<egui::ColorImage>>>>, rect: Option<(String, egui::Rect)>, scale: f32 },
}

struct Pending {
    job: Job,
    reply: mpsc::Sender<mcp::Reply>,
    since: Instant,
}

/// The editor's side of the tools.
pub struct Driver {
    shared: Arc<Mutex<Shared>>,
    pending: Vec<Pending>,
    /// The main window's panes as drawn last frame.
    panes: Vec<(String, egui::Rect)>,
    installed: bool,
    /// What agents asked of the commands, which the editor answers.
    pub commands: Vec<(String, serde_json::Map<String, serde_json::Value>, mpsc::Sender<mcp::Reply>)>,
    /// When an agent last asked: the tree is built until a while after.
    asked: Option<Instant>,
    /// When an agent last acted: the status bar says so for a while.
    pub acted: Option<Instant>,
    /// The project is asking something only a person may answer.
    pub asking: bool,
}

impl Driver {
    pub fn new() -> Driver {
        Driver {
            shared: Arc::default(),
            pending: Vec::new(),
            panes: Vec::new(),
            installed: false,
            commands: Vec::new(),
            asked: None,
            acted: None,
            asking: false,
        }
    }

    /// Take requests that arrived.
    /// Why an agent may not act.
    pub fn refusal(&self) -> &'static str {
        if self.asking {
            "the project is asking its user something, and only they can answer: wait for them"
        } else {
            "the user has not let agents use the editor (its Settings, Agents, or --agents-drive)"
        }
    }

    pub fn take(&mut self, ctx: &egui::Context, requests: Vec<Request>) {
        if requests.is_empty() {
            return;
        }
        if !self.installed {
            ctx.add_plugin(Plugin(self.shared.clone()));
            self.installed = true;
        }
        ctx.enable_accesskit();
        self.asked = Some(Instant::now());
        let after = lock(&self.shared).frames;
        for (request, reply) in requests {
            let mcp::Request::Call { name, arguments } = request else { continue };
            if name == "editor_commands" || name == "editor_run" {
                self.commands.push((name, arguments, reply));
                continue;
            }
            self.pending.push(Pending { job: Job::Waiting { name, arguments, after }, reply, since: Instant::now() });
        }
    }

    /// Advance the requests in progress, at the start of a frame; `drive`: whether agents may act.
    pub fn update(&mut self, ctx: &egui::Context, drive: bool) {
        self.panes = ctx.data_mut(|d| std::mem::take(&mut d.get_temp_mut_or_default::<PaneRects>(panes_id()).0));
        if self.asked.is_some_and(|t| t.elapsed() > KEEP_TREE) && self.pending.is_empty() {
            ctx.disable_accesskit();
            lock(&self.shared).tree = None;
            self.asked = None;
        }
        if let Some(acted) = self.acted {
            ctx.data_mut(|d| d.insert_temp(acted_id(), acted));
        }
        let pending = std::mem::take(&mut self.pending);
        for mut pending in pending {
            if pending.since.elapsed() > PATIENCE {
                let _ = pending.reply.send(mcp::Reply::failed("the editor did not draw in time".into()));
                continue;
            }
            match self.advance(ctx, pending.job, drive) {
                Ok(Some(job)) => {
                    pending.job = job;
                    self.pending.push(pending);
                }
                Ok(None) => {}
                Err(reply) => {
                    let _ = pending.reply.send(reply);
                }
            }
        }
        if !self.pending.is_empty() {
            ctx.request_repaint();
        }
    }

    /// A job's next state, or its answer (`Err`, being final).
    fn advance(&mut self, ctx: &egui::Context, job: Job, drive: bool) -> Result<Option<Job>, mcp::Reply> {
        match job {
            Job::Waiting { name, arguments, after } => {
                let shared = lock(&self.shared);
                let Some(tree) = shared.tree.clone().filter(|_| shared.tree_frame > after) else {
                    return Ok(Some(Job::Waiting { name, arguments, after }));
                };
                drop(shared);
                self.start(ctx, &tree, &name, arguments, drive).map(Some)
            }
            Job::Settling { before, until } => {
                let shared = lock(&self.shared);
                if shared.frames <= until || shared.tree_frame <= until {
                    return Ok(Some(Job::Settling { before, until }));
                }
                let Some(tree) = &shared.tree else { return Err(mcp::Reply::failed("no widgets were drawn".into())) };
                Err(mcp::Reply::text(changes(&before, &lines(tree, None, None))))
            }
            Job::Capturing { image, rect, scale } => {
                let Some(image) = image.lock().unwrap_or_else(PoisonError::into_inner).take() else {
                    return Ok(Some(Job::Capturing { image, rect, scale }));
                };
                Err(screenshot(&image, rect, scale))
            }
        }
    }

    /// Start a request, with the tree to read or act on.
    fn start(
        &mut self,
        ctx: &egui::Context,
        tree: &Tree,
        name: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
        drive: bool,
    ) -> Result<Job, mcp::Reply> {
        let arguments = serde_json::Value::Object(arguments);
        let bad = |e: serde_json::Error| mcp::Reply::failed(format!("arguments: {e}"));
        match name {
            "editor_ui" => {
                let args: mcp::UiArgs = serde_json::from_value(arguments).map_err(bad)?;
                Err(self.read(ctx, tree, &args))
            }
            "editor_screenshot" => {
                let args: mcp::ScreenshotArgs = serde_json::from_value(arguments).map_err(bad)?;
                let rect = args.pane.as_deref().map(|p| self.pane(p)).transpose().map_err(mcp::Reply::failed)?;
                let image = Arc::new(Mutex::new(None));
                let (slot, repaint) = (image.clone(), ctx.clone());
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |shot| {
                    *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(shot);
                    repaint.request_repaint();
                })));
                Ok(Job::Capturing { image, rect, scale: ctx.pixels_per_point() })
            }
            "editor_act" | "editor_type" if !drive => Err(mcp::Reply::failed(self.refusal().to_string())),
            "editor_act" => {
                let args: mcp::ActArgs = serde_json::from_value(arguments).map_err(bad)?;
                let batches = act(ctx, tree, &args).map_err(mcp::Reply::failed)?;
                Ok(self.queue(tree, batches))
            }
            "editor_type" => {
                let args: mcp::TypeArgs = serde_json::from_value(arguments).map_err(bad)?;
                let batches = typing(tree, &args).map_err(mcp::Reply::failed)?;
                Ok(self.queue(tree, batches))
            }
            _ => Err(mcp::Reply::failed(format!("{name}: not a tool of the editor's window"))),
        }
    }

    /// Queue an action's input, to be answered once it has settled.
    fn queue(&mut self, tree: &Tree, batches: Vec<Vec<egui::Event>>) -> Job {
        self.acted = Some(Instant::now());
        let before = lines(tree, None, None);
        let mut shared = lock(&self.shared);
        // The frames the batches go in, then those settling them; a batch already queued goes first.
        let until = shared.frames + (shared.batches.len() + batches.len()) as u64 + SETTLE;
        shared.batches.extend(batches);
        Job::Settling { before, until }
    }

    /// A pane by its tab's title, case aside.
    fn pane(&self, title: &str) -> Result<(String, egui::Rect), String> {
        self.panes.iter().find(|(t, _)| t.eq_ignore_ascii_case(title.trim())).cloned().ok_or_else(|| {
            let open: Vec<&str> = self.panes.iter().map(|(t, _)| t.as_str()).collect();
            format!("no pane {title} in the main window; open: {}", open.join(", "))
        })
    }

    /// The window's widgets as an answer.
    fn read(&self, ctx: &egui::Context, tree: &Tree, args: &mcp::UiArgs) -> mcp::Reply {
        let rect = match args.pane.as_deref().map(|p| self.pane(p)).transpose() {
            Ok(rect) => rect,
            Err(e) => return mcp::Reply::failed(e),
        };
        let size = ctx.content_rect().size();
        let mut text = format!("window {:.0} x {:.0} points; panes: ", size.x, size.y);
        text += &self.panes.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>().join(", ");
        text.push('\n');
        let all = lines(tree, rect.as_ref().map(|(_, r)| *r), args.bounds.then_some(ctx));
        let shown: Vec<String> = match args.text.as_deref().map(str::to_lowercase) {
            Some(needle) => {
                let mut within: Vec<&Line> = Vec::new();
                let mut found = Vec::new();
                for line in &all {
                    within.truncate(line.depth);
                    if line.words.to_lowercase().contains(&needle) {
                        let path: Vec<&str> =
                            within.iter().filter(|l| !l.label.is_empty()).map(|l| l.label.as_str()).collect();
                        let context =
                            if path.is_empty() { String::new() } else { format!("   (in {})", path.join(" › ")) };
                        found.push(format!("{}{context}", line.text));
                    }
                    within.push(line);
                }
                found
            }
            None => all.iter().map(|l| format!("{}{}", "  ".repeat(l.depth), l.text)).collect(),
        };
        if shown.is_empty() {
            text += "no widgets match\n";
        }
        for line in shown.iter().take(MOST_LINES) {
            text += line;
            text.push('\n');
        }
        if shown.len() > MOST_LINES {
            let _ = writeln!(text, "... {} more: narrow it by pane or text", shown.len() - MOST_LINES);
        }
        mcp::Reply::text(text)
    }
}

/// A widget as an answer shows it.
struct Line {
    id: NodeId,
    depth: usize,
    /// Its label, for the context of others.
    label: String,
    /// What text matching looks at: its label and value.
    words: String,
    text: String,
}

/// Whether a node is shown: one with something to read or do. Containers without either are left out, their
/// children shown in their place.
fn shown(node: &accesskit::Node) -> bool {
    let plain = matches!(node.role(), Role::GenericContainer | Role::Unknown | Role::Group | Role::Pane);
    node.label().is_some_and(|l| !l.trim().is_empty())
        || node.value().is_some_and(|v| !v.trim().is_empty())
        || node.supports_action(Action::Click)
        || node.supports_action(Action::Focus)
        || !plain
}

/// A node's id as answers give it: its low 32 bits in hex, unique enough within a window and short.
fn short(id: NodeId) -> String {
    format!("{:08x}", id.0 & 0xFFFF_FFFF)
}

/// The tree's shown widgets in order, with their depths among shown ones; within `rect` (by their centers) when given.
fn lines(tree: &Tree, rect: Option<egui::Rect>, places: Option<&egui::Context>) -> Vec<Line> {
    let mut out = Vec::new();
    let mut stack = vec![(tree.root, 0)];
    while let Some((id, depth)) = stack.pop() {
        let Some(node) = tree.nodes.get(&id) else { continue };
        let area = node.bounds().map(|b| {
            #[expect(clippy::cast_possible_truncation, reason = "window coordinates")]
            let area =
                egui::Rect::from_min_max(egui::pos2(b.x0 as f32, b.y0 as f32), egui::pos2(b.x1 as f32, b.y1 as f32));
            places.map_or(area, |ctx| to_window(ctx, id).mul_rect(area))
        });
        let inside = rect.is_none_or(|r| area.is_some_and(|a| r.contains(a.center())));
        // A label's text runs say again what it says.
        if node.role() == Role::TextRun || node.is_hidden() {
            continue;
        }
        let show = shown(node) && node.role() != Role::Window && inside;
        if show {
            out.push(line(id, node, depth, id == tree.focus, area.filter(|_| places.is_some())));
        }
        let next = if show { depth + 1 } else { depth };
        stack.extend(node.children().iter().rev().map(|c| (*c, next)));
    }
    out
}

fn line(id: NodeId, node: &accesskit::Node, depth: usize, focused: bool, area: Option<egui::Rect>) -> Line {
    let mut label = crate::icons::spelled(node.label().unwrap_or_default().trim());
    let mut value = crate::icons::spelled(node.value().unwrap_or_default().trim());
    // A label's text is its value; it reads as its label.
    if node.role() == Role::Label && label.is_empty() {
        label = std::mem::take(&mut value);
    }
    let mut text = format!("{:?}", node.role()).to_lowercase();
    if !label.is_empty() {
        let _ = write!(text, " \"{label}\"");
    }
    if !value.is_empty() && value != label {
        let _ = write!(text, " = \"{value}\"");
    }
    let mut states = Vec::new();
    match node.toggled() {
        Some(Toggled::True) => states.push("checked"),
        Some(Toggled::Mixed) => states.push("mixed"),
        Some(Toggled::False) => states.push("unchecked"),
        None => {}
    }
    if node.is_selected() == Some(true) {
        states.push("selected");
    }
    match node.is_expanded() {
        Some(true) => states.push("expanded"),
        Some(false) => states.push("collapsed"),
        None => {}
    }
    if node.is_disabled() {
        states.push("disabled");
    }
    if focused {
        states.push("focused");
    }
    if !states.is_empty() {
        let _ = write!(text, " [{}]", states.join(", "));
    }
    let _ = write!(text, " #{}", short(id));
    if let Some(a) = area {
        let _ = write!(text, " @{:.0},{:.0} {:.0}x{:.0}", a.min.x, a.min.y, a.width(), a.height());
    }
    let words = format!("{label} {value}");
    Line { id, depth, label, words, text }
}

/// What changed between two readings of the window.
fn changes(before: &[Line], after: &[Line]) -> String {
    let old: HashMap<NodeId, &Line> = before.iter().map(|l| (l.id, l)).collect();
    let new: HashMap<NodeId, &Line> = after.iter().map(|l| (l.id, l)).collect();
    let mut out = Vec::new();
    for line in after {
        match old.get(&line.id) {
            None => out.push(format!("+ {}", line.text)),
            Some(was) if was.text != line.text => out.push(format!("~ {}   (was {})", line.text, was.text)),
            Some(_) => {}
        }
    }
    out.extend(before.iter().filter(|l| !new.contains_key(&l.id)).map(|l| format!("- {}", l.text)));
    if out.is_empty() {
        return "nothing in the window changed\n".into();
    }
    let total = out.len();
    let mut text = format!("{total} change{}:\n", if total == 1 { "" } else { "s" });
    for line in out.iter().take(MOST_CHANGES) {
        text += line;
        text.push('\n');
    }
    if total > MOST_CHANGES {
        let _ = writeln!(text, "... {} more: read the window with editor_ui", total - MOST_CHANGES);
    }
    text
}

/// A node by the id an answer gave it.
fn node<'a>(tree: &'a Tree, id: &str) -> Result<(NodeId, &'a accesskit::Node), String> {
    let id = id.trim().trim_start_matches('#').to_lowercase();
    let mut found = tree.nodes.iter().filter(|(n, _)| short(**n) == id);
    match (found.next(), found.next()) {
        (Some((id, node)), None) => Ok((*id, node)),
        (None, _) => Err(format!("no widget #{id} in the window now: read it again with editor_ui")),
        (Some(_), Some(_)) => Err(format!("#{id} names several widgets: act at its point instead")),
    }
}

fn center(node: &accesskit::Node) -> Option<egui::Pos2> {
    #[expect(clippy::cast_possible_truncation, reason = "window coordinates")]
    node.bounds().map(|b| egui::pos2(f64::midpoint(b.x0, b.x1) as f32, f64::midpoint(b.y0, b.y1) as f32))
}

fn request(id: NodeId, action: Action) -> egui::Event {
    egui::Event::AccessKitActionRequest(accesskit::ActionRequest {
        action,
        target_tree: accesskit::TreeId::ROOT,
        target_node: id,
        data: None,
    })
}

fn button(pos: egui::Pos2, button: egui::PointerButton, pressed: bool) -> Vec<egui::Event> {
    vec![egui::Event::PointerButton { pos, button, pressed, modifiers: egui::Modifiers::NONE }]
}

/// What egui knows of a widget by its node: its response.
fn response(ctx: &egui::Context, id: NodeId) -> Option<egui::Response> {
    // SAFETY: only entropy is asked of it, and this value is an `Id`'s own, as a node's id is.
    ctx.read_response(unsafe { egui::Id::from_high_entropy_bits(id.0) })
}

/// What takes a widget's place to the window's: a node gives it in its layer's coordinates, and a zoomed scene is a layer
/// of its own.
fn to_window(ctx: &egui::Context, id: NodeId) -> egui::emath::TSTransform {
    response(ctx, id).and_then(|r| ctx.layer_transform_to_global(r.layer_id)).unwrap_or_default()
}

/// Whether a widget cannot be reached at a point of the window: egui knows where each is clickable, clipped by what holds
/// it, so a pointer sent elsewhere would land on whatever is there. One egui keeps no place for takes no pointer anyway.
fn clipped(ctx: &egui::Context, id: NodeId, point: egui::Pos2) -> bool {
    response(ctx, id).is_some_and(|r| !to_window(ctx, id).mul_rect(r.interact_rect).contains(point))
}

/// An action's input, a batch a frame.
fn act(ctx: &egui::Context, tree: &Tree, args: &mcp::ActArgs) -> Result<Vec<Vec<egui::Event>>, String> {
    let action = args.action.as_deref().unwrap_or("click");
    let target = args.node.as_deref().map(|id| node(tree, id)).transpose()?;
    let at = match (args.at, target) {
        (Some([x, y]), _) => Some(egui::pos2(x, y)),
        (None, Some((id, node))) => center(node).map(|p| to_window(ctx, id).mul_pos(p)),
        (None, None) => return Err("give a widget (node) or a point (at)".into()),
    };
    let window = ctx.content_rect();
    let hidden = args.at.is_none()
        && at.is_none_or(|p| !window.contains(p) || target.is_some_and(|(id, _)| clipped(ctx, id, p)));
    let point = || at.ok_or_else(|| "the widget has no place in the window".to_string());
    let moved = |p: egui::Pos2| vec![egui::Event::PointerMoved(p)];
    let primary = egui::PointerButton::Primary;
    Ok(match action {
        "focus" | "scroll_into_view" => {
            let (id, _) = target.ok_or("focus and scroll_into_view take a widget")?;
            let which = if action == "focus" { Action::Focus } else { Action::ScrollIntoView };
            vec![vec![request(id, which)]]
        }
        // What the pointer cannot reach, the pointer is not sent to: it would land on whatever is there.
        "double_click" | "right_click" | "drag" if hidden => {
            return Err("the widget is out of sight: scroll_into_view first".into());
        }
        // Clicked as the user would, with the pointer: what a click does besides (closing a popup) follows. A widget
        // out of sight is clicked through its accessibility, as a screen reader would.
        "click" if hidden => {
            let (id, node) = target.ok_or("no widget")?;
            if !node.supports_action(Action::Click) {
                return Err("the widget is out of sight and cannot be clicked: scroll_into_view first".into());
            }
            vec![vec![request(id, Action::Click)]]
        }
        "click" => {
            let p = point()?;
            vec![moved(p), button(p, primary, true), button(p, primary, false)]
        }
        "double_click" => {
            let p = point()?;
            let mut batches = vec![moved(p)];
            for _ in 0..2 {
                batches.extend([button(p, primary, true), button(p, primary, false)]);
            }
            batches
        }
        "right_click" => {
            let p = point()?;
            let secondary = egui::PointerButton::Secondary;
            vec![moved(p), button(p, secondary, true), button(p, secondary, false)]
        }
        "hover" => vec![moved(point()?)],
        "drag" => {
            let (from, [x, y]) = (point()?, args.to.ok_or("a drag takes where it ends (to)")?);
            let to = egui::pos2(x, y);
            vec![
                moved(from),
                button(from, primary, true),
                moved(from.lerp(to, 0.5)),
                moved(to),
                button(to, primary, false),
            ]
        }
        "scroll" => {
            let (p, [dx, dy]) = (point()?, args.by.ok_or("a scroll takes how far (by)")?);
            let wheel = egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(dx, dy),
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Unknown,
                modifiers: egui::Modifiers::NONE,
            };
            vec![moved(p), vec![wheel]]
        }
        other => {
            return Err(format!(
                "{other}: not an action (click, double_click, right_click, hover, focus, \
                                     scroll_into_view, drag, scroll)"
            ));
        }
    })
}

/// Typing's input: the widget focused, the text, then each key pressed and released.
fn typing(tree: &Tree, args: &mcp::TypeArgs) -> Result<Vec<Vec<egui::Event>>, String> {
    let mut batches = Vec::new();
    if let Some(id) = &args.node {
        let (id, _) = node(tree, id)?;
        batches.push(vec![request(id, Action::Focus)]);
    }
    if let Some(text) = args.text.as_ref().filter(|t| !t.is_empty()) {
        batches.push(vec![egui::Event::Text(text.clone())]);
    }
    for combo in &args.keys {
        let (key, modifiers) = chord(combo)?;
        let event = |pressed| egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers };
        batches.push(vec![event(true)]);
        batches.push(vec![event(false)]);
    }
    if batches.is_empty() {
        return Err("nothing to type: give text or keys".into());
    }
    Ok(batches)
}

/// A key and its modifiers: `ctrl+shift+Z`.
pub(crate) fn chord(combo: &str) -> Result<(egui::Key, egui::Modifiers), String> {
    let mut parts: Vec<&str> = combo.split('+').map(str::trim).collect();
    let name = parts.pop().filter(|n| !n.is_empty()).ok_or_else(|| format!("{combo}: no key"))?;
    let key = egui::Key::from_name(name).ok_or_else(|| format!("{name}: not a key's name (Enter, Tab, A, F2...)"))?;
    let mut modifiers = egui::Modifiers::NONE;
    for part in parts {
        match part.to_lowercase().as_str() {
            "ctrl" | "control" => modifiers.ctrl = true,
            "shift" => modifiers.shift = true,
            "alt" | "option" => modifiers.alt = true,
            "cmd" | "command" => modifiers.command = true,
            other => return Err(format!("{other}: not a modifier (ctrl, shift, alt, cmd)")),
        }
    }
    // egui's command is Ctrl outside macOS, and is what shortcuts test.
    if cfg!(target_os = "macos") {
        modifiers.mac_cmd = modifiers.command;
    } else {
        modifiers.command |= modifiers.ctrl;
        modifiers.ctrl |= modifiers.command;
    }
    Ok((key, modifiers))
}

/// A screenshot as an answer: the window, or a pane of it.
fn screenshot(image: &egui::ColorImage, rect: Option<(String, egui::Rect)>, scale: f32) -> mcp::Reply {
    let [width, height] = image.size;
    let (x0, y0, x1, y1) = match &rect {
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "pixels within the window")]
        Some((_, r)) => (
            ((r.min.x * scale).round().max(0.0) as usize).min(width),
            ((r.min.y * scale).round().max(0.0) as usize).min(height),
            ((r.max.x * scale).round().max(0.0) as usize).min(width),
            ((r.max.y * scale).round().max(0.0) as usize).min(height),
        ),
        None => (0, 0, width, height),
    };
    let mut rgba = Vec::with_capacity((x1 - x0) * (y1 - y0) * 4);
    for y in y0..y1 {
        rgba.extend(image.pixels[y * width + x0..y * width + x1].iter().flat_map(egui::Color32::to_array));
    }
    let picture = pe2_ext::Image { width: x1 - x0, height: y1 - y0, rgba };
    match project::ext::png(&picture) {
        Ok(png) => {
            let what = rect.map_or_else(|| "the main window".to_string(), |(title, _)| format!("the {title} pane"));
            let caption = format!("{what}: {} x {} pixels, {scale:.2} a point", picture.width, picture.height);
            mcp::Reply::picture(caption, &png)
        }
        Err(e) => mcp::Reply::failed(format!("{e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::clipped;
    use eframe::egui;

    #[test]
    fn a_widget_scrolled_out_of_view_is_clipped() {
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(300.0, 300.0));
        let input = || egui::RawInput { screen_rect: Some(screen), ..egui::RawInput::default() };
        let mut buttons: Vec<egui::Response> = Vec::new();
        // Two passes: what a widget's place is, is known from the last.
        for _ in 0..2 {
            buttons.clear();
            let mut output = ctx.run_ui(input(), |ui| {
                egui::ScrollArea::vertical().max_height(100.0).show(ui, |ui| {
                    buttons.extend((0..40).map(|i| ui.button(format!("row {i}"))));
                });
            });
            output.textures_delta.clear();
        }
        let (first, last) = (buttons.first().unwrap(), buttons.last().unwrap());
        assert!(screen.contains(first.rect.center()));
        assert!(!clipped(&ctx, first.id.accesskit_id(), first.rect.center()));
        // Below the scroll area, in the window: where a pointer would land on something else.
        let hidden = egui::pos2(last.rect.center().x, 200.0);
        assert!(screen.contains(hidden));
        assert!(clipped(&ctx, last.id.accesskit_id(), hidden));
    }
}
