//! Rule judgment: what one document's path and frontmatter breach under the
//! field declarations and the combined constraint of the rules selecting it.
//!
//! Every case judges a document a vault could hold against schema bytes a
//! vault author could have written. Judgment is pure, so a case needs no
//! directory and no clock.

use norn_config::schema::{Breach, CaseFold, RuleFinding, VaultSchema};
use norn_wire::{AuthoredValue, Severity, ValueMap};

fn schema(bytes: &str) -> VaultSchema {
    VaultSchema::parse(bytes.as_bytes()).expect("a schema that reads")
}

fn frontmatter(entries: &[(&str, AuthoredValue)]) -> ValueMap {
    ValueMap::new(
        entries
            .iter()
            .map(|(key, value)| (key.to_string(), value.clone())),
    )
    .expect("each key once")
}

fn text(value: &str) -> AuthoredValue {
    AuthoredValue::string(value)
}

fn list(items: &[&str]) -> AuthoredValue {
    AuthoredValue::list(items.iter().map(|item| text(item)))
}

/// Every finding `schema` judges the document at `path` holding `entries` to.
fn judged(schema: &VaultSchema, path: &str, entries: &[(&str, AuthoredValue)]) -> Vec<RuleFinding> {
    schema
        .judge(path, &frontmatter(entries), CaseFold::Exact)
        .into_findings()
}

/// A finding as [`summary`] states it.
type Summary = (Breach, Option<String>, Option<String>, Vec<String>);

/// A finding as a case states it: its breach, its field, its value's text —
/// a scalar's own, or the debug spelling of anything else — and the rules it
/// cites.
fn summary(finding: &RuleFinding) -> Summary {
    (
        finding.breach(),
        finding.field().map(str::to_string),
        finding
            .value()
            .map(|value| value.scalar_text().unwrap_or_else(|| format!("{value:?}"))),
        finding.rules().to_vec(),
    )
}

fn summaries(findings: &[RuleFinding]) -> Vec<Summary> {
    findings.iter().map(summary).collect()
}

fn names(rules: &[&str]) -> Vec<String> {
    rules.iter().map(|rule| rule.to_string()).collect()
}

/// **One finding per path, field, constraint kind and offending value**: a
/// list is judged element by element, each distinct offending element its own
/// finding naming it, and an element written twice is one finding.
#[test]
fn a_list_is_judged_element_by_element_and_a_repeated_element_is_one_finding() {
    let schema = schema(
        "version: 1
fields:
  status: { type: text, shape: list }
rules:
  task: { one_of: { status: { values: [todo, done] } }, max_length: { status: 5 } }
",
    );
    let findings = judged(
        &schema,
        "a.md",
        &[(
            "status",
            list(&["todo", "bogus", "done", "bogus", "overlong"]),
        )],
    );
    assert_eq!(
        summaries(&findings),
        [
            (
                Breach::NotOneOf,
                Some("status".to_string()),
                Some("bogus".to_string()),
                names(&["task"])
            ),
            (
                Breach::NotOneOf,
                Some("status".to_string()),
                Some("overlong".to_string()),
                names(&["task"])
            ),
            (
                Breach::TooLong,
                Some("status".to_string()),
                Some("overlong".to_string()),
                names(&["task"])
            ),
        ]
    );
}

/// **With no shape declared, a list is judged per element and a scalar as
/// its one element.**
#[test]
fn an_undeclared_shape_judges_a_list_per_element_and_a_scalar_as_one() {
    let schema = schema(
        "version: 1
rules:
  task: { one_of: { status: { values: [todo, done] } } }
",
    );
    let listed = judged(&schema, "a.md", &[("status", list(&["todo", "x", "y"]))]);
    assert_eq!(
        listed
            .iter()
            .map(|finding| finding.value().and_then(AuthoredValue::scalar_text))
            .collect::<Vec<_>>(),
        [Some("x".to_string()), Some("y".to_string())]
    );
    let scalar = judged(&schema, "a.md", &[("status", text("x"))]);
    assert_eq!(
        summaries(&scalar),
        [(
            Breach::NotOneOf,
            Some("status".to_string()),
            Some("x".to_string()),
            names(&["task"])
        )]
    );
    assert!(judged(&schema, "a.md", &[("status", text("todo"))]).is_empty());
}

