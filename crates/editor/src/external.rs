//! Opening files in other applications: the tool the settings name for the file's type, or the system's default.
//! A form no tool reads is converted first - a BS picture to a PNG in the temporary directory.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::settings::Tools;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Image,
    Audio,
    Video,
    Text,
    Model,
    Hex,
}

impl Category {
    pub const ALL: [Category; 6] =
        [Category::Image, Category::Audio, Category::Video, Category::Text, Category::Model, Category::Hex];

    pub fn label(self) -> &'static str {
        match self {
            Category::Image => "Image editor",
            Category::Audio => "Audio editor",
            Category::Video => "Video player",
            Category::Text => "Text editor",
            Category::Model => "3D modeling",
            Category::Hex => "Hex editor (other files)",
        }
    }

    pub fn examples(self) -> &'static str {
        match self {
            Category::Image => "gimp {file}, krita {file}, aseprite {file}",
            Category::Audio => "audacity {file}",
            Category::Video => "mpv {file}, vlc {file}",
            Category::Text => "code {file}, gedit {file}",
            Category::Model => "blender {file}",
            Category::Hex => "imhex {file}, ghex {file}",
        }
    }

    pub fn command(self, tools: &Tools) -> &str {
        match self {
            Category::Image => &tools.image,
            Category::Audio => &tools.audio,
            Category::Video => &tools.video,
            Category::Text => &tools.text,
            Category::Model => &tools.model,
            Category::Hex => &tools.hex,
        }
    }

    pub fn command_mut(self, tools: &mut Tools) -> &mut String {
        match self {
            Category::Image => &mut tools.image,
            Category::Audio => &mut tools.audio,
            Category::Video => &mut tools.video,
            Category::Text => &mut tools.text,
            Category::Model => &mut tools.model,
            Category::Hex => &mut tools.hex,
        }
    }

    /// The type of tool a media type (`image/openraster`) is edited in, by its top-level type; none for what no tool
    /// of the settings takes.
    pub fn of_media_type(media_type: &str) -> Option<Category> {
        match media_type.split('/').next()? {
            "image" => Some(Category::Image),
            "audio" => Some(Category::Audio),
            "video" => Some(Category::Video),
            "text" => Some(Category::Text),
            "model" => Some(Category::Model),
            _ => None,
        }
    }

    /// The type of tool a file opens in, by its extension.
    pub fn of(file: &Path) -> Category {
        match file.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
            Some("png" | "bs" | "pe2img" | "pe2clut" | "bmp" | "tga") => Category::Image,
            Some("wav" | "sfz" | "spk" | "vag") => Category::Audio,
            Some("str" | "mp4" | "webm" | "mkv") => Category::Video,
            Some("toml" | "txt" | "cnf" | "json" | "md") => Category::Text,
            Some("usd" | "usda" | "usdc" | "glb" | "gltf" | "obj") => Category::Model,
            _ => Category::Hex,
        }
    }
}

/// The file a tool is given: a BS picture becomes a PNG in the temporary directory; any other file is itself.
fn openable(file: &Path, name: &str) -> Result<PathBuf, String> {
    if file.extension().is_none_or(|e| e != "bs") {
        return Ok(file.to_path_buf());
    }
    let bytes = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let picture = codecs::bs::decode(&bytes).map_err(|e| e.to_string())?;
    let out = std::env::temp_dir().join("pe2-editor").join(format!("{}.png", name.replace(['/', '\\'], "_")));
    std::fs::create_dir_all(out.parent().unwrap_or(Path::new("."))).map_err(|e| e.to_string())?;
    let writer = std::io::BufWriter::new(std::fs::File::create(&out).map_err(|e| e.to_string())?);
    let width = u32::try_from(picture.width).map_err(|e| e.to_string())?;
    let height = u32::try_from(picture.height).map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(writer, width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header().and_then(|mut w| w.write_image_data(&picture.rgb)).map_err(|e| e.to_string())?;
    Ok(out)
}

/// A command line split into words, quotes grouping words and removed.
fn words(command: &str) -> Vec<String> {
    let (mut out, mut word, mut quote, mut any) = (Vec::new(), String::new(), None, false);
    for c in command.chars() {
        match (quote, c) {
            (None, '"' | '\'') => (quote, any) = (Some(c), true),
            (Some(q), c) if c == q => quote = None,
            (None, c) if c.is_whitespace() => {
                if any {
                    out.push(std::mem::take(&mut word));
                    any = false;
                }
            }
            (_, c) => {
                word.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(word);
    }
    out
}

/// The system's way of opening a file in its default application.
fn system_default(file: &Path) -> Command {
    if cfg!(target_os = "windows") {
        let mut c = Command::new("cmd");
        c.args(["/C", "start", ""]).arg(file);
        c
    } else if cfg!(target_os = "macos") {
        let mut c = Command::new("open");
        c.arg(file);
        c
    } else {
        // Under WSL, wslview hands the file to Windows' default application.
        let wsl = std::env::var_os("WSL_DISTRO_NAME").is_some();
        let mut c = Command::new(if wsl && which("wslview") { "wslview" } else { "xdg-open" });
        c.arg(file);
        c
    }
}

/// Open a file in the system's default application, as it is.
///
/// # Errors
/// The system's opener cannot be started.
pub fn open_default(file: &Path) -> Result<(), String> {
    system_default(file).spawn().map(drop).map_err(|e| format!("{}: {e}", file.display()))
}

fn which(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| std::env::split_paths(&paths).any(|d| d.join(program).is_file()))
}

/// Open a file (named `name` for whatever it is converted to) with the tool for its type, or the system's default.
///
/// # Errors
/// The file cannot be converted, or the tool cannot be started.
pub fn open(file: &Path, name: &str, tools: &Tools) -> Result<String, String> {
    open_as(file, name, Category::of(file), tools)
}

/// Open a file with the tool for `category`, or the system's default.
///
/// # Errors
/// The file cannot be converted, or the tool cannot be started.
pub fn open_as(file: &Path, name: &str, category: Category, tools: &Tools) -> Result<String, String> {
    let target = openable(file, name)?;
    let command = category.command(tools).trim();
    let mut process = if command.is_empty() {
        system_default(&target)
    } else {
        let mut parts = words(command);
        let file_text = target.to_string_lossy().into_owned();
        if !parts.iter().any(|w| w.contains("{file}")) {
            parts.push("{file}".into());
        }
        let parts: Vec<String> = parts.iter().map(|w| w.replace("{file}", &file_text)).collect();
        let (program, args) = parts.split_first().ok_or("an empty command")?;
        let mut c = Command::new(program);
        c.args(args);
        c
    };
    process.spawn().map_err(|e| format!("{}: {e}", target.display()))?;
    Ok(format!("opened {}", target.display()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn words_split_and_unquote() {
        assert_eq!(
            super::words(r#"flatpak run "org.gimp.GIMP" {file}"#),
            ["flatpak", "run", "org.gimp.GIMP", "{file}"]
        );
        assert_eq!(super::words("  a  ''  b "), ["a", "", "b"]);
    }
}
