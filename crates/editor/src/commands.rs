//! Commands: what the editor can be told to do, each one thing with one name. A menu item, a shortcut, a mouse button and the
//! palette all run the same command, so what is bound to one shows wherever it is offered, and can be bound differently in
//! the settings (`[shortcuts]`, by a command's id: a list of bindings, an empty one leaving it unbound).

use eframe::egui::{self, RichText};

use crate::panes::{EditorKind, PaneKind, PanelName};
use crate::settings::EditorSettings;
use crate::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Palette,
    OpenProject,
    Import,
    ReloadProject,
    Quit,
    Settings,
    Undo,
    Redo,
    Refresh,
    DeleteSelected,
    Back,
    Forward,
    Up,
    CloseTab,
    KeepTab,
    NextTab,
    PreviousTab,
    ShowIn(EditorKind),
    GraphFit,
    GraphArrange,
    DocumentsOnly,
    Maximize,
    Fullscreen,
    ResetLayout,
    NewMode,
    ResetMode,
    DeleteMode,
    ZoomIn,
    ZoomOut,
    ActualSize,
    Pane(PaneKind),
    /// The mode at a place in the list.
    Mode(usize),
    /// An extension's, by the tool it runs.
    Extension(PanelName),
}

impl Command {
    /// Every command but the modes, which are the user's: a list that changes.
    pub fn all() -> Vec<Command> {
        use Command::{
            ActualSize, Back, CloseTab, DeleteMode, DeleteSelected, DocumentsOnly, Forward, Fullscreen, GraphArrange, GraphFit,
            Import, KeepTab, Maximize, NewMode, NextTab, OpenProject, Palette, PreviousTab, Quit, Redo, Refresh, ReloadProject,
            ResetLayout, ResetMode, Settings, ShowIn, Undo, Up, ZoomIn, ZoomOut,
        };
        let fixed = [
            Palette, OpenProject, Import, ReloadProject, Quit, Settings, Undo, Redo, Refresh, DeleteSelected, Back, Forward, Up,
            CloseTab, KeepTab, NextTab, PreviousTab, ShowIn(EditorKind::Preview), ShowIn(EditorKind::Files),
            ShowIn(EditorKind::Graph), GraphFit, GraphArrange, DocumentsOnly, Maximize, Fullscreen, ResetLayout, NewMode,
            ResetMode, DeleteMode, ZoomIn, ZoomOut, ActualSize,
        ];
        // Settings is a command of its own, and a document is opened, not shown.
        let shown = |kind: &PaneKind| !matches!(kind, PaneKind::Document | PaneKind::Settings);
        let panes = PaneKind::with_panels().into_iter().filter(shown).map(Command::Pane);
        let registered = extensions::shared();
        let editors = registered.editor_names().into_iter().map(|name| ShowIn(EditorKind::Extension(PanelName::new(name))));
        let offered = extensions::shared().commands().iter().map(|c| Command::Extension(PanelName::new(&c.tool.to_string()))).collect::<Vec<_>>();
        fixed.into_iter().chain(editors).chain(panes).chain(offered).collect()
    }

    /// What it goes by in the settings.
    pub fn id(self) -> String {
        match self {
            Command::Palette => "palette".to_string(),
            Command::OpenProject => "file.open".to_string(),
            Command::Import => "file.import".to_string(),
            Command::ReloadProject => "file.reload".to_string(),
            Command::Quit => "file.quit".to_string(),
            Command::Settings => "edit.settings".to_string(),
            Command::Undo => "edit.undo".to_string(),
            Command::Redo => "edit.redo".to_string(),
            Command::Refresh => "view.refresh".to_string(),
            Command::DeleteSelected => "edit.delete".to_string(),
            Command::GraphFit => "graph.fit".to_string(),
            Command::GraphArrange => "graph.arrange".to_string(),
            Command::Maximize => "window.maximize".to_string(),
            Command::Fullscreen => "window.fullscreen".to_string(),
            Command::Extension(tool) => format!("ext.{}", tool.as_str()),
            Command::Back => "go.back".to_string(),
            Command::Forward => "go.forward".to_string(),
            Command::Up => "go.up".to_string(),
            Command::CloseTab => "tab.close".to_string(),
            Command::KeepTab => "tab.keep".to_string(),
            Command::NextTab => "tab.next".to_string(),
            Command::PreviousTab => "tab.previous".to_string(),
            Command::ShowIn(editor) => format!("tab.show.{}", editor.title().to_lowercase()),
            Command::DocumentsOnly => "window.documents_only".to_string(),
            Command::ResetLayout => "window.reset_layout".to_string(),
            Command::NewMode => "mode.new".to_string(),
            Command::ResetMode => "mode.reset".to_string(),
            Command::DeleteMode => "mode.delete".to_string(),
            Command::ZoomIn => "view.zoom_in".to_string(),
            Command::ZoomOut => "view.zoom_out".to_string(),
            Command::ActualSize => "view.actual_size".to_string(),
            Command::Pane(kind) => format!("pane.{}", kind.title().to_lowercase().replace(' ', "_")),
            Command::Mode(at) => format!("mode.{at}"),
        }
    }

