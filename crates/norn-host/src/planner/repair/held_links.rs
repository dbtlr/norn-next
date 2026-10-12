//! Held links: which skipped link findings the plan's final state still
//! holds.
//!
//! **A repair skips every link finding it selects, and then the plan may mend
//! some of them.** No fix is declared for a broken link, a link naming a
//! heading that is not there, or an ambiguous one, so planning skips them all
//! from their rows alone. A route's move can still change what another
//! document's link resolves to, and a finding the final state no longer holds
//! is **dropped** — neither fixed nor skipped — as a finding the judgment of
//! a document's bytes concludes the composed document no longer holds is. This
//! step runs after the whole resolution, since only the resolved plan's change
//! set says which links the plan changed and what they resolve to afterwards.
//!
//! **A finding is judged by the link it is about.** The batch hands back, for
//! each link finding, the family and address of the link at the finding's
//! ordinal in its holder ([`FindingLink`]), so two links of one address and
//! two syntaxes in one holder are two links with two verdicts. The link's key
//! at the plan's after-state is its holder where the batch's routes leave it,
//! its syntax as stored, and its address as the cascade that respells it
//! leaves it ([`Landing`]). The plan's resolution change set is keyed at the
//! after-state and exact: it holds an entry for every link whose resolution
//! the plan changes, every link whose text the plan writes, every link a
//! cascade leaves behind and every link that named a document the plan
//! removes, and no other. A link with no entry is one the plan leaves
//! resolving as it did, or one it removes, and its finding stands. Which of
//! the four put a link in the set does not matter here: an entry
//! records what the link resolves to at the plan's after-state, and the verdict
//! below reads only that.
//!
//! **An entry says whether the finding still holds.** A `link/broken` finding
//! stands only where the link still resolves to nothing; a `link/ambiguous`
//! finding only where it still resolves to several documents; a
//! `link/missing-anchor` finding is about a link resolving to one document,
//! so it is dropped where the link resolves to none or to several, and stands
//! where it resolves to one document.
//!
//! **Declared limit: a missing anchor whose link now names another single
//! document stands.** A link resolving to the document it named — at its path,
//! or at the destination of that document's own route — keeps its finding,
//! since repair never edits a heading or a block id. One resolving to a
//! different single document would need that document judged for the anchor,
//! which is the store's anchor predicate (`norn_store::anchor`) asked through
//! a second predicate or a new statement; until that is asked the finding
//! stands as skipped, so it may be reported for an anchor the new document
//! holds.
//!
//! **A cascade's respelling is read by the plan's own account of it.** A
//! cascade rewrite and a forecast advisory both name a link by its holder,
//! syntax and address, never by its ordinal, so the plan does not say which of
//! the links of one key a holder's rewrite batch respelled. Where the forecast
//! advises on no link of the old key, the batch respelled every one of them,
//! and the finding is read under the key of the new address. Where it does
//! advise on one, the batch left some of them as written, so which one the
//! finding is about is not known, and it is read both ways: respelled, under
//! the new address, and as written, under the old one, where no entry means the
//! link resolves as it did. The finding is dropped only where both readings
//! drop it.
//!
//! **Declared limit: in a holder whose cascade respells some links of an
//! address and leaves others as written, a finding on that address may stay
//! listed though it no longer holds.** It is dropped only where it holds under
//! neither reading, so one that holds under the reading that does not match its
//! link stays. None is dropped that holds.
//!
//! **Declared limit: a link finding about a link the plan removes stands.** The
//! change set has no entry for a link the after-state no longer holds, so a
//! declared `field/forbidden` fix with `remove` deletes a frontmatter field,
//! and a link finding on a wikilink that field held stays skipped though the
//! link is gone (NORN-389). It is the only fix that removes a link: a rename
//! keeps the link in its new field, and a closed set never holds a wikilink.

use std::collections::{BTreeMap, HashMap, HashSet};

use norn_store::FindingLink;
use norn_wire::{
    DocumentPath, FindingKind, FindingRow, LinkAdvisory, LinkFamily, LinkKey, Operation,
    OperationKind, PlanCondition, Resolves, SkippedFinding,
};

use crate::planner::links::wire_family;

