//! Events reaching the graph. An epoch begins from outside - a click, a batch of files - with the events it began with, and everything signaled in it ends
//! with it: an event stays signaled for its epoch, so signaling it again does nothing, and an epoch always ends, however its graphs are wired. Running a node
//! is itself such an event: however much pulls it, it runs once. A push makes an output stale and runs nothing; a pull makes it current, which for what
//! is kept and still holds is nothing, and for what is not kept is running it.
//!
//! `schedule` is the order of it, and knows nothing of what its actions do.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::graph::Gate;

/// What an event expression says, of the events signaled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expr {
    Event(String),
    Gate(&'static Gate, Vec<Expr>),
    /// Any of these: what several wires into one port come to.
    Any(Vec<Expr>),
}

impl Expr {
    #[must_use]
    pub fn holds(&self, signaled: &dyn Fn(&str) -> bool) -> bool {
        match self {
            Expr::Event(name) => signaled(name),
            Expr::Any(any) => any.iter().any(|e| e.holds(signaled)),
            Expr::Gate(gate, inputs) => {
                let inputs: Vec<bool> = inputs.iter().map(|e| e.holds(signaled)).collect();
                (gate.table)(&inputs)
            }
        }
    }

    /// Whether what it says can be known only once its events have settled: a signal arriving could turn it false.
    #[must_use]
    pub fn waits(&self) -> bool {
        match self {
            Expr::Event(_) => false,
            Expr::Any(any) => any.iter().any(Expr::waits),
            Expr::Gate(gate, inputs) => !gate.is_monotone() || inputs.iter().any(Expr::waits),
        }
    }

    /// The events it reads.
    #[must_use]
    pub fn named(&self) -> BTreeSet<&str> {
        let mut events = BTreeSet::new();
        self.events(&mut events);
        events
    }

    fn events<'a>(&'a self, events: &mut BTreeSet<&'a str>) {
        match self {
            Expr::Event(name) => {
                events.insert(name);
            }
            Expr::Any(inputs) | Expr::Gate(_, inputs) => inputs.iter().for_each(|e| e.events(events)),
        }
    }
}

/// What a trigger does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Make an asset's output current.
    Pull { asset: String, output: String },
    /// Make an asset's output stale.
    Push { asset: String, output: String },
    /// Signal an event.
    Signal(String),
    /// Run an extension's hook: it runs before what the same event pulls, and failing stops those pulls.
    Hook { op: String, event: String, signals: Vec<String> },
}

/// Something done where an expression holds, with the events it may signal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trigger {
    pub when: Expr,
    pub action: Action,
    pub signals: Vec<String>,
}

/// What an action came to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub signaled: Vec<String>,
    pub failed: Option<String>,
    /// What what was made says of itself, though it was made.
    pub notes: Vec<String>,
    /// It made something now, not finding it kept.
    pub made: bool,
}

/// What actions are, to whatever runs an epoch.
pub trait Effects: Sync {
    fn run(&self, action: &Action) -> Outcome;
    /// Whether what a pull made has gone stale since.
    fn is_stale(&self, action: &Action) -> bool;
}

/// What an epoch came to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub signaled: BTreeSet<String>,
    /// What actually made something, by asset and output.
    pub made: Vec<(String, String)>,
    pub failed: Vec<(String, String)>,
    /// What what was made says of itself, by asset.
    pub notes: Vec<(String, String)>,
    /// Pulls left stale by what ran after them: wired backwards, or changed while it ran.
    pub stale: Vec<(String, String)>,
    pub cancelled: bool,
}

/// The triggers that can be planned, and why those that cannot were refused, which are left out.
#[must_use]
pub fn order(triggers: Vec<Trigger>) -> (Vec<Trigger>, Vec<(usize, String)>) {
    let refused: Vec<usize> =
        (0..triggers.len()).filter(|t| triggers[*t].when.waits() && reaches_itself(&triggers, *t)).collect();
    let problems =
        refused.iter().map(|t| (*t, "it waits for what it signals: its gate can never settle".to_string())).collect();
    let kept = triggers.into_iter().enumerate().filter(|(t, _)| !refused.contains(t)).map(|(_, trigger)| trigger);
    (kept.collect(), problems)
}

/// The triggers whose signals a trigger waits for.
fn producers(triggers: &[Trigger], trigger: &Trigger) -> Vec<usize> {
    let mut events = BTreeSet::new();
    trigger.when.events(&mut events);
    (0..triggers.len()).filter(|t| triggers[*t].signals.iter().any(|s| events.contains(s.as_str()))).collect()
}

fn reaches_itself(triggers: &[Trigger], start: usize) -> bool {
    let mut seen = BTreeSet::new();
    let mut todo = producers(triggers, &triggers[start]);
    while let Some(t) = todo.pop() {
        if t == start {
            return true;
        }
        if seen.insert(t) {
            todo.extend(producers(triggers, &triggers[t]));
        }
    }
    false
}

