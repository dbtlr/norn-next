//! The write gate over the schema rules: a plan refuses exactly the
//! violations it introduces, each told apart by its kind, field, offending
//! value and combined constraint (ADR 0037).

use norn_wire::{
    AuthoredValue, ErrorDetail, FindingKind, Operation, OperationKind, RefusedCheck, RuleSet,
    SchemaViolation, WriteTarget,
};

use super::{Fixture, applied, creating, editing, path, refused, refused_for};
use crate::applier::ApplyOutcome;

/// A set of `field` in `at` to `value`.
fn setting(at: &str, field: &str, value: AuthoredValue) -> Operation {
    Operation::new(OperationKind::set_frontmatter(
        WriteTarget::path(path(at)),
        field,
        value,
    ))
}

fn text(value: &str) -> AuthoredValue {
    AuthoredValue::string(value)
}

fn list(items: &[&str]) -> AuthoredValue {
    AuthoredValue::List(items.iter().map(|item| text(item)).collect())
}

/// Each schema violation among `checks`, as its path, kind, field and the
/// text of the value it names.
fn violations(
    checks: &[RefusedCheck],
) -> Vec<(String, FindingKind, Option<String>, Option<String>)> {
    checks
        .iter()
        .map(|check| match check {
            RefusedCheck::SchemaViolation { violation, .. } => summary(violation),
            other => panic!("a schema check: {other:?}"),
        })
        .collect()
}

fn summary(violation: &SchemaViolation) -> (String, FindingKind, Option<String>, Option<String>) {
    (
        violation.path.as_str().to_string(),
        violation.kind,
        violation.target.clone(),
        violation.value.as_ref().map(|head| head.text().to_string()),
    )
}

fn violation(
    at: &str,
    kind: FindingKind,
    field: &str,
    value: Option<&str>,
) -> (String, FindingKind, Option<String>, Option<String>) {
    (
        at.to_string(),
        kind,
        Some(field.to_string()),
        value.map(str::to_string),
    )
}

/// The rule names each violation in `violations` cites, resolved against
/// `rule_sets`, the sets the response carries beside them.
fn cited(violations: &[&SchemaViolation], rule_sets: &[RuleSet]) -> Vec<Vec<String>> {
    violations
        .iter()
        .map(|violation| {
            violation.rule_set.map_or_else(Vec::new, |id| {
                rule_sets
                    .iter()
                    .find(|set| set.id == id)
                    .unwrap_or_else(|| panic!("the set {id} among {rule_sets:?}"))
                    .rules
                    .clone()
            })
        })
        .collect()
}

const TASKS: &str = "version: 1
fields:
  rating: { type: number }
rules:
  every: { required: { title: } }
  tasks: { match: { frontmatter: { type: task } }, one_of: { status: { values: [todo, done] } } }
";

/// **A create without a required field refuses, naming the field**: the
/// derivation's own judgment of the composed bytes is the gate's, so a
/// document `new` would write missing what a rule requires is refused with
/// the finding a derivation would file, citing the rule.
#[test]
fn a_create_without_a_required_field_refuses_naming_it() {
    let mut fixture = Fixture::with_schema(TASKS, &[]);
    let outcome = fixture.apply(fixture.plan(vec![creating("a.md", "---\ntype: note\n---\n")]));
    let envelope = outcome
        .into_wire()
        .expect("a refusal is answered")
        .expect_err("a refusal");
    let ErrorDetail::PlanRefused {
        checks, rule_sets, ..
    } = envelope.detail()
    else {
        panic!("a refusal: {envelope:?}");
    };
    assert_eq!(
        violations(checks),
        [violation(
            "a.md",
            FindingKind::RequiredMissing,
            "title",
            None
        )]
    );
    let RefusedCheck::SchemaViolation { violation, .. } = &checks[0] else {
        unreachable!("a schema check");
    };
    assert_eq!(cited(&[violation], rule_sets), [vec!["every".to_string()]]);
    assert_eq!(fixture.read("a.md"), None);
}

