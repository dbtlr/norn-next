//! Bounded, process-wide timing aggregates for a manually armed harness.
//!
//! The caller supplies the label vocabulary. Spans are inclusive: nested or
//! concurrent labels overlap, so summing their durations does not give elapsed
//! wall time. Only completed spans contribute calls and time; a report names
//! unknown labels and unfinished spans as counts instead of losing that evidence.
//! The entire module is gated by `induced-failure`.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

/// The fixed upper bound on aggregate entries in one recording.
pub const MAX_LABELS: usize = 64;

static ACTIVE_GENERATION: AtomicU64 = AtomicU64::new(0);
static STATE: Mutex<State> = Mutex::new(State {
    generation: 0,
    report: None,
});

struct State {
    generation: u64,
    report: Option<Report>,
}

/// Owns the active recording. Dropping it discards the report and disarms timing.
#[derive(Debug)]
#[must_use = "keep the recording guard until the measured work has finished"]
pub struct Recording {
    finished: bool,
}

/// A recording was refused without changing the active session.
#[derive(Debug, Eq, PartialEq)]
pub enum StartError {
    AlreadyRecording,
    DuplicateLabel(&'static str),
    TooManyLabels,
    GenerationExhausted,
}

impl fmt::Display for StartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyRecording => formatter.write_str("a timing recording is already active"),
            Self::DuplicateLabel(label) => write!(formatter, "duplicate timing label: {label}"),
            Self::TooManyLabels => write!(
                formatter,
                "a timing recording admits at most {MAX_LABELS} labels"
            ),
            Self::GenerationExhausted => {
                formatter.write_str("timing recording generations are exhausted")
            }
        }
    }
}

impl std::error::Error for StartError {}

/// Completed spans for one label, including time spent inside nested spans.
#[derive(Debug)]
pub struct Entry {
    pub label: &'static str,
    pub calls: u64,
    pub elapsed_ns: u128,
}

/// One stopped session, in the label order supplied to [`start`].
#[derive(Debug)]
pub struct Report {
    /// Includes unused labels, with zero calls and time.
    pub entries: Vec<Entry>,
    /// Calls to [`span`] with a label absent from the active recording.
    pub unknown_labels: u64,
    /// Admitted spans still open at [`Recording::finish`]. Their time and calls
    /// are excluded, and their later drops cannot change another recording.
    pub unfinished_spans: u64,
}

/// Records one admitted span when dropped, including during unwinding.
#[must_use = "keep the span guard alive for the operation being timed"]
pub struct Span {
    generation: u64,
    index: usize,
    started: Instant,
}

/// Arm one process-wide recording for at most [`MAX_LABELS`] distinct labels.
/// A caller must run in an isolated process or serialize competing recordings.
pub fn start(labels: &'static [&'static str]) -> Result<Recording, StartError> {
    if labels.len() > MAX_LABELS {
        return Err(StartError::TooManyLabels);
    }
    for (index, label) in labels.iter().enumerate() {
        if labels[..index].contains(label) {
            return Err(StartError::DuplicateLabel(label));
        }
    }
    let mut state = STATE.lock().unwrap_or_else(PoisonError::into_inner);
    if state.report.is_some() {
        return Err(StartError::AlreadyRecording);
    }
    state.generation = state
        .generation
        .checked_add(1)
        .ok_or(StartError::GenerationExhausted)?;
    state.report = Some(Report {
        entries: labels
            .iter()
            .map(|&label| Entry {
                label,
                calls: 0,
                elapsed_ns: 0,
            })
            .collect(),
        unknown_labels: 0,
        unfinished_spans: 0,
    });
    ACTIVE_GENERATION.store(state.generation, Ordering::Release);
    Ok(Recording { finished: false })
}

/// Time a label in the active session, or return `None` while disarmed or for
/// an unknown label. The disarmed path reads no clock and takes no lock.
pub fn span(label: &'static str) -> Option<Span> {
    let generation = ACTIVE_GENERATION.load(Ordering::Acquire);
    if generation == 0 {
        return None;
    }
    let mut state = STATE.lock().unwrap_or_else(PoisonError::into_inner);
    // A span delayed on the lock cannot cross the end of the session it saw.
    if state.generation != generation {
        return None;
    }
    let report = state.report.as_mut()?;
    let Some(index) = report.entries.iter().position(|entry| entry.label == label) else {
        report.unknown_labels += 1;
        return None;
    };
    report.unfinished_spans += 1;
    Some(Span {
        generation,
        index,
        started: Instant::now(),
    })
}