/// **Co-selecting rules mint one finding against their combined constraint**,
/// citing every rule contributing to it and reported at the highest of their
/// severities.
#[test]
fn co_selecting_rules_mint_one_finding_citing_every_contributing_rule_at_the_highest_severity() {
    let schema = schema(
        "version: 1
rules:
  wide: { one_of: { status: { values: [todo, doing, done] } } }
  task: { severity: error, match: { frontmatter: { type: task } }, one_of: { status: { values: [todo, done, open] } } }
",
    );
    let findings = judged(
        &schema,
        "a.md",
        &[("type", text("task")), ("status", text("doing"))],
    );
    assert_eq!(
        summaries(&findings),
        [(
            Breach::NotOneOf,
            Some("status".to_string()),
            Some("doing".to_string()),
            names(&["task", "wide"])
        )]
    );
    assert_eq!(findings[0].severity(), Severity::Error);
    let alone = judged(&schema, "a.md", &[("status", text("open"))]);
    assert_eq!(alone[0].rules(), names(&["wide"]));
    assert_eq!(alone[0].severity(), Severity::Warning);
}

/// **A finding cites the rules stating the constraint it breaches**, and no
/// other rule selecting the document.
#[test]
fn each_breach_cites_the_rules_stating_its_constraint_kind() {
    let schema = schema(
        "version: 1
rules:
  needs: { severity: error, required: { owner: } }
  bans: { forbidden: { scratch: remove } }
  long: { max_length: { title: 10 } }
  short: { max_length: { title: 20 } }
  closed: { one_of: { status: { values: [todo] } } }
  placed: { allowed_paths: { paths: ['tasks/**'] } }
",
    );
    let findings = judged(
        &schema,
        "notes/a.md",
        &[
            ("scratch", text("x")),
            ("title", text("a title past ten")),
            ("status", text("open")),
        ],
    );
    let cited: Vec<(Breach, Vec<String>, Severity)> = findings
        .iter()
        .map(|finding| {
            (
                finding.breach(),
                finding.rules().to_vec(),
                finding.severity(),
            )
        })
        .collect();
    assert_eq!(
        cited,
        [
            (Breach::RequiredMissing, names(&["needs"]), Severity::Error),
            (Breach::Forbidden, names(&["bans"]), Severity::Warning),
            (Breach::NotOneOf, names(&["closed"]), Severity::Warning),
            (
                Breach::TooLong,
                names(&["long", "short"]),
                Severity::Warning
            ),
            (Breach::Misplaced, names(&["placed"]), Severity::Warning),
        ]
    );
}

/// **A type or shape mismatch is judged against the field declarations**: it
/// cites no rule, is always a warning, and mints in a schema stating none.
#[test]
fn a_type_or_shape_mismatch_cites_no_rule_is_a_warning_and_mints_without_rules() {
    let schema = schema(
        "version: 1
fields:
  rating: { type: number }
  kind: { type: text, shape: single }
",
    );
    let findings = judged(
        &schema,
        "a.md",
        &[
            (
                "rating",
                AuthoredValue::list([text("high"), AuthoredValue::Integer(3)]),
            ),
            ("kind", list(&["a", "b"])),
        ],
    );
    assert_eq!(
        summaries(&findings),
        [
            (
                Breach::ShapeMismatch,
                Some("kind".to_string()),
                Some(format!("{:?}", list(&["a", "b"]))),
                Vec::new()
            ),
            (
                Breach::TypeMismatch,
                Some("rating".to_string()),
                Some("high".to_string()),
                Vec::new()
            ),
        ]
    );
    assert!(
        findings
            .iter()
            .all(|finding| finding.severity() == Severity::Warning)
    );
    let undeclared = judged(&schema, "a.md", &[("other", list(&["x"]))]);
    assert!(
        undeclared.is_empty(),
        "an undeclared field breached {undeclared:?}"
    );
}