/// **A set outside a closed set refuses, naming its value and its rules**,
/// and so does a value that does not read as its field's declared type,
/// which no rule states: every schema finding gates, whatever its severity
/// and whether or not a rule states it.
#[test]
fn a_set_outside_a_closed_set_or_its_type_refuses() {
    let mut fixture = Fixture::with_schema(
        TASKS,
        &[("a.md", "---\ntitle: A\ntype: task\nstatus: todo\n---\n")],
    );
    let checks = refused_for(fixture.apply(fixture.plan(vec![
        setting("a.md", "status", text("someday")),
        setting("a.md", "rating", text("high")),
    ])));
    assert_eq!(
        violations(&checks),
        [
            violation("a.md", FindingKind::TypeMismatch, "rating", Some("high")),
            violation("a.md", FindingKind::NotOneOf, "status", Some("someday")),
        ]
    );
    assert_eq!(
        fixture.read("a.md").as_deref(),
        Some("---\ntitle: A\ntype: task\nstatus: todo\n---\n")
    );
}

/// **A plan writing a field refuses a new element violation on it, and not a
/// standing one whose identity is unchanged**: rewriting `[complete, bogus]`
/// to `[done, bogus]` writes `status`, and `bogus` keeps its identity, so it
/// applies; adding `wrong` refuses on `wrong` alone.
#[test]
fn a_written_field_refuses_only_the_element_violations_it_introduces() {
    let mut fixture = Fixture::with_schema(
        "version: 1
fields:
  status: { type: text, shape: list }
rules:
  states: { one_of: { status: { values: [todo, done], synonyms: { complete: done } } } }
",
        &[("a.md", "---\nstatus: [complete, bogus]\n---\n")],
    );
    applied(fixture.apply(fixture.plan(vec![setting("a.md", "status", list(&["done", "bogus"]))])));
    assert_eq!(
        fixture.read("a.md").as_deref(),
        Some("---\nstatus: [done, bogus]\n---\n")
    );
    let checks = refused_for(fixture.apply(fixture.plan(vec![setting(
        "a.md",
        "status",
        list(&["done", "bogus", "wrong"]),
    )])));
    assert_eq!(
        violations(&checks),
        [violation(
            "a.md",
            FindingKind::NotOneOf,
            "status",
            Some("wrong")
        )]
    );
    fixture.assert_store_is_a_build_from_zero();
}

/// **A combined constraint is compared by value, never by the rules stating
/// it**: a document missing `status` that a write brings under a second rule
/// also requiring `status` holds the same violation, so the unrelated write
/// applies; a write that moves a held `status: unknown` from under `[todo]`
/// to under `[done]` introduces a violation the document did not hold, and
/// refuses.
#[test]
fn a_violation_under_another_combined_constraint_is_introduced() {
    let mut fixture = Fixture::with_schema(
        "version: 1
rules:
  every: { required: { status: } }
  chores: { match: { frontmatter: { type: chore } }, required: { status: } }
  open: { match: { frontmatter: { area: open } }, one_of: { state: { values: [todo] } } }
  shut: { match: { frontmatter: { area: shut } }, one_of: { state: { values: [done] } } }
",
        &[
            ("a.md", "---\ntitle: A\n---\n"),
            ("b.md", "---\nstatus: x\narea: open\nstate: unknown\n---\n"),
        ],
    );
    applied(fixture.apply(fixture.plan(vec![setting("a.md", "type", text("chore"))])));
    let checks =
        refused_for(fixture.apply(fixture.plan(vec![setting("b.md", "area", text("shut"))])));
    assert_eq!(
        violations(&checks),
        [violation(
            "b.md",
            FindingKind::NotOneOf,
            "state",
            Some("unknown")
        )]
    );
    fixture.assert_store_is_a_build_from_zero();
}

