//! The seam every read verb answers through.
//!
//! A read verb resolves the vault its request addresses, takes one hold on
//! that vault's entry, runs its store builder on the hold's snapshot against
//! the content model the snapshot pins, and wraps what the builder answered in
//! a [`VaultAnswer`] carrying the reading the hold was taken under. Every
//! refusal on that path leaves as `norn-wire`'s one envelope: the address's,
//! the hold's through [`ReadRefusal::answer`](crate::ReadRefusal::answer), and
//! the builder's through [`page_refusal`]. A builder whose store finds its
//! derived data damaged is the one refusal the entry answers for: the damage
//! is published on the entry, which owes the rebuild that resolves it, and the
//! read is refused with what the entry then publishes.
//!
//! **The answer carries the reading of the snapshot it was read from.** The
//! reading and the snapshot come out of one hold of the entry gate, so the
//! trust state and the store generation an answer names describe the instant
//! its rows were read at.

use norn_store::{
    ContentModel, CountWork, Counted, DescribeWork, Described, FindWork, Found, GetWork, Gotten,
    PageRefusal, Snapshot, SnapshotCounters, ValidateWork, Validated,
};
use norn_wire::{
    AnswerAdvisory, AnswerReading, CountParams, CountReport, DescribeParams, DescribeReport,
    ErrorDetail, ErrorEnvelope, FindParams, FindReport, GetParams, GetReport, ReadFailure,
    Unsatisfied, ValidateParams, ValidateReport, VaultAnswer, VaultName,
};

use crate::address::registered_name;
use crate::lifecycle::{EntryOps, HoldReading, Host, ReadSource, SnapshotSource};
use crate::refusal::{PageRefused, page_refusal};
use crate::text::TextLayer;

/// What a read builder answered on one snapshot: the parts of the request it
/// could not apply, what the parts it applied assumed, the verb's report, and
/// what the builder read to answer it.
pub(crate) struct Built<R, W> {
    pub(crate) unsatisfied: Vec<Unsatisfied>,
    pub(crate) advisories: Vec<AnswerAdvisory>,
    pub(crate) report: R,
    pub(crate) work: W,
}

/// Why a read builder answered nothing: the store refused the request, or the
/// builder refused it with an envelope of its own — a search's rung the vault
/// cannot run.
pub(crate) enum BuildRefused {
    Page(PageRefusal),
    Answered(ErrorEnvelope),
}

impl From<PageRefusal> for BuildRefused {
    fn from(refusal: PageRefusal) -> Self {
        BuildRefused::Page(refusal)
    }
}

/// A read verb's answer, and what reading it cost.
///
/// The answer is what crosses the wire. The two readings beside it are the
/// host's own and cross nothing: `work` is what the verb's builder read, by
/// kind, and `snapshot` is what the snapshot the answer was read from cost —
/// the one snapshot the hold established and every statement run on it.
#[derive(Debug)]
pub struct Answered<R, W> {
    /// The verb's answer, under the reading it was taken at.
    pub answer: VaultAnswer<R>,
    /// What the builder read to answer it.
    pub work: W,
    /// What the snapshot the answer was read from cost.
    pub snapshot: SnapshotCounters,
}

impl HoldReading {
    /// The reading an answer taken under this hold carries: the trust state the
    /// entry published and the store reading its snapshot was established at.
    ///
    /// A hold is taken only under a published demand that serves, so the
    /// demand answers with its state; one that did not would be refused with
    /// its own envelope, through the mapping every demand renders through. A
    /// write generation below zero names no position in any database, so the
    /// statement that read it is refused under `host/read-failed`.
    pub(crate) fn answer_reading(&self, name: &VaultName) -> Result<AnswerReading, ErrorEnvelope> {
        let trust = self.published().clone().answer(name)?;
        let generation = u64::try_from(self.store().write_generation()).map_err(|_| {
            ErrorEnvelope::new(
                "the store answered this read a write generation below zero",
                ErrorDetail::read_failed(
                    ReadFailure::statement(),
                    "the store's write generation is below zero",
                ),
            )
        })?;
        Ok(AnswerReading::new(trust, self.store().epoch(), generation))
    }
}

