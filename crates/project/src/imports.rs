//! Imports: what an asset's importer makes of its source, kept in the project cache (`.cache/imports/<key>/`) and seen
//! by every reader as the asset's own - the bundle holds the source, the native form is made from it.
//!
//! An import makes native files, and type settings it derives from the source (an image's size, its cells'
//! rectangles): those its importer says it `derives`. They take the place of the declaration's - a value the
//! declaration has there is overridden, and said to be - and the parts of a keyed table the declaration has but the
//! source no longer does are orphaned: kept, reported, until removed or their source part returns. A part the source
//! has and the declaration not is there with what the import derives of it. What readers see is the resolved settings
//! (`index::Asset::settings`); the declaration stays as written.
//!
//! An import is an output of the work graph (`graph`): the asset's stack takes its source - the stored file on top -
//! through its importer to the `native` output, kept in the project's store by content with its trace, which records
//! what the import read (its source's hash, the declaration's). A changed source, a changed setting or a new importer
//! makes it again, and anything else finds it made. Extraction makes the imports it checks through the same engine,
//! so a project opens without importing. Like the rest of the cache, any of it may be deleted at the cost of time only.

use anyhow::{Context, Result};
use pe2_value as V;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::cache::Cache;
use crate::ext::{Extensions, Origin, Status};
use crate::extract::sha1_hex;
use crate::graph::exec::{Engine, Reads};
use crate::graph::store::{Blob, Kept, Store};
use crate::graph::{self, Plan};
use crate::index::{Asset, BundleFile, Imported, Index, Problem, Report};

/// Where imports were kept before the store, removed on opening.
const OLD: &str = "imports";

/// What attaching found.
#[derive(Debug, Default)]
pub struct Attached {
    /// Imports made now, and those found made.
    pub made: usize,
    pub kept: usize,
    /// Imports that failed, with why: their assets have no native files.
    pub failed: Vec<(String, String)>,
}

/// The project's imports.
pub struct Imports {
    store: Store,
    /// What attaching read of each import, and the settings it resolved to, by its output's key, with what it read:
    /// a refresh finds them here rather than on disk while that holds.
    known: Mutex<HashMap<String, (Reads, Arc<Known>)>>,
    /// The nodes each asset's last import read outputs of (as traces name them), by its path.
    read_from: Mutex<HashMap<String, BTreeSet<String>>>,
    /// The addresses each referenced asset has, by its path, with the hash of the settings they were listed from: kept
    /// in the cache (`LISTED`) for the extensions that listed them, as listing a family's members asks its extension.
    addresses: Mutex<HashMap<String, Listed>>,
    /// Where they are kept.
    listed_at: std::path::PathBuf,
    /// Set when what is attaching is no longer wanted.
    stop: Arc<AtomicBool>,
}

/// An asset's addresses, with the hash of the settings they were listed from.
type Listed = (String, Arc<HashSet<String>>);

/// Where the addresses listed are kept, in the cache, with the extensions that listed them.
const LISTED: &str = "listed.json";

/// What tells the extensions loaded apart from others: each one's id, version and build.
fn builds_of(extensions: &Extensions) -> String {
    let mut builds: Vec<String> = extensions
        .loaded
        .iter()
        .map(|l| format!("{} {} {}", l.id, l.version, l.build.as_deref().unwrap_or("linked")))
        .collect();
    builds.sort();
    sha1_hex(builds.join("\n").as_bytes())
}

/// The addresses kept in the cache, where these extensions listed them.
fn listed_kept(at: &std::path::Path, builds: &str) -> HashMap<String, Listed> {
    let Ok(text) = std::fs::read_to_string(at) else { return HashMap::new() };
    let Ok(kept) = serde_json::from_str::<serde_json::Value>(&text) else { return HashMap::new() };
    if kept["builds"].as_str() != Some(builds) {
        return HashMap::new();
    }
    let listed = kept["listed"].as_object().into_iter().flatten();
    listed
        .filter_map(|(target, entry)| {
            let digest = entry[0].as_str()?.to_string();
            let addresses = entry[1].as_array()?.iter().filter_map(|a| Some(a.as_str()?.to_string())).collect();
            Some((target.clone(), (digest, Arc::new(addresses))))
        })
        .collect()
}

/// Keep the addresses listed in the cache, for the extensions that listed them.
fn keep_listed(at: &std::path::Path, builds: &str, listed: &HashMap<String, Listed>) {
    let mut sorted: Vec<(&String, &Listed)> = listed.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    let mut entries = serde_json::Map::new();
    for (target, (digest, addresses)) in sorted {
        let mut addresses: Vec<&String> = addresses.iter().collect();
        addresses.sort();
        entries.insert(target.clone(), serde_json::json!([digest, addresses]));
    }
    let kept = serde_json::json!({ "builds": builds, "listed": entries });
    let _ = crate::cache::write_atomic(at, kept.to_string().as_bytes());
}

/// What there is of an asset's import, found without making anything.
enum Import {
    /// As it was last attached, what it read still holding.
    Held(Arc<Known>),
    /// Kept in the store.
    Kept(Reads, Arc<Known>),
    /// Kept, and not as an import can be.
    Unread(anyhow::Error),
    /// To be made.
    ToMake { importer: String, plan: Plan, output: usize },
}

/// What attaching found of an asset: what is wrong with its stack, and its import by its output's key.
struct Look {
    wrong: Vec<graph::Problem>,
    import: Option<(String, Import)>,
}

/// An import as attaching uses it.
struct Known {
    importer: String,
    files: Vec<(String, Blob)>,
    /// The settings it derived, the paths it derives them at, and its notes: resolved over the declaration as it stands
    /// each time it is attached, as its settings may have changed - unsaved, in a working copy.
    made: pe2_value::Table,
    derives: Vec<String>,
    notes: Vec<String>,
    /// Its last resolution, with the hash of the declaration it was over: resolved again only when that changed.
    resolved: Mutex<Option<(String, Arc<Imported>)>>,
}

impl Known {
    /// Its settings resolved over a declaration (`resolve`): as last resolved, where that was this declaration.
    fn resolved(&self, asset: &Asset) -> Arc<Imported> {
        let mut last = self.resolved.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let declared = sha1_hex(asset.text().as_bytes());
        if let Some((was, resolved)) = last.as_ref()
            && *was == declared
        {
            return resolved.clone();
        }
        let mut resolved = resolve(asset.fields(), &self.made, &self.derives);
        fill(&mut resolved.settings, &self.made);
        resolved.notes.clone_from(&self.notes);
        let resolved = Arc::new(resolved);
        *last = Some((declared, resolved.clone()));
        resolved
    }
}

impl Imports {
    /// The project's imports.
    ///
    /// # Errors
    /// The store cannot be opened.
    pub fn open(cache: &Cache) -> Result<Imports> {
        let old = cache.path(OLD);
        if old.exists() {
            std::fs::remove_dir_all(&old).with_context(|| old.display().to_string())?;
        }
        Ok(Imports {
            store: Store::open(cache)?,
            known: Mutex::default(),
            read_from: Mutex::default(),
            addresses: Mutex::default(),
            listed_at: cache.path(LISTED),
            stop: Arc::default(),
        })
    }

    /// Sweep the store of what the project's nodes no longer make, and of what nodes it no longer has made
    /// (`Store::sweep`): what was swept, as (traces, values, blobs). It reads all the store keeps - a third of a
    /// second for the game - so it is run aside, not on the way to anything.
    pub fn sweep(&self, index: &Index) -> (usize, usize, usize) {
        sweep_store(&self.store, &live_nodes(index))
    }

    /// The engine imports are made by: the extensions' operations, keeping in the store.
    #[must_use]
    pub fn engine(&self, extensions: &Extensions) -> Engine {
        Engine::new(extensions.operations(), Some(self.store.clone())).for_a_run()
    }

    /// What stops an attach going on: set, it gives up where it is, which leaves the index it was given without imports
    /// and is for when nobody will look at that.
    #[must_use]
    pub fn stopper(&self) -> Arc<AtomicBool> {
        self.stop.clone()
    }

    fn stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Give every asset whose importer makes files what its import made, made now where it is not yet: the files take
    /// the place of the bundle's of the same names and say which importer made them, the derived settings resolve over
    /// the declaration's. An import that fails, a declared value it overrides, a part it orphans and a reference
    /// naming a part its asset does not have are problems of the index. Imports no asset made are removed.
    pub fn attach(&self, index: &mut Index, extensions: &Extensions) -> Attached {
        self.attach_some(index, extensions, None, &|_, _, _| {})
    }

    /// Attach as `attach` does, saying how many assets of how many it has gone through.
    pub fn attach_reporting(&self, index: &mut Index, extensions: &Extensions, report: &Report<'_>) -> Attached {
        self.attach_some(index, extensions, None, report)
    }

