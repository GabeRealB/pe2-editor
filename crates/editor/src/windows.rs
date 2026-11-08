//! Windows of their own for panes. A pane is moved out of the main window into a native window - egui's immediate
//! viewports, which share the editor's state rather than a copy of it; where the platform cannot open one, egui draws
//! it as a window inside the main one - and each window holds a layout of its own, so panes dock together there too.
//! Closing a window, or its dock-back button, returns its panes to the main window, where they came from.

use eframe::egui;
use egui_tiles::{Container, Tile, TileId, Tree};
use serde::{Deserialize, Serialize};

use std::collections::BTreeMap;

use crate::panes::{EditorKind, MODES, Pane, PaneAction, PaneKind, PaneState};

#[derive(Serialize, Deserialize)]
pub struct Window {
    pub id: u64,
    pub tree: Tree<Pane>,
    /// Where it was: it opens there again.
    #[serde(default)]
    pub place: Option<Place>,
    /// The place it opened with this run. Given once: fed back every frame, a position the platform reports a decoration away
    /// from where it places the window (X11 under `WSLg`) walks the window off to the top left.
    #[serde(skip)]
    opened: Option<Place>,
    /// Whether it has been opened this run.
    #[serde(skip)]
    made: bool,
    /// What corrects what the platform reports of it.
    #[serde(skip)]
    tracker: Tracker,
}

/// Where a window is. What is asked for when one is made does not land exactly where it says: X11 under `WSLg` scales a size
/// and a position by different factors, and reports the outer position a decoration off. So what counts is where its contents
/// were, in pixels; `position` is only what to ask for to land near there, learned from where the last one landed.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Place {
    /// What to ask for, in points as a window is made in.
    pub position: [f32; 2],
    pub size: [f32; 2],
    /// Where its contents were, in pixels.
    #[serde(default)]
    pub contents: [f32; 2],
    /// Its pixels a point, then: zero where it is not known.
    #[serde(default)]
    pub scale: f32,
}

impl Place {
    /// Whether a window here can be got at: enough of its top on some monitor to take hold of it. With no monitors known, or
    /// no pixels known of it, nothing says otherwise.
    #[must_use]
    pub fn is_reachable(&self, monitors: &[egui::Rect]) -> bool {
        if monitors.is_empty() || self.scale <= 0.0 {
            return true;
        }
        let top = egui::Rect::from_min_size(self.contents.into(), egui::vec2(self.size[0] * self.scale, GRIP.y));
        monitors.iter().any(|monitor| {
            let shared = monitor.intersect(top);
            shared.width() >= GRIP.x && shared.height() >= GRIP.y
        })
    }
}

/// What of a window's top must be on a monitor for it to be taken hold of, in pixels.
const GRIP: egui::Vec2 = egui::vec2(120.0, 24.0);

/// Corrections to try once a window is shown: each takes the platform a moment.
const CORRECTIONS: u8 = 8;
/// Frames to leave one before looking again.
const SETTLING: u8 = 12;
/// How many pixels off is off.
const NEAR: f32 = 2.0;
/// Frames its scale must hold before a window is measured.
const STEADY: u8 = 8;
/// How much of its monitor a window too large for it comes to.
const FITTED: f32 = 0.85;

/// Follows where a window is, and moves it where it was once it is shown. Only what the user does to it afterwards changes
/// where it should be: taking where a window landed for where it was left is what walks it across the screen.
#[derive(Clone, Copy, Debug, Default)]
pub struct Tracker {
    /// Where it should be.
    wanted: Option<Place>,
    target: Option<egui::Vec2>,
    /// What was asked for, and where that landed.
    asked: Option<[f32; 2]>,
    landed: Option<egui::Vec2>,
    /// Where it stood when the corrections ended: what the user moves it from.
    settled: Option<egui::Vec2>,
    /// The outer position last asked for, in pixels: a window going to another monitor changes its scale on the way.
    requested: Option<egui::Pos2>,
    corrections: u8,
    waiting: u8,
    /// The scale last seen, and for how many frames it has held.
    scale: f32,
    steady: u8,
}

impl Tracker {
    #[must_use]
    pub fn new(wanted: Option<Place>) -> Tracker {
        Tracker {
            wanted: wanted.filter(|place| place.scale > 0.0 && place.contents != [0.0, 0.0]),
            asked: wanted.map(|place| place.position),
            ..Tracker::default()
        }
    }

    /// It has stopped filling the screen: back where it was, which not every platform sees to.
    pub fn return_to(&mut self, place: Option<Place>) {
        *self = Tracker { wanted: place, asked: self.asked, landed: self.landed, ..Tracker::default() };
    }

    /// Whether it is where it should be, and what the user does from now on is theirs.
    #[must_use]
    pub fn is_settled(&self) -> bool {
        self.settled.is_some()
    }

