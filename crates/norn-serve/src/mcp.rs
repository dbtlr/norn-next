//! What `/mcp` adds around the wire route's dispatch path for one
//! `tools/call`: the reads made before dispatch from the call's raw
//! arguments, and the per-request wrapper the call's dispatches pass through.
//!
//! **Translation only.** Every MCP decision is `norn-mcp`'s: the wrapper
//! splices the tool's checked mode through [`ModeSplice`] and composes the
//! outcome through the call's [`Reply`]. It returns a [`Dispatched`], so the
//! readiness retry and the stop trigger see what they see on the wire route:
//! a `Warming` refusal, a stopped worker and a body that did not parse pass
//! through uncomposed.

use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use norn_mcp::{ModeSplice, Reply};
use norn_wire::{ApplyMode, Verb};

use crate::request::{AtBound, Composed, Dispatch, Dispatched, Lane, is_warming};
use crate::wire;

/// `/mcp`'s cumulative counts, read by the counters route.
#[derive(Debug, Default)]
pub(crate) struct McpCounters {
    /// Tool-list derivations this process made: [`prepare_tools`] counts
    /// each call it makes to `norn_mcp::ToolList::derive`, so the served
    /// count reads 1 for the life of the process exactly when start derived
    /// once.
    pub(crate) tool_list_derivations: AtomicU64,
    /// Messages handed to `norn-mcp`: every `/mcp` POST whose body arrived.
    pub(crate) messages: AtomicU64,
    /// Of those, the `tools/call`s that named a served tool with object
    /// arguments, and so went on toward dispatch.
    pub(crate) calls: AtomicU64,
    /// Nanoseconds the wrappers spent splicing and composing, across every
    /// call: the clock `mcp.calls` pairs with.
    #[cfg(feature = "counters")]
    pub(crate) translate_ns: AtomicU64,
}

/// Derive the tool list for `served` and prepare its bodies under `build`,
/// counting the derivation in `counters`.
pub(crate) fn prepare_tools(
    counters: &McpCounters,
    served: impl IntoIterator<Item = Verb>,
    build: &str,
) -> Result<norn_mcp::PreparedToolLists, norn_mcp::SchemaError> {
    counters
        .tool_list_derivations
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Ok(norn_mcp::ToolList::derive(served)?.prepare(build))
}

/// The lane and drain-bound class of a call, read off its raw arguments as
/// the wire route reads them off a body, with the tool's mode standing in for
/// the `mode` the arguments do not carry. A missing or malformed `vault` is
/// the reason it cannot be read.
pub(crate) fn pre_dispatch(
    verb: Verb,
    runs: wire::Runs,
    mode: Option<ApplyMode>,
    arguments: &[u8],
) -> Result<(Lane, AtBound), String> {
    Ok((
        runs.request_lane(verb, arguments)?,
        wire::at_bound_as(verb, arguments, mode),
    ))
}

/// One call's dispatcher: the route table, wrapped by the call's mode and
/// reply.
pub(crate) struct McpCall {
    inner: Arc<dyn Dispatch>,
    splice: ModeSplice,
    reply: Reply,
    /// Nanoseconds this call's mode check, splices and compositions took, across its
    /// attempts.
    #[cfg(feature = "counters")]
    translate_ns: AtomicU64,
}

impl McpCall {
    pub(crate) fn new(inner: Arc<dyn Dispatch>, splice: ModeSplice, reply: Reply) -> Self {
        McpCall {
            inner,
            splice,
            reply,
            #[cfg(feature = "counters")]
            translate_ns: AtomicU64::new(0),
        }
    }

    /// The nanoseconds this call spent in translation so far.
    #[cfg(feature = "counters")]
    pub(crate) fn translate_ns(&self) -> u64 {
        self.translate_ns.load(std::sync::atomic::Ordering::Relaxed)
    }

    #[cfg(feature = "counters")]
    fn translated(&self, since: std::time::Instant) {
        self.add_translation(since.elapsed());
    }

    /// Count translation time spent before this call's wrapper existed.
    #[cfg(feature = "counters")]
    pub(crate) fn add_translation(&self, spent: std::time::Duration) {
        self.translate_ns.fetch_add(
            u64::try_from(spent.as_nanos()).unwrap_or(u64::MAX),
            std::sync::atomic::Ordering::Relaxed,
        );
    }
}

impl Dispatch for McpCall {
    fn dispatch(&self, verb: Verb, arguments: &[u8]) -> Dispatched {
        #[cfg(feature = "counters")]
        let started = std::time::Instant::now();
        let body = self.splice.apply(arguments);
        #[cfg(feature = "counters")]
        self.translated(started);
        let answer = self.inner.dispatch(verb, &body);
        drop(body);
        #[cfg(feature = "counters")]
        let started = std::time::Instant::now();
        let composed = match answer {
            Dispatched::Report(report) => Dispatched::Composed(Composed {
                answer: self.reply.report(&report),
                report_bytes: Some(report.len()),
            }),
            Dispatched::Refused(envelope) if !is_warming(&envelope) => {
                Dispatched::Composed(Composed {
                    answer: self.reply.failure(&envelope),
                    report_bytes: None,
                })
            }
            // A warming refusal goes back to the readiness retry, a stopped
            // worker to the stop trigger, and a body that did not parse to the
            // route's own message, each as on the wire route.
            passed => passed,
        };
        #[cfg(feature = "counters")]
        self.translated(started);
        composed
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use super::*;

    /// The derivation count is counted, not stated: each derivation moves it.
    #[test]
    fn each_derivation_is_counted() {
        let counters = McpCounters::default();
        assert_eq!(counters.tool_list_derivations.load(Ordering::SeqCst), 0);
        prepare_tools(&counters, [Verb::Find], "0.0.0+test").unwrap();
        assert_eq!(counters.tool_list_derivations.load(Ordering::SeqCst), 1);
        prepare_tools(&counters, [Verb::Find], "0.0.0+test").unwrap();
        assert_eq!(counters.tool_list_derivations.load(Ordering::SeqCst), 2);
    }
}