    /// Attach the imports of these assets alone - those a refresh changed (`Refreshed::assets`) - and of those reading
    /// their outputs (`Extensions::dependents`), the others keeping theirs: their problems made again, and those of the
    /// references from or to them naming outputs. What it costs is what changed; imports no asset makes any more are
    /// removed by the next full attach.
    pub fn attach_changed(&self, index: &mut Index, extensions: &Extensions, changed: &BTreeSet<String>) -> Attached {
        let mut changed = changed.clone();
        let depending = tracing::info_span!("attach: what depends on what changed").entered();
        changed.extend(extensions.dependents(index, &changed));
        changed.extend(self.readers(index, &changed));
        drop(depending);
        // What failed may have failed for what another made, which it did not get to say it read: it is tried again.
        let failed = index.problems.iter().filter_map(|p| match p {
            Problem::Import { asset, .. } => Some(asset.clone()),
            _ => None,
        });
        let failed: Vec<String> = failed.collect();
        changed.extend(failed.iter().cloned());
        changed.extend(self.readers(index, &failed.into_iter().collect()));
        self.attach_some(index, extensions, Some(&changed), &|_, _, _| {})
    }

    /// The assets whose last import read an output of one of these, through others or not: what an operation reads of
    /// another node as it runs (`Call::output_of`), which no `references` layer says. These not among them.
    fn readers(&self, index: &Index, changed: &BTreeSet<String>) -> BTreeSet<String> {
        let read_from = self.read_from.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut found = BTreeSet::new();
        let mut next: Vec<String> = changed.iter().cloned().collect();
        while let Some(path) = next.pop() {
            let Some(asset) = index.assets.get(&path) else { continue };
            let node = graph::exec::id(&**asset);
            for (reader, nodes) in read_from.iter() {
                if !changed.contains(reader) && !found.contains(reader) && nodes.contains(node) {
                    found.insert(reader.clone());
                    next.push(reader.clone());
                }
            }
        }
        found
    }

    #[tracing::instrument(name = "imports attach", skip_all)]
    fn attach_some(
        &self,
        index: &mut Index,
        extensions: &Extensions,
        only: Option<&BTreeSet<String>>,
        report: &Report<'_>,
    ) -> Attached {
        let mut attached = Attached::default();
        let of = |path: &str| only.is_none_or(|o| o.contains(path));
        // What the project defines, for the stacks laying it in.
        let defined = tracing::info_span!("attach: definitions taken").in_scope(|| extensions.take_definitions(index));
        // What earlier imports made goes first: an asset may import otherwise now, or not at all.
        tracing::info_span!("attach: earlier imports forgotten").in_scope(|| forget_imports(index, &of));
        let engine = self.engine(extensions);
        let mut known = self.known.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let assets: Vec<&Asset> = match only {
            None => index.assets.values().map(|a| &**a).collect(),
            Some(only) => only.iter().filter_map(|p| index.assets.get(p).map(|a| &**a)).collect(),
        };
        report("Attaching what the importers made", 0, assets.len());
        // What an import reads of other nodes, made in the index as it is.
        let world = crate::ext::Graph { index: &*index, extensions };
        let reach = graph::exec::Reach { engine: &engine, world: &world };
        // Each asset's by itself, at the same time, as all of it only reads.
        let held = &*known;
        let looked = tracing::info_span!("attach: each asset looked at").in_scope(|| {
            crate::parallel(&assets, |asset| (!self.stopping()).then(|| self.look(asset, extensions, reach, held)))
        });
        let mut made =
            tracing::info_span!("attach: imports made").in_scope(|| self.make(&world, &assets, &looked, extensions, reach, report));
        let mut found: Vec<(String, String, Arc<Known>)> = Vec::new();
        let mut stacks: Vec<Problem> = Vec::new();
        let mut new: Vec<(String, (Reads, Arc<Known>))> = Vec::new();
        let mut read_from: Vec<(String, BTreeSet<String>)> = Vec::new();
        if self.stopping() {
            return attached;
        }
        for (at, (asset, look)) in assets.into_iter().zip(looked).enumerate() {
            let Some(look) = look else { return attached };
            let wrong = |p: graph::Problem| Problem::Stack {
                asset: asset.path.clone(),
                error: format!("{}: {}", p.at, p.what),
            };
            stacks.extend(look.wrong.into_iter().map(wrong));
            let Some((key, import)) = look.import else { continue };
            let done = match import {
                Import::Held(import) => {
                    attached.kept += 1;
                    found.push((asset.path.clone(), key, import));
                    continue;
                }
                Import::Kept(reads, import) => Ok((false, reads, import)),
                Import::Unread(why) => Err(why),
                Import::ToMake { .. } => match made.remove(&at) {
                    Some(done) => done,
                    None => return attached,
                },
            };
            match done {
                Ok((made, reads, import)) => {
                    if made {
                        attached.made += 1;
                    } else {
                        attached.kept += 1;
                    }
                    let nodes = reads
                        .outputs
                        .keys()
                        .map(|read| read.split_once('#').map_or(read.as_str(), |(of, _)| of).to_string())
                        .collect();
                    read_from.push((asset.path.clone(), nodes));
                    new.push((key.clone(), (reads, import.clone())));
                    found.push((asset.path.clone(), key, import));
                }
                Err(e) => {
                    read_from.push((asset.path.clone(), BTreeSet::new()));
                    attached.failed.push((asset.path.clone(), format!("{e:#}")));
                }
            }
        }
        if only.is_none() {
            let live: HashSet<&String> = found.iter().map(|(_, key, _)| key).collect();
            known.retain(|key, _| live.contains(key));
        }
        known.extend(new);
        drop(known);
        self.read_from.lock().unwrap_or_else(std::sync::PoisonError::into_inner).extend(read_from);
        let mut problems: Vec<Problem> = attached
            .failed
            .iter()
            .map(|(asset, error)| Problem::Import { asset: asset.clone(), error: error.clone() })
            .collect();
        problems.extend(stacks);
        problems.extend(tracing::info_span!("attach: given to the assets").in_scope(|| self.give(index, found)));
        problems.extend(tracing::info_span!("attach: outputs referred to looked for").in_scope(|| self.missing_outputs(index, extensions, only)));
        problems.extend(extension_problems(extensions));
        index.problems.retain(|p| match p {
            Problem::Import { asset, .. }
            | Problem::Stack { asset, .. }
            | Problem::Overridden { asset, .. }
            | Problem::Orphan { asset, .. }
            | Problem::Taken { asset, .. } => !of(asset),
            Problem::MissingOutput { asset, target, .. } => !of(asset) && !of(target),
            Problem::Extension { .. } | Problem::Required { .. } => false,
            _ => true,
        });
        index.problems.retain(|p| !defined.contains(p));
        index.problems.extend(problems);
        index.problems.extend(defined);
        attached
    }

    /// What each asset's import comes to, given it: what they make among its files, and its settings resolved. Worked out
    /// at the same time, as that only reads. Returns what is wrong with them.
    fn give(&self, index: &mut Index, found: Vec<(String, String, Arc<Known>)>) -> Vec<Problem> {
        let mut problems = Vec::new();
        let resolved = crate::parallel(&found, |(path, _, import)| {
            let asset = index.assets.get(path)?;
            Some((source_file(asset).map_or_default(|f| f.layer.clone()), import.resolved(asset)))
        });
        for ((path, _, import), resolved) in found.into_iter().zip(resolved) {
            let Some((layer, resolved)) = resolved else { continue };
            let Some(asset) = Arc::make_mut(&mut index.assets).get_mut(&path).map(Arc::make_mut) else { continue };
            asset.files.retain(|f| !import.files.iter().any(|(name, _)| *name == f.name));
            for (name, blob) in &import.files {
                asset.files.push(BundleFile {
                    path: self.store.blob_path(&blob.sha1),
                    name: name.clone(),
                    size: blob.size,
                    modified: (0, 0),
                    layer: layer.clone(),
                    sha1: blob.sha1.clone(),
                    made_by: Some(import.importer.clone()),
                });
            }
            problems.extend(
                resolved
                    .overridden
                    .iter()
                    .map(|setting| Problem::Overridden { asset: path.clone(), setting: setting.clone() }),
            );
            problems.extend(
                resolved.orphans.iter().map(|part| Problem::Orphan { asset: path.clone(), part: part.clone() }),
            );
            problems
                .extend(resolved.notes.iter().map(|note| Problem::Taken { asset: path.clone(), note: note.clone() }));
            asset.imported = Some(resolved);
        }
        problems
    }