    /// The place of the window drawn now, once it is known: not while it fills the screen, which is no place to come back to.
    pub fn observe(&mut self, ctx: &egui::Context) -> Option<Place> {
        // All of one frame: the scale the rectangles were measured in, which a zoom set this frame is not.
        let (inner, outer, monitor, filling, scale) = ctx.input(|i| {
            let viewport = i.viewport();
            let filling = viewport.maximized == Some(true) || viewport.fullscreen == Some(true);
            (viewport.inner_rect, viewport.outer_rect, viewport.monitor_size, filling, i.pixels_per_point)
        });
        // Before it is shown, a window is far off every screen.
        let inner = inner.filter(|r| r.min.x > -10_000.0 && r.min.y > -10_000.0)?;
        if filling {
            return None;
        }
        // A window finds its scale over its first frames, and again on reaching another monitor: until that has held, what it
        // measures says nothing.
        if self.settled.is_none() {
            self.steady = if (scale - self.scale).abs() < 0.001 { self.steady.saturating_add(1) } else { 0 };
            self.scale = scale;
            if self.steady < STEADY || self.waiting > 0 {
                self.waiting = self.waiting.saturating_sub(1);
                ctx.request_repaint();
                return None;
            }
        }
        let contents = inner.min.to_vec2() * scale;
        let landed = *self.landed.get_or_insert(contents);
        // Where it should be: where it stands, for a window with no place to go back to.
        let target =
            *self.target.get_or_insert(self.wanted.map_or(contents, |wanted| egui::Vec2::from(wanted.contents)));
        if self.settled.is_none() {
            let moved = target - contents;
            let size = self.wanted.map_or(inner.size() * scale, |wanted| egui::Vec2::from(wanted.size) * wanted.scale);
            // Its monitor is known once it is on it.
            let room = monitor.filter(|_| moved.length() <= NEAR).map(|monitor| monitor * scale);
            let size =
                room.filter(|room| size.x > room.x || size.y > room.y).map_or(size, |room| size.min(room * FITTED));
            let resized = size - inner.size() * scale;
            if self.corrections < CORRECTIONS && (moved.length() > NEAR || resized.length() > NEAR) {
                self.corrections += 1;
                self.waiting = SETTLING;
                // From what was last asked for: the outer position read back is what may be off.
                if moved.length() > NEAR
                    && let Some(from) =
                        self.requested.or(outer.map(|outer| egui::pos2(outer.min.x * scale, outer.min.y * scale)))
                {
                    let to = from + moved;
                    self.requested = Some(to);
                    ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(to.x / scale, to.y / scale)));
                }
                if resized.length() > NEAR {
                    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size / scale));
                }
                ctx.request_repaint();
                return None;
            }
            self.settled = Some(contents);
        }
        let contents = target + (contents - self.settled?);
        // What to ask for to land there, by how far from it the last one landed.
        let asked = egui::Vec2::from(self.asked.unwrap_or([0.0, 0.0]));
        let position = asked + (contents - landed) / scale;
        Some(Place {
            position: [position.x, position.y],
            size: [inner.width(), inner.height()],
            contents: [contents.x, contents.y],
            scale,
        })
    }
}

impl Window {
    pub fn new(id: u64, pane: Pane) -> Window {
        let tree = Tree::new_tabs(egui::Id::unique(("pe2 window", id)), vec![pane]);
        Window { id, tree, place: None, opened: None, made: false, tracker: Tracker::default() }
    }

    pub fn viewport(&self) -> egui::ViewportId {
        egui::ViewportId::from_hash_of(("pe2 window", self.id))
    }

    /// Its title: the panes it shows.
    pub fn title(&self) -> String {
        let names: Vec<String> = panes_of(&self.tree)
            .into_iter()
            .filter(|(id, _)| self.tree.tiles.is_visible(*id))
            .map(|(_, pane)| pane.kind.title())
            .collect();
        if names.is_empty() { "Window".to_string() } else { names.join(", ") }
    }

    /// Its viewport: the title it has now, the place and size it opened with.
    pub fn builder(&mut self) -> egui::ViewportBuilder {
        if !self.made {
            self.made = true;
            self.opened = self.place;
            self.tracker = Tracker::new(self.place);
        }
        let place = self.opened;
        let size = place.map_or([560.0, 640.0], |place| place.size);
        let mut builder =
            egui::ViewportBuilder::default().with_title(format!("{} - PE2 Editor", self.title())).with_inner_size(size);
        if let Some(place) = place {
            builder = builder.with_position(place.position);
        }
        builder
    }

    /// Keep where the window is and its size, to open it there again.
    pub fn remember_place(&mut self, ctx: &egui::Context) {
        if let Some(place) = self.tracker.observe(ctx) {
            self.place = Some(place);
        }
    }
}

/// Every layout: the main window's, the other windows', each pane instance's state, and where panes moved into
/// windows came from.
pub struct Layouts {
    pub main: Tree<Pane>,
    pub windows: Vec<Window>,
    /// Each pane instance's own state: what it is pinned to.
    pub states: BTreeMap<Pane, PaneState>,
    /// The group of the main layout each pane in a window came from, to return it there.
    pub homes: BTreeMap<Pane, TileId>,
    /// The document tab last used: a new one opens beside it.
    pub focused: Option<Pane>,
    /// A tab just brought to the front, to scroll its strip to.
    pub reveal: Option<Pane>,
    /// The modes: saved layouts, one in use, whose own is `main`.
    pub modes: Vec<Mode>,
    pub mode: usize,
}

/// A saved layout of the main window: what is changed in it stays. The documents are not its: they go from mode to mode.
#[derive(Serialize, Deserialize)]
pub struct Mode {
    pub name: String,
    /// Its layout, while another mode is in use.
    pub tree: Option<Tree<Pane>>,
}

/// The modes there are at first, the first in use.
pub fn first_modes() -> Vec<Mode> {
    let mode = |(at, name): (usize, &&str)| Mode {
        name: (*name).to_string(),
        tree: (at > 0).then(|| crate::panes::preset(name)),
    };
    MODES.iter().enumerate().map(mode).collect()
}

impl Layouts {
    pub fn new(main: Tree<Pane>) -> Layouts {
        Layouts {
            main,
            windows: vec![],
            states: BTreeMap::new(),
            homes: BTreeMap::new(),
            focused: None,
            reveal: None,
            modes: first_modes(),
            mode: 0,
        }
    }

    /// Where a pane is: its window (none: the main one) and its tile.
    pub fn find(&self, pane: Pane) -> Option<(Option<u64>, TileId)> {
        if let Some(tile) = tile_of(&self.main, pane) {
            return Some((None, tile));
        }
        self.windows.iter().find_map(|w| Some((Some(w.id), tile_of(&w.tree, pane)?)))
    }

    /// A window's layout; none: the main one.
    fn tree_mut(&mut self, window: Option<u64>) -> Option<&mut Tree<Pane>> {
        match window {
            None => Some(&mut self.main),
            Some(id) => self.windows.iter_mut().find(|w| w.id == id).map(|w| &mut w.tree),
        }
    }

    pub fn is_visible(&self, pane: Pane) -> bool {
        match self.find(pane) {
            Some((None, tile)) => self.main.tiles.is_visible(tile),
            Some((Some(window), tile)) => self.windows.iter().any(|w| w.id == window && w.tree.tiles.is_visible(tile)),
            None => false,
        }
    }

