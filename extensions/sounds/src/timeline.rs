//! A script, or a bank's chosen slot, as a sequencer shows one, drawn under the player that plays it: a ruler, a lane a
//! voice command, each note where the game plays it (`engine::score`) - the time its gate holds it, then the time its
//! release sounds - and the playhead, where the player is.
//!
//! - On the ruler, a click or a drag goes there; Space plays and pauses, Home goes to the start.
//! - The wheel zooms about the pointer; a drag on the lanes' background scrolls them.
//! - A click on a note selects its command, whose settings are shown under the lanes.
//! - In a script (not a bank, whose scripts are edited in their own nodes), a note's body drags it in time - its delay,
//!   the next command's taking up the difference so that nothing after it moves - and its end drags its gate; Delete
//!   removes it, "Copy after it" copies it, and its settings are typed in. Each is one undoable step.
//! - A script is made from nothing there too: an instrument of the project added to those it plays, a note added at
//!   its end.

#![expect(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "updates, points and settings of a few thousand at most, drawn and edited"
)]

use formats::spk::{Chunk, Command};
use pe2_ext::{Align, Asset, Color, Result, Role, Shape, Style, Table, Transport, Ui, Value, View, ViewportInput};

use crate::engine;

/// The lanes' labels' width, the ruler's height and a lane's, in points.
const LABEL: f32 = 170.0;
const RULER: f32 = 24.0;
const LANE: f32 = 28.0;
/// How near a note's end a press takes its gate rather than the note, in points.
const EDGE: f32 = 5.0;

fn theme(role: Role) -> Color {
    Color::Theme(role)
}

/// A script as played: its notes' spans, its commands, the instruments its programs are (by path), and what is edited.
struct Made {
    score: engine::Score,
    commands: Vec<Command>,
    instruments: Vec<Option<String>>,
    /// The instruments' references, as the script writes them.
    references: Vec<String>,
    /// The node's name: what its instruments' names share with it is left out of the lanes' labels.
    name: String,
    /// The script's node, for a bank's slot: where it is edited.
    script: Option<String>,
}

/// What the pointer is doing.
enum Drag {
    Seek,
    Scroll,
    /// A note dragged in time or by its end: its command, how far it has moved (updates).
    Move(usize, f32),
    Gate(usize, f32),
}

/// What the view keeps of itself across being made again (`Ui::leave_note`): its zoom, scroll and selection.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Kept {
    /// Points an update, the first update shown, the command selected.
    zoom: f32,
    scroll: f32,
    selected: Option<usize>,
}

impl Kept {
    fn read(note: Option<&str>) -> Option<Kept> {
        let mut parts = note?.split(' ');
        let zoom = parts.next()?.parse().ok()?;
        let scroll = parts.next()?.parse().ok()?;
        let selected = parts.next()?.parse().ok();
        Some(Kept { zoom, scroll, selected })
    }

    fn written(&self) -> String {
        let selected = self.selected.map_or_else(|| "-".to_string(), |s| s.to_string());
        format!("{} {} {selected}", self.zoom, self.scroll)
    }
}

/// What a script as played was made of: the project's revision, and the slot.
type Key = (u64, Option<usize>);

/// The timeline of a script or of a bank's chosen slot.
pub struct Timeline {
    /// Whether it is a bank's, by its chosen slot.
    slots: bool,
    made: Option<(Key, std::result::Result<Made, String>)>,
    kept: Option<Kept>,
    drag: Option<Drag>,
    /// A setting being typed, by its key: its text so far.
    typing: Vec<(String, String)>,
}

impl Timeline {
    fn boxed(slots: bool) -> Box<dyn View> {
        Box::new(Timeline { slots, made: None, kept: None, drag: None, typing: Vec::new() })
    }

    #[must_use]
    pub fn of_script() -> Box<dyn View> {
        Timeline::boxed(false)
    }

    #[must_use]
    pub fn of_bank() -> Box<dyn View> {
        Timeline::boxed(true)
    }
}

