//! pe2.sounds: sound - samples, the instruments that play them, the scripts that play instruments, and the banks that
//! bind them as the game loads them.
//!
//! A sample is a WAV, made into the SPU-ADPCM body the game stores by Sony's own encoder, in the mode its node states
//! where normal does not reproduce it (`formats::vag`). An instrument is a program of a bank: its layers, each a sample
//! and how it plays. A script is what a sound id starts: its controls and its commands - voices playing an
//! instrument's layer, loops, waits. A bank binds them as the game loads them: its instruments at their program
//! numbers, its scripts at their slots, its samples in its pool, its image laid out as the disc has it (`bank`).

pub mod bank;
pub mod engine;
pub mod play;
pub mod preview;
pub mod reverb;
pub mod spu;
pub mod timeline;
pub mod wav;

use pe2_ext::ty::{self, Files, Registered};
use pe2_ext::{Field, Icon, Registry, Result, Signature, Table, Tint, Typed, Value, anyhow};

pe2_ext::extension!(register);

pe2_ext::media_type!(
    /// A WAV file, by its media type: whoever recognizes it.
    pub WavFile = "audio/wav"
);

/// A sample's native form: its SPU-ADPCM body as a bank stores it.
pub struct Vag;

impl Registered for Vag {
    const NAME: &str = "pe2.sounds@vag";
    const LABEL: &str = "SPU-ADPCM sample";

    fn signatures() -> Vec<Signature> {
        vec![Signature::Name(VAG.into())]
    }
}

impl Files for Vag {}

/// An instrument, a script and a bank, as their files.
macro_rules! text_type {
    ($marker:ident, $name:literal, $label:literal, $file:literal) => {
        pub struct $marker;

        impl Registered for $marker {
            const NAME: &str = $name;
            const LABEL: &str = $label;

            fn signatures() -> Vec<Signature> {
                vec![Signature::Name($file.into())]
            }
        }

        impl Files for $marker {}
    };
}

text_type!(InstrumentFile, "pe2.sounds@instrument_file", "Instrument", "instrument.toml");
pe2_ext::media_type!(
    /// A sound script, as its node holds it: its controls and commands are what it is set to (its generated
    /// `settings.toml`, beneath what a layer sets over it), its instruments what it names.
    pub Script = "pe2.sounds@sound_script"
);

/// A script's stack: what it is set to, played.
const SCRIPT: &str = r#"[[stack]]
id = "script"
op = "pe2.sounds@sound_script"

[[stack]]
op = "output"
name = "native"

[[stack]]
op = "pe2.sounds@play_script"
script = { from = "script" }

[[stack]]
op = "preview"
name = "played"
"#;
text_type!(BankFile, "pe2.sounds@bank_file", "Sound bank", "bank.toml");
text_type!(ReverbFile, "pe2.sounds@reverb_file", "Reverb mode", "reverb.toml");

/// The reverb a project's sounds are heard through: the mode the game sets, as its node's file says it.
pub struct Reverb;

impl Reverb {
    /// Its node's type.
    pub const NODE: &str = "pe2.sounds@reverb";
}

/// The area's reverb setting a preview plays in: left out, the game's default, most areas' (`engine::ROOM`).
fn room_field() -> Field {
    let choices = [(-1, "No reverb"), (2, "A stage's two reverberant areas"), (3, "The Mine Gorge")];
    Field { absent: Some("Most areas".into()), ..Field::new("room").label("Room reverb").choices(choices) }
}

/// A sample's native file.
pub const VAG: &str = "native.vag";

/// The modes Sony's encoder runs in, by the filters it weighs.
const MODES: [(&str, &str); 4] =
    [("normal", "Normal"), ("high", "High frequencies"), ("low", "Low frequencies"), ("4bit", "4-bit")];

/// A sample's sound, by the rate it is written at (`rate`, 44100 where none is set).
fn rate_of(settings: &Table) -> u32 {
    settings.get("rate").and_then(Value::as_integer).and_then(|r| u32::try_from(r).ok()).unwrap_or(44100)
}