    /// Hide a pane: one in a window of its own returns to the main window, hidden.
    pub fn hide(&mut self, pane: Pane) {
        if let Some((window, tile)) = self.find(pane) {
            self.act(window, tile, PaneAction::Close);
        }
    }

    /// Show a pane and bring its tab to the front; a pane no layout holds joins the preview's tabs. Returns the
    /// window to bring to the front, where it is in one.
    pub fn show(&mut self, pane: Pane) -> Option<egui::ViewportId> {
        let Some((window, tile)) = self.find(pane) else {
            let beside = self.home_of(pane.kind);
            put(&mut self.main, pane, None, beside);
            return None;
        };
        if let Some(tree) = self.tree_mut(window) {
            tree.tiles.set_visible(tile, true);
            activate(tree, tile);
        }
        window.and_then(|id| self.windows.iter().find(|w| w.id == id)).map(Window::viewport)
    }

    /// The next instance of a kind: one past every instance any layout holds.
    pub fn next_instance(&self, kind: PaneKind) -> u32 {
        let trees = std::iter::once(&self.main).chain(self.windows.iter().map(|w| &w.tree));
        trees.flat_map(panes_of).filter(|(_, p)| p.kind == kind).map(|(_, p)| p.instance + 1).max().unwrap_or(0)
    }

    /// The document tabs, in the order they were opened.
    fn documents(&self) -> Vec<Pane> {
        let trees = std::iter::once(&self.main).chain(self.windows.iter().map(|w| &w.tree));
        let mut documents: Vec<Pane> =
            trees.flat_map(panes_of).map(|(_, p)| p).filter(|p| p.kind == PaneKind::Document).collect();
        documents.sort();
        documents
    }

    /// What a document tab shows.
    pub fn subject(&self, pane: Pane) -> Option<&str> {
        self.states.get(&pane)?.pinned.as_deref()
    }

    /// Whether a document tab is the one the next asset opened replaces.
    fn is_preview(&self, pane: Pane) -> bool {
        self.states.get(&pane).is_none_or(|s| s.pinned.is_none() || s.preview)
    }

    /// A pane a new tab of the document kind joins.
    fn anchor(&self) -> Option<Pane> {
        let open = |pane: &Pane| self.find(*pane).is_some();
        self.focused
            .filter(open)
            .or_else(|| self.documents().first().copied())
            .or(Some(Pane::first(PaneKind::Layer)).filter(open))
    }

    /// The pane one no layout holds joins: what belongs in its area, else the documents.
    fn home_of(&self, kind: PaneKind) -> Option<Pane> {
        let open = |pane: &Pane| tile_of(&self.main, *pane).is_some();
        kind.home().iter().map(|home| Pane::first(*home)).find(open).or_else(|| self.anchor())
    }

    /// The group in use and the tab in front of it: of the document in use, else of the group holding the first document.
    pub fn tab_in_use(&self) -> Option<(Option<u64>, TileId, TileId)> {
        let anchor =
            self.focused.filter(|pane| self.find(*pane).is_some()).or_else(|| self.documents().first().copied())?;
        let (window, tile) = self.find(anchor)?;
        let tree = self.windows.iter().find(|w| Some(w.id) == window).map_or(&self.main, |w| &w.tree);
        let group = tree.tiles.parent_of(tile)?;
        let Some(Tile::Container(Container::Tabs(tabs))) = tree.tiles.get(group) else {
            return Some((window, group, tile));
        };
        Some((window, group, tabs.active.filter(|active| tree.tiles.is_visible(*active)).unwrap_or(tile)))
    }

    /// Whether there is a tab to act on.
    pub fn has_tab(&self) -> bool {
        self.tab_in_use().is_some()
    }

    /// Close the tab in front of the group in use, whatever it shows.
    pub fn close_tab(&mut self) {
        if let Some((window, _, tile)) = self.tab_in_use() {
            self.focused = self.focused.or_else(|| self.active_document());
            self.act(window, tile, PaneAction::Close);
        }
    }

    /// Bring the next tab of the group in use to the front, or the one before.
    pub fn next_tab(&mut self, forward: bool) {
        let Some((window, group, tile)) = self.tab_in_use() else { return };
        let Some(tree) = self.tree_mut(window) else { return };
        let Some(Tile::Container(Container::Tabs(tabs))) = tree.tiles.get(group) else { return };
        let shown: Vec<TileId> = tabs.children.iter().copied().filter(|t| tree.tiles.is_visible(*t)).collect();
        let Some(at) = shown.iter().position(|t| *t == tile) else { return };
        let next = shown[(at + if forward { 1 } else { shown.len() - 1 }) % shown.len()];
        activate(tree, next);
        let pane = panes_of(tree).into_iter().find_map(|(id, pane)| (id == next).then_some(pane));
        if let Some(pane) = pane {
            self.reveal = Some(pane);
            if pane.kind == PaneKind::Document {
                self.focused = Some(pane);
            }
        }
    }

    /// Whether a pane is what its group shows now.
    pub fn is_showing(&self, pane: Pane) -> bool {
        let trees = std::iter::once(&self.main).chain(self.windows.iter().map(|w| &w.tree));
        let front: Vec<TileId> = trees.flat_map(Tree::active_tiles).collect();
        self.find(pane).is_some_and(|(_, tile)| front.contains(&tile))
    }

    /// Change to another mode: its layout, with the documents open now where its own were.
    pub fn switch_mode(&mut self, to: usize) {
        if to == self.mode {
            return;
        }
        let Some(mut tree) = self.modes.get_mut(to).and_then(|mode| mode.tree.take()) else { return };
        self.carry_documents(&mut tree);
        let left = std::mem::replace(&mut self.main, tree);
        self.modes[self.mode].tree = Some(left);
        self.mode = to;
        self.focused = None;
    }

