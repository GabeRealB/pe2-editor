//! Every byte of a command and of a record by name (`CapCommand`, `CapSequenceRecord`: `packages/gameplay/include/cap.h`).
//! What a command's opcode or a record's type does not read is kept apart, under `unread`, where it differs from what
//! the game's tool usually left there; zero bytes and false flags are left out, and what the layout implies is not
//! written at all.

use std::collections::BTreeMap;

use pe2_ext::{Table, Value};

const OPCODES: [&str; 5] = ["plain", "counter", "flag", "room", "tally"];
const COMMAND_FLAGS: [(&str, u8); 3] = [("wraps", 0x01), ("persists", 0x02), ("branches", 0x04)];
/// What each opcode reads (capRunCommand).
fn command_reads(opcode: &str) -> &'static [&'static str] {
    match opcode {
        "counter" => &["wraps", "persists", "branches", "limit", "game_flag", "counter", "next"],
        "flag" => &["game_flag"],
        "tally" => &["branches", "tally_first", "tally_count", "next"],
        _ => &[],
    }
}
const COMMAND_VALUES: [&str; 13] = [
    "wraps",
    "persists",
    "branches",
    "flag_bits",
    "game_flag",
    "limit",
    "counter",
    "tally_first",
    "tally_count",
    "next",
    "tail_0",
    "tail_1",
    "tail_2",
];

const SCENE: u8 = 0x80;
const RECORD_FLAGS: [(&str, u8); 7] = [
    ("bit0", 0x01),
    ("left_aligned", 0x02),
    ("caret", 0x04),
    ("send_message", 0x08),
    ("title_bank", 0x10),
    ("scene_sync", 0x20),
    ("wait_for_cue", 0x40),
];
const CHILD_ACTION_BASE: u8 = 100;
const MESSAGE_FIELDS: [&str; 3] = ["message", "message_delay", "message_to"];

/// The speakers a dialogue names by their plates: each plate's numbers, and which plate the dialogue shows for a number
/// where two stand for it. Where titles are glyph cells (the other layout tool), a speaker is its number.
#[derive(Clone, Debug, Default)]
pub struct Speakers {
    pub numbers: BTreeMap<String, Vec<u8>>,
    pub chosen: BTreeMap<u8, String>,
    pub titles_are_cells: bool,
}

impl Speakers {
    /// The plate standing for a number: a named one before an unseen placeholder.
    #[must_use]
    pub fn plate(&self, number: u8) -> Option<String> {
        let mut found: Vec<&String> = self.numbers.iter().filter(|(_, n)| n.contains(&number)).map(|(p, _)| p).collect();
        found.sort_by_key(|p| p.starts_with("unseen"));
        found.first().map(|p| (*p).clone())
    }

    /// A speaker number by name: the plate's, `<plate>#<number>` where it stands for several.
    #[must_use]
    pub fn name(&self, number: u8) -> Value {
        if self.titles_are_cells {
            return Value::Integer(i64::from(number));
        }
        let Some(plate) = self.chosen.get(&number).cloned().or_else(|| self.plate(number)) else {
            return Value::Integer(i64::from(number));
        };
        // A plate no cast lists (an unseen one) says its number, as one standing for several does.
        if self.numbers.get(&plate).is_some_and(|n| n.as_slice() == [number]) {
            Value::String(plate)
        } else {
            Value::String(format!("{plate}#{number}"))
        }
    }

    /// A speaker's number.
    ///
    /// # Errors
    /// No plate of that name is known.
    pub fn number(&self, name: &Value) -> Result<u8, String> {
        match name {
            Value::Integer(n) => u8::try_from(*n).map_err(|_| format!("speaker {n} is out of range")),
            Value::String(text) => match text.rsplit_once('#') {
                Some((_, n)) => n.parse().map_err(|_| format!("speaker {text:?}")),
                None => self
                    .numbers
                    .get(text)
                    .and_then(|n| n.first().copied())
                    .ok_or_else(|| format!("no speaker {text:?}")),
            },
            other => Err(format!("speaker {other} is no name")),
        }
    }

    /// The plate a speaker's name shows; none for a number.
    #[must_use]
    pub fn token(name: &Value) -> Option<String> {
        let text = name.as_str()?;
        Some(text.rsplit_once('#').map_or(text, |(plate, _)| plate).to_string())
    }
}

fn int(value: u32) -> Value {
    Value::Integer(i64::from(value))
}

fn get_int(table: &Table, key: &str) -> Option<i64> {
    table.get(key).and_then(Value::as_integer)
}