/// A sample's body from its WAV, as Sony's encoder makes it in a mode: whole 28-sample frames, its loop starting on one.
///
/// # Errors
/// It is no 16-bit mono WAV, it is not whole frames, or there is no such mode.
pub fn body_of_wav(bytes: &[u8], mode: &str) -> Result<Vec<u8>> {
    let sound = wav::read(bytes)?;
    anyhow::ensure!(sound.pcm.len().is_multiple_of(28), "{} samples, not whole 28-sample frames", sound.pcm.len());
    let loop_start = match sound.loop_start {
        Some(at) if at % 28 != 0 => anyhow::bail!("its loop starts at {at}, within a 28-sample frame"),
        at => at.map(|at| at / 28),
    };
    let pcm: Vec<i64> = sound.pcm.iter().map(|&s| i64::from(s)).collect();
    let weights = formats::vag::weights(mode).ok_or_else(|| anyhow!("no mode {mode}"))?;
    Ok(formats::vag::encode(&pcm, &weights, loop_start))
}

/// A sample's WAV from its body: input the encoder turns into it, at its rate.
///
/// # Errors
/// No input reproduces it.
pub fn wav_of(body: &[u8], rate: u32) -> Result<(Vec<u8>, &'static str)> {
    let decoded = formats::vag::decode(body).map_err(|e| anyhow!(e))?;
    let pcm = decoded
        .pcm
        .iter()
        .map(|&v| i16::try_from(v).map_err(|_| anyhow!("an input of {v} is no 16-bit sample")))
        .collect::<Result<_>>()?;
    let wav = wav::Wav { pcm, loop_start: decoded.loop_start.map(|l| 28 * l), rate };
    Ok((wav::write(&wav), decoded.mode))
}

fn samples(r: &mut Registry) {
    let wav_source = r
        .op("wav_source")
        .label("WAV, written")
        .input::<Vag>("native")
        .output::<ty::File<WavFile>>()
        .provides("wav_file")
        .exports("WAV file", "wav")
        .run(|call| {
            let native = call.take::<Vag>(0)?;
            let body = call.read(native.file()?)?;
            let (bytes, _) = wav_of(&body, rate_of(&pe2_ext::merged(call.settings, &native.settings)))?;
            Ok(Typed::new("audio/wav", vec![pe2_ext::File::new("sound.wav", bytes)]))
        });
    let _ = r
        .op("wav")
        .label("WAV")
        .input::<WavFile>("source")
        .output::<Vag>()
        .field(Field::new("mode").label("Encoder mode").choices(MODES))
        .field(Field::new("rate").label("Rate").read_only())
        .derives("rate")
        .inverse(&wav_source)
        .priority(10)
        .run(|call| {
            let source = call.take::<WavFile>(0)?;
            let bytes = call.read(source.file()?)?;
            let sound = wav::read(&bytes)?;
            let body = body_of_wav(&bytes, call.settings.get("mode").and_then(Value::as_str).unwrap_or("normal"))?;
            // What it makes says the rate its WAV is written at, which its node does not.
            let mut made = Typed::new(Vag::NAME, vec![pe2_ext::File::new(VAG, body)]);
            made.settings.insert("rate".into(), Value::Integer(i64::from(sound.rate)));
            Ok(made)
        });
}

/// A sample played raw (`play`).
fn preview(r: &mut Registry) {
    let _ = r.op("play").label("Played").input::<Vag>("body").output::<ty::Sound>().streams(|call| {
        let native = call.take::<Vag>(0)?;
        let body = call.read(native.file()?)?;
        Ok(Box::new(play::Played::new(&body, rate_of(&pe2_ext::merged(call.settings, &native.settings)))))
    });
}

/// A script, and a bank's slot, played as the game plays them (`preview`).
fn played(r: &mut Registry) {
    let _ = r
        .op("play_script")
        .label("Played")
        .input::<Script>("script")
        .output::<ty::Sound>()
        .field(room_field())
        .streams(|call| {
            let script = call.take::<Script>(0)?;
            Ok(Box::new(preview::Played::new(preview::script(call, &script)?)))
        });
    let _ = r
        .op("play_slot")
        .label("Slot played")
        .input::<BankFile>("bank")
        .output::<ty::Sound>()
        .field(room_field())
        .parameter::<String>("slot")
        .members(|asset| {
            preview::slots(asset).into_iter().map(|(slot, _)| (slot.to_string(), slot.to_string())).collect()
        })
        .streams(|call| {
            let slot: String = call.parameter()?;
            let bank = call.take::<BankFile>(0)?;
            Ok(Box::new(preview::Played::new(preview::slot(call, &bank, slot.parse()?)?)))
        });
}