/// **A force lets each violation through and lists it**, its value's head
/// and its rules beside it: the preview's forecast and the applied report
/// each list the violation and carry the rule set it cites.
#[test]
fn a_forced_plan_lists_each_rule_violation_with_its_value_and_rules() {
    let mut fixture = Fixture::with_schema(TASKS, &[("a.md", "---\ntitle: A\ntype: task\n---\n")]);
    let mut plan = fixture.plan(vec![setting("a.md", "status", text("someday"))]);
    plan.force = true;
    let (_, forecast) = fixture
        .preview(plan.clone())
        .expect("the forced plan previews");
    assert_eq!(
        forecast.forced.iter().map(summary).collect::<Vec<_>>(),
        [violation(
            "a.md",
            FindingKind::NotOneOf,
            "status",
            Some("someday")
        )]
    );
    assert_eq!(
        cited(
            &forecast.forced.iter().collect::<Vec<_>>(),
            &forecast.rule_sets
        ),
        [vec!["tasks".to_string()]]
    );
    let report = ApplyOutcome::Applied(applied(fixture.apply(plan)))
        .into_wire()
        .expect("an applied plan is answered")
        .expect("an applied plan is a report");
    let norn_wire::ApplyReport::Applied {
        forced, rule_sets, ..
    } = report
    else {
        panic!("an applied report: {report:?}");
    };
    assert_eq!(forced, forecast.forced);
    assert_eq!(rule_sets, forecast.rule_sets);
    assert_eq!(
        fixture.read("a.md").as_deref(),
        Some("---\ntitle: A\ntype: task\nstatus: someday\n---\n")
    );
    fixture.assert_store_is_a_build_from_zero();
}

/// **Link health does not gate a plan**: an edit writing a link that names
/// no document applies under a schema whose rules it meets, and the broken
/// link stands as a finding.
#[test]
fn a_link_a_plan_breaks_does_not_gate_it() {
    let mut fixture = Fixture::with_schema(TASKS, &[("a.md", "---\ntitle: A\n---\nSee here.\n")]);
    let landed = applied(fixture.apply(fixture.plan(vec![editing(
        "a.md",
        "See here.",
        "See [[nowhere]].",
    )])));
    assert!(landed.forced.is_empty());
    assert!(
        Fixture::derived(&mut fixture.store)
            .iter()
            .any(|line| line.contains("link/broken")),
        "the broken link stands as a finding"
    );
}

/// A refusal's fresh plan of a forced plan forecasts its violations under
/// the identities the refusal's own checks would cite them by: one numbering
/// per response.
#[test]
fn a_refusal_cites_its_checks_and_its_forecast_under_one_numbering() {
    let mut fixture = Fixture::with_schema(TASKS, &[("a.md", "---\ntitle: A\ntype: task\n---\n")]);
    let mut plan = fixture.plan(vec![setting("a.md", "status", text("someday"))]);
    plan.force = true;
    fixture.foreign("a.md", "---\ntitle: A\ntype: task\nnote: foreign\n---\n");
    let refusal = refused(fixture.apply(plan));
    let forced: Vec<&SchemaViolation> = refusal.forecast.forced.iter().collect();
    assert_eq!(
        cited(&forced, &refusal.forecast.rule_sets),
        [vec!["tasks".to_string()]]
    );
}

/// A schema with a field declared single, a rule forbidding `scratch`, and
/// two rules requiring and forbidding `owner` at once on a document of area
/// `y` and kind `x`.
const WHOLE_VALUES: &str = "version: 1
fields:
  rank: { type: number, shape: single }
rules:
  bans: { forbidden: { scratch: } }
  needs: { match: { frontmatter: { area: y } }, required: { owner: } }
  never: { match: { frontmatter: { kind: x } }, forbidden: { owner: } }
";

/// A document standing in every whole-value violation [`WHOLE_VALUES`]
/// states: a forbidden `scratch: a`, a misshaped `rank: [1, 2]` and a
/// conflicted `owner: ana`.
const STANDING_WHOLE_VALUES: &str =
    "---\ntitle: A\nscratch: a\nrank: [1, 2]\narea: y\nkind: x\nowner: ana\n---\n";

/// **Setting another value over a standing forbidden value refuses**: the
/// whole value a forbidden field holds is part of its identity, so `b` over
/// `a` introduces a violation, and so does null over `a`, which names no
/// value.
#[test]
fn setting_another_value_over_a_standing_forbidden_value_refuses() {
    let mut fixture = Fixture::with_schema(WHOLE_VALUES, &[("a.md", STANDING_WHOLE_VALUES)]);
    let checks =
        refused_for(fixture.apply(fixture.plan(vec![setting("a.md", "scratch", text("b"))])));
    assert_eq!(
        violations(&checks),
        [violation(
            "a.md",
            FindingKind::Forbidden,
            "scratch",
            Some("b")
        )]
    );
    let checks = refused_for(fixture.apply(fixture.plan(vec![setting(
        "a.md",
        "scratch",
        AuthoredValue::Null,
    )])));
    assert_eq!(
        violations(&checks),
        [violation("a.md", FindingKind::Forbidden, "scratch", None)]
    );
    assert_eq!(fixture.read("a.md").as_deref(), Some(STANDING_WHOLE_VALUES));
}

