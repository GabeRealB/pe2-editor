//! Extracting movies (`pe2.movies@clips`). A disc's movie file (`pe2.game`'s `stream`) is a list of entries, each a
//! movie, its clips back to back in the entries' order; a movie is located by the entries naming it - the file's slot
//! on a disc, and the entry's place - and declares only its sha1. A clip starts where a video sector's frame number
//! drops below the one before it (each clip counts its frames from 1); the records of the stages start exactly there.
//! The clips are found once a file, reading it a window at a time - a movie file is hundreds of megabytes - and each
//! movie's sectors are read alone and checked; a movie on both discs is both files' entry, the same bytes.

use formats::str::{SECTOR, Sector};
use pe2_ext::{Declared, Discs, Extraction, Made, Result, Table, Value, anyhow, bail};
use sha1::{Digest, Sha1};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use crate::NATIVE;

/// Sectors read at once while finding a file's clips: about 9.5 MB.
const WINDOW: u64 = 4096;

/// Where a movie is: a release's disc slot holding a movie file, and the movie's place among its entries.
struct Place {
    release: String,
    disc: String,
    slot: String,
    entry: usize,
}

/// A session: the movies' declarations, where each is (by the movie files' entries), and each movie file's clips as
/// found, by (release, disc, slot).
struct Clips {
    movies: BTreeMap<String, Table>,
    places: HashMap<String, Vec<Place>>,
    found: HashMap<(String, String, String), Vec<(u64, u64)>>,
}

/// Begin a session: the movies, by path, and their places in the movie files' entries.
///
/// # Errors
/// A movie file's entry names no movie, or the file is in a release without saying where.
pub fn begin(declared: &dyn Declared) -> Result<Box<dyn Extraction>> {
    let movies = declared.declarations("movie").into_iter().collect();
    let mut places: HashMap<String, Vec<Place>> = HashMap::new();
    for (file, declaration) in declared.declarations("stream") {
        let Some(entries) = declaration.get("entries").and_then(Value::as_table) else { continue };
        let releases = declaration.get("from").and_then(Value::as_table).cloned().unwrap_or_default();
        for (release, from) in releases {
            let at = from.get("at").and_then(Value::as_array).and_then(|a| a.first());
            let field = |key: &str| at.and_then(|a| a.get(key)).and_then(Value::as_str).map(str::to_string);
            let (Some(disc), Some(slot)) = (field("disc"), field("slot")) else {
                bail!("{file}: {release} says not where it is");
            };
            for (entry, (id, value)) in entries.iter().enumerate() {
                let path = value
                    .get("movie")
                    .and_then(|m| m.get("path"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("{file}: its entry {id} names no movie"))?;
                let movie = path.trim_start_matches("res://").trim_end_matches(".asset").to_string();
                places.entry(movie).or_default().push(Place {
                    release: release.clone(),
                    disc: disc.clone(),
                    slot: slot.clone(),
                    entry,
                });
            }
        }
    }
    Ok(Box::new(Clips { movies, places, found: HashMap::new() }))
}

/// A movie file's clips: their sectors' ranges, `[start, end)`.
fn clips(discs: &mut dyn Discs, at: &Table) -> Result<Vec<(u64, u64)>> {
    let sectors = discs.size(at)? / SECTOR as u64;
    let mut clips = vec![];
    let (mut begin, mut last) = (0u64, 0u32);
    let mut i = 0;
    while i < sectors {
        let count = WINDOW.min(sectors - i);
        let bytes = discs.read_range(at, i * SECTOR as u64, usize::try_from(count)? * SECTOR)?;
        for (k, sector) in bytes.as_chunks::<SECTOR>().0.iter().enumerate() {
            if let Sector::Video(chunk, _) = Sector::raw(sector) {
                let at = i + k as u64;
                if chunk.frame < last && at > 0 {
                    clips.push((begin, at));
                    begin = at;
                }
                last = chunk.frame;
            }
        }
        i += count;
    }
    clips.push((begin, sectors));
    Ok(clips)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

impl Extraction for Clips {
    fn extract(&mut self, path: &str, release: &str, discs: &mut dyn Discs) -> Result<Made> {
        let declared = self.movies.get(path).ok_or_else(|| anyhow!("{path}: not a movie"))?;
        let from = declared
            .get("from")
            .and_then(|f| f.get(release))
            .ok_or_else(|| anyhow!("{path}: not in {release}"))?;
        let want = from.get("sha1").and_then(Value::as_str).ok_or_else(|| anyhow!("{path}: {release} gives no sha1"))?;
        let mut native = None;
        for place in self.places.get(path).into_iter().flatten().filter(|p| p.release == release) {
            let (disc, slot, entry) = (&place.disc, &place.slot, place.entry);
            let mut file = Table::new();
            file.insert("disc".into(), Value::String(disc.clone()));
            file.insert("slot".into(), Value::String(slot.clone()));
            let key = (release.to_string(), disc.clone(), slot.clone());
            if !self.found.contains_key(&key) {
                let found = clips(discs, &file)?;
                self.found.insert(key.clone(), found);
            }
            let found = &self.found[&key];
            let &(start, end) = found
                .get(entry)
                .ok_or_else(|| anyhow!("{path}: {disc} {slot} holds {} clips, not an entry {entry}", found.len()))?;
            let bytes = discs.read_range(&file, start * SECTOR as u64, usize::try_from(end - start)? * SECTOR)?;
            let got = hex(&Sha1::digest(&bytes));
            if got != want {
                bail!("{path}: {disc} {slot} entry {entry} has sha1 {got}, the declaration {want}");
            }
            native.get_or_insert(bytes);
        }
        let native = native.ok_or_else(|| anyhow!("{path}: no movie file of {release} lists it"))?;
        Ok(vec![(NATIVE.to_string(), native)])
    }
}