/// What is played, as a WAV file (`played_wav`): a script, a bank's slot, a sample - exported as heard.
fn recorded(r: &mut Registry) {
    let _ = r
        .op("played_wav")
        .label("WAV, as played")
        .input::<ty::Stream<ty::Sound>>("played")
        .output::<ty::File<WavFile>>()
        .provides("played_wav")
        .exports("WAV file, as played", "wav")
        .run(|call| {
            let stream = call.take::<ty::Stream<ty::Sound>>(0)?;
            let (mut pcm, mut format) = (Vec::new(), None);
            while let Some(item) = stream.next()? {
                let parts = if let pe2_ext::Data::List(parts) = item { parts } else { vec![item] };
                for part in parts {
                    if let pe2_ext::Data::Audio(heard) = part {
                        if heard.format != pe2_ext::media::SampleFormat::S16Interleaved {
                            anyhow::bail!("it takes s16-interleaved sound, not {}", heard.format.name());
                        }
                        format.get_or_insert((heard.channels, heard.sample_rate));
                        let bytes = heard.buffer.read();
                        pcm.extend(bytes.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)));
                    }
                }
            }
            let (channels, rate) = format.ok_or_else(|| anyhow!("nothing was played"))?;
            let channels = u16::try_from(channels).map_err(|_| anyhow!("{channels} channels"))?;
            let bytes = wav::write_interleaved(&pcm, channels, rate);
            Ok(Typed::new("audio/wav", vec![pe2_ext::File::new("played.wav", bytes)]))
        });
}

/// The reverb mode at two places of the executable (`args`: `registers`, the address of its 32 registers, and
/// `work_area`, of the word saying where its work area starts), as its node's file (`node`).
///
/// # Errors
/// It is no PS-X executable, or the addresses are not in it.
pub fn extract_reverb(exe: &[u8], args: &Table) -> Result<Vec<(String, Vec<u8>)>> {
    anyhow::ensure!(exe.starts_with(b"PS-X EXE") && exe.len() > 0x800, "not a PS-X executable");
    let load = u32::from_le_bytes(exe[0x18..0x1C].try_into()?);
    let at = |key: &str| -> Result<usize> {
        let address = args.get(key).and_then(Value::as_integer).ok_or_else(|| anyhow!("no {key}"))?;
        let offset = u32::try_from(address)?.checked_sub(load).ok_or_else(|| anyhow!("{key} is before the image"))?;
        Ok(usize::try_from(offset)? + 0x800)
    };
    let word = |offset: usize| -> Result<u16> {
        Ok(u16::from_le_bytes(exe.get(offset..offset + 2).ok_or_else(|| anyhow!("past the image"))?.try_into()?))
    };
    let mut registers = [0; 32];
    let first = at("registers")?;
    for (n, register) in registers.iter_mut().enumerate() {
        *register = word(first + 2 * n)?;
    }
    let mode = reverb::Mode { registers, work_area: word(at("work_area")?)? };
    let node = args.get("node").and_then(Value::as_str).ok_or_else(|| anyhow!("no node"))?;
    Ok(vec![(format!("{node}/reverb.toml"), bank::text(&mode.table()).into_bytes())])
}