/// What the plan changes about where links stand: the batch's routes, the
/// cascade's respellings, the resolution change set and the links the
/// forecast advises on, each read off the resolved plan once.
pub(crate) struct Landing {
    /// Where each routed document goes, by where it stood.
    routes: HashMap<DocumentPath, DocumentPath>,
    /// What a cascade respells a link's address to, by the holder where the
    /// plan leaves it, the link's syntax and the address it was written with.
    respelled: HashMap<(DocumentPath, LinkFamily, String), String>,
    /// What each link the plan changes resolved to before it and resolves to
    /// after it, by its key at the after-state.
    resolutions: HashMap<(DocumentPath, LinkFamily, String), (Resolves, Resolves)>,
    /// The keys, at the after-state, the forecast advises on. A key a cascade
    /// respells that is advised on holds a link the batch left as written.
    advised: HashSet<(DocumentPath, LinkFamily, String)>,
}

impl Landing {
    /// What `operations`, a resolved plan's, route and respell, what its
    /// `conditions` record the plan changes the resolution of, and what its
    /// forecast `advisories` say of the links.
    pub(crate) fn of(
        operations: &[Operation],
        conditions: &[PlanCondition],
        advisories: &[LinkAdvisory],
    ) -> Self {
        let mut routes = HashMap::new();
        let mut respelled = HashMap::new();
        for operation in operations {
            if let OperationKind::MoveDocument { from, to } = &operation.kind {
                routes.insert(from.clone(), to.clone());
            }
            for rewrite in &operation.cascade {
                respelled.insert(
                    (rewrite.path.clone(), rewrite.syntax, rewrite.from.clone()),
                    rewrite.to.clone(),
                );
            }
        }
        let resolutions = conditions
            .iter()
            .filter_map(|condition| match condition {
                PlanCondition::LinkResolution {
                    link,
                    before,
                    after,
                } => Some((keyed(link), (before.clone(), after.clone()))),
                PlanCondition::ContentHash { .. } | PlanCondition::AddressResolution { .. } => None,
            })
            .collect();
        let advised = advisories
            .iter()
            .map(|advisory| keyed(advisory.link()))
            .collect();
        Landing {
            routes,
            respelled,
            resolutions,
            advised,
        }
    }

    /// Where the document at `path` stands once the plan has landed.
    fn placed<'p>(&'p self, path: &'p DocumentPath) -> &'p DocumentPath {
        self.routes.get(path).unwrap_or(path)
    }

    /// The readings of the change the plan makes to the resolution of `link`,
    /// a link as the document at `holder` holds it before the plan; each is
    /// what the plan changes about it — what it resolved to before and what it
    /// resolves to after — and `None` where the plan does not change it.
    ///
    /// **A reading is the link read as the plan may leave it.** A link no
    /// cascade respells is read under its own key. A respelled one is read
    /// under the key of the address it is respelled to, where the forecast
    /// advises on no link of the old key, since then the batch respelled every
    /// link of it. Where the forecast does advise on one, the batch left some
    /// links of that key as written and the plan does not say which this is,
    /// so it is read both ways: respelled, and as written under the old key.
    fn readings(
        &self,
        holder: &DocumentPath,
        link: &FindingLink,
    ) -> impl Iterator<Item = Option<&(Resolves, Resolves)>> {
        let syntax = wire_family(link.family);
        let holder = self.placed(holder).clone();
        let old = (holder, syntax, link.address.clone());
        let respelled = self.respelled.get(&old).map(|to| {
            let (holder, syntax, _) = &old;
            self.resolutions.get(&(holder.clone(), *syntax, to.clone()))
        });
        let as_written = (respelled.is_none() || self.advised.contains(&old))
            .then(|| self.resolutions.get(&old));
        respelled.into_iter().chain(as_written)
    }
}

/// `link` as the maps of a [`Landing`] key it.
fn keyed(link: &LinkKey) -> (DocumentPath, LinkFamily, String) {
    (link.holder.clone(), link.syntax, link.address.clone())
}

