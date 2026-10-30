//! What an epoch does to a project: an output pulled is made, or found kept, and goes where it says, under `build/`; pushed, it goes stale and what it left there goes.
//! What is wired to what is read from the plans of every asset, and a build is only the `build` event: the outputs that answer it say so.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result};
use pe2_ext::data::BuildConfig;

use crate::events::{Action, Effects, Expr, Outcome, Report, Trigger, order, schedule};
use crate::ext::Extensions;
use crate::graph::exec::Engine;
use crate::graph::{Content, File, Output, Plan, Value};
use crate::index::{Asset, Index};
use crate::{Config, Project};

/// What a project's graphs wire to events.
#[derive(Clone, Debug, Default)]
pub struct Wiring {
    pub triggers: Vec<Trigger>,
    /// What could not be wired, by asset, with why.
    pub problems: Vec<(String, String)>,
}

impl Wiring {
    /// How much is wired to each event.
    #[must_use]
    pub fn counts(&self) -> BTreeMap<String, usize> {
        let mut counts = BTreeMap::new();
        for trigger in &self.triggers {
            for event in trigger.when.named() {
                *counts.entry(event.to_string()).or_default() += 1;
            }
        }
        counts
    }
}

fn any_of(plan: &Plan, steps: &[usize]) -> Expr {
    match steps {
        [step] => plan.expression(*step),
        steps => Expr::Any(steps.iter().map(|step| plan.expression(*step)).collect()),
    }
}

fn signals_of(output: &Output) -> Vec<String> {
    [&output.events.ran, &output.events.changed, &output.events.failed].into_iter().flatten().cloned().collect()
}

/// A project's triggers, from the plans of all a config holds.
#[must_use]
pub fn wiring(index: &Index, extensions: &Extensions, config: &Config) -> Wiring {
    let mut triggers = Vec::new();
    for asset in index.assets.values().filter(|asset| !config.disabled.contains(&asset.path)) {
        let (plan, _) = extensions.plan(asset);
        for output in &plan.outputs {
            let target = (asset.path.clone(), output.name.clone());
            if !output.events.pull.is_empty() {
                let action = Action::Pull { asset: target.0.clone(), output: target.1.clone() };
                triggers.push(Trigger { when: any_of(&plan, &output.events.pull), action, signals: signals_of(output) });
            }
            if !output.events.listen.is_empty() {
                let action = Action::Push { asset: target.0, output: target.1 };
                triggers.push(Trigger { when: any_of(&plan, &output.events.listen), action, signals: Vec::new() });
            }
        }
        for rule in &plan.rules {
            let action = Action::Signal(rule.event.clone());
            triggers.push(Trigger { when: any_of(&plan, &rule.when), action, signals: vec![rule.event.clone()] });
        }
    }
    for hook in &extensions.hooks {
        let action = Action::Hook { op: hook.op.clone(), event: hook.event.clone(), signals: hook.signals.clone() };
        triggers.push(Trigger { when: Expr::Event(hook.event.clone()), action, signals: hook.signals.clone() });
    }
    let named: Vec<String> = triggers.iter().map(|t: &Trigger| named(&t.action)).collect();
    let (triggers, refused) = order(triggers);
    let problems = refused.into_iter().map(|(at, why)| (named[at].clone(), why)).collect();
    Wiring { triggers, problems }
}

fn named(action: &Action) -> String {
    match action {
        Action::Pull { asset, .. } | Action::Push { asset, .. } => asset.clone(),
        Action::Signal(event) => event.clone(),
        Action::Hook { op, .. } => op.clone(),
    }
}

/// The config as what is made reads it.
#[must_use]
pub fn build_config(project: &Project, config: &Config) -> BuildConfig {
    let folder = project.root.join("build").join(&config.name);
    BuildConfig { name: config.name.clone(), release: config.release.clone(), target: config.target.clone(), folder: folder.display().to_string() }
}

/// An epoch's effects on a project.
struct Site<'a> {
    project: &'a Project,
    index: &'a Index,
    extensions: &'a Extensions,
    config: &'a BuildConfig,
    engine: Engine,
}

/// Where a deploy puts a file, relative to the project.
fn deployed(place: &str, config: &str, file: &str) -> PathBuf {
    let place = place.replace("{config}", config);
    if place.ends_with('/') { Path::new(&place).join(file) } else { PathBuf::from(place) }
}