/// What a plan runs of them: a bank's nodes' files - its own, its instruments', scripts' and samples' - from its
/// bytes (`extract::extract_bank`).
fn planned(r: &mut Registry) {
    let _ = r
        .op("extract_bank")
        .label("A bank and what it binds, read from its bytes")
        .port("chunks", "bytes", true)
        .output_described("bytes")
        .run_described(|call| {
            let given = call.inputs.first().map_or(&[][..], Vec::as_slice);
            let chunk = match given.first() {
                Some(pe2_ext::Data::Typed(typed)) => call.read(typed.file()?)?,
                _ => anyhow::bail!("no bank given"),
            };
            let files = extract_bank(&chunk, call.settings)?
                .into_iter()
                .map(|(name, bytes)| pe2_ext::File::new(&name, bytes))
                .collect();
            Ok(pe2_ext::Data::Typed(Typed::new("bytes", files)))
        });
    let _ = r
        .op("extract_reverb")
        .label("A reverb mode, read from the executable")
        .port("chunks", "bytes", true)
        .output_described("bytes")
        .run_described(|call| {
            let given = call.inputs.first().map_or(&[][..], Vec::as_slice);
            let exe = match given.first() {
                Some(pe2_ext::Data::Typed(typed)) => call.read(typed.file()?)?,
                _ => anyhow::bail!("no executable given"),
            };
            let files =
                extract_reverb(&exe, call.settings)?.into_iter().map(|(n, b)| pe2_ext::File::new(&n, b)).collect();
            Ok(pe2_ext::Data::Typed(Typed::new("bytes", files)))
        });
}

/// A bank's nodes' files, each named by its node's path: the bank's `bank.toml`, each instrument's
/// `instrument.toml`, each script's `settings.toml` (what its node is set to) and each sample's `sound.wav`. `args`: the bank's path (`bank`);
/// the paths of its `instruments` in program order and of its `scripts` in image order; its `samples` in pool order,
/// each a path, its `rate` and its `mode` where not normal. What each instrument's and script's file names is a
/// position in its own list - an instrument's samples, a script's instruments, each in order of first use - which its
/// declaration lists in the same order (`bank::Binding::own`).
///
/// # Errors
/// The bank is not as its declarations say, or a sample cannot be decoded in the mode its node states.
pub fn extract_bank(blob: &[u8], args: &Table) -> Result<Vec<(String, Vec<u8>)>> {
    let bank = bank::read(blob)?;
    let path = args.get("bank").and_then(Value::as_str).ok_or_else(|| anyhow!("no bank"))?;
    let list = |key: &str| -> Result<Vec<Value>> {
        Ok(args.get(key).and_then(Value::as_array).ok_or_else(|| anyhow!("no {key}"))?.clone())
    };
    let (instruments, scripts, samples) = (list("instruments")?, list("scripts")?, list("samples")?);
    let path_of = |v: &Value| {
        v.as_str()
            .or_else(|| v.get("path").and_then(Value::as_str))
            .map(str::to_string)
            .ok_or_else(|| anyhow!("no path"))
    };
    anyhow::ensure!(
        instruments.len() == bank.programs.len(),
        "{} instruments declared, the bank has {}",
        instruments.len(),
        bank.programs.len()
    );
    let (file, instrument_files, script_files) = bank::split(&bank, &bank::Binding::own(&bank))?;
    anyhow::ensure!(
        scripts.len() == script_files.len(),
        "{} scripts declared, the bank has {}",
        scripts.len(),
        script_files.len()
    );
    let text = |t: &Table| -> Result<Vec<u8>> { Ok(bank::text(t).into_bytes()) };
    let mut out = vec![(format!("{path}/bank.toml"), text(&file)?)];
    // A file once, where the bank binds the same instrument, script or sample at several places.
    let mut add = |name: String, bytes: Vec<u8>| -> Result<()> {
        match out.iter().find(|(n, _)| *n == name) {
            Some((_, made)) => anyhow::ensure!(*made == bytes, "{name} is made twice, not alike"),
            None => out.push((name, bytes)),
        }
        Ok(())
    };
    for (t, made) in instruments.iter().zip(&instrument_files) {
        add(format!("{}/instrument.toml", path_of(t)?), text(made)?)?;
    }
    for (t, made) in scripts.iter().zip(&script_files) {
        add(format!("{}/settings.toml", path_of(t)?), text(made)?)?;
    }
    anyhow::ensure!(
        samples.len() == bank.pool.len(),
        "{} samples declared, the pool holds {}",
        samples.len(),
        bank.pool.len()
    );
    for (t, item) in samples.iter().zip(&bank.pool) {
        let formats::spk::PoolItem::Sample(body) = item else { anyhow::bail!("a raw pool item is not handled") };
        let settings = t.as_table().ok_or_else(|| anyhow!("a sample is no table"))?;
        let declared = settings.get("mode").and_then(Value::as_str).unwrap_or("normal");
        let (wav, mode) = wav_of(body, rate_of(settings))?;
        anyhow::ensure!(mode == declared, "{}: declares mode {declared}; its body needs {mode}", path_of(t)?);
        add(format!("{}/sound.wav", path_of(t)?), wav)?;
    }
    Ok(out)
}

