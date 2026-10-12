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
//! after-state and exact, with an entry for every link whose resolution the
//! plan changes and no other, so a link with no entry is one whose resolution
//! the plan does not change, or one it removes, and its finding stands.
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
//! **Declared limit: a cascade rewrite is mapped for every link of its holder
//! with that syntax and address, though the batch may rewrite only some.** A
//! holder's rewrite batch can leave some links of that address as written;
//! they keep their own entry under the old address. A missing-anchor finding on
//! such a link is read under the new address, so it stands where its own entry
//! may say it resolves to none or to several. The error keeps a finding listed
//! and never drops one that holds.
//!
//! **Declared limit: a link finding about a link the plan removes stands.** The
//! change set has no entry for a link the after-state no longer holds, so a
//! declared `field/forbidden` fix with `remove` deletes a frontmatter field,
//! and a link finding on a wikilink that field held stays skipped though the
//! link is gone (NORN-389). It is the only fix that removes a link: a rename
//! keeps the link in its new field, and a closed set never holds a wikilink.

use std::collections::{BTreeMap, HashMap};

use norn_store::FindingLink;
use norn_wire::{
    DocumentPath, FindingKind, FindingRow, LinkFamily, LinkKey, Operation, OperationKind,
    PlanCondition, Resolves, SkippedFinding,
};

use crate::planner::links::wire_family;

/// What the plan changes about where links stand: the batch's routes, the
/// cascade's respellings and the resolution change set, each read off the
/// resolved plan once.
pub(crate) struct Landing {
    /// Where each routed document goes, by where it stood.
    routes: HashMap<DocumentPath, DocumentPath>,
    /// What a cascade respells a link's address to, by the holder where the
    /// plan leaves it, the link's syntax and the address it was written with.
    respelled: HashMap<(DocumentPath, LinkFamily, String), String>,
    /// What each link the plan changes resolved to before it and resolves to
    /// after it, by its key at the after-state.
    resolutions: HashMap<(DocumentPath, LinkFamily, String), (Resolves, Resolves)>,
}

impl Landing {
    /// What `operations`, a resolved plan's, route and respell, and what its
    /// `conditions` record the plan changes the resolution of.
    pub(crate) fn of(operations: &[Operation], conditions: &[PlanCondition]) -> Self {
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
        Landing {
            routes,
            respelled,
            resolutions,
        }
    }

    /// Where the document at `path` stands once the plan has landed.
    fn placed<'p>(&'p self, path: &'p DocumentPath) -> &'p DocumentPath {
        self.routes.get(path).unwrap_or(path)
    }

    /// The change the plan makes to the resolution of `link`, a link as the
    /// document at `holder` holds it before the plan: what it resolved to
    /// before and what it resolves to after, and `None` where the plan does
    /// not change it.
    fn change_of(
        &self,
        holder: &DocumentPath,
        link: &FindingLink,
    ) -> Option<&(Resolves, Resolves)> {
        let syntax = wire_family(link.family);
        let holder = self.placed(holder).clone();
        let address = self
            .respelled
            .get(&(holder.clone(), syntax, link.address.clone()))
            .cloned()
            .unwrap_or_else(|| link.address.clone());
        self.resolutions.get(&(holder, syntax, address))
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
/// `landing` describes has landed.
fn standing(row: &FindingRow, link: &FindingLink, landing: &Landing) -> bool {
    let Some((_, after)) = landing.change_of(&row.path, link) else {
        return true;
    };
    match row.kind {
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
    /// `conditions` describe.
    fn kept(
        of: &[(u64, FindingKind, &str, FindingLink)],
        operations: &[Operation],
        conditions: &[PlanCondition],
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
        skipped_still_held(skipped, &rows, &links, &Landing::of(operations, conditions))
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
            skipped_still_held(skipped, &rows, &links, &Landing::of(&[], &conditions))
                .into_iter()
                .map(|skip| skip.finding)
                .collect();

        // 5 is dropped; 3 is no link finding; 9 has no link; 7 still stands.
        assert_eq!(kept, vec![7, 3, 9]);
    }
}