/// **Swapping a misshaped list for another refuses**: a shape mismatch is
/// told apart by the whole value it names.
#[test]
fn swapping_a_misshaped_list_for_another_refuses() {
    let mut fixture = Fixture::with_schema(WHOLE_VALUES, &[("a.md", STANDING_WHOLE_VALUES)]);
    let checks =
        refused_for(fixture.apply(fixture.plan(vec![setting("a.md", "rank", list(&["3", "4"]))])));
    assert_eq!(
        violations(&checks),
        [violation(
            "a.md",
            FindingKind::ShapeMismatch,
            "rank",
            Some("[\"3\",\"4\"]")
        )]
    );
}

/// **Swapping a conflicted field's value refuses**: a conflict over a field
/// is told apart by the whole value the field holds.
#[test]
fn swapping_a_conflicted_fields_value_refuses() {
    let mut fixture = Fixture::with_schema(WHOLE_VALUES, &[("a.md", STANDING_WHOLE_VALUES)]);
    let checks =
        refused_for(fixture.apply(fixture.plan(vec![setting("a.md", "owner", text("bo"))])));
    assert_eq!(
        violations(&checks),
        [violation(
            "a.md",
            FindingKind::FieldRulesConflict,
            "owner",
            Some("bo")
        )]
    );
}

/// **A forced whole-value swap lists each violation with its value's
/// head**, on the preview's forecast and the applied report alike.
#[test]
fn a_forced_whole_value_swap_lists_each_with_its_value_head() {
    let mut fixture = Fixture::with_schema(WHOLE_VALUES, &[("a.md", STANDING_WHOLE_VALUES)]);
    let mut plan = fixture.plan(vec![
        setting("a.md", "scratch", text("b")),
        setting("a.md", "rank", list(&["3", "4"])),
        setting("a.md", "owner", text("bo")),
    ]);
    plan.force = true;
    let expected = [
        violation("a.md", FindingKind::FieldRulesConflict, "owner", Some("bo")),
        violation(
            "a.md",
            FindingKind::ShapeMismatch,
            "rank",
            Some("[\"3\",\"4\"]"),
        ),
        violation("a.md", FindingKind::Forbidden, "scratch", Some("b")),
    ];
    let (_, forecast) = fixture
        .preview(plan.clone())
        .expect("the forced plan previews");
    assert_eq!(
        forecast.forced.iter().map(summary).collect::<Vec<_>>(),
        expected
    );
    let landed = applied(fixture.apply(plan));
    assert_eq!(
        landed.forced.iter().map(summary).collect::<Vec<_>>(),
        expected
    );
    fixture.assert_store_is_a_build_from_zero();
}

/// **A write leaving each whole value as it stood applies**: every standing
/// whole-value violation keeps its identity, so a write elsewhere in the
/// document introduces none.
#[test]
fn a_write_leaving_each_whole_value_unchanged_applies() {
    let mut fixture = Fixture::with_schema(WHOLE_VALUES, &[("a.md", STANDING_WHOLE_VALUES)]);
    let landed = applied(fixture.apply(fixture.plan(vec![setting("a.md", "title", text("B"))])));
    assert!(landed.forced.is_empty());
    fixture.assert_store_is_a_build_from_zero();
}