/// `skipped` less the link findings the plan `landing` describes leaves no
/// longer holding, in the order it was given.
///
/// `rows` are the batch's findings, which say each skipped finding's kind and
/// holder, and `links` the link each link finding is about, by the finding's
/// id. A skipped finding that is no link finding, or whose link the batch did
/// not hand back, is kept.
pub(crate) fn skipped_still_held(
    skipped: Vec<SkippedFinding>,
    rows: &[FindingRow],
    links: &BTreeMap<u64, FindingLink>,
    landing: &Landing,
) -> Vec<SkippedFinding> {
    let by_id: HashMap<u64, &FindingRow> = rows.iter().map(|row| (row.id, row)).collect();
    skipped
        .into_iter()
        .filter(|skip| {
            let (Some(row), Some(link)) = (by_id.get(&skip.finding), links.get(&skip.finding))
            else {
                return true;
            };
            standing(row, link, landing)
        })
        .collect()
}

/// Whether the link finding `row`, about `link`, still holds once the plan
/// `landing` describes has landed: where any reading of the link says it does,
/// since a finding is dropped only where every reading drops it.
fn standing(row: &FindingRow, link: &FindingLink, landing: &Landing) -> bool {
    landing
        .readings(&row.path, link)
        .any(|change| change.is_none_or(|(_, after)| holds_after(row.kind, after)))
}