/// A command's named values from its bytes, and the defaults the tool left: `limit` the highest key of its records;
/// a tally's `tally_count` too, a counter's, flag's and room's 1.
fn command_values(bytes: &[u8; 12]) -> BTreeMap<&'static str, Value> {
    let mut out = BTreeMap::new();
    for (name, bit) in COMMAND_FLAGS {
        out.insert(name, Value::Boolean(bytes[1] & bit != 0));
    }
    out.insert("flag_bits", int(u32::from(bytes[1] & !0x07)));
    out.insert("game_flag", int(u32::from(bytes[3]) | u32::from(bytes[7]) << 8));
    for (name, at) in
        [("limit", 2), ("counter", 4), ("tally_first", 5), ("tally_count", 6), ("next", 8), ("tail_0", 9), ("tail_1", 10), ("tail_2", 11)]
    {
        out.insert(name, int(u32::from(bytes[at])));
    }
    out
}

fn command_defaults(opcode: &str, highest_key: u8) -> BTreeMap<&'static str, Value> {
    let mut out: BTreeMap<&'static str, Value> = COMMAND_VALUES.iter().map(|n| (*n, int(0))).collect();
    for (name, _) in COMMAND_FLAGS {
        out.insert(name, Value::Boolean(false));
    }
    out.insert("limit", int(u32::from(highest_key)));
    match opcode {
        "tally" => {
            out.insert("tally_count", int(u32::from(highest_key)));
        }
        "counter" | "flag" | "room" => {
            out.insert("tally_count", int(1));
        }
        _ => {}
    }
    out
}

fn game_flag_text(value: &Value) -> Value {
    Value::String(format!("{:#05x}", value.as_integer().unwrap_or(0)))
}

/// A command as named fields (its records apart).
#[must_use]
pub fn command_named(bytes: &[u8; 12], highest_key: u8) -> Table {
    let opcode = OPCODES.get(usize::from(bytes[0])).map_or_else(|| int(u32::from(bytes[0])), |o| Value::String((*o).into()));
    let name = opcode.as_str().unwrap_or("");
    let (values, defaults, reads) = (command_values(bytes), command_defaults(name, highest_key), command_reads(name));
    let (mut out, mut unread) = (Table::new(), Table::new());
    out.insert("opcode".into(), opcode.clone());
    for field in COMMAND_VALUES {
        if values[field] != defaults[field] {
            let value = if field == "game_flag" { game_flag_text(&values[field]) } else { values[field].clone() };
            if reads.contains(&field) { &mut out } else { &mut unread }.insert(field.into(), value);
        }
    }
    if !unread.is_empty() {
        out.insert("unread".into(), Value::Table(unread));
    }
    out
}

fn byte(value: Option<&Value>, what: &str) -> Result<u8, String> {
    let n = match value {
        None => 0,
        Some(Value::Boolean(b)) => i64::from(*b),
        Some(Value::Integer(n)) => *n,
        Some(Value::String(text)) => {
            i64::from_str_radix(text.trim_start_matches("0x"), 16).map_err(|_| format!("{what}: {text:?}"))?
        }
        Some(other) => return Err(format!("{what}: {other} is no number")),
    };
    u8::try_from(n).map_err(|_| format!("{what}: {n} is out of range"))
}

/// A command's bytes from its named fields; its `game_flag` already a number or hex text.
///
/// # Errors
/// A field is not a number in range.
pub fn command_bytes(named: &Table, highest_key: u8) -> Result<[u8; 12], String> {
    let opcode = match named.get("opcode") {
        Some(Value::String(name)) => {
            OPCODES.iter().position(|o| o == name).ok_or_else(|| format!("no opcode {name:?}"))?
        }
        Some(Value::Integer(n)) => usize::try_from(*n).map_err(|_| "an opcode out of range")?,
        _ => return Err("a command without an opcode".into()),
    };
    let mut values = command_defaults(OPCODES.get(opcode).copied().unwrap_or(""), highest_key);
    let unread = named.get("unread").and_then(Value::as_table);
    for field in COMMAND_VALUES {
        if let Some(value) = named.get(field).or_else(|| unread.and_then(|u| u.get(field))) {
            values.insert(field, value.clone());
        }
    }
    let flag = match &values["game_flag"] {
        Value::String(text) => u16::from_str_radix(text.trim_start_matches("0x"), 16).map_err(|_| "game_flag")?,
        Value::Integer(n) => u16::try_from(*n).map_err(|_| "game_flag out of range")?,
        other => return Err(format!("game_flag: {other}")),
    };
    let mut flags = byte(values.get("flag_bits"), "flag_bits")?;
    for (name, bit) in COMMAND_FLAGS {
        if values[name].as_bool() == Some(true) {
            flags |= bit;
        }
    }
    let get = |name: &str| byte(values.get(name), name);
    let [lo, hi] = flag.to_le_bytes();
    Ok([
        u8::try_from(opcode).map_err(|_| "opcode")?,
        flags,
        get("limit")?,
        lo,
        get("counter")?,
        get("tally_first")?,
        get("tally_count")?,
        hi,
        get("next")?,
        get("tail_0")?,
        get("tail_1")?,
        get("tail_2")?,
    ])
}

