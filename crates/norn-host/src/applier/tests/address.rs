//! An address-resolution condition judged at a plan's after-state, in a
//! preview and in an apply alike: the condition holds where the link's
//! address resolves from its holder, with every target of the plan at its
//! after-state, to what the plan records; where it does not, the plan is
//! refused as a failed condition and its fresh plan carries the condition as
//! recorded.

use norn_wire::{
    ErrorDetail, LinkFamily, LinkKey, PlanCondition, ReasonCode, RefusedCheck, ResolvedPlan,
    Resolves,
};

use super::{
    Fixture, TAG_SCHEMA, applied, breaking, creating, editing, moving, path, present,
    pushing_a_stray_tag, refused,
};

/// The files most cases start from: `b.md` links `a.md`, and `c.md` is the
/// document the plan edits.
const FILES: [(&str, &str); 3] = [("a.md", "A\n"), ("b.md", "[[a]]\n"), ("c.md", "draft\n")];

/// The plan editing `c.md`, which writes no link and moves no document.
fn editing_c(fixture: &Fixture) -> ResolvedPlan {
    fixture.plan(vec![editing("c.md", "draft", "final")])
}

/// `plan` carrying `condition` beside what planning recorded.
fn carrying(plan: &ResolvedPlan, condition: PlanCondition) -> ResolvedPlan {
    let mut carrying = plan.clone();
    carrying.conditions.push(condition);
    carrying
}

/// The condition that the wikilink `[[address]]` in `holder` resolves to
/// `after`.
fn wikilink_resolving(holder: &str, address: &str, after: Resolves) -> PlanCondition {
    PlanCondition::address_resolution(
        LinkKey::new(path(holder), LinkFamily::Wikilink, address),
        after,
    )
}

/// The condition that `[[a]]` in `b.md` resolves to `after`.
fn a_resolving(after: Resolves) -> PlanCondition {
    wikilink_resolving("b.md", "a", after)
}

fn failed(condition: PlanCondition) -> RefusedCheck {
    RefusedCheck::condition_failed(condition)
}

impl Fixture {
    /// Another writer removes the file at `at`, and the store takes it in.
    fn foreign_removal(&mut self, at: &str) {
        std::fs::remove_file(self.vault.join(at)).expect("another writer removes it");
        crate::production::heal_from_zero(&mut self.store, &self.vault, &self.exclusions)
            .expect("a heal");
    }

    /// The checks a preview refuses `plan` with.
    fn previewed_refusal(&mut self, plan: ResolvedPlan) -> Vec<RefusedCheck> {
        let envelope = self
            .preview(plan)
            .expect_err("a preview of a plan that is refused");
        assert_eq!(envelope.code(), &ReasonCode::VaultPlanRefused);
        let ErrorDetail::PlanRefused { checks, .. } = envelope.detail() else {
            panic!("the preview is not a plan refusal: {envelope:?}");
        };
        checks.clone()
    }

    /// Hold that `plan` is refused with exactly `checks` by a preview and by
    /// an apply alike, and that the apply published nothing.
    fn refuses_with(&mut self, plan: &ResolvedPlan, checks: &[RefusedCheck]) {
        assert_eq!(self.previewed_refusal(plan.clone()), checks, "the preview");
        assert_eq!(
            refused(self.apply(plan.clone())).checks,
            checks,
            "the apply"
        );
        assert!(self.recorded.calls.borrow().is_empty(), "nothing published");
    }

    /// Hold that `plan` previews as itself and then applies.
    fn previews_and_applies(&mut self, plan: &ResolvedPlan) {
        let (previewed, _) = self.preview(plan.clone()).expect("the plan previews");
        assert_eq!(&previewed, plan);
        applied(self.apply(plan.clone()));
    }
}

