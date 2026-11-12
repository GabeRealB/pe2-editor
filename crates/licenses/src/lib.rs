//! What the editor ships, by its license - kept in the programs, so that a copy of one still says it.
//!
//! Four kinds of thing, kept apart (`Listing`): the editor itself; every crate its programs link, each with its
//! version, its license and the license texts its package holds; what is bundled that is no crate (a font, declarations
//! transcribed from another project's header); and what is no part of the editor and is used where the system has it.
//! What a project's extensions fetch onto a machine is theirs to say (`[[setup]]` of their manifests), not listed
//! here.
//!
//! The build gathers it (`build.rs`: `cargo metadata` for the platform built for, and the workspace's
//! `licenses/licenses.toml` for what no tool finds), and fails on a crate under a license not allowed or with no
//! text. A text is kept once, whatever is under it.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// What the build gathered, deflated.
static PACKED: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/listing.json.deflate"));

/// A license text something is under: the file it was in, and which of the listing's texts it is.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Text {
    pub file: String,
    pub text: usize,
}

/// The editor itself.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Own {
    pub name: String,
    pub version: String,
    pub license: String,
    pub texts: Vec<Text>,
}

/// A crate linked into what ships.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Crate {
    pub name: String,
    pub version: String,
    /// Its license, as it says it: an SPDX expression.
    pub license: String,
    /// Where it comes from, unless that is crates.io.
    pub source: Option<String>,
    pub repository: Option<String>,
    /// What links it: `pe2-editor`, `pe2-cli`, `extensions` (the built-in ones, inside both), or an extension shipped
    /// beside them, by its id.
    pub within: Vec<String>,
    pub texts: Vec<Text>,
}

/// Something inside the programs that is no crate.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bundled {
    pub name: String,
    pub version: Option<String>,
    /// What it is to the editor.
    pub what: String,
    pub license: String,
    pub copyright: Option<String>,
    pub from: Option<String>,
    pub within: Vec<String>,
    pub texts: Vec<Text>,
}

/// Something that is no part of the editor, used where the system has it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Installed {
    pub name: String,
    pub what: String,
    pub license: String,
    pub from: Option<String>,
}

/// What ships, by its license, as built for one platform.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Listing {
    /// The platform it was built for (`linux-x86_64`): what is linked differs by platform.
    pub target: String,
    pub own: Own,
    pub crates: Vec<Crate>,
    pub bundled: Vec<Bundled>,
    pub installed: Vec<Installed>,
    /// Every license text, once.
    pub texts: Vec<String>,
}

/// What this program's build gathered.
///
/// # Panics
/// What the build kept does not read: a fault of the build's, which its test finds.
pub fn listing() -> &'static Listing {
    static LISTING: OnceLock<Listing> = OnceLock::new();
    LISTING.get_or_init(|| {
        let json = miniz_oxide::inflate::decompress_to_vec(PACKED).expect("the build deflated it");
        serde_json::from_slice(&json).expect("the build wrote it")
    })
}

impl Listing {
    /// A text something is under.
    #[must_use]
    pub fn text(&self, text: &Text) -> &str {
        self.texts.get(text.text).map_or("", String::as_str)
    }

    /// The crates linked into one program or extension, as `Crate::within` names it; all of them where none is named.
    pub fn crates_within<'a>(&'a self, within: Option<&'a str>) -> impl Iterator<Item = &'a Crate> {
        self.crates.iter().filter(move |c| within.is_none_or(|w| c.within.iter().any(|i| i == w)))
    }

    /// What ships, as a table: a line each. Of one program or extension where one is named.
    #[must_use]
    pub fn table(&self, within: Option<&str>) -> String {
        let mut out = String::new();
        let own = &self.own;
        // Of one program or extension, it is that one's: the editor's own license is not what is asked.
        let _ = match within {
            None => writeln!(out, "{} {}, as built for {}: {}", own.name, own.version, self.target, own.license),
            Some(within) => writeln!(out, "{within}, of {} {} as built for {}", own.name, own.version, self.target),
        };
        let crates: Vec<&Crate> = self.crates_within(within).collect();
        let of = within.unwrap_or("its programs");
        let _ = writeln!(out, "\nLinked into {of}: {} crates", crates.len());
        let rows = crates.iter().map(|c| {
            let version = c.source.as_ref().map_or_else(|| c.version.clone(), |from| format!("{} ({from})", c.version));
            [c.name.clone(), version, c.license.clone(), c.within.join(", ")]
        });
        columns(&mut out, &["crate", "version", "license", "in"], &rows.collect::<Vec<_>>());
        let bundled: Vec<&Bundled> =
            self.bundled.iter().filter(|b| within.is_none_or(|w| b.within.iter().any(|i| i == w))).collect();
        if !bundled.is_empty() {
            let _ = writeln!(out, "\nBundled with them, and no crate");
            for item in bundled {
                let version = item.version.as_ref().map_or_else(String::new, |v| format!(" {v}"));
                let _ = writeln!(out, "  {}{version}: {}", item.name, item.license);
                let _ = writeln!(out, "    {}; in {}", item.what, item.within.join(", "));
                let rights = [item.copyright.as_ref().map(|c| format!("(c) {c}")), item.from.clone()];
                let rights: Vec<String> = rights.into_iter().flatten().collect();
                if !rights.is_empty() {
                    let _ = writeln!(out, "    {}", rights.join("; "));
                }
            }
        }
        if within.is_none() && !self.installed.is_empty() {
            let _ = writeln!(out, "\nNo part of the editor: used where the system has it");
            for item in &self.installed {
                let _ = writeln!(out, "  {}: {}", item.name, item.license);
                let from = item.from.as_ref().map_or_else(String::new, |from| format!("; {from}"));
                let _ = writeln!(out, "    {}{from}", item.what);
            }
        }
        out
    }

