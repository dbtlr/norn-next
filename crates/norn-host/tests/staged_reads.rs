//! Public first-index admission (ADR 0043), with Ready publication held back.
//!
//! This boundary follows canonical construction: it proves public admission,
//! not service before canonical work. A construction-stage seam must prove that
//! stronger property separately.
#![cfg(unix)]
#![allow(clippy::disallowed_methods)] // This test owns its fixture tree.

use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

use norn_config::ConfigDirs;
use norn_config::registry::VaultRoot;
use norn_host::{
    ActiveFingerprints, ApplyEnd, ApplyProgress, AttachmentAdvisory, EntryOps, EntryReloadFailure,
    Host, JobFailure, LifecyclePolicy, PlanGround, ProductionEntryOps, ProductionPolicy,
    ProgressReporter, ReconcileWork, RecordRefusal, Registration, RegistryRead, ReloadJudgment,
    RetireRefusal, SemanticEngines,
};
use norn_store::ContentModel;
use norn_testkit::isolation::{self, Lease};
use norn_testkit::scratch::Scratch;
use norn_testkit::wait::{Budget, Observed, wait_until};
use norn_wire::{
    Anchor, BlockRow, BodyText, Candidate, CandidateHead, Collection, DocumentPath, DocumentRow,
    FieldValue, GetParams, GetReport, HeadingRow, LinkFamily, LinkRow, PlanDocument,
    ResolutionTarget, RollingHealStatus, Span, TagRow, TagSource, TrustState, UntrustedReason,
    VaultAddress, VaultName,
};

const OUTER: Budget = Budget::new(Duration::from_secs(30), Duration::from_millis(250));
const BODY: &str = "# Topic\n[[target#Present]] #demo\nA block. ^local\n";

#[test]
fn public_get_answers_the_requested_slug_during_first_attach_before_ready() {
    let scratch = Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "public-staged-get")
        .expect("a fixture in this worktree's target directory");
    let root = scratch.join("vault");
    std::fs::create_dir_all(root.join(".norn")).unwrap();
    std::fs::write(
        root.join(".norn/schema.yaml"),
        "version: 1\nfields:\n  rating: { type: number }\n",
    )
    .unwrap();
    std::fs::write(
        root.join("requested.md"),
        format!("---\nrating: 7\n---\n{BODY}"),
    )
    .unwrap();
    std::fs::write(root.join("target.md"), "# Present\nTarget body.\n").unwrap();

    // One ordinary watcher lease covers host creation through host destruction.
    let _watcher = Lease::hold(
        isolation::REAL_WATCHER,
        isolation::acquisition_budget(OUTER),
    );
    let name = VaultName::new("notes").unwrap();
    let registration = Registration::new(name.clone(), VaultRoot::new(&root).unwrap());
    let dirs = ConfigDirs::new(scratch.join("config"), scratch.join("data")).unwrap();
    let gate = Arc::new(AttachGate::default());
    let host = Host::new(
        RegistryRead::from_entries([registration]),
        HeldAttach {
            inner: ProductionEntryOps::new(dirs, ProductionPolicy::new(32, 32).unwrap()),
            gate: Arc::clone(&gate),
        },
        LifecyclePolicy {
            idle_after: Duration::from_secs(7200),
            worker_slots: 1,
            watch_poll_interval: Duration::from_millis(50),
            read_settle_bound: norn_host::READ_SETTLE_BOUND,
        },
    )
    .unwrap();

    std::thread::scope(|scope| {
        // Closure locals unwind before scope joins its callers, and before the
        // outer host drops. A failing assertion therefore cannot strand attach.
        let _release = ReleaseAttach(&gate);
        // The actual request triggers attachment and supplies its target before
        // construction runs. Neither the ops nor an experiment preselects it.
        // A target arriving after inner attach returned would have no remaining
        // construction checkpoint and may legitimately wait for canonical Ready.
        let (sent, received) = mpsc::sync_channel(1);
        let host_ref = &host;
        let name_ref = &name;
        let caller = scope.spawn(move || {
            let params = GetParams::new(
                VaultAddress::name(name_ref.clone()),
                ResolutionTarget::new("requested").unwrap(),
            );
            sent.send(host_ref.get(&params)).unwrap();
        });
        wait_until(
            "production attach to reach the publication gate",
            OUTER,
            || {
                if gate.reached() {
                    Observed::Met(())
                } else {
                    Observed::pending("inner attach has not returned successfully")
                }
            },
        )
        .unwrap();
        let warming = host.state(&name).unwrap();
        assert!(matches!(warming, TrustState::Warming { .. }), "{warming:?}");

        let early = received
            .recv_timeout(OUTER.work())
            .expect("the public Get must finish while Ready publication is held");
        caller.join().expect("the public Get caller joined");
        assert_eq!(host.state(&name).unwrap(), warming);
        assert!(
            !gate.expired(),
            "the attach pause expired before Get finished"
        );
        gate.release();

        wait_until("canonical Ready after releasing attach", OUTER, || {
            let state = host.state(&name).unwrap();
            if state == TrustState::Ready {
                Observed::Met(())
            } else {
                Observed::pending(format!("{state:?}"))
            }
        })
        .unwrap();
        let params = GetParams::new(
            VaultAddress::name(name.clone()),
            ResolutionTarget::new("requested").unwrap(),
        );
        let canonical = host.get(&params).expect("canonical public Get after Ready");
        assert!(canonical.answer.unsatisfied.is_empty());
        assert!(canonical.answer.advisories.is_empty());
        assert_eq!(canonical.answer.reading.trust, TrustState::Ready);
        assert_eq!(canonical.answer.report, expected_report());

        // Check canonical behavior and release all holds even at this RED
        // checkpoint. The intended failure is this public admission assertion.
        let early = early
            .expect("ordinary public Host::get must answer the requested slug before global Ready");
        assert!(early.answer.unsatisfied.is_empty());
        assert!(early.answer.advisories.is_empty());
        // Progress may advance after the early snapshot was established, so
        // its truthful warming counters need not equal the later held state.
        assert!(matches!(
            early.answer.reading.trust,
            TrustState::Warming { .. }
        ));
        assert!(early.answer.reading.generation > 0);
        assert!(!early.answer.reading.epoch.is_empty());
        assert_ne!(early.answer.reading.epoch, canonical.answer.reading.epoch);
        assert_eq!(early.answer.report, expected_report());
        assert_eq!(early.answer.report, canonical.answer.report);
    });
    drop(host);
}

