//! The editor's own settings - global, per user, beside eframe's saved layout - as `settings.toml`, readable and
//! editable by hand: appearance, the external tools files open in, startup. A project's settings are its
//! `project.toml`; the Settings pane shows both.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const APP_ID: &str = "pe2-editor";
const FILE: &str = "settings.toml";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    Dark,
    Light,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    pub theme: Theme,
    /// The accent color, as RGB.
    pub accent: [u8; 3],
    /// A zoom chosen by the user; none follows the monitor.
    pub zoom: Option<f32>,
}

impl Default for Appearance {
    fn default() -> Self {
        Appearance { theme: Theme::Dark, accent: [0x4d, 0x96, 0xd9], zoom: None }
    }
}

/// How a picture is drawn larger than it is: `nearest` repeats each pixel (exact); the others smooth it for viewing -
/// `bilinear` blends neighbors, `sharp` blends only at pixel edges (a nearest prescale then bilinear), `scale2x` and
/// `scale3x` round diagonal edges without blending (EPX / `AdvMAME3x`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Upscale {
    #[default]
    Nearest,
    Bilinear,
    Sharp,
    Scale2x,
    Scale3x,
}

impl Upscale {
    pub const ALL: [Upscale; 5] =
        [Upscale::Nearest, Upscale::Bilinear, Upscale::Sharp, Upscale::Scale2x, Upscale::Scale3x];

    pub fn label(self) -> &'static str {
        match self {
            Upscale::Nearest => "Nearest (exact)",
            Upscale::Bilinear => "Bilinear",
            Upscale::Sharp => "Sharp bilinear",
            Upscale::Scale2x => "Scale2x",
            Upscale::Scale3x => "Scale3x",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preview {
    pub upscale: Upscale,
    /// A texel grid over a picture, its spacing following the zoom.
    pub grid: bool,
    /// The player's volume, 0 to 1 (its amplitude this squared), and whether it is muted.
    pub volume: f32,
    pub muted: bool,
}

impl Default for Preview {
    fn default() -> Self {
        Preview { upscale: Upscale::default(), grid: true, volume: 1.0, muted: false }
    }
}

/// The command each kind of file opens with: `{file}` stands for the file (appended where it is absent); an empty
/// command opens it with the system's default application.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tools {
    pub image: String,
    pub audio: String,
    pub video: String,
    pub text: String,
    pub model: String,
    pub hex: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Startup {
    pub reopen_last_project: bool,
}

impl Default for Startup {
    fn default() -> Self {
        Startup { reopen_last_project: true }
    }
}

/// When changes other programs make to the project's files are taken in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TakeIn {
    /// When the editor regains focus, as Unity's Auto Refresh and Godot's rescan: a tool saving often imports once.
    #[default]
    OnFocus,
    /// As the watcher sees them.
    AsTheyHappen,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Changes {
    pub take_in: TakeIn,
}

/// What agents connected to the project may do in the editor.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Agents {
    /// Click, type and press keys, as the user would. Reading the window is always allowed.
    pub drive: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorSettings {
    pub appearance: Appearance,
    pub preview: Preview,
    pub tools: Tools,
    pub startup: Startup,
    pub agents: Agents,
    /// Changes made outside the editor.
    pub changes: Changes,
    /// What runs a command, by its id, where it is not what it comes with.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub shortcuts: std::collections::BTreeMap<String, Vec<String>>,
    /// Extensions' user settings, by id (`[extensions."<id>"]`): the extensions read them from this file too.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub extensions: std::collections::BTreeMap<String, toml::Table>,
    /// The config chosen for a project, by its folder (state, not a preference): each person's own, never the project's.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub active_configs: std::collections::BTreeMap<String, String>,
    /// The project opened last (state, not a preference).
    pub last_project: Option<PathBuf>,
    /// Where the main window was (state, not a preference).
    pub main_window: Option<WindowState>,
}

/// The main window as it was left: the place it comes back to, whether it filled the screen on top of that.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowState {
    #[serde(flatten)]
    pub place: crate::windows::Place,
    #[serde(default)]
    pub maximized: bool,
    #[serde(default)]
    pub fullscreen: bool,
}

/// Where the settings live: the editor's per-user storage directory.
pub fn path() -> Option<PathBuf> {
    eframe::storage_dir(APP_ID).map(|d| d.join(FILE))
}

impl EditorSettings {
    /// The saved settings, or the defaults; a file that cannot be read is reported, not overwritten silently.
    pub fn load() -> (EditorSettings, Option<String>) {
        let Some(path) = path() else { return (EditorSettings::default(), None) };
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str(&text) {
                Ok(settings) => (settings, None),
                Err(e) => (EditorSettings::default(), Some(format!("{}: {e}", path.display()))),
            },
            Err(_) => (EditorSettings::default(), None),
        }
    }

    /// # Errors
    /// The directory or the file cannot be written.
    pub fn save(&self) -> Result<(), String> {
        let path = path().ok_or("no settings directory on this system")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(
            &path,
            format!("# pe2-editor settings: edited from the editor's Settings pane, or by hand.\n\n{text}"),
        )
        .map_err(|e| format!("{}: {e}", path.display()))
    }
}
