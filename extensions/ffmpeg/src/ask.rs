//! What is asked of ffmpeg: what this build has, read from what it prints, and whether an encoding works.

use pe2_ext::process::spawn;
use pe2_ext::{Project, Registry, Result, Value, anyhow};

use crate::{Encoding, strings, table};

/// What a run of ffmpeg printed.
fn printed(args: &[&str]) -> Result<String> {
    let mut all = vec!["-hide_banner"];
    all.extend_from_slice(args);
    let mut process = spawn("ffmpeg", &strings(&all))?;
    let mut text = Vec::new();
    loop {
        let piece = process.read(1 << 16)?;
        if piece.is_empty() {
            break;
        }
        text.extend(piece);
    }
    process.wait()?;
    Ok(String::from_utf8_lossy(&text).into_owned())
}

/// The lines of a listing, after the rule ending its legend.
fn listed(text: &str) -> impl Iterator<Item = &str> {
    text.lines().skip_while(|l| !l.trim_start().starts_with("--")).skip(1)
}

fn text(value: &str) -> Value {
    Value::String(value.to_string())
}

/// The muxers of `ffmpeg -muxers`: its devices left out, which are no files.
fn containers(listing: &str) -> Vec<Value> {
    listed(listing)
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let (flags, name) = (words.next()?, words.next()?);
            let label = words.collect::<Vec<_>>().join(" ");
            (flags.contains('E') && !flags.contains('d'))
                .then(|| table([("name", text(name)), ("label", text(&label))]))
        })
        .collect()
}

/// The encoders of `ffmpeg -encoders` of a type (`V`, `A`): each with the codec it makes.
fn encoders(listing: &str, kind: char) -> Vec<Value> {
    listed(listing)
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let (flags, name) = (words.next()?, words.next()?);
            if !flags.starts_with(kind) {
                return None;
            }
            let label = words.collect::<Vec<_>>().join(" ");
            let (label, codec) = match label.rsplit_once(" (codec ") {
                Some((label, codec)) => (label.to_string(), codec.trim_end_matches(')').to_string()),
                None => (label, name.to_string()),
            };
            Some(table([("name", text(name)), ("label", text(&label)), ("codec", text(&codec))]))
        })
        .collect()
}

/// A value after a `name:` line of `ffmpeg -h`.
fn after<'a>(help: &'a str, name: &str) -> Option<&'a str> {
    help.lines().find_map(|line| line.trim().strip_prefix(name)).map(|v| v.trim().trim_end_matches('.'))
}

/// What `ffmpeg -h muxer=<name>` says of a container.
fn container(help: &str) -> Value {
    let words =
        |name: &str| after(help, name).map_or_default(|v| v.split(',').map(|w| text(w.trim())).collect::<Vec<_>>());
    let one = |name: &str| text(after(help, name).unwrap_or_default());
    table([
        ("extensions", Value::Array(words("Common extensions:"))),
        ("media_type", one("Mime type:")),
        ("video", one("Default video codec:")),
        ("audio", one("Default audio codec:")),
    ])
}

/// An option of `ffmpeg -h encoder=<name>`, from its line: its name, its type and what it says.
fn option(line: &str) -> Option<(String, Value)> {
    let line = line.trim_start();
    let rest = line.strip_prefix('-')?;
    let (name, rest) = rest.split_once(char::is_whitespace)?;
    let rest = rest.trim_start();
    let (kind, rest) = rest.strip_prefix('<')?.split_once('>')?;
    let says = rest.trim_start().split_once(char::is_whitespace).map_or("", |(_, says)| says.trim());
    let default = says.rsplit_once("(default ").map(|(_, d)| d.trim_end_matches(')').trim_matches('"').to_string());
    let says = says.rsplit_once(" (default ").map_or(says, |(says, _)| says);
    let mut entry = vec![("type", text(kind)), ("says", text(says))];
    if let Some(default) = default {
        entry.push(("default", text(&default)));
    }
    Some((name.to_string(), table(entry)))
}