impl Drop for Span {
    fn drop(&mut self) {
        let elapsed = self.started.elapsed().as_nanos();
        let mut state = STATE.lock().unwrap_or_else(PoisonError::into_inner);
        if state.generation != self.generation {
            return;
        }
        if let Some(report) = state.report.as_mut() {
            report.entries[self.index].calls += 1;
            report.entries[self.index].elapsed_ns += elapsed;
            report.unfinished_spans -= 1;
        }
    }
}

impl Recording {
    /// Stop admitting spans and return completed aggregates and anomaly counts.
    /// Work must have quiesced before this call for `unfinished_spans` to be zero.
    pub fn finish(mut self) -> Report {
        let mut state = STATE.lock().unwrap_or_else(PoisonError::into_inner);
        ACTIVE_GENERATION.store(0, Ordering::Release);
        self.finished = true;
        state
            .report
            .take()
            .expect("the recording guard owns the active session")
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        if !self.finished {
            let mut state = STATE.lock().unwrap_or_else(PoisonError::into_inner);
            ACTIVE_GENERATION.store(0, Ordering::Release);
            state.report = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    static TEST_SESSION: Mutex<()> = Mutex::new(());

    #[test]
    fn no_recording_means_no_span() {
        let _serial = TEST_SESSION.lock().unwrap();
        assert!(span("idle").is_none());
    }

    #[test]
    fn nested_spans_report_inclusive_time_and_aggregate_calls() {
        let _serial = TEST_SESSION.lock().unwrap();
        let recording = start(&["outer", "inner", "untouched"]).unwrap();
        {
            let _outer = span("outer").unwrap();
            drop(span("inner").unwrap());
            drop(span("inner").unwrap());
        }
        let report = recording.finish();
        assert_eq!(
            report
                .entries
                .iter()
                .map(|entry| (entry.label, entry.calls))
                .collect::<Vec<_>>(),
            [("outer", 1), ("inner", 2), ("untouched", 0)],
        );
        assert!(report.entries[0].elapsed_ns >= report.entries[1].elapsed_ns);
        assert_eq!(report.entries[2].elapsed_ns, 0);
        assert_eq!(report.unknown_labels, 0);
        assert_eq!(report.unfinished_spans, 0);
        assert!(span("outer").is_none());
    }

    #[test]
    fn errors_and_unwinds_complete_spans() {
        fn returns_error() -> Result<(), ()> {
            let _span = span("error").unwrap();
            Err(())
        }

        let _serial = TEST_SESSION.lock().unwrap();
        let recording = start(&["error", "unwind"]).unwrap();
        assert_eq!(returns_error(), Err(()));
        assert!(
            std::panic::catch_unwind(|| {
                let _span = span("unwind").unwrap();
                panic!("an operation unwound");
            })
            .is_err()
        );
        let report = recording.finish();
        assert_eq!(
            report
                .entries
                .iter()
                .map(|entry| entry.calls)
                .collect::<Vec<_>>(),
            [1, 1]
        );
        assert_eq!(report.unfinished_spans, 0);
    }

    #[test]
    fn dropping_or_unwinding_a_recording_releases_the_session() {
        let _serial = TEST_SESSION.lock().unwrap();
        drop(start(&["abandoned"]).unwrap());
        assert!(span("abandoned").is_none());
        assert!(
            std::panic::catch_unwind(|| {
                let _recording = start(&["unwind"]).unwrap();
                let _span = span("unwind").unwrap();
                panic!("the recording unwound");
            })
            .is_err()
        );
        let recording = start(&["next"]).unwrap();
        drop(span("next").unwrap());
        assert_eq!(recording.finish().entries[0].calls, 1);
    }

    #[test]
    fn duplicate_or_excess_labels_are_refused_before_recording() {
        let _serial = TEST_SESSION.lock().unwrap();
        assert_eq!(
            start(&["duplicate", "duplicate"]).unwrap_err(),
            StartError::DuplicateLabel("duplicate")
        );
        assert_eq!(
            start(&["label"; 65]).unwrap_err(),
            StartError::TooManyLabels
        );
        assert!(span("duplicate").is_none());
        let recording = start(&["valid"]).unwrap();
        assert_eq!(recording.finish().entries.len(), 1);
    }

    #[test]
    fn overlapping_recordings_are_refused_without_changing_the_active_one() {
        let _serial = TEST_SESSION.lock().unwrap();
        let recording = start(&["original"]).unwrap();
        assert_eq!(start(&["other"]).unwrap_err(), StartError::AlreadyRecording);
        drop(span("original").unwrap());
        let report = recording.finish();
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].label, "original");
        assert_eq!(report.entries[0].calls, 1);
    }

