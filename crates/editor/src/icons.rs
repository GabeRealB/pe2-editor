//! Icons: Phosphor's regular font (`assets/Phosphor.ttf`, MIT, `assets/Phosphor-LICENSE`), installed as a fallback
//! of both font families, so an icon is a character that sits in any label. Codepoints from the font's own table.

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily};
use pe2_ext::{Icon, Tint};
use project::ext::Extensions;
use std::sync::Arc;

pub const FOLDER: &str = "\u{E24A}";
pub const FOLDER_OPEN: &str = "\u{E256}";
pub const FOLDERS: &str = "\u{E260}";
pub const SQUARES_FOUR: &str = "\u{E464}";
pub const FILE: &str = "\u{E230}";
pub const FILE_TEXT: &str = "\u{E23A}";
pub const FILE_DASHED: &str = "\u{E704}";
pub const IMAGE: &str = "\u{E2CA}";
pub const PALETTE: &str = "\u{E6C8}";
pub const FILM_STRIP: &str = "\u{E792}";
pub const WAVEFORM: &str = "\u{E802}";
pub const SPEAKER_HIGH: &str = "\u{E44A}";
pub const MUSIC_NOTES: &str = "\u{E340}";
pub const DISC: &str = "\u{E564}";
pub const STACK: &str = "\u{E466}";
pub const TERMINAL_WINDOW: &str = "\u{EAE8}";
pub const BINARY: &str = "\u{EE60}";
pub const PACKAGE: &str = "\u{E390}";
pub const CARET_LEFT: &str = "\u{E138}";
pub const CARET_UP: &str = "\u{E13C}";
pub const CARET_DOWN: &str = "\u{E136}";
pub const X: &str = "\u{E4F6}";
pub const CARET_RIGHT: &str = "\u{E13A}";
pub const MAGNIFYING_GLASS: &str = "\u{E30C}";
pub const HARD_DRIVES: &str = "\u{E2A0}";
pub const GEAR: &str = "\u{E270}";
pub const ARROW_SQUARE_OUT: &str = "\u{E5DE}";
pub const WARNING: &str = "\u{E4E0}";
pub const EYE: &str = "\u{E220}";
pub const EYE_SLASH: &str = "\u{E224}";
pub const LOCK_SIMPLE: &str = "\u{E308}";
pub const LOCK_SIMPLE_OPEN: &str = "\u{E30A}";
pub const DOTS_SIX_VERTICAL: &str = "\u{EAE2}";
pub const PLUS: &str = "\u{E3D4}";
pub const PLUGS_CONNECTED: &str = "\u{EB5A}";
pub const STACK_SIMPLE: &str = "\u{E468}";
pub const GIT_DIFF: &str = "\u{E27C}";
pub const PUSH_PIN: &str = "\u{E3E2}";
pub const PENCIL_SIMPLE: &str = "\u{E3B4}";
pub const CLOCK_COUNTER_CLOCKWISE: &str = "\u{E1A0}";
pub const APP_WINDOW: &str = "\u{E5DA}";
pub const ARROW_SQUARE_IN: &str = "\u{E5DC}";
pub const CHECK_CIRCLE: &str = "\u{E184}";
pub const X_CIRCLE: &str = "\u{E4F8}";

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

fn tint(tint: Tint) -> Color32 {
    match tint {
        Tint::Structure => STRUCTURE,
        Tint::Picture => PICTURE,
        Tint::Motion => MOTION,
        Tint::Sound => SOUND,
        Tint::Code => CODE,
        Tint::Plain => PLAIN,
    }
}

/// An asset kind's icon and its tint, as its extension registers it; a kind no extension registers, dashed.
pub fn for_kind(extensions: &Extensions, kind: Option<&str>) -> (&'static str, Color32) {
    match kind.and_then(|k| extensions.icon(k)) {
        Some((icon, family)) => (glyph(icon), tint(family)),
        None => (FILE_DASHED, PLAIN),
    }
}