    /// Put the main layout's documents in a layout, in place of those it held when it was left.
    fn carry_documents(&self, tree: &mut Tree<Pane>) {
        let is_document = |(_, pane): &(TileId, Pane)| pane.kind == PaneKind::Document;
        let held: Vec<(TileId, Pane)> = panes_of(tree).into_iter().filter(is_document).collect();
        let group = held.first().and_then(|(tile, _)| tree.tiles.parent_of(*tile));
        let mut documents: Vec<Pane> =
            panes_of(&self.main).into_iter().filter(is_document).map(|(_, pane)| pane).collect();
        documents.sort();
        for (tile, _) in held {
            take(tree, tile);
        }
        let front = self.active_document();
        for pane in documents {
            put(tree, pane, group, None);
        }
        if let Some(tile) = front.and_then(|pane| tile_of(tree, pane)) {
            activate(tree, tile);
        }
        // What a window holds is there: a pane is in one place.
        let windows: Vec<Pane> = self.windows.iter().flat_map(|w| panes_of(&w.tree)).map(|(_, pane)| pane).collect();
        for (tile, _) in panes_of(tree).into_iter().filter(|(_, pane)| windows.contains(pane)) {
            take(tree, tile);
        }
    }

    /// A copy of the mode in use, under a name, and in use.
    pub fn add_mode(&mut self, name: &str) {
        self.modes.push(Mode { name: name.to_string(), tree: Some(self.main.clone()) });
        self.switch_mode(self.modes.len() - 1);
    }

    /// The mode in use as it began.
    pub fn reset_mode(&mut self) {
        let mut tree = crate::panes::preset(&self.modes[self.mode].name);
        self.carry_documents(&mut tree);
        self.main = tree;
        self.focused = None;
    }

    /// Delete the mode in use, the one before it taking over. The last stays.
    pub fn delete_mode(&mut self) {
        if self.modes.len() < 2 {
            return;
        }
        let gone = self.mode;
        self.switch_mode(gone.checked_sub(1).unwrap_or(1));
        self.modes.remove(gone);
        self.mode -= usize::from(self.mode > gone);
    }

    /// Show an entry as a document: in the tab already showing it, else in the tab the last one previewed in, else in a new
    /// one. Kept, no later entry replaces it.
    pub fn open(&mut self, path: &str, keep: bool) {
        let documents = self.documents();
        let showing = |pane: &&Pane| self.subject(**pane) == Some(path);
        let tab = self.focused.iter().find(showing).or_else(|| documents.iter().find(showing)).copied();
        let tab = tab.or_else(|| documents.iter().copied().find(|pane| self.is_preview(*pane))).unwrap_or_else(|| {
            let pane = Pane { kind: PaneKind::Document, instance: self.next_instance(PaneKind::Document) };
            let beside = self.anchor();
            let window = beside.and_then(|pane| self.find(pane)).and_then(|(window, _)| window);
            if let Some(tree) = self.tree_mut(window) {
                let tile = put(tree, pane, None, beside);
                if let Some(beside) = beside.and_then(|beside| tile_of(tree, beside)) {
                    move_beside(tree, tile, beside);
                }
            }
            pane
        });
        // Another entry in the tab starts over; the same keeps how it was shown.
        let same = self.states.get(&tab).filter(|s| s.pinned.as_deref() == Some(path));
        let previewed = same.is_none_or(|s| s.preview);
        let (editor, uid) = same.map_or((None, None), |s| (s.editor, s.uid.clone()));
        self.states.insert(tab, PaneState { pinned: Some(path.to_string()), preview: previewed && !keep, editor, uid });
        self.focused = Some(tab);
        self.reveal = Some(tab);
        if let Some((window, tile)) = self.find(tab)
            && let Some(tree) = self.tree_mut(window)
        {
            tree.tiles.set_visible(tile, true);
            activate(tree, tile);
        }
    }

    /// Show the document in use in an editor.
    pub fn show_in(&mut self, editor: EditorKind) {
        if let Some(state) = self.focused.and_then(|pane| self.states.get_mut(&pane)) {
            state.editor = Some(editor);
        }
    }

    /// An entry's document tab is kept: it was edited.
    pub fn keep(&mut self, path: &str) {
        for state in self.states.values_mut().filter(|s| s.pinned.as_deref() == Some(path)) {
            state.preview = false;
        }
    }

    /// The document tab in front: the one last used where it is, else the first in front in any layout.
    pub fn active_document(&self) -> Option<Pane> {
        let trees = std::iter::once(&self.main).chain(self.windows.iter().map(|w| &w.tree));
        let front: Vec<TileId> = trees.flat_map(Tree::active_tiles).collect();
        let shown = |pane: &Pane| self.find(*pane).is_some_and(|(_, tile)| front.contains(&tile));
        self.focused.filter(shown).or_else(|| self.documents().into_iter().find(shown))
    }

    /// The tab in use closed: the last document of its group comes to the front, else the last of any.
    fn front_after_close(&mut self, window: Option<u64>, group: Option<TileId>) {
        let Some(tree) = self.tree_mut(window) else { return };
        let children = match group.and_then(|group| tree.tiles.get(group)) {
            Some(Tile::Container(Container::Tabs(tabs))) => tabs.children.clone(),
            _ => vec![],
        };
        let is_document =
            |tile: &TileId| matches!(tree.tiles.get(*tile), Some(Tile::Pane(p)) if p.kind == PaneKind::Document);
        if let Some(tile) = children.iter().rev().find(|tile| is_document(tile)).copied() {
            activate(tree, tile);
            self.focused = panes_of(tree).into_iter().find_map(|(id, pane)| (id == tile).then_some(pane));
        } else if let Some(pane) = self.documents().last().copied()
            && let Some((window, tile)) = self.find(pane)
            && let Some(tree) = self.tree_mut(window)
        {
            activate(tree, tile);
            self.focused = Some(pane);
        }
    }

    /// Close the document tabs an entry is gone from: a layout outlives the project its tabs were opened in.
    pub fn close_documents(&mut self, gone: impl Fn(&str) -> bool) {
        // A state outlives a layout that could not be read.
        let open: Vec<Pane> = std::iter::once(&self.main)
            .chain(self.windows.iter().map(|w| &w.tree))
            .flat_map(panes_of)
            .map(|(_, p)| p)
            .collect();
        self.states.retain(|pane, _| open.contains(pane));
        for pane in self.documents() {
            if self.subject(pane).is_some_and(&gone)
                && let Some((window, tile)) = self.find(pane)
            {
                self.act(window, tile, PaneAction::Close);
            }
        }
    }