    #[test]
    fn unknown_labels_are_counted_without_growing_the_record() {
        let _serial = TEST_SESSION.lock().unwrap();
        let recording = start(&["known"]).unwrap();
        assert!(span("unknown").is_none());
        assert!(span("another").is_none());
        drop(span("known").unwrap());
        let report = recording.finish();
        assert_eq!(report.unknown_labels, 2);
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].calls, 1);
    }

    #[test]
    fn late_spans_are_reported_and_cannot_enter_a_later_session() {
        let _serial = TEST_SESSION.lock().unwrap();
        let recording = start(&["old"]).unwrap();
        let late = span("old").unwrap();
        let report = recording.finish();
        assert_eq!(report.unfinished_spans, 1);
        assert_eq!(report.entries[0].calls, 0);
        assert_eq!(report.entries[0].elapsed_ns, 0);

        let next = start(&["new"]).unwrap();
        drop(late);
        drop(span("new").unwrap());
        let report = next.finish();
        assert_eq!(report.entries[0].label, "new");
        assert_eq!(report.entries[0].calls, 1);
        assert_eq!(report.unknown_labels, 0);
        assert_eq!(report.unfinished_spans, 0);
    }

    #[test]
    fn an_abandoned_recordings_late_span_cannot_enter_a_later_session() {
        let _serial = TEST_SESSION.lock().unwrap();
        let recording = start(&["old"]).unwrap();
        let late = span("old").unwrap();
        drop(recording);
        let next = start(&[]).unwrap();
        drop(late);
        let report = next.finish();
        assert!(report.entries.is_empty());
        assert_eq!(report.unknown_labels, 0);
        assert_eq!(report.unfinished_spans, 0);
    }

    #[test]
    fn all_sixty_four_labels_fit_and_remain_distinct() {
        static LABELS: [&str; 64] = [
            "00", "01", "02", "03", "04", "05", "06", "07", "08", "09", "10", "11", "12", "13",
            "14", "15", "16", "17", "18", "19", "20", "21", "22", "23", "24", "25", "26", "27",
            "28", "29", "30", "31", "32", "33", "34", "35", "36", "37", "38", "39", "40", "41",
            "42", "43", "44", "45", "46", "47", "48", "49", "50", "51", "52", "53", "54", "55",
            "56", "57", "58", "59", "60", "61", "62", "63",
        ];
        let _serial = TEST_SESSION.lock().unwrap();
        let recording = start(&LABELS).unwrap();
        for label in LABELS {
            drop(span(label).unwrap());
        }
        let report = recording.finish();
        assert_eq!(report.entries.len(), 64);
        assert!(report.entries.iter().all(|entry| entry.calls == 1));
        assert_eq!(report.unknown_labels, 0);
        assert_eq!(report.unfinished_spans, 0);
    }

    #[test]
    fn concurrent_threads_contribute_to_the_same_recording() {
        let _serial = TEST_SESSION.lock().unwrap();
        let recording = start(&["worker"]).unwrap();
        std::thread::scope(|threads| {
            for _ in 0..4 {
                threads.spawn(|| {
                    for _ in 0..8 {
                        drop(span("worker").unwrap());
                    }
                });
            }
        });
        let report = recording.finish();
        assert_eq!(report.entries[0].calls, 32);
        assert_eq!(report.unknown_labels, 0);
        assert_eq!(report.unfinished_spans, 0);
    }
}
