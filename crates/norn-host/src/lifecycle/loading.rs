//! Canonical early admission while a from-zero build fills the store it serves.
//!
//! A from-zero build begins a [`Construction`] under its claim before it walks
//! the vault, and publishes each step it ends into it. A request whose shape a
//! published step admits ([`norn_store::early`]) establishes an early hold on the
//! store under construction; one whose step is not yet published waits under
//! its original deadline; every other request refuses at once. Publication never
//! publishes `Ready`, and `Ready` closes the construction in the hold that
//! publishes it.
use super::*;
use norn_fs::watch::BuildAuthority;
use norn_store::Step;
use norn_store::early::Boundary;
use norn_wire::{FindParams, GetParams};

#[derive(Clone, Copy)]
pub(crate) enum Request<'a> {
    Canonical,
    Get(&'a GetParams),
    Find(&'a FindParams),
}

impl Request<'_> {
    /// The construction boundary whose publication admits this request early,
    /// or `None` where only `Ready` serves it.
    pub(super) fn boundary(self) -> Option<Boundary> {
        let boundary = match self {
            Self::Canonical => return None,
            Self::Get(params) => norn_store::early::get(params),
            Self::Find(params) => norn_store::early::find(params),
        };
        (boundary != Boundary::Ready).then_some(boundary)
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
/// watcher authority it began under.
pub(super) struct Building<R: ReadSource> {
    claim: u64,
    token: BuildToken,
    store_epoch: String,
    pool: Arc<R>,
    model: Arc<ContentModel>,
    authority: BuildAuthority,
    /// The last step this build ended and published; `None` before step 1 ends.
    published: Option<Step>,
    /// NORN-468 C-2 (Get pull-ahead) is the consuming layer.
    #[allow(dead_code)]
    requests: Requests,
}

/// The requests a build pulls ahead of its queue and the targets it has
/// published ready. NORN-468 C-2 (pull-ahead) fills this; until then every Get
/// is admitted at vault-wide Structure and no build drains requests.
#[derive(Default)]
pub(super) struct Requests;

/// What a request meets in its entry's construction.
pub(super) enum Early<R: ReadSource> {
    /// A published step admits the request.
    Admitted(Admitted<R>),
    /// The request's step is not yet published, or no build has begun under a
    /// provisional first attach: the request waits under its deadline.
    Waits,
    /// No capability stands for this request: refuse at once.
    Refused,
}

/// One request's admission against a build's publications.
pub(crate) struct Admitted<R> {
    pub(super) pool: Arc<R>,
    pub(super) model: Arc<ContentModel>,
    authority: BuildAuthority,
    token: BuildToken,
    published: Step,
    /// The build had not judged relations when this request was admitted.
    pub(super) relations_loading: bool,
}

impl<R> Clone for Admitted<R> {
    fn clone(&self) -> Self {
        Self {
            pool: Arc::clone(&self.pool),
            model: Arc::clone(&self.model),
            authority: self.authority.clone(),
            token: self.token,
            published: self.published,
            relations_loading: self.relations_loading,
        }
    }
}

impl<R> Admitted<R> {
    /// Check the build's root outside every lock; a loss withdraws the build.
    pub(super) fn authority_root_current(&self) -> bool {
        self.authority.check_root()
    }

    fn serves(&self, request: Request<'_>) -> bool {
        request
            .boundary()
            .is_some_and(|boundary| reaches(Some(self.published), boundary))
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
            claim: self.epoch,
            token,
            store_epoch: begun.store_epoch,
            pool,
            model: begun.model,
            authority: begun.authority,
            published: None,
            requests: Requests,
        }));
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

    /// Close construction, retiring its pool outside the gate and waking every waiter.
    pub(super) fn close_construction(&mut self) {
        if let Some(construction) = self.construction.take() {
            if let Construction::Building(building) = construction {
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
    /// C-2 adds per-target Get admission here: a Get whose target the build
    /// has published ready is admitted before vault-wide Structure, and an
    /// interest in a target not yet ready joins [`Building::requests`].
    pub(super) fn early(&self, request: Request<'_>) -> Early<A::Reader> {
        let Some(boundary) = request.boundary() else {
            return Early::Refused;
        };
        // A rebuild under a damage verdict publishes that verdict, not warming,
        // and no answer carries an untrusted reading: it refuses at once.
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
                match building.published {
                    Some(published) if reaches(Some(published), boundary) => {
                        Early::Admitted(Admitted {
                            pool: Arc::clone(&building.pool),
                            model: Arc::clone(&building.model),
                            authority: building.authority.clone(),
                            token: building.token,
                            published,
                            relations_loading: !reaches(Some(published), Boundary::Relations),
                        })
                    }
                    _ => Early::Waits,
                }
            }
            _ => Early::Refused,
        }
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
        ) && admitted.serves(request)
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

    /// Whether no accepted fact or coverage loss known now withdrew the
    /// capability an early hold answers under. Canonical holds are current.
    pub(crate) fn current(&self) -> bool {
        self.early
            .as_ref()
            .is_none_or(|early| early.authority.check_root())
    }

    pub(crate) fn damage(&self, host: &Host<O>, detail: String) -> ReadRefusal {
        host.withdraw_for_read_damage(&self.hold, detail)
    }
}

#[cfg(all(test, feature = "induced-failure"))]
mod tests;