    /// The asset a document tab shows, where it is the last tab showing it: closing it closes the document.
    pub fn last_tab_of(&self, window: Option<u64>, tile: TileId) -> Option<String> {
        let tree = match window {
            None => &self.main,
            Some(id) => &self.windows.iter().find(|w| w.id == id)?.tree,
        };
        let Some(Tile::Pane(pane)) = tree.tiles.get(tile) else { return None };
        let path = self.subject(*pane).filter(|_| pane.kind == PaneKind::Document)?;
        let elsewhere = self.documents().into_iter().any(|other| other != *pane && self.subject(other) == Some(path));
        (!elsewhere).then(|| path.to_string())
    }

    /// Do what a tab asked: in the layout of `window` (none: the main one), for the pane in `tile`.
    pub fn act(&mut self, window: Option<u64>, tile: TileId, action: PaneAction) {
        let Some(tree) = self.tree_mut(window) else { return };
        let Some(Tile::Pane(pane)) = tree.tiles.get(tile).cloned() else { return };
        match action {
            PaneAction::Another => {
                let another = Pane { kind: pane.kind, instance: self.next_instance(pane.kind) };
                if let Some(tree) = self.tree_mut(window) {
                    let group = tree.tiles.parent_of(tile);
                    let made = put(tree, another, group, None);
                    move_beside(tree, made, tile);
                }
                // A document's other tab shows the same, kept.
                if pane.kind == PaneKind::Document
                    && let Some(state) = self.states.get(&pane).cloned()
                {
                    self.states.insert(another, PaneState { preview: false, ..state });
                    self.focused = Some(another);
                }
            }
            PaneAction::Keep => {
                if let Some(state) = self.states.get_mut(&pane) {
                    state.preview = false;
                }
            }
            PaneAction::Editor(editor) => self.states.entry(pane).or_default().editor = Some(editor),
            PaneAction::Pin(path) => self.states.entry(pane).or_default().pinned = Some(path),
            PaneAction::Follow => {
                self.states.remove(&pane);
            }
            PaneAction::ToNewWindow => {
                if let Some(pane) = self.take(window, tile) {
                    let id = self.windows.iter().map(|w| w.id + 1).max().unwrap_or(1);
                    self.windows.push(Window::new(id, pane));
                }
            }
            PaneAction::ToWindow(target) => {
                if let Some(pane) = self.take(window, tile) {
                    self.place(target, pane);
                }
            }
            // The last document stays, empty: the centre keeps its place.
            PaneAction::Close if pane.kind == PaneKind::Document && self.documents().len() == 1 => {
                self.states.remove(&pane);
                self.focused = None;
            }
            PaneAction::Close if pane.instance > 0 || pane.kind == PaneKind::Document => {
                let group = self.tree_mut(window).and_then(|tree| tree.tiles.parent_of(tile));
                self.take(window, tile);
                self.states.remove(&pane);
                self.homes.remove(&pane);
                if self.focused == Some(pane) {
                    self.focused = None;
                    self.front_after_close(window, group);
                }
            }
            PaneAction::Close => match window {
                Some(_) => {
                    if let Some(pane) = self.take(window, tile) {
                        let shown = self.place(None, pane);
                        self.main.tiles.set_visible(shown, false);
                    }
                }
                None => self.main.tiles.set_visible(tile, false),
            },
        }
        self.windows.retain(|w| !panes_of(&w.tree).is_empty());
    }

    /// Take a pane out of a layout; one leaving the main layout remembers its group there.
    fn take(&mut self, window: Option<u64>, tile: TileId) -> Option<Pane> {
        let (pane, group) = take(self.tree_mut(window)?, tile)?;
        if window.is_none()
            && let Some(group) = group
        {
            self.homes.insert(pane, group);
        }
        Some(pane)
    }

    /// Put a pane into a window's layout (none: the main one, in the group it came from).
    fn place(&mut self, window: Option<u64>, pane: Pane) -> TileId {
        let beside = self.home_of(pane.kind);
        if let Some(target) = window.and_then(|id| self.windows.iter_mut().find(|w| w.id == id)) {
            put(&mut target.tree, pane, None, None)
        } else {
            let home = self.homes.remove(&pane);
            put(&mut self.main, pane, home, beside)
        }
    }

    /// Return a window's panes to the main window and close it.
    pub fn dock_back(&mut self, window: u64) {
        let Some(at) = self.windows.iter().position(|w| w.id == window) else { return };
        let closed = self.windows.remove(at);
        for (tile, pane) in panes_of(&closed.tree) {
            let shown = self.place(None, pane);
            self.main.tiles.set_visible(shown, closed.tree.tiles.is_visible(tile));
        }
    }
}

/// The panes of a layout, with their tiles.
pub fn panes_of(tree: &Tree<Pane>) -> Vec<(TileId, Pane)> {
    tree.tiles
        .iter()
        .filter_map(|(id, tile)| match tile {
            Tile::Pane(pane) => Some((*id, *pane)),
            Tile::Container(_) => None,
        })
        .collect()
}

/// The tile showing a pane in a layout.
pub fn tile_of(tree: &Tree<Pane>, pane: Pane) -> Option<TileId> {
    panes_of(tree).into_iter().find_map(|(id, p)| (p == pane).then_some(id))
}

/// Take a pane's tile out of a layout: the pane, and the group it was in.
pub fn take(tree: &mut Tree<Pane>, tile: TileId) -> Option<(Pane, Option<TileId>)> {
    let group = tree.tiles.parent_of(tile);
    let pane = tree.remove_recursively(tile).into_iter().find_map(|t| match t {
        Tile::Pane(pane) => Some(pane),
        Tile::Container(_) => None,
    })?;
    Some((pane, group))
}

