//! Unified Font Object 3 (`.ufo`, unifiedfontobject.org): a folder of XML property lists and one `.glif` file a glyph,
//! which font editors (`FontForge`, Fontra, Glyphs, `RoboFont`) open and save. Read and written here from and to its
//! files by their names within the folder, as an extension is given them: the font's info, lib, groups and kerning, and
//! its default layer's glyphs - each with its advance, its Unicode values, its image (a PNG of the folder's `images/`
//! placed by an offset), its outline and its lib. What the glyphs mean is their reader's business. Written
//! deterministically: the same font is the same files.

use std::collections::BTreeMap;
use std::fmt::{self, Write as _};

/// Why a font could not be read.
#[derive(Debug)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

fn error(at: &str, why: impl fmt::Display) -> Error {
    Error(format!("{at}: {why}"))
}

/// A property list's value.
#[derive(Clone, Debug, PartialEq)]
pub enum Plist {
    String(String),
    Integer(i64),
    Real(f64),
    Bool(bool),
    Array(Vec<Plist>),
    /// Its keys in the order written.
    Dict(Vec<(String, Plist)>),
}

impl Plist {
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Plist> {
        match self {
            Plist::Dict(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// A number, integer or real.
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            #[expect(clippy::cast_precision_loss, reason = "a font's numbers are small")]
            Plist::Integer(n) => Some(*n as f64),
            Plist::Real(r) => Some(*r),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Plist::String(s) => Some(s),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_integer(&self) -> Option<i64> {
        match self {
            Plist::Integer(n) => Some(*n),
            #[expect(clippy::cast_possible_truncation, reason = "a real that is whole")]
            Plist::Real(n) if n.fract() == 0.0 => Some(*n as i64),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_real(&self) -> Option<f64> {
        match self {
            #[expect(clippy::cast_precision_loss, reason = "a font's numbers are small")]
            Plist::Integer(n) => Some(*n as f64),
            Plist::Real(n) => Some(*n),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_array(&self) -> Option<&[Plist]> {
        match self {
            Plist::Array(items) => Some(items),
            _ => None,
        }
    }

    #[must_use]
    pub fn entries(&self) -> &[(String, Plist)] {
        match self {
            Plist::Dict(entries) => entries,
            _ => &[],
        }
    }
}

/// A point of a contour: where it is and what kind (`line`, `curve`, `qcurve`, `move`; none for an off-curve point).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
    pub kind: Option<String>,
    pub smooth: bool,
}

/// A glyph drawn into another, by its name and a transformation (`xScale`, `xyScale`, `yxScale`, `yScale`, `xOffset`,
/// `yOffset`).
#[derive(Clone, Debug, PartialEq)]
pub struct Component {
    pub base: String,
    pub transform: [f64; 6],
}

/// A glyph's image: a PNG of the font's `images/`, its bottom-left corner placed at `(x, y)` in font units.
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub file: String,
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Glyph {
    pub name: String,
    pub advance: f64,
    pub unicodes: Vec<u32>,
    pub image: Option<Image>,
    pub contours: Vec<Vec<Point>>,
    pub components: Vec<Component>,
    pub note: Option<String>,
    /// Its lib: a dictionary.
    pub lib: Vec<(String, Plist)>,
}

/// A font: its default layer's glyphs in their order (`public.glyphOrder`, else by name), and what is said of it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Font {
    /// `fontinfo.plist`: a dictionary.
    pub info: Vec<(String, Plist)>,
    /// `lib.plist`, less `public.glyphOrder`, which the glyphs' order is.
    pub lib: Vec<(String, Plist)>,
    pub groups: BTreeMap<String, Vec<String>>,
    /// First member (a glyph or a `public.kern1.` group) to second to value.
    pub kerning: BTreeMap<String, BTreeMap<String, f64>>,
    pub glyphs: Vec<Glyph>,
    /// `images/`, by file name.
    pub images: BTreeMap<String, Vec<u8>>,
}

impl Font {
    #[must_use]
    pub fn glyph(&self, name: &str) -> Option<&Glyph> {
        self.glyphs.iter().find(|g| g.name == name)
    }
}

// --- reading ---------------------------------------------------------------------------------------------------------

/// Read a font from its files, by their names within its folder (`fontinfo.plist`, `glyphs/A_.glif`, ...).
///
/// # Errors
/// It is not a UFO 3 font, or a file of it cannot be read.
pub fn read(files: &BTreeMap<String, Vec<u8>>) -> Result<Font, Error> {
    let file = |name: &str| files.get(name).map(Vec::as_slice);
    let meta = file("metainfo.plist").ok_or_else(|| Error("no metainfo.plist: not a UFO font".into()))?;
    let meta = parse_plist(meta).map_err(|e| error("metainfo.plist", e))?;
    match meta.get("formatVersion").and_then(Plist::as_integer) {
        Some(3) => {}
        other => return Err(Error(format!("metainfo.plist: format {other:?}; UFO 3 is read"))),
    }
    let plist = |name: &str| -> Result<Option<Plist>, Error> {
        file(name).map(|b| parse_plist(b).map_err(|e| error(name, e))).transpose()
    };
    let info = plist("fontinfo.plist")?.map_or_default(|p| p.entries().to_vec());
    let mut lib = plist("lib.plist")?.map_or_default(|p| p.entries().to_vec());
    let order: Vec<String> = lib
        .iter()
        .find(|(k, _)| k == "public.glyphOrder")
        .and_then(|(_, v)| v.as_array())
        .map_or_default(|names| names.iter().filter_map(Plist::as_str).map(str::to_string).collect());
    lib.retain(|(k, _)| k != "public.glyphOrder");
    let mut groups = BTreeMap::new();
    for (name, members) in plist("groups.plist")?.map_or_default(|p| p.entries().to_vec()) {
        let members =
            members.as_array().unwrap_or_default().iter().filter_map(Plist::as_str).map(str::to_string).collect();
        groups.insert(name, members);
    }
    let mut kerning: BTreeMap<String, BTreeMap<String, f64>> = BTreeMap::new();
    for (first, seconds) in plist("kerning.plist")?.map_or_default(|p| p.entries().to_vec()) {
        for (second, value) in seconds.entries() {
            let value =
                value.as_real().ok_or_else(|| Error(format!("kerning.plist: {first} {second} is not a number")))?;
            kerning.entry(first.clone()).or_default().insert(second.clone(), value);
        }
    }
    // The default layer: the first `layercontents.plist` names, `glyphs` where none is written.
    let layer = plist("layercontents.plist")?
        .and_then(|p| {
            p.as_array()
                .and_then(|layers| layers.first().and_then(|l| l.as_array()?.get(1)?.as_str().map(str::to_string)))
        })
        .unwrap_or_else(|| "glyphs".to_string());
    let contents_name = format!("{layer}/contents.plist");
    let contents = plist(&contents_name)?.ok_or_else(|| Error(format!("no {contents_name}")))?;
    let mut glyphs = Vec::new();
    for (name, path) in contents.entries() {
        let path = path.as_str().ok_or_else(|| Error(format!("{contents_name}: {name} names no file")))?;
        let at = format!("{layer}/{path}");
        let bytes = file(&at).ok_or_else(|| Error(format!("{at}: missing, which {contents_name} names")))?;
        let glyph = parse_glif(bytes).map_err(|e| error(&at, e))?;
        if glyph.name != *name {
            return Err(Error(format!("{at}: names its glyph {}, {contents_name} {name}", glyph.name)));
        }
        glyphs.push(glyph);
    }
    // In the font's order; what it does not order, after, by name.
    let rank = |name: &str| order.iter().position(|n| n == name).unwrap_or(usize::MAX);
    glyphs.sort_by(|a, b| rank(&a.name).cmp(&rank(&b.name)).then_with(|| a.name.cmp(&b.name)));
    let images = files
        .iter()
        .filter_map(|(name, bytes)| Some((name.strip_prefix("images/")?.to_string(), bytes.clone())))
        .collect();
    Ok(Font { info, lib, groups, kerning, glyphs, images })
}

/// A container being read, innermost last: an array's items, or a dictionary's entries and the key waiting for its value.
enum Open {
    Array(Vec<Plist>),
    Dict(Vec<(String, Plist)>, Option<String>),
}

/// A property list's XML: its `<plist>`'s one value.
///
/// # Errors
/// It is not a property list.
pub fn parse_plist(bytes: &[u8]) -> Result<Plist, String> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_reader(bytes);
    let mut open: Vec<Open> = Vec::new();
    let mut done: Option<Plist> = None;
    let mut text = String::new();
    let mut buf = Vec::new();
    let put = |open: &mut Vec<Open>, done: &mut Option<Plist>, value: Plist| -> Result<(), String> {
        match open.last_mut() {
            Some(Open::Array(items)) => items.push(value),
            Some(Open::Dict(entries, key)) => {
                let key = key.take().ok_or("a value without its key")?;
                entries.push((key, value));
            }
            None => *done = Some(value),
        }
        Ok(())
    };
    loop {
        match reader.read_event_into(&mut buf).map_err(|e| e.to_string())? {
            Event::Start(e) => {
                text.clear();
                match e.name().as_ref() {
                    b"array" => open.push(Open::Array(Vec::new())),
                    b"dict" => open.push(Open::Dict(Vec::new(), None)),
                    _ => {}
                }
            }
            Event::Empty(e) => match e.name().as_ref() {
                b"array" => put(&mut open, &mut done, Plist::Array(Vec::new()))?,
                b"dict" => put(&mut open, &mut done, Plist::Dict(Vec::new()))?,
                b"true" => put(&mut open, &mut done, Plist::Bool(true))?,
                b"false" => put(&mut open, &mut done, Plist::Bool(false))?,
                b"string" => put(&mut open, &mut done, Plist::String(String::new()))?,
                _ => {}
            },
            Event::Text(t) => text.push_str(&t.decode().map_err(|e| e.to_string())?),
            Event::GeneralRef(r) => text.push_str(entity(&r.decode().map_err(|e| e.to_string())?)?),
            Event::End(e) => {
                let value = match e.name().as_ref() {
                    b"key" => {
                        match open.last_mut() {
                            Some(Open::Dict(_, key)) => *key = Some(std::mem::take(&mut text)),
                            _ => return Err("a key outside a dict".into()),
                        }
                        None
                    }
                    b"string" => Some(Plist::String(std::mem::take(&mut text))),
                    b"integer" => {
                        Some(Plist::Integer(text.trim().parse().map_err(|_| format!("{text} is not an integer"))?))
                    }
                    b"real" => Some(Plist::Real(text.trim().parse().map_err(|_| format!("{text} is not a number"))?)),
                    b"array" => match open.pop() {
                        Some(Open::Array(items)) => Some(Plist::Array(items)),
                        _ => return Err("an array's end without it".into()),
                    },
                    b"dict" => match open.pop() {
                        Some(Open::Dict(entries, _)) => Some(Plist::Dict(entries)),
                        _ => return Err("a dict's end without it".into()),
                    },
                    _ => None,
                };
                text.clear();
                if let Some(value) = value {
                    put(&mut open, &mut done, value)?;
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    done.ok_or_else(|| "no value".to_string())
}

/// A glyph's `.glif` (format 1 or 2).
///
/// # Errors
/// It is not a glyph.
pub fn parse_glif(bytes: &[u8]) -> Result<Glyph, String> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_reader(bytes);
    let mut glyph = Glyph::default();
    let mut contour: Option<Vec<Point>> = None;
    let mut buf = Vec::new();
    let mut in_note = false;
    // The lib: everything between `<lib>` and `</lib>`, read as a property list.
    let mut lib_xml: Option<String> = None;
    let mut depth_in_lib = 0usize;
    loop {
        let event = reader.read_event_into(&mut buf).map_err(|e| e.to_string())?;
        if let Some(xml) = lib_xml.as_mut() {
            if lib_event(&event, xml, &mut depth_in_lib)? {
                let lib = parse_plist(format!("<plist>{xml}</plist>").as_bytes())?;
                glyph.lib = lib.entries().to_vec();
                lib_xml = None;
            }
            buf.clear();
            continue;
        }
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let attrs = attributes(e)?;
                let get = |key: &str| attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
                let number = |key: &str| {
                    get(key)
                        .map(|v| v.trim().parse::<f64>().map_err(|_| format!("{key}={v} is not a number")))
                        .transpose()
                };
                match e.name().as_ref() {
                    b"glyph" => glyph.name = get("name").ok_or("a glyph without a name")?.to_string(),
                    b"advance" => glyph.advance = number("width")?.unwrap_or(0.0),
                    b"unicode" => {
                        let hex = get("hex").ok_or("a unicode without hex")?;
                        glyph.unicodes.push(u32::from_str_radix(hex, 16).map_err(|_| format!("{hex} is not hex"))?);
                    }
                    b"image" => {
                        let file = get("fileName").ok_or("an image without a fileName")?.to_string();
                        glyph.image = Some(Image {
                            file,
                            x: number("xOffset")?.unwrap_or(0.0),
                            y: number("yOffset")?.unwrap_or(0.0),
                        });
                    }
                    b"contour" => contour = Some(Vec::new()),
                    b"point" => {
                        let point = Point {
                            x: number("x")?.ok_or("a point without x")?,
                            y: number("y")?.ok_or("a point without y")?,
                            kind: get("type").filter(|t| *t != "offcurve").map(str::to_string),
                            smooth: get("smooth") == Some("yes"),
                        };
                        contour.as_mut().ok_or("a point outside a contour")?.push(point);
                    }
                    b"component" => {
                        let base = get("base").ok_or("a component without a base")?.to_string();
                        let n = |key: &str, default: f64| number(key).map(|v| v.unwrap_or(default));
                        let transform = [
                            n("xScale", 1.0)?,
                            n("xyScale", 0.0)?,
                            n("yxScale", 0.0)?,
                            n("yScale", 1.0)?,
                            n("xOffset", 0.0)?,
                            n("yOffset", 0.0)?,
                        ];
                        glyph.components.push(Component { base, transform });
                    }
                    b"note" => in_note = matches!(event, Event::Start(_)),
                    b"lib" if matches!(event, Event::Start(_)) => lib_xml = Some(String::new()),
                    _ => {}
                }
            }
            Event::End(ref e) => match e.name().as_ref() {
                b"contour" => glyph.contours.extend(contour.take()),
                b"note" => in_note = false,
                _ => {}
            },
            Event::Text(ref t) if in_note => {
                let note = t.decode().map_err(|e| e.to_string())?;
                glyph.note.get_or_insert_with(String::new).push_str(&note);
            }
            Event::GeneralRef(ref r) if in_note => {
                let note = glyph.note.get_or_insert_with(String::new);
                note.push_str(entity(&r.decode().map_err(|e| e.to_string())?)?);
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    if glyph.name.is_empty() {
        return Err("no glyph".into());
    }
    Ok(glyph)
}

/// An event inside a glyph's `<lib>`, kept as the XML it is (`depth`: the elements open in it): whether it ends the lib.
fn lib_event(event: &quick_xml::events::Event, xml: &mut String, depth: &mut usize) -> Result<bool, String> {
    use quick_xml::events::Event;
    match event {
        Event::End(e) if e.name().as_ref() == b"lib" && *depth == 0 => return Ok(true),
        Event::Start(e) => {
            *depth += 1;
            let _ = write!(xml, "<{}>", String::from_utf8_lossy(e.name().as_ref()));
        }
        Event::Empty(e) => {
            let _ = write!(xml, "<{}/>", String::from_utf8_lossy(e.name().as_ref()));
        }
        Event::End(e) => {
            *depth = depth.saturating_sub(1);
            let _ = write!(xml, "</{}>", String::from_utf8_lossy(e.name().as_ref()));
        }
        Event::Text(t) => xml.push_str(&escape(&t.decode().map_err(|e| e.to_string())?)),
        Event::GeneralRef(r) => {
            let _ = write!(xml, "&{};", r.decode().map_err(|e| e.to_string())?);
        }
        _ => {}
    }
    Ok(false)
}

/// What one of XML's own entities stands for.
fn entity(name: &str) -> Result<&'static str, String> {
    Ok(match name {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        other => return Err(format!("an entity {other}")),
    })
}

fn attributes(e: &quick_xml::events::BytesStart) -> Result<Vec<(String, String)>, String> {
    e.attributes()
        .map(|a| {
            let a = a.map_err(|e| e.to_string())?;
            let key = String::from_utf8_lossy(a.key.as_ref()).into_owned();
            let value = a.normalized_value(quick_xml::XmlVersion::Implicit1_0).map_err(|e| e.to_string())?.into_owned();
            Ok((key, value))
        })
        .collect()
}

// --- writing ---------------------------------------------------------------------------------------------------------

/// A font's files, by their names within its folder.
#[must_use]
pub fn write(font: &Font) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut put = |name: &str, text: String| {
        files.insert(name.to_string(), text.into_bytes());
    };
    put(
        "metainfo.plist",
        plist(&Plist::Dict(vec![
            ("creator".into(), Plist::String("org.pe2decomp".into())),
            ("formatVersion".into(), Plist::Integer(3)),
        ])),
    );
    put("fontinfo.plist", plist(&Plist::Dict(font.info.clone())));
    let mut lib = vec![(
        "public.glyphOrder".to_string(),
        Plist::Array(font.glyphs.iter().map(|g| Plist::String(g.name.clone())).collect()),
    )];
    lib.extend(font.lib.iter().cloned());
    put("lib.plist", plist(&Plist::Dict(lib)));
    if !font.groups.is_empty() {
        let groups = font
            .groups
            .iter()
            .map(|(name, members)| (name.clone(), Plist::Array(members.iter().cloned().map(Plist::String).collect())))
            .collect();
        put("groups.plist", plist(&Plist::Dict(groups)));
    }
    if !font.kerning.is_empty() {
        let kerning = font
            .kerning
            .iter()
            .map(|(first, seconds)| {
                (first.clone(), Plist::Dict(seconds.iter().map(|(s, v)| (s.clone(), number(*v))).collect()))
            })
            .collect();
        put("kerning.plist", plist(&Plist::Dict(kerning)));
    }
    put(
        "layercontents.plist",
        plist(&Plist::Array(vec![Plist::Array(vec![
            Plist::String("public.default".into()),
            Plist::String("glyphs".into()),
        ])])),
    );
    let names = glyph_file_names(&font.glyphs.iter().map(|g| g.name.as_str()).collect::<Vec<_>>());
    let contents = font.glyphs.iter().map(|g| (g.name.clone(), Plist::String(names[&g.name].clone()))).collect();
    put("glyphs/contents.plist", plist(&Plist::Dict(contents)));
    for glyph in &font.glyphs {
        put(&format!("glyphs/{}", names[&glyph.name]), glif(glyph));
    }
    for (name, bytes) in &font.images {
        files.insert(format!("images/{name}"), bytes.clone());
    }
    files
}

/// A whole number as an integer, else a real.
fn number(value: f64) -> Plist {
    #[expect(clippy::cast_possible_truncation, reason = "a whole number")]
    if value.fract() == 0.0 && value.abs() < 1e15 { Plist::Integer(value as i64) } else { Plist::Real(value) }
}

fn format_number(value: f64) -> String {
    match number(value) {
        Plist::Integer(n) => n.to_string(),
        _ => value.to_string(),
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A property list's XML.
#[must_use]
pub fn plist(value: &Plist) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n",
    );
    plist_value(&mut out, value, 1);
    out.push_str("</plist>\n");
    out
}

fn plist_value(out: &mut String, value: &Plist, depth: usize) {
    let pad = "  ".repeat(depth);
    match value {
        Plist::String(s) => {
            let _ = writeln!(out, "{pad}<string>{}</string>", escape(s));
        }
        Plist::Integer(n) => {
            let _ = writeln!(out, "{pad}<integer>{n}</integer>");
        }
        Plist::Real(n) => {
            let _ = writeln!(out, "{pad}<real>{n}</real>");
        }
        Plist::Bool(b) => {
            let _ = writeln!(out, "{pad}<{}/>", if *b { "true" } else { "false" });
        }
        Plist::Array(items) if items.is_empty() => {
            let _ = writeln!(out, "{pad}<array/>");
        }
        Plist::Array(items) => {
            let _ = writeln!(out, "{pad}<array>");
            for item in items {
                plist_value(out, item, depth + 1);
            }
            let _ = writeln!(out, "{pad}</array>");
        }
        Plist::Dict(entries) if entries.is_empty() => {
            let _ = writeln!(out, "{pad}<dict/>");
        }
        Plist::Dict(entries) => {
            let _ = writeln!(out, "{pad}<dict>");
            for (key, item) in entries {
                let _ = writeln!(out, "{pad}  <key>{}</key>", escape(key));
                plist_value(out, item, depth + 1);
            }
            let _ = writeln!(out, "{pad}</dict>");
        }
    }
}

/// A glyph's `.glif`, format 2.
#[must_use]
pub fn glif(glyph: &Glyph) -> String {
    let mut out =
        format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<glyph name=\"{}\" format=\"2\">\n", escape(&glyph.name));
    let _ = writeln!(out, "  <advance width=\"{}\"/>", format_number(glyph.advance));
    for code in &glyph.unicodes {
        let _ = writeln!(out, "  <unicode hex=\"{code:04X}\"/>");
    }
    if let Some(note) = &glyph.note {
        let _ = writeln!(out, "  <note>{}</note>", escape(note));
    }
    if let Some(image) = &glyph.image {
        let _ = writeln!(
            out,
            "  <image fileName=\"{}\" xOffset=\"{}\" yOffset=\"{}\"/>",
            escape(&image.file),
            format_number(image.x),
            format_number(image.y)
        );
    }
    if !glyph.contours.is_empty() || !glyph.components.is_empty() {
        out.push_str("  <outline>\n");
        for contour in &glyph.contours {
            out.push_str("    <contour>\n");
            for p in contour {
                let kind = p.kind.as_ref().map_or_default(|k| format!(" type=\"{k}\""));
                let smooth = if p.smooth { " smooth=\"yes\"" } else { "" };
                let _ = writeln!(
                    out,
                    "      <point x=\"{}\" y=\"{}\"{kind}{smooth}/>",
                    format_number(p.x),
                    format_number(p.y)
                );
            }
            out.push_str("    </contour>\n");
        }
        for c in &glyph.components {
            let [xx, xy, yx, yy, dx, dy] = c.transform;
            let mut attrs = format!("base=\"{}\"", escape(&c.base));
            for (key, value, default) in [
                ("xScale", xx, 1.0),
                ("xyScale", xy, 0.0),
                ("yxScale", yx, 0.0),
                ("yScale", yy, 1.0),
                ("xOffset", dx, 0.0),
                ("yOffset", dy, 0.0),
            ] {
                #[expect(clippy::float_cmp, reason = "a default written as it is")]
                if value != default {
                    let _ = write!(attrs, " {key}=\"{}\"", format_number(value));
                }
            }
            let _ = writeln!(out, "    <component {attrs}/>");
        }
        out.push_str("  </outline>\n");
    }
    if !glyph.lib.is_empty() {
        out.push_str("  <lib>\n");
        plist_value(&mut out, &Plist::Dict(glyph.lib.clone()), 2);
        out.push_str("  </lib>\n");
    }
    out.push_str("</glyph>\n");
    out
}

/// The `.glif` file name of each glyph, by the user-name-to-file-name convention: an uppercase letter followed by `_`,
/// a reserved character as `_`, a leading `.` as `_`; unique ignoring case.
#[must_use]
pub fn glyph_file_names(names: &[&str]) -> BTreeMap<String, String> {
    const RESERVED: &str = "\"*+/:<>?[\\]|";
    let mut out = BTreeMap::new();
    let mut taken = std::collections::BTreeSet::new();
    for name in names {
        let mut base: String = name
            .chars()
            .map(|c| {
                if RESERVED.contains(c) || u32::from(c) < 0x20 || c == '\u{7F}' {
                    "_".to_string()
                } else if c.is_uppercase() {
                    format!("{c}_")
                } else {
                    c.to_string()
                }
            })
            .collect();
        if base.starts_with('.') {
            base.replace_range(..1, "_");
        }
        let mut stem = base.clone();
        let mut n = 1;
        while !taken.insert(stem.to_lowercase()) {
            stem = format!("{base}{n:02}");
            n += 1;
        }
        out.insert((*name).to_string(), format!("{stem}.glif"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Component, Font, Glyph, Image, Plist, Point, glyph_file_names, read, write};

    fn font() -> Font {
        let a = Glyph {
            name: "A".into(),
            advance: 10.0,
            unicodes: vec![0x41, 0x61],
            image: Some(Image { file: "A_.png".into(), x: -1.0, y: -2.0 }),
            note: Some("drawn <clipped> & so".into()),
            lib: vec![
                ("org.pe2decomp.code".into(), Plist::Integer(0x41)),
                ("org.pe2decomp.cell".into(), Plist::Array(vec![Plist::Integer(3), Plist::Integer(4)])),
            ],
            ..Glyph::default()
        };
        let o = Glyph {
            name: "o".into(),
            advance: 7.5,
            unicodes: vec![0x6F],
            contours: vec![vec![
                Point { x: 0.0, y: 0.0, kind: Some("line".into()), smooth: false },
                Point { x: 1.0, y: 2.0, kind: None, smooth: false },
                Point { x: 3.0, y: 0.0, kind: Some("qcurve".into()), smooth: true },
            ]],
            components: vec![Component { base: "A".into(), transform: [1.0, 0.0, 0.0, 1.0, 5.0, 0.0] }],
            ..Glyph::default()
        };
        Font {
            info: vec![("familyName".into(), Plist::String("Test".into())), ("unitsPerEm".into(), Plist::Integer(16))],
            lib: vec![("org.pe2decomp.face".into(), Plist::Dict(vec![("v".into(), Plist::Integer(38))]))],
            groups: [("public.kern1.positive".to_string(), vec!["A".to_string()])].into_iter().collect(),
            kerning: [("public.kern1.positive".to_string(), [("o".to_string(), -2.0)].into_iter().collect())]
                .into_iter()
                .collect(),
            // In their own order, not by name.
            glyphs: vec![o, a],
            images: [("A_.png".to_string(), vec![1, 2, 3])].into_iter().collect(),
        }
    }

    #[test]
    fn a_font_is_written_and_read_back_whole() {
        let font = font();
        let files = write(&font);
        assert!(
            files.contains_key("glyphs/A_.glif")
                && files.contains_key("glyphs/contents.plist")
                && files.contains_key("images/A_.png")
        );
        assert_eq!(read(&files).unwrap(), font);
        // The same font is the same files.
        assert_eq!(write(&read(&files).unwrap()), files);
    }

    #[test]
    fn what_is_not_a_ufo_is_said_so() {
        assert!(read(&std::collections::BTreeMap::new()).unwrap_err().to_string().contains("not a UFO"));
        let mut files = write(&font());
        files.remove("glyphs/o.glif");
        assert!(read(&files).unwrap_err().to_string().contains("glyphs/o.glif: missing"));
    }

    #[test]
    fn file_names_follow_the_convention() {
        let names = glyph_file_names(&["A", "a", ".notdef", "a/b", "AB"]);
        assert_eq!(names["A"], "A_.glif");
        assert_eq!(names["a"], "a.glif");
        assert_eq!(names[".notdef"], "_notdef.glif");
        assert_eq!(names["a/b"], "a_b.glif");
        assert_eq!(names["AB"], "A_B_.glif");
    }
}