/// **A field one rule requires and another forbids is one conflict**,
/// whatever it holds, citing both rules, instead of its required and
/// forbidden findings; it names the whole value where the key stands and
/// none where it is absent.
#[test]
fn a_field_required_and_forbidden_is_one_rules_conflict_instead_of_its_findings() {
    let schema = schema(
        "version: 1
rules:
  needs: { match: { frontmatter: { kind: x } }, required: { owner: }, one_of: { owner: { values: [me] } } }
  bans: { severity: error, match: { path: 'notes/**' }, forbidden: { owner: } }
",
    );
    for (entries, value) in [
        (vec![("kind", text("x"))], None),
        (
            vec![("kind", text("x")), ("owner", text("someone"))],
            Some("someone".to_string()),
        ),
    ] {
        let findings = judged(&schema, "notes/a.md", &entries);
        assert_eq!(
            summaries(&findings),
            [(
                Breach::FieldRulesConflict,
                Some("owner".to_string()),
                value,
                names(&["bans", "needs"])
            )]
        );
        assert_eq!(findings[0].severity(), Severity::Error);
    }
}

/// **Closed sets sharing no member on a required field are one conflict
/// whatever the field holds**, citing every rule closing or requiring it.
#[test]
fn an_empty_closed_set_on_a_required_field_is_a_conflict_whatever_it_holds() {
    let schema = schema(
        "version: 1
rules:
  a: { match: { frontmatter: { kind: x } }, required: { status: }, one_of: { status: { values: [todo] } } }
  b: { match: { path: 'notes/**' }, one_of: { status: { values: [open] } } }
",
    );
    for entries in [
        vec![("kind", text("x"))],
        vec![("kind", text("x")), ("status", text("todo"))],
    ] {
        let findings = judged(&schema, "notes/a.md", &entries);
        assert_eq!(
            findings
                .iter()
                .map(|finding| (finding.breach(), finding.rules().to_vec()))
                .collect::<Vec<_>>(),
            [(Breach::FieldRulesConflict, names(&["a", "b"]))]
        );
    }
}

/// **Closed sets sharing no member on a field no rule requires conflict only
/// where the field holds a value.**
#[test]
fn an_empty_closed_set_on_an_unrequired_field_is_a_conflict_only_where_a_value_stands() {
    let schema = schema(
        "version: 1
rules:
  a: { match: { frontmatter: { kind: x } }, one_of: { status: { values: [todo] } } }
  b: { match: { path: 'notes/**' }, one_of: { status: { values: [open] } } }
",
    );
    assert!(judged(&schema, "notes/a.md", &[("kind", text("x"))]).is_empty());
    assert!(
        judged(
            &schema,
            "notes/a.md",
            &[("kind", text("x")), ("status", AuthoredValue::Null)]
        )
        .is_empty()
    );
    let held = judged(
        &schema,
        "notes/a.md",
        &[("kind", text("x")), ("status", list(&["todo", "open"]))],
    );
    assert_eq!(
        summaries(&held),
        [(
            Breach::FieldRulesConflict,
            Some("status".to_string()),
            Some(format!("{:?}", list(&["todo", "open"]))),
            names(&["a", "b"])
        )]
    );
}