impl<O> Host<O>
where
    O: EntryOps,
    <O::Attachment as SnapshotSource>::Reader: ReadSource<Snapshot = Snapshot>,
{
    /// Answer one read verb: resolve `address`, take a hold on the vault it
    /// names, run `build` on that name, the hold's snapshot and the content
    /// model that snapshot pins, and wrap what it built under the hold's
    /// reading.
    ///
    /// The hold is ended when this returns, whichever way it returns, so the
    /// snapshot a builder ran on is given back before the answer leaves. A
    /// builder refused for damage is answered through
    /// [`Host::withdraw_for_read_damage`] while the hold stands, after the
    /// builder returned, so no statement runs under the gate that publishes. A
    /// builder's own envelope leaves as it is.
    pub(crate) fn answer_read<R, W>(
        &self,
        address: &norn_wire::VaultAddress,
        build: impl FnOnce(&VaultName, &Snapshot, &ContentModel) -> Result<Built<R, W>, BuildRefused>,
    ) -> Result<Answered<R, W>, ErrorEnvelope> {
        let name = registered_name(address)?;
        let hold = self
            .begin_read(name)
            .map_err(|refusal| refusal.answer(name))?;
        let reading = hold.reading().answer_reading(name)?;
        let built = match build(name, hold.snapshot(), hold.content_model()) {
            Ok(built) => built,
            Err(BuildRefused::Answered(envelope)) => return Err(envelope),
            Err(BuildRefused::Page(refusal)) => {
                return Err(match page_refusal(refusal) {
                    PageRefused::Answered(envelope) => envelope,
                    PageRefused::Damaged(detail) => {
                        self.withdraw_for_read_damage(&hold, detail).answer(name)
                    }
                });
            }
        };
        Ok(Answered {
            answer: VaultAnswer::new(reading, built.unsatisfied, built.report)
                .with_advisories(built.advisories),
            work: built.work,
            snapshot: hold.snapshot().counters(),
        })
    }

    /// Answer a `count`: one page of the tallies `params` asks for, from the
    /// vault it addresses.
    pub fn count(
        &self,
        params: &CountParams,
    ) -> Result<Answered<CountReport, CountWork>, ErrorEnvelope> {
        self.answer_read(&params.vault, |_, snapshot, declared| {
            Ok(snapshot.count(params, declared)?.into())
        })
    }

    /// Answer a `find`: one page of the documents `params` asks for, from the
    /// vault it addresses, with what its order and conjunction assumed.
    pub fn find(
        &self,
        params: &FindParams,
    ) -> Result<Answered<FindReport, FindWork>, ErrorEnvelope> {
        self.answer_read(&params.vault, |_, snapshot, declared| {
            Ok(snapshot.find(params, declared)?.into())
        })
    }

    /// Answer a `validate`: one page of the findings `params` asks for, or
    /// their tally, from the vault it addresses.
    pub fn validate(
        &self,
        params: &ValidateParams,
    ) -> Result<Answered<ValidateReport, ValidateWork>, ErrorEnvelope> {
        self.answer_read(&params.vault, |_, snapshot, declared| {
            Ok(snapshot.validate(params, declared)?.into())
        })
    }

    /// Answer a `describe`: one page of the facets `params` asks for, from
    /// the vault it addresses.
    pub fn describe(
        &self,
        params: &DescribeParams,
    ) -> Result<Answered<DescribeReport, DescribeWork>, ErrorEnvelope> {
        self.answer_read(&params.vault, |_, snapshot, declared| {
            Ok(snapshot.describe(params, declared)?.into())
        })
    }

    /// Answer a `get`: the one document `params.target` names in the vault
    /// it addresses, as its record, the section or block its anchor names, or
    /// one page of one of its collections.
    ///
    /// A section and a block are cut by `norn-text` from the body the
    /// snapshot holds of the document, so the text answered is the text the
    /// snapshot derived.
    pub fn get(&self, params: &GetParams) -> Result<Answered<GetReport, GetWork>, ErrorEnvelope> {
        self.answer_read(&params.vault, |_, snapshot, declared| {
            Ok(snapshot.get(params, declared, &TextLayer)?.into())
        })
    }
}

// ---- what each store builder answered, as the seam wraps it ----
//
// A count, a find and a validate each report the parts they could not apply
// and what the parts they applied assumed. A describe applies every part it
// takes and compares no dates, so it carries neither; a get compares no dates,
// so it carries no advisory.

impl From<Counted> for Built<CountReport, CountWork> {
    fn from(counted: Counted) -> Self {
        let work = counted.work;
        let (unsatisfied, advisories, report) = counted.into_report();
        Built {
            unsatisfied,
            advisories,
            report,
            work,
        }
    }
}

impl From<Found> for Built<FindReport, FindWork> {
    fn from(found: Found) -> Self {
        let work = found.work;
        let (unsatisfied, advisories, report) = found.into_report();
        Built {
            unsatisfied,
            advisories,
            report,
            work,
        }
    }
}

impl From<Validated> for Built<ValidateReport, ValidateWork> {
    fn from(validated: Validated) -> Self {
        let work = validated.work;
        let (unsatisfied, advisories, report) = validated.into_report();
        Built {
            unsatisfied,
            advisories,
            report,
            work,
        }
    }
}

impl From<Described> for Built<DescribeReport, DescribeWork> {
    fn from(described: Described) -> Self {
        let work = described.work;
        Built {
            unsatisfied: Vec::new(),
            advisories: Vec::new(),
            report: described.into_report(),
            work,
        }
    }
}

impl From<Gotten> for Built<GetReport, GetWork> {
    fn from(gotten: Gotten) -> Self {
        let work = gotten.work;
        let (unsatisfied, report) = gotten.into_report();
        Built {
            unsatisfied,
            advisories: Vec::new(),
            report,
            work,
        }
    }
}