fn label(action: &Action) -> (String, String) {
    match action {
        Action::Pull { asset, output } | Action::Push { asset, output } => (asset.clone(), output.clone()),
        Action::Signal(event) => (event.clone(), String::new()),
        Action::Hook { op, .. } => (op.clone(), String::new()),
    }
}

/// Run an epoch begun by `roots`. Each trigger runs at most once. What can fire fires, hooks first; once nothing can, a gate waiting is told, those whose inputs
/// nothing left can signal first, as `order` left no cycle among them.
#[must_use]
pub fn schedule(triggers: &[Trigger], roots: &[String], effects: &dyn Effects, cancel: &AtomicBool) -> Report {
    let _span = tracing::info_span!("epoch", roots = ?roots).entered();
    let mut report = Report { signaled: roots.iter().cloned().collect(), ..Report::default() };
    // Events a failed hook took back: signaled, so not again, but pulling nothing.
    let mut refused: BTreeSet<String> = BTreeSet::new();
    let mut fired = vec![false; triggers.len()];
    let mut rounds = 0;
    let holds = |report: &Report, refused: &BTreeSet<String>, t: &Trigger| {
        t.when.holds(&|event| report.signaled.contains(event) && !refused.contains(event))
    };
    loop {
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            return report;
        }
        let mut ready: Vec<usize> = (0..triggers.len())
            .filter(|t| !fired[*t] && !triggers[*t].when.waits() && holds(&report, &refused, &triggers[*t]))
            .collect();
        if ready.is_empty() {
            // Settled, but for the gates: each told once nothing unfired signals what it reads.
            let settled: Vec<usize> = (0..triggers.len())
                .filter(|t| !fired[*t] && triggers[*t].when.waits())
                .filter(|t| producers(triggers, &triggers[*t]).iter().all(|p| fired[*p] || !triggers[*p].when.waits()))
                .collect();
            if settled.is_empty() {
                break;
            }
            for t in &settled {
                fired[*t] = true;
            }
            ready = settled.into_iter().filter(|t| holds(&report, &refused, &triggers[*t])).collect();
            for t in &ready {
                fired[*t] = false;
            }
            if ready.is_empty() {
                continue;
            }
        }
        let hooks: Vec<usize> =
            ready.iter().copied().filter(|t| matches!(triggers[*t].action, Action::Hook { .. })).collect();
        let batch = if hooks.is_empty() { ready } else { hooks };
        for t in &batch {
            fired[*t] = true;
        }
        let outcomes = run_all(triggers, &batch, effects);
        rounds += 1;
        for (t, outcome) in batch.into_iter().zip(outcomes) {
            let trigger = &triggers[t];
            let (asset, output) = label(&trigger.action);
            report.notes.extend(outcome.notes.iter().map(|note| (asset.clone(), note.clone())));
            if let Some(why) = outcome.failed {
                report.failed.push((asset, why));
                if matches!(trigger.action, Action::Hook { .. }) {
                    let mut events = BTreeSet::new();
                    trigger.when.events(&mut events);
                    refused.extend(events.into_iter().map(str::to_string));
                }
            } else if outcome.made {
                report.made.push((asset, output));
            }
            report.signaled.extend(outcome.signaled);
        }
    }
    if rounds > 1 {
        // What failed is said to have: it was never current.
        let failed = |t: &usize| report.failed.iter().any(|(asset, _)| *asset == label(&triggers[*t].action).0);
        let pulled = (0..triggers.len())
            .filter(|t| fired[*t] && matches!(triggers[*t].action, Action::Pull { .. }) && !failed(t));
        let stale: Vec<usize> = pulled.filter(|t| effects.is_stale(&triggers[*t].action)).collect();
        report.stale = stale.into_iter().map(|t| label(&triggers[t].action)).collect();
    }
    report
}

