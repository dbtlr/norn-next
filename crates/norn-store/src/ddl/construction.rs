//! Secondary indexes built once their construction step has finished writing.

use crate::Step;

/// Non-unique indexes and the boundary before their readers can run: dropped
/// when construction begins and created once, from the create ceremony's own
/// text, when that step ends. Unique indexes and primary keys remain
/// throughout construction, and so do every index on findings, directory
/// marks, rule sets, tombstones and quarantines, and the indexes in [`KEPT`].
pub(crate) const DEFERRED: &[(&str, Step)] = &[
    ("documents_suffix_key", Step::Names),
    ("documents_admitted_suffix_key", Step::Names),
    ("vault_files_path_nocase", Step::Names),
    ("vault_files_path_key", Step::Names),
    ("vault_files_suffix_key", Step::Names),
    ("vault_files_admitted_suffix_key", Step::Names),
    ("document_fields_typed", Step::Content),
    ("document_fields_least_typed", Step::Content),
    ("document_fields_presence", Step::Content),
    ("document_fields_offset", Step::Content),
    ("document_fields_folded", Step::Content),
    ("link_keys_key", Step::Structure),
    ("link_keys_suffix_key", Step::Structure),
    ("link_keys_path_key", Step::Structure),
    ("link_anchor_keys_value", Step::Structure),
    ("headings_document_reading", Step::Structure),
    ("headings_document_slug", Step::Structure),
    ("blocks_document_block_id", Step::Structure),
    ("document_tags_folded_name", Step::Structure),
    ("documents_content_hash", Step::FullTextComplete),
    ("documents_change_feed", Step::FullTextComplete),
];

/// Recover the original create text, preserving sqlite_schema and its digest.
pub(crate) fn statement(name: &str) -> String {
    super::statements()
        .into_iter()
        .find(|sql| {
            sql.strip_prefix("CREATE INDEX ")
                .and_then(|rest| rest.split_whitespace().next())
                == Some(name)
        })
        .expect("deferred index belongs to the create ceremony")
}

/// Non-unique indexes on the deferred tables that construction maintains row
/// by row, as an ordinary build does.
///
/// - `document_trigrams_document_trigram` and `documents_path_nocase`: the
///   names step already seeks them, the first through a document upsert's
///   foreign-key plan and the second through unreadable-directory ranges.
/// - `documents_path_key`, `document_fields_value_type`, `document_fields_raw`
///   and `document_fields_least_raw`: a hub rename moves a cluster of their
///   entries between two key regions on every write. Built in bulk, they are
///   packed so full that each reinsertion splits a page and each emptied page
///   merges back, so every later hub rename wrote about nine more WAL pages
///   than an ordinary build's, indefinitely. Maintained row by row they keep
///   an ordinary build's fill, and steady-state writes cost what they always
///   did.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "names what begin_steps keeps; the partition test checks it"
    )
)]
pub(crate) const KEPT: &[&str] = &[
    "document_trigrams_document_trigram",
    "documents_path_nocase",
    "documents_path_key",
    "document_fields_value_type",
    "document_fields_raw",
    "document_fields_least_raw",
];

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_non_unique_index_has_one_step() {
        use std::collections::BTreeSet;
        let tables = [
            "documents",
            "vault_files",
            "document_fields",
            "link_keys",
            "link_anchor_keys",
            "headings",
            "blocks",
            "document_tags",
            "document_trigrams",
        ];
        let expected: BTreeSet<_> = super::super::statements()
            .into_iter()
            .filter_map(|sql| {
                let rest = sql.strip_prefix("CREATE INDEX ")?;
                let mut words = rest.split_whitespace();
                let name = words.next()?;
                if words.next()? != "ON" {
                    return None;
                }
                let table = words.next()?.split(['(', ' ', '\n']).next()?;
                tables.contains(&table).then(|| name.trim().to_owned())
            })
            .collect();
        let deferred: BTreeSet<_> = DEFERRED.iter().map(|(name, _)| name.to_string()).collect();
        let kept: BTreeSet<_> = KEPT.iter().map(|name| name.to_string()).collect();
        assert_eq!(deferred.len(), DEFERRED.len(), "duplicate deferred index");
        assert_eq!(kept.len(), KEPT.len(), "duplicate kept index");
        assert!(
            deferred.is_disjoint(&kept),
            "an index both deferred and kept"
        );
        assert_eq!(&deferred | &kept, expected);
        for name in KEPT {
            assert!(statement(name).starts_with("CREATE INDEX "));
        }
        for (name, _) in DEFERRED {
            assert!(statement(name).starts_with("CREATE INDEX "));
        }
    }
}
