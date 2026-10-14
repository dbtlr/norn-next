//! A bare value made a link: the derived fix of a `field/type-mismatch` on a
//! field declared `link` whose offending element is a bare value, such as
//! `note` where `[[note]]` is meant. The same phase writes a list field's
//! unquoted wikilink elements whose value is exactly `[["s"]]` as `"[[s]]"`
//! ([`derived`]), without requiring resolution. A one-string list `["s"]`
//! and every other nested element have no such fix. An exposed link type
//! mismatch waits for this phase, whose answer or skip decides the shape fix.
//!
//! **A value qualifies only where `[[value]]` reads back as exactly one plain
//! wikilink.** The element is a non-empty string — and so is every element of
//! the field the finding names, so a YAML number, boolean or null is never
//! linked, nor a string sharing its text — with no surrounding white space and
//! no `#` fragment, `|` alias, `[` or `]`, or line break, and `[[value]]` is,
//! to the `link` reader ([`link_key`]) and the text layer, one wikilink of
//! that address and nothing else. A date is a string in norn's value model,
//! which has no date type (an unquoted `2026-10-11` reads as the string), so a
//! bare date naming one document, a daily note say, is linked like any other
//! string. Any other value has no declared fix: a repair never respells a
//! value to make it qualify.
//!
//! **The answer rests on the link's resolution, so the fix is decided last.**
//! A route of the batch moves its target, so the path a link resolves to, and
//! whether a path-qualified address still names a document, depend on the
//! batch's settled routes. A document's link fixes are therefore composed
//! after all its other fixes — at its route's destination where its route
//! stands — once every route of the batch is settled
//! ([`Reckoned::link_fixes`]): an exception to finding order. Each is
//! resolved over the plan the settled routes make. A plain link wrap or
//! unwrap admitted in finding order stays there; only a reshape needing a
//! bare-value answer waits for this phase and its respell veto. Every address
//! the combined write may hold joins the same one probe the
//! applier makes again of an address resolution
//! ([`crate::planner::links::resolved_addresses`]), every link the batch's
//! fixes write in one judgment of the index
//! ([`crate::planner::repair::Reading::namings`]).
//!
//! - One document: the element is written as `[[value]]`, a derived fix, and
//!   the plan records the address resolution the link has at its after-state:
//!   the holder where it stands then, the wikilink, the address written, and
//!   the one document's path then — as it records one for every link any fix
//!   writes ([`super::Decided::emit`]).
//! - Several documents: [`SkipReason::AmbiguousLink`], naming the documents
//!   the address could name.
//! - None: [`SkipReason::NoDeclaredFix`], the note saying the link would
//!   resolve to no document.
//!
//! A list's elements are written into one write of the field
//! ([`elements`]), and an element left standing never blocks its neighbours'.

use std::collections::BTreeSet;

use norn_config::schema::typed::link_key;
use norn_config::schema::{FieldType, Shape, VaultSchema};
use norn_wire::{
    AuthoredValue, Confidence, DocumentPath, FindingKind, FindingRow, Resolves, SkipReason,
    SkippedCandidates,
};

use super::elements::{self, Offending, Proposed, Replacement};
use super::{
    Composed, Document, Reckoned, Respellable, State, decided_skips, derived, field_of, respells,
    skip, written_links,
};
use crate::planner::repair::{Namings, Written, naming_of};

/// Whether `row` is a `field/type-mismatch` on a field declared `link`.
pub(super) fn is_link_mismatch(schema: &VaultSchema, row: &FindingRow) -> bool {
    row.kind == FindingKind::TypeMismatch
        && row
            .target
            .as_deref()
            .is_some_and(|field| schema.declared_type(field) == FieldType::Link)
}

/// The wikilink address `value` is, written as `[[value]]`, where that reads
/// back as exactly one plain wikilink of that address; `None` for any other
/// value.
///
/// **One round trip defines it**: the `link` reader's key of `[[value]]`
/// ([`link_key`]) is `[[value]]` itself — one wikilink, no embed, no alias,
/// nothing the parser trims or drops, which an empty value, surrounding white
/// space, a `|` or a `]` each break. What the round trip keeps is excluded
/// beside it: a `#` fragment, which the key holds though it names a place in
/// a document rather than a document, and an opening `[` or a line break,
/// which the parser reads past.
pub(super) fn bare_address(value: &str) -> Option<&str> {
    if value.contains(['#', '[', '\n', '\r']) {
        return None;
    }
    let written = format!("[[{value}]]");
    (link_key(&written).as_deref() == Some(written.as_str())).then_some(value)
}

/// What the document standing at `holder` makes of the offending element
/// `offending` of a link field, its address resolved as `namings` answers.
fn propose(
    holder: &DocumentPath,
    namings: &Namings,
    shape: Option<Shape>,
    offending: &Offending<'_>,
) -> Proposed {
    let row = offending.row;
    if shape == Some(Shape::List)
        && let Some(member) = derived::unquoted_link(offending.value)
    {
        return Proposed::Replace(derived::replacement(member));
    }
    let Some(address) = offending
        .string
        .then(|| bare_address(offending.text))
        .flatten()
    else {
        return Proposed::Skipped(Box::new(crate::planner::repair::skipped(row)));
    };
    let naming = naming_of(namings, &(holder.clone(), address.to_string()));
    match (&naming.resolves, &naming.candidates) {
        (Resolves::One { path }, _) => {
            let member = AuthoredValue::string(format!("[[{address}]]"));
            Proposed::Replace(Replacement {
                candidates: derived::candidate_of(&member),
                notes: vec![format!("`[[{address}]]` resolves to `{path}`")],
                member,
                level: Confidence::Derived,
            })
        }
        (Resolves::Several {}, candidates) => {
            let skipped = skip(row, SkipReason::AmbiguousLink);
            Proposed::Skipped(Box::new(match candidates {
                Some(head) => skipped.with_candidates(SkippedCandidates::documents(head.clone())),
                None => skipped,
            }))
        }
        (Resolves::None {}, _) => Proposed::Skipped(Box::new(
            skip(row, SkipReason::NoDeclaredFix)
                .with_note(format!("`[[{address}]]` would resolve to no document")),
        )),
    }
}