/// A batch at once, as many at a time as there are threads to lend.
fn run_all(triggers: &[Trigger], batch: &[usize], effects: &dyn Effects) -> Vec<Outcome> {
    crate::parallel(batch, |t| {
        let (asset, output) = label(&triggers[*t].action);
        let _span = tracing::info_span!("trigger", asset = %asset, output = %output).entered();
        let run = std::panic::AssertUnwindSafe(|| effects.run(&triggers[*t].action));
        std::panic::catch_unwind(run)
            .unwrap_or_else(|_| Outcome { failed: Some("it panicked".into()), ..Outcome::default() })
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use super::*;
    use crate::graph::GATES;

    fn event(name: &str) -> Expr {
        Expr::Event(name.to_string())
    }

    fn gate(name: &str, inputs: Vec<Expr>) -> Expr {
        Expr::Gate(GATES.iter().find(|g| g.name == name).unwrap(), inputs)
    }

    fn pull(when: Expr, asset: &str, signals: &[&str]) -> Trigger {
        let action = Action::Pull { asset: asset.to_string(), output: "native".to_string() };
        Trigger { when, action, signals: signals.iter().map(ToString::to_string).collect() }
    }

    /// Runs what it is told, each action signaling all it may.
    #[derive(Default)]
    struct Log {
        ran: Mutex<Vec<String>>,
        failing: Vec<String>,
        emits: BTreeMap<String, Vec<String>>,
    }

    impl Effects for Log {
        fn run(&self, action: &Action) -> Outcome {
            let (name, _) = label(action);
            self.ran.lock().unwrap().push(name.clone());
            let failed = self.failing.contains(&name).then(|| "no".to_string());
            let signaled = self.emits.get(&name).cloned().unwrap_or_default();
            Outcome { signaled, failed, notes: Vec::new(), made: true }
        }

        fn is_stale(&self, _: &Action) -> bool {
            false
        }
    }

    fn run(triggers: &[Trigger], emits: &[(&str, &str)], roots: &[&str]) -> (Vec<String>, Report) {
        let mut log = Log::default();
        for (from, to) in emits {
            log.emits.entry((*from).to_string()).or_default().push((*to).to_string());
        }
        let roots: Vec<String> = roots.iter().map(ToString::to_string).collect();
        let report = schedule(triggers, &roots, &log, &AtomicBool::new(false));
        (log.ran.into_inner().unwrap(), report)
    }

    #[test]
    fn a_cycle_of_signals_ends_with_its_epoch_each_part_run_once() {
        let triggers = [pull(event("a"), "x", &["b"]), pull(event("b"), "y", &["a"])];
        let (ran, report) = run(&triggers, &[("x", "b"), ("y", "a")], &["a"]);
        assert_eq!(ran, ["x", "y"]);
        assert!(report.signaled.contains("a") && report.signaled.contains("b"));
    }

    #[test]
    fn several_pulls_of_a_node_are_one_run() {
        let triggers = [pull(Expr::Any(vec![event("a"), event("b")]), "x", &[])];
        let (ran, _) = run(&triggers, &[], &["a", "b"]);
        assert_eq!(ran, ["x"]);
    }

    #[test]
    fn a_gate_that_inverts_is_told_after_what_it_reads_has_settled() {
        // Whether `b` comes is only known once `x` has run.
        let triggers = [
            pull(gate("and", vec![event("a"), gate("not", vec![event("b")])]), "late", &[]),
            pull(event("a"), "x", &["b"]),
        ];
        let (ran, _) = run(&triggers, &[("x", "b")], &["a"]);
        assert_eq!(ran, ["x"]);
    }

    #[test]
    fn a_gate_with_nothing_against_it_fires_once_settled() {
        let triggers = [
            pull(gate("and", vec![event("a"), gate("not", vec![event("b")])]), "late", &[]),
            pull(event("a"), "x", &[]),
        ];
        let (ran, _) = run(&triggers, &[], &["a"]);
        assert_eq!(ran, ["x", "late"]);
    }

    #[test]
    fn a_gate_waiting_for_itself_is_refused_when_planned() {
        let triggers = vec![pull(gate("and", vec![event("a"), gate("not", vec![event("b")])]), "x", &["b"])];
        let (kept, problems) = order(triggers);
        assert_eq!(kept, []);
        assert_eq!(problems.len(), 1);
    }

    #[test]
    fn a_plain_cycle_is_no_problem() {
        let triggers = vec![pull(event("a"), "x", &["b"]), pull(event("b"), "y", &["a"])];
        assert_eq!(order(triggers).1, []);
    }

    #[test]
    fn a_hook_runs_before_what_its_event_pulls_and_failing_stops_them() {
        let action = Action::Hook { op: "check".into(), event: "a".into(), signals: vec![] };
        let hook = Trigger { when: event("a"), action, signals: vec![] };
        let triggers = [pull(event("a"), "x", &[]), hook];
        let (ran, _) = run(&triggers, &[], &["a"]);
        assert_eq!(ran, ["check", "x"]);
        let log = Log { failing: vec!["check".to_string()], ..Log::default() };
        let report = schedule(&triggers, &["a".to_string()], &log, &AtomicBool::new(false));
        assert_eq!(log.ran.into_inner().unwrap(), ["check"]);
        assert_eq!(report.failed, [("check".to_string(), "no".to_string())]);
    }

    #[test]
    fn a_cancelled_epoch_runs_nothing_more() {
        let triggers = [pull(event("a"), "x", &[])];
        let log = Log::default();
        let report = schedule(&triggers, &["a".to_string()], &log, &AtomicBool::new(true));
        assert!(report.cancelled && log.ran.into_inner().unwrap().is_empty());
    }
}