/// Put a pane into a layout - into `group` where that is still one of its containers, else into the group holding
/// `beside`, else at the top - and bring its tab to the front.
pub fn put(tree: &mut Tree<Pane>, pane: Pane, group: Option<TileId>, beside: Option<Pane>) -> TileId {
    let id = tree.tiles.insert_pane(pane);
    let is_container = |tree: &Tree<Pane>, g: TileId| matches!(tree.tiles.get(g), Some(Tile::Container(_)));
    let group = group
        .filter(|g| is_container(tree, *g))
        .or_else(|| beside.and_then(|b| tile_of(tree, b)).and_then(|t| tree.tiles.parent_of(t)))
        .or(tree.root);
    match group {
        Some(g) if is_container(tree, g) => {
            if let Some(Tile::Container(container)) = tree.tiles.get_mut(g) {
                container.add_child(id);
            }
        }
        // A layout of one pane: the two side by side as tabs.
        Some(lone) => tree.root = Some(tree.tiles.insert_tab_tile(vec![lone, id])),
        None => tree.root = Some(id),
    }
    tree.tiles.set_visible(id, true);
    activate(tree, id);
    id
}

/// Move a tab next after another of its group.
fn move_beside(tree: &mut Tree<Pane>, tile: TileId, beside: TileId) {
    if let Some(parent) = tree.tiles.parent_of(tile)
        && let Some(Tile::Container(Container::Tabs(tabs))) = tree.tiles.get_mut(parent)
        && let Some(from) = tabs.children.iter().position(|child| *child == tile)
    {
        tabs.children.remove(from);
        let at = tabs.children.iter().position(|child| *child == beside).map_or(from, |at| at + 1);
        tabs.children.insert(at, tile);
    }
}

/// Bring a tile's tab to the front of its group.
pub fn activate(tree: &mut Tree<Pane>, tile: TileId) {
    if let Some(parent) = tree.tiles.parent_of(tile)
        && let Some(Tile::Container(Container::Tabs(tabs))) = tree.tiles.get_mut(parent)
    {
        tabs.set_active(tile);
    }
}

#[cfg(test)]
mod tests {
    use super::{Layouts, Place, SETTLING, STEADY, Tracker, Window, panes_of, put, take, tile_of};
    use crate::panes::{EditorKind, Pane, PaneAction, PaneKind, default_layout};
    use eframe::egui;

    /// A frame of a window with its contents at a place, what the tracker makes of it.
    fn frame(ctx: &egui::Context, tracker: &mut Tracker, at: [f32; 2]) -> Option<Place> {
        sized(ctx, tracker, at, [800.0, 600.0], [2560.0, 1440.0])
    }

    fn sized(
        ctx: &egui::Context,
        tracker: &mut Tracker,
        at: [f32; 2],
        size: [f32; 2],
        monitor: [f32; 2],
    ) -> Option<Place> {
        let mut input = egui::RawInput::default();
        let info = input.viewports.entry(egui::ViewportId::ROOT).or_default();
        info.inner_rect = Some(egui::Rect::from_min_size(at.into(), size.into()));
        info.outer_rect = Some(egui::Rect::from_min_size(at.into(), size.into()));
        info.monitor_size = Some(monitor.into());
        let mut place = None;
        let mut output = ctx.run_ui(input, |ui| place = tracker.observe(ui.ctx()));
        output.textures_delta.clear();
        place
    }

    #[test]
    fn a_window_with_no_place_yet_is_where_it_is() {
        let ctx = egui::Context::default();
        let mut tracker = Tracker::new(None);
        // Nothing is taken from it until its scale has held.
        for _ in 0..STEADY {
            assert_eq!(frame(&ctx, &mut tracker, [100.0, 50.0]), None);
        }
        assert_eq!(frame(&ctx, &mut tracker, [100.0, 50.0]).map(|place| place.contents), Some([100.0, 50.0]));
        // Moved, it is where it went, not that far twice over.
        assert_eq!(frame(&ctx, &mut tracker, [4200.0, 200.0]).map(|place| place.contents), Some([4200.0, 200.0]));
    }