/// **Placement is judged where it fails**: a path some placement rule does
/// not admit is a misplacement, unless the rules' allowed paths share no
/// document path, which is a document conflict instead; an admitted path is
/// neither.
#[test]
fn disjoint_allowed_paths_are_a_document_conflict_and_overlapping_ones_a_misplacement() {
    let schema = schema(
        "version: 1
rules:
  tasks: { match: { frontmatter: { kind: task } }, allowed_paths: { paths: ['tasks/**'] } }
  notes: { match: { frontmatter: { area: notes } }, allowed_paths: { paths: ['notes/**'] } }
  wide: { match: { frontmatter: { area: work } }, allowed_paths: { paths: ['**'] } }
",
    );
    let disjoint = [("kind", text("task")), ("area", text("notes"))];
    for at in ["tasks/a.md", "notes/a.md", "elsewhere/a.md"] {
        assert_eq!(
            judged(&schema, at, &disjoint)
                .iter()
                .map(|finding| (finding.breach(), finding.field(), finding.rules().to_vec()))
                .collect::<Vec<_>>(),
            [(
                Breach::DocumentRulesConflict,
                None,
                names(&["notes", "tasks"])
            )],
            "at {at}"
        );
    }
    let overlapping = [("kind", text("task")), ("area", text("work"))];
    assert!(judged(&schema, "tasks/a.md", &overlapping).is_empty());
    let misplaced = judged(&schema, "notes/a.md", &overlapping);
    assert_eq!(
        summaries(&misplaced),
        [(Breach::Misplaced, None, None, names(&["tasks", "wide"]))]
    );
}

/// **Each kind names the value it states**: one element, or the scalar, for
/// a closed set, a length limit and a type; the whole value for a forbidden
/// field and a shape; none for a missing field or a placement.
#[test]
fn each_finding_names_the_value_its_kind_states() {
    let schema = schema(
        "version: 1
fields:
  rating: { type: number }
  kind: { type: text, shape: single }
rules:
  r: { required: { owner: }, forbidden: { scratch: }, one_of: { status: { values: [todo] } }, max_length: { title: 3 }, allowed_paths: { paths: ['tasks/**'] } }
",
    );
    let scratch = AuthoredValue::list([text("a"), AuthoredValue::Integer(2)]);
    let findings = judged(
        &schema,
        "notes/a.md",
        &[
            ("scratch", scratch.clone()),
            ("status", list(&["todo", "open"])),
            ("title", text("long")),
            ("rating", list(&["4", "four"])),
            ("kind", list(&["a"])),
        ],
    );
    let values: Vec<(Breach, Option<AuthoredValue>)> = findings
        .iter()
        .map(|finding| (finding.breach(), finding.value().cloned()))
        .collect();
    assert_eq!(
        values,
        [
            (Breach::ShapeMismatch, Some(list(&["a"]))),
            (Breach::RequiredMissing, None),
            (Breach::TypeMismatch, Some(text("four"))),
            (Breach::Forbidden, Some(scratch)),
            (Breach::NotOneOf, Some(text("open"))),
            (Breach::TooLong, Some(text("long"))),
            (Breach::Misplaced, None),
        ]
    );
}

/// **A value that does not read as its declared type or shape is judged by
/// that alone**: no closed set and no length limit judges it, while whether
/// the field is present still answers to `required` and `forbidden`.
#[test]
fn a_value_not_of_its_declared_type_or_shape_is_judged_by_that_alone() {
    let schema = schema(
        "version: 1
fields:
  rating: { type: number }
  kind: { type: text, shape: single }
  tagsy: { type: text, shape: list }
rules:
  r: { forbidden: { kind: }, one_of: { rating: { values: [1, 2] }, kind: { values: [a] }, tagsy: { values: [a] } }, max_length: { rating: 1, kind: 1, tagsy: 1 } }
",
    );
    let findings = judged(
        &schema,
        "a.md",
        &[
            (
                "rating",
                AuthoredValue::list([text("high"), AuthoredValue::Integer(3)]),
            ),
            ("kind", list(&["long", "b"])),
            ("tagsy", text("zz")),
        ],
    );
    assert_eq!(
        findings
            .iter()
            .map(|finding| (finding.breach(), finding.field().map(str::to_string)))
            .collect::<Vec<_>>(),
        [
            (Breach::ShapeMismatch, Some("kind".to_string())),
            (Breach::Forbidden, Some("kind".to_string())),
            (Breach::TypeMismatch, Some("rating".to_string())),
            (Breach::NotOneOf, Some("rating".to_string())),
            (Breach::ShapeMismatch, Some("tagsy".to_string())),
        ]
    );
    assert_eq!(
        findings[3].value().and_then(AuthoredValue::scalar_text),
        Some("3".to_string()),
        "the element that reads as a number is judged by the closed set, and only it"
    );
}