/// **A condition that holds at the after-state previews and applies.** `[[a]]`
/// in `b.md` resolves to `a.md` and the plan changes nothing about that, so
/// the plan lands as it would without the condition, whatever protocol the
/// address is written with.
#[test]
fn an_address_resolution_that_holds_previews_and_applies() {
    let mut fixture = Fixture::new(&FILES);
    let plan = carrying(
        &editing_c(&fixture),
        a_resolving(Resolves::one(path("a.md"))),
    );
    fixture.previews_and_applies(&plan);
    assert_eq!(fixture.read("c.md").as_deref(), Some("final\n"));
    assert!(!fixture.recorded.calls.borrow().is_empty());

    let mut fixture = Fixture::new(&FILES[..]);
    fixture.write("b.md", "[[vault://a]] [x](vault://a.md) [y](a.md)\n");
    let plan = editing_c(&fixture);
    let conditions = [
        ("vault://a", LinkFamily::Wikilink),
        ("vault://a.md", LinkFamily::Markdown),
        ("a.md", LinkFamily::Markdown),
    ]
    .map(|(address, family)| {
        PlanCondition::address_resolution(
            LinkKey::new(path("b.md"), family, address),
            Resolves::one(path("a.md")),
        )
    });
    let mut carrying = plan.clone();
    carrying.conditions.extend(conditions);
    fixture.previews_and_applies(&carrying);
}

/// **A written link's target deleted since planning refuses the plan.** The
/// condition records `[[a]]` resolving to `a.md`; another writer removed
/// `a.md`, so the address resolves to none.
#[test]
fn a_target_deleted_since_planning_refuses_the_plan() {
    let mut fixture = Fixture::new(&FILES);
    let condition = a_resolving(Resolves::one(path("a.md")));
    let plan = carrying(&editing_c(&fixture), condition.clone());
    fixture.foreign_removal("a.md");
    fixture.refuses_with(&plan, &[failed(condition)]);
    assert_eq!(fixture.read("c.md").as_deref(), Some("draft\n"));
}

/// **A competing target created since planning refuses the plan.** The
/// condition records `[[a]]` resolving to one document; another writer
/// created a second `a.md` in another folder, so the address names several.
#[test]
fn a_competing_target_created_since_planning_refuses_the_plan() {
    let mut fixture = Fixture::new(&FILES);
    let condition = a_resolving(Resolves::one(path("a.md")));
    let plan = carrying(&editing_c(&fixture), condition.clone());
    fixture.foreign("other/a.md", "another\n");
    fixture.refuses_with(&plan, &[failed(condition)]);
    assert_eq!(fixture.read("c.md").as_deref(), Some("draft\n"));
}

/// **A broken link's old target recreated since planning refuses the plan.**
/// The condition records `[[missing]]` resolving to none; another writer
/// created `missing.md`, so it now resolves to one.
#[test]
fn a_broken_links_target_recreated_since_planning_refuses_the_plan() {
    let mut fixture = Fixture::new(&[("b.md", "[[missing]]\n"), ("c.md", "draft\n")]);
    let condition = wikilink_resolving("b.md", "missing", Resolves::none());
    let plan = carrying(&editing_c(&fixture), condition.clone());
    fixture.previews_and_applies(&plan);

    let mut fixture = Fixture::new(&[("b.md", "[[missing]]\n"), ("c.md", "draft\n")]);
    let plan = carrying(&editing_c(&fixture), condition.clone());
    fixture.foreign("missing.md", "back\n");
    fixture.refuses_with(&plan, &[failed(condition)]);
    assert_eq!(fixture.read("c.md").as_deref(), Some("draft\n"));
}