    /// What ships, as a document: the table, then every license text once, each with what is under it. Of one
    /// program or extension where one is named.
    #[must_use]
    pub fn document(&self, within: Option<&str>) -> String {
        let mut out = self.table(within);
        // Each text, with what is under it and the file it was in there.
        let mut under: BTreeMap<usize, Vec<String>> = BTreeMap::new();
        let mut put = |texts: &[Text], who: String| {
            for text in texts {
                under.entry(text.text).or_default().push(format!("{who} ({})", text.file));
            }
        };
        if within.is_none() {
            put(&self.own.texts, format!("{} {}", self.own.name, self.own.version));
        }
        for c in self.crates_within(within) {
            put(&c.texts, format!("{} {}", c.name, c.version));
        }
        for item in self.bundled.iter().filter(|b| within.is_none_or(|w| b.within.iter().any(|i| i == w))) {
            put(&item.texts, item.name.clone());
        }
        let _ = writeln!(out, "\nThe license texts: {}", under.len());
        for (number, (text, who)) in under.iter().enumerate() {
            let rule = "=".repeat(100);
            let _ = writeln!(out, "\n{rule}\n[{}] The license of:", number + 1);
            for line in wrapped(&who.join(", "), 98) {
                let _ = writeln!(out, "  {line}");
            }
            let _ = writeln!(out, "{rule}\n\n{}", self.texts.get(*text).map_or("", String::as_str));
        }
        out
    }
}

/// Rows written as columns, each as wide as its widest cell, under their headings.
fn columns(out: &mut String, headings: &[&str; 4], rows: &[[String; 4]]) {
    let mut widths = headings.map(str::len);
    for row in rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let mut line = |cells: [&str; 4]| {
        let mut text = String::from(" ");
        for (cell, width) in cells.iter().zip(widths) {
            let _ = write!(text, " {cell:<width$} ");
        }
        let _ = writeln!(out, "{}", text.trim_end());
    };
    line(*headings);
    for row in rows {
        line([&row[0], &row[1], &row[2], &row[3]]);
    }
}

/// Text broken into lines no longer than a width, at its spaces.
fn wrapped(text: &str, width: usize) -> Vec<String> {
    let mut lines = vec![String::new()];
    for word in text.split(' ') {
        let last = lines.last_mut().expect("there is a line");
        if !last.is_empty() && last.len() + 1 + word.len() > width {
            lines.push(word.to_string());
        } else {
            if !last.is_empty() {
                last.push(' ');
            }
            last.push_str(word);
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::listing;

    #[test]
    fn everything_linked_has_its_license_and_a_text() {
        let listing = listing();
        assert!(listing.crates.len() > 100, "{} crates", listing.crates.len());
        for c in &listing.crates {
            assert!(!c.license.is_empty() && !c.version.is_empty(), "{}", c.name);
            assert!(!c.within.is_empty(), "{} is linked into nothing", c.name);
            // A file may only name the license (typenum's `LICENSE`): one of them is its text.
            assert!(c.texts.iter().any(|text| listing.text(text).len() > 500), "{} has no license text", c.name);
        }
        // Both programs, and the extensions inside them.
        for within in ["pe2-editor", "pe2-cli", "extensions"] {
            assert!(listing.crates_within(Some(within)).count() > 10, "{within}");
        }
        assert!(listing.text(&listing.own.texts[0]).contains("CC0"));
    }

    #[test]
    fn a_text_is_kept_once_and_written_once() {
        let listing = listing();
        let mut texts = listing.texts.clone();
        texts.sort();
        texts.dedup();
        assert_eq!(texts.len(), listing.texts.len());
        // The Apache license is under hundreds of crates, and in the document as often as its wordings differ.
        let terms = "TERMS AND CONDITIONS FOR USE, REPRODUCTION, AND DISTRIBUTION";
        let (document, kept) = (listing.document(None), listing.texts.iter().filter(|t| t.contains(terms)).count());
        let under = listing.crates.iter().filter(|c| c.texts.iter().any(|t| listing.text(t).contains(terms))).count();
        // A file may hold it twice: as often as the texts kept have it.
        assert_eq!(document.matches(terms).count(), listing.texts.iter().map(|t| t.matches(terms).count()).sum::<usize>());
        assert!(kept * 4 < under, "{kept} wordings for {under} crates");
        assert!(document.contains("Phosphor Icons"));
    }

    #[test]
    fn a_program_s_table_lists_what_it_links_alone() {
        let listing = listing();
        let (all, cli) = (listing.table(None), listing.table(Some("pe2-cli")));
        assert!(all.lines().count() > cli.lines().count());
        // The editor's window is not in the command line's program.
        assert!(all.contains("\n  egui ") && !cli.contains("\n  egui "));
    }
}