    /// The references naming an output their asset does not have: with the settings resolved, as members an import makes
    /// are among them. An asset's outputs are listed again only when its settings change.
    fn missing_outputs(&self, index: &Index, extensions: &Extensions, only: Option<&BTreeSet<String>>) -> Vec<Problem> {
        let mut known = self.addresses.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // What an earlier run listed, with these extensions.
        let builds = builds_of(extensions);
        if known.is_empty() {
            *known = listed_kept(&self.listed_at, &builds);
        }
        let of = |path: &str| only.is_none_or(|o| o.contains(path));
        let targets: Vec<_> = index.referenced_by.iter().collect();
        // Each by itself, listed where its settings changed.
        let held = &*known;
        let listed = crate::parallel(&targets, |(target, references)| {
            // Of the assets attached: the references to them, and from them.
            let named: Vec<_> =
                references.iter().filter(|r| r.output.is_some() && (of(target) || of(&r.from))).collect();
            let asset =
                index.assets.get(*target).filter(|a| !named.is_empty() && extensions.node_type_of(a).is_some())?;
            // Of what it makes, what is named: listing the members of the rest would be asking its extension for nothing.
            let mut families: Vec<&str> = named.iter().filter_map(|r| r.output.as_deref()?.split('/').next()).collect();
            families.sort_unstable();
            families.dedup();
            let digest = sha1_hex(format!("{}\n{}", families.join(" "), asset.settings_text()).as_bytes());
            let addresses = held.get(*target).filter(|(d, _)| *d == digest).map_or_else(
                || {
                    Arc::new(
                        extensions
                            .addresses_of(asset, Some(&families))
                            .into_iter()
                            .map(|(address, _)| address)
                            .collect::<HashSet<_>>(),
                    )
                },
                |(_, addresses)| addresses.clone(),
            );
            let problems: Vec<Problem> = named
                .into_iter()
                .filter_map(|reference| {
                    let output = reference.output.as_ref().filter(|o| !addresses.contains(crate::ext::listed(o)))?;
                    Some(Problem::MissingOutput {
                        asset: reference.from.clone(),
                        slot: reference.slot.clone(),
                        target: (*target).clone(),
                        output: output.clone(),
                    })
                })
                .collect();
            Some(((*target).clone(), digest, addresses, problems))
        });
        let mut problems = Vec::new();
        let mut new = false;
        for (target, digest, addresses, missing) in listed.into_iter().flatten() {
            new |= known.get(&target).is_none_or(|(was, _)| *was != digest);
            known.insert(target, (digest, addresses));
            problems.extend(missing);
        }
        if new {
            keep_listed(&self.listed_at, &builds, &known);
        }
        problems
    }
}

impl Imports {
    /// What there is of an asset's import without making anything.
    fn look(
        &self,
        asset: &Asset,
        extensions: &Extensions,
        reach: graph::exec::Reach<'_>,
        held: &HashMap<String, (Reads, Arc<Known>)>,
    ) -> Look {
        let (plan, wrong) = extensions.plan(asset);
        let Some((importer, output)) = import_output(&plan, extensions) else { return Look { wrong, import: None } };
        let key = reach.engine.key(&plan, asset, output, None);
        if let Some((_, import)) = held.get(&key).filter(|(reads, _)| reads.hold(asset, Some(reach))) {
            return Look { wrong, import: Some((key, Import::Held(import.clone()))) };
        }
        let kept = reach.engine.kept_in(Some(reach), &plan, asset, output, None);
        let kept = kept.and_then(|trace| Some((self.store.describe(&trace.value)?, trace)));
        let import = match kept {
            Some((kept, trace)) => match known_of(extensions, importer, kept, trace.reads) {
                Ok((reads, import)) => Import::Kept(reads, import),
                Err(why) => Import::Unread(why),
            },
            None => Import::ToMake { importer, plan, output },
        };
        Look { wrong, import: Some((key, import)) }
    }

    /// The imports nothing holds, by their asset's place among those looked at: each after what its declaration refers to,
    /// so that what it reads is kept when it asks and it holds one instance, not one for each node down the line. Those with
    /// nothing between them at the same time, as many as their extensions take (`component::JOBS`).
    fn make(
        &self,
        world: &crate::ext::Graph<'_>,
        assets: &[&Asset],
        looked: &[Option<Look>],
        extensions: &Extensions,
        reach: graph::exec::Reach<'_>,
        report: &Report<'_>,
    ) -> HashMap<usize, Result<(bool, Reads, Arc<Known>)>> {
        let to_make: Vec<_> = looked
            .iter()
            .enumerate()
            .filter_map(|(at, look)| match &look.as_ref()?.import {
                Some((key, Import::ToMake { importer, plan, output })) => Some((at, key, importer, plan, *output)),
                _ => None,
            })
            .collect();
        let mut made = HashMap::new();
        if to_make.is_empty() {
            return made;
        }
        report("Making what the importers make", 0, to_make.len());
        let paths: Vec<&str> = to_make.iter().map(|(at, ..)| assets[*at].path.as_str()).collect();
        for level in in_order(world.index, &paths) {
            let level: Vec<_> = level.into_iter().map(|at| to_make[at]).collect();
            let done = crate::parallel(&level, |(at, key, importer, plan, output)| {
                (!self.stopping()).then(|| {
                    (*at, self.import(assets[*at], extensions, reach, ((*importer).clone(), plan, *output), key))
                })
            });
            made.extend(done.into_iter().flatten());
        }
        made
    }

    /// An asset's import, found kept since it was looked for or made now: whether it was made, what it read, and what
    /// attaching uses of it.
    fn import(
        &self,
        asset: &Asset,
        extensions: &Extensions,
        reach: graph::exec::Reach<'_>,
        (importer, plan, output): (String, &Plan, usize),
        key: &str,
    ) -> Result<(bool, Reads, Arc<Known>)> {
        reach.engine.output_in(Some(reach), plan, asset, &plan.outputs[output].name, None, true)?;
        let trace = self.store.trace(key).context("its import was not kept")?;
        let kept = self.store.describe(&trace.value).context("its import was not kept")?;
        let (reads, import) = known_of(extensions, importer, kept, trace.reads)?;
        Ok((true, reads, import))
    }
}

/// What earlier imports made of the assets `of` takes, gone. Copied where shared with a snapshot, and only where
/// something goes.
fn forget_imports(index: &mut Index, of: &dyn Fn(&str) -> bool) {
    let imported = |a: &Asset| a.imported.is_some() || a.files.iter().any(|f| f.made_by.is_some());
    let going: Vec<String> =
        index.assets.values().filter(|a| of(&a.path) && imported(a)).map(|a| a.path.clone()).collect();
    if going.is_empty() {
        return;
    }
    let assets = Arc::make_mut(&mut index.assets);
    for path in going {
        if let Some(asset) = assets.get_mut(&path) {
            let asset = Arc::make_mut(asset);
            asset.files.retain(|f| f.made_by.is_none());
            asset.imported = None;
        }
    }
}

/// These nodes in rounds, by their places, each round after those holding what its nodes refer to. Nodes referring to each
/// other in a circle go last: what they make of it is theirs to say.
fn in_order(index: &Index, paths: &[&str]) -> Vec<Vec<usize>> {
    let places: HashMap<&str, usize> = paths.iter().enumerate().map(|(at, path)| (*path, at)).collect();
    let reads = |at: usize| {
        index
            .references_from(paths[at])
            .into_iter()
            .filter_map(|r| places.get(index.resolve(&r.uid, &r.target)?.path.as_str()).copied())
    };
    let mut waiting: Vec<(usize, HashSet<usize>)> =
        (0..paths.len()).map(|at| (at, reads(at).filter(|other| *other != at).collect())).collect();
    let mut rounds = Vec::new();
    while !waiting.is_empty() {
        let (free, rest): (Vec<_>, Vec<_>) = waiting.into_iter().partition(|(_, on)| on.is_empty());
        if free.is_empty() {
            rounds.push(rest.into_iter().map(|(at, _)| at).collect());
            break;
        }
        let done: HashSet<usize> = free.iter().map(|(at, _)| *at).collect();
        waiting = rest.into_iter().map(|(at, on)| (at, on.difference(&done).copied().collect())).collect();
        rounds.push(free.into_iter().map(|(at, _)| at).collect());
    }
    rounds
}

/// What attaching uses of an import as it was kept.
fn known_of(extensions: &Extensions, importer: String, kept: Kept, reads: Reads) -> Result<(Reads, Arc<Known>)> {
    let Kept::Typed { files, settings, notes, .. } = kept else {
        anyhow::bail!("its import is kept as something else than an extension's type");
    };
    let derives = extensions.op(&importer).map_or_default(|op| op.derives.clone());
    let files: Vec<(String, Blob)> = files.into_iter().map(|f| (f.name, f.blob)).collect();
    Ok((reads, Arc::new(Known { importer, files, made: settings, derives, notes, resolved: Mutex::default() })))
}

/// What is wrong with a project's extensions: those it holds not loaded, and what it requires and does not have.
fn extension_problems(extensions: &Extensions) -> Vec<Problem> {
    let refused = extensions.present.iter().filter_map(|p| match (&p.origin, &p.status) {
        (Origin::Project(_), Status::Refused(why)) => Some(Problem::Extension { id: p.id.clone(), error: why.clone() }),
        _ => None,
    });
    let unmet = extensions.unmet.iter().map(|(id, why)| Problem::Required { id: id.clone(), why: why.clone() });
    refused.chain(unmet).collect()
}

/// How long a store goes unswept, while something is kept in it, before a sweep is due.
pub const SWEEP_EVERY: std::time::Duration = std::time::Duration::from_mins(30);

/// Sweep a store of what the nodes `live` no longer make (`Store::sweep`), noted as swept before - so that another
/// process finds it is not due - and after, what the sweep removed being no news of its own.
#[must_use = "what was swept is the sweep's report"]
#[expect(clippy::implicit_hasher, reason = "the store sweeps by a set of the standard hasher's")]
pub fn sweep_store(store: &Store, live: &HashSet<String>) -> (usize, usize, usize) {
    let _span = tracing::info_span!("store sweep").entered();
    store.mark_swept();
    let swept = store.sweep(live, &HashSet::new());
    store.mark_swept();
    swept
}