    /// Its name, in the palette and the settings.
    pub fn title(self) -> String {
        match self {
            Command::Palette => "Commands".to_string(),
            Command::OpenProject => "Open project…".to_string(),
            Command::Import => "Import files…".to_string(),
            Command::ReloadProject => "Reload project".to_string(),
            Command::Quit => "Quit".to_string(),
            Command::Settings => "Settings".to_string(),
            Command::Undo => "Undo".to_string(),
            Command::Redo => "Redo".to_string(),
            Command::Refresh => "Refresh".to_string(),
            Command::DeleteSelected => "Delete selected".to_string(),
            Command::GraphFit => "Graph: Fit".to_string(),
            Command::GraphArrange => "Graph: Arrange".to_string(),
            Command::Maximize => "Maximize window".to_string(),
            Command::Fullscreen => "Full screen".to_string(),
            Command::Extension(tool) => extensions::shared()
                .commands()
                .iter()
                .find(|c| c.tool.to_string() == tool.as_str())
                .map_or_else(|| tool.as_str().to_string(), |c| c.label.clone()),
            Command::Back => "Back".to_string(),
            Command::Forward => "Forward".to_string(),
            Command::Up => "Up a folder".to_string(),
            Command::CloseTab => "Close tab".to_string(),
            Command::KeepTab => "Keep tab open".to_string(),
            Command::NextTab => "Next tab".to_string(),
            Command::PreviousTab => "Previous tab".to_string(),
            Command::ShowIn(editor) => format!("Show as {}", editor.title()),
            Command::DocumentsOnly => "Documents only".to_string(),
            Command::ResetLayout => "Reset layout".to_string(),
            Command::NewMode => "New mode…".to_string(),
            Command::ResetMode => "Reset this mode".to_string(),
            Command::DeleteMode => "Delete this mode".to_string(),
            Command::ZoomIn => "Zoom in".to_string(),
            Command::ZoomOut => "Zoom out".to_string(),
            Command::ActualSize => "Actual size".to_string(),
            Command::Pane(kind) => format!("Show {}", kind.title()),
            Command::Mode(at) => format!("Mode {at}"),
        }
    }

    /// What it is bound to unless the settings say otherwise.
    fn defaults(self) -> &'static [&'static str] {
        match self {
            Command::Palette => &["ctrl+shift+P", "F1"],
            Command::Undo => &["ctrl+Z"],
            Command::Redo => &["ctrl+shift+Z", "ctrl+Y"],
            Command::Refresh => &["F5"],
            Command::DeleteSelected => &["Delete"],
            Command::GraphFit => &["Home"],
            Command::Fullscreen => &["F11"],
            Command::Back => &["alt+ArrowLeft", "mouse4"],
            Command::Forward => &["alt+ArrowRight", "mouse5"],
            Command::Up => &["alt+ArrowUp"],
            Command::CloseTab => &["ctrl+W"],
            Command::NextTab => &["ctrl+PageDown"],
            Command::PreviousTab => &["ctrl+PageUp"],
            Command::ShowIn(EditorKind::Preview) => &["ctrl+1"],
            Command::ShowIn(EditorKind::Files) => &["ctrl+2"],
            Command::ShowIn(EditorKind::Graph) => &["ctrl+3"],
            Command::DocumentsOnly => &["ctrl+Space"],
            _ => &[],
        }
    }

    /// What it is bound to.
    pub fn bindings(self, settings: &EditorSettings) -> Vec<Binding> {
        match settings.shortcuts.get(&self.id()) {
            Some(specs) => specs.iter().filter_map(|spec| Binding::parse(spec)).collect(),
            None => self.defaults().iter().filter_map(|spec| Binding::parse(spec)).collect(),
        }
    }

    /// What it is bound to, to show: the first of it.
    pub fn keys(self, settings: &EditorSettings) -> String {
        self.bindings(settings).first().copied().map_or_else(String::new, Binding::text)
    }
}