// Authored from the fixture bytes, including source positions after the
// 18-byte frontmatter. Empty collections are required columns, not omissions.
fn expected_report() -> GetReport {
    GetReport::record(
        DocumentRow::new(DocumentPath::new("requested.md").unwrap())
            .with_fields([("rating".to_owned(), FieldValue::scalar("7"))].into())
            .with_body(BodyText::new(BODY, 49).unwrap())
            .with_links(
                Collection::new(
                    vec![
                        LinkRow::new(
                            LinkFamily::Wikilink,
                            false,
                            None,
                            "target",
                            None,
                            Some(Anchor::heading("Present")),
                            Some(Span::new(5, 1, 26)),
                            CandidateHead::new(
                                [Candidate::new(
                                    DocumentPath::new("target.md").unwrap(),
                                    "target",
                                )],
                                1,
                            )
                            .unwrap(),
                        )
                        .unwrap(),
                    ],
                    1,
                )
                .unwrap(),
            )
            .with_headings(
                Collection::new(
                    vec![HeadingRow::new(1, "Topic", "topic", Span::new(4, 1, 18))],
                    1,
                )
                .unwrap(),
            )
            .with_blocks(
                Collection::new(vec![BlockRow::new("local", Some(Span::new(6, 10, 60)))], 1)
                    .unwrap(),
            )
            .with_tags(
                Collection::new(
                    vec![TagRow::new(
                        "demo",
                        TagSource::Body,
                        Some(Span::new(5, 20, 45)),
                    )],
                    1,
                )
                .unwrap(),
            )
            .with_findings(Collection::new(vec![], 0).unwrap()),
        [],
    )
}

#[derive(Default)]
struct AttachGate {
    state: Mutex<GateState>,
    changed: Condvar,
}

#[derive(Default)]
struct GateState {
    reached: bool,
    released: bool,
    expired: bool,
}

impl AttachGate {
    fn pause(&self) {
        let mut state = self.state.lock().unwrap();
        state.reached = true;
        self.changed.notify_all();
        // Dominates arrival, the caller's read, and the state observations
        // outside this hold. A missed release still has a finite unwind path.
        let (mut state, timed) = self
            .changed
            .wait_timeout_while(state, OUTER.dominating(4).work(), |s| !s.released)
            .unwrap();
        state.expired = timed.timed_out() && !state.released;
        assert!(
            !state.expired,
            "attach publication gate exhausted its dominating budget"
        );
    }

    fn reached(&self) -> bool {
        self.state.lock().unwrap().reached
    }

    fn expired(&self) -> bool {
        self.state.lock().unwrap().expired
    }