fn recipient_name(value: u8) -> Value {
    Value::String(match value {
        0 => "player".into(),
        1 => "companion".into(),
        n => format!("actor {}", n - 2),
    })
}

fn recipient_value(name: &Value) -> Result<u8, String> {
    match name.as_str() {
        Some("player") => Ok(0),
        Some("companion") => Ok(1),
        Some(text) => text
            .strip_prefix("actor ")
            .and_then(|n| n.parse::<u8>().ok())
            .map(|n| n + 2)
            .ok_or_else(|| format!("no recipient {text:?}")),
        None => Err("a recipient is no name".into()),
    }
}

fn is_scene(bytes: [u8; 8]) -> bool {
    bytes[1] & SCENE != 0
}

/// Whether a record shows a speaker's plate: a line naming a speaker (its first byte is a fallback key where it has an
/// action).
#[must_use]
pub fn is_title(bytes: [u8; 8]) -> bool {
    bytes[0] != 0 && !is_scene(bytes) && bytes[6] == 0
}

/// Every byte of a record by name, as its type reads it; `text` the key of its text, empty where it has none.
fn record_values(bytes: [u8; 8], text: &str, speakers: &Speakers) -> BTreeMap<&'static str, Value> {
    let [b0, flags, b2, b3, b4, _key, action, minimum] = bytes;
    let mut out = BTreeMap::new();
    for (name, bit) in RECORD_FLAGS {
        out.insert(name, Value::Boolean(flags & bit != 0));
    }
    out.insert("text", Value::String(text.into()));
    out.insert("minimum_frames", int(u32::from(minimum)));
    let action_value = if action == 0 {
        Value::Boolean(false)
    } else {
        let mut t = Table::new();
        if action <= CHILD_ACTION_BASE {
            t.insert("placed_object".into(), int(u32::from(action)));
        } else {
            t.insert("scene_child".into(), int(u32::from(action - CHILD_ACTION_BASE)));
        }
        Value::Table(t)
    };
    out.insert("action", action_value);
    if is_scene(bytes) {
        out.insert("view", int(u32::from(b0)));
        out.insert("message", int(u32::from(b2)));
        out.insert("message_delay", int(u32::from(b3)));
        out.insert("message_to", recipient_name(b4));
    } else {
        out.insert("show_frames", int(u32::from(b2)));
        out.insert("pause_frames", int(u32::from(b3)));
        out.insert("room_sound", int(u32::from(b4 >> 1)));
        out.insert("instant", Value::Boolean(b4 & 1 != 0));
        out.insert("fallback_key", int(if action != 0 { u32::from(b0) } else { 0 }));
        let speaker = if b0 != 0 && action == 0 { speakers.name(b0 - 1) } else { Value::Boolean(false) };
        out.insert("speaker", speaker);
    }
    out
}

fn record_defaults(scene: bool, sends: bool) -> BTreeMap<&'static str, Value> {
    let mut out = BTreeMap::new();
    for (name, _) in RECORD_FLAGS {
        out.insert(name, Value::Boolean(false));
    }
    out.insert("text", Value::String(String::new()));
    out.insert("minimum_frames", int(0));
    out.insert("action", Value::Boolean(false));
    if scene {
        out.insert("view", int(0));
        out.insert("message", int(0));
        out.insert("message_delay", int(0));
        out.insert("message_to", recipient_name(u8::from(!sends)));
    } else {
        for name in ["show_frames", "pause_frames", "room_sound", "fallback_key"] {
            out.insert(name, int(0));
        }
        out.insert("instant", Value::Boolean(false));
        out.insert("speaker", Value::Boolean(false));
    }
    out
}

fn line_reads() -> &'static [&'static str] {
    &[
        "speaker",
        "fallback_key",
        "text",
        "show_frames",
        "pause_frames",
        "room_sound",
        "instant",
        "minimum_frames",
        "action",
        "left_aligned",
        "caret",
    ]
}

fn scene_reads() -> &'static [&'static str] {
    &["view", "scene_sync", "wait_for_cue", "send_message", "message", "message_delay", "message_to"]
}

