//! Canonical early admission while a from-zero build fills the store it serves.
//!
//! A from-zero build begins a [`Construction`] under its claim before it walks
//! the vault, and publishes each step it ends into it. A request whose shape a
//! published step admits ([`norn_store::early`]) establishes an early hold on the
//! store under construction; one whose step is not yet published waits under
//! its original deadline; every other request refuses at once. Publication never
//! publishes `Ready`, and `Ready` closes the construction in the hold that
//! publishes it.
//!
//! A waiting Get also queues its target ([`Requests`]); the build pulls the
//! target's candidates and document ahead at its next chunk boundary and
//! publishes the target ready, admitting that Get before vault-wide Structure.
use super::*;
use norn_fs::watch::BuildAuthority;
use norn_store::Step;
use norn_store::early::Boundary;
use norn_wire::{Column, FindParams, GetParams, SearchParams, ValidateParams};

/// What a read asks of its entry: the shape early admission classifies, or
/// a canonical read only `Ready` serves.
#[derive(Clone, Copy)]
pub(crate) enum Request<'a> {
    Canonical,
    Get(&'a GetParams),
    Find(&'a FindParams),
    Validate(&'a ValidateParams),
    Search(&'a SearchParams),
}

impl Request<'_> {
    /// The construction boundary whose publication admits this request early,
    /// or `None` where only `Ready` serves it.
    pub(super) fn boundary(self) -> Option<Boundary> {
        let boundary = match self {
            Self::Canonical => return None,
            Self::Get(params) => norn_store::early::get(params),
            Self::Find(params) => norn_store::early::find(params),
            Self::Validate(params) => norn_store::early::validate(params),
            Self::Search(params) => norn_store::early::search(params),
        };
        (boundary != Boundary::Ready).then_some(boundary)
    }
}

impl<'a> Request<'a> {
    /// The target a build may publish ready for this request ahead of
    /// vault-wide Structure: a Get admitted at Structure that names no
    /// undeclared field, whose absence only the complete key universe
    /// settles. Every other request waits for its vault-wide step.
    fn pulled_ahead_target(self, model: &ContentModel) -> Option<&'a str> {
        let Self::Get(params) = self else {
            return None;
        };
        let undeclared = params
            .columns
            .iter()
            .any(|column| matches!(column, Column::Field { key, .. } if !model.is_declared(key)));
        (norn_store::early::get(params) == Boundary::Structure && !undeclared)
            .then(|| params.target.address())
    }
}

/// Whether a build whose last published step is `published` has completed
/// every row `boundary` reads.
fn reaches(published: Option<Step>, boundary: Boundary) -> bool {
    fn rank(step: Step) -> u8 {
        match step {
            Step::Names => 0,
            Step::Content => 1,
            Step::Structure => 2,
            Step::Relations => 3,
            Step::FullText => 4,
            Step::FullTextComplete => 5,
        }
    }
    let needed = match boundary {
        Boundary::Content => Step::Content,
        Boundary::Structure => Step::Structure,
        Boundary::Relations => Step::Relations,
        Boundary::FullTextComplete => Step::FullTextComplete,
        Boundary::Ready => return false,
    };
    published.is_some_and(|published| rank(published) >= rank(needed))
}

/// One construction's identity within its entry. A later build never matches
/// an earlier build's token, so a stale build publishes nothing over its successor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BuildToken(u64);

/// What a from-zero build hands its entry as it begins: the canonical store's
/// zero-I/O reader pool, its pinned content model, the watcher authority
/// captured for this build alone, and the store's epoch.
pub(crate) struct Begun<R> {
    pub(crate) pool: R,
    pub(crate) evidence: Arc<crate::evidence::JobEvidence>,
    pub(crate) model: Arc<ContentModel>,
    pub(crate) authority: BuildAuthority,
    pub(crate) store_epoch: String,
}

/// The from-zero build an entry's early reads are admitted against.
pub(super) enum Construction<R: ReadSource> {
    /// A first attach of a vault with no store at registration, before its
    /// build begins: a read with an early capability waits for that build.
    /// The end of the claim closes it where no build began.
    Provisional { claim: u64 },
    /// A build under way, with the steps it has published.
    Building(Building<R>),
}

impl<R: ReadSource> Construction<R> {
    fn claim(&self) -> u64 {
        match self {
            Self::Provisional { claim } => *claim,
            Self::Building(building) => building.claim,
        }
    }
}

/// A from-zero build's publications, bound to the claim, store, model and
/// watcher authority it began under. A fully published build rebinds its claim
/// to the pre-Ready reconcile without replacing its acceptance history.
pub(super) struct Building<R: ReadSource> {
    claim: u64,
    evidence: Arc<crate::evidence::JobEvidence>,
    token: BuildToken,
    store_epoch: String,
    pool: Arc<R>,
    model: Arc<ContentModel>,
    authority: BuildAuthority,
    /// The last step this build ended and published; `None` before step 1 ends.
    published: Option<Step>,
    requests: Requests,
}

/// The most distinct live targets a build holds, pending and ready together,
/// whatever the vault's size. A request past it waits for vault-wide Structure.
pub(super) const MAX_PENDING: usize = 256;

/// The Get targets a build pulls ahead of its queue and the ones it has
/// published ready, keyed by target address. Each slot lives only while a
/// waiting read's lease holds its interest: an expired interest releases its
/// slot, ready or not, so no vault-sized map of ready targets accrues.
#[derive(Default)]
pub(super) struct Requests {
    slots: Vec<Slot>,
}

struct Slot {
    target: String,
    interest: Weak<()>,
    ready: bool,
}

/// What a read's interest in a target found in its build's queue.
pub(super) enum Joined {
    /// A new slot: the first live interest in this target.
    New(Arc<()>),
    /// The slot a live interest in the same target already holds.
    Shared(Arc<()>),
    /// The queue holds [`MAX_PENDING`] live targets.
    PastBound,
}

impl Requests {
    fn release_expired(&mut self) {
        self.slots.retain(|slot| slot.interest.strong_count() > 0);
    }

    fn ready(&self, target: &str) -> bool {
        self.ready_interest(target).is_some()
    }

    /// A further hold on `target`'s ready slot, while a live interest holds it.
    /// It adds no slot, so the bound never turns it away.
    fn ready_interest(&self, target: &str) -> Option<Arc<()>> {
        self.slots
            .iter()
            .filter(|slot| slot.ready && slot.target == target)
            .find_map(|slot| slot.interest.upgrade())
    }

    fn join(&mut self, target: &str) -> Joined {
        self.release_expired();
        if let Some(interest) = self
            .slots
            .iter()
            .find(|slot| slot.target == target)
            .and_then(|slot| slot.interest.upgrade())
        {
            return Joined::Shared(interest);
        }
        if self.slots.len() >= MAX_PENDING {
            return Joined::PastBound;
        }
        let interest = Arc::new(());
        self.slots.push(Slot {
            target: target.to_owned(),
            interest: Arc::downgrade(&interest),
            ready: false,
        });
        Joined::New(interest)
    }

    fn pending(&mut self) -> Vec<String> {
        self.release_expired();
        self.slots
            .iter()
            .filter(|slot| !slot.ready)
            .map(|slot| slot.target.clone())
            .collect()
    }

    /// Mark each of `targets` a live interest still holds ready; whether any moved.
    fn publish(&mut self, targets: &[String]) -> bool {
        self.release_expired();
        let mut moved = false;
        for slot in &mut self.slots {
            if !slot.ready && targets.contains(&slot.target) {
                slot.ready = true;
                moved = true;
            }
        }
        moved
    }
}

/// What a request meets in its entry's construction.
pub(super) enum Early<R: ReadSource> {
    /// A published step admits the request.
    Admitted(Admitted<R>),
    /// The request's step is not yet published, or no build has begun under a
    /// provisional first attach: the request waits under its deadline.
    Waits,
    /// A newer concerning fact withdrew this meeting: refuse as still indexing.
    Withdrawn,
    /// No capability stands for this request: refuse at once.
    Refused,
}

/// A request fixes its name class and fact requirement at its first building hold.
/// A replacement construction takes a new meeting against its own pinned model.
pub(super) struct Meeting {
    token: BuildToken,
    sequence: u64,
    touch: Option<Arc<norn_store::early::Touch>>,
    held_by_fact: bool,
}

/// One request's admission against a build's publications and accepted-fact sequence.
/// The compiled target and vault-wide flag bound its final exposure check.
pub(crate) struct Admitted<R> {
    pub(super) pool: Arc<R>,
    pub(super) model: Arc<ContentModel>,
    authority: BuildAuthority,
    evidence: Arc<crate::evidence::JobEvidence>,
    admitted_at: u64,
    met_at: u64,
    touch: Option<Arc<norn_store::early::Touch>>,
    vault_wide: bool,
    token: BuildToken,
    published: Step,
    /// The target the build published ready for this request ahead of its
    /// vault-wide step, where that is what admitted it.
    target: Option<String>,
    /// This request's own hold on that target's slot, taken under the hold
    /// that admitted it, for the read's lease to keep: the read that queued
    /// the target may end first.
    interest: Option<Arc<()>>,
    /// Relations were unfinished or any fact was outstanding at admission.
    pub(super) relations_loading: bool,
}

impl<R> Clone for Admitted<R> {
    fn clone(&self) -> Self {
        Self {
            pool: Arc::clone(&self.pool),
            model: Arc::clone(&self.model),
            authority: self.authority.clone(),
            evidence: self.evidence.clone(),
            admitted_at: self.admitted_at,
            met_at: self.met_at,
            touch: self.touch.clone(),
            vault_wide: self.vault_wide,
            token: self.token,
            published: self.published,
            target: self.target.clone(),
            interest: self.interest.clone(),
            relations_loading: self.relations_loading,
        }
    }
}

impl<R> Admitted<R> {
    /// Newer facts refuse rather than extend the meeting's wait. Reports after
    /// admission also refuse after derivation, since the held snapshot predates them.
    fn facts_current(&self) -> bool {
        self.authority.visit_roots(|_, mut roots| {
            !Iterator::any(&mut roots, |(root, reported, underived, beneath)| {
                (reported > self.admitted_at || (reported > self.met_at && underived.is_some()))
                    && (self.vault_wide
                        || self.touch.as_ref().is_none_or(|touch| {
                            root.as_path()
                                .to_str()
                                .is_none_or(|root| touch.touches(root, beneath))
                        }))
            })
        })
    }

    /// The last step the build had published when it admitted this request.
    pub(super) fn published(&self) -> Step {
        self.published
    }

    /// Take this admission's hold on its pulled-ahead target, for the read's
    /// lease to keep through its answer.
    pub(super) fn take_interest(&mut self) -> Option<Arc<()>> {
        self.interest.take()
    }

    /// Check the build's root outside every lock; a loss withdraws the build.
    pub(super) fn authority_root_current(&self) -> bool {
        self.authority.check_root()
    }

    fn serves(&self, request: Request<'_>) -> bool {
        request
            .boundary()
            .is_some_and(|boundary| reaches(Some(self.published), boundary))
            || self
                .target
                .as_deref()
                .is_some_and(|target| request.pulled_ahead_target(&self.model) == Some(target))
    }
}

impl<A: SnapshotSource> ProgressReporter<A> {
    /// Begin this claim's from-zero construction, replacing any earlier one.
    ///
    /// An earlier construction's pool is retired and its waiters woken; no
    /// authority is reused. `None` closes construction: the build could not
    /// capture watcher authority, so its reads keep the ordinary refusal.
    pub(crate) fn begin_construction(&self, begun: Option<Begun<A::Reader>>) -> Option<BuildToken> {
        let entry = self.entry.upgrade()?;
        let waker = entry.gate.stance_waker();
        let mut state = entry.gate.lock().expect("entry gate poisoned");
        if !state.claim.stands_at(self.epoch) {
            return None;
        }
        state.close_construction();
        let begun = begun?;
        let pool = Arc::new(begun.pool);
        if !state.may_publish_construction() || !begun.authority.admits() {
            pool.retire();
            state.retired_readers.push(pool);
            return None;
        }
        begun.authority.register_waker(&waker);
        state.construction_revision += 1;
        let token = BuildToken(state.construction_revision);
        state.construction = Some(Construction::Building(Building {
            evidence: begun.evidence,
            claim: self.epoch,
            token,
            store_epoch: begun.store_epoch,
            pool,
            model: begun.model,
            authority: begun.authority,
            published: None,
            requests: Requests::default(),
        }));
        state.publish_warming_over_a_damage_rebuild();
        Some(token)
    }

    /// Publish that `token`'s build ended `step` over the store `store_epoch`
    /// names. A changed claim, construction or store, withdrawn authority,
    /// park, out-of-service entry, release or known damage publishes nothing.
    /// It never publishes `Ready` or trust unbroken since `Ready`.
    pub(crate) fn publish_step(&self, token: BuildToken, store_epoch: &str, step: Step) -> bool {
        self.with_claimed_state(|state| {
            if !state.may_publish_construction() {
                return false;
            }
            let Some(Construction::Building(building)) = state.construction.as_mut() else {
                return false;
            };
            if building.claim != self.epoch
                || building.token != token
                || building.store_epoch != store_epoch
                || !building.authority.admits()
            {
                return false;
            }
            building.published = Some(step);
            state.construction_revision += 1;
            true
        })
        .unwrap_or(false)
    }

    /// The targets live reads wait on that `token`'s build has not yet
    /// published ready, for its next drain. Zero I/O under the gate.
    pub(crate) fn pending_targets(&self, token: BuildToken) -> Vec<String> {
        self.with_claimed_state(|state| match state.construction.as_mut() {
            Some(Construction::Building(building)) if building.token == token => {
                building.requests.pending()
            }
            _ => Vec::new(),
        })
        .unwrap_or_default()
    }

    /// Publish that `token`'s build committed every candidate of each of
    /// `targets` past step 2 and its resolved document past step 3, over the
    /// store `store_epoch` names. The guards are [`Self::publish_step`]'s; a
    /// target no live interest holds any longer publishes nothing.
    pub(crate) fn publish_targets(
        &self,
        token: BuildToken,
        store_epoch: &str,
        targets: &[String],
    ) -> bool {
        self.with_claimed_state(|state| {
            if !state.may_publish_construction() {
                return false;
            }
            let Some(Construction::Building(building)) = state.construction.as_mut() else {
                return false;
            };
            if building.claim != self.epoch
                || building.token != token
                || building.store_epoch != store_epoch
                || building.published.is_none()
                || !building.authority.admits()
                || !building.requests.publish(targets)
            {
                return false;
            }
            state.construction_revision += 1;
            true
        })
        .unwrap_or(false)
    }

    /// Derivation publication shares the entry gate with admission. Watcher
    /// callbacks never take this gate; the proof lock is strictly below it.
    pub(crate) fn publish_requeued(
        &self,
        token: BuildToken,
        epoch: &str,
        roots: &std::collections::BTreeSet<norn_fs::NormalizedPath>,
        through: u64,
    ) {
        self.with_claimed_state(|state| {
            if !state.may_publish_construction() {
                return;
            }
            let Some(Construction::Building(building)) = state.construction.as_mut() else {
                return;
            };
            if building.claim != self.epoch
                || building.token != token
                || building.store_epoch != epoch
                || !building.authority.admits()
            {
                return;
            }
            let covered = building.authority.mark_derived(roots, through);
            for slot in &mut building.requests.slots {
                let touch =
                    norn_store::early::Touch::of(&slot.target, building.model.ambiguity_ignore());
                if covered.iter().any(|(root, beneath)| {
                    root.as_path()
                        .to_str()
                        .is_none_or(|root| touch.touches(root, *beneath))
                }) {
                    slot.ready = false;
                }
            }
            state.construction_revision += 1;
        });
    }

    /// Close `token`'s construction: its build failed, and nothing it wrote is
    /// admitted again. A successor's construction stands.
    pub(crate) fn close_construction(&self, token: BuildToken) {
        self.with_claimed_state(|state| {
            if matches!(
                &state.construction,
                Some(Construction::Building(building)) if building.token == token
            ) {
                state.close_construction();
            }
        });
    }
}

impl<A: SnapshotSource> EntryState<A> {
    /// Whether a build may begin or publish: no park, out-of-service entry,
    /// release or known damage stands. The claim is the caller's check, and
    /// an owed rung is no guard: rung 3 publishes as every from-zero build does.
    fn may_publish_construction(&self) -> bool {
        !self.stands_parked()
            && self.out_of_service().is_none()
            && !self.detach_in_flight
            && self.damage_met_under_a_claim.is_none()
    }

    /// Publish warming over the damage verdict of a rebuild whose construction
    /// has just begun, so its reads follow first-build admission (ADR 0045).
    ///
    /// The rebuild is still owed until it lands, so ordinary reads and applies
    /// keep refusing or waiting; only early admission changes. From here the
    /// entry's status no longer names the damage.
    fn publish_warming_over_a_damage_rebuild(&mut self) {
        if self.rebuild_required
            && matches!(
                self.trust,
                TrustState::Untrusted {
                    reason: UntrustedReason::StoreDamagedRebuilding { .. },
                    ..
                }
            )
        {
            self.trust = TrustState::warming(WarmingPhase::Healing, 0, None);
        }
    }

    /// Carry fully published construction authority into its pre-Ready reconcile.
    pub(super) fn hand_on_reconcile(&mut self, name: &VaultName) -> Job {
        let previous = self.claim.epoch();
        let next = self
            .claim
            .hand_on(|epoch| Job::Reconcile(name.clone(), epoch));
        if let Some(Construction::Building(building)) = &mut self.construction
            && building.claim == previous
            && building.published == Some(Step::FullTextComplete)
        {
            building.claim = self.claim.epoch();
            building.evidence.count_construction_rebind();
        }
        next
    }

    /// Close construction, retiring its pool outside the gate and waking every waiter.
    pub(super) fn close_construction(&mut self) {
        if let Some(construction) = self.construction.take() {
            if let Construction::Building(building) = construction {
                building.authority.retire();
                building.pool.retire();
                // The deferred pool reference prevents any last connection close here.
                self.retired_readers.push(building.pool);
            }
            self.construction_revision += 1;
        }
    }

    /// Close a construction this hold leaves without a standing claim, a
    /// served `Ready`, or an entry that may publish one.
    pub(super) fn close_stale_construction(&mut self) {
        if self
            .construction
            .as_ref()
            .is_some_and(|construction| !self.claim.stands_at(construction.claim()))
            || self.trust == TrustState::Ready
            || self.stands_parked()
            || self.out_of_service().is_some()
            || self.detach_in_flight
        {
            self.close_construction();
        }
    }

    /// Close the construction a job leg at `epoch` began, as that leg ends.
    pub(super) fn close_construction_of(&mut self, epoch: u64) {
        if self
            .construction
            .as_ref()
            .is_some_and(|construction| construction.claim() == epoch)
        {
            self.close_construction();
        }
    }

    /// Whether a read that ran on `ran_on` ran on this entry's construction pool.
    pub(super) fn reads_construction(&self, ran_on: &Weak<A::Reader>) -> bool {
        matches!(
            &self.construction,
            Some(Construction::Building(building))
                if std::ptr::eq(Arc::as_ptr(&building.pool), ran_on.as_ptr())
        )
    }

    /// What `request` meets in this entry's construction under this hold.
    ///
    /// Once step 1 has ended, a Get whose target the build has published
    /// ready is admitted before vault-wide Structure; one whose target is not
    /// yet ready waits, and [`Self::join_requests`] queues its interest.
    pub(super) fn early(
        &self,
        request: Request<'_>,
        met: &mut Option<Meeting>,
    ) -> Early<A::Reader> {
        let Some(boundary) = request.boundary() else {
            return Early::Refused;
        };
        // No answer carries an untrusted reading, so an untrusted entry refuses
        // at once. A rebuild under a damage verdict stops being one when its
        // construction begins, which publishes warming over the verdict.
        if matches!(self.trust, TrustState::Untrusted { .. }) {
            return Early::Refused;
        }
        match &self.construction {
            Some(Construction::Provisional { claim }) if self.claim.stands_at(*claim) => {
                Early::Waits
            }
            Some(Construction::Building(building))
                if self.claim.stands_at(building.claim) && building.authority.admits() =>
            {
                if met
                    .as_ref()
                    .is_some_and(|meeting| meeting.token != building.token)
                {
                    *met = None;
                }
                let meeting = met.get_or_insert_with(|| Meeting {
                    token: building.token,
                    sequence: building.authority.sequence(),
                    touch: request.pulled_ahead_target(&building.model).map(|target| {
                        Arc::new(norn_store::early::Touch::of(
                            target,
                            building.model.ambiguity_ignore(),
                        ))
                    }),
                    held_by_fact: false,
                });
                let touch = &meeting.touch;
                let (admitted_at, held, outstanding) =
                    building.authority.visit_roots(|sequence, roots| {
                        let met = meeting.sequence;
                        let mut held = false;
                        let mut outstanding = false;
                        for (root, _, underived, beneath) in roots {
                            outstanding |= underived.is_some();
                            if underived.is_some_and(|from| from <= met) {
                                held |= touch.as_ref().is_none_or(|touch| {
                                    root.as_path()
                                        .to_str()
                                        .is_none_or(|root| touch.touches(root, beneath))
                                });
                            }
                        }
                        (sequence, held, outstanding)
                    });
                if held {
                    if !meeting.held_by_fact {
                        building.evidence.count_fact_hold();
                        meeting.held_by_fact = true;
                    }
                    return Early::Waits;
                }
                let Some(published) = building.published else {
                    return Early::Waits;
                };
                let (target, interest) = if reaches(Some(published), boundary) {
                    (None, None)
                } else {
                    let Some(target) = request.pulled_ahead_target(&building.model) else {
                        return Early::Waits;
                    };
                    let Some(interest) = building.requests.ready_interest(target) else {
                        return Early::Waits;
                    };
                    (Some(target.to_owned()), Some(interest))
                };
                let relations_loading =
                    !reaches(Some(published), Boundary::Relations) || outstanding;
                let admitted = Admitted {
                    evidence: building.evidence.clone(),
                    admitted_at,
                    met_at: meeting.sequence,
                    vault_wide: touch.is_none() || !relations_loading,
                    touch: touch.clone(),
                    pool: Arc::clone(&building.pool),
                    model: Arc::clone(&building.model),
                    authority: building.authority.clone(),
                    token: building.token,
                    published,
                    target,
                    interest,
                    relations_loading,
                };
                if !admitted.facts_current() {
                    admitted.evidence.count_fact_refusal();
                    return Early::Withdrawn;
                }
                Early::Admitted(admitted)
            }
            _ => Early::Refused,
        }
    }

    /// Queue a waiting Get's interest in its target with this entry's build,
    /// for the build to pull ahead at its next drain; `None` for a request no
    /// drain serves, or with no build begun.
    pub(super) fn join_requests(&mut self, request: Request<'_>) -> Option<Joined> {
        let Some(Construction::Building(building)) = self.construction.as_mut() else {
            return None;
        };
        let target = request.pulled_ahead_target(&building.model)?;
        Some(building.requests.join(target))
    }

    /// Whether `admitted` still stands for `request` under this hold: the same
    /// build, its authority admitting, and its claim standing.
    pub(super) fn still_admits(
        &self,
        admitted: &Admitted<A::Reader>,
        request: Request<'_>,
    ) -> bool {
        matches!(
            &self.construction,
            Some(Construction::Building(building))
                if building.token == admitted.token
                    && self.claim.stands_at(building.claim)
                    && building.authority.admits()
                    && admitted
                        .target
                        .as_deref()
                        .is_none_or(|target| building.requests.ready(target))
        ) && admitted.facts_current()
            && admitted.serves(request)
    }

    /// Whether this entry holds a construction a waiting early request could meet.
    pub(super) fn constructing(&self) -> bool {
        self.construction.is_some()
    }
}

/// Owns a possibly early hold. It exposes operation execution, reading and counters only.
pub(crate) struct QueryHold<O: EntryOps> {
    hold: ReadHold<O>,
    early: Option<Admitted<<O::Attachment as SnapshotSource>::Reader>>,
}

impl<O: EntryOps> QueryHold<O> {
    pub(super) fn new(
        hold: ReadHold<O>,
        early: Option<Admitted<<O::Attachment as SnapshotSource>::Reader>>,
    ) -> Self {
        Self { hold, early }
    }

    pub(super) fn into_canonical(self) -> ReadHold<O> {
        assert!(
            self.early.is_none(),
            "an early hold cannot become an unrestricted hold"
        );
        self.hold
    }
}

impl<O: EntryOps> QueryHold<O>
where
    <O::Attachment as SnapshotSource>::Reader: ReadSource<Snapshot = norn_store::Snapshot>,
{
    #[cfg(feature = "induced-failure")]
    pub(crate) fn query_boundary(&self, host: &Host<O>, completed: bool) {
        if self.early.is_some() {
            host.shared.ops.loading_read_boundary(completed);
        }
    }

    pub(crate) fn reading(&self) -> &HoldReading {
        self.hold.reading()
    }
    pub(crate) fn counters(&self) -> norn_store::SnapshotCounters {
        self.hold.snapshot().counters()
    }

    pub(crate) fn get(
        &self,
        params: &GetParams,
    ) -> Result<norn_store::Gotten, norn_store::PageRefusal> {
        assert!(
            self.early
                .as_ref()
                .is_none_or(|early| early.serves(Request::Get(params)))
        );
        self.hold
            .snapshot()
            .get(params, self.hold.content_model(), &crate::text::TextLayer)
    }

    pub(crate) fn find(
        &self,
        params: &FindParams,
    ) -> Result<norn_store::Found, norn_store::PageRefusal> {
        assert!(
            self.early
                .as_ref()
                .is_none_or(|early| early.serves(Request::Find(params)))
        );
        self.hold.snapshot().find(params, self.hold.content_model())
    }

    pub(crate) fn validate(
        &self,
        params: &ValidateParams,
    ) -> Result<norn_store::Validated, norn_store::PageRefusal> {
        assert!(
            self.early
                .as_ref()
                .is_none_or(|early| early.serves(Request::Validate(params)))
        );
        self.hold
            .snapshot()
            .validate(params, self.hold.content_model())
    }

    /// Run a search's builder for `params` alone on this hold's snapshot and
    /// pinned model, told whether the hold is early. An early hold reads only
    /// the store: semantic engines start after `Ready`, so its builder
    /// samples no vector rung.
    pub(crate) fn search<T>(
        &self,
        params: &SearchParams,
        build: impl FnOnce(&norn_store::Snapshot, &ContentModel, bool) -> T,
    ) -> T {
        assert!(
            self.early
                .as_ref()
                .is_none_or(|early| early.serves(Request::Search(params)))
        );
        build(
            self.hold.snapshot(),
            self.hold.content_model(),
            self.early.is_some(),
        )
    }

    /// Whether no accepted fact or coverage loss known now withdrew the
    /// capability an early hold answers under. Canonical holds are current.
    pub(crate) fn current(&self) -> bool {
        self.early.as_ref().is_none_or(|early| {
            if !early.authority.check_root() {
                return false;
            }
            if !early.facts_current() {
                early.evidence.count_fact_refusal();
                return false;
            }
            true
        })
    }

    pub(crate) fn damage(&self, host: &Host<O>, detail: String) -> ReadRefusal {
        host.withdraw_for_read_damage(&self.hold, detail)
    }
}

#[cfg(all(test, feature = "induced-failure"))]
mod tests;