/// What runs a command: a key with its modifiers, or a mouse button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Binding {
    Key(egui::KeyboardShortcut),
    Mouse(egui::PointerButton),
}

impl Binding {
    /// One as the settings write it: `ctrl+shift+P`, `mouse4`.
    pub fn parse(spec: &str) -> Option<Binding> {
        match spec.to_lowercase().as_str() {
            "mouse3" => return Some(Binding::Mouse(egui::PointerButton::Middle)),
            "mouse4" => return Some(Binding::Mouse(egui::PointerButton::Extra1)),
            "mouse5" => return Some(Binding::Mouse(egui::PointerButton::Extra2)),
            _ => {}
        }
        let (key, modifiers) = crate::drive::chord(spec).ok()?;
        Some(Binding::Key(egui::KeyboardShortcut::new(modifiers, key)))
    }

    /// As it is written in the settings.
    fn spec(self) -> String {
        match self {
            Binding::Mouse(egui::PointerButton::Middle) => "mouse3".to_string(),
            Binding::Mouse(egui::PointerButton::Extra2) => "mouse5".to_string(),
            Binding::Mouse(_) => "mouse4".to_string(),
            Binding::Key(shortcut) => {
                let modifiers = shortcut.modifiers;
                let mut spec = String::new();
                for (held, name) in [(modifiers.command || modifiers.ctrl, "ctrl+"), (modifiers.shift, "shift+"), (modifiers.alt, "alt+")] {
                    if held {
                        spec.push_str(name);
                    }
                }
                spec + shortcut.logical_key.name()
            }
        }
    }

    /// As it is shown.
    pub fn text(self) -> String {
        match self {
            Binding::Mouse(egui::PointerButton::Middle) => "Middle mouse".to_string(),
            Binding::Mouse(egui::PointerButton::Extra2) => "Mouse 5".to_string(),
            Binding::Mouse(_) => "Mouse 4".to_string(),
            Binding::Key(shortcut) => {
                let modifiers = shortcut.modifiers;
                let mut text = String::new();
                for (held, name) in [(modifiers.command || modifiers.ctrl, "Ctrl+"), (modifiers.shift, "Shift+"), (modifiers.alt, "Alt+")] {
                    if held {
                        text.push_str(name);
                    }
                }
                text + shortcut.logical_key.name()
            }
        }
    }

    /// The modifiers it takes: the more, the sooner it is tried, as Ctrl+Z is also held in Ctrl+Shift+Z.
    pub fn weight(self) -> u8 {
        match self {
            Binding::Mouse(_) => 0,
            Binding::Key(shortcut) => {
                let modifiers = shortcut.modifiers;
                u8::from(modifiers.command || modifiers.ctrl) + u8::from(modifiers.shift) + u8::from(modifiers.alt)
            }
        }
    }
}

/// Whether to bind it: a mode's place in the list is not one to bind to.
fn binds(command: Command) -> bool {
    !matches!(command, Command::Mode(_))
}

/// How well a command's title answers what was typed: lower first, none where it does not. Each word typed is found in order
/// of how much it looks meant: the start of the title, the start of a word, inside one, or scattered through it.
pub fn rank(title: &str, typed: &str) -> Option<u32> {
    let title = title.to_lowercase();
    let mut total = 0;
    for word in typed.to_lowercase().split_whitespace() {
        total += if title.starts_with(word) {
            0
        } else if title.split_whitespace().any(|w| w.starts_with(word)) {
            1
        } else if title.contains(word) {
            2
        } else if scattered(&title, word) {
            3
        } else {
            return None;
        };
    }
    Some(total)
}

/// Whether a word's letters are in a text, in order.
fn scattered(text: &str, word: &str) -> bool {
    let mut letters = text.chars();
    word.chars().all(|letter| letters.any(|c| c == letter))
}

/// The id a shortcut being taken is kept under: while there is one, keys are not commands.
fn taking() -> egui::Id {
    egui::Id::unique("shortcut being taken")
}

/// Whether the settings are waiting for a key.
pub fn is_taking(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<Option<String>>(taking())).flatten().is_some()
}