/// Sweep a store of what the project's nodes no longer make where it is due (`Store::sweep_due`, every
/// `SWEEP_EVERY`): what was swept, none where it was not due. For what keeps outputs while the project is open - the
/// editor, the agents' server, a build - so that the store stays as large as what the project makes now.
#[must_use = "what was swept is the sweep's report"]
pub fn sweep_if_due(store: &Store, index: &Index) -> Option<(usize, usize, usize)> {
    store.sweep_due(SWEEP_EVERY).then(|| sweep_store(store, &live_nodes(index)))
}

/// The project's nodes, as traces name them: what the store keeps for others goes in a sweep.
#[must_use]
pub fn live_nodes(index: &Index) -> HashSet<String> {
    index.assets.values().map(|a| graph::exec::id(&**a).to_string()).collect()
}

/// What makes an asset's result - the operation its stack ends with - and the output keeping it, where its result is
/// made: its files are then attached in its bundle's, and its derived settings resolved over its declaration's. (A
/// result that is a stored file is seen as it is.)
fn import_output(plan: &Plan, extensions: &Extensions) -> Option<(String, usize)> {
    let result = plan.result.filter(|_| !plan.result_stored())?;
    let output = plan.result_output()?;
    // One of several outputs is its operation's.
    let picked = plan.steps[result].op == graph::PICK;
    let result = if picked { *plan.steps[result].inputs.first()?.first()? } else { result };
    // A table is read by what asks for it: it has no files to attach.
    if extensions.op(&plan.steps[result].op).is_some_and(|op| op.output == pe2_ext::Type::Table) {
        return None;
    }
    // What events make current is theirs: taking a project in compiles nothing.
    plan.outputs[output].events.pull.is_empty().then(|| (plan.steps[result].op.clone(), output))
}

/// The file an asset's stack takes as its import's source, from its bundle.
fn source_file(asset: &Asset) -> Option<&BundleFile> {
    let (name, _) = graph::import_of_fields(asset.fields())?;
    asset.files.iter().find(|f| f.name == name && f.made_by.is_none())
}

/// A path of `derives`, split: `cells.*.rect` to `["cells", "*", "rect"]`.
fn steps(path: &str) -> Vec<&str> {
    path.split('.').collect()
}

/// The declaration's settings with an import's derived ones in their places: at each path the importer derives, the
/// import's value (a declared one there is overridden); under a `*`, the parts the import has - the declaration's
/// settings of each kept beside what the import derives of it, a part only the import has added after the
/// declaration's, a part only the declaration has kept and orphaned. What it leaves be is the declaration's, shared;
/// what it places, the import's, shared: only the tables on the way to a place are copied.
#[must_use]
pub fn resolve(declared: &V::Table, made: &V::Table, paths: &[String]) -> Imported {
    let mut settings = declared.clone();
    let (mut overridden, mut orphans) = (Vec::new(), Vec::new());
    for path in paths {
        place(&mut settings, made, &steps(path), "", &mut overridden, &mut orphans);
    }
    Imported { settings, text: std::sync::OnceLock::new(), overridden, orphans, notes: vec![] }
}

/// Whether `fill` would add anything to `into`.
fn fills(into: &V::Table, made: &V::Table) -> bool {
    made.iter().any(|(key, value)| match (into.get(key), value) {
        (None, _) => true,
        (Some(V::Value::Table(mine)), V::Value::Table(made)) => fills(mine, made),
        _ => false,
    })
}

/// What an import made beyond what it says it derives, under what the node says: a node reads what its import came up with,
/// which is what lists the members of what it makes, though no declaration has them. A table it adds nothing to stays
/// shared.
fn fill(into: &mut V::Table, made: &V::Table) {
    for (key, value) in made.iter() {
        match (into.get(key), value) {
            (None, _) => drop(into.insert(key, value.clone())),
            (Some(V::Value::Table(mine)), V::Value::Table(made)) if fills(mine, made) => {
                if let Some(V::Value::Table(mine)) = into.get_mut(key) {
                    fill(mine, made);
                }
            }
            _ => {}
        }
    }
}

fn place(
    into: &mut V::Table,
    from: &V::Table,
    steps: &[&str],
    at: &str,
    overridden: &mut Vec<String>,
    orphans: &mut Vec<String>,
) {
    let Some((&step, rest)) = steps.split_first() else { return };
    let join = |key: &str| if at.is_empty() { key.to_string() } else { format!("{at}.{key}") };
    match (step, rest.first()) {
        // The parts of a keyed table: each the import has, and the declaration's others orphaned.
        (_, Some(&"*")) => {
            let Some(V::Value::Table(parts)) = from.get(step) else { return };
            match into.get(step) {
                Some(V::Value::Table(_)) => {}
                Some(_) => {
                    overridden.push(join(step));
                    into.insert(step, V::Value::Table(V::Table::new()));
                }
                None => drop(into.insert(step, V::Value::Table(V::Table::new()))),
            }
            let Some(V::Value::Table(table)) = into.get_mut(step) else { return };
            for name in table.keys() {
                if !parts.contains_key(name) {
                    orphans.push(format!("{}.{name}", join(step)));
                }
            }
            for (name, part) in parts.iter() {
                let V::Value::Table(part) = part else { continue };
                if !table.contains_key(name) {
                    table.insert(name, V::Value::Table(V::Table::new()));
                }
                if let Some(V::Value::Table(mine)) = table.get_mut(name) {
                    place(mine, part, &rest[1..], &format!("{}.{name}", join(step)), overridden, orphans);
                }
            }
        }
        (_, Some(_)) => {
            let Some(V::Value::Table(from)) = from.get(step) else { return };
            if !into.contains_key(step) {
                into.insert(step, V::Value::Table(V::Table::new()));
            }
            if let Some(V::Value::Table(table)) = into.get_mut(step) {
                place(table, from, rest, &join(step), overridden, orphans);
            }
        }
        (_, None) => {
            let Some(value) = from.get(step) else { return };
            if into.get(step).is_some_and(|v| v != value) {
                overridden.push(join(step));
            }
            into.insert(step, value.clone());
        }
    }
}

/// A declaration's text without the settings an importer derives, as extraction writes it for an asset whose source
/// it writes: they are the source's.
///
/// # Errors
/// The text is not TOML.
pub fn without_derived(text: &str, derives: &[String]) -> Result<String> {
    let mut doc: toml_edit::DocumentMut = text.parse()?;
    for path in derives {
        remove(doc.as_item_mut(), &steps(path));
    }
    Ok(doc.to_string())
}

fn remove(item: &mut toml_edit::Item, steps: &[&str]) {
    let Some((&step, rest)) = steps.split_first() else { return };
    let Some(table) = item.as_table_like_mut() else { return };
    if rest.is_empty() {
        table.remove(step);
        return;
    }
    if step == "*" {
        for (_, part) in table.iter_mut() {
            remove(part, rest);
        }
        return;
    }
    if let Some(next) = table.get_mut(step) {
        // An inline table (`cells.dot = { rect = ..., palette = ... }`) is a value, not an item: made an item to walk.
        if let Some(inline) = next.as_inline_table_mut() {
            let mut item = toml_edit::Item::Value(toml_edit::Value::InlineTable(std::mem::take(inline)));
            remove(&mut item, rest);
            if let toml_edit::Item::Value(toml_edit::Value::InlineTable(done)) = item {
                *next.as_inline_table_mut().expect("still inline") = done;
            }
            return;
        }
        remove(next, rest);
    }
}

#[cfg(test)]
mod tests {
    use super::{resolve, without_derived};

    const DECLARED: &str = r#"type = "image"
mode = "indexed_8bpp"
size = [8, 2]
cells.wide = { rect = [4, 0, 4, 2], palette = "cold" }
cells.gone = { rect = [0, 0, 1, 1], palette = "warm" }

[[stack]]
op = "file"
file = "source.ora"

[[stack]]
op = "pe2.images@ora"
"#;

    #[test]
    fn derived_settings_take_their_places() {
        use pe2_value as V;
        let derives = ["size".to_string(), "cells.*.rect".to_string()];
        let declared: V::Table = toml::from_str(DECLARED).unwrap();
        let made: V::Table =
            toml::from_str("size = [8, 3]\ncells.wide = { rect = [4, 0, 4, 2] }\ncells.new = { rect = [0, 2, 2, 1] }")
                .unwrap();
        let resolved = resolve(&declared, &made, &derives);
        // What it leaves be is the declaration's, shared, not copied.
        let (Some(V::Value::Array(a)), Some(V::Value::Array(b))) =
            (resolved.settings.get("stack"), declared.get("stack"))
        else {
            panic!("stacks")
        };
        assert!(std::sync::Arc::ptr_eq(a, b), "the stack, left be, is shared");
        let settings = resolved.settings.to_toml();
        assert_eq!(settings["size"].as_array().unwrap()[1].as_integer(), Some(3));
        assert_eq!(settings["cells"]["wide"]["palette"].as_str(), Some("cold"));
        assert_eq!(settings["cells"]["new"]["rect"].as_array().unwrap().len(), 4);
        let order: Vec<&String> = settings["cells"].as_table().unwrap().keys().collect();
        assert_eq!(order, ["wide", "gone", "new"]);
        assert_eq!(resolved.overridden, ["size"]);
        assert_eq!(resolved.orphans, ["cells.gone"]);
        let asset_text = toml::to_string(&resolved.settings).unwrap();
        assert!(asset_text.contains("[[stack]]"));
    }