/// **A map reads as no type**: under a declared field, text included, it is a
/// type mismatch naming the whole map, and nothing else judges it.
#[test]
fn a_map_under_a_declared_field_is_a_type_mismatch_naming_the_whole_map() {
    let schema = schema(
        "version: 1
fields:
  owner: { type: text }
rules:
  r: { one_of: { owner: { values: [me] } }, max_length: { owner: 2 } }
",
    );
    let map = AuthoredValue::map([("name".to_string(), text("me"))]).expect("a map");
    let findings = judged(&schema, "a.md", &[("owner", map.clone())]);
    assert_eq!(
        findings
            .iter()
            .map(|finding| (finding.breach(), finding.value().cloned()))
            .collect::<Vec<_>>(),
        [(Breach::TypeMismatch, Some(map))]
    );
}

/// **`forbidden` acts on the key, and `required` on a value**: a forbidden
/// key breaches even holding null, and a required field is unmet by absent or
/// null and met by an empty string or an empty list.
#[test]
fn forbidden_is_breached_by_the_key_and_required_is_unmet_only_by_absent_or_null() {
    let schema = schema(
        "version: 1
rules:
  r: { required: { owner: }, forbidden: { scratch: } }
",
    );
    let null = judged(
        &schema,
        "a.md",
        &[("owner", text("me")), ("scratch", AuthoredValue::Null)],
    );
    assert_eq!(
        summaries(&null),
        [(
            Breach::Forbidden,
            Some("scratch".to_string()),
            None,
            names(&["r"])
        )]
    );
    for (owner, missing) in [
        (None, true),
        (Some(AuthoredValue::Null), true),
        (Some(text("")), false),
        (Some(AuthoredValue::list([])), false),
    ] {
        let entries: Vec<(&str, AuthoredValue)> =
            owner.into_iter().map(|value| ("owner", value)).collect();
        let findings = judged(&schema, "a.md", &entries);
        assert_eq!(
            findings
                .iter()
                .any(|finding| finding.breach() == Breach::RequiredMissing),
            missing,
            "{entries:?}"
        );
    }
}

/// **A null is no value**: a field holding null, or a list holding one among
/// its elements, breaches no type, no closed set and no length limit with it,
/// while `required` still reads the field holding null as missing.
#[test]
fn a_null_is_no_value_to_a_type_a_closed_set_or_a_limit() {
    let schema = schema(
        "version: 1
fields:
  rating: { type: number }
rules:
  r: { one_of: { status: { values: [todo] }, rating: { values: [1] } }, max_length: { status: 4 } }
",
    );
    let findings = judged(
        &schema,
        "a.md",
        &[
            (
                "status",
                AuthoredValue::list([text("todo"), AuthoredValue::Null]),
            ),
            (
                "rating",
                AuthoredValue::list([AuthoredValue::Null, AuthoredValue::Integer(1)]),
            ),
        ],
    );
    assert!(findings.is_empty(), "{findings:?}");
    assert!(judged(&schema, "a.md", &[("rating", AuthoredValue::Null)]).is_empty());
}