/// The binding a press just made, while one is being taken: the press of a key, or of a mouse button but the plain ones.
fn pressed(ctx: &egui::Context) -> Option<Binding> {
    ctx.input(|i| {
        i.events.iter().find_map(|event| match event {
            egui::Event::Key { key, pressed: true, modifiers, .. } => Some(Binding::Key(egui::KeyboardShortcut::new(*modifiers, *key))),
            egui::Event::PointerButton { button, pressed: true, .. }
                if matches!(button, egui::PointerButton::Middle | egui::PointerButton::Extra1 | egui::PointerButton::Extra2) =>
            {
                Some(Binding::Mouse(*button))
            }
            _ => None,
        })
    })
}

/// The settings' page of shortcuts: every command and what runs it, a click taking a new one.
pub fn shortcuts_ui(ui: &mut egui::Ui, settings: &mut EditorSettings) {
    let mut being_taken: Option<String> = ui.data(|d| d.get_temp(taking())).flatten();
    if let Some(id) = &being_taken
        && let Some(binding) = pressed(ui.ctx())
    {
        if binding != Binding::Key(egui::KeyboardShortcut::new(egui::Modifiers::NONE, egui::Key::Escape)) {
            settings.shortcuts.insert(id.clone(), vec![binding.spec()]);
        }
        being_taken = None;
    }
    theme::section(ui, "Shortcuts", |ui| {
        ui.label(RichText::new("Click one and press what should run it. Esc leaves it.").small().color(theme::weak()));
        ui.add_space(4.0);
        egui::Grid::new("shortcuts").num_columns(3).spacing([18.0, 4.0]).show(ui, |ui| {
            for command in Command::all().into_iter().filter(|command| binds(*command)) {
                let id = command.id();
                ui.label(command.title());
                let bound: Vec<Binding> = command.bindings(settings);
                let text = bound.iter().copied().map(Binding::text).collect::<Vec<_>>().join(", ");
                let taken = being_taken.as_ref() == Some(&id);
                let shown = if taken { "Press a key…" } else if text.is_empty() { "None" } else { text.as_str() };
                if ui.add(egui::Button::selectable(taken, RichText::new(shown).monospace())).clicked() {
                    being_taken = Some(id.clone());
                }
                if settings.shortcuts.contains_key(&id) && ui.small_button("Default").clicked() {
                    settings.shortcuts.remove(&id);
                }
                ui.end_row();
            }
        });
    });
    ui.data_mut(|d| d.insert_temp(taking(), being_taken));
}

#[cfg(test)]
mod tests {
    use super::{Binding, Command, rank};
    use crate::settings::EditorSettings;

    #[test]
    fn a_binding_reads_and_writes_the_same() {
        for spec in ["ctrl+shift+P", "F5", "alt+Left", "mouse4", "mouse5"] {
            assert_eq!(Binding::parse(spec).map(Binding::spec).as_deref(), Some(spec));
        }
        assert_eq!(Binding::parse("ctrl+shift+P").unwrap().text(), "Ctrl+Shift+P");
        assert_eq!(Binding::parse("no such key"), None);
    }

    #[test]
    fn the_more_held_is_tried_first() {
        assert!(Binding::parse("ctrl+shift+Z").unwrap().weight() > Binding::parse("ctrl+Z").unwrap().weight());
    }

    #[test]
    fn what_is_typed_finds_a_command_best_first() {
        assert!(rank("Undo", "un") < rank("Reset this mode", "t"));
        assert!(rank("Show Assets", "as").is_some());
        assert_eq!(rank("Show Assets", "zz"), None);
        // Scattered letters still find it, after anything nearer.
        assert!(rank("Documents only", "dcs").is_some());
        assert!(rank("Documents only", "doc") < rank("Documents only", "dcs"));
    }

    #[test]
    fn settings_replace_or_empty_what_a_command_is_bound_to() {
        let mut settings = EditorSettings::default();
        assert_eq!(Command::Back.keys(&settings), "Alt+Left");
        assert_eq!(Command::Back.bindings(&settings).len(), 2);
        settings.shortcuts.insert(Command::Back.id(), vec!["mouse5".to_string()]);
        assert_eq!(Command::Back.keys(&settings), "Mouse 5");
        settings.shortcuts.insert(Command::Back.id(), vec![]);
        assert_eq!(Command::Back.bindings(&settings), []);
    }

    #[test]
    fn every_command_has_an_id_of_its_own() {
        let mut ids: Vec<String> = Command::all().into_iter().map(Command::id).collect();
        let count = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), count);
    }
}