    #[test]
    fn a_place_on_no_monitor_cannot_be_reached() {
        let rect = |x, y, w, h| egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, h));
        let monitors = [rect(0.0, 0.0, 3840.0, 2160.0), rect(3840.0, 0.0, 2560.0, 1440.0)];
        let place = |x, y| Place { position: [0.0, 0.0], size: [800.0, 600.0], contents: [x, y], scale: 1.25 };
        assert!(place(100.0, 100.0).is_reachable(&monitors));
        assert!(place(4118.0, 266.0).is_reachable(&monitors));
        // Beyond the second, above them, or with too little of it left to hold; a window can straddle two.
        assert!(!place(9000.0, 100.0).is_reachable(&monitors));
        assert!(!place(4000.0, -500.0).is_reachable(&monitors));
        assert!(!place(6350.0, 100.0).is_reachable(&monitors));
        assert!(place(3500.0, 100.0).is_reachable(&monitors));
        // On the monitor that has gone.
        assert!(!place(4118.0, 266.0).is_reachable(&monitors[..1]));
        // Nothing known, nothing refused.
        assert!(place(9000.0, 100.0).is_reachable(&[]));
    }

    #[test]
    fn a_window_too_large_for_its_monitor_comes_to_fit_it() {
        let ctx = egui::Context::default();
        let wanted = Place { position: [90.0, 40.0], size: [2800.0, 1800.0], contents: [100.0, 50.0], scale: 1.0 };
        let mut tracker = Tracker::new(Some(wanted));
        let monitor = [2560.0, 1440.0];
        for _ in 0..=STEADY + 1 {
            assert_eq!(sized(&ctx, &mut tracker, [100.0, 50.0], [2800.0, 1800.0], monitor), None);
        }
        // Asked to fit, it does, and that is where it stays.
        let fitted = [2560.0 * 0.85, 1440.0 * 0.85];
        let mut place = None;
        for _ in 0..=SETTLING {
            place = sized(&ctx, &mut tracker, [100.0, 50.0], fitted, monitor);
        }
        assert_eq!(place.map(|place| place.size), Some(fitted));
    }

    #[test]
    fn where_a_window_landed_is_not_where_it_was_left() {
        let ctx = egui::Context::default();
        let wanted = Place { position: [90.0, 40.0], size: [800.0, 600.0], contents: [100.0, 50.0], scale: 1.0 };
        let mut tracker = Tracker::new(Some(wanted));
        // It lands off; a correction is asked for, and while it takes effect nothing is taken for its place.
        for _ in 0..=STEADY + 1 {
            assert_eq!(frame(&ctx, &mut tracker, [106.0, 77.0]), None);
        }
        // It is there.
        for _ in 1..SETTLING {
            assert_eq!(frame(&ctx, &mut tracker, [100.0, 50.0]), None);
        }
        let there = frame(&ctx, &mut tracker, [100.0, 50.0]).unwrap();
        assert_eq!(there.contents, [100.0, 50.0]);
        // What to ask for next is what lands there at once: less what it was off by.
        assert_eq!(there.position, [84.0, 13.0]);
        // Then what the user does counts.
        assert_eq!(frame(&ctx, &mut tracker, [130.0, 50.0]).map(|place| place.contents), Some([130.0, 50.0]));
    }

    #[test]
    fn a_pane_opens_where_it_belongs() {
        let mut layouts = Layouts::new(default_layout());
        let (window, at) = layouts.find(Pane::first(PaneKind::History)).unwrap();
        let band = layouts.main.tiles.parent_of(at);
        // Closed it stays where it was; out of the layout it finds its area again.
        layouts.act(window, at, PaneAction::Close);
        layouts.show(Pane::first(PaneKind::History));
        let (_, at) = layouts.find(Pane::first(PaneKind::History)).unwrap();
        assert_eq!(layouts.main.tiles.parent_of(at), band);
        take(&mut layouts.main, at);
        layouts.show(Pane::first(PaneKind::History));
        let (_, at) = layouts.find(Pane::first(PaneKind::History)).unwrap();
        assert_eq!(layouts.main.tiles.parent_of(at), band);
        // What takes a document's room opens among the documents.
        layouts.show(Pane::first(PaneKind::Extensions));
        let (_, at) = layouts.find(Pane::first(PaneKind::Extensions)).unwrap();
        let (_, document) = layouts.find(Pane::first(PaneKind::Document)).unwrap();
        assert_eq!(layouts.main.tiles.parent_of(at), layouts.main.tiles.parent_of(document));
    }

    #[test]
    fn a_mode_is_a_layout_of_its_own_and_the_documents_go_with_you() {
        let mut layouts = Layouts::new(default_layout());
        layouts.open("g/a", true);
        layouts.open("g/b", true);
        let documents = layouts.documents();
        assert_eq!(documents.len(), 2);
        // Another mode has its own panes, and the same documents.
        layouts.switch_mode(1);
        assert_eq!(layouts.documents(), documents);
        assert!(layouts.find(Pane::first(PaneKind::Contents)).is_none());
        assert_eq!(layouts.subject(documents[1]), Some("g/b"));
        // What is changed in it stays with it.
        layouts.hide(Pane::first(PaneKind::Inspector));
        layouts.switch_mode(0);
        assert!(layouts.is_visible(Pane::first(PaneKind::Contents)));
        layouts.switch_mode(1);
        assert!(!layouts.is_visible(Pane::first(PaneKind::Inspector)));
        // Reset, as it began, the documents kept; a new one is a copy; the last cannot go.
        layouts.reset_mode();
        assert!(layouts.is_visible(Pane::first(PaneKind::Inspector)));
        assert_eq!(layouts.documents(), documents);
        layouts.add_mode("Mine");
        assert_eq!((layouts.modes.len(), layouts.modes[layouts.mode].name.as_str()), (3, "Mine"));
        layouts.delete_mode();
        layouts.delete_mode();
        layouts.delete_mode();
        assert_eq!(layouts.modes.len(), 1);
        assert_eq!(layouts.documents(), documents);
    }

    #[test]
    fn acts_on_tabs() {
        let mut layouts = Layouts::new(default_layout());
        let inspector = Pane::first(PaneKind::Inspector);
        let tile = |l: &Layouts, pane| l.find(pane).unwrap();
        let group = layouts.main.tiles.parent_of(tile(&layouts, inspector).1);
        // Another instance joins its group; pinned, then following again.
        let (window, at) = tile(&layouts, inspector);
        layouts.act(window, at, PaneAction::Another);
        let second = Pane { kind: PaneKind::Inspector, instance: 1 };
        assert_eq!(layouts.main.tiles.parent_of(tile(&layouts, second).1), group);
        let (window, at) = tile(&layouts, second);
        layouts.act(window, at, PaneAction::Pin("g/a".into()));
        assert_eq!(layouts.states[&second].pinned.as_deref(), Some("g/a"));
        // Into a window of its own; the second joins it; the first goes home.
        let (window, at) = tile(&layouts, inspector);
        layouts.act(window, at, PaneAction::ToNewWindow);
        assert_eq!(layouts.windows.len(), 1);
        let id = layouts.windows[0].id;
        assert_eq!(tile(&layouts, inspector).0, Some(id));
        let (window, at) = tile(&layouts, second);
        layouts.act(window, at, PaneAction::ToWindow(Some(id)));
        assert_eq!(tile(&layouts, second).0, Some(id));
        let (window, at) = tile(&layouts, inspector);
        layouts.act(window, at, PaneAction::ToWindow(None));
        assert_eq!(layouts.main.tiles.parent_of(tile(&layouts, inspector).1), group);
        // Closing the second instance removes it and its state; the window, empty, goes.
        let (window, at) = tile(&layouts, second);
        layouts.act(window, at, PaneAction::Close);
        assert!(layouts.find(second).is_none() && !layouts.states.contains_key(&second) && layouts.windows.is_empty());
        // A first instance closed in a window returns hidden; docking a window back returns its panes.
        let (window, at) = tile(&layouts, inspector);
        layouts.act(window, at, PaneAction::ToNewWindow);
        let (window, at) = tile(&layouts, inspector);
        layouts.act(window, at, PaneAction::Close);
        assert!(layouts.find(inspector).unwrap().0.is_none() && !layouts.is_visible(inspector));
        layouts.show(inspector);
        let (window, at) = tile(&layouts, inspector);
        layouts.act(window, at, PaneAction::ToNewWindow);
        layouts.dock_back(layouts.windows[0].id);
        assert!(layouts.windows.is_empty() && layouts.is_visible(inspector));
        assert_eq!(layouts.main.tiles.parent_of(tile(&layouts, inspector).1), group);
    }

    #[test]
    fn an_entry_opens_as_a_document() {
        let mut layouts = Layouts::new(default_layout());
        let first = Pane::first(PaneKind::Document);
        let second = Pane { kind: PaneKind::Document, instance: 1 };
        let front = |l: &Layouts| l.active_document().and_then(|p| l.subject(p).map(str::to_string));
        // The preview tab is replaced; a double click keeps it; the next opens beside it.
        layouts.open("g/a", false);
        layouts.open("g/b", false);
        assert_eq!(layouts.subject(first), Some("g/b"));
        assert!(layouts.find(second).is_none());
        layouts.open("g/b", true);
        layouts.open("g/c", false);
        assert_eq!((layouts.subject(first), layouts.subject(second)), (Some("g/b"), Some("g/c")));
        let group = layouts.main.tiles.parent_of(layouts.find(first).unwrap().1);
        assert_eq!(layouts.main.tiles.parent_of(layouts.find(second).unwrap().1), group);
        assert_eq!(front(&layouts).as_deref(), Some("g/c"));
        // It opens next to the one in use, not after what else is in the group.
        let Some(egui_tiles::Tile::Container(egui_tiles::Container::Tabs(tabs))) =
            layouts.main.tiles.get(group.unwrap())
        else {
            panic!()
        };
        let at = |tile| tabs.children.iter().position(|t| *t == tile).unwrap();
        assert_eq!(at(layouts.find(second).unwrap().1), at(layouts.find(first).unwrap().1) + 1);
        // One open comes to the front, and the preview tab stays what it was.
        layouts.open("g/b", false);
        assert_eq!(front(&layouts).as_deref(), Some("g/b"));
        assert!(!layouts.is_preview(first) && layouts.subject(second) == Some("g/c"));
        // A tab keeps its editor while it shows the same; replaced, it starts over; another of it has the same.
        layouts.open("g/c", false);
        layouts.show_in(EditorKind::Graph);
        layouts.open("g/c", false);
        assert_eq!(layouts.states[&second].editor, Some(EditorKind::Graph));
        let (window, at) = layouts.find(second).unwrap();
        layouts.act(window, at, PaneAction::Another);
        let copy = Pane { kind: PaneKind::Document, instance: 2 };
        let Some(egui_tiles::Tile::Container(egui_tiles::Container::Tabs(tabs))) =
            layouts.main.tiles.get(group.unwrap())
        else {
            panic!()
        };
        let order = |pane| tabs.children.iter().position(|t| *t == layouts.find(pane).unwrap().1).unwrap();
        assert_eq!(order(copy), order(second) + 1);
        assert_eq!((layouts.states[&copy].editor, layouts.subject(copy)), (Some(EditorKind::Graph), Some("g/c")));
        layouts.open("g/x", false);
        assert_eq!(layouts.states[&second].editor, None);
        let (window, at) = layouts.find(copy).unwrap();
        layouts.act(window, at, PaneAction::Close);
        layouts.open("g/c", false);
        // An edit keeps it.
        layouts.keep("g/c");
        layouts.open("g/d", false);
        let third = Pane { kind: PaneKind::Document, instance: 2 };
        assert_eq!(layouts.subject(third), Some("g/d"));
        // The next and the one before go round the group, and a page in front closes as a tab does.
        let order = layouts.active_document().unwrap();
        layouts.next_tab(true);
        assert_ne!(layouts.active_document(), Some(order));
        layouts.next_tab(false);
        assert_eq!(layouts.active_document(), Some(order));
        layouts.show(Pane::first(PaneKind::Extensions));
        assert!(layouts.is_showing(Pane::first(PaneKind::Extensions)));
        layouts.close_tab();
        assert!(!layouts.is_visible(Pane::first(PaneKind::Extensions)));
        assert_eq!(layouts.documents().len(), 3);
        // Closing goes, the first as any other; what is left is in front; the last stays, empty, and opens the next.
        let (window, at) = layouts.find(first).unwrap();
        layouts.act(window, at, PaneAction::Close);
        assert!(layouts.find(first).is_none());
        layouts.close_documents(|path| path == "g/d");
        assert_eq!(front(&layouts).as_deref(), Some("g/c"));
        layouts.close_documents(|_| true);
        let last = layouts.documents();
        assert_eq!(last.len(), 1);
        assert_eq!(layouts.subject(last[0]), None);
        layouts.open("g/e", false);
        assert_eq!(layouts.main.tiles.parent_of(layouts.find(last[0]).unwrap().1), group);
        assert_eq!(front(&layouts).as_deref(), Some("g/e"));
    }

    #[test]
    fn moves_panes_between_layouts() {
        let mut main = default_layout();
        let inspector = Pane::first(PaneKind::Inspector);
        let tile = tile_of(&main, inspector).unwrap();
        // Out of the main layout into a window of its own, remembering its group.
        let (pane, group) = take(&mut main, tile).unwrap();
        assert_eq!(pane, inspector);
        assert!(tile_of(&main, inspector).is_none());
        let mut window = Window::new(1, pane);
        assert_eq!(window.title(), "Inspector");
        // A second instance joins it there.
        let second = Pane { kind: PaneKind::Inspector, instance: 1 };
        put(&mut window.tree, second, None, None);
        assert_eq!(panes_of(&window.tree).len(), 2);
        // Back into its group in the main layout.
        let tile = tile_of(&window.tree, inspector).unwrap();
        let (pane, _) = take(&mut window.tree, tile).unwrap();
        let back = put(&mut main, pane, group, None);
        assert_eq!(main.tiles.parent_of(back), group);
        // A window survives being saved and read back.
        let text = serde_json::to_string(&vec![&window]).unwrap();
        let read: Vec<Window> = serde_json::from_str(&text).unwrap();
        assert_eq!(panes_of(&read[0].tree), panes_of(&window.tree));
        if std::env::var_os("PE2_PRINT_WINDOW").is_some() {
            println!("{text}");
        }
    }
}
