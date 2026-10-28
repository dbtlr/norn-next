//! Secondary indexes built once their construction step has finished writing.

use crate::Step;

/// Non-unique indexes and the boundary before their readers can run.
/// Unique indexes and primary keys remain throughout construction. Findings,
/// directory marks, rule sets, tombstones and quarantines keep every index:
/// construction maintains them incrementally. The reverse trigram index is
/// retained during names: document upsert foreign-key plans already seek it.
/// The NOCASE path index likewise stays for unreadable-directory ranges.
/// These two entries name their latest boundary but are needed before it.
pub(crate) const DEFERRED: &[(&str, Step)] = &[
    ("documents_path_key", Step::Names),
    ("documents_path_nocase", Step::Names),
    ("documents_suffix_key", Step::Names),
    ("documents_admitted_suffix_key", Step::Names),
    ("vault_files_path_nocase", Step::Names),
    ("vault_files_path_key", Step::Names),
    ("vault_files_suffix_key", Step::Names),
    ("vault_files_admitted_suffix_key", Step::Names),
    ("document_trigrams_document_trigram", Step::Names),
    ("document_fields_value_type", Step::Content),
    ("document_fields_raw", Step::Content),
    ("document_fields_typed", Step::Content),
    ("document_fields_least_raw", Step::Content),
    ("document_fields_least_typed", Step::Content),
    ("document_fields_presence", Step::Content),
    ("document_fields_offset", Step::Content),
    ("document_fields_folded", Step::Content),
    ("link_keys_key", Step::Structure),
    ("link_keys_suffix_key", Step::Structure),
    ("link_keys_path_key", Step::Structure),
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

/// Names already runs upserts and unreadable-directory range statements.
pub(crate) fn needed_during_names(name: &str) -> bool {
    matches!(
        name,
        "documents_path_nocase" | "document_trigrams_document_trigram"
    )
}

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
        let actual: BTreeSet<_> = DEFERRED.iter().map(|(name, _)| name.to_string()).collect();
        assert_eq!(actual.len(), DEFERRED.len(), "duplicate deferred index");
        assert_eq!(actual, expected);
        for (name, _) in DEFERRED {
            assert!(statement(name).starts_with("CREATE INDEX "));
        }
    }
}

