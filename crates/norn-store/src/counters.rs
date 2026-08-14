//! Derivation counters, scoped to one request.
//!
//! A counter says how much work a request did, and unlike a clock it says the
//! same thing on a loaded machine. Two bars are built on them — a warm request
//! derives nothing, and the same operation costs the same whatever the vault
//! around it weighs — and both are statements about **one request**.
//!
//! # There is no process-global counter, and there cannot be one
//!
//! A counter set is created by opening a request and travels with it. Nothing
//! accumulates outside one, so "how much did this request derive" is answerable
//! without subtracting two readings of a shared number and hoping nothing else
//! ran in between. That matters as soon as a second request exists: a global
//! counter attributes concurrent work to whoever read it last.
//!
//! **The prohibition is over this vocabulary.** It says that derivation is
//! counted per request and nowhere else; it does not say that nothing in the
//! workspace may accumulate. `norn-host` keeps a cumulative account of what its
//! own jobs spent and did — reads, changesets, rungs — which is a different
//! subject read a different way: those are acts of a host rather than
//! derivation by a request, they are attributed by the window a job holds open
//! rather than by the request it opened, and no counter here is spelled from
//! them or they from it.
//!
//! The reading is what a harness compares. Every counter is always present,
//! whatever its value, because a counter that appears in one reading and not
//! another is a difference rather than a zero: reading a missing counter as zero
//! is how a renamed counter compares equal to the one it replaced.
//!
//! # What is counted is derivation, and only derivation
//!
//! Reading rows back derives nothing, so no read moves a counter. That is the
//! whole mechanism behind the zero-on-warm bar: a request that only reads
//! finishes with every counter at zero, and it does so by construction rather
//! than by a rule the read paths have to keep.
//!
//! An *act* is not a counter either. Nothing counts increments applied or
//! requests opened: those are things that happened, not work that was done, and
//! a counter that moves for an act nobody paid for makes the readings answer a
//! different question than the bars ask of them.
//!
//! # The Composed bar, and why the projection is not on the wrong side of it
//!
//! A changeset carries a mark saying where its post-state came from
//! ([`crate::IncrementProvenance`]), and **a `Composed` increment must record no
//! store-side recomputation of state the applier already composed.** The bar is
//! read off these counters, so which of them can move under `Composed` is the
//! whole of it.
//!
//! [`Counter::FrontmatterProjections`] is the only counter here that names a
//! computation, and **canonical-JSON projection is storage encoding rather than
//! recomputation.** A value tree is what the caller
//! supplied; projecting it to canonical JSON is how a value tree is written into
//! a `TEXT` column, the same act as binding a length to an integer column and
//! only larger. It learns nothing about the document that the caller did not
//! hand over, so it derives nothing — and it is the identical code path under
//! either mark, which means a reading that counted it as derivation would fail
//! the bar for every document with frontmatter regardless of where its facts
//! came from.
//!
//! So the bar binds where the two marks could differ, and today they do not:
//! **the same changeset reads the same counters under either mark.** That is the
//! statement the store makes true by composing supplied facts under both and
//! re-deriving under neither, and it is what a suite asserts rather than a
//! promise a reviewer has to take.

/// One derivation counter.
///
/// The list is the vocabulary: adding a counter is an edit here, and every
/// reading carries all of them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Counter {
    DocumentsUpserted,
    DocumentsDeleted,
    LinkRowsWritten,
    HeadingRowsWritten,
    BlockRowsWritten,
    TagRowsWritten,
    FactRowsDiscarded,
    /// Frontmatter value trees written as canonical JSON. **Storage encoding,
    /// not derivation** — see this module's Composed bar.
    FrontmatterProjections,
    TombstonesRecorded,
    FindingsWritten,
    FindingsDiscarded,
    VectorsWritten,
    VaultSchemaPins,
}

impl Counter {
    /// Every counter, in the order a reading reports them.
    const ALL: &'static [Counter] = &[
        Counter::DocumentsUpserted,
        Counter::DocumentsDeleted,
        Counter::LinkRowsWritten,
        Counter::HeadingRowsWritten,
        Counter::BlockRowsWritten,
        Counter::TagRowsWritten,
        Counter::FactRowsDiscarded,
        Counter::FrontmatterProjections,
        Counter::TombstonesRecorded,
        Counter::FindingsWritten,
        Counter::FindingsDiscarded,
        Counter::VectorsWritten,
        Counter::VaultSchemaPins,
    ];

    /// The name the counter is compared by. A harness matches readings on these
    /// strings, so a rename is a deliberate break rather than a silent one.
    const fn name(self) -> &'static str {
        match self {
            Counter::DocumentsUpserted => "documents_upserted",
            Counter::DocumentsDeleted => "documents_deleted",
            Counter::LinkRowsWritten => "link_rows_written",
            Counter::HeadingRowsWritten => "heading_rows_written",
            Counter::BlockRowsWritten => "block_rows_written",
            Counter::TagRowsWritten => "tag_rows_written",
            Counter::FactRowsDiscarded => "fact_rows_discarded",
            Counter::FrontmatterProjections => "frontmatter_projections",
            Counter::TombstonesRecorded => "tombstones_recorded",
            Counter::FindingsWritten => "findings_written",
            Counter::FindingsDiscarded => "findings_discarded",
            Counter::VectorsWritten => "vectors_written",
            Counter::VaultSchemaPins => "vault_schema_pins",
        }
    }
}

/// What one request derived.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DerivationCounters {
    values: [u64; Counter::ALL.len()],
}

impl DerivationCounters {
    /// The whole reading, name by name, every counter present.
    ///
    /// This is the shape a counter snapshot is built from: a sequence of
    /// `(name, value)` pairs, which is why the names are `'static` and the
    /// order is fixed. It is also the whole vocabulary, so anything that needs
    /// the names has them.
    pub fn readings(&self) -> impl Iterator<Item = (&'static str, u64)> + '_ {
        Counter::ALL
            .iter()
            .enumerate()
            .map(|(index, counter)| (counter.name(), self.values[index]))
    }

    /// One counter's value, or `None` for a name this vocabulary does not carry.
    ///
    /// Absent rather than zero, because reading a missing counter as zero is how
    /// a renamed counter compares equal to the one it replaced — which is the
    /// drift the fixed reading order exists to prevent.
    pub fn get(&self, name: &str) -> Option<u64> {
        self.readings()
            .find(|(counter, _)| *counter == name)
            .map(|(_, value)| value)
    }

    /// Whether this request derived nothing at all.
    pub fn is_all_zero(&self) -> bool {
        self.values.iter().all(|value| *value == 0)
    }

    pub(crate) fn add(&mut self, counter: Counter, amount: u64) {
        let index = Counter::ALL
            .iter()
            .position(|candidate| *candidate == counter)
            .expect("every counter is in the reading order");
        self.values[index] = self.values[index].saturating_add(amount);
    }
}
