//! Closest-heading suggestions, composed after the batch's routes settle.

use super::{Composed, Made, Reckoned, State, cited, skip};
use crate::planner::repair::Reading;
use norn_store::similarity;
use norn_wire::{
    AuthorCondition, Confidence, ContentHash, FindingKind, Operation, OperationKind, SkipReason,
};

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
) -> Result<(), R::Error> {
    let document = &reckoned.document;
    let original = norn_text::Document::parse(
        std::str::from_utf8(&reckoned.start.bytes).expect("a read document"),
    );
    let links: Vec<_> = original
        .frontmatter_wikilinks()
        .into_iter()
        .chain(original.scan_body().links())
        .collect();
    let mut groups = Vec::new();
    let mut by_link = std::collections::HashMap::new();
    for (at, row) in reckoned.rows.iter().enumerate().filter(|(_, row)| {
        row.kind == FindingKind::MissingAnchor && super::has_fix(row, document.repairing)
    }) {
        let Some(link) = links.iter().find(|link| {
            let address = match &link.protocol {
                Some(protocol) => format!("{protocol}://{}", link.target),
                None => link.target.clone(),
            };
            let target = link
                .anchor
                .as_ref()
                .map(|anchor| format!("{address}#{anchor}"));
            target == row.target
                && link.span.map(|span| span.byte_offset as u64)
                    == row.span.as_ref().map(|span| span.byte_offset)
        }) else {
            continue;
        };
        let Some(from) = link.anchor.as_deref() else {
            continue;
        };
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
    for (link, address, from, rows) in groups {
        let (_, row) = rows[0];
        let target = &row.head.candidates()[0].path;
        let Some(held) = vault.headings(target)? else {
            continue;
        };
        let mut work = document.repairing.work.get();
        work.headings += held.headings.len() as u64;
        document.repairing.work.set(work);
        let query = similarity::trigrams(from);
        let mut best = similarity::CUTOFF;
        let mut winners = Vec::new();
        for heading in &held.headings {
            let score = similarity::jaccard(&query, &similarity::trigrams(&heading.text));
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
        let parsed = norn_text::Document::parse(
            std::str::from_utf8(&composed.state.bytes).expect("a read document"),
        );
        let edited = parsed.rewrite_anchors(link.family, &address, from, to);
        let rewritten = match edited {
            Ok(rewritten) if rewritten.rewritten > 0 => rewritten,
            result => {
                let reason = match result {
                    Err(reason) => format!("{reason:?}"),
                    _ => "no matching link remains".to_string(),
                };
                for (at, row) in &rows {
                    composed.decided[*at] =
                        Some(skip(row, SkipReason::LinkNotRewritable).with_note(reason.clone()));
                }
                continue;
            }
        };
        let bytes: std::sync::Arc<[u8]> = std::sync::Arc::from(rewritten.text.into_bytes());
        let verdict = crate::applier::verdict(
            &document.before,
            &composed.state.standing,
            &composed.state.at,
            &bytes,
            document.repairing.declared,
            document.repairing.case,
        );
        if !verdict.introduced.is_empty() {
            for (at, row) in &rows {
                composed.decided[*at] = Some(
                    skip(row, SkipReason::JudgeWouldRefuse).with_candidates(candidates.clone()),
                );
            }
            continue;
        }
        let note = score_note(&heading.text, best);
        if !document.admits(Confidence::Suggested) {
            let proposed = Operation::new(operation).with_conditions(vec![condition]);
            for (at, row) in &rows {
                composed.decided[*at] = Some(
                    skip(row, SkipReason::BelowThreshold)
                        .with_proposed(proposed.clone())
                        .with_note(note.clone()),
                );
            }
            continue;
        }
        composed.state = State::of(composed.state.at.clone(), bytes, verdict.standing);
        for (at, _) in &rows {
            composed.decided[*at] = None;
        }
        composed.made.push(Made::Fixed {
            operations: vec![operation],
            conditions: vec![condition],
            cited: rows
                .iter()
                .map(|(_, row)| cited(row, Confidence::Suggested, vec![note.clone()]))
                .collect(),
            removed_field: None,
        });
    }
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
        let scratch = norn_testkit::scratch::Scratch::new("repair-closest-heading");
        std::fs::write(scratch.join("holder.md"), source).unwrap();
        if !own {
            std::fs::write(scratch.join("target.md"), target).unwrap();
        }
        let parsed = norn_text::Document::parse(source);
        let rows: Vec<_> = parsed
            .scan_body()
            .links()
            .iter()
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
        let declared = Declared::unpinned();
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
            &repairing,
            &Over {
                view: &view,
                index: &index,
            },
        )
        .unwrap()
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
