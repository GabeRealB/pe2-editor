//! What the editor and its tools spend their time on: spans (`tracing`), totaled here by name and shown on a timeline
//! by Tracy in builds with the `tracy` feature.
//!
//! A span's total is the time it was entered, summed over every span of its name - over threads too, so the totals of
//! work run in parallel add up to more than the time it took. A span naming what it ran in a field `of`
//! (`info_span!("op", of = %name)`) is totaled apart for each: `op pe2.images@ora`. `pe2-cli --profile` prints the
//! totals of its run; the editor's `editor_performance` gives them since it started.
//!
//! Without a viewer connected, Tracy keeps nothing (its on-demand mode); without the `tracy` feature a span costs the
//! totals' bookkeeping alone, a lock and a map entry, so spans go on work worth timing - an operation, an asset, a
//! phase - and never on what runs per pixel or per byte.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use tracing::Subscriber;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::LookupSpan;

/// What spans of one name took.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Total {
    /// The span's name, and what it ran where it says (`op pe2.images@ora`).
    pub name: String,
    /// How many closed.
    pub count: u64,
    /// The time they were entered, summed.
    pub total: Duration,
    /// The longest one entered at a stretch.
    pub longest: Duration,
}

fn totals_map() -> &'static Mutex<HashMap<String, Total>> {
    static TOTALS: OnceLock<Mutex<HashMap<String, Total>>> = OnceLock::new();
    TOTALS.get_or_init(Mutex::default)
}

/// The crates whose spans are kept: the editor's and its tools'. Libraries' own (turso's, per page and instruction:
/// millions in an extraction) would cost more than what they time.
const OURS: [&str; 9] = ["project", "cli", "pe2_cli", "editor", "mcp", "extensions", "pe2_ext", "holder", "vfs"];

/// Install the spans' subscriber for this process: the totals, and Tracy where built with it, of this workspace's
/// crates alone. A second call, or one after another subscriber took the process, does nothing.
pub fn install() {
    let ours = OURS.iter().fold(tracing_subscriber::filter::Targets::new(), |targets, krate| {
        targets.with_target(*krate, tracing::Level::TRACE)
    });
    let registry = tracing_subscriber::registry().with(ours).with(Totals);
    #[cfg(feature = "tracy")]
    let registry = registry.with(tracing_tracy::TracyLayer::default());
    let _ = tracing::subscriber::set_global_default(registry);
}

/// Tracy's frame mark, where built with it: the editor's frame ended.
pub fn frame() {
    #[cfg(feature = "tracy")]
    if let Some(client) = tracing_tracy::client::Client::running() {
        client.frame_mark();
    }
}

/// Add a duration measured otherwise than by a span: what crosses threads, as a request's wait from asked to answered.
pub fn record(name: &str, took: Duration) {
    let Ok(mut totals) = totals_map().lock() else { return };
    let total = totals.entry(name.to_string()).or_insert_with(|| Total { name: name.to_string(), ..Total::default() });
    total.count += 1;
    total.total += took;
    total.longest = total.longest.max(took);
}

/// Every name's total so far, the longest first.
#[must_use]
pub fn totals() -> Vec<Total> {
    let mut all: Vec<Total> = totals_map().lock().map_or_else(|_| Vec::new(), |m| m.values().cloned().collect());
    all.sort_by(|a, b| b.total.cmp(&a.total).then_with(|| a.name.cmp(&b.name)));
    all
}

/// The totals as a table: each name, how many, their time, the mean and the longest.
#[must_use]
pub fn table(totals: &[Total]) -> String {
    let width = totals.iter().map(|t| t.name.len()).max().unwrap_or(0).max(4);
    let mut out = format!("{:<width$}  {:>7}  {:>10}  {:>10}  {:>10}\n", "span", "count", "total", "mean", "longest");
    for t in totals {
        let mean = t.total / u32::try_from(t.count.max(1)).unwrap_or(u32::MAX);
        let _ = writeln!(
            out,
            "{:<width$}  {:>7}  {:>10}  {:>10}  {:>10}",
            t.name,
            t.count,
            shown(t.total),
            shown(mean),
            shown(t.longest)
        );
    }
    out
}

/// A duration as a person reads it: seconds, milliseconds or microseconds.
fn shown(d: Duration) -> String {
    if d >= Duration::from_secs(1) {
        format!("{:.2} s", d.as_secs_f64())
    } else if d >= Duration::from_millis(1) {
        format!("{:.1} ms", d.as_secs_f64() * 1e3)
    } else {
        format!("{} µs", d.as_micros())
    }
}

/// The layer totaling spans by name.
struct Totals;

/// A span's state in the registry: its name as totaled, and when it was entered, for each enter not exited yet.
struct Timing {
    name: String,
    entered: Vec<Instant>,
    total: Duration,
    longest: Duration,
}

/// The `of` field's value, where a span has one.
struct Of(Option<String>);

impl Visit for Of {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "of" {
            self.0 = Some(value.to_string());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "of" {
            self.0 = Some(format!("{value:?}"));
        }
    }
}

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for Totals {
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let mut of = Of(None);
        attrs.record(&mut of);
        let name = match of.0 {
            Some(of) => format!("{} {of}", attrs.metadata().name()),
            None => attrs.metadata().name().to_string(),
        };
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(Timing {
                name,
                entered: Vec::new(),
                total: Duration::ZERO,
                longest: Duration::ZERO,
            });
        }
    }

    fn on_enter(&self, id: &Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id)
            && let Some(timing) = span.extensions_mut().get_mut::<Timing>()
        {
            timing.entered.push(Instant::now());
        }
    }

    fn on_exit(&self, id: &Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id)
            && let Some(timing) = span.extensions_mut().get_mut::<Timing>()
            && let Some(at) = timing.entered.pop()
        {
            let stretch = at.elapsed();
            timing.total += stretch;
            timing.longest = timing.longest.max(stretch);
        }
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else { return };
        let Some(timing) = span.extensions_mut().remove::<Timing>() else { return };
        let Ok(mut totals) = totals_map().lock() else { return };
        let total =
            totals.entry(timing.name.clone()).or_insert_with(|| Total { name: timing.name, ..Total::default() });
        total.count += 1;
        total.total += timing.total;
        total.longest = total.longest.max(timing.longest);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_are_totaled_by_name_and_what_they_ran() {
        let subscriber = tracing_subscriber::registry().with(Totals);
        tracing::subscriber::with_default(subscriber, || {
            for op in ["a", "a", "b"] {
                let _span = tracing::info_span!("profile-test-op", of = %op).entered();
                std::thread::sleep(Duration::from_millis(2));
            }
            let _plain = tracing::info_span!("profile-test-plain").entered();
        });
        let all = totals();
        let named = |name: &str| all.iter().find(|t| t.name == name).cloned().unwrap_or_default();
        assert_eq!(named("profile-test-op a").count, 2);
        assert!(named("profile-test-op a").total >= Duration::from_millis(4));
        assert_eq!(named("profile-test-op b").count, 1);
        assert_eq!(named("profile-test-plain").count, 1);
        assert!(table(&all).contains("profile-test-op a"));
    }
}