/// A node's script as played: a script's with its own instruments, a bank's slot's with the bank.
fn made_of(project: &dyn pe2_ext::Project, asset: &Asset<'_>, slot: Option<usize>) -> Result<Made> {
    let nodes = crate::preview::Looked(project);
    let settings = asset.fields();
    let (bank, entry, script) = if let Some(slot) = slot {
        let file = toml::from_str(&String::from_utf8(asset.read("bank.toml")?)?)?;
        let (bank, entry) = crate::preview::slot_bank(&nodes, &file, settings, slot)?;
        let at = crate::preview::slots(asset).into_iter().find(|(s, _)| *s == slot).map(|(_, script)| script);
        let scripts = asset.array("scripts");
        let script = at.and_then(|at| scripts.get(at)).and_then(|r| project.resolve(r)).map(|a| a.path().to_string());
        (bank, entry, script)
    } else {
        let (bank, entry) = crate::preview::script_bank(&nodes, settings.clone(), settings)?;
        (bank, entry, None)
    };
    let score = engine::score(&bank, entry, crate::preview::LIMIT)?;
    let commands = match bank.chunks.get(entry) {
        Some(Chunk::Entry { commands, .. }) => commands.clone(),
        _ => Vec::new(),
    };
    let references = crate::preview::references(settings, "instruments");
    let instruments =
        references.iter().map(|r| project.resolve(&Value::String(r.clone())).map(|a| a.path().to_string())).collect();
    let name = asset.path().rsplit('/').next().unwrap_or_default().to_string();
    Ok(Made { score, commands, instruments, references, name, script })
}

/// An instrument's name without what it shares with the node's, up to a `_` (`level_1_instrument_0` beside
/// `level_1_script_0`: `instrument_0`).
fn beside(path: Option<&String>, name: &str) -> String {
    let full = short(path);
    let shared = full.chars().zip(name.chars()).take_while(|(a, b)| a == b).count();
    let cut = full[..full.char_indices().nth(shared).map_or(full.len(), |(i, _)| i)].rfind('_').map_or(0, |i| i + 1);
    if cut > 0 && cut < full.len() { full[cut..].to_string() } else { full }
}

fn short(path: Option<&String>) -> String {
    path.map_or_else(|| "an instrument".to_string(), |p| p.rsplit('/').next().unwrap_or(p).to_string())
}