/// What `ffmpeg -h encoder=<name>` says of an encoder: the pixel or sample formats it takes, and its options, each with
/// the named values it takes, where it says them.
fn encoder(help: &str) -> Value {
    let formats = after(help, "Supported pixel formats:").or_else(|| after(help, "Supported sample formats:"));
    let formats = formats.map_or_default(|f| f.split_whitespace().map(text).collect::<Vec<_>>());
    let mut options: Vec<(String, Value)> = Vec::new();
    for line in help.lines() {
        if let Some(option) = option(line) {
            options.push(option);
            continue;
        }
        // Indented deeper than an option: a value of the last.
        let named = line.starts_with("     ").then(|| line.split_whitespace().next()).flatten();
        if let (Some(named), Some((_, Value::Table(entry)))) = (named, options.last_mut()) {
            let values = entry.entry("values").or_insert_with(|| Value::Array(vec![]));
            if let Value::Array(values) = values {
                values.push(text(named));
            }
        }
    }
    table([("formats", Value::Array(formats)), ("options", Value::Table(options.into_iter().collect()))])
}

/// The extension a container's files take.
pub fn extension(name: &str) -> Result<String> {
    let help = printed(&["-h", &format!("muxer={name}")])?;
    let extensions = after(&help, "Common extensions:").and_then(|e| e.split(',').next().map(|e| e.trim().to_string()));
    Ok(extensions.unwrap_or_else(|| name.to_string()))
}

/// A word of an argument table.
fn argument<'a>(arguments: &'a Value, name: &str) -> Result<&'a str> {
    arguments.get(name).and_then(Value::as_str).ok_or_else(|| anyhow!("no `{name}`"))
}

/// A number of an argument table.
fn number(arguments: &Value, name: &str) -> Result<i64> {
    arguments.get(name).and_then(Value::as_integer).ok_or_else(|| anyhow!("no `{name}`"))
}

/// Where a trial may write: what the system says, a path a program can open.
fn scratch() -> String {
    let folder = ["TMPDIR", "TEMP", "TMP"]
        .iter()
        .find_map(|name| std::env::var(name).ok())
        .unwrap_or_else(|| "/tmp".to_string());
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |t| t.as_nanos());
    format!("{}/pe2-ffmpeg-check-{stamp}", folder.trim_end_matches('/'))
}

/// An encoding tried on one black frame of a size, and a moment of silence: whether it works, and what ffmpeg said.
fn check(arguments: &Value) -> Result<Value> {
    let encoding = Encoding::parse(argument(arguments, "encoding")?)?;
    let size = format!("{}x{}", number(arguments, "width")?, number(arguments, "height")?);
    let file = scratch();
    let mut args = strings(&[
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-y",
        "-f",
        "lavfi",
        "-i",
        &format!("color=black:s={size}:r=15:d=0.07"),
        "-f",
        "lavfi",
        "-i",
        "anullsrc=r=37800:cl=stereo:d=0.07",
        "-frames:v",
        "1",
    ]);
    args.extend(encoding.video.args("v"));
    args.extend(encoding.audio.args("a"));
    args.extend(strings(&["-strict", "-2", "-f", &encoding.container, &file]));
    let worked = spawn("ffmpeg", &args)?.wait();
    let _ = std::fs::remove_file(&file);
    Ok(match worked {
        Ok(()) => table([("works", Value::Boolean(true))]),
        Err(why) => table([("works", Value::Boolean(false)), ("why", text(&format!("{why:#}")))]),
    })
}

