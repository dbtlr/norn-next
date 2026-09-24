//! The reading a read answers from, as a cursor carries it, and the judgment
//! of the reading a cursor was minted under against it.

use norn_db::rusqlite::types::Value;
use norn_wire::{Cursor, CursorOrderChanged, Moved};

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
    /// refusal names the order the cursor was minted in either way.
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