/// **The after-state is what is judged, where the plan creates the target.**
/// `[[new]]` in `b.md` resolves to none before the plan and to the document
/// the plan creates after it, so a condition recording the after-state holds
/// and one recording the before-state does not.
#[test]
fn an_address_is_judged_with_the_document_the_plan_creates() {
    let files = [("b.md", "[[new]]\n")];
    let holds = wikilink_resolving("b.md", "new", Resolves::one(path("new.md")));
    let mut fixture = Fixture::new(&files);
    let plan = carrying(&fixture.plan(vec![creating("new.md", "N\n")]), holds);
    fixture.previews_and_applies(&plan);

    let before = wikilink_resolving("b.md", "new", Resolves::none());
    let mut fixture = Fixture::new(&files);
    let plan = carrying(
        &fixture.plan(vec![creating("new.md", "N\n")]),
        before.clone(),
    );
    fixture.refuses_with(&plan, &[failed(before)]);
    assert_eq!(fixture.read("new.md"), None);
}

/// **The after-state is what is judged, where the plan deletes the target.**
/// `[[a]]` in `b.md` resolves to `a.md` before the plan and to none after it,
/// so a condition recording none holds and one recording `a.md` does not.
#[test]
fn an_address_is_judged_without_the_document_the_plan_deletes() {
    let files = [("a.md", "A\n"), ("b.md", "[[a]]\n")];
    let holds = a_resolving(Resolves::none());
    let mut fixture = Fixture::new(&files);
    let plan = carrying(&fixture.plan(vec![breaking("a.md")]), holds);
    fixture.previews_and_applies(&plan);

    let before = a_resolving(Resolves::one(path("a.md")));
    let mut fixture = Fixture::new(&files);
    let plan = carrying(&fixture.plan(vec![breaking("a.md")]), before.clone());
    fixture.refuses_with(&plan, &[failed(before)]);
    assert_eq!(fixture.read("a.md").as_deref(), Some("A\n"));
}

/// **A holder the plan moves is judged from where it lands.** `[z](c.md)` in
/// `notes/b.md` is broken there, and resolves to `archive/c.md` from
/// `archive/b.md` once the plan has moved its holder, so a condition on the
/// holder's landing path recording `archive/c.md` holds and one recording
/// none, what the address resolves to from where the holder stood, does not.
#[test]
fn a_relative_address_is_judged_from_where_the_plan_moves_its_holder() {
    let files = [("notes/b.md", "[z](c.md)\n"), ("archive/c.md", "C\n")];
    let key = || LinkKey::new(path("archive/b.md"), LinkFamily::Markdown, "c.md");
    let holds = PlanCondition::address_resolution(key(), Resolves::one(path("archive/c.md")));
    let mut fixture = Fixture::new(&files);
    let plan = carrying(
        &fixture.plan(vec![moving("notes/b.md", "archive/b.md")]),
        holds,
    );
    fixture.previews_and_applies(&plan);
    // The move's cascade respells the link toward where `c.md` stood from
    // `notes/`; the condition is about the address the plan replaced.
    assert_eq!(
        fixture.read("archive/b.md").as_deref(),
        Some("[z](../notes/c.md)\n")
    );

    let from_the_source = PlanCondition::address_resolution(key(), Resolves::none());
    let mut fixture = Fixture::new(&files);
    let plan = carrying(
        &fixture.plan(vec![moving("notes/b.md", "archive/b.md")]),
        from_the_source.clone(),
    );
    fixture.refuses_with(&plan, &[failed(from_the_source)]);
    assert_eq!(fixture.read("notes/b.md").as_deref(), Some("[z](c.md)\n"));
}