/// A record as named fields; `text` the key of its text in its string table, empty where it has none.
#[must_use]
pub fn record_named(bytes: [u8; 8], text: &str, speakers: &Speakers) -> Table {
    let scene = is_scene(bytes);
    let values = record_values(bytes, text, speakers);
    let sends = values["send_message"].as_bool() == Some(true);
    let defaults = record_defaults(scene, sends);
    let reads: Vec<&str> = if scene {
        scene_reads().iter().copied().filter(|r| sends || !MESSAGE_FIELDS.contains(r)).collect()
    } else {
        line_reads().to_vec()
    };
    let mut out = Table::new();
    out.insert("key".into(), int(u32::from(bytes[5])));
    out.insert("type".into(), Value::String(if scene { "scene" } else { "line" }.into()));
    let mut unread = Table::new();
    for (name, value) in &values {
        if *value != defaults[name] {
            if reads.contains(name) { &mut out } else { &mut unread }.insert((*name).into(), value.clone());
        }
    }
    if !unread.is_empty() {
        out.insert("unread".into(), Value::Table(unread));
    }
    out
}

/// A record's field, where it is read or kept unread.
fn field<'a>(named: &'a Table, name: &str) -> Option<&'a Value> {
    named.get(name).or_else(|| named.get("unread").and_then(Value::as_table).and_then(|u| u.get(name)))
}

/// A record's text key: empty where it has none.
#[must_use]
pub fn record_text(named: &Table) -> String {
    field(named, "text").and_then(Value::as_str).unwrap_or("").to_string()
}

/// A record's bytes from its named fields.
///
/// # Errors
/// A field is not in range, or its speaker is not known.
pub fn record_bytes(named: &Table, speakers: &Speakers) -> Result<[u8; 8], String> {
    let scene = named.get("type").and_then(Value::as_str) == Some("scene");
    let flag = |name: &str| field(named, name).and_then(Value::as_bool) == Some(true);
    let mut flags = if scene { SCENE } else { 0 };
    for (name, bit) in RECORD_FLAGS {
        if flag(name) {
            flags |= bit;
        }
    }
    let number = |name: &str| byte(field(named, name), name);
    let action = match field(named, "action").and_then(Value::as_table) {
        Some(t) => match (get_int(t, "placed_object"), get_int(t, "scene_child")) {
            (Some(n), _) => u8::try_from(n).map_err(|_| "placed_object")?,
            (None, Some(n)) => u8::try_from(n + i64::from(CHILD_ACTION_BASE)).map_err(|_| "scene_child")?,
            _ => 0,
        },
        None => 0,
    };
    let (b0, b2, b3, b4) = if scene {
        let to = field(named, "message_to").cloned().unwrap_or_else(|| recipient_name(u8::from(!flag("send_message"))));
        (number("view")?, number("message")?, number("message_delay")?, recipient_value(&to)?)
    } else {
        let b0 = if action != 0 {
            number("fallback_key")?
        } else {
            match field(named, "speaker") {
                Some(Value::Boolean(false)) | None => 0,
                Some(speaker) => speakers.number(speaker)? + 1,
            }
        };
        let sound = number("room_sound")?;
        (b0, number("show_frames")?, number("pause_frames")?, sound << 1 | u8::from(flag("instant")))
    };
    Ok([b0, flags, b2, b3, b4, number("key")?, action, number("minimum_frames")?])
}

#[cfg(test)]
mod tests {
    use super::{Speakers, command_bytes, command_named, record_bytes, record_named};

    #[test]
    fn commands_read_back_as_written() {
        for bytes in [[1u8, 3, 2, 0xF3, 4, 0, 1, 0, 2, 0, 0, 0], [4, 4, 3, 0, 0, 7, 3, 0, 1, 0, 0, 0], [0, 0, 5, 0, 0, 0, 0, 0, 0, 9, 0, 0]] {
            let named = command_named(&bytes, 2);
            assert_eq!(command_bytes(&named, 2), Ok(bytes), "{named:?}");
        }
    }

    #[test]
    fn records_read_back_as_written() {
        let mut speakers = Speakers::default();
        speakers.numbers.insert("Aya".into(), vec![0]);
        speakers.numbers.insert("???".into(), vec![3, 4]);
        for bytes in [
            [1u8, 0x06, 30, 4, 7, 2, 0, 0],
            [5, 0x00, 0, 0, 0, 0, 0, 0],
            [3, 0x00, 0, 0, 0, 1, 104, 0],
            [9, 0x80 | 0x08 | 0x20, 2, 4, 3, 0, 0, 0],
            [9, 0x80 | 0x40, 0, 0, 1, 0, 0, 0],
            [9, 0x80, 0, 0, 0, 0, 0, 0],
        ] {
            let named = record_named(bytes, "a/0/1/0", &speakers);
            assert_eq!(record_bytes(&named, &speakers), Ok(bytes), "{named:?}");
        }
    }
}
