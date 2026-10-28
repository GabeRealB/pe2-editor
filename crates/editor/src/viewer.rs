//! The file viewer: what the Preview pane shows of a file no preview is registered for - a plain file, a bundle's file
//! that is not a picture. A file is text when it is UTF-8 without a NUL byte (only UTF-8: other encodings are read as
//! binary); text is shown with line numbers, highlighted by its extension where the highlighter knows the language,
//! binary as a hex dump. Both are drawn a line at a time, only the lines in view, so a large file costs no more to
//! show than a small one. A file is read again when it changes on disk.

use eframe::egui::{self, RichText};
use egui_extras::syntax_highlighting::{self, CodeTheme};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::theme;

/// The most of a file read: past it, the viewer says the rest is not shown.
const LIMIT: usize = 64 << 20;
/// The largest text highlighted: larger text is shown plain.
const HIGHLIGHT_LIMIT: usize = 2 << 20;
/// Bytes a hex dump row shows.
const ROW: usize = 16;

/// A file as read for viewing.
pub struct Viewed {
    stamp: Option<(u64, SystemTime)>,
    content: Content,
    /// Its size, where only the first `LIMIT` bytes were read.
    truncated: Option<u64>,
    /// The highlighter's language for its extension.
    language: Option<&'static str>,
    /// The text's lines, highlighted, for a theme (dark or light): made on the first drawing.
    lines: Option<(bool, Vec<egui::text::LayoutJob>)>,
}

enum Content {
    Text(String),
    Binary(Vec<u8>),
    Error(String),
}

/// The highlighter's language for a file's extension, where it knows one.
fn language(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match extension.as_str() {
        "c" | "h" | "inc" | "cpp" | "hpp" => "c",
        "py" => "py",
        "rs" => "rs",
        "toml" => "toml",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "sh" | "bash" => "sh",
        "js" | "ts" => "js",
        "sql" => "sql",
        _ => return None,
    })
}

fn stamp(path: &Path) -> Option<(u64, SystemTime)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

impl Viewed {
    fn read(path: &Path) -> Viewed {
        let stamp = stamp(path);
        let mut truncated = None;
        let content = match std::fs::File::open(path) {
            Ok(file) => {
                use std::io::Read;
                let mut bytes = Vec::new();
                match file.take(LIMIT as u64).read_to_end(&mut bytes) {
                    Ok(_) => {
                        if let Some((size, _)) = stamp.filter(|(size, _)| *size > LIMIT as u64) {
                            truncated = Some(size);
                        }
                        // A cut may fall inside a character: what is before it decides.
                        let text = match std::str::from_utf8(&bytes) {
                            Ok(text) => Some(text),
                            Err(e) if truncated.is_some() && e.error_len().is_none() => {
                                std::str::from_utf8(&bytes[..e.valid_up_to()]).ok()
                            }
                            Err(_) => None,
                        };
                        match text.filter(|t| !t.contains('\0')) {
                            Some(text) => Content::Text(text.to_string()),
                            None => Content::Binary(bytes),
                        }
                    }
                    Err(e) => Content::Error(e.to_string()),
                }
            }
            Err(e) => Content::Error(e.to_string()),
        };
        Viewed { stamp, content, truncated, language: language(path), lines: None }
    }

    /// The text's lines, highlighted where the language is known and the text not too large: the whole text at once,
    /// so a comment over several lines is colored as one, then cut at its lines.
    fn lines(&mut self, ui: &egui::Ui) -> &[egui::text::LayoutJob] {
        let dark = ui.visuals().dark_mode;
        if self.lines.as_ref().is_none_or(|(d, _)| *d != dark) {
            let Content::Text(text) = &self.content else { return &[] };
            let font = egui::TextStyle::Monospace.resolve(ui.style());
            let whole = match self.language.filter(|_| text.len() <= HIGHLIGHT_LIMIT) {
                Some(language) => {
                    let theme = CodeTheme::from_style(ui.style());
                    syntax_highlighting::highlight(ui.ctx(), ui.style(), &theme, text, language)
                }
                None => egui::text::LayoutJob::simple(text.clone(), font.clone(), theme::text(), f32::INFINITY),
            };
            self.lines = Some((dark, split_lines(&whole)));
        }
        self.lines.as_ref().map_or(&[], |(_, lines)| lines.as_slice())
    }
}

/// A layout job cut at its line breaks, one job a line, each keeping its sections' formats.
fn split_lines(whole: &egui::text::LayoutJob) -> Vec<egui::text::LayoutJob> {
    let text = whole.text.as_str();
    let mut lines = Vec::new();
    let mut start = 0;
    for end in text.match_indices('\n').map(|(i, _)| i).chain(std::iter::once(text.len())) {
        let mut job = egui::text::LayoutJob { wrap: egui::text::TextWrapping::no_max_width(), ..Default::default() };
        for section in &whole.sections {
            let (from, to) = (section.byte_range.start.0.max(start), section.byte_range.end.0.min(end));
            if from < to {
                job.append(&text[from..to], 0.0, section.format.clone());
            }
        }
        lines.push(job);
        start = end + 1;
        if start > text.len() {
            break;
        }
    }
    // A last line break ends the last line: no empty line after it.
    if text.ends_with('\n') {
        lines.pop();
    }
    lines
}