impl Site<'_> {
    /// An output, with its plan.
    fn output<'b>(&'b self, asset: &str, output: &str) -> Result<(&'b Asset, Plan, usize)> {
        let found = self.index.assets.get(asset).with_context(|| format!("{asset}: no such asset"))?;
        let (plan, _) = self.extensions.plan(found);
        let at = plan.outputs.iter().position(|o| o.name == output).with_context(|| format!("{asset} has no output `{output}`"))?;
        Ok((found, plan, at))
    }

    fn bytes(&self, file: &File) -> Result<Vec<u8>> {
        match &file.content {
            Content::Inline(bytes) => Ok(bytes.clone()),
            Content::Stored { sha1, .. } => {
                let store = self.engine.store().context("no store holds it")?;
                std::fs::read(store.blob_path(sha1)).with_context(|| sha1.clone())
            }
        }
    }

    /// Put a value's file where its output says, and what is linked to that: a place that holds it already is left, so its readers see no change.
    fn deploy(&self, places: &[String], value: &Value) -> Result<()> {
        let Value::Typed(typed) = value else { anyhow::bail!("it is no file to deploy") };
        let file = typed.file()?;
        let bytes = self.bytes(file)?;
        for place in places {
            let relative = deployed(place, &self.config.name, &file.name);
            let to = self.project.root.join(&relative);
            if let Some(folder) = to.parent() {
                std::fs::create_dir_all(folder).with_context(|| folder.display().to_string())?;
            }
            if std::fs::read(&to).ok().as_deref() != Some(&bytes) {
                crate::cache::write_atomic(&to, &bytes)?;
            }
            for (link, target) in &self.project.links {
                if Path::new(target) == relative {
                    link_to(&to, &self.project.root.join(link))?;
                }
            }
        }
        Ok(())
    }

    fn pull(&self, asset: &str, output: &str) -> Outcome {
        let mut outcome = Outcome::default();
        let (found, plan, at) = match self.output(asset, output) {
            Ok(found) => found,
            Err(why) => return Outcome { failed: Some(format!("{why:#}")), ..outcome },
        };
        let events = &plan.outputs[at].events;
        let key = self.engine.key(&plan, found, at, None);
        let before = self.engine.store().and_then(|store| store.trace(&key)).map(|trace| trace.value);
        let made = self.extensions.output(&self.engine, self.index, found, output, true);
        let deployed = made.and_then(|given| {
            let value = given.value.as_deref().context("no value")?;
            if !events.deploy.is_empty() {
                self.deploy(&events.deploy, value)?;
            }
            Ok((given.kept, given.hash))
        });
        match deployed {
            Ok((kept, hash)) => {
                outcome.made = !kept;
                if !kept {
                    outcome.signaled.extend(events.ran.clone());
                    if before.as_deref() != Some(hash.as_str()) {
                        outcome.signaled.extend(events.changed.clone());
                    }
                }
            }
            Err(why) => {
                outcome.failed = Some(format!("{why:#}"));
                outcome.signaled.extend(events.failed.clone());
            }
        }
        outcome
    }

    /// Stale, what it left under `build/` gone with it, so nothing is left to be mistaken for what is current.
    fn push(&self, asset: &str, output: &str) -> Outcome {
        let (found, plan, at) = match self.output(asset, output) {
            Ok(found) => found,
            Err(why) => return Outcome { failed: Some(format!("{why:#}")), ..Outcome::default() },
        };
        let key = self.engine.key(&plan, found, at, None);
        let Some(store) = self.engine.store() else { return Outcome::default() };
        let trace = store.trace(&key);
        let kept = trace.and_then(|trace| store.get(&trace.value).ok());
        if let Some(Value::Typed(typed)) = kept
            && let Ok(file) = typed.file()
        {
            for place in &plan.outputs[at].events.deploy {
                let _ = std::fs::remove_file(self.project.root.join(deployed(place, &self.config.name, &file.name)));
            }
        }
        store.forget_trace(&key);
        Outcome::default()
    }
}

impl Effects for Site<'_> {
    fn run(&self, action: &Action) -> Outcome {
        match action {
            Action::Pull { asset, output } => self.pull(asset, output),
            Action::Push { asset, output } => self.push(asset, output),
            Action::Signal(event) => Outcome { signaled: vec![event.clone()], ..Outcome::default() },
            Action::Hook { op, event, signals } => match self.extensions.run_hook(&self.engine, self.index, op, event) {
                Ok(()) => Outcome { signaled: signals.clone(), ..Outcome::default() },
                Err(why) => Outcome { failed: Some(format!("{why:#}")), ..Outcome::default() },
            },
        }
    }

    fn is_stale(&self, action: &Action) -> bool {
        let Action::Pull { asset, output } = action else { return false };
        let Ok((found, plan, at)) = self.output(asset, output) else { return false };
        plan.outputs[at].retain != crate::graph::Retain::Transient
            && self.extensions.output_is_stale(&self.engine, self.index, found, &plan, at)
    }
}

/// Run an epoch on a project, begun by `roots`.
#[must_use]
pub fn epoch(project: &Project, index: &Index, extensions: &Extensions, wiring: &Wiring, config: &Config, roots: &[String], cancel: &AtomicBool) -> Report {
    extensions.set_config(Some(build_config(project, config)), &config.disabled);
    let config = build_config(project, config);
    let site = Site { project, index, extensions, config: &config, engine: extensions.engine() };
    schedule(&wiring.triggers, roots, &site, cancel)
}

/// Put a link to a file where it is looked for, or where links take a permission, a copy. One there already and right is left.
fn link_to(file: &Path, at: &Path) -> Result<()> {
    let file = std::fs::canonicalize(file)?;
    #[cfg(unix)]
    if std::fs::read_link(at).is_ok_and(|now| now == file) {
        return Ok(());
    }
    let _ = std::fs::remove_file(at);
    #[cfg(unix)]
    let linked = std::os::unix::fs::symlink(&file, at);
    #[cfg(not(unix))]
    let linked = std::fs::copy(&file, at).map(|_| ());
    linked.with_context(|| at.display().to_string())
}

/// The links a project asks for: where each goes, and the deployed file it stands for.
pub type Links = BTreeMap<String, String>;
