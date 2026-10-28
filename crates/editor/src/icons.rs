//! Icons: Phosphor's regular font (`assets/Phosphor.ttf`, MIT, `assets/Phosphor-LICENSE`), installed as a fallback
//! of both font families, so an icon is a character that sits in any label. Codepoints from the font's own table.

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily};
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
pub const CARET_RIGHT: &str = "\u{E13A}";
pub const MAGNIFYING_GLASS: &str = "\u{E30C}";
pub const HARD_DRIVES: &str = "\u{E2A0}";
pub const GEAR: &str = "\u{E270}";
pub const ARROW_SQUARE_OUT: &str = "\u{E5DE}";
pub const WARNING: &str = "\u{E4E0}";

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

/// An asset kind's icon and its tint.
pub fn for_kind(kind: Option<&str>) -> (&'static str, Color32) {
    const STRUCTURE: Color32 = Color32::from_rgb(0x8f, 0xa8, 0xc8);
    const PICTURE: Color32 = Color32::from_rgb(0x6f, 0xc8, 0xb0);
    const MOTION: Color32 = Color32::from_rgb(0xb0, 0x93, 0xe0);
    const SOUND: Color32 = Color32::from_rgb(0xe6, 0xa8, 0x5c);
    const CODE: Color32 = Color32::from_rgb(0xe0, 0x84, 0x84);
    const PLAIN: Color32 = Color32::from_rgb(0x9a, 0xa3, 0xaf);
    match kind {
        Some("disc") => (DISC, STRUCTURE),
        Some("stage") => (STACK, STRUCTURE),
        Some("folder") => (FOLDERS, STRUCTURE),
        Some("file_group") => (SQUARES_FOUR, STRUCTURE),
        Some("file") => (FILE, STRUCTURE),
        Some("image") => (IMAGE, PICTURE),
        Some("vram_slot") => (SQUARES_FOUR, PICTURE),
        Some("palette") => (PALETTE, PICTURE),
        Some("movie" | "stream") => (FILM_STRIP, MOTION),
        Some("scene_audio") => (WAVEFORM, SOUND),
        Some("sound_bank") => (SPEAKER_HIGH, SOUND),
        Some("sequence") => (MUSIC_NOTES, SOUND),
        Some("executable") => (TERMINAL_WINDOW, CODE),
        Some("package") => (PACKAGE, CODE),
        Some("text") => (FILE_TEXT, PLAIN),
        Some("binary") => (BINARY, PLAIN),
        _ => (FILE_DASHED, PLAIN),
    }
}