/// A sample's stack: its WAV, made into its body.
const SOUND: &str = r#"[[stack]]
op = "file"
file = "{file}"

[[stack]]
id = "body"
op = "pe2.sounds@wav"

[[stack]]
op = "output"
name = "native"
retain = "cache"
when = "eager"

[[stack]]
op = "pe2.sounds@play"
body = { from = "body" }

[[stack]]
op = "preview"
name = "played"
"#;

/// An instrument's, a script's and a bank's stack: its file, and what plays it.
fn file_stack(file: &str, played: Option<(&str, &str)>) -> String {
    let mut stack = format!(
        "[[stack]]\nid = \"file\"\nop = \"file\"\nfile = \"{file}\"\n\n[[stack]]\nop = \"output\"\nname = \"native\"\n"
    );
    if let Some((op, port)) = played {
        stack.push_str(
            &[
                "\n[[stack]]",
                &format!("op = \"pe2.sounds@{op}\""),
                &format!("{port} = {{ from = \"file\" }}"),
                "\n[[stack]]\nop = \"preview\"\nname = \"played\"\n",
            ]
            .join("\n"),
        );
    }
    stack
}

#[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
fn register(r: &mut Registry) -> Result<()> {
    let _ = r.value_type::<Vag>();
    let _ = r.value_type::<InstrumentFile>();
    let _ = r.value_type::<BankFile>();
    let _ = r.value_type::<ReverbFile>();
    samples(r);
    preview(r);
    played(r);
    recorded(r);
    planned(r);
    for (name, label, of, icon) in [
        ("sound", "Sound", Vag::NAME, Icon::Waveform),
        ("instrument", "Instrument", InstrumentFile::NAME, Icon::Speaker),
        ("sound_bank", "Sound bank", BankFile::NAME, Icon::Stack),
        ("reverb", "Reverb mode", ReverbFile::NAME, Icon::Speaker),
    ] {
        let ty = r.named_type(name).alias(name).label(label).of(of).icon(icon, Tint::Sound);
        let ty = if name == "sound_bank" {
            ty.contents(preview::contents).view("Timeline", |_| timeline::Timeline::of_bank())
        } else {
            ty
        };
        ty.register();
    }
    r.named_type("sound_script")
        .alias("sound_script")
        .label("Sound script")
        .data()
        .stack(SCRIPT)
        .icon(Icon::Music, Tint::Sound)
        .field(Field::new("instruments").custom())
        .field(Field::new("commands").custom())
        // What a script made new starts from: what a bank makes of one that leaves them out (`bank::join`).
        .field(Field::new("volume").label("Volume").default_value(127))
        .field(Field::new("pan").label("Pan").default_value(64))
        .field(Field::new("instances").label("Instances").default_value(1))
        .field(Field::new("retrigger").label("Retrigger").default_value(0))
        .field(Field::new("priority").label("Priority").default_value(0))
        .field(Field::new("flags").label("Flags").default_value(0))
        .field(Field::new("unknown_4").label("Header byte 4").default_value(0))
        .view("Timeline", |_| timeline::Timeline::of_script())
        .register();
    drop(r.template("sound", "pe2.sounds@sound", SOUND).label("Sound, from a WAV").files(&["*.wav"]));
    for (template, label, file, played) in [
        ("instrument", "Instrument", "instrument.toml", None),
        ("sound_bank", "Sound bank", "bank.toml", Some(("play_slot", "bank"))),
        ("reverb", "Reverb mode", "reverb.toml", None),
    ] {
        let stack = file_stack(file, played);
        drop(r.template(template, &format!("pe2.sounds@{template}"), &stack).label(label).files(&[file]));
    }
    Ok(())
}