    #[test]
    fn extraction_leaves_the_derived_settings_out() {
        let derives = ["size".to_string(), "cells.*.rect".to_string()];
        let text = without_derived(DECLARED, &derives).unwrap();
        assert!(!text.contains("size"), "{text}");
        assert!(!text.contains("rect"), "{text}");
        assert!(text.contains("cells.wide = { palette = \"cold\" }"), "{text}");
        assert!(text.contains("mode = \"indexed_8bpp\""), "{text}");
    }
}

#[cfg(test)]
mod attaching {
    use super::Imports;
    use crate::Project;
    use crate::cache::Cache;
    use crate::ext::{Extensions, Loadable};
    use crate::index::{Index, Problem};
    use crate::lock::Lock;
    use pe2_ext::ty::{self, Files, Registered};
    use pe2_ext::{Extension, File, Image, Registry, Result, Signature, Table, Typed, Value};

    /// A strip's source: a line of cell names, recognized by its name.
    struct Line;

    impl Registered for Line {
        const NAME: &str = "test.strips@text_line";
        const LABEL: &str = "A line of names";

        fn signatures() -> Vec<Signature> {
            vec![Signature::Name("source.txt".into())]
        }
    }

    impl Files for Line {}

    /// A strip's native form.
    struct Strip;

    impl Registered for Strip {
        const NAME: &str = "test.strips@line_native";
        const LABEL: &str = "A strip's native form";
    }

    impl Files for Strip {}
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    /// A type whose source is a line of cell names: each a cell one texel wide, its native file the source reversed.
    #[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
    fn strips(r: &mut Registry) -> Result<()> {
        let _ = r.value_type::<Line>();
        let _ = r.value_type::<Strip>();
        let _ = r
            .op("cell")
            .input::<Strip>("native")
            .output::<ty::Image>()
            .parameter::<String>("name")
            .provides("cells")
            .members(|asset| {
                asset.parts("cells").into_iter().map(|(name, _)| (name.to_string(), name.to_string())).collect()
            })
            .run(|_| Ok(Image::default()));
        let _ = r.named_type("strip").alias("strip").of(Strip::NAME).register();
        let _ =
            r.op("line").input::<Line>("source").output::<Strip>().derives("size").derives("cells.*.rect").run(strip);
        // The same, saying nothing of what it makes.
        let _ = r.op("silent_line").input::<Line>("source").output::<Strip>().run(strip);
        Ok(())
    }

    fn strip(call: &mut pe2_ext::Call<'_>) -> Result<Typed> {
        let source = call.take::<Line>(0)?;
        let source = String::from_utf8(call.read(source.file()?)?)?;
        let names: Vec<&str> = source.split_whitespace().collect();
        let mut cells = Table::new();
        for (i, name) in names.iter().enumerate() {
            let rect = Value::Array([i, 0, 1, 1].iter().map(|&v| Value::Integer(i64::try_from(v).unwrap())).collect());
            cells.insert((*name).to_string(), Value::Table(Table::from_iter([("rect".to_string(), rect)])));
        }
        let mut settings = Table::new();
        settings
            .insert("size".into(), Value::Array(vec![Value::Integer(i64::try_from(names.len())?), Value::Integer(1)]));
        settings.insert("cells".into(), Value::Table(cells));
        let files = vec![File::new("native.bin", source.bytes().rev().collect())];
        Ok(Typed { ty: Strip::NAME.into(), files, settings, ..Typed::default() })
    }

