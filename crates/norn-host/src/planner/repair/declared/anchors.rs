//! Closest-heading suggestions, composed after the batch's routes settle.

use super::names::{self, NameFix};
use super::{Composed, Reckoned, skip};
use crate::planner::repair::Reading;
use norn_store::similarity;
use norn_wire::{AuthorCondition, Confidence, ContentHash, FindingKind, OperationKind, SkipReason};

/// The fixed score wording of a closest-heading suggestion.
fn score_note(heading: &str, score: f64) -> String {
    format!("closest heading `{heading}` scores {score:.2} by trigram Jaccard, cutoff 0.50")
}

/// Compose anchor fixes last, using the selected link's original spelling
/// and the target's heading projection; no target body is loaded.
pub(super) fn fixes<R: Reading>(
    reckoned: &Reckoned<'_>,
    composed: &mut Composed,
    vault: &R,
    tagged: &std::collections::BTreeSet<norn_wire::DocumentPath>,
) -> Result<(), R::Error> {
    let document = &reckoned.document;
    if !reckoned.rows.iter().any(|row| {
        row.kind == FindingKind::MissingAnchor && super::has_fix(row, document.repairing)
    }) {
        return Ok(());
    }
    // A holder whose bytes no longer decode holds no link to rewrite, so its
    // missing-anchor rows keep their plain skip.
    let Ok(source) = crate::derivation::document_source(&reckoned.start.bytes) else {
        return Ok(());
    };
    let original = norn_text::Document::parse(source);
    let mut links = std::collections::HashMap::new();
    for link in original
        .frontmatter_wikilinks()
        .into_iter()
        .chain(original.scan_body().links())
    {
        let address = match &link.protocol {
            Some(protocol) => format!("{protocol}://{}", link.target),
            None => link.target.clone(),
        };
        if let Some(anchor) = &link.anchor {
            let key = (
                format!("{address}#{anchor}"),
                link.span.map(|span| span.byte_offset as u64),
            );
            links.entry(key).or_insert(link);
        }
    }
    let mut groups = Vec::new();
    let mut by_link = std::collections::HashMap::new();
    for (at, row) in reckoned.rows.iter().enumerate().filter(|(_, row)| {
        row.kind == FindingKind::MissingAnchor && super::has_fix(row, document.repairing)
    }) {
        let Some(link) = row.target.as_ref().and_then(|target| {
            links.get(&(
                target.clone(),
                row.span.as_ref().map(|span| span.byte_offset),
            ))
        }) else {
            continue;
        };
        let Some(anchor) = link.anchor.as_deref() else {
            continue;
        };
        let from = link
            .heading_anchor_range()
            .map(|range| &link.raw[range])
            .unwrap_or(anchor);
        let address = match &link.protocol {
            Some(protocol) => format!("{protocol}://{}", link.target),
            None => link.target.clone(),
        };
        let group = *by_link
            .entry((link.family, address.clone(), from))
            .or_insert_with(|| {
                groups.push((link, address, from, Vec::new()));
                groups.len() - 1
            });
        groups[group].3.push((at, row));
    }
    let mut headings = std::collections::HashMap::new();
    let mut fixes = Vec::new();
    for (link, address, from, rows) in groups {
        let (_, row) = rows[0];
        let target = &row.head.candidates()[0].path;
        if tagged.contains(target) {
            continue;
        }
        if !headings.contains_key(target) {
            let held = vault.headings(target)?.map(|held| {
                let mut work = document.repairing.work.get();
                work.headings += held.headings.len() as u64;
                document.repairing.work.set(work);
                let trigrams: Vec<_> = held
                    .headings
                    .iter()
                    .map(|heading| similarity::trigrams(&heading.text))
                    .collect();
                (held, trigrams)
            });
            headings.insert(target.clone(), held);
        }
        let Some((held, trigrams)) = &headings[target] else {
            continue;
        };
        let query = similarity::trigrams(link.anchor.as_deref().expect("a grouped heading anchor"));
        let mut best = similarity::CUTOFF;
        let mut winners = Vec::new();
        for (heading, trigrams) in held.headings.iter().zip(trigrams) {
            let written = match link.family {
                norn_text::LinkFamily::Wikilink => &heading.text,
                norn_text::LinkFamily::Markdown => &heading.slug,
            };
            if written.is_empty() || written.starts_with('^') {
                continue;
            }
            let score = similarity::jaccard(&query, trigrams);
            if score > best {
                best = score;
                winners.clear();
            }
            if score == best {
                winners.push(heading);
            }
        }
        let Some((heading, rest)) = winners.split_first() else {
            continue;
        };
        let members: Vec<_> = winners.iter().map(|heading| heading.text.clone()).collect();
        let candidates = super::closed_set::derived_candidates(&members);
        if !rest.is_empty() {
            if document.admits(Confidence::Suggested) {
                let note = winners
                    .iter()
                    .take(norn_wire::CANDIDATE_HEAD)
                    .map(|heading| score_note(&heading.text, best))
                    .collect::<Vec<_>>()
                    .join("; ");
                for (at, row) in &rows {
                    composed.decided[*at] = Some(
                        skip(row, SkipReason::Tie)
                            .with_candidates(candidates.clone())
                            .with_note(note.clone()),
                    );
                }
            }
            continue;
        }
        let to = match link.family {
            norn_text::LinkFamily::Wikilink => &heading.text,
            norn_text::LinkFamily::Markdown => &heading.slug,
        };
        let operation = OperationKind::rewrite_anchor(
            composed.state.at.clone(),
            match link.family {
                norn_text::LinkFamily::Wikilink => norn_wire::LinkFamily::Wikilink,
                norn_text::LinkFamily::Markdown => norn_wire::LinkFamily::Markdown,
            },
            address.clone(),
            from,
            to,
        );
        let condition = AuthorCondition::content_hash(
            target.clone(),
            ContentHash::new(format!("sha256:{}", held.content_hash))
                .expect("an indexed document hash"),
        );
        fixes.push(NameFix {
            operation,
            rows: rows.iter().map(|(at, _)| *at).collect(),
            conditions: vec![condition],
            candidates,
            note: score_note(&heading.text, best),
        });
    }
    names::compose(reckoned, document, composed, fixes);
    Ok(())
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Harness scaffolding: a tree and its derived snapshot.
mod tests {
    use crate::clock::OneReading;
    use crate::derivation::Declared;
    use crate::planner::control::SchemaPlace;
    use crate::planner::links::testing::TreeStore;
    use crate::planner::repair::{Over, Planned, Repairing, plan};
    use crate::planner::view::TreeView;
    use norn_wire::{
        AuthorCondition, Candidate, CandidateHead, Confidence, DocumentPath, FindingKind,
        FindingRow, LinkFamily, OperationKind, Severity, SkipReason, SkippedCandidates,
    };

    /// Plan the holder's missing-anchor rows over its indexed target.
    fn planned(source: &str, target: &str, threshold: Confidence, own: bool) -> Planned {
        planned_under(source, target, threshold, own, "version: 1\n")
    }

    /// Plan missing anchors under the declaration that judges their holder.
    fn planned_under(
        source: &str,
        target: &str,
        threshold: Confidence,
        own: bool,
        schema: &str,
    ) -> Planned {
        let scratch = norn_testkit::scratch::Scratch::new("repair-closest-heading");
        std::fs::write(scratch.join("holder.md"), source).unwrap();
        if !own {
            std::fs::write(scratch.join("target.md"), target).unwrap();
        }
        let parsed = norn_text::Document::parse(source);
        let rows: Vec<_> = parsed
            .frontmatter_wikilinks()
            .into_iter()
            .chain(parsed.scan_body().links())
            .enumerate()
            .map(|(ordinal, link)| {
                let span = link.span.unwrap();
                let address = if own { "" } else { "target" };
                FindingRow::new(
                    ordinal as u64 + 1,
                    FindingKind::MissingAnchor,
                    Severity::Warning,
                    DocumentPath::new("holder.md").unwrap(),
                    Some(format!("{address}#{}", link.anchor.as_ref().unwrap())),
                    Some(norn_wire::Span::new(
                        span.line as u64,
                        span.column as u64,
                        span.byte_offset as u64,
                    )),
                    CandidateHead::new(
                        [Candidate::new(
                            DocumentPath::new(if own { "holder.md" } else { "target.md" }).unwrap(),
                            if own { "holder" } else { "target" },
                        )],
                        1,
                    )
                    .unwrap(),
                    None,
                    "a missing anchor",
                    1,
                )
            })
            .collect();
        let declared = Declared::pinned(
            norn_config::schema::VaultSchema::parse(schema.as_bytes()).unwrap(),
            "a".repeat(64),
        );
        let clock = || panic!("a heading suggestion reads no clock");
        let one = OneReading::of(&clock);
        let repairing = Repairing {
            work: std::cell::Cell::new(crate::evidence::RepairSuggestionsCost::default()),
            declared: &declared,
            case: norn_wire::CaseFold::Exact,
            clock: &one,
            rule_sets: &[],
            threshold,
        };
        let view = TreeView::open(scratch.root(), &[], &SchemaPlace::default()).unwrap();
        let store = TreeStore::over(scratch.root());
        let index = store.index();
        plan(
            &rows,
            &std::collections::BTreeMap::new(),
            &repairing,
            &Over {
                view: &view,
                index: &index,
            },
        )
        .unwrap()
    }

    /// Required fields introduced by an anchor edit take precedence over other refusals.
    #[test]
    fn an_anchor_fix_bringing_in_required_fields_skips_with_the_missing_field() {
        let planned = planned_under(
            "---\nsee: '[[target#Important Headinh]]'\n---\n",
            "# Important Heading\n",
            Confidence::Suggested,
            false,
            "version: 1\nrules:\n  linked:\n    match: {frontmatter: {see: '[[target#Important Heading]]'}}\n    required: {status: null}\n",
        );
        assert!(planned.operations.is_empty());
        assert_eq!(
            planned.skipped[0].reason,
            SkipReason::BringsInRequiredFields
        );
        assert_eq!(
            planned.skipped[0]
                .required_fields
                .as_ref()
                .unwrap()
                .fields()[0]
                .field,
            "status"
        );
    }

    /// A Markdown heading with an empty slug cannot be a wire-valid proposal.
    #[test]
    fn a_heading_with_no_written_anchor_is_not_proposed() {
        for threshold in [Confidence::Suggested, Confidence::DEFAULT_THRESHOLD] {
            let planned = planned("[x](target#!!!!)\n", "# !!!\n", threshold, false);
            assert!(
                !planned
                    .operations
                    .iter()
                    .any(|operation| matches!(operation.kind, OperationKind::RewriteAnchor { .. }))
            );
            assert!(planned.skipped.iter().all(|skip| skip.proposed.is_none()));
        }
    }

    /// The closest heading is written in its syntax's form, including self-links.
    #[test]
    fn a_closest_heading_is_written_and_cited_at_suggested() {
        for (source, family, to, own) in [
            (
                "[[target#Important Headinh|Shown]]\n",
                LinkFamily::Wikilink,
                "Important Heading",
                false,
            ),
            (
                "[Shown](target#important-headinh)\n",
                LinkFamily::Markdown,
                "important-heading",
                false,
            ),
            (
                "# Important Heading\n[[#Important Headinh]]\n",
                LinkFamily::Wikilink,
                "Important Heading",
                true,
            ),
        ] {
            let planned = planned(source, "# Important Heading\n", Confidence::Suggested, own);
            assert!(
                matches!(&planned.operations[0].kind, OperationKind::RewriteAnchor { syntax, to: written, address, .. } if *syntax == family && written == to && address == if own { "" } else { "target" })
            );
            assert_eq!(
                planned.citations[0].findings[0].confidence,
                Confidence::Suggested
            );
            assert_eq!(planned.operations[0].conditions.len(), 1);
            assert!(
                matches!(&planned.operations[0].conditions[0], AuthorCondition::ContentHash { path, .. } if path.as_str() == if own { "holder.md" } else { "target.md" })
            );
            assert!(planned.skipped.is_empty());
        }
    }

    /// One anchor rewrite cites every identical broken link it respells.
    #[test]
    fn identical_missing_anchor_links_share_one_operation_citing_both_findings() {
        let source = "[[target#Important Headinh]]\n[[target#Important Headinh]]\n";
        let fixed = planned(
            source,
            "# Important Heading\n",
            Confidence::Suggested,
            false,
        );
        assert_eq!(fixed.operations.len(), 1);
        assert!(fixed.skipped.is_empty());
        assert_eq!(fixed.citations.len(), 1);
        assert_eq!(
            Some(&fixed.citations[0].operation),
            fixed.operations[0].id.as_ref()
        );
        assert_eq!(
            fixed.citations[0]
                .findings
                .iter()
                .map(|finding| finding.finding)
                .collect::<Vec<_>>(),
            [1, 2],
        );
        let below = planned(
            source,
            "# Important Heading\n",
            Confidence::DEFAULT_THRESHOLD,
            false,
        );
        assert!(below.operations.is_empty());
        assert_eq!(below.skipped.len(), 2);
        assert!(
            below
                .skipped
                .iter()
                .all(|skip| skip.reason == SkipReason::BelowThreshold)
        );
        assert_eq!(below.skipped[0].proposed, below.skipped[1].proposed);
        assert_eq!(below.skipped[0].note, below.skipped[1].note);
    }

    /// Duplicate heading texts tie even though their document-order slugs differ.
    #[test]
    fn equally_close_headings_skip_as_a_suggested_tie() {
        let planned = planned(
            "[[target#Important Headinh]]\n",
            "# Important Heading\n# Important Heading\n",
            Confidence::Suggested,
            false,
        );
        assert!(planned.operations.is_empty());
        assert_eq!(planned.skipped[0].reason, SkipReason::Tie);
        let Some(SkippedCandidates::Values { head, .. }) = &planned.skipped[0].candidates else {
            panic!("heading candidates");
        };
        assert_eq!(head.total(), 2);
    }

    /// The heading suggestion's score has one fixed human-readable spelling.
    #[test]
    fn a_closest_heading_citation_carries_its_fixed_score_note() {
        let planned = planned(
            "[[target#Important Headinh]]\n",
            "# Important Heading\n",
            Confidence::Suggested,
            false,
        );
        assert_eq!(
            planned.citations[0].findings[0].notes,
            ["closest heading `Important Heading` scores 0.80 by trigram Jaccard, cutoff 0.50"]
        );
    }

    /// The default threshold keeps the target hash on its proposed anchor write.
    #[test]
    fn a_closest_heading_below_the_default_threshold_carries_its_operation() {
        let planned = planned(
            "[[target#Important Headinh]]\n",
            "# Important Heading\n",
            Confidence::DEFAULT_THRESHOLD,
            false,
        );
        assert!(planned.operations.is_empty());
        assert_eq!(planned.skipped[0].reason, SkipReason::BelowThreshold);
        let proposed = planned.skipped[0].proposed.as_ref().unwrap();
        assert!(matches!(proposed.kind, OperationKind::RewriteAnchor { .. }));
        assert_eq!(proposed.conditions.len(), 1);
        assert_eq!(
            planned.skipped[0].note.as_deref(),
            Some("closest heading `Important Heading` scores 0.80 by trigram Jaccard, cutoff 0.50")
        );
    }
}