    fn release(&self) {
        // Release during panic too, without introducing a second panic.
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .released = true;
        self.changed.notify_all();
    }
}

struct ReleaseAttach<'a>(&'a AttachGate);

impl Drop for ReleaseAttach<'_> {
    fn drop(&mut self) {
        self.0.release();
    }
}

struct HeldAttach {
    inner: ProductionEntryOps,
    gate: Arc<AttachGate>,
}

// Every operation keeps production behavior; only successful attach's return
// is held, before the lifecycle can publish canonical Ready.
macro_rules! delegate {
    ($(fn $method:ident(&self $(, $arg:ident: $ty:ty)*) $(-> $ret:ty)?;)*) => {
        $(fn $method(&self $(, $arg: $ty)*) $(-> $ret)? {
            self.inner.$method($($arg),*)
        })*
    };
}

impl EntryOps for HeldAttach {
    type Attachment = <ProductionEntryOps as EntryOps>::Attachment;
    #[cfg(feature = "induced-failure")]
    fn loading_read_boundary(&self, completed: bool) {
        self.inner.loading_read_boundary(completed);
    }

    fn attach(
        &self,
        registration: &Registration,
        progress: &ProgressReporter<Self::Attachment>,
    ) -> Result<Self::Attachment, JobFailure> {
        let attachment = self.inner.attach(registration, progress)?;
        self.gate.pause();
        Ok(attachment)
    }

    delegate! {
        fn first_index_reads(&self) -> bool;
        fn first_index_candidate(&self, registration: &Registration) -> bool;
        fn trim_readers(&self, now: Instant);
        fn reconcile(&self, name: &VaultName, attachment: &mut Self::Attachment, work: ReconcileWork, progress: &ProgressReporter<Self::Attachment>) -> Result<(), JobFailure>;
        fn recover(&self, name: &VaultName, attachment: &mut Self::Attachment, progress: &ProgressReporter<Self::Attachment>) -> Result<(), JobFailure>;
        fn rebuild(&self, name: &VaultName, attachment: Self::Attachment, progress: &ProgressReporter<Self::Attachment>) -> Result<Self::Attachment, JobFailure>;
        fn poll(&self, name: &VaultName, attachment: &mut Self::Attachment) -> Result<Option<norn_fs::Batch>, JobFailure>;
        fn reload(&self, name: &VaultName, attachment: &mut Self::Attachment, progress: &ProgressReporter<Self::Attachment>) -> Result<ReloadJudgment, EntryReloadFailure>;
        fn judge_reload(&self, name: &VaultName, attachment: &Self::Attachment) -> Result<ReloadJudgment, EntryReloadFailure>;
        fn active_fingerprints(&self, attachment: &Self::Attachment) -> Option<ActiveFingerprints>;
        fn active_content_model(&self, attachment: &Self::Attachment) -> Arc<ContentModel>;
        fn control_root(&self, attachment: &Self::Attachment) -> Option<std::path::PathBuf>;
        fn plan_ground(&self, attachment: &Self::Attachment) -> Option<PlanGround>;
        fn apply(&self, name: &VaultName, attachment: &mut Self::Attachment, plan: PlanDocument, apply: &ApplyProgress, progress: &ProgressReporter<Self::Attachment>) -> Result<ApplyEnd, JobFailure>;
        fn withheld_trust(&self, attachment: &Self::Attachment) -> Option<UntrustedReason>;
        fn advisories(&self, attachment: &Self::Attachment) -> Vec<AttachmentAdvisory>;
        fn count_leg_mint(&self, statements: u64);
        fn semantic(&self) -> Option<&SemanticEngines>;
        fn rolling_heal(&self, attachment: &Self::Attachment) -> Option<RollingHealStatus>;
        fn maintenance_due(&self, name: &VaultName, attachment: &Self::Attachment) -> bool;
        fn maintain_quiet(&self, name: &VaultName, attachment: &mut Self::Attachment, quiet: bool, progress: &ProgressReporter<Self::Attachment>) -> Result<(), JobFailure>;
        fn maintain(&self, name: &VaultName, attachment: &mut Self::Attachment) -> Result<(), JobFailure>;
        fn detach(&self, name: &VaultName, attachment: Self::Attachment);
        fn discard(&self, name: &VaultName);
        fn record(&self, registration: &Registration) -> Result<(), RecordRefusal>;
        fn retire(&self, registration: &Registration, keep_state: bool, standing: &[Registration]) -> Result<(), RetireRefusal>;
        fn amend(&self, current: &Registration, amended: &Registration, standing: &[Registration]) -> Result<(), RetireRefusal>;
    }
}