/// Whether a link finding of `kind` still holds about a link that resolves to
/// `after`.
fn holds_after(kind: FindingKind, after: &Resolves) -> bool {
    match kind {
        FindingKind::Broken => matches!(after, Resolves::None {}),
        FindingKind::Ambiguous => matches!(after, Resolves::Several {}),
        // A link resolving to one document keeps its finding, whether it is
        // the document it named — at its path or at its own route's
        // destination — or, as the declared limit says, another.
        FindingKind::MissingAnchor => matches!(after, Resolves::One { .. }),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use norn_wire::{LinkRewrite, SkipReason};

    use super::*;

    fn path(text: &str) -> DocumentPath {
        DocumentPath::new(text).expect("a document path")
    }

    fn one(text: &str) -> Resolves {
        Resolves::one(path(text))
    }

    fn row(id: u64, kind: FindingKind, holder: &str) -> FindingRow {
        FindingRow::new(
            id,
            kind,
            norn_wire::Severity::Warning,
            path(holder),
            None,
            None,
            norn_wire::CandidateHead::new([], 0).expect("an empty head"),
            None,
            "a link finding",
            1,
        )
    }

    fn wikilink(address: &str) -> FindingLink {
        FindingLink {
            family: norn_store::LinkFamily::Wikilink,
            address: address.to_string(),
        }
    }

    fn markdown(address: &str) -> FindingLink {
        FindingLink {
            family: norn_store::LinkFamily::Markdown,
            address: address.to_string(),
        }
    }

    fn route(from: &str, to: &str) -> Operation {
        Operation::new(OperationKind::move_document(path(from), path(to)))
    }

    fn changed(
        holder: &str,
        syntax: LinkFamily,
        address: &str,
        before: Resolves,
        after: Resolves,
    ) -> PlanCondition {
        PlanCondition::link_resolution(LinkKey::new(path(holder), syntax, address), before, after)
    }

    /// The ids of the findings `skipped_still_held` keeps among the link findings `of`,
    /// each `(id, kind, holder, link)`, under the plan `operations` and
    /// `conditions` describe, whose forecast advises on nothing.
    fn kept(
        of: &[(u64, FindingKind, &str, FindingLink)],
        operations: &[Operation],
        conditions: &[PlanCondition],
    ) -> Vec<u64> {
        kept_advised(of, operations, conditions, &[])
    }

    /// The ids kept as [`kept`] keeps them, under a plan whose forecast
    /// carries `advisories`.
    fn kept_advised(
        of: &[(u64, FindingKind, &str, FindingLink)],
        operations: &[Operation],
        conditions: &[PlanCondition],
        advisories: &[LinkAdvisory],
    ) -> Vec<u64> {
        let rows: Vec<FindingRow> = of
            .iter()
            .map(|(id, kind, holder, _)| row(*id, *kind, holder))
            .collect();
        let links: BTreeMap<u64, FindingLink> = of
            .iter()
            .map(|(id, _, _, link)| (*id, link.clone()))
            .collect();
        let skipped = of
            .iter()
            .map(|(id, ..)| SkippedFinding::new(*id, SkipReason::NoDeclaredFix))
            .collect();
        let landing = Landing::of(operations, conditions, advisories);
        skipped_still_held(skipped, &rows, &links, &landing)
            .into_iter()
            .map(|skip| skip.finding)
            .collect()
    }

    /// The ids kept of the one finding `kind` about the wikilink `a` held by
    /// `h.md`, where the plan records `before` and `after` for it.
    fn kept_after(kind: FindingKind, before: Resolves, after: Resolves) -> bool {
        let conditions = [changed("h.md", LinkFamily::Wikilink, "a", before, after)];
        !kept(&[(1, kind, "h.md", wikilink("a"))], &[], &conditions).is_empty()
    }

    /// **A link the plan does not change keeps its finding**, whatever the
    /// kind: the resolution change set is exact, so no entry says the link
    /// resolves as it did.
    #[test]
    fn a_link_with_no_entry_in_the_change_set_keeps_its_finding() {
        for kind in [
            FindingKind::Broken,
            FindingKind::Ambiguous,
            FindingKind::MissingAnchor,
        ] {
            let other = [changed(
                "h.md",
                LinkFamily::Wikilink,
                "elsewhere",
                Resolves::none(),
                one("x.md"),
            )];
            assert_eq!(
                kept(&[(1, kind, "h.md", wikilink("a"))], &[], &other),
                vec![1],
                "{kind:?}"
            );
        }
    }

    /// **A broken link stands only where it still resolves to none.**
    #[test]
    fn a_broken_link_is_dropped_where_it_resolves_to_one_or_several_documents() {
        assert!(kept_after(
            FindingKind::Broken,
            Resolves::none(),
            Resolves::none()
        ));
        assert!(!kept_after(
            FindingKind::Broken,
            Resolves::none(),
            one("a.md")
        ));
        assert!(!kept_after(
            FindingKind::Broken,
            Resolves::none(),
            Resolves::several()
        ));
    }

    /// **An ambiguous link stands only where it still resolves to several
    /// documents.**
    #[test]
    fn an_ambiguous_link_is_dropped_where_it_resolves_to_one_document_or_none() {
        assert!(kept_after(
            FindingKind::Ambiguous,
            Resolves::several(),
            Resolves::several()
        ));
        assert!(!kept_after(
            FindingKind::Ambiguous,
            Resolves::several(),
            one("a.md")
        ));
        assert!(!kept_after(
            FindingKind::Ambiguous,
            Resolves::several(),
            Resolves::none()
        ));
    }

    /// **A missing-anchor link is dropped where it resolves to none or to
    /// several documents, and stands where it resolves to the one document it
    /// named.**
    #[test]
    fn a_missing_anchor_link_is_dropped_where_it_no_longer_names_one_document() {
        assert!(!kept_after(
            FindingKind::MissingAnchor,
            one("a.md"),
            Resolves::none()
        ));
        assert!(!kept_after(
            FindingKind::MissingAnchor,
            one("a.md"),
            Resolves::several()
        ));
        assert!(kept_after(
            FindingKind::MissingAnchor,
            one("a.md"),
            one("a.md")
        ));
    }

    /// **The same document at its route's destination is the same document**,
    /// and repair never edits a heading, so the finding stands.
    #[test]
    fn a_missing_anchor_link_naming_a_routed_document_where_it_lands_stands() {
        let conditions = [changed(
            "h.md",
            LinkFamily::Wikilink,
            "a",
            one("loose/a.md"),
            one("tasks/a.md"),
        )];
        let operations = [route("loose/a.md", "tasks/a.md")];
        assert_eq!(
            kept(
                &[(1, FindingKind::MissingAnchor, "h.md", wikilink("a"))],
                &operations,
                &conditions
            ),
            vec![1]
        );
    }

    /// **Declared limit: a missing-anchor link that now names another single
    /// document stands**, since whether that document holds the anchor is the
    /// store's predicate and not judged here.
    #[test]
    fn a_missing_anchor_link_naming_a_different_single_document_stands() {
        assert!(kept_after(
            FindingKind::MissingAnchor,
            one("a.md"),
            one("b.md")
        ));
        // Not even a route elsewhere makes the destination the document it
        // named: only that document's own route does.
        let conditions = [changed(
            "h.md",
            LinkFamily::Wikilink,
            "a",
            one("a.md"),
            one("tasks/b.md"),
        )];
        let operations = [route("b.md", "tasks/b.md")];
        assert_eq!(
            kept(
                &[(1, FindingKind::MissingAnchor, "h.md", wikilink("a"))],
                &operations,
                &conditions
            ),
            vec![1]
        );
    }

    /// **A routed holder's links are keyed at its destination.**
    #[test]
    fn a_routed_holders_link_is_judged_at_the_holder_s_destination() {
        let conditions = [changed(
            "tasks/h.md",
            LinkFamily::Wikilink,
            "a",
            Resolves::none(),
            one("a.md"),
        )];
        let operations = [route("loose/h.md", "tasks/h.md")];
        let finding = [(1, FindingKind::Broken, "loose/h.md", wikilink("a"))];

        assert!(kept(&finding, &operations, &conditions).is_empty());
        // The entry is the destination's: a holder that stays is not it.
        let staying = [(1, FindingKind::Broken, "tasks/h.md", wikilink("a"))];
        assert!(kept(&staying, &[], &conditions).is_empty());
        let unrouted = [(1, FindingKind::Broken, "loose/h.md", wikilink("a"))];
        assert_eq!(kept(&unrouted, &[], &conditions), vec![1]);
    }

    /// **A link a cascade respells is keyed by its new address**, in the
    /// holder where the plan leaves it.
    #[test]
    fn a_respelled_link_is_judged_at_the_address_the_cascade_gives_it() {
        let mut moving = route("loose/a.md", "tasks/a.md");
        moving.cascade = vec![LinkRewrite::new(
            path("h.md"),
            LinkFamily::Wikilink,
            "loose/a",
            "tasks/a",
        )];
        let conditions = [changed(
            "h.md",
            LinkFamily::Wikilink,
            "tasks/a",
            one("loose/a.md"),
            Resolves::several(),
        )];
        let finding = [(1, FindingKind::MissingAnchor, "h.md", wikilink("loose/a"))];

        assert!(
            kept(&finding, &[moving], &conditions).is_empty(),
            "the respelled address is the entry's key"
        );
        assert_eq!(
            kept(&finding, &[], &conditions),
            vec![1],
            "the old address has no entry"
        );
    }

    /// **The syntax is part of the key**: a wikilink and a Markdown link of
    /// one address in one holder are judged apart, and each by its own entry.
    #[test]
    fn two_syntaxes_of_one_address_in_one_holder_are_judged_apart() {
        let conditions = [changed(
            "h.md",
            LinkFamily::Wikilink,
            "tasks/a.md",
            Resolves::none(),
            one("tasks/a.md"),
        )];
        let findings = [
            (1, FindingKind::Broken, "h.md", wikilink("tasks/a.md")),
            (2, FindingKind::Broken, "h.md", markdown("tasks/a.md")),
        ];

        assert_eq!(kept(&findings, &[], &conditions), vec![2]);
    }

    /// **Only link findings are judged**: a skipped finding of any other kind, and
    /// a link finding whose link the batch did not hand back, stand, as does
    /// the batch's order.
    #[test]
    fn a_finding_that_is_no_link_finding_stands_and_the_order_is_kept() {
        let rows = [
            row(7, FindingKind::Broken, "h.md"),
            row(3, FindingKind::UndeclaredTag, "h.md"),
            row(5, FindingKind::Broken, "h.md"),
            row(9, FindingKind::Broken, "h.md"),
        ];
        let links = BTreeMap::from([(3, wikilink("a")), (5, wikilink("a")), (7, wikilink("b"))]);
        let skipped = [7, 3, 5, 9]
            .into_iter()
            .map(|id| SkippedFinding::new(id, SkipReason::NoDeclaredFix))
            .collect();
        let conditions = [
            changed(
                "h.md",
                LinkFamily::Wikilink,
                "a",
                Resolves::none(),
                one("a.md"),
            ),
            changed(
                "h.md",
                LinkFamily::Wikilink,
                "b",
                Resolves::none(),
                Resolves::none(),
            ),
        ];

        let kept: Vec<u64> =
            skipped_still_held(skipped, &rows, &links, &Landing::of(&[], &conditions, &[]))
                .into_iter()
                .map(|skip| skip.finding)
                .collect();

        // 5 is dropped; 3 is no link finding; 9 has no link; 7 still stands.
        assert_eq!(kept, vec![7, 3, 9]);
    }

    /// A route whose cascade respells the wikilink `from` of `h.md` to `to`.
    fn respelling(from: &str, to: &str) -> Operation {
        let mut moving = route("loose/a.md", "tasks/a.md");
        moving.cascade = vec![LinkRewrite::new(
            path("h.md"),
            LinkFamily::Wikilink,
            from,
            to,
        )];
        moving
    }

    /// Whether the broken-link finding about the wikilink `old` of `h.md` is
    /// kept, where a cascade respells it to `new`, the forecast advises on
    /// `old` if `advised`, and the plan records `new_entry` under `new` and
    /// `old_entry` under `old`.
    fn kept_beside(
        advised: bool,
        new_entry: Option<Resolves>,
        old_entry: Option<Resolves>,
    ) -> bool {
        let entry = |address: &str, after: Option<Resolves>| {
            after.map(|after| {
                changed(
                    "h.md",
                    LinkFamily::Wikilink,
                    address,
                    Resolves::none(),
                    after,
                )
            })
        };
        let conditions: Vec<PlanCondition> = [entry("new", new_entry), entry("old", old_entry)]
            .into_iter()
            .flatten()
            .collect();
        let advisories = if advised {
            vec![LinkAdvisory::skipped_not_rewritable(LinkKey::new(
                path("h.md"),
                LinkFamily::Wikilink,
                "old",
            ))]
        } else {
            Vec::new()
        };
        let finding = [(1, FindingKind::Broken, "h.md", wikilink("old"))];
        !kept_advised(
            &finding,
            &[respelling("old", "new")],
            &conditions,
            &advisories,
        )
        .is_empty()
    }

    /// **A link no cascade respells is read under its own key**, whatever the
    /// forecast advises on and whatever is recorded under another address.
    #[test]
    fn a_link_no_cascade_respells_is_read_under_its_own_key() {
        let conditions = [
            changed(
                "h.md",
                LinkFamily::Wikilink,
                "a",
                Resolves::none(),
                one("a.md"),
            ),
            changed(
                "h.md",
                LinkFamily::Wikilink,
                "b",
                Resolves::none(),
                Resolves::none(),
            ),
        ];
        let advisories = [LinkAdvisory::skipped_not_rewritable(LinkKey::new(
            path("h.md"),
            LinkFamily::Wikilink,
            "a",
        ))];
        let finding = [(1, FindingKind::Broken, "h.md", wikilink("a"))];

        assert!(kept_advised(&finding, &[], &conditions, &advisories).is_empty());
    }

    /// **Where the forecast advises on no link of the old key, every link of
    /// it was respelled**, so the finding is read under the new address alone.
    #[test]
    fn a_respelled_link_the_forecast_does_not_advise_on_is_read_under_its_new_address() {
        // The old key's entry would keep the finding; it is not read.
        assert!(!kept_beside(
            false,
            Some(one("a.md")),
            Some(Resolves::none())
        ));
        assert!(kept_beside(
            false,
            Some(Resolves::none()),
            Some(one("a.md"))
        ));
    }

    /// **Where the forecast advises on a link of the old key, some links of
    /// it were left as written and which this is is not known**, so the
    /// finding is dropped only where the respelled and the as-written reading
    /// both drop it, and no entry under the old key is a link that stands.
    #[test]
    fn a_respelled_link_beside_one_left_as_written_is_dropped_only_where_both_readings_drop_it() {
        // Both readings drop it.
        assert!(!kept_beside(true, Some(one("a.md")), Some(one("b.md"))));
        // The respelled reading drops it, the as-written one keeps it.
        assert!(kept_beside(true, Some(one("a.md")), Some(Resolves::none())));
        // The as-written reading has no entry: that link resolves as it did.
        assert!(kept_beside(true, Some(one("a.md")), None));
        // The as-written reading drops it, the respelled one keeps it.
        assert!(kept_beside(true, Some(Resolves::none()), Some(one("a.md"))));
        // The respelled reading has no entry.
        assert!(kept_beside(true, None, Some(one("a.md"))));
    }
}
