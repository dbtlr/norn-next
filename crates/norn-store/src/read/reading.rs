//! The reading a read answers from, as a cursor carries it, and the judgment
//! of the reading a cursor was minted under against it.

use norn_db::rusqlite::types::Value;
use norn_wire::{Cursor, CursorOrderChanged, HitResume, Moved, PagedRows, RungSet};

use super::{FieldOrder, Lookups, PageRefusal, Ran};
use crate::ddl;
use crate::error::{self, StoreError};
use crate::fields::ContentModel;
use crate::find::FindStatement;
use crate::store::Snapshot;

impl Snapshot {
    /// Judge the reading `cursor` was minted under against the reading a page
    /// in `order` answers from on this snapshot, and say what moved since.
    ///
    /// The cursor is refused where its fingerprint is not the order's — the
    /// active fingerprint for a typed order, none for any other — and where
    /// `misplaced` says its position is detectably in another order. The
    /// refusal names the order the cursor was minted in either way. A page of
    /// rows in no schema's order judges through
    /// [`Snapshot::judge_unordered_reading`] instead.
    pub(crate) fn judge_reading(
        &self,
        cursor: &Cursor,
        order: Option<FieldOrder>,
        misplaced: bool,
        lookups: &mut Lookups,
    ) -> Result<Vec<Moved>, PageRefusal> {
        let now = self.reading_facts(order, lookups)?;
        let minted_under = cursor.snapshot().schema_fingerprint.as_deref();
        if minted_under != now.schema_fingerprint.as_deref() || misplaced {
            let current = now.schema_fingerprint.clone();
            return Err(PageRefusal::OrderChanged(match minted_under {
                Some(minted_under) => CursorOrderChanged::new(minted_under, current),
                None => CursorOrderChanged::minted_raw(current),
            }));
        }
        cursor.continuation(&now).map_err(PageRefusal::OrderChanged)
    }

    /// Judge the reading `cursor` was minted under against the reading a page
    /// of `paged` answers from on this snapshot, and say what moved since.
    ///
    /// `paged` are rows in no schema's order — hits, facets, findings or a
    /// collection's ordinals — and no page of them mints a cursor carrying a
    /// fingerprint, so one carrying a fingerprint names no position among
    /// them and is refused as not taken.
    pub(crate) fn judge_unordered_reading(
        &self,
        cursor: &Cursor,
        paged: PagedRows,
        lookups: &mut Lookups,
    ) -> Result<Vec<Moved>, PageRefusal> {
        if cursor.snapshot().schema_fingerprint.is_some() {
            return Err(PageRefusal::cursor_not_taken(cursor, paged));
        }
        self.judge_reading(cursor, None, false, lookups)
    }

    /// Judge a cursor continuing a page of hits ranked by `ladder` against the
    /// reading that page answers from on this snapshot: where it resumes and
    /// what moved since.
    ///
    /// A cursor that is no hit's names no position among hits, and a ranking
    /// is no schema's order, so a hit's carrying a fingerprint names none
    /// either, as [`Snapshot::judge_unordered_reading`] refuses it: both are
    /// refused as not taken. A hit cursor minted under another ladder is
    /// refused as an order that changed, naming both ladders.
    pub(crate) fn judge_ranked_reading<'c>(
        &self,
        cursor: &'c Cursor,
        ladder: &RungSet,
        lookups: &mut Lookups,
    ) -> Result<HitResume<'c>, PageRefusal> {
        let not_taken = || PageRefusal::cursor_not_taken(cursor, PagedRows::Hit);
        if cursor.snapshot().schema_fingerprint.is_some() {
            return Err(not_taken());
        }
        let now = self.reading_facts(None, lookups)?;
        cursor
            .ranked_continuation(&now, ladder)
            .ok_or_else(not_taken)?
            .map_err(PageRefusal::OrderChanged)
    }

    /// This snapshot's reading as a cursor carries it for a page in `order`:
    /// the epoch and the write generation, and the active fingerprint where
    /// the order is typed.
    pub(crate) fn reading_facts(
        &self,
        order: Option<FieldOrder>,
        lookups: &mut Lookups,
    ) -> Result<norn_wire::Snapshot, StoreError> {
        let fingerprint = match order {
            Some(FieldOrder::Typed) => self.fingerprint(lookups)?,
            Some(FieldOrder::Raw) | None => None,
        };
        let generation =
            u64::try_from(self.reading().write_generation()).map_err(|_| StoreError::Damaged {
                what: "the store's write generation is below zero".to_string(),
            })?;
        Ok(norn_wire::Snapshot::new(
            self.epoch(),
            generation,
            fingerprint,
            None,
        ))
    }

    /// The active fingerprint, read once per request; `None` where no schema
    /// is pinned.
    pub(crate) fn fingerprint(&self, lookups: &mut Lookups) -> Result<Option<String>, StoreError> {
        if let Some(fingerprint) = &lookups.fingerprint {
            return Ok(fingerprint.clone());
        }
        let read = Ran::new(
            FindStatement::ActiveFingerprint,
            (
                norn_db::meta::META_READ_SQL.to_string(),
                vec![Value::Text(ddl::meta::VAULT_SCHEMA_FINGERPRINT.to_string())],
            ),
        );
        let fingerprint = self
            .run_statement(&mut lookups.ran, read, |row| row.get::<_, String>(0))
            .map_err(|problem| error::sql("reading the pinned schema fingerprint", problem))?
            .into_iter()
            .next();
        lookups.fingerprint = Some(fingerprint.clone());
        Ok(fingerprint)
    }

    /// Refuse `declared` where it was read from another schema than the
    /// snapshot pins, so every typed order a request compiles under is the
    /// one the typed column holds, and every declared facet a describe answers
    /// is the pinned schema's.
    pub(crate) fn declaration_pinned(
        &self,
        declared: &ContentModel,
        lookups: &mut Lookups,
    ) -> Result<(), PageRefusal> {
        let pinned = self.fingerprint(lookups)?;
        if declared.schema() != pinned.as_deref() {
            return Err(PageRefusal::DeclarationNotPinned {
                declared_under: declared.schema().map(str::to_string),
                pinned,
            });
        }
        Ok(())
    }
}
