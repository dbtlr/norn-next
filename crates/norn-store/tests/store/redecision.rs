//! Link health re-decided inside the changeset: after every entry of a
//! changeset is written, the store judges the links the changeset reaches —
//! the links its written documents hold, the suffix-addressed links whose keys
//! fall in a changed path's class, and the path-addressed links spelling a
//! changed path — and files their findings in the same transaction.

use norn_store::{Change, ContentModel, IncrementProvenance, StoreError};

use crate::common::{Scratch, path};
use crate::health::derived;

/// A changeset handed a declaration read under another schema than the one
/// the store pins is refused whole, before any entry is written: its link
/// health would be judged under ambiguity-ignore globs no rebuild under the
/// pinned schema reads.
#[test]
fn a_changeset_under_a_declaration_the_store_does_not_pin_is_refused() {
    let scratch = Scratch::new("redecide-unpinned");
    let mut store = scratch.open();
    let mut request = store.begin_request();
    request
        .pin_vault_schema(b"version: 1\n", "pinned")
        .expect("pinning a schema");
    let before = request.write_generation().expect("the write generation");

    for (declared, under) in [
        (ContentModel::none(), None),
        (ContentModel::under("another"), Some("another")),
    ] {
        let refused = request
            .apply_increment(
                IncrementProvenance::Derived,
                [Change::Upsert(derived("a.md", "[[nowhere]]\n"))],
                &[],
                &declared,
            )
            .expect_err("a declaration the store does not pin");
        assert_eq!(
            refused,
            StoreError::UnpinnedDeclaration {
                what: "the declaration a changeset's link health is judged under was read",
                derived_under: under.map(str::to_string),
                pinned: Some("pinned".to_string()),
            }
        );
        assert_eq!(
            request.stored_document(&path("a.md")).expect("a read"),
            None,
            "a refused changeset's document stands"
        );
        assert_eq!(
            request.write_generation().expect("the write generation"),
            before,
            "a refused changeset took a generation"
        );
    }

    request
        .apply_increment(
            IncrementProvenance::Derived,
            [Change::Upsert(derived("a.md", "[[nowhere]]\n"))],
            &[],
            &ContentModel::under("pinned"),
        )
        .expect("the pinned schema's declaration");
}