/// **A closed set on the tag carrier compares under the tag fold**, `#`
/// marker optional, as its selectors do.
#[test]
fn the_tag_carrier_is_judged_under_the_tag_fold() {
    let schema = schema(
        "version: 1
rules:
  r: { one_of: { tags: { values: [work] } } }
",
    );
    assert!(judged(&schema, "a.md", &[("tags", list(&["Work", "#WORK"]))]).is_empty());
    let findings = judged(&schema, "a.md", &[("tags", list(&["Work", "play"]))]);
    assert_eq!(
        findings[0].value().and_then(AuthoredValue::scalar_text),
        Some("play".to_string())
    );
}

/// **A document no rule selects and no declaration judges breaches
/// nothing**, and its judgment still evaluates every rule's selector.
#[test]
fn a_document_no_rule_selects_breaches_nothing() {
    let schema = schema(
        "version: 1
rules:
  task: { match: { frontmatter: { kind: task } }, required: { status: } }
",
    );
    let judgment = schema.judge("a.md", &frontmatter(&[]), CaseFold::Exact);
    assert!(judgment.findings().is_empty());
    assert_eq!(judgment.work().rules_evaluated, 1);
    assert_eq!(judgment.work().rules_selected, 0);
}

/// **The placement walk is paid once per set of rules**: the verdict a first
/// misplaced document's set walked is read by every later one, and a set of
/// one rule is never walked.
#[test]
fn a_placement_verdict_is_walked_once_per_set_of_rules() {
    let schema = schema(
        "version: 1
rules:
  tasks: { match: { frontmatter: { kind: task } }, allowed_paths: { paths: ['tasks/**'] } }
  notes: { match: { frontmatter: { area: notes } }, allowed_paths: { paths: ['notes/**'] } }
",
    );
    let both = frontmatter(&[("kind", text("task")), ("area", text("notes"))]);
    let first = schema.judge("a.md", &both, CaseFold::Exact).work();
    assert_eq!(
        (first.placement_walks, first.placement_verdicts_reused),
        (1, 0)
    );
    assert!(first.placement_weight > 0);
    let second = schema.judge("b.md", &both, CaseFold::Exact).work();
    assert_eq!(
        (
            second.placement_walks,
            second.placement_weight,
            second.placement_verdicts_reused
        ),
        (0, 0, 1)
    );
    let folded = schema.judge("c.md", &both, CaseFold::Ascii).work();
    assert_eq!(folded.placement_walks, 1, "a verdict is held per fold");
    let one = frontmatter(&[("kind", text("task"))]);
    let alone = schema.judge("a.md", &one, CaseFold::Exact).work();
    assert_eq!(
        (alone.placement_walks, alone.placement_verdicts_reused),
        (0, 0)
    );
}

/// **The memo is safe under concurrent judgments**: threads judging documents
/// of one set together reach the verdict a single judgment reaches, and the
/// memo leaves the schema equal to a fresh reading of its bytes.
#[test]
fn concurrent_judgments_share_the_placement_memo() {
    let bytes = "version: 1
rules:
  tasks: { match: { frontmatter: { kind: task } }, allowed_paths: { paths: ['tasks/**'] } }
  notes: { match: { frontmatter: { area: notes } }, allowed_paths: { paths: ['notes/**'] } }
";
    let shared = schema(bytes);
    let both = frontmatter(&[("kind", text("task")), ("area", text("notes"))]);
    let breaches: Vec<Vec<Breach>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|at| {
                let (shared, both) = (&shared, &both);
                scope.spawn(move || {
                    shared
                        .judge(&format!("{at}.md"), both, CaseFold::Exact)
                        .findings()
                        .iter()
                        .map(RuleFinding::breach)
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("a judgment"))
            .collect()
    });
    assert!(
        breaches
            .iter()
            .all(|breach| *breach == [Breach::DocumentRulesConflict])
    );
    assert_eq!(shared, schema(bytes));
    assert_eq!(shared.clone(), shared);
}