/// **The fresh plan of a refusal carries the conditions as recorded, and they
/// are checked again.** Refused because `a.md` was removed, the fresh plan
/// refuses again while the evidence stays changed, and applies once the
/// evidence is restored; a refused plan carrying none yields a fresh plan
/// carrying none.
#[test]
fn the_fresh_plan_of_a_refusal_checks_its_address_resolutions_again() {
    let mut fixture = Fixture::new(&FILES);
    let condition = a_resolving(Resolves::one(path("a.md")));
    let plan = carrying(&editing_c(&fixture), condition.clone());
    fixture.foreign_removal("a.md");

    let fresh = refused(fixture.apply(plan.clone())).plan;
    assert_eq!(fresh, plan);
    assert_eq!(fresh.conditions.last(), Some(&condition));
    let ErrorDetail::PlanRefused {
        plan: previewed, ..
    } = fixture
        .preview(plan)
        .expect_err("a preview of the refused plan")
        .detail()
        .clone()
    else {
        panic!("the preview is not a plan refusal");
    };
    assert_eq!(previewed, fresh);

    fixture.refuses_with(&fresh, &[failed(condition)]);
    fixture.foreign("a.md", "A\n");
    fixture.previews_and_applies(&fresh);
    assert_eq!(fixture.read("c.md").as_deref(), Some("final\n"));

    // A plan carrying none, refused for drift, yields a fresh plan with none.
    let mut fixture = Fixture::new(&FILES);
    let plain = editing_c(&fixture);
    fixture.write("c.md", "draft\nmore\n");
    let fresh = refused(fixture.apply(plain)).plan;
    assert!(
        fresh
            .conditions
            .iter()
            .all(|condition| !matches!(condition, PlanCondition::AddressResolution { .. })),
        "{:?}",
        fresh.conditions
    );
}

/// **A forced plan whose carried condition fails still forecasts its
/// violations**, in an apply and in a preview of its fresh plan alike: the
/// fresh plan's forecast is judged before the condition is carried, so what
/// the force lets through does not depend on whether the evidence held.
#[test]
fn a_refused_forced_plan_carrying_a_failing_address_resolution_still_forecasts_its_violations() {
    let mut fixture = Fixture::with_schema(
        TAG_SCHEMA,
        &[("a.md", "---\ntags: [project]\n---\n"), ("b.md", "[[a]]\n")],
    );
    let plan = fixture.plan(vec![pushing_a_stray_tag()]);
    let unforced = super::refused_for(fixture.apply(plan.clone()));
    let [RefusedCheck::SchemaViolation { violation, .. }] = unforced.as_slice() else {
        panic!("the unforced plan refuses on the schema: {unforced:?}");
    };
    let failing = a_resolving(Resolves::none());
    let mut carrying = plan;
    carrying.force = true;
    carrying.conditions.push(failing.clone());

    let refusal = refused(fixture.apply(carrying));
    assert_eq!(refusal.checks, vec![failed(failing.clone())]);
    assert_eq!(refusal.forecast.forced, vec![violation.clone()]);
    assert_eq!(refusal.plan.conditions.last(), Some(&failing));

    let previewed = fixture
        .preview(refusal.plan)
        .expect_err("a preview of the fresh plan");
    let ErrorDetail::PlanRefused {
        forecast, checks, ..
    } = previewed.detail()
    else {
        panic!("the preview is not a plan refusal: {previewed:?}");
    };
    assert_eq!(checks, &vec![failed(failing)]);
    assert_eq!(forecast.forced, vec![violation.clone()]);
}

/// **A plan whose transitions disagree with its operations is invalid, not
/// refused for a failed address resolution.** The shape check and the
/// recomposition both run first.
#[test]
fn a_plan_disagreeing_with_its_operations_is_invalid_whatever_its_address_resolutions() {
    let failing = a_resolving(Resolves::none());

    let mut fixture = Fixture::new(&FILES);
    let mut plan = carrying(&fixture.plan(vec![moving("a.md", "d.md")]), failing.clone());
    plan.transitions
        .retain(|transition| transition.path != path("d.md"));
    assert_eq!(fixture.refuses_disagreeing(plan), vec![path("d.md")]);

    let mut fixture = Fixture::new(&FILES);
    let mut plan = carrying(&editing_c(&fixture), failing);
    plan.transitions[0].after = present("not what the edit writes\n");
    assert_eq!(fixture.refuses_disagreeing(plan), vec![path("c.md")]);
}