/// What is asked of it, as services.
pub fn register(r: &mut Registry) {
    let empty = r#"{"type":"object"}"#;
    let _ = r.service("capabilities", "The containers and encoders there are", empty, |_: &dyn Project, _| {
        let (muxers, encoded) = (printed(&["-muxers"])?, printed(&["-encoders"])?);
        Ok(table([
            ("containers", Value::Array(containers(&muxers))),
            ("video", Value::Array(encoders(&encoded, 'V'))),
            ("audio", Value::Array(encoders(&encoded, 'A'))),
        ]))
    });
    let _ = r.service(
        "container",
        "A container: its extensions, media type and default codecs",
        empty,
        |_: &dyn Project, arguments| {
            Ok(container(&printed(&["-h", &format!("muxer={}", argument(arguments, "name")?)])?))
        },
    );
    let _ = r.service(
        "encoder",
        "An encoder: the formats it takes and its options",
        empty,
        |_: &dyn Project, arguments| {
            Ok(encoder(&printed(&["-h", &format!("encoder={}", argument(arguments, "name")?)])?))
        },
    );
    let _ = r.service("check", "Whether an encoding works, at a frame's size", empty, |_: &dyn Project, arguments| {
        check(arguments)
    });
}

#[cfg(test)]
mod tests {
    use super::{container, containers, encoder, encoders};
    use pe2_ext::Value;

    const MUXERS: &str = "Formats:\n D.. = Demuxing supported\n .E. = Muxing supported\n ..d = Is a device\n ---\n  E  mp4             MP4 (MPEG-4 Part 14)\n  Ed alsa            ALSA audio output\n";
    const ENCODERS: &str = "Encoders:\n V..... = Video\n ------\n V....D libx264              libx264 H.264 / AVC (codec h264)\n V.S..D ffv1                 FFmpeg video codec #1\n A....D flac                 FLAC (Free Lossless Audio Codec)\n";

    fn word<'a>(value: &'a Value, key: &str) -> &'a str {
        value.get(key).and_then(Value::as_str).unwrap()
    }

    #[test]
    fn what_it_has_is_read_from_what_it_prints() {
        let found = containers(MUXERS);
        assert_eq!(found.len(), 1, "a device is no file");
        assert_eq!(word(&found[0], "name"), "mp4");
        assert_eq!(word(&found[0], "label"), "MP4 (MPEG-4 Part 14)");
        let video = encoders(ENCODERS, 'V');
        assert_eq!(video.len(), 2);
        assert_eq!((word(&video[0], "name"), word(&video[0], "codec")), ("libx264", "h264"));
        assert_eq!((word(&video[1], "name"), word(&video[1], "codec")), ("ffv1", "ffv1"));
        assert_eq!(word(&encoders(ENCODERS, 'A')[0], "name"), "flac");
    }

    #[test]
    fn a_container_and_an_encoder_are_read_from_their_help() {
        let help = "Muxer mp4 [MP4]:\n    Common extensions: mp4.\n    Mime type: video/mp4.\n    Default video codec: h264.\n    Default audio codec: aac.\n";
        let read = container(help);
        assert_eq!(read.get("extensions").unwrap().as_array().unwrap()[0].as_str(), Some("mp4"));
        assert_eq!(
            (word(&read, "media_type"), word(&read, "video"), word(&read, "audio")),
            ("video/mp4", "h264", "aac")
        );
        let help = "Encoder libx264 [H.264]:\n    Supported pixel formats: yuv420p yuvj420p\nlibx264 AVOptions:\n  -preset            <string>     E..V....... Set the encoding preset (default \"medium\")\n  -aq-mode           <int>        E..V....... AQ method (from -1 to INT_MAX) (default -1)\n     none            0            E..V.......\n     variance        1            E..V....... Variance AQ\n";
        let read = encoder(help);
        assert_eq!(read.get("formats").unwrap().as_array().unwrap().len(), 2);
        let options = read.get("options").unwrap();
        let preset = options.get("preset").unwrap();
        assert_eq!((word(preset, "type"), word(preset, "default")), ("string", "medium"));
        assert_eq!(word(preset, "says"), "Set the encoding preset");
        let values = options.get("aq-mode").unwrap().get("values").unwrap().as_array().unwrap();
        assert_eq!(values.iter().filter_map(Value::as_str).collect::<Vec<_>>(), ["none", "variance"]);
    }
}