/// The files viewed so far, read again when they change on disk.
#[derive(Default)]
pub struct Viewer {
    files: std::collections::HashMap<PathBuf, Viewed>,
}

impl Viewer {
    /// A file's view.
    fn get(&mut self, path: &Path) -> &mut Viewed {
        let now = stamp(path);
        let stale = self.files.get(path).is_none_or(|v| v.stamp != now);
        if stale {
            self.files.insert(path.to_path_buf(), Viewed::read(path));
        }
        self.files.get_mut(path).expect("just read")
    }

    /// Show a file: its text, or its bytes.
    pub fn ui(&mut self, ui: &mut egui::Ui, path: &Path) {
        let viewed = self.get(path);
        let summary = match &viewed.content {
            Content::Text(text) => {
                let lines = text.lines().count();
                let language = viewed.language.map_or_else(String::new, |l| format!("  ·  {l}"));
                format!("text  ·  {lines} line{}{language}", if lines == 1 { "" } else { "s" })
            }
            Content::Binary(bytes) => format!("binary  ·  {} bytes", bytes.len()),
            Content::Error(e) => e.clone(),
        };
        ui.label(RichText::new(summary).small().color(theme::weak()));
        if let Some(size) = viewed.truncated {
            let text = format!("the first {} MB of {size} bytes", LIMIT >> 20);
            ui.label(RichText::new(text).small().color(ui.visuals().warn_fg_color));
        }
        ui.add_space(4.0);
        let row = ui.text_style_height(&egui::TextStyle::Monospace);
        let gutter = egui::TextStyle::Monospace.resolve(ui.style());
        match &viewed.content {
            Content::Text(_) => {
                let lines = viewed.lines(ui).to_vec();
                let digits = lines.len().to_string().len();
                egui::ScrollArea::both().id_salt(("viewer", path)).auto_shrink(false).show_rows(
                    ui,
                    row,
                    lines.len(),
                    |ui, range| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for (i, line) in lines[range.clone()].iter().enumerate() {
                            ui.horizontal(|ui| {
                                let number = format!("{:>digits$}  ", range.start + i + 1);
                                let weak = theme::weak().gamma_multiply(0.7);
                                ui.label(egui::RichText::new(number).font(gutter.clone()).color(weak));
                                ui.add(egui::Label::new(line.clone()).extend());
                            });
                        }
                    },
                );
            }
            Content::Binary(bytes) => {
                let rows = bytes.len().div_ceil(ROW);
                egui::ScrollArea::both().id_salt(("viewer", path)).auto_shrink(false).show_rows(
                    ui,
                    row,
                    rows,
                    |ui, range| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for r in range {
                            let chunk = &bytes[r * ROW..((r + 1) * ROW).min(bytes.len())];
                            ui.add(egui::Label::new(RichText::new(hex_row(r * ROW, chunk)).monospace()).extend());
                        }
                    },
                );
            }
            Content::Error(_) => {}
        }
    }
}

/// A hex dump's row: the offset, the bytes, and the bytes that are printable ASCII.
fn hex_row(offset: usize, bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut row = format!("{offset:08x}  ");
    for i in 0..ROW {
        match bytes.get(i) {
            Some(b) => {
                let _ = write!(row, "{b:02x} ");
            }
            None => row.push_str("   "),
        }
        if i == 7 {
            row.push(' ');
        }
    }
    row.push(' ');
    row.extend(bytes.iter().map(|&b| if b.is_ascii_graphic() || b == b' ' { b as char } else { '.' }));
    row
}

#[cfg(test)]
mod tests {
    use super::{Content, Viewed, hex_row};

    #[test]
    fn tells_text_from_binary() {
        let dir = std::env::temp_dir().join(format!("pe2-viewer-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, bytes: &[u8]| {
            let path = dir.join(name);
            std::fs::write(&path, bytes).unwrap();
            Viewed::read(&path)
        };
        assert!(matches!(write("a.c", "int main; /* é */\n".as_bytes()).content, Content::Text(_)));
        assert!(matches!(write("b.bin", b"ab\0cd").content, Content::Binary(_)));
        assert!(matches!(write("c.txt", &[0x66, 0xff, 0x66]).content, Content::Binary(_)));
        assert_eq!(write("a.c", b"x").language, Some("c"));
        std::fs::remove_dir_all(&dir).unwrap();
        let row = hex_row(16, b"AB\0");
        assert!(row.starts_with("00000010  41 42 00 ") && row.ends_with("  AB."), "{row}");
    }
}