    fn write(root: &Path, path: &str, text: &str) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
    }

    fn open(root: &Path, extensions: &Extensions) -> (Index, super::Attached) {
        let project = Project::load(root).unwrap();
        let lock = Lock::try_acquire(root, "a test").unwrap();
        let cache = Cache::open(&project, &lock).unwrap();
        let mut index = Index::open(&project, &cache).unwrap();
        let attached = Imports::open(&cache).unwrap().attach(&mut index, extensions);
        (index, attached)
    }

    /// How many of `reading`'s runs are going on inside each other on this thread, and the most there have been.
    static DEEPEST: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    thread_local! {
        static DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    #[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
    fn links(r: &mut Registry) -> Result<()> {
        let _ = r.value_type::<Strip>();
        let next_of = |call: &pe2_ext::Call<'_>| call.settings.get("next").and_then(Value::as_str).map(str::to_string);
        let reads = move |call: &mut pe2_ext::Call<'_>| {
            Ok(pe2_ext::Prepared {
                needs: next_of(call).iter().map(|next| pe2_ext::Need::new(next, "native")).collect(),
                micros: Vec::new(),
            })
        };
        let _ = r.op("link").output::<Strip>().prepare(reads).run(|call| {
            let depth = DEPTH.with(|depth| {
                depth.set(depth.get() + 1);
                depth.get()
            });
            DEEPEST.fetch_max(depth, std::sync::atomic::Ordering::SeqCst);
            let next = call.settings.get("next").and_then(Value::as_str).map(str::to_string);
            let read = next.map(|next| call.output_of(&next, "native")).transpose();
            DEPTH.with(|depth| depth.set(depth.get() - 1));
            read?;
            Ok(Typed {
                ty: Strip::NAME.into(),
                files: vec![File::new("native.bin", b"a".to_vec())],
                ..Typed::default()
            })
        });
        Ok(())
    }

    #[test]
    fn what_a_node_reads_is_made_before_it() {
        const COUNT: usize = 24;
        let root: PathBuf = std::env::temp_dir().join(format!("pe2-imports-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write(&root, "project.toml", "[[layer]]\nname = \"base\"\n");
        // Named last first, so that the order they are found in is the wrong one.
        for at in 0..COUNT {
            let next = if at + 1 == COUNT { String::new() } else { format!("next = \"uid://n{}\"\n", at + 1) };
            let stack = format!(
                "[[stack]]\nop = \"test.strips@link\"\n{next}\n[[stack]]\nop = \"output\"\nname = \"native\"\nretain = \"cache\"\nwhen = \"eager\"\n"
            );
            write(
                &root,
                &format!(".layers/base/n{at:02}.node/node.toml"),
                &format!("uid = \"uid://n{at}\"\n\n{stack}"),
            );
        }
        let extensions = Extensions::load(
            vec![Loadable::from(Extension {
                id: "test.strips",
                version: "0.1.0",
                requires: &[],
                weak: &[],
                optional: &[],
                provides: &[],
                platforms: &[],
                unsandboxed: false,
                register: links,
            })],
            true,
        );
        assert!(extensions.problems.is_empty(), "{:?}", extensions.problems);
        let (_, attached) = open(&root, &extensions);
        assert_eq!((attached.made, attached.failed.len()), (COUNT, 0), "{:?}", attached.failed);
        assert_eq!(DEEPEST.load(std::sync::atomic::Ordering::SeqCst), 1, "each found what it reads made");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `strips`, and `copy`: a strip of what `uid://s` makes, read as it runs - no `references` layer says it.
    fn copying(r: &mut Registry) -> Result<()> {
        strips(r)?;
        let needs = |_: &mut pe2_ext::Call<'_>| {
            Ok(pe2_ext::Prepared { needs: vec![pe2_ext::Need::new("uid://s", "native")], micros: Vec::new() })
        };
        let _ = r.op("copy").output::<Strip>().prepare(needs).run(|call| {
            let pe2_ext::Data::Typed(read) = call.output_of("uid://s", "native")? else {
                anyhow::bail!("uid://s makes no files")
            };
            let bytes = call.read(read.file()?)?;
            anyhow::ensure!(!bytes.is_empty(), "uid://s is empty");
            Ok(Typed { ty: Strip::NAME.into(), files: vec![File::new("native.bin", bytes)], ..Typed::default() })
        });
        Ok(())
    }

    #[test]
    fn what_reads_another_node_as_it_runs_is_attached_again_when_that_changes() {
        let root: PathBuf = std::env::temp_dir().join(format!("pe2-imports-readers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write(&root, "project.toml", "[[layer]]\nname = \"base\"\n");
        write(&root, ".layers/base/s.node/node.toml", &format!("uid = \"uid://s\"\n{STACK}"));
        write(&root, ".layers/base/s.node/source.txt", "a b");
        let copy = "[[stack]]\nop = \"test.strips@copy\"\n\n[[stack]]\nop = \"output\"\nname = \"native\"\nretain = \"cache\"\nwhen = \"eager\"\n";
        // Read through another: `c` copies `uid://s`, which nothing of its declaration names.
        write(&root, ".layers/base/c.node/node.toml", &format!("uid = \"uid://c\"\n{copy}"));
        let extension = Extension {
            id: "test.strips",
            version: "0.1.0",
            requires: &[],
            weak: &[],
            optional: &[],
            provides: &[],
            platforms: &[],
            unsandboxed: false,
            register: copying,
        };
        let extensions = Extensions::load(vec![Loadable::from(extension)], true);
        assert!(extensions.problems.is_empty(), "{:?}", extensions.problems);
        let project = Project::load(&root).unwrap();
        let (mut index, attached) = open(&root, &extensions);
        assert_eq!((attached.made, attached.failed.len()), (2, 0), "{:?}", attached.failed);
        let lock = Lock::try_acquire(&root, "a test").unwrap();
        let imports = Imports::open(&Cache::open(&project, &lock).unwrap()).unwrap();
        drop(lock);
        // Its first attach is what it learns what each read from.
        imports.attach(&mut index, &extensions);
        write(&root, ".layers/base/s.node/source.txt", "a b c");
        let refreshed = index.refresh(&project, &[root.join(".layers/base/s.node/source.txt")]).unwrap();
        assert!(!refreshed.assets.contains("c"), "c's own files did not change");
        imports.attach_changed(&mut index, &extensions, &refreshed.assets);
        let (fresh, _) = open(&root, &extensions);
        assert_eq!(shown(&index), shown(&fresh));
        // What it reads emptied, it fails; filled again, it is made again, though its failure read nothing.
        for (text, fails) in [("", true), ("a", false)] {
            write(&root, ".layers/base/s.node/source.txt", text);
            let refreshed = index.refresh(&project, &[root.join(".layers/base/s.node/source.txt")]).unwrap();
            imports.attach_changed(&mut index, &extensions, &refreshed.assets);
            let failed = index.problems.iter().any(|p| matches!(p, Problem::Import { asset, .. } if asset == "c"));
            assert_eq!(failed, fails, "{text:?}: {:?}", index.problems);
            let (fresh, _) = open(&root, &extensions);
            assert_eq!(shown(&index), shown(&fresh), "{text:?}");
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Runs of `counted`, of the tests below, by the node: they run at once, so each counts its own.
    static RUNS: Mutex<Vec<String>> = Mutex::new(Vec::new());

    fn runs_of(prefix: &str) -> usize {
        RUNS.lock().unwrap().iter().filter(|run| run.starts_with(prefix)).count()
    }

    /// A project of a layer of these nodes, with the extension of `stages`.
    fn staged(name: &str, nodes: &[(&str, String)]) -> (PathBuf, Extensions) {
        let root: PathBuf = std::env::temp_dir().join(format!("pe2-imports-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write(&root, "project.toml", "[[layer]]\nname = \"base\"\n");
        for (path, declaration) in nodes {
            write(&root, &format!(".layers/base/{path}.node/node.toml"), declaration);
        }
        let extension = Extension {
            id: "test.staged",
            version: "0.1.0",
            requires: &[],
            weak: &[],
            optional: &[],
            provides: &[],
            platforms: &[],
            unsandboxed: false,
            register: stages,
        };
        let extensions = Extensions::load(vec![Loadable::from(extension)], true);
        assert!(extensions.problems.is_empty(), "{:?}", extensions.problems);
        (root, extensions)
    }

    /// A node of one layer of `test.staged`, with what is set on it, kept or not.
    fn staged_node(op: &str, settings: &str, retain: &str) -> String {
        format!(
            "[[stack]]\nop = \"test.staged@{op}\"\n{settings}\n[[stack]]\nop = \"output\"\nname = \"native\"\nretain = \"{retain}\"\n"
        )
    }

    fn one_table(key: &str, value: &str) -> Table {
        Table::from_iter([(key.to_string(), Value::String(value.to_string()))])
    }

    #[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
    fn stages(r: &mut Registry) -> Result<()> {
        // Counts its runs, by what it is given to count by.
        let _ = r.op("counted").output::<ty::Table>().run(|call| {
            RUNS.lock().unwrap().push(call.settings.get("as").and_then(Value::as_str).unwrap_or_default().to_string());
            Ok(call.settings.clone())
        });
        // Reads what the setting names, and what that says comes next: known only once the first is made.
        let _ = r.op("hops").output::<ty::Table>().steps(pe2_ext::stepped::replayed(|call| {
            let first = call.settings.get("first").and_then(Value::as_str).unwrap_or_default().to_string();
            let found = call.table_of(&first, "native")?;
            let next = found.get("next").and_then(Value::as_str).unwrap_or_default().to_string();
            call.table_of(&next, "native")
        }));
        // Reads each of its `reads`, a list of nodes.
        let list = |call: &pe2_ext::Call<'_>| -> Vec<String> {
            call.settings
                .get("reads")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        };
        let _ = r
            .op("reader")
            .output::<ty::Table>()
            .prepare(move |call| {
                Ok(pe2_ext::Prepared {
                    needs: list(call).iter().map(|node| pe2_ext::Need::new(node, "native")).collect(),
                    micros: Vec::new(),
                })
            })
            .run(move |call| {
                for node in list(call) {
                    call.table_of(&node, "native")?;
                }
                Ok(Table::new())
            });
        // A piece, as what it is given says, counted.
        let _ = r.op("piece").output::<ty::Table>().run(|call| {
            let name = call.settings.get("name").and_then(Value::as_str).unwrap_or_default();
            RUNS.lock().unwrap().push(format!("piece {name}"));
            Ok(one_table("made", name))
        });
        // What the pieces came to, in their order.
        let _ = r.op("join").input::<ty::Table>("pieces").output::<ty::Table>().run(|call| {
            let mut names = String::new();
            for piece in call.take_all::<ty::Table>(0)? {
                names.push_str(piece.get("made").and_then(Value::as_str).unwrap_or_default());
            }
            Ok(one_table("joined", &names))
        });
        // A piece for each of its `pieces`, then those joined.
        let _ = r
            .op("whole")
            .output::<ty::Table>()
            .prepare(|call| {
                let names: Vec<String> = call
                    .settings
                    .get("pieces")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect();
                let mut micros: Vec<pe2_ext::Micro> = names
                    .iter()
                    .map(|name| pe2_ext::Micro {
                        op: "piece".into(),
                        label: name.clone(),
                        settings: one_table("name", name),
                        ports: vec![],
                        keep: true,
                    })
                    .collect();
                let pieces = (0..names.len()).map(pe2_ext::MicroInput::micro).collect();
                micros.push(pe2_ext::Micro {
                    op: "join".into(),
                    label: "join".into(),
                    ports: vec![pieces],
                    ..pe2_ext::Micro::default()
                });
                Ok(pe2_ext::Prepared { needs: Vec::new(), micros })
            })
            .run(|_| anyhow::bail!("it is done by its micro ops"));
        let _ = r.op("chained").output::<ty::Table>().steps(chained());
        let _ = r.op("unasked").output::<ty::Table>().steps(unasked());
        let _ = r.op("followed").output::<ty::Table>().steps(followed());
        let _ = r.op("counted_on").output::<ty::Table>().steps(counted_on());
        Ok(())
    }

    /// Reads what its setting names, then what that names next, each requested where a step ends; doubles three numbers,
    /// each its own micro op, counted; then reads both nodes again, each an item reading its own in steps.
    #[pe2_ext::steps]
    fn chained(call: &mut pe2_ext::Call<'_>) -> Result<Table> {
        let first: String = call.settings.get("first").and_then(Value::as_str).unwrap_or_default().to_string();
        #[request]
        let found: Table = call.table_of(&first, "native")?;
        let next: String = found.get("next").and_then(Value::as_str).unwrap_or_default().to_string();
        #[request]
        let arrived: Vec<Table> = call.tables_of([(next.as_str(), "native")])?;
        #[each]
        let doubled: Vec<i64> = vec![1_i64, 2, 3]
            .into_iter()
            .map(|n: i64| {
                RUNS.lock().unwrap().push(format!("doubled {n}"));
                n * 2
            })
            .collect();
        #[each]
        let hops: Vec<Table> = [first, next].into_iter().map(hop).collect();
        let mut out = arrived[0].clone();
        out.insert("doubled".into(), Value::Array(doubled.into_iter().map(Value::Integer).collect()));
        out.insert("hops".into(), Value::Array(hops.into_iter().map(Value::Table).collect()));
        Ok(out)
    }

    /// An item reading the node it is given.
    #[pe2_ext::steps]
    fn hop(call: &mut pe2_ext::Call<'_>, node: String) -> Result<Table> {
        #[request]
        let got: Table = call.table_of(&node, "native")?;
        Ok(got)
    }

    /// Follows `next` from node to node, a step each, until a node names none: the nodes it went through.
    #[pe2_ext::steps]
    fn followed(call: &mut pe2_ext::Call<'_>) -> Result<Table> {
        let mut at: String = call.settings.get("first").and_then(Value::as_str).unwrap_or_default().to_string();
        let mut seen: Vec<String> = Vec::new();
        loop {
            #[request]
            let found: Table = call.table_of(&at, "native")?;
            seen.push(at.clone());
            let Some(next) = found.get("next").and_then(Value::as_str) else { break };
            at = next.to_string();
        }
        Ok(Table::from_iter([("seen".to_string(), Value::Array(seen.into_iter().map(Value::String).collect()))]))
    }

    /// How many nodes follow from the first, `next` after `next`: a helper calling itself while there is a next.
    #[pe2_ext::steps]
    fn counted_on(call: &mut pe2_ext::Call<'_>) -> Result<Table> {
        let first: String = call.settings.get("first").and_then(Value::as_str).unwrap_or_default().to_string();
        #[call]
        let length: i64 = depth(call, first)?;
        let names: Vec<String> = vec!["one".to_string(), "two".to_string()];
        let mut keys: Vec<String> = Vec::new();
        for name in names {
            #[request]
            let found: Table = call.table_of(&name, "native")?;
            keys.extend(found.keys().cloned());
        }
        Ok(Table::from_iter([
            ("length".to_string(), Value::Integer(length)),
            ("keys".to_string(), Value::Array(keys.into_iter().map(Value::String).collect())),
        ]))
    }

    /// How many nodes there are from this one on.
    #[pe2_ext::steps]
    fn depth(call: &mut pe2_ext::Call<'_>, node: String) -> Result<i64> {
        #[request]
        let found: Table = call.table_of(&node, "native")?;
        if let Some(next) = found.get("next").and_then(Value::as_str) {
            let next: String = next.to_string();
            #[call]
            let below: i64 = depth(call, next)?;
            return Ok(below + 1);
        }
        Ok(1)
    }

    /// Reads what nothing requested.
    #[pe2_ext::steps]
    fn unasked(call: &mut pe2_ext::Call<'_>) -> Result<Table> {
        let read: Table = call.table_of("one", "native")?;
        #[request]
        let never: Table = call.table_of("two", "native")?;
        Ok(read)
    }

    fn opened(root: &Path) -> (Lock, Cache, Index) {
        let project = Project::load(root).unwrap();
        let lock = Lock::try_acquire(root, "a test").unwrap();
        let cache = Cache::open(&project, &lock).unwrap();
        let index = Index::open(&project, &cache).unwrap();
        (lock, cache, index)
    }

    #[test]
    fn what_is_transient_lasts_the_run_and_no_longer() {
        let shared = staged_node("counted", "as = \"lasting\"", "transient");
        let reads = "reads = [\"shared\"]";
        let nodes = [
            ("shared", shared),
            ("a", staged_node("reader", reads, "transient")),
            ("b", staged_node("reader", reads, "transient")),
        ];
        let (root, extensions) = staged("lasting", &nodes);
        let (_lock, cache, index) = opened(&root);
        let make = |engine: &crate::graph::exec::Engine, at: &str| {
            extensions.output(engine, &index, &index.assets[at], "native", true).unwrap()
        };
        let store = crate::graph::store::Store::open(&cache).unwrap();
        let run = crate::graph::exec::Engine::new(extensions.operations(), Some(store.clone())).for_a_run();
        drop((make(&run, "a"), make(&run, "b")));
        assert_eq!(runs_of("lasting"), 1, "one run shares it");
        drop(make(&run, "a"));
        assert_eq!(runs_of("lasting"), 1);
        // Outside a run, it is made for each.
        let passing = crate::graph::exec::Engine::new(extensions.operations(), Some(store));
        drop((make(&passing, "a"), make(&passing, "b")));
        assert_eq!(runs_of("lasting"), 3);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_read_can_name_what_the_last_one_found() {
        let nodes = [
            ("hop", staged_node("hops", "first = \"one\"", "transient")),
            ("one", staged_node("counted", "next = \"two\"", "transient")),
            ("two", staged_node("counted", "arrived = \"yes\"", "transient")),
        ];
        let (root, extensions) = staged("hops", &nodes);
        let (_lock, cache, index) = opened(&root);
        let engine = crate::graph::exec::Engine::new(
            extensions.operations(),
            Some(crate::graph::store::Store::open(&cache).unwrap()),
        );
        let given = extensions.output(&engine, &index, &index.assets["hop"], "native", true).unwrap();
        let value = format!("{:?}", given.value.unwrap());
        assert!(value.contains("arrived"), "{value}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn steps_read_what_the_steps_before_requested_and_join_what_they_spawned() {
        let nodes = [
            ("chain", staged_node("chained", "first = \"one\"", "transient")),
            ("one", staged_node("counted", "next = \"two\"", "transient")),
            ("two", staged_node("counted", "arrived = \"yes\"", "transient")),
            ("unasked", staged_node("unasked", "", "transient")),
            ("followed", staged_node("followed", "first = \"one\"", "transient")),
            ("counted_on", staged_node("counted_on", "first = \"one\"", "transient")),
            ("three", staged_node("counted", "last = \"yes\"", "transient")),
        ];
        let (root, extensions) = staged("chained", &nodes);
        let (_lock, cache, index) = opened(&root);
        let store = crate::graph::store::Store::open(&cache).unwrap();
        let make = |at: &str| {
            let engine = crate::graph::exec::Engine::new(extensions.operations(), Some(store.clone()));
            extensions.output(&engine, &index, &index.assets[at], "native", true)
        };
        let value = format!("{:?}", make("chain").unwrap().value.unwrap());
        assert!(value.contains("arrived"), "the second read, named by the first: {value}");
        assert!(value.contains("\"doubled\": Array([Integer(2), Integer(4), Integer(6)])"), "in their order: {value}");
        assert_eq!(runs_of("doubled "), 3);
        assert!(value.contains("\"hops\": Array([Table({\"next\": String(\"two\")}), Table({\"arrived\""), "{value}");
        drop(make("chain").unwrap());
        assert_eq!(runs_of("doubled "), 3, "each item kept, by what it was given");
        // A loop, a step a turn: one, then two, which names three, which names none.
        write(
            &root,
            ".layers/base/two.node/node.toml",
            &staged_node("counted", "arrived = \"yes\"\nnext = \"three\"", "transient"),
        );
        let index = Index::open(&Project::load(&root).unwrap(), &cache).unwrap();
        let engine = crate::graph::exec::Engine::new(extensions.operations(), Some(store.clone()));
        let followed = extensions.output(&engine, &index, &index.assets["followed"], "native", true).unwrap();
        let followed = format!("{:?}", followed.value.unwrap());
        assert!(followed.contains("[String(\"one\"), String(\"two\"), String(\"three\")]"), "{followed}");
        // A helper calling itself, a `for` with a cut in it.
        let counted = extensions.output(&engine, &index, &index.assets["counted_on"], "native", true).unwrap();
        let counted = format!("{:?}", counted.value.unwrap());
        assert!(counted.contains("\"length\": Integer(3)"), "{counted}");
        assert!(counted.contains("[String(\"next\"), String(\"arrived\"), String(\"next\")]"), "{counted}");
        let why = make("unasked").err().expect("a read nothing requested");
        assert!(format!("{why:#}").contains("it did not say it reads this"), "{why:#}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_circle_of_reads_is_said_rather_than_followed() {
        let nodes = [
            ("a", staged_node("reader", "reads = [\"b\", \"c\"]", "transient")),
            ("b", staged_node("reader", "reads = [\"a\"]", "transient")),
            ("c", staged_node("reader", "reads = [\"a\"]", "transient")),
        ];
        let (root, extensions) = staged("circle", &nodes);
        let (_lock, cache, index) = opened(&root);
        let engine = crate::graph::exec::Engine::new(
            extensions.operations(),
            Some(crate::graph::store::Store::open(&cache).unwrap()),
        );
        let why = extensions.output(&engine, &index, &index.assets["a"], "native", true).err().expect("a circle");
        assert!(format!("{why:#}").contains("reads, through others, itself"), "{why:#}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_split_keeps_its_pieces_and_makes_only_what_changed() {
        let settings = |pieces: &str| staged_node("whole", &format!("pieces = [{pieces}]"), "transient");
        let (root, extensions) = staged("split", &[("whole", settings("\"s1\", \"s2\", \"s3\""))]);
        let (_lock, cache, index) = opened(&root);
        let store = crate::graph::store::Store::open(&cache).unwrap();
        let make = |index: &Index| {
            let engine = crate::graph::exec::Engine::new(extensions.operations(), Some(store.clone()));
            let value =
                extensions.output(&engine, index, &index.assets["whole"], "native", true).unwrap().value.unwrap();
            format!("{value:?}")
        };
        let first = make(&index);
        assert!(first.contains("s1s2s3"), "in their order: {first}");
        assert_eq!(runs_of("piece s"), 3);
        drop(make(&index));
        assert_eq!(runs_of("piece s"), 3, "kept");
        // One more piece: only it is made.
        write(&root, ".layers/base/whole.node/node.toml", &settings("\"s1\", \"s2\", \"s3\", \"s4\""));
        let project = Project::load(&root).unwrap();
        let again = Index::open(&project, &cache).unwrap();
        assert!(make(&again).contains("s1s2s3s4"));
        assert_eq!(runs_of("piece s"), 4);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn what_is_worked_out_at_once_comes_back_in_its_order() {
        let items: Vec<usize> = (0..500).collect();
        assert_eq!(crate::parallel(&items, |item| item * 2), items.iter().map(|item| item * 2).collect::<Vec<_>>());
        assert_eq!(crate::parallel(&[] as &[usize], |item| *item), Vec::<usize>::new());
    }

    #[test]
    fn an_attach_nobody_wants_gives_up_without_making_anything() {
        let root: PathBuf = std::env::temp_dir().join(format!("pe2-imports-stopped-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write(&root, "project.toml", "[[layer]]\nname = \"base\"\n");
        write(&root, ".layers/base/s.node/node.toml", &format!("uid = \"uid://s\"\n{STACK}"));
        write(&root, ".layers/base/s.node/source.txt", "a b");
        let extensions = Extensions::load(
            vec![Loadable::from(Extension {
                id: "test.strips",
                version: "0.1.0",
                requires: &[],
                weak: &[],
                optional: &[],
                provides: &[],
                platforms: &[],
                unsandboxed: false,
                register: strips,
            })],
            true,
        );
        let project = Project::load(&root).unwrap();
        let lock = Lock::try_acquire(&root, "a test").unwrap();
        let cache = Cache::open(&project, &lock).unwrap();
        let mut index = Index::open(&project, &cache).unwrap();
        let imports = Imports::open(&cache).unwrap();
        imports.stopper().store(true, std::sync::atomic::Ordering::Relaxed);
        let attached = imports.attach(&mut index, &extensions);
        assert_eq!((attached.made, attached.kept, attached.failed.len()), (0, 0, 0));
        assert!(index.assets["s"].files.iter().all(|file| file.made_by.is_none()), "nothing was made");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A strip's stack: its source through the importer, kept as its native form.
    const STACK: &str = "\n[[stack]]\nop = \"file\"\nfile = \"source.txt\"\n\n[[stack]]\nop = \"test.strips@line\"\n\n\
                         [[stack]]\nop = \"output\"\nname = \"native\"\nretain = \"cache\"\nwhen = \"eager\"\n";

    #[test]
    fn imports_resolve_settings_and_parts() {
        let root: PathBuf = std::env::temp_dir().join(format!("pe2-imports-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write(&root, "project.toml", "[[layer]]\nname = \"base\"\n");
        write(
            &root,
            ".layers/base/s.node/node.toml",
            &format!(
                "uid = \"uid://s\"\nsize = [9, 9]\ncells.a = {{ palette = \"warm\" }}\ncells.gone = {{}}\n{STACK}"
            ),
        );
        write(&root, ".layers/base/s.node/source.txt", "a b");
        write(&root, ".layers/base/r.node/node.toml", "fine = \"uid://s#cells/b\"\nbad = \"uid://s#cells/z\"\n");
        let extensions = Extensions::load(
            vec![Loadable::from(Extension {
                id: "test.strips",
                version: "0.1.0",
                requires: &[],
                weak: &[],
                optional: &[],
                provides: &[],
                platforms: &[],
                unsandboxed: false,
                register: strips,
            })],
            true,
        );
        assert!(extensions.problems.is_empty(), "{:?}", extensions.problems);
        let (index, attached) = open(&root, &extensions);
        assert_eq!((attached.made, attached.kept, attached.failed.len()), (1, 0, 0));
        let s = &index.assets["s"];
        let made = s.files.iter().find(|f| f.name == "native.bin").unwrap();
        assert_eq!(made.made_by.as_deref(), Some("test.strips@line"));
        assert_eq!(std::fs::read(&made.path).unwrap(), b"b a");
        let settings = s.settings().to_toml();
        assert_eq!(settings["size"].as_array().unwrap()[0].as_integer(), Some(2));
        assert_eq!(settings["cells"]["a"]["palette"].as_str(), Some("warm"));
        assert!(settings["cells"]["b"]["rect"].is_array());
        let mut problems: Vec<String> = index.problems.iter().map(|p| format!("{p:?}")).collect();
        problems.sort();
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(
            matches!(index.problems.iter().find(|p| matches!(p, Problem::MissingOutput { .. })), Some(Problem::MissingOutput { output, .. }) if output == "cells/z")
        );
        assert!(index.problems.iter().any(|p| matches!(p, Problem::Overridden { setting, .. } if setting == "size")));
        assert!(index.problems.iter().any(|p| matches!(p, Problem::Orphan { part, .. } if part == "cells.gone")));
        // Opened again: found made. The source changed: made again, its old import swept.
        let (_, attached) = open(&root, &extensions);
        assert_eq!((attached.made, attached.kept), (0, 1));
        write(&root, ".layers/base/s.node/source.txt", "a b z");
        let (index, attached) = open(&root, &extensions);
        assert_eq!((attached.made, attached.kept), (1, 0));
        assert!(!index.problems.iter().any(|p| matches!(p, Problem::MissingOutput { .. })));
        // Swept, the old import goes.
        let store = crate::graph::store::Store::at(root.join(".cache/store")).unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
        let _ = store.sweep_older(&super::live_nodes(&index), &std::collections::HashSet::new(), later);
        let kept = |sub: &str| std::fs::read_dir(root.join(".cache/store").join(sub)).unwrap().count();
        assert_eq!((kept("traces"), kept("values")), (1, 1));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn what_an_import_makes_lists_its_members_though_nothing_declares_them() {
        let root: PathBuf = std::env::temp_dir().join(format!("pe2-imports-silent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write(&root, "project.toml", "[[layer]]\nname = \"base\"\n");
        let stack = STACK.replace("test.strips@line", "test.strips@silent_line");
        write(
            &root,
            ".layers/base/s.node/node.toml",
            &format!("uid = \"uid://s\"\ncells.a = {{ palette = \"warm\" }}\n{stack}"),
        );
        write(&root, ".layers/base/s.node/source.txt", "a b");
        write(&root, ".layers/base/r.node/node.toml", "fine = \"uid://s#cells/b\"\nbad = \"uid://s#cells/z\"\n");
        let extensions = Extensions::load(
            vec![Loadable::from(Extension {
                id: "test.strips",
                version: "0.1.0",
                requires: &[],
                weak: &[],
                optional: &[],
                provides: &[],
                platforms: &[],
                unsandboxed: false,
                register: strips,
            })],
            true,
        );
        assert!(extensions.problems.is_empty(), "{:?}", extensions.problems);
        let (index, _) = open(&root, &extensions);
        let settings = index.assets["s"].settings().to_toml();
        assert_eq!(settings["cells"]["a"]["palette"].as_str(), Some("warm"));
        assert!(settings["cells"]["a"]["rect"].is_array(), "what the node says stays, what was made joins it");
        assert!(settings["cells"]["b"]["rect"].is_array());
        let missing: Vec<&Problem> =
            index.problems.iter().filter(|p| matches!(p, Problem::MissingOutput { .. })).collect();
        assert_eq!(missing.len(), 1, "{missing:?}");
        assert!(format!("{:?}", missing[0]).contains("cells/z"), "{missing:?}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// What an index's imports show: each asset's made files and settings, and the problems (in no order).
    fn shown(index: &Index) -> String {
        let mut out: Vec<String> = index
            .assets
            .values()
            .map(|a| {
                let made: Vec<_> = a.files.iter().filter(|f| f.made_by.is_some()).map(|f| (&f.name, &f.sha1)).collect();
                format!("{} {made:?} {:?}", a.path, a.imported.as_ref().map(|i| &i.settings))
            })
            .collect();
        let mut problems: Vec<String> = index.problems.iter().map(|p| format!("{p:?}")).collect();
        problems.sort();
        out.extend(problems);
        out.join("\n")
    }

    #[test]
    fn attaching_what_changed_shows_what_attaching_all_shows() {
        let root: PathBuf = std::env::temp_dir().join(format!("pe2-imports-changed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write(&root, "project.toml", "[[layer]]\nname = \"base\"\n");
        let declared = format!("cells.a = {{}}\n{STACK}");
        write(&root, ".layers/base/s.node/node.toml", &format!("uid = \"uid://s\"\n{declared}"));
        write(&root, ".layers/base/s.node/source.txt", "a b");
        write(&root, ".layers/base/t.node/node.toml", &format!("uid = \"uid://t\"\n{declared}"));
        write(&root, ".layers/base/t.node/source.txt", "a");
        write(&root, ".layers/base/r.node/node.toml", "one = \"uid://s#cells/b\"\ntwo = \"uid://t#cells/b\"\n");
        let extensions = Extensions::load(
            vec![Loadable::from(Extension {
                id: "test.strips",
                version: "0.1.0",
                requires: &[],
                weak: &[],
                optional: &[],
                provides: &[],
                platforms: &[],
                unsandboxed: false,
                register: strips,
            })],
            true,
        );
        let project = Project::load(&root).unwrap();
        let (mut index, _) = open(&root, &extensions);
        let lock = Lock::try_acquire(&root, "a test").unwrap();
        let imports = Imports::open(&Cache::open(&project, &lock).unwrap()).unwrap();
        drop(lock);
        let base = |p: &str| root.join(".layers/base").join(p);
        // A source repainted (a part gone and one come), a declaration naming another part, both at once.
        for (path, text) in [
            ("s.node/source.txt", "a"),
            ("t.node/source.txt", "a b"),
            ("r.node/node.toml", "one = \"uid://s#cells/a\"\ntwo = \"uid://t#cells/c\"\n"),
        ] {
            write(&root, &format!(".layers/base/{path}"), text);
            let refreshed = index.refresh(&project, &[base(path)]).unwrap();
            imports.attach_changed(&mut index, &extensions, &refreshed.assets);
            let (fresh, _) = open(&root, &extensions);
            assert_eq!(shown(&index), shown(&fresh), "after {path}");
        }
        std::fs::remove_dir_all(&root).unwrap();
    }
}