/// **An interrupted forced plan's response carries the rule sets its
/// violations cite**: a forced create missing the required `title` lands,
/// another writer then changes the edit's target, and the interruption
/// lists the create's violation beside the set naming `every`.
#[test]
fn an_interrupted_forced_plan_carries_the_rule_sets_its_violations_cite() {
    let mut fixture = Fixture::with_schema(TASKS, &[("e.md", "---\ntitle: E\n---\ndraft\n")]);
    let mut plan = fixture.plan(vec![
        creating("n.md", "---\ntype: note\n---\n"),
        editing("e.md", "draft", "final"),
    ]);
    plan.force = true;
    let links = fixture.links();
    let anchor = fixture.vault.clone();
    let publishing = || {
        std::fs::write(anchor.join("e.md"), "---\ntitle: E\n---\nforeign\n")
            .expect("another writer changes the edit's target");
        true
    };
    let applier = crate::applier::Applier {
        anchor: &fixture.vault,
        root: fixture.root,
        exclusions: &fixture.exclusions,
        schema: &fixture.schema,
        shadows: &fixture.shadows,
        own_writes: &fixture.recorded,
        publishing: &publishing,
        links: &links.index(),
    };
    let envelope = applier
        .apply(plan, &std::cell::RefCell::new(&mut fixture.store))
        .into_wire()
        .expect("an interruption is answered")
        .expect_err("the apply stops after its create");
    let ErrorDetail::PlanInterrupted {
        landed,
        forced,
        rule_sets,
        ..
    } = envelope.detail()
    else {
        panic!("an interruption: {envelope:?}");
    };
    assert_eq!(*landed, vec![path("n.md")]);
    assert_eq!(
        forced.iter().map(summary).collect::<Vec<_>>(),
        [violation(
            "n.md",
            FindingKind::RequiredMissing,
            "title",
            None
        )]
    );
    assert_eq!(
        cited(&forced.iter().collect::<Vec<_>>(), rule_sets),
        [vec!["every".to_string()]]
    );
}

/// **Distinct citation sets get distinct identities, each in the table**: a
/// forced create missing `owner`, which one rule requires, and `title`,
/// which another does, lists each violation citing its own set, and every
/// set a violation cites stands in the response's table.
#[test]
fn distinct_citation_sets_get_distinct_identities_each_in_the_table() {
    let mut fixture = Fixture::with_schema(
        "version: 1
rules:
  owners: { required: { owner: } }
  titles: { required: { title: } }
",
        &[],
    );
    let mut plan = fixture.plan(vec![creating("a.md", "Body.\n")]);
    plan.force = true;
    let (_, forecast) = fixture
        .preview(plan.clone())
        .expect("the forced plan previews");
    let ids: Vec<Option<u64>> = forecast
        .forced
        .iter()
        .map(|violation| violation.rule_set)
        .collect();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
    assert_eq!(
        cited(
            &forecast.forced.iter().collect::<Vec<_>>(),
            &forecast.rule_sets
        ),
        [vec!["owners".to_string()], vec!["titles".to_string()]]
    );
    let landed = applied(fixture.apply(plan));
    assert_eq!(landed.forced, forecast.forced);
}

/// **A forced create holding a forbidden value lists it with its value's
/// head**, on the preview's forecast and the applied report alike.
#[test]
fn a_forced_forbidden_create_carries_its_value_head_in_forecast_and_report() {
    let mut fixture = Fixture::with_schema(WHOLE_VALUES, &[]);
    let mut plan = fixture.plan(vec![creating("a.md", "---\nscratch: b\n---\n")]);
    plan.force = true;
    let expected = [violation(
        "a.md",
        FindingKind::Forbidden,
        "scratch",
        Some("b"),
    )];
    let (_, forecast) = fixture
        .preview(plan.clone())
        .expect("the forced plan previews");
    assert_eq!(
        forecast.forced.iter().map(summary).collect::<Vec<_>>(),
        expected
    );
    let report = ApplyOutcome::Applied(applied(fixture.apply(plan)))
        .into_wire()
        .expect("an applied plan is answered")
        .expect("an applied plan is a report");
    let norn_wire::ApplyReport::Applied { forced, .. } = report else {
        panic!("an applied report: {report:?}");
    };
    assert_eq!(forced.iter().map(summary).collect::<Vec<_>>(), expected);
}

/// **An undeclared tag respelled in another case introduces nothing**: the
/// tag's identity is its fold, so `[stray]` set to `[STRAY]` keeps the
/// violation that stood.
#[test]
fn an_undeclared_tag_respelled_in_another_case_introduces_nothing() {
    let mut fixture =
        Fixture::with_schema(super::TAG_SCHEMA, &[("a.md", "---\ntags: [stray]\n---\n")]);
    let landed =
        applied(fixture.apply(fixture.plan(vec![setting("a.md", "tags", list(&["STRAY"]))])));
    assert!(landed.forced.is_empty());
    assert_eq!(
        fixture.read("a.md").as_deref(),
        Some("---\ntags: [STRAY]\n---\n")
    );
}
