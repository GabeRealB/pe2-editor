//! The project's generated layers as their plans say they are: what each reads (its plan's header) and whether what it
//! holds is what its plan makes (`project::plan::state`) - known without the discs, but by reading the whole plan, so
//! worked out off the window's thread, again whenever a plan or what one made changes. Where a layer is to be filled,
//! or that is asked (`want_discs`), its discs are looked for too, each image of a disc's size hashed.

use eframe::egui;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::SystemTime;

use project::Project;
use project::ext::Extensions;
use project::plan::{Disc, Header, State};

/// A generated layer as its plan says it is.
#[derive(Clone)]
pub struct Planned {
    /// The layer it fills.
    pub fills: String,
    /// What the plan reads and needs; why not, where its plan cannot be read.
    pub header: Result<Header, String>,
    /// How the layer is; none until worked out.
    pub state: Option<Result<State, String>>,
    /// Its plan's discs, looked for: none where they were not.
    pub discs: Option<Vec<Disc>>,
}

#[derive(Default)]
pub struct Plans {
    /// By generated layer, in the stack's order.
    pub layers: BTreeMap<String, Planned>,
    /// The plans' and records' stamps they were worked out from.
    stamps: Vec<Option<SystemTime>>,
    working: Option<mpsc::Receiver<BTreeMap<String, Planned>>>,
    /// Whether they were worked out at all.
    worked: bool,
    /// Worked out again at the next look, whatever the stamps: the project was read again.
    again: bool,
    /// The discs looked for, whatever the layers' states.
    discs_wanted: bool,
}

/// The files a project's plans are worked out from: each generated layer's plan and its record.
pub fn watched(project: &Project) -> Vec<PathBuf> {
    project.generated().flat_map(|l| [l.plan(), l.path.join(project::plan::RECORD)]).collect()
}

impl Plans {
    /// Worked out again at the next look: the project, or its extensions, were read again.
    pub fn again(&mut self) {
        self.again = true;
    }

    /// Have the discs looked for, whatever the layers' states: worked out again where they were not.
    pub fn want_discs(&mut self) {
        if !self.discs_wanted {
            self.discs_wanted = true;
            self.again = self.layers.values().any(|p| p.discs.is_none());
        }
    }

    /// Take what was worked out, and work it out again where a plan or a record changed since - by when each
    /// last changed, as the worker last looked (`stamps`; none: not yet). Asked each frame.
    pub fn update(
        &mut self,
        project: &Project,
        extensions: &Arc<Extensions>,
        ctx: &egui::Context,
        stamps: Option<Vec<Option<SystemTime>>>,
    ) {
        if let Some(working) = &self.working
            && let Ok(layers) = working.try_recv()
        {
            self.layers = layers;
            self.working = None;
        }
        let Some(stamps) = stamps.filter(|_| self.working.is_none()) else { return };
        if stamps == self.stamps && !std::mem::take(&mut self.again) && self.worked {
            return;
        }
        self.stamps = stamps;
        self.worked = true;
        if project.generated().next().is_none() {
            self.layers.clear();
            return;
        }
        let (project, extensions, ctx, wanted) = (project.clone(), extensions.clone(), ctx.clone(), self.discs_wanted);
        let (sender, receiver) = mpsc::channel();
        let work = move || {
            let mut layers = BTreeMap::new();
            for layer in project.generated() {
                let plan = layer.plan();
                let header = project::plan::header(&plan).map_err(|e| format!("{e:#}"));
                let state = project::plan::state(&plan, &layer.path, &extensions).map_err(|e| format!("{e:#}"));
                let fills = layer.fills.clone().unwrap_or_default();
                let filled = matches!(state, Ok(State::Filled | State::Partial(_) | State::NoPlan));
                let discs = match &header {
                    Ok(header) if wanted || !filled => Some(project::plan::discs(&project, header)),
                    _ => None,
                };
                layers.insert(layer.name.clone(), Planned { fills, header, state: Some(state), discs });
            }
            let _ = sender.send(layers);
            // What reading the plans freed, given back.
            project::give_back_memory();
            ctx.request_repaint();
        };
        if std::thread::Builder::new().name("plans".into()).spawn(work).is_ok() {
            self.working = Some(receiver);
        }
    }

    /// How a generated layer is, in a few words.
    #[must_use]
    pub fn said(&self, layer: &str) -> String {
        match self.layers.get(layer).and_then(|p| p.state.as_ref()) {
            None => "looking at its plan".into(),
            Some(Err(why)) => format!("its plan cannot be read: {why}"),
            Some(Ok(State::NoPlan)) => "no plan fills it".into(),
            Some(Ok(State::Empty)) => "empty: its plan has not run".into(),
            Some(Ok(State::Filled)) => "filled, as its plan says".into(),
            Some(Ok(State::Partial(not))) => format!("filled but for {}: their discs were not read", not.join(", ")),
            Some(Ok(State::Stale(why))) => format!("not as its plan says now: {}", why.join("; ")),
        }
    }
}
