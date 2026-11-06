//! Icons: Phosphor's regular font (`assets/Phosphor.ttf`, MIT, `assets/Phosphor-LICENSE`), installed as a fallback
//! of both font families, so an icon is a character that sits in any label. Codepoints from the font's own table.

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily};
use pe2_ext::{Icon, Tint};
use project::ext::Extensions;
use std::sync::Arc;

/// The icons the editor uses, each a constant, and all of them by name (`ICONS`) for what reads them back.
macro_rules! icons {
    ($($name:ident = $glyph:literal,)*) => {
        $(pub const $name: &str = $glyph;)*
        pub const ICONS: &[(&str, &str)] = &[$((stringify!($name), $name),)*];
    };
}

icons! {
    FOLDER = "\u{E24A}",
    ARROW_ARC_LEFT = "\u{E014}",
    ARROW_ARC_RIGHT = "\u{E016}",
    FOLDER_OPEN = "\u{E256}",
    FOLDERS = "\u{E260}",
    SQUARES_FOUR = "\u{E464}",
    FILE = "\u{E230}",
    FILE_TEXT = "\u{E23A}",
    FILE_DASHED = "\u{E704}",
    IMAGE = "\u{E2CA}",
    PALETTE = "\u{E6C8}",
    FILM_STRIP = "\u{E792}",
    WAVEFORM = "\u{E802}",
    SPEAKER_HIGH = "\u{E44A}",
    PLAY = "\u{E3D0}",
    PAUSE = "\u{E39E}",
    SPEAKER_X = "\u{E45C}",
    MUSIC_NOTES = "\u{E340}",
    DISC = "\u{E564}",
    STACK = "\u{E466}",
    SLIDERS_HORIZONTAL = "\u{E434}",
    LIST_BULLETS = "\u{E2F2}",
    LINK_SIMPLE = "\u{E2E6}",
    TERMINAL_WINDOW = "\u{EAE8}",
    BINARY = "\u{EE60}",
    PACKAGE = "\u{E390}",
    CUBE = "\u{E1DA}",
    BONE = "\u{E7F2}",
    POLYGON = "\u{E6D0}",
    CARET_LEFT = "\u{E138}",
    CARET_UP = "\u{E13C}",
    CARET_DOWN = "\u{E136}",
    X = "\u{E4F6}",
    CARET_RIGHT = "\u{E13A}",
    MAGNIFYING_GLASS = "\u{E30C}",
    HARD_DRIVES = "\u{E2A0}",
    GEAR = "\u{E270}",
    ARROW_SQUARE_OUT = "\u{E5DE}",
    WARNING = "\u{E4E0}",
    EYE = "\u{E220}",
    EYE_SLASH = "\u{E224}",
    LOCK_SIMPLE = "\u{E308}",
    LOCK_SIMPLE_OPEN = "\u{E30A}",
    DOTS_SIX_VERTICAL = "\u{EAE2}",
    PLUS = "\u{E3D4}",
    ARROW_UP = "\u{E08E}",
    ARROW_DOWN = "\u{E03E}",
    DOTS_THREE = "\u{E1FE}",
    TRASH = "\u{E4A6}",
    TREE_STRUCTURE = "\u{E67C}",
    PLUGS_CONNECTED = "\u{EB5A}",
    STACK_SIMPLE = "\u{E468}",
    GIT_DIFF = "\u{E27C}",
    PUSH_PIN = "\u{E3E2}",
    PENCIL_SIMPLE = "\u{E3B4}",
    CLOCK_COUNTER_CLOCKWISE = "\u{E1A0}",
    APP_WINDOW = "\u{E5DA}",
    ARROW_SQUARE_IN = "\u{E5DC}",
    CHECK_CIRCLE = "\u{E184}",
    X_CIRCLE = "\u{E4F8}",
}

/// Text with the icons in it named (`[caret down]`), for reading it outside the window.
pub fn spelled(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match ICONS.iter().find(|(_, glyph)| glyph.starts_with(c) && glyph.len() == c.len_utf8()) {
            Some((name, _)) => {
                out.push('[');
                out.push_str(&name.to_lowercase().replace('_', " "));
                out.push(']');
            }
            None => out.push(c),
        }
    }
    out
}

/// Add the icon font behind the text fonts.
pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts
        .font_data
        .insert("phosphor".into(), Arc::new(FontData::from_static(include_bytes!("../assets/Phosphor.ttf"))));
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        fonts.families.entry(family).or_default().push("phosphor".into());
    }
    ctx.set_fonts(fonts);
}

const STRUCTURE: Color32 = Color32::from_rgb(0x8f, 0xa8, 0xc8);
const PICTURE: Color32 = Color32::from_rgb(0x6f, 0xc8, 0xb0);
const MOTION: Color32 = Color32::from_rgb(0xb0, 0x93, 0xe0);
const SOUND: Color32 = Color32::from_rgb(0xe6, 0xa8, 0x5c);
const CODE: Color32 = Color32::from_rgb(0xe0, 0x84, 0x84);
const PLAIN: Color32 = Color32::from_rgb(0x9a, 0xa3, 0xaf);

/// An extension's icon, from the host's set.
pub fn glyph(icon: Icon) -> &'static str {
    match icon {
        Icon::Disc => DISC,
        Icon::Stack => STACK,
        Icon::Image => IMAGE,
        Icon::Palette => PALETTE,
        Icon::Grid => SQUARES_FOUR,
        Icon::Film => FILM_STRIP,
        Icon::Waveform => WAVEFORM,
        Icon::Speaker => SPEAKER_HIGH,
        Icon::Music => MUSIC_NOTES,
        Icon::Package => PACKAGE,
        Icon::Cube => CUBE,
        Icon::Bone => BONE,
        Icon::Polygon => POLYGON,
        Icon::Terminal => TERMINAL_WINDOW,
        Icon::Text => FILE_TEXT,
        Icon::Binary => BINARY,
        Icon::File => FILE,
        Icon::Folder => FOLDERS,
        Icon::CaretUp => CARET_UP,
        Icon::CaretDown => CARET_DOWN,
        Icon::Cross => X,
        Icon::Plus => PLUS,
    }
}

pub fn tint(tint: Tint) -> Color32 {
    match tint {
        Tint::Structure => STRUCTURE,
        Tint::Picture => PICTURE,
        Tint::Motion => MOTION,
        Tint::Sound => SOUND,
        Tint::Code => CODE,
        Tint::Plain => PLAIN,
    }
}

/// An asset type's icon and its tint, as its extension registers it; a type no extension registers, dashed.
pub fn for_type(extensions: &Extensions, node_type: Option<&str>) -> (&'static str, Color32) {
    match node_type.and_then(|k| extensions.icon(k)) {
        Some((icon, family)) => (glyph(icon), tint(family)),
        None => (FILE_DASHED, PLAIN),
    }
}