impl Reckoned<'_> {
    /// The selected `field/type-mismatch` findings on link fields, in
    /// finding order, each field's run of them once: the field, and the
    /// elements of its run the before-state holds a finding for.
    fn link_runs(&self) -> Vec<(&str, Vec<elements::Element<'_>>)> {
        let mut runs = Vec::new();
        let mut next = 0;
        while let Some(&at) = self.order.get(next) {
            next += 1;
            let row = &self.rows[at];
            let Some(field) = row
                .target
                .as_deref()
                .filter(|_| is_link_mismatch(self.document.schema(), row))
            else {
                continue;
            };
            let (elements, siblings) = self.field_run(next - 1);
            next += siblings;
            runs.push((field, elements));
        }
        runs
    }

    /// Add to `written` every wikilink the document's link fixes may write
    /// once composed: bare values, unquoted wikilinks and existing links,
    /// including every address a deferred shape fix's combined write may hold.
    pub(super) fn link_writes(&self, composed: &Composed, written: &mut BTreeSet<Written>) {
        let state = &composed.state;
        for &at in &composed.link_shapes {
            let row = &self.rows[at];
            let field = row
                .target
                .as_deref()
                .expect("a shape finding names a field");
            let shape = self
                .document
                .schema()
                .field(field)
                .and_then(|f| f.shape())
                .expect("a shape finding names a declared shape");
            if let Some(Some(value)) = field_of(&state.bytes, field)
                && let Some(value) = derived::reshaped(shape, &value)
            {
                self.element_writes(state, field, &value, written);
            }
        }
        for (field, elements) in self.link_runs() {
            for element in elements {
                if let Some(address) = element.finding.value.as_deref().and_then(bare_address) {
                    written.insert((state.at.clone(), address.to_string()));
                }
            }
            if let Some(Some(value)) = field_of(&state.bytes, field) {
                self.element_writes(state, field, &value, written);
            }
        }
    }

    /// Every address a link field's candidate write may hold, including
    /// its existing links, unquoted wikilinks and qualifying bare values.
    fn element_writes(
        &self,
        state: &State,
        field: &str,
        value: &AuthoredValue,
        written: &mut BTreeSet<Written>,
    ) {
        written.extend(written_links(&self.document.set(
            &state.at,
            field,
            value.clone(),
        )));
        let elements = match value {
            AuthoredValue::List(items) => items.as_slice(),
            scalar => std::slice::from_ref(scalar),
        };
        for element in elements {
            let unquoted = (self.document.schema().field(field).and_then(|f| f.shape())
                == Some(Shape::List))
            .then(|| derived::unquoted_link(element))
            .flatten();
            let link = unquoted.or_else(|| {
                let AuthoredValue::String(text) = element else {
                    return None;
                };
                bare_address(text).map(|address| AuthoredValue::string(format!("[[{address}]]")))
            });
            if let Some(link) = link {
                written.extend(written_links(&self.document.set(&state.at, field, link)));
            }
        }
    }

    /// Compose the document's link fixes onto `composed`, last, as
    /// `composing` — the document at a threshold — makes them: its selected
    /// `field/type-mismatch` findings on link fields, in finding order, each
    /// field's elements into one write of it, and the deferred shape fixes
    /// composed with their exposed link answers, each address resolved as
    /// `namings` answers, where the batch's settled routes leave it.
    ///
    /// **A link fix never brings its holder under a rule reading by value a
    /// link a settled route may respell** (`respellable`): the respell check
    /// judged the holder's states without its link fixes, which compose after
    /// it, so such a fix is skipped as one the judge would refuse — the plan
    /// it would make is one the applier refuses — and never the route.
    pub(super) fn link_fixes(
        &self,
        composing: &Document<'_>,
        composed: &mut Composed,
        namings: &Namings,
        respellable: Option<&Respellable>,
    ) {
        let repairing = composing.repairing;
        let vetoed = |state: &State| {
            let (field, (from, to)) =
                respells::reads_a_respellable_link(state, respellable?, repairing)?;
            Some(format!(
                "the link brings `{}` under a rule reading `{field}` by value, whose link the \
                 move of `{from}` to `{to}` would respell",
                state.at
            ))
        };
        for &at in &composed.link_shapes.clone() {
            let row = &self.rows[at];
            let Some(finding) = self.found[at].as_ref() else {
                continue;
            };
            if composed.state.holding(finding).is_none() {
                continue;
            }
            let field = row
                .target
                .as_deref()
                .expect("a shape finding names a field");
            let holder = composed.state.at.clone();
            let fixed = derived::reshape(
                composing,
                &composed.state,
                row,
                finding,
                field,
                Some(&mut |offending| {
                    propose(
                        &holder,
                        namings,
                        composing.schema().field(field).and_then(|f| f.shape()),
                        offending,
                    )
                }),
                &vetoed,
            );
            match fixed {
                Ok(derived::Reshaping::Fixed(fix)) => {
                    composed.state = fix.made(&mut composed.made);
                }
                Err(skip) => composed.decided[at] = Some(*skip),
                Ok(derived::Reshaping::NeedsLinks) => {
                    unreachable!("the link phase supplies its answers")
                }
            }
        }
        for (field, mut elements) in self.link_runs() {
            // A whole unquoted wikilink is decided in finding order; its
            // skip stands rather than being decided again here.
            elements.retain(|element| composed.decided[element.at].is_none());
            let holder = composed.state.at.clone();
            let mapped = elements::fix_field(
                composing,
                &composed.state,
                field,
                &elements,
                &mut |offending| {
                    propose(
                        &holder,
                        namings,
                        composing.schema().field(field).and_then(|f| f.shape()),
                        offending,
                    )
                },
                &vetoed,
            );
            decided_skips(&mut composed.decided, mapped.skipped);
            if let Some(fix) = mapped.fix {
                composed.state = fix.made(&mut composed.made);
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Harness scaffolding: the tree each case plans over.
pub(super) mod tests {
    use norn_config::schema::{LocalTimestamp, NotALocalTimestamp};
    use norn_testkit::scratch::Scratch;
    use norn_wire::{
        AuthoredValue, CandidateHead, Confidence, DocumentPath, FindingRow, LinkFamily, LinkKey,
        OperationKind, PlanCondition, Resolves, RuleSet, Severity, SkipReason, SkippedCandidates,
        WriteTarget,
    };

    use super::*;
    use crate::clock::OneReading;
    use crate::derivation::Declared;
    use crate::planner::control::SchemaPlace;
    use crate::planner::links::testing::TreeStore;
    use crate::planner::repair::{Over, Planned, Reading, Repairing, plan};
    use crate::planner::view::TreeView;

    /// The rule set every misplaced row cites.
    const RULE_SET: u64 = 1;

    /// The finding `id`: `value`, an element of `field` of `at`, is no link.
    fn mismatch(id: u64, at: &str, field: &str, value: &str) -> FindingRow {
        FindingRow::new(
            id,
            FindingKind::TypeMismatch,
            Severity::Warning,
            DocumentPath::new(at).expect("a document path"),
            Some(field.to_string()),
            None,
            CandidateHead::new([], 0).expect("an empty head"),
            None,
            "a finding",
            1,
        )
        .with_value(norn_store::value_head(value))
    }

    /// The finding `id`: the document at `at` is misplaced.
    fn misplaced(id: u64, at: &str) -> FindingRow {
        FindingRow::new(
            id,
            FindingKind::Misplaced,
            Severity::Warning,
            DocumentPath::new(at).expect("a document path"),
            None,
            None,
            CandidateHead::new([], 0).expect("an empty head"),
            None,
            "a finding",
            1,
        )
        .citing(RULE_SET)
    }

    /// `rows` planned over `documents` written to a tree on disk, whose links
    /// a store derived, under `schema` at `threshold`.
    pub(in crate::planner::repair::declared) fn planned_at(
        schema: &str,
        documents: &[(&str, &str)],
        rows: &[FindingRow],
        threshold: Confidence,
    ) -> Planned {
        planned_reading(schema, documents, rows, threshold, false)
    }

    /// The vault of [`planned_reading`] read by a repair whose index answers
    /// the batch's links with none: what a broken index would hand back.
    struct Unanswering<'r, R>(&'r R);

    impl<R: Reading> Reading for Unanswering<'_, R> {
        type Error = R::Error;

        fn tombstones(
            &self,
            link: &norn_wire::LinkKey,
        ) -> Result<Vec<norn_store::MovedTombstone>, Self::Error> {
            self.0.tombstones(link)
        }
        fn live_holders(&self, hash: &str) -> Result<Vec<norn_wire::DocumentPath>, Self::Error> {
            self.0.live_holders(hash)
        }
        fn resolutions(
            &self,
            overlay: &norn_store::PathOverlay,
            links: &[norn_wire::LinkKey],
        ) -> Result<
            std::collections::BTreeMap<crate::planner::links::EntryKey, norn_wire::Resolves>,
            Self::Error,
        > {
            self.0.resolutions(overlay, links)
        }

        fn before(&self, path: &DocumentPath) -> Result<crate::planner::repair::Before, R::Error> {
            self.0.before(path)
        }

        fn destination(
            &self,
            path: &DocumentPath,
        ) -> Result<crate::planner::repair::Destination, R::Error> {
            self.0.destination(path)
        }

        fn place(&self, path: &DocumentPath) -> Option<norn_fs::NormalizedPath> {
            self.0.place(path)
        }

        fn links(
            &self,
            overlay: &norn_store::PathOverlay,
            probed: &[norn_store::ProbedLink],
            each: &mut dyn FnMut(norn_store::LinkChange),
        ) -> Result<(), R::Error> {
            self.0.links(overlay, probed, each)
        }

        fn namings(
            &self,
            _: &norn_store::PathOverlay,
            _: &std::collections::BTreeSet<crate::planner::repair::Written>,
        ) -> Result<Namings, R::Error> {
            Ok(Namings::new())
        }
    }

    /// [`planned_at`], over a vault whose index answers no link where
    /// `unanswering`.
    fn planned_reading(
        schema: &str,
        documents: &[(&str, &str)],
        rows: &[FindingRow],
        threshold: Confidence,
        unanswering: bool,
    ) -> Planned {
        let scratch = Scratch::new("repair-bare-link");
        for (path, text) in documents {
            let at = scratch.join(path);
            std::fs::create_dir_all(at.parent().expect("a parent")).expect("a folder");
            std::fs::write(at, text).expect("a document");
        }
        let declared = Declared::pinned(
            norn_config::schema::VaultSchema::parse(schema.as_bytes()).expect("a schema"),
            "a fingerprint",
        );
        let names: Vec<String> = declared
            .schema()
            .rules()
            .map(|rule| rule.name().to_string())
            .collect();
        let rule_sets: Vec<RuleSet> = RuleSet::new(RULE_SET, names).into_iter().collect();
        let clock = || -> Result<LocalTimestamp, NotALocalTimestamp> {
            panic!("no fix here reads the clock")
        };
        let one = OneReading::of(&clock);
        let repairing = Repairing {
            declared: &declared,
            case: norn_wire::CaseFold::Exact,
            clock: &one,
            rule_sets: &rule_sets,
            threshold,
        };
        let view = TreeView::open(scratch.root(), &[], &SchemaPlace::default()).expect("a view");
        let store = TreeStore::over(scratch.root());
        let index = store.index();
        let reading = Over {
            view: &view,
            index: &index,
        };
        let planned = if unanswering {
            plan(
                rows,
                &std::collections::BTreeMap::new(),
                &repairing,
                &Unanswering(&reading),
            )
        } else {
            plan(
                rows,
                &std::collections::BTreeMap::new(),
                &repairing,
                &reading,
            )
        };
        match planned {
            Ok(planned) => planned,
            Err(_) => panic!("the tree reads"),
        }
    }

    /// [`planned_at`] the default threshold.
    fn planned(schema: &str, documents: &[(&str, &str)], rows: &[FindingRow]) -> Planned {
        planned_at(schema, documents, rows, Confidence::DEFAULT_THRESHOLD)
    }

    /// **An unquoted wikilink in a single link field writes its spelled link and resolution.**
    #[test]
    fn an_unquoted_wikilink_in_a_single_link_field_is_written_with_its_condition() {
        let row = mismatch(1, "a.md", "up", "[[\"zz-b\"]]");
        let mut row = row;
        row.kind = FindingKind::ShapeMismatch;
        let planned = planned(
            "version: 1\nfields:\n  up: {type: link, shape: single}\n",
            &[("a.md", "---\nup: [[zz-b]]\n---\n"), ("zz-b.md", "# B\n")],
            &[row],
        );
        assert_eq!(
            kinds(&planned),
            vec![setting("a.md", "up", AuthoredValue::string("[[zz-b]]"))]
        );
        assert_eq!(
            planned.conditions,
            vec![resolving("a.md", "zz-b", "zz-b.md")]
        );
        assert_eq!(
            planned.citations[0].findings[0].confidence,
            Confidence::Derived
        );
        assert!(planned.skipped.is_empty());
    }

    /// **An unquoted wikilink is written even where its address resolves to none.**
    #[test]
    fn an_unquoted_wikilink_resolving_to_none_is_written_with_its_condition() {
        let planned = planned(
            LINKED,
            &[("a.md", "---\nup: [[ghost]]\n---\n")],
            &[mismatch(1, "a.md", "up", "[\"ghost\"]")],
        );
        assert_eq!(
            kinds(&planned),
            vec![setting("a.md", "up", AuthoredValue::string("[[ghost]]"))]
        );
        assert_eq!(
            planned.conditions,
            vec![PlanCondition::address_resolution(
                LinkKey::new(
                    DocumentPath::new("a.md").expect("a path"),
                    LinkFamily::Wikilink,
                    "ghost"
                ),
                Resolves::none(),
            )]
        );
        assert!(planned.skipped.is_empty());
    }

    /// **An unquoted wikilink on an unshaped field below the threshold keeps
    /// its proposed operation.** The link phase leaves the earlier decision alone.
    #[test]
    fn an_unquoted_wikilink_on_an_unshaped_field_below_the_threshold_keeps_its_proposed_operation()
    {
        let planned = planned_at(
            LINKED,
            &[("a.md", "---\nup: [[zz-b]]\n---\n"), ("zz-b.md", "# B\n")],
            &[mismatch(1, "a.md", "up", "[\"zz-b\"]")],
            Confidence::Declared,
        );

        assert!(planned.operations.is_empty());
        assert!(planned.conditions.is_empty());
        assert_eq!(reasons(&planned), vec![(1, SkipReason::BelowThreshold)]);
        let proposed = planned.skipped[0].proposed.as_ref().expect("a proposal");
        assert_eq!(
            proposed.kind,
            setting("a.md", "up", AuthoredValue::string("[[zz-b]]"))
        );
    }

    /// **An unquoted wikilink on an unshaped field the judge refuses keeps
    /// its candidate.** The link phase leaves the earlier refusal alone.
    #[test]
    fn an_unquoted_wikilink_on_an_unshaped_field_the_judge_refuses_keeps_its_candidate() {
        let schema = "version: 1\nfields:\n  up: {type: link}\nrules:\n  linked:\n    match: {frontmatter: {up: '[[zz-b]]'}}\n    forbidden:\n      draft:\n";
        let planned = planned(
            schema,
            &[
                ("a.md", "---\nup: [[zz-b]]\ndraft: x\n---\n"),
                ("zz-b.md", "# B\n"),
            ],
            &[mismatch(1, "a.md", "up", "[\"zz-b\"]")],
        );

        assert!(planned.operations.is_empty());
        assert!(planned.conditions.is_empty());
        assert_eq!(reasons(&planned), vec![(1, SkipReason::JudgeWouldRefuse)]);
        assert_eq!(
            planned.skipped[0].candidates,
            Some(derived::candidate_of(&AuthoredValue::string("[[zz-b]]")))
        );
    }

    /// **Flow list elements spelling unquoted wikilinks are written as their
    /// links in one field write**, with both address resolutions recorded.
    #[test]
    fn flow_unquoted_wikilink_elements_are_written_as_links_in_one_write() {
        let element = |text| {
            AuthoredValue::List(vec![AuthoredValue::List(vec![AuthoredValue::string(text)])])
        };
        let planned = planned(
            LINKED,
            &[
                ("h.md", "---\nups: [[[a]], [[b]]]\n---\n"),
                ("a.md", "# A\n"),
                ("b.md", "# B\n"),
            ],
            &[
                mismatch(
                    1,
                    "h.md",
                    "ups",
                    &crate::derivation::stored_spelling(&element("a")).expect("a spelling"),
                ),
                mismatch(
                    2,
                    "h.md",
                    "ups",
                    &crate::derivation::stored_spelling(&element("b")).expect("a spelling"),
                ),
            ],
        );

        assert_eq!(
            kinds(&planned),
            vec![setting(
                "h.md",
                "ups",
                AuthoredValue::List(vec![
                    AuthoredValue::string("[[a]]"),
                    AuthoredValue::string("[[b]]"),
                ])
            )]
        );
        assert_eq!(
            planned.conditions,
            vec![
                resolving("h.md", "a", "a.md"),
                resolving("h.md", "b", "b.md")
            ]
        );
        assert!(planned.skipped.is_empty());
    }

    /// **One-string list elements have no unquoted-wikilink fix**, leaving
    /// each selected finding skipped without a write or a condition.
    #[test]
    fn single_bracket_elements_in_a_list_link_field_have_no_declared_fix() {
        let element = |text| AuthoredValue::List(vec![AuthoredValue::string(text)]);
        let planned = planned(
            LINKED,
            &[
                ("h.md", "---\nups: [[a], [b]]\n---\n"),
                ("a.md", "# A\n"),
                ("b.md", "# B\n"),
            ],
            &[
                mismatch(
                    1,
                    "h.md",
                    "ups",
                    &crate::derivation::stored_spelling(&element("a")).expect("a spelling"),
                ),
                mismatch(
                    2,
                    "h.md",
                    "ups",
                    &crate::derivation::stored_spelling(&element("b")).expect("a spelling"),
                ),
            ],
        );

        assert!(planned.operations.is_empty());
        assert!(planned.conditions.is_empty());
        assert_eq!(
            reasons(&planned),
            vec![
                (1, SkipReason::NoDeclaredFix),
                (2, SkipReason::NoDeclaredFix)
            ]
        );
    }

    /// **A nested list holding several strings spells no single wikilink.**
    #[test]
    fn a_nested_list_holding_several_strings_has_no_declared_link_fix() {
        let planned = planned(
            LINKED,
            &[("a.md", "---\nup: [[a, b]]\n---\n")],
            &[mismatch(1, "a.md", "up", "[\"a\",\"b\"]")],
        );
        assert!(planned.operations.is_empty());
        assert!(planned.conditions.is_empty());
        assert_eq!(reasons(&planned), vec![(1, SkipReason::NoDeclaredFix)]);
    }

    /// **An unquoted wikilink on a text field has no derived link fix.**
    #[test]
    fn an_unquoted_wikilink_on_a_text_field_is_left_skipped() {
        let planned = planned(
            "version: 1\nfields:\n  up: {type: text}\n",
            &[("a.md", "---\nup: [[x]]\n---\n")],
            &[mismatch(1, "a.md", "up", "[\"x\"]")],
        );
        assert!(planned.operations.is_empty());
        assert!(planned.conditions.is_empty());
        assert_eq!(reasons(&planned), vec![(1, SkipReason::NoDeclaredFix)]);
    }

    const LINKED: &str =
        "version: 1\nfields:\n  up: {type: link}\n  ups: {type: link, shape: list}\n";

    /// The condition recording that `address`, written in `holder`, resolves
    /// to the document at `target`.
    fn resolving(holder: &str, address: &str, target: &str) -> PlanCondition {
        PlanCondition::address_resolution(
            LinkKey::new(
                DocumentPath::new(holder).expect("a path"),
                LinkFamily::Wikilink,
                address,
            ),
            Resolves::one(DocumentPath::new(target).expect("a path")),
        )
    }

    /// The operation setting `field` of `at` to the string `value`.
    fn setting(at: &str, field: &str, value: AuthoredValue) -> OperationKind {
        OperationKind::set_frontmatter(
            WriteTarget::path(DocumentPath::new(at).expect("a path")),
            field,
            value,
        )
    }

    fn kinds(planned: &Planned) -> Vec<OperationKind> {
        planned
            .operations
            .iter()
            .map(|operation| operation.kind.clone())
            .collect()
    }

    fn reasons(planned: &Planned) -> Vec<(u64, SkipReason)> {
        planned
            .skipped
            .iter()
            .map(|skipped| (skipped.finding, skipped.reason))
            .collect()
    }

    /// **A bare value resolving to exactly one document is written as a link
    /// at the derived level, and the plan records the resolution of the
    /// address it wrote** from the holder where it stands.
    #[test]
    fn a_bare_value_resolving_to_one_document_is_written_as_a_link_with_its_condition() {
        let planned = planned(
            LINKED,
            &[("a.md", "---\nup: b\n---\n# A\n"), ("b.md", "# B\n")],
            &[mismatch(1, "a.md", "up", "b")],
        );

        assert_eq!(
            kinds(&planned),
            vec![setting("a.md", "up", AuthoredValue::string("[[b]]"))]
        );
        assert_eq!(planned.conditions, vec![resolving("a.md", "b", "b.md")]);
        let [citation] = planned.citations.as_slice() else {
            panic!("one citation: {:?}", planned.citations);
        };
        let [cited] = citation.findings.as_slice() else {
            panic!("one finding: {citation:?}");
        };
        assert_eq!((cited.finding, cited.confidence), (1, Confidence::Derived));
        assert!(planned.skipped.is_empty(), "{:?}", planned.skipped);
    }

    /// **A list's element is written as a link into the one write of the
    /// field, and a neighbour that resolves to nothing stays reported.**
    #[test]
    fn a_list_element_resolving_uniquely_is_written_and_a_neighbour_resolving_to_nothing_stays() {
        let planned = planned(
            LINKED,
            &[
                ("a.md", "---\nups: [b, ghost]\n---\n# A\n"),
                ("b.md", "# B\n"),
            ],
            &[
                mismatch(1, "a.md", "ups", "b"),
                mismatch(2, "a.md", "ups", "ghost"),
            ],
        );

        assert_eq!(
            kinds(&planned),
            vec![setting(
                "a.md",
                "ups",
                AuthoredValue::List(vec![
                    AuthoredValue::string("[[b]]"),
                    AuthoredValue::string("ghost")
                ])
            )]
        );
        assert_eq!(planned.conditions, vec![resolving("a.md", "b", "b.md")]);
        assert_eq!(reasons(&planned), vec![(2, SkipReason::NoDeclaredFix)]);
        let note = planned.skipped[0].note.as_deref().unwrap_or_default();
        assert!(note.contains("no document"), "{note}");
    }

    /// **A value naming several documents is skipped as an ambiguous link,
    /// naming the documents it could name**; nothing is written or recorded.
    #[test]
    fn a_value_naming_several_documents_skips_as_ambiguous_with_its_candidates() {
        let planned = planned(
            LINKED,
            &[
                ("a.md", "---\nup: b\n---\n# A\n"),
                ("x/b.md", "# X\n"),
                ("y/b.md", "# Y\n"),
            ],
            &[mismatch(1, "a.md", "up", "b")],
        );

        assert!(planned.operations.is_empty());
        assert!(planned.conditions.is_empty());
        assert_eq!(reasons(&planned), vec![(1, SkipReason::AmbiguousLink)]);
        let Some(SkippedCandidates::Documents { head, .. }) = &planned.skipped[0].candidates else {
            panic!("candidates: {:?}", planned.skipped);
        };
        let mut named: Vec<&str> = head
            .candidates()
            .iter()
            .map(|candidate| candidate.path.as_str())
            .collect();
        named.sort_unstable();
        assert_eq!(named, vec!["x/b.md", "y/b.md"]);
    }

    /// **A value resolving to no document has no declared fix**, and the note
    /// says the link would resolve to none.
    #[test]
    fn a_value_resolving_to_no_document_has_no_declared_fix() {
        let planned = planned(
            LINKED,
            &[("a.md", "---\nup: ghost\n---\n# A\n")],
            &[mismatch(1, "a.md", "up", "ghost")],
        );

        assert!(planned.operations.is_empty());
        assert!(planned.conditions.is_empty());
        assert_eq!(reasons(&planned), vec![(1, SkipReason::NoDeclaredFix)]);
    }

    /// **Each spelling qualifies or not by the one round trip and the
    /// exclusions beside it**: a fragment, an opening bracket and a line
    /// break are excluded though the key would keep them; an alias, a
    /// closing bracket, surrounding white space and nothing at all fail the
    /// round trip; a path, a protocol, an extension, inner white space and
    /// any script qualify.
    #[test]
    fn a_spelling_qualifies_by_the_round_trip_and_the_exclusions_beside_it() {
        for value in [
            "b#h", "b#^x", "a[b", "[b]", "a\nb", "a\rb", "b|c", "b]", "a]b", " b", "b ", "",
        ] {
            assert_eq!(bare_address(value), None, "{value:?}");
        }
        for value in ["b", "a/b", "vault://b", "a.md", "b c", "日本"] {
            assert_eq!(bare_address(value), Some(value), "{value:?}");
        }
    }

    /// **A value that does not read back as exactly one plain wikilink is
    /// never made a link**: a fragment, an alias, a bracket, surrounding space
    /// and nothing at all, and a number that is no string, are left with no
    /// declared fix though a document of the same name stands.
    #[test]
    fn a_value_that_is_no_plain_wikilink_stays_with_no_declared_fix() {
        let schema = "version: 1\nfields:\n  f1: {type: link}\n  f2: {type: link}\n  f3: {type: link}\n  f4: {type: link}\n  f5: {type: link}\n  f6: {type: link}\n";
        let planned = planned(
            schema,
            &[
                (
                    "a.md",
                    "---\nf1: \"b#h\"\nf2: \"b|c\"\nf3: \"[b]\"\nf4: \" b\"\nf5: \"\"\nf6: 3\n---\n",
                ),
                ("b.md", "# B\n"),
                ("c.md", "# C\n"),
                ("3.md", "# 3\n"),
            ],
            &[
                mismatch(1, "a.md", "f1", "b#h"),
                mismatch(2, "a.md", "f2", "b|c"),
                mismatch(3, "a.md", "f3", "[b]"),
                mismatch(4, "a.md", "f4", " b"),
                mismatch(5, "a.md", "f5", ""),
                mismatch(6, "a.md", "f6", "3"),
            ],
        );

        assert!(planned.operations.is_empty(), "{:?}", planned.operations);
        assert!(planned.conditions.is_empty());
        assert_eq!(
            reasons(&planned),
            (1..=6)
                .map(|id| (id, SkipReason::NoDeclaredFix))
                .collect::<Vec<_>>()
        );
    }

    /// **Only a string is made a link**: a YAML number, boolean or null
    /// never is, and a string sharing its text with one is left too, since
    /// the finding names them both. A date is a string in the value model, so
    /// an unquoted `2026-10-11` is made a link, as a daily note is named.
    #[test]
    fn only_a_string_is_made_a_link_and_one_sharing_its_text_with_a_scalar_stays() {
        let planned = planned(
            LINKED,
            &[
                (
                    "a.md",
                    "---\nups: [3, '3', true, 'true', null, 2026-10-11]\n---\n",
                ),
                ("3.md", "# 3\n"),
                ("true.md", "# True\n"),
                ("2026-10-11.md", "# Day\n"),
            ],
            &[
                mismatch(1, "a.md", "ups", "2026-10-11"),
                mismatch(2, "a.md", "ups", "3"),
                mismatch(3, "a.md", "ups", "true"),
            ],
        );

        assert_eq!(
            kinds(&planned),
            vec![setting(
                "a.md",
                "ups",
                AuthoredValue::List(vec![
                    AuthoredValue::Integer(3),
                    AuthoredValue::string("3"),
                    AuthoredValue::Bool(true),
                    AuthoredValue::string("true"),
                    AuthoredValue::Null,
                    AuthoredValue::string("[[2026-10-11]]"),
                ])
            )]
        );
        assert_eq!(
            reasons(&planned),
            vec![
                (2, SkipReason::NoDeclaredFix),
                (3, SkipReason::NoDeclaredFix)
            ]
        );
    }

    /// **A link fix under a threshold that does not admit the derived level is
    /// skipped as below it, carrying the operation the plan would have held
    /// and recording no condition.**
    #[test]
    fn a_link_fix_below_the_threshold_skips_with_its_proposed_operation_and_records_nothing() {
        let planned = planned_at(
            LINKED,
            &[("a.md", "---\nup: b\n---\n# A\n"), ("b.md", "# B\n")],
            &[mismatch(1, "a.md", "up", "b")],
            Confidence::Declared,
        );

        assert!(planned.operations.is_empty());
        assert!(planned.conditions.is_empty());
        assert_eq!(reasons(&planned), vec![(1, SkipReason::BelowThreshold)]);
        let proposed = planned.skipped[0].proposed.as_ref().expect("a proposal");
        assert_eq!(
            proposed.kind,
            setting("a.md", "up", AuthoredValue::string("[[b]]"))
        );
    }

    /// **Each below-threshold link fix of a list proposes the one write of
    /// the field lowering the threshold makes**, both elements linked.
    #[test]
    fn below_threshold_link_fixes_of_a_list_each_propose_the_write_lowering_makes() {
        let documents = [
            ("a.md", "---\nups: [b, c]\n---\n"),
            ("b.md", "# B\n"),
            ("c.md", "# C\n"),
        ];
        let rows = [
            mismatch(1, "a.md", "ups", "b"),
            mismatch(2, "a.md", "ups", "c"),
        ];

        let lowered = planned(LINKED, &documents, &rows);
        let strict = planned_at(LINKED, &documents, &rows, Confidence::Declared);

        let written = setting(
            "a.md",
            "ups",
            AuthoredValue::List(vec![
                AuthoredValue::string("[[b]]"),
                AuthoredValue::string("[[c]]"),
            ]),
        );
        assert_eq!(kinds(&lowered), vec![written.clone()]);
        let proposed: Vec<Option<OperationKind>> = strict
            .skipped
            .iter()
            .map(|skipped| {
                skipped
                    .proposed
                    .as_ref()
                    .map(|operation| operation.kind.clone())
            })
            .collect();
        assert_eq!(proposed, vec![Some(written.clone()), Some(written)]);
        assert!(strict.conditions.is_empty(), "{:?}", strict.conditions);
    }

    const ROUTED: &str = "version: 1\nfields:\n  up: {type: link}\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    allowed_paths: {paths: ['tasks/**'], route: 'tasks/'}\n";

    /// **A same-batch route moving the target is what the link resolves
    /// to**: the condition records the path the target lands at.
    #[test]
    fn a_same_batch_route_moving_the_target_records_the_moved_path() {
        let planned = planned(
            ROUTED,
            &[
                ("a.md", "---\nup: b\n---\n# A\n"),
                ("loose/b.md", "---\ntype: task\n---\n# B\n"),
            ],
            &[mismatch(1, "a.md", "up", "b"), misplaced(2, "loose/b.md")],
        );

        assert_eq!(
            planned.conditions,
            vec![resolving("a.md", "b", "tasks/b.md")]
        );
        assert!(
            kinds(&planned).contains(&setting("a.md", "up", AuthoredValue::string("[[b]]"))),
            "{:?}",
            planned.operations
        );
        assert!(planned.skipped.is_empty(), "{:?}", planned.skipped);
    }

    /// **A holder a route moves is recorded and written where it lands.**
    #[test]
    fn a_holder_a_route_moves_is_written_and_recorded_at_its_destination() {
        let planned = planned(
            ROUTED,
            &[
                ("loose/a.md", "---\ntype: task\nup: b\n---\n# A\n"),
                ("b.md", "# B\n"),
            ],
            &[
                mismatch(1, "loose/a.md", "up", "b"),
                misplaced(2, "loose/a.md"),
            ],
        );

        assert_eq!(
            planned.conditions,
            vec![resolving("tasks/a.md", "b", "b.md")]
        );
        assert!(
            kinds(&planned).contains(&setting("tasks/a.md", "up", AuthoredValue::string("[[b]]"))),
            "{:?}",
            planned.operations
        );
    }

    /// **A path-qualified value a same-batch route breaks is not written**:
    /// `loose/b` names the document before the route moves it and none after.
    #[test]
    fn a_path_qualified_value_a_same_batch_route_breaks_is_skipped() {
        let documents = [
            ("a.md", "---\nup: loose/b\n---\n# A\n"),
            ("loose/b.md", "---\ntype: task\n---\n# B\n"),
        ];

        let unrouted = planned(ROUTED, &documents, &[mismatch(1, "a.md", "up", "loose/b")]);
        assert_eq!(
            unrouted.conditions,
            vec![resolving("a.md", "loose/b", "loose/b.md")]
        );

        let routed = planned(
            ROUTED,
            &documents,
            &[
                mismatch(1, "a.md", "up", "loose/b"),
                misplaced(2, "loose/b.md"),
            ],
        );
        assert!(routed.conditions.is_empty(), "{:?}", routed.conditions);
        assert_eq!(reasons(&routed), vec![(1, SkipReason::NoDeclaredFix)]);
    }

    /// **A link fix the judge refuses on the composed state skips as one it
    /// would refuse**: `[[b]]` brings the document under a rule forbidding
    /// the `scratch` it holds. Nothing is recorded.
    #[test]
    fn a_link_fix_the_judge_refuses_skips_as_judge_would_refuse() {
        let schema = "version: 1\nfields:\n  up: {type: link}\nrules:\n  linked:\n    match: {frontmatter: {up: '[[b]]'}}\n    forbidden:\n      scratch:\n";
        let planned = planned(
            schema,
            &[
                ("a.md", "---\nup: b\nscratch: x\n---\n# A\n"),
                ("b.md", "# B\n"),
            ],
            &[mismatch(1, "a.md", "up", "b")],
        );

        assert!(planned.operations.is_empty());
        assert!(planned.conditions.is_empty());
        assert_eq!(reasons(&planned), vec![(1, SkipReason::JudgeWouldRefuse)]);
    }

    /// The finding `id` of `kind` on `field` of `at`, judging `value`.
    pub(in crate::planner::repair::declared) fn finding(
        id: u64,
        kind: FindingKind,
        at: &str,
        field: &str,
        value: &str,
    ) -> FindingRow {
        FindingRow::new(
            id,
            kind,
            Severity::Warning,
            DocumentPath::new(at).expect("a document path"),
            Some(field.to_string()),
            None,
            CandidateHead::new([], 0).expect("an empty head"),
            None,
            "a finding",
            1,
        )
        .with_value(norn_store::value_head(value))
    }

    /// **A fix writing no link records no condition**: a number wrapped
    /// beside a link fix leaves the link's alone.
    #[test]
    fn a_fix_writing_no_link_records_no_condition() {
        let schema =
            "version: 1\nfields:\n  up: {type: link}\n  ranks: {type: number, shape: list}\n";
        let planned = planned(
            schema,
            &[("a.md", "---\nup: b\nranks: 3\n---\n"), ("b.md", "# B\n")],
            &[
                mismatch(1, "a.md", "up", "b"),
                finding(2, FindingKind::ShapeMismatch, "a.md", "ranks", "3"),
            ],
        );

        assert_eq!(planned.operations.len(), 2, "{:?}", planned.operations);
        assert_eq!(planned.conditions, vec![resolving("a.md", "b", "b.md")]);
    }

    /// The condition recording that `address`, written in `holder`, resolves
    /// as `after` says.
    fn recording(holder: &str, address: &str, after: Resolves) -> PlanCondition {
        PlanCondition::address_resolution(
            LinkKey::new(
                DocumentPath::new(holder).expect("a path"),
                LinkFamily::Wikilink,
                address,
            ),
            after,
        )
    }

    /// **A re-shape writing a link records what the link resolves to at the
    /// plan's after-state, a broken link included**: `[[b]]` wrapped names
    /// `b.md`, and `[[ghost]]` unwrapped names nothing.
    #[test]
    fn a_reshape_writing_a_link_records_its_resolution_broken_or_not() {
        let schema = "version: 1\nfields:\n  ups: {type: link, shape: list}\n  up: {type: link, shape: single}\n";
        let planned = planned(
            schema,
            &[
                ("a.md", "---\nups: '[[b]]'\n---\n"),
                ("b.md", "# B\n"),
                ("c.md", "---\nup: ['[[ghost]]']\n---\n"),
            ],
            &[
                finding(1, FindingKind::ShapeMismatch, "a.md", "ups", "[[b]]"),
                finding(
                    2,
                    FindingKind::ShapeMismatch,
                    "c.md",
                    "up",
                    &crate::derivation::stored_spelling(&AuthoredValue::List(vec![
                        AuthoredValue::string("[[ghost]]"),
                    ]))
                    .expect("a list's spelling"),
                ),
            ],
        );

        assert_eq!(
            kinds(&planned),
            vec![
                setting(
                    "a.md",
                    "ups",
                    AuthoredValue::List(vec![AuthoredValue::string("[[b]]")])
                ),
                setting("c.md", "up", AuthoredValue::string("[[ghost]]")),
            ],
            "{:?}",
            planned.skipped
        );
        assert_eq!(
            planned.conditions,
            vec![
                resolving("a.md", "b", "b.md"),
                recording("c.md", "ghost", Resolves::none()),
            ]
        );
    }

    /// **A declared default writing a link records its resolution too**:
    /// `next` fills from a default naming `[[c]]`.
    #[test]
    fn a_declared_default_writing_a_link_records_its_resolution() {
        let schema = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    required:\n      next: {default: '[[c]]'}\n";
        let planned = planned(
            schema,
            &[("a.md", "---\ntype: task\n---\n"), ("c.md", "# C\n")],
            &[FindingRow::new(
                1,
                FindingKind::RequiredMissing,
                Severity::Warning,
                DocumentPath::new("a.md").expect("a document path"),
                Some("next".to_string()),
                None,
                CandidateHead::new([], 0).expect("an empty head"),
                None,
                "a finding",
                1,
            )],
        );

        assert_eq!(planned.operations.len(), 1, "{:?}", planned.skipped);
        assert_eq!(planned.conditions, vec![resolving("a.md", "c", "c.md")]);
    }

    /// **A link fix's one write of its field records every link the field
    /// holds, and one condition stands for each link however many fixes
    /// write it**: `ups` already holds `[[c]]`, and `up` and `ups` both come
    /// to write `[[b]]`.
    #[test]
    fn every_link_a_field_write_holds_is_recorded_once() {
        let planned = planned(
            LINKED,
            &[
                ("a.md", "---\nup: b\nups: ['[[c]]', b]\n---\n"),
                ("b.md", "# B\n"),
                ("c.md", "# C\n"),
            ],
            &[
                mismatch(1, "a.md", "up", "b"),
                mismatch(2, "a.md", "ups", "b"),
            ],
        );

        assert_eq!(planned.operations.len(), 2, "{:?}", planned.skipped);
        assert_eq!(
            planned.conditions,
            vec![
                resolving("a.md", "b", "b.md"),
                resolving("a.md", "c", "c.md")
            ]
        );
    }

    /// The rule default `default` for `next` on `a.md`, planned over
    /// documents `c.md` and `d.md`; `unanswering` if the index answers none.
    fn defaulting(default: &str, unanswering: bool) -> Planned {
        let schema = format!(
            "version: 1\nrules:\n  tasks:\n    match: {{frontmatter: {{type: task}}}}\n    \
             required:\n      next: {{default: '{}'}}\n",
            default.replace('\'', "''")
        );
        planned_reading(
            &schema,
            &[
                ("a.md", "---\ntype: task\n---\n"),
                ("c.md", "# C\n"),
                ("d.md", "# D\n"),
            ],
            &[FindingRow::new(
                1,
                FindingKind::RequiredMissing,
                Severity::Warning,
                DocumentPath::new("a.md").expect("a document path"),
                Some("next".to_string()),
                None,
                CandidateHead::new([], 0).expect("an empty head"),
                None,
                "a finding",
                1,
            )],
            Confidence::DEFAULT_THRESHOLD,
            unanswering,
        )
    }

    /// **Every link a fix writes is recorded, whatever it addresses**: a link
    /// of a protocol, of no document of the vault, with a fragment or an
    /// alias, an embed, several in one text. The index judges each wikilink
    /// the plan asks of it, so the batch's namings hold one for each link
    /// the fix writes, and the plan records its resolution — broken
    /// included, as the applier probes it again — and a link naming no
    /// document at all, as `[[#section]]` does, is written by no fix.
    #[test]
    fn every_link_a_fix_writes_is_recorded_whatever_it_addresses() {
        let cases: [(&str, &[(&str, Resolves)]); 9] = [
            (
                "[[c]]",
                &[(
                    "c",
                    Resolves::one(DocumentPath::new("c.md").expect("a path")),
                )],
            ),
            (
                "[[vault://c]]",
                &[(
                    "vault://c",
                    Resolves::one(DocumentPath::new("c.md").expect("a path")),
                )],
            ),
            (
                "[[https://example.com/x]]",
                &[("https://example.com/x", Resolves::none())],
            ),
            (
                "[[c#section]]",
                &[(
                    "c",
                    Resolves::one(DocumentPath::new("c.md").expect("a path")),
                )],
            ),
            ("[[#section]]", &[]),
            (
                "![[c]]",
                &[(
                    "c",
                    Resolves::one(DocumentPath::new("c.md").expect("a path")),
                )],
            ),
            (
                "[[c|alias]]",
                &[(
                    "c",
                    Resolves::one(DocumentPath::new("c.md").expect("a path")),
                )],
            ),
            ("[[note:draft]]", &[("note:draft", Resolves::none())]),
            (
                "see [[c]] and [[d]]",
                &[
                    (
                        "c",
                        Resolves::one(DocumentPath::new("c.md").expect("a path")),
                    ),
                    (
                        "d",
                        Resolves::one(DocumentPath::new("d.md").expect("a path")),
                    ),
                ],
            ),
        ];
        for (default, recorded) in cases {
            let planned = defaulting(default, false);
            assert_eq!(
                planned.operations.len(),
                1,
                "{default}: {:?}",
                planned.skipped
            );
            let expected: Vec<PlanCondition> = recorded
                .iter()
                .map(|(address, after)| recording("a.md", address, after.clone()))
                .collect();
            assert_eq!(planned.conditions, expected, "{default}");
        }
    }

    /// **A bare value that qualifies is exactly one written link of its own
    /// address**: the link fix looks its address up in the batch's namings
    /// by the value, and the batch asked for it by the link the fix writes,
    /// so the two cannot name different addresses.
    #[test]
    fn a_qualifying_bare_value_writes_exactly_one_link_of_its_own_address() {
        let mut characters: Vec<char> = (0x20u8..0x7f).map(char::from).collect();
        characters.extend(['\t', '\u{a0}', '\u{200b}', '\u{2028}', 'é', '\u{85}']);
        let mut values: Vec<String> = ["vault://c", "HTTPS://x", "a://b", "a:/b", "x://", "c.md"]
            .map(String::from)
            .to_vec();
        for a in &characters {
            values.extend([format!("x{a}y"), format!("{a}x"), format!("x{a}")]);
            for b in &characters {
                values.push(format!("x{a}{b}y"));
            }
        }
        let holder = DocumentPath::new("a.md").expect("a path");
        let mut qualifying = 0;
        for value in values {
            if bare_address(&value) != Some(value.as_str()) {
                continue;
            }
            qualifying += 1;
            let link = setting("a.md", "up", AuthoredValue::string(format!("[[{value}]]")));
            assert_eq!(
                written_links(&link),
                vec![(holder.clone(), value.clone())],
                "{value:?}"
            );
        }
        assert!(qualifying > 1000, "{qualifying}");
    }

    /// **A link a fix writes that the batch's namings lack stops the plan,
    /// in any build**: recording no condition for it would let the applier
    /// write a link it never checked, and answering it as broken would say
    /// what nothing read.
    #[test]
    #[should_panic(expected = "the batch resolved every link a fix writes")]
    fn a_written_link_the_batch_did_not_resolve_stops_the_plan() {
        defaulting("[[c]]", true);
    }

    /// The same stop for a bare value the link fix asks the namings of.
    #[test]
    #[should_panic(expected = "the batch resolved every link a fix writes")]
    fn a_bare_value_the_batch_did_not_resolve_stops_the_plan() {
        planned_reading(
            LINKED,
            &[("a.md", "---\nup: b\n---\n"), ("b.md", "# B\n")],
            &[mismatch(1, "a.md", "up", "b")],
            Confidence::DEFAULT_THRESHOLD,
            true,
        );
    }
}
