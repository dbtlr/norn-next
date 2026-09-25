//! The seam every read verb answers through.
//!
//! A read verb resolves the vault its request addresses, takes one hold on
//! that vault's entry, runs its store builder on the hold's snapshot against
//! the content model the snapshot pins, and wraps what the builder answered in
//! a [`VaultAnswer`] carrying the reading the hold was taken under. Every
//! refusal on that path leaves as `norn-wire`'s one envelope: the address's,
//! the hold's through [`ReadRefusal::answer`](crate::ReadRefusal::answer), and
//! the builder's through [`page_refusal`].
//!
//! **The answer carries the reading of the snapshot it was read from.** The
//! reading and the snapshot come out of one hold of the entry gate, so the
//! trust state and the store generation an answer names describe the instant
//! its rows were read at.

use norn_store::{ContentModel, CountWork, PageRefusal, Snapshot, SnapshotCounters, StoreError};
use norn_wire::{
    AnswerAdvisory, AnswerReading, CountParams, CountReport, ErrorEnvelope, Unsatisfied,
    VaultAnswer, VaultName,
};

use crate::address::registered_name;
use crate::lifecycle::{EntryOps, HoldReading, Host, ReadSource, SnapshotSource};
use crate::refusal::page_refusal;

/// What a read builder answered on one snapshot: the parts of the request it
/// could not apply, what the parts it applied assumed, the verb's report, and
/// what the builder read to answer it.
pub(crate) struct Built<R, W> {
    pub(crate) unsatisfied: Vec<Unsatisfied>,
    pub(crate) advisories: Vec<AnswerAdvisory>,
    pub(crate) report: R,
    pub(crate) work: W,
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
    /// write generation below zero names no position in any database, and is
    /// refused as the damaged store it is.
    pub(crate) fn answer_reading(&self, name: &VaultName) -> Result<AnswerReading, ErrorEnvelope> {
        let trust = self.published().clone().answer(name)?;
        let generation = u64::try_from(self.store().write_generation()).map_err(|_| {
            page_refusal(PageRefusal::Store(StoreError::Damaged {
                what: "the store's write generation is below zero".to_string(),
            }))
        })?;
        Ok(AnswerReading::new(
            trust,
            self.store().epoch(),
            generation,
            None,
        ))
    }
}

impl<O> Host<O>
where
    O: EntryOps,
    <O::Attachment as SnapshotSource>::Reader: ReadSource<Snapshot = Snapshot>,
{
    /// Answer one read verb: resolve `address`, take a hold on the vault it
    /// names, run `build` on the hold's snapshot against the content model
    /// that snapshot pins, and wrap what it built under the hold's reading.
    ///
    /// The hold is ended when this returns, whichever way it returns, so the
    /// snapshot a builder ran on is given back before the answer leaves.
    pub(crate) fn answer_read<R, W>(
        &self,
        address: &norn_wire::VaultAddress,
        build: impl FnOnce(&Snapshot, &ContentModel) -> Result<Built<R, W>, PageRefusal>,
    ) -> Result<Answered<R, W>, ErrorEnvelope> {
        let name = registered_name(address)?;
        let hold = self
            .begin_read(name)
            .map_err(|refusal| refusal.answer(name))?;
        let reading = hold.reading().answer_reading(name)?;
        let built = build(hold.snapshot(), hold.content_model()).map_err(page_refusal)?;
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
        self.answer_read(&params.vault, |snapshot, declared| {
            let counted = snapshot.count(params, declared)?;
            let work = counted.work;
            let (unsatisfied, advisories, report) = counted.into_report();
            Ok(Built {
                unsatisfied,
                advisories,
                report,
                work,
            })
        })
    }
}