/// **A failed change-set entry and a failed address resolution are both
/// listed**, the entry first.
#[test]
fn a_failed_entry_and_a_failed_address_resolution_are_both_listed() {
    let mut fixture = Fixture::new(&FILES);
    let entry = super::link_entry(
        "b.md",
        "a",
        Resolves::one(path("a.md")),
        Resolves::one(path("a.md")),
    );
    let failing = a_resolving(Resolves::none());
    let mut plan = editing_c(&fixture);
    plan.conditions.push(entry.clone());
    plan.conditions.push(failing.clone());
    fixture.refuses_with(&plan, &[failed(entry), failed(failing)]);
    assert_eq!(fixture.read("c.md").as_deref(), Some("draft\n"));
}

/// **A failed address resolution answers before a delete's contradicted link
/// choice does**, as a failed change-set entry does: the evidence the vault
/// moved since planning is what the fresh plan answers for, and it resolves
/// the delete afresh. Without the failing condition the same plan is invalid.
#[test]
fn a_failed_address_resolution_refuses_before_a_contradicted_delete_is_invalid() {
    let files = [("a.md", "A\n"), ("b.md", "[[a]]\n")];
    let forbidding = |fixture: &Fixture| {
        let mut plan = fixture.plan(vec![breaking("a.md")]);
        let operation = &mut plan.operations[0].kind;
        let norn_wire::OperationKind::DeleteDocument { backlinks, .. } = operation else {
            panic!("the plan's one operation is the delete: {operation:?}");
        };
        *backlinks = norn_wire::Backlinks::Forbidden;
        plan
    };

    let mut fixture = Fixture::new(&files);
    let plan = forbidding(&fixture);
    assert_eq!(fixture.refuses_disagreeing(plan), vec![path("a.md")]);

    // After the delete `[[a]]` resolves to none, not to `a.md`.
    let failing = a_resolving(Resolves::one(path("a.md")));
    let plan = carrying(&forbidding(&fixture), failing.clone());
    fixture.refuses_with(&plan, &[failed(failing)]);
    assert_eq!(fixture.read("a.md").as_deref(), Some("A\n"));
}

impl Fixture {
    /// What applying `plan` cost the link index it was judged on.
    fn judgments_of_applying(&mut self, plan: &ResolvedPlan) -> crate::evidence::LinkJudgmentCost {
        let links = self.links();
        let index = links.index();
        let applier = super::Applier {
            anchor: &self.vault,
            root: self.root,
            exclusions: &self.exclusions,
            schema: &self.schema,
            shadows: &self.shadows,
            own_writes: &self.recorded,
            publishing: &|| true,
            links: &index,
        };
        applied(applier.apply(plan.clone(), &std::cell::RefCell::new(&mut self.store)));
        index.link_judgment_cost()
    }
}

/// **The check costs one judgment for all of a plan's address resolutions,
/// and a plan carrying none judges nothing.** Each judgment is one call of
/// the store's resolution door, however many conditions it answers, and it
/// reads no link the conditions do not name.
#[test]
fn address_resolutions_cost_one_judgment_and_a_plan_without_any_judges_none() {
    let mut fixture = Fixture::new(&FILES);
    let plain = editing_c(&fixture);
    let cost = fixture.judgments_of_applying(&plain);
    assert_eq!((cost.judgments, cost.statements_executed), (0, 0));

    let mut fixture = Fixture::new(&FILES);
    fixture.write("b.md", "[[a]] [[b]] [[c]]\n");
    let mut plan = editing_c(&fixture);
    for (address, target) in [("a", "a.md"), ("b", "b.md"), ("c", "c.md")] {
        plan.conditions.push(wikilink_resolving(
            "b.md",
            address,
            Resolves::one(path(target)),
        ));
    }
    let cost = fixture.judgments_of_applying(&plan);
    assert_eq!((cost.judgments, cost.links_evaluated), (1, 3));
    assert_eq!(cost.full_scan_steps, 0);
}