/// A label cut to fit a width at a size, an ellipsis where it is cut: its characters taken as about 0.55 of its size
/// wide.
fn fitted(text: &str, width: f32, size: f32) -> String {
    let room = (width / (size * 0.55)).max(0.0) as usize;
    if text.chars().count() <= room {
        return text.to_string();
    }
    let kept: String = text.chars().take(room.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// Updates between ticks on the ruler, for a zoom: the first of a few that leaves room between them.
fn tick_step(zoom: f32) -> f32 {
    [1.0, 2.0, 3.0, 6.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0].into_iter().find(|s| s * zoom >= 56.0).unwrap_or(1200.0)
}

/// The lanes, a voice command each, in the script's order.
fn lanes(commands: &[Command]) -> Vec<usize> {
    (0..commands.len()).filter(|&c| matches!(commands[c], Command::Voice(_))).collect()
}

/// A note's span as drawn, in updates: keyed on, keyed off, let go - moved or stretched while it is dragged.
fn span_of(span: &engine::Span, drag: Option<&Drag>) -> (f32, f32, f32) {
    let (start, off, end) = (span.start as f32, span.off.unwrap_or(0) as f32, span.end.unwrap_or(0) as f32);
    match drag {
        Some(Drag::Move(c, by)) if *c == span.command => (start + by, off + by, end + by),
        Some(Drag::Gate(c, by)) if *c == span.command => {
            (start, (off + by).max(start + 1.0), (end + by).max(start + 1.0))
        }
        _ => (start, off, end),
    }
}

fn rect(min: (f32, f32), max: (f32, f32), fill: Option<Color>, stroke: Option<(f32, Color)>, rounding: f32) -> Shape {
    Shape::Rect { min, max, fill, stroke, rounding }
}

fn line(from: (f32, f32), to: (f32, f32), width: f32, color: Color) -> Shape {
    Shape::Line { points: vec![from, to], width, color }
}

/// A place in a lane a note is drawn at: its command, and its held part's corners.
type Hit = (usize, (f32, f32, f32, f32));

impl Timeline {
    /// The view's place in time: points an update and the first update shown, fitted at first.
    fn kept(&mut self, note: Option<&str>, width: f32, updates: usize) -> Kept {
        *self.kept.get_or_insert_with(|| {
            Kept::read(note).unwrap_or(Kept {
                zoom: ((width - LABEL) / updates.max(30) as f32).max(0.5),
                scroll: 0.0,
                selected: None,
            })
        })
    }

    /// The ruler, the lanes and their notes, and the playhead: what is painted, and the notes' places.
    fn shapes(
        &self,
        made: &Made,
        kept: &Kept,
        (width, height): (f32, f32),
        playing: Option<f64>,
    ) -> (Vec<Shape>, Vec<Hit>) {
        let rate = engine::UPDATES as f32;
        let x = |u: f32| LABEL + (u - kept.scroll) * kept.zoom;
        let rows = lanes(&made.commands);
        let mut shapes = vec![
            rect((0.0, 0.0), (width, height), Some(theme(Role::Background)), None, 0.0),
            rect((0.0, 0.0), (width, RULER), Some(theme(Role::Raised)), None, 0.0),
            rect((0.0, RULER), (LABEL, height), Some(theme(Role::Raised)), None, 0.0),
        ];
        // The ruler's ticks, in seconds, and the grid under them.
        let step = tick_step(kept.zoom);
        let mut tick = (kept.scroll / step).floor() * step;
        while x(tick) < width {
            let at = x(tick);
            if at >= LABEL {
                shapes.push(line((at, RULER - 6.0), (at, height), 1.0, theme(Role::Border)));
                shapes.push(Shape::Text {
                    at: (at + 3.0, RULER / 2.0 - 2.0),
                    text: format!("{:.2} s", tick / rate),
                    size: 11.0,
                    color: theme(Role::Weak),
                    align: Align::Start,
                });
            }
            tick += step;
        }
        for (row, &command) in rows.iter().enumerate() {
            let top = RULER + LANE * row as f32;
            shapes.push(line((0.0, top + LANE), (width, top + LANE), 1.0, theme(Role::Border)));
            let Command::Voice(v) = &made.commands[command] else { continue };
            let instrument = made.instruments.get(usize::from(v.program)).and_then(Option::as_ref);
            let label =
                fitted(&format!("#{command} L{} {}", v.layer, beside(instrument, &made.name)), LABEL - 16.0, 12.0);
            let color = if kept.selected == Some(command) { Role::Text } else { Role::Weak };
            shapes.push(Shape::Text {
                at: (8.0, top + LANE / 2.0),
                text: label,
                size: 12.0,
                color: theme(color),
                align: Align::Start,
            });
        }
        // The notes: held, then sounding out.
        let mut hits = Vec::new();
        for span in &made.score.spans {
            let Some(row) = rows.iter().position(|&c| c == span.command) else { continue };
            let top = RULER + LANE * row as f32;
            let (start, off, end) = span_of(span, self.drag.as_ref());
            let (x0, x1, x2) = (x(start).max(LABEL), x(off).max(LABEL), x(end).max(LABEL));
            if x2 <= LABEL || x0 >= width {
                continue;
            }
            if x2 > x1 {
                let stroke = Some((1.0, theme(Role::Accent)));
                shapes.push(rect((x1, top + 9.0), (x2, top + LANE - 9.0), Some(theme(Role::Raised)), stroke, 2.0));
            }
            let held = (x0, top + 4.0, x1.max(x0 + 2.0), top + LANE - 4.0);
            let stroke = (kept.selected == Some(span.command)).then(|| (2.0, theme(Role::Text)));
            shapes.push(rect((held.0, held.1), (held.2, held.3), Some(theme(Role::Accent)), stroke, 3.0));
            hits.push((span.command, held));
        }
        if let Some(at) = playing {
            let at = x(at as f32 * rate);
            if (LABEL..width).contains(&at) {
                shapes.push(line((at, 0.0), (at, height), 2.0, theme(Role::Error)));
            }
        }
        (shapes, hits)
    }

    /// The lanes painted, and what the pointer and the keys did over them.
    fn lanes_ui(&mut self, ui: &mut Ui<'_>, made: &Made, editing: bool) -> Option<Edit> {
        let (width, _, _) = ui.available();
        let width = width.max(LABEL + 200.0);
        let height = RULER + LANE * lanes(&made.commands).len().max(1) as f32 + 2.0;
        let note = ui.note();
        let mut kept = self.kept(note.as_deref(), width, made.score.updates);
        let playing = ui.playback();
        let (shapes, hits) = self.shapes(made, &kept, (width, height), playing.map(|p| p.at));
        let input = ui.paint(width, height, &shapes);
        let edit = self.pointer(ui, &input, (&mut kept, made, &hits), (width, editing));
        // Playing past either edge, it follows.
        if let Some(playing) = playing.filter(|p| p.playing) {
            let at = playing.at as f32 * engine::UPDATES as f32;
            let shown = (width - LABEL) / kept.zoom;
            if at > kept.scroll + shown * 0.95 || at < kept.scroll {
                kept.scroll = (at - shown * 0.1).max(0.0);
            }
            ui.request_repaint();
        }
        if self.kept != Some(kept) {
            self.kept = Some(kept);
            ui.leave_note(&kept.written());
        }
        edit
    }

    /// What the pointer and the keys did over the lanes: where to go, what to select, what to change.
    fn pointer(
        &mut self,
        ui: &mut Ui<'_>,
        input: &ViewportInput,
        (kept, made, hits): (&mut Kept, &Made, &[Hit]),
        (width, editing): (f32, bool),
    ) -> Option<Edit> {
        let rate = engine::UPDATES;
        let update_at = |px: f32, k: &Kept| ((px - LABEL) / k.zoom + k.scroll).max(0.0);
        let mut edit = None;
        if input.pressed.0
            && let Some((px, py)) = input.pointer
        {
            let hit = hits
                .iter()
                .rev()
                .find(|(_, (x0, y0, x1, y1))| px >= *x0 - EDGE && px <= *x1 + EDGE && py >= *y0 && py <= *y1);
            if py < RULER && px > LABEL {
                self.drag = Some(Drag::Seek);
                ui.transport(Transport::Seek(f64::from(update_at(px, kept)) / rate));
            } else if let Some((command, (x0, _, x1, _))) = hit {
                kept.selected = Some(*command);
                self.typing.clear();
                if editing {
                    let at_end = (px - x1).abs() <= EDGE && px > x0 + EDGE;
                    self.drag = Some(if at_end { Drag::Gate(*command, 0.0) } else { Drag::Move(*command, 0.0) });
                }
            } else if px > LABEL {
                kept.selected = None;
                self.typing.clear();
                self.drag = Some(Drag::Scroll);
            }
        }
        if input.held.0 {
            match &mut self.drag {
                Some(Drag::Seek) => {
                    if let Some((px, _)) = input.pointer {
                        ui.transport(Transport::Seek(f64::from(update_at(px.clamp(LABEL, width), kept)) / rate));
                    }
                }
                Some(Drag::Scroll) => kept.scroll = (kept.scroll - input.drag.0 / kept.zoom).max(0.0),
                Some(Drag::Move(_, by) | Drag::Gate(_, by)) => *by += input.drag.0 / kept.zoom,
                None => {}
            }
        }
        if input.released.0 {
            edit = match self.drag.take() {
                Some(Drag::Move(command, by)) if by.round() != 0.0 => Some(Edit::Move(command, by.round() as i64)),
                Some(Drag::Gate(command, by)) if by.round() != 0.0 => {
                    let span = made.score.spans.iter().find(|s| s.command == command);
                    span.map(|s| {
                        let gate = (s.off.unwrap_or(0) as f32 + by - s.start as f32).round().max(1.0);
                        Edit::Gate(command, gate as i64)
                    })
                }
                _ => None,
            };
        }
        if input.hovered && input.scroll != 0.0 {
            let px = input.pointer.map_or(LABEL, |(px, _)| px.max(LABEL));
            let under = update_at(px, kept);
            kept.zoom = (kept.zoom * (input.scroll * 0.002).exp()).clamp(0.2, 200.0);
            kept.scroll = (under - (px - LABEL) / kept.zoom).max(0.0);
        }
        for key in &input.keys {
            match key.as_str() {
                "Space" => {
                    let playing = ui.playback().is_some_and(|p| p.playing);
                    ui.transport(if playing { Transport::Pause } else { Transport::Play });
                }
                "Home" => ui.transport(Transport::Seek(0.0)),
                "Delete" | "Backspace" if editing => edit = kept.selected.map(Edit::Delete),
                _ => {}
            }
        }
        edit
    }
}

/// A change to a script's commands.
enum Edit {
    /// A note moved by so many updates, a gate set, a note removed or copied after itself.
    Move(usize, i64),
    Gate(usize, i64),
    Delete(usize),
    Copy(usize),
    /// A setting of a command typed in.
    Set(usize, String, Value),
    /// A note added at the end, played on the instrument at this place among the script's.
    Add(usize),
}

/// A note as a script made new plays it: at once, on its layer's envelope, centered, at full volume, middle C, for half a
/// second.
fn new_note(instrument: usize) -> Table {
    let mut note = Table::new();
    for (key, value) in [
        ("op", Value::String("voice".into())),
        ("instrument", Value::Integer(i64::try_from(instrument).unwrap_or(0))),
        ("layer", Value::Integer(0)),
        ("delay", Value::Integer(0)),
        ("gate", Value::Integer(30)),
        ("pan", Value::Integer(-1)),
        ("volume", Value::Integer(127)),
        ("reverb", Value::Integer(-1)),
        ("priority", Value::Integer(0)),
        ("pitch", Value::Integer(60 * 128)),
    ] {
        note.insert(key.into(), value);
    }
    note
}

/// The key a command's time is under, where it waits: a voice's or a loop's delay, a wait's ticks.
fn timing(command: &Table) -> Option<&'static str> {
    match command.get("op").and_then(Value::as_str) {
        Some("voice" | "loop") => Some("delay"),
        Some("wait") => Some("ticks"),
        _ => None,
    }
}

fn number(table: &Table, key: &str) -> i64 {
    table.get(key).and_then(Value::as_integer).unwrap_or(0)
}

/// The next command after one that waits, where there is one.
fn next_timed(commands: &[Table], after: usize) -> Option<usize> {
    (after + 1..commands.len()).find(|&j| timing(&commands[j]).is_some())
}

/// The commands with a change made: a note moved or removed with the next command waiting the difference, so that
/// nothing after it moves.
fn edited(mut commands: Vec<Table>, edit: Edit) -> Vec<Table> {
    match edit {
        Edit::Move(i, by) => {
            let Some(key) = commands.get(i).and_then(timing) else { return commands };
            let own = number(&commands[i], key);
            let next = next_timed(&commands, i);
            let room = next.map_or(i64::MAX, |j| number(&commands[j], timing(&commands[j]).unwrap_or("delay")));
            let by = by.clamp(-own, room);
            commands[i].insert(key.into(), Value::Integer(own + by));
            if let Some(j) = next {
                let k = timing(&commands[j]).unwrap_or("delay");
                let theirs = number(&commands[j], k);
                commands[j].insert(k.into(), Value::Integer(theirs - by));
            }
        }
        Edit::Gate(i, gate) => {
            if let Some(command) = commands.get_mut(i) {
                command.insert("gate".into(), Value::Integer(gate.clamp(1, i64::from(u16::MAX))));
            }
        }
        Edit::Delete(i) => {
            if i < commands.len() {
                let own = timing(&commands[i]).map_or(0, |k| number(&commands[i], k));
                if let Some(j) = next_timed(&commands, i) {
                    let k = timing(&commands[j]).unwrap_or("delay");
                    let theirs = number(&commands[j], k);
                    commands[j].insert(k.into(), Value::Integer(theirs + own));
                }
                commands.remove(i);
            }
        }
        Edit::Copy(i) => {
            if let Some(command) = commands.get(i) {
                let mut copy = command.clone();
                if let Some(key) = timing(&copy) {
                    copy.insert(key.into(), Value::Integer(0));
                }
                commands.insert(i + 1, copy);
            }
        }
        Edit::Set(i, key, value) => {
            if let Some(command) = commands.get_mut(i) {
                command.insert(key, value);
            }
        }
        Edit::Add(instrument) => commands.push(new_note(instrument)),
    }
    commands
}

/// What a script is made of, added to: an instrument of the project to those it plays, a note at its end. The
/// instruments' references as they become, where one was added.
fn adding_ui(ui: &mut Ui<'_>, made: &Made, selected: Option<usize>, edit: &mut Option<Edit>) -> Option<Vec<Value>> {
    let mut added = None;
    ui.horizontal(|ui| {
        let playing: Vec<String> = made.instruments.iter().map(|p| short(p.as_ref())).collect();
        let says = if playing.is_empty() { "No instruments yet".to_string() } else { playing.join(", ") };
        ui.styled(&format!("Instruments: {says}"), Style::Weak);
        let project = ui.project();
        let mut offered: Vec<(String, String)> = project
            .find("pe2.sounds@instrument")
            .iter()
            .filter_map(|a| Some((a.path().to_string(), a.uid()?.to_string())))
            .filter(|(path, _)| !made.instruments.iter().flatten().any(|p| p == path))
            .collect();
        offered.sort();
        let names: Vec<String> = offered.iter().map(|(path, _)| path.clone()).collect();
        if names.is_empty() {
            ui.styled("· no other instrument in the project to add", Style::Weak);
        } else if let Some(chosen) = ui.combo("add instrument", "Add an instrument", &names) {
            let mut references: Vec<Value> = made.references.iter().map(|r| Value::String(r.clone())).collect();
            references.push(Value::String(offered[chosen].1.clone()));
            added = Some(references);
        }
        if !made.instruments.is_empty() && ui.button("Add a note") {
            // On the instrument of the note selected, else the first.
            let on = selected.and_then(|s| match made.commands.get(s) {
                Some(Command::Voice(v)) => Some(usize::from(v.program)),
                _ => None,
            });
            *edit = Some(Edit::Add(on.unwrap_or(0)));
        }
    });
    added
}

impl Timeline {
    /// One setting of the selected command: typed in, kept as typed until it is committed.
    fn typed(&mut self, ui: &mut Ui<'_>, (selected, key, value): (usize, &str, &str)) -> Option<Edit> {
        let at = self.typing.iter().position(|(k, _)| k == key);
        let mut text = at.map_or_else(|| value.to_string(), |at| self.typing[at].1.clone());
        let committed = ui.text_edit_hinted(&format!("{selected}/{key}"), &mut text, "", 56.0);
        match at {
            Some(at) => self.typing[at].1.clone_from(&text),
            None if text != value => self.typing.push((key.to_string(), text.clone())),
            None => {}
        }
        if !committed {
            return None;
        }
        self.typing.retain(|(k, _)| k != key);
        let parsed = if key == "pitch" {
            text.trim().parse::<f64>().ok().map(|st| (st * 128.0).round() as i64)
        } else {
            text.trim().parse::<i64>().ok()
        };
        parsed.filter(|_| text != value).map(|parsed| Edit::Set(selected, key.to_string(), Value::Integer(parsed)))
    }

    /// The selected command's settings: typed in, in a script; read, in a bank, with where its script is edited.
    fn settings_ui(&mut self, ui: &mut Ui<'_>, made: &Made, editing: bool) -> Option<Edit> {
        let selected = self.kept.and_then(|k| k.selected)?;
        let Some(Command::Voice(v)) = made.commands.get(selected) else { return None };
        let mut edit = None;
        let instrument = made.instruments.get(usize::from(v.program)).and_then(Option::as_ref);
        let fields: [(&str, &str, String); 8] = [
            ("layer", "Layer", v.layer.to_string()),
            ("delay", "Delay", v.delay.to_string()),
            ("gate", "Gate", v.gate.to_string()),
            ("pitch", "Pitch (st)", format!("{:.2}", f64::from(v.pitch) / 128.0)),
            ("volume", "Volume", v.volume.to_string()),
            ("pan", "Pan", v.pan.to_string()),
            ("reverb", "Reverb", v.reverb.to_string()),
            ("priority", "Priority", v.priority.to_string()),
        ];
        ui.horizontal(|ui| {
            ui.styled(&format!("#{selected}"), Style::Strong);
            if editing && made.instruments.len() > 1 {
                let names: Vec<String> = made.instruments.iter().map(|p| short(p.as_ref())).collect();
                if let Some(chosen) = ui.combo("instrument", &short(instrument), &names) {
                    edit = Some(Edit::Set(
                        selected,
                        "instrument".into(),
                        Value::Integer(i64::try_from(chosen).unwrap_or(0)),
                    ));
                }
            } else {
                ui.label(&short(instrument));
            }
            let own = |is: bool| if is { "own" } else { "layer's" };
            ui.styled(
                &format!("ADSR {} · pitch envelope {}", own(v.adsr.is_some()), own(v.envelope.is_some())),
                Style::Weak,
            );
        });
        ui.horizontal(|ui| {
            for (key, label, value) in &fields {
                ui.styled(label, Style::Weak);
                if editing {
                    if let Some(typed) = self.typed(ui, (selected, key, value)) {
                        edit = Some(typed);
                    }
                } else {
                    ui.label(value);
                }
            }
        });
        if editing {
            ui.horizontal(|ui| {
                if ui.button("Copy after it") {
                    edit = Some(Edit::Copy(selected));
                }
                if ui.button("Remove") {
                    edit = Some(Edit::Delete(selected));
                }
            });
        } else if let Some(script) = &made.script
            && ui.button("Edit its script")
        {
            ui.navigate(script);
        }
        edit
    }
}

impl View for Timeline {
    fn show(&mut self, ui: &mut Ui<'_>) {
        let Some(asset) = ui.asset() else { return };
        let path = asset.path().to_string();
        let slot = if self.slots {
            let chosen = ui.project().chosen(&path, "picture").and_then(|s| s.parse().ok());
            let Some(slot) = chosen.or_else(|| crate::preview::slots(&asset).first().map(|(slot, _)| *slot)) else {
                ui.label("It has no slot that plays a script.");
                return;
            };
            Some(slot)
        } else {
            None
        };
        let key = (ui.project().revision(), slot);
        if self.made.as_ref().is_none_or(|(made, _)| *made != key) {
            let made = made_of(ui.project(), &asset, slot).map_err(|why| format!("{why:#}"));
            self.made = Some((key, made));
        }
        let commands: Vec<Table> = asset.array("commands").iter().filter_map(|c| c.as_table().cloned()).collect();
        let editing = !self.slots && ui.editable();
        let Some((key, made)) = self.made.take() else { return };
        let made = match made {
            Ok(made) => made,
            Err(why) => {
                ui.error(&why);
                self.made = Some((key, Err(why)));
                return;
            }
        };
        let seconds = made.score.updates as f64 / engine::UPDATES;
        let held = if made.score.stopped { " · held: stopped as the game would stop it" } else { "" };
        ui.styled(&format!("{seconds:.2} s · {} notes{held}", made.score.spans.len()), Style::Weak);
        let mut edit = self.lanes_ui(ui, &made, editing);
        if let Some(from_settings) = self.settings_ui(ui, &made, editing) {
            edit = Some(from_settings);
        }
        if editing && let Some(instruments) = adding_ui(ui, &made, self.kept.and_then(|k| k.selected), &mut edit) {
            ui.set_step("instruments", Some(&Value::Array(instruments)));
        }
        if let Some(edit) = edit.filter(|_| editing) {
            let changed: Vec<Value> = edited(commands, edit).into_iter().map(Value::Table).collect();
            ui.set_step("commands", Some(&Value::Array(changed)));
        }
        self.made = Some((key, Ok(made)));
    }
}

#[cfg(test)]
mod tests {
    use super::beside;

    #[test]
    fn an_instrument_is_named_without_what_it_shares_with_its_script() {
        let name = |path: &str| beside(Some(&path.to_string()), "level_1_script_0");
        assert_eq!(name("pe/antibody/sounds/level_1_instrument_0"), "instrument_0");
        assert_eq!(name("pe/shared/footstep"), "footstep");
        assert_eq!(beside(None, "x"), "an instrument");
    }
}
