//! Extracting movies (`pe2.str@clips`). A movie says where it is (`from.<release>.at`): a clip of a disc's movie file
//! (`disc`, `slot`, `clip`), the file's clips back to back, for a movie with sound; or a stream stored in a stage
//! (`stage`, `folder`, `stream`), for one without, which the host reads. A clip starts where a video sector's frame
//! number drops below the one before it (each clip counts its frames from 1); the records of the stages start exactly
//! there. A movie file's clips are found once, reading it a window at a time - it is hundreds of megabytes - and each
//! movie's sectors are read alone and checked against its sha1; a movie on both discs is a clip of each.

use formats::str::{SECTOR, Sector};
use pe2_ext::{Declared, Discs, Extraction, Made, Result, Table, Value, anyhow, bail};
use sha1::{Digest, Sha1};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use crate::NATIVE;

/// Sectors read at once while finding a file's clips: about 9.5 MB.
const WINDOW: u64 = 4096;

/// A session: the movies' declarations, and each movie file's clips as found, by (release, disc, slot).
struct Clips {
    movies: BTreeMap<String, Table>,
    found: HashMap<(String, String, String), Vec<(u64, u64)>>,
}

/// Begin a session: the movies, by path; a movie's place is read when it is extracted.
///
/// # Errors
/// None: an extractor's entry may fail, this one does not.
#[expect(clippy::unnecessary_wraps, reason = "an extractor's entry may fail; this one does not")]
pub fn begin(declared: &dyn Declared) -> Result<Box<dyn Extraction>> {
    let movies = declared.declarations("movie").into_iter().collect();
    Ok(Box::new(Clips { movies, found: HashMap::new() }))
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
        let from =
            declared.get("from").and_then(|f| f.get(release)).ok_or_else(|| anyhow!("{path}: not in {release}"))?;
        let want =
            from.get("sha1").and_then(Value::as_str).ok_or_else(|| anyhow!("{path}: {release} gives no sha1"))?;
        let places = from
            .get("at")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("{path}: {release} says not where it is"))?;
        let mut native = None;
        for at in places {
            let at = at.as_table().ok_or_else(|| anyhow!("{path}: a place that is no table"))?;
            let text = |key: &str| at.get(key).and_then(Value::as_str).map(str::to_string);
            let (bytes, said) = if let Some(clip) = at.get("clip").and_then(Value::as_integer) {
                let (Some(disc), Some(slot)) = (text("disc"), text("slot")) else {
                    bail!("{path}: a clip of no disc's file")
                };
                let mut file = Table::new();
                file.insert("disc".into(), Value::String(disc.clone()));
                file.insert("slot".into(), Value::String(slot.clone()));
                let key = (release.to_string(), disc.clone(), slot.clone());
                if !self.found.contains_key(&key) {
                    let found = clips(discs, &file)?;
                    self.found.insert(key.clone(), found);
                }
                let found = &self.found[&key];
                let &(start, end) = usize::try_from(clip)
                    .ok()
                    .and_then(|c| found.get(c))
                    .ok_or_else(|| anyhow!("{path}: {disc} {slot} holds {} clips, not a clip {clip}", found.len()))?;
                (
                    discs.read_range(&file, start * SECTOR as u64, usize::try_from(end - start)? * SECTOR)?,
                    format!("{disc} {slot} clip {clip}"),
                )
            } else {
                (discs.read(at)?, format!("{at:?}"))
            };
            let got = hex(&Sha1::digest(&bytes));
            if got != want {
                bail!("{path}: {said} has sha1 {got}, the declaration {want}");
            }
            native.get_or_insert(bytes);
        }
        let native = native.ok_or_else(|| anyhow!("{path}: {release} gives no place"))?;
        Ok(vec![(NATIVE.to_string(), native)])
    }
}
