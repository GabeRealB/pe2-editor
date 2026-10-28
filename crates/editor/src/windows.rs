//! Windows of their own for panes. A pane is moved out of the main window into a native window - egui's immediate
//! viewports, which share the editor's state rather than a copy of it; where the platform cannot open one, egui draws
//! it as a window inside the main one - and each window holds a layout of its own, so panes dock together there too.
//! Closing a window, or its dock-back button, returns its panes to the main window, where they came from.

use eframe::egui;
use egui_tiles::{Container, Tile, TileId, Tree};
use serde::{Deserialize, Serialize};

use std::collections::BTreeMap;

use crate::panes::{Pane, PaneAction, PaneKind, PaneState};

#[derive(Serialize, Deserialize)]
pub struct Window {
    pub id: u64,
    pub tree: Tree<Pane>,
    /// Where it was, and how large, in points: it opens there again.
    #[serde(default)]
    pub position: Option<[f32; 2]>,
    #[serde(default)]
    pub size: Option<[f32; 2]>,
    /// The place and size it opened with this run. Given once: fed back every frame, a position the platform reports
    /// a decoration away from where it places the window (X11 under `WSLg`) walks the window off to the top left.
    #[serde(skip)]
    opened: Option<([f32; 2], Option<[f32; 2]>)>,
    /// Where the platform put its contents relative to the position it was asked for, measured once it is shown:
    /// what a position read back is corrected by, so that the same request places it there again.
    #[serde(skip)]
    offset: Option<egui::Vec2>,
}

impl Window {
    pub fn new(id: u64, pane: Pane) -> Window {
        let tree = Tree::new_tabs(egui::Id::unique(("pe2 window", id)), vec![pane]);
        Window { id, tree, position: None, size: None, opened: None, offset: None }
    }

    pub fn viewport(&self) -> egui::ViewportId {
        egui::ViewportId::from_hash_of(("pe2 window", self.id))
    }

    /// Its title: the panes it shows.
    pub fn title(&self) -> String {
        let names: Vec<&str> = panes_of(&self.tree)
            .into_iter()
            .filter(|(id, _)| self.tree.tiles.is_visible(*id))
            .map(|(_, pane)| pane.kind.title())
            .collect();
        if names.is_empty() { "Window".to_string() } else { names.join(", ") }
    }

    /// Its viewport: the title it has now, the place and size it opened with.
    pub fn builder(&mut self) -> egui::ViewportBuilder {
        let (size, position) = *self.opened.get_or_insert((self.size.unwrap_or([560.0, 640.0]), self.position));
        let mut builder =
            egui::ViewportBuilder::default().with_title(format!("{} - PE2 Editor", self.title())).with_inner_size(size);
        if let Some(position) = position {
            builder = builder.with_position(position);
        }
        builder
    }

    /// Keep where the window is and its size, to open it there again. The contents' position is what the platform
    /// reports reliably; the outer position may not be (X11 under `WSLg` reports it 32 points off). So the place kept
    /// is the contents' position less where they landed relative to the position asked for, measured the first time
    /// the window is shown; a window opened without a position keeps the outer one this once.
    pub fn remember_place(&mut self, ctx: &egui::Context) {
        ctx.input(|i| {
            let viewport = i.viewport();
            // Before it is shown, a window is far off every screen.
            let Some(inner) = viewport.inner_rect.filter(|r| r.min.x > -10_000.0 && r.min.y > -10_000.0) else {
                return;
            };
            self.size = Some([inner.width(), inner.height()]);
            let asked = self.opened.and_then(|(_, position)| position).map(egui::Pos2::from);
            if self.offset.is_none() {
                self.offset = asked.map(|asked| inner.min - asked);
            }
            let position = match self.offset {
                Some(offset) => Some(inner.min - offset),
                None => viewport.outer_rect.map(|r| r.min),
            };
            self.position = position.map(|p| [p.x, p.y]);
        });
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
}

impl Layouts {
    pub fn new(main: Tree<Pane>) -> Layouts {
        Layouts { main, windows: vec![], states: BTreeMap::new(), homes: BTreeMap::new() }
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
            put(&mut self.main, pane, None, Some(Pane::first(PaneKind::Preview)));
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

    /// Do what a tab asked: in the layout of `window` (none: the main one), for the pane in `tile`.
    pub fn act(&mut self, window: Option<u64>, tile: TileId, action: PaneAction) {
        let Some(tree) = self.tree_mut(window) else { return };
        let Some(Tile::Pane(pane)) = tree.tiles.get(tile).cloned() else { return };
        match action {
            PaneAction::Another => {
                let another = Pane { kind: pane.kind, instance: self.next_instance(pane.kind) };
                if let Some(tree) = self.tree_mut(window) {
                    let group = tree.tiles.parent_of(tile);
                    put(tree, another, group, None);
                }
            }
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
            PaneAction::Close if pane.instance > 0 => {
                self.take(window, tile);
                self.states.remove(&pane);
                self.homes.remove(&pane);
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
        let preview = Some(Pane::first(PaneKind::Preview));
        if let Some(target) = window.and_then(|id| self.windows.iter_mut().find(|w| w.id == id)) {
            put(&mut target.tree, pane, None, None)
        } else {
            let home = self.homes.remove(&pane);
            put(&mut self.main, pane, home, preview)
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
    use super::{Layouts, Window, panes_of, put, take, tile_of};
    use crate::panes::{Pane, PaneAction, PaneKind, default_layout};

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
