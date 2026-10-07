//! Rule judgment: what one document's path and frontmatter breach under the
//! field declarations and the combined constraint of the rules selecting it.
//!
//! Every case judges a document a vault could hold against schema bytes a
//! vault author could have written. Judgment is pure, so a case needs no
//! directory and no clock.

use norn_config::schema::{Breach, CaseFold, FindingIdentity, RuleFinding, VaultSchema};
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
/// misplaced document's set walked is read by every later one. A set of one
/// rule is never walked: schema read refuses a rule whose allowed paths admit
/// no document path, so a document it alone places is misplaced.
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
    let alone = schema.judge("a.md", &one, CaseFold::Exact);
    assert_eq!(
        summaries(alone.findings()),
        [(Breach::Misplaced, None, None, names(&["tasks"]))]
    );
    assert_eq!(
        (
            alone.work().placement_walks,
            alone.work().placement_weight,
            alone.work().placement_verdicts_reused
        ),
        (0, 0, 0),
        "a set of one rule is not walked"
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

/// **An offending value is told apart by its equality key**, as a closed set
/// compares it: one value written several ways is one finding, carrying the
/// spelling written first. A tag key compares under the tag fold, `#` marker
/// optional, and a typed key by its typed value.
#[test]
fn one_value_written_several_ways_is_one_finding_carrying_its_first_spelling() {
    let schema = schema(
        "version: 1
fields:
  n: { type: number }
rules:
  r: { one_of: { tags: { values: [work] }, n: { values: [1] } } }
",
    );
    let findings = judged(
        &schema,
        "a.md",
        &[
            ("n", list(&["2", "2.0", "02"])),
            ("tags", list(&["play", "#play", "PLAY"])),
        ],
    );
    assert_eq!(
        summaries(&findings),
        [
            (
                Breach::NotOneOf,
                Some("n".to_string()),
                Some("2".to_string()),
                names(&["r"])
            ),
            (
                Breach::NotOneOf,
                Some("tags".to_string()),
                Some("play".to_string()),
                names(&["r"])
            ),
        ]
    );
}

/// **A length is judged on each spelling**, and the spellings of one value
/// past the limit are one finding, carrying the first of them.
#[test]
fn the_spellings_of_one_value_past_a_limit_are_one_finding_at_the_first_past_it() {
    let schema = schema(
        "version: 1
rules:
  r: { max_length: { tags: 4 } }
",
    );
    let findings = judged(
        &schema,
        "a.md",
        &[("tags", list(&["play", "#play", "#PLAY"]))],
    );
    assert_eq!(
        summaries(&findings),
        [(
            Breach::TooLong,
            Some("tags".to_string()),
            Some("#play".to_string()),
            names(&["r"])
        )]
    );
}

/// **An integer and the string spelling it are one offending element**: the
/// field row holds one text for both, and an undeclared field compares by it.
#[test]
fn an_integer_and_its_string_are_one_offending_element() {
    let schema = schema(
        "version: 1
rules:
  r: { one_of: { f: { values: [z] } } }
",
    );
    let findings = judged(
        &schema,
        "a.md",
        &[(
            "f",
            AuthoredValue::list([AuthoredValue::Integer(1), text("1")]),
        )],
    );
    assert_eq!(
        summaries(&findings),
        [(
            Breach::NotOneOf,
            Some("f".to_string()),
            Some("1".to_string()),
            names(&["r"])
        )]
    );
}

/// **A scalar not reading as its field's type is one offending element by its
/// text**: the integer `7` and the string `"7"` under a boolean field are one
/// type mismatch, not one for each kind of YAML scalar.
#[test]
fn a_mistyped_integer_and_its_string_are_one_type_mismatch() {
    let schema = schema(
        "version: 1
fields:
  b: { type: boolean }
",
    );
    let findings = judged(
        &schema,
        "a.md",
        &[(
            "b",
            AuthoredValue::list([AuthoredValue::Integer(7), text("7")]),
        )],
    );
    let mismatches: Vec<_> = findings
        .iter()
        .filter(|finding| finding.breach() == Breach::TypeMismatch)
        .map(|finding| finding.field().map(str::to_string))
        .collect();
    assert_eq!(mismatches, [Some("b".to_string())]);
}

/// **Two key orders of one map are one offending value**: a map with no
/// equality key is told apart by its structure, entries in key order.
#[test]
fn two_key_orders_of_one_map_are_one_offending_value() {
    let schema = schema(
        "version: 1
rules:
  r: { one_of: { f: { values: [z] } } }
",
    );
    let written = AuthoredValue::map([("b".to_string(), text("2")), ("a".to_string(), text("1"))])
        .expect("a map");
    let reordered =
        AuthoredValue::map([("a".to_string(), text("1")), ("b".to_string(), text("2"))])
            .expect("a map");
    let findings = judged(
        &schema,
        "a.md",
        &[("f", AuthoredValue::list([written.clone(), reordered]))],
    );
    assert_eq!(
        findings
            .iter()
            .map(|finding| (finding.breach(), finding.value().cloned()))
            .collect::<Vec<_>>(),
        [(Breach::NotOneOf, Some(written))]
    );
}

/// **A mistyped element repeated in a list is one type mismatch.**
#[test]
fn a_mistyped_element_repeated_in_a_list_is_one_type_mismatch() {
    let schema = schema(
        "version: 1
fields:
  n: { type: number, shape: list }
",
    );
    let findings = judged(&schema, "a.md", &[("n", list(&["four", "1", "four"]))]);
    assert_eq!(
        summaries(&findings),
        [(
            Breach::TypeMismatch,
            Some("n".to_string()),
            Some("four".to_string()),
            Vec::new()
        )]
    );
}

/// **A map is no value of either shape**: under a field declared a list it
/// is the one element that does not read as the type, not a value of the
/// other shape.
#[test]
fn a_map_under_a_list_shaped_field_is_a_type_mismatch_not_a_shape_mismatch() {
    let schema = schema(
        "version: 1
fields:
  f: { type: text, shape: list }
",
    );
    let map = AuthoredValue::map([("a".to_string(), text("b"))]).expect("a map");
    let findings = judged(&schema, "a.md", &[("f", map.clone())]);
    assert_eq!(
        findings
            .iter()
            .map(|finding| (finding.breach(), finding.value().cloned()))
            .collect::<Vec<_>>(),
        [(Breach::TypeMismatch, Some(map))]
    );
}

/// **The length limit still judges a conflicted field**: a conflict stands
/// in place of the required, forbidden and closed-set findings alone.
#[test]
fn the_length_limit_still_judges_a_conflicted_field() {
    let schema = schema(
        "version: 1
rules:
  needs: { match: { frontmatter: { kind: x } }, required: { f: }, max_length: { f: 2 } }
  bans: { match: { path: 'n/**' }, forbidden: { f: } }
",
    );
    let findings = judged(
        &schema,
        "n/a.md",
        &[("kind", text("x")), ("f", text("abc"))],
    );
    assert_eq!(
        summaries(&findings),
        [
            (
                Breach::FieldRulesConflict,
                Some("f".to_string()),
                Some("abc".to_string()),
                names(&["bans", "needs"])
            ),
            (
                Breach::TooLong,
                Some("f".to_string()),
                Some("abc".to_string()),
                names(&["needs"])
            ),
        ]
    );
}

/// **A conflict cites the rules in conflict and no other**: a rule closing
/// the field over a set the others leave a member does not contribute to a
/// required-and-forbidden conflict.
#[test]
fn a_rules_conflict_cites_only_the_rules_in_conflict() {
    let schema = schema(
        "version: 1
rules:
  a: { match: { frontmatter: { k: x } }, required: { f: } }
  b: { match: { path: 'n/**' }, forbidden: { f: } }
  c: { severity: error, one_of: { f: { values: [z] } } }
",
    );
    let findings = judged(&schema, "n/a.md", &[("k", text("x")), ("f", text("q"))]);
    assert_eq!(
        summaries(&findings),
        [(
            Breach::FieldRulesConflict,
            Some("f".to_string()),
            Some("q".to_string()),
            names(&["a", "b"])
        )]
    );
    assert_eq!(findings[0].severity(), Severity::Warning);
}

/// **An empty closed set on a field no rule requires replaces only its
/// closed-set findings**: a rule forbidding the field still breaches, citing
/// its own rules at their severity, beside the conflict citing the rules
/// closing the field.
#[test]
fn a_forbidden_field_still_breaches_beside_an_unrequired_empty_closed_set() {
    let schema = schema(
        "version: 1
rules:
  a: { match: { frontmatter: { k: x } }, one_of: { f: { values: [a] } } }
  b: { match: { path: 'n/**' }, one_of: { f: { values: [b] } } }
  c: { severity: error, forbidden: { f: } }
",
    );
    let findings = judged(&schema, "n/a.md", &[("k", text("x")), ("f", text("a"))]);
    assert_eq!(
        findings
            .iter()
            .map(|finding| (
                finding.breach(),
                finding.rules().to_vec(),
                finding.severity()
            ))
            .collect::<Vec<_>>(),
        [
            (Breach::Forbidden, names(&["c"]), Severity::Error),
            (
                Breach::FieldRulesConflict,
                names(&["a", "b"]),
                Severity::Warning
            ),
        ]
    );
}

/// **A value the closed sets do not judge mints no conflict over them**: a
/// value not of its declared type or shape is judged by that alone, so an
/// empty intersection on a field no rule requires conflicts only where the
/// field holds a value that reads as its type and shape.
#[test]
fn a_mistyped_or_misshaped_value_alone_mints_no_unrequired_closed_set_conflict() {
    let schema = schema(
        "version: 1
fields:
  n: { type: number }
  s: { type: text, shape: single }
rules:
  a: { match: { frontmatter: { k: x } }, one_of: { n: { values: [1] }, s: { values: [a] } } }
  b: { match: { path: 'n/**' }, one_of: { n: { values: [2] }, s: { values: [b] } } }
",
    );
    let breaches = |entries: &[(&str, AuthoredValue)]| -> Vec<(Breach, Option<String>)> {
        judged(&schema, "n/a.md", entries)
            .iter()
            .map(|finding| (finding.breach(), finding.field().map(str::to_string)))
            .collect()
    };
    let selected = ("k", text("x"));
    assert_eq!(
        breaches(&[selected.clone(), ("n", text("abc")), ("s", list(&["a"]))]),
        [
            (Breach::TypeMismatch, Some("n".to_string())),
            (Breach::ShapeMismatch, Some("s".to_string())),
        ]
    );
    assert_eq!(
        breaches(&[selected, ("n", list(&["abc", "3"]))]),
        [
            (Breach::TypeMismatch, Some("n".to_string())),
            (Breach::FieldRulesConflict, Some("n".to_string())),
        ]
    );
}

/// **Each constraint judged is counted once**: a value against its declared
/// shape, each element against its type, a field's presence against
/// `required`, and a path against the allowed paths.
#[test]
fn each_constraint_judged_is_counted_once() {
    let judged_count = |bytes: &str, entries: &[(&str, AuthoredValue)]| {
        schema(bytes)
            .judge("tasks/a.md", &frontmatter(entries), CaseFold::Exact)
            .work()
            .constraints_judged
    };
    assert_eq!(
        judged_count(
            "version: 1\nfields:\n  kind: { type: text, shape: single }\n",
            &[("kind", text("a"))]
        ),
        2,
        "the shape and the one element's type"
    );
    assert_eq!(
        judged_count(
            "version: 1\nfields:\n  kind: { type: text }\n",
            &[("kind", text("a"))]
        ),
        1,
        "the one element's type, and no shape where none is declared"
    );
    assert_eq!(
        judged_count(
            "version: 1\nrules:\n  r: { allowed_paths: { paths: ['tasks/**'] } }\n",
            &[]
        ),
        1,
        "the path against the allowed paths"
    );
    assert_eq!(
        judged_count(
            "version: 1\nrules:\n  r: { required: { owner: } }\n",
            &[("owner", text("me"))]
        ),
        1,
        "the field's presence against `required`"
    );
    assert_eq!(
        judged_count(
            "version: 1\nrules:\n  a: { match: { frontmatter: { kind: x } }, required: { owner: } }\n  b: { match: { path: 'tasks/**' }, forbidden: { owner: } }\n",
            &[("kind", text("x")), ("owner", text("me"))]
        ),
        1,
        "the rules conflict over the field, in place of its presence judgments"
    );
}

/// **The bytes of the declaration a judgment reads are counted**: the
/// selector values it compares and the constraint entries it combines, so
/// two schemas differing only in how large a closed set's member is judge
/// alike and pay apart, in proportion to the member's bytes.
#[test]
fn the_declaration_bytes_a_judgment_reads_are_counted() {
    let closing = |member: &str| {
        schema(&format!(
            "version: 1\nrules:\n  r: {{ match: {{ frontmatter: {{ kind: x }} }}, one_of: {{ status: {{ values: ['{member}'] }} }} }}\n"
        ))
    };
    let document = frontmatter(&[("kind", text("x")), ("status", text("other"))]);
    let small = closing("a").judge("a.md", &document, CaseFold::Exact);
    let large = closing(&"a".repeat(257)).judge("a.md", &document, CaseFold::Exact);
    assert_eq!(summaries(small.findings()), summaries(large.findings()));
    // The selector's key and value, then the member.
    assert_eq!(small.work().declaration_bytes, 4 + 1 + 1);
    assert_eq!(large.work().declaration_bytes, 4 + 1 + 257);
    let unselected = closing(&"a".repeat(257)).judge(
        "a.md",
        &frontmatter(&[("kind", text("y"))]),
        CaseFold::Exact,
    );
    assert_eq!(
        unselected.work().declaration_bytes,
        4 + 1,
        "a rule not selecting the document has its constraints read by no combination"
    );
    // A forbidden field and a length-limited one are each read by their name.
    let declared_bytes = |declaration: &str| {
        schema(&format!("version: 1\nrules:\n  r: {{ {declaration} }}\n"))
            .judge("a.md", &frontmatter(&[]), CaseFold::Exact)
            .work()
            .declaration_bytes
    };
    assert_eq!(declared_bytes("forbidden: { scratch: }"), 7);
    assert_eq!(declared_bytes("max_length: { title: 3 }"), 5);
}

/// The identity of the one finding `schema` judges the document at `path`
/// holding `entries` to of `breach`.
fn identity_of(
    schema: &VaultSchema,
    path: &str,
    entries: &[(&str, AuthoredValue)],
    breach: Breach,
) -> FindingIdentity {
    let findings = judged(schema, path, entries);
    let mut of_breach = findings.iter().filter(|finding| finding.breach() == breach);
    let finding = of_breach
        .next()
        .unwrap_or_else(|| panic!("a {breach:?} finding at {path}: {findings:?}"));
    assert!(
        of_breach.next().is_none(),
        "one {breach:?} finding: {findings:?}"
    );
    finding.identity().clone()
}

/// **A finding's identity compares its combined constraint by value, never by
/// the rules stating it** (ADR 0037): a second rule requiring a field the
/// document lacks leaves the missing field one identity, though the finding
/// now cites both rules.
#[test]
fn a_second_rule_stating_the_same_requirement_leaves_the_identity_unchanged() {
    let schema = schema(
        "version: 1
rules:
  every: { required: { status: } }
  chores: { match: { frontmatter: { type: chore } }, required: { status: } }
",
    );
    let alone = judged(&schema, "a.md", &[]);
    let beside = judged(&schema, "a.md", &[("type", text("chore"))]);
    assert_eq!(alone[0].rules(), names(&["every"]));
    assert_eq!(beside[0].rules(), names(&["chores", "every"]));
    assert_eq!(alone[0].identity(), beside[0].identity());
}

/// **A closed set's identity is its intersection, by equality key**: one
/// value outside `[todo]` and outside `[done]` breaches two constraints, so
/// it is two identities, while two rules closing a field over one set in
/// another order and under other names are one.
#[test]
fn a_value_outside_another_closed_set_is_another_identity() {
    let schema = schema(
        "version: 1
rules:
  open: { match: { path: 'open/**' }, one_of: { status: { values: [todo] } } }
  shut: { match: { path: 'shut/**' }, one_of: { status: { values: [done] } } }
  left: { match: { path: 'left/**' }, one_of: { status: { values: [todo, doing] } } }
  right: { match: { path: 'right/**' }, one_of: { status: { values: [doing, todo] } } }
",
    );
    let unknown = [("status", text("unknown"))];
    assert_ne!(
        identity_of(&schema, "open/a.md", &unknown, Breach::NotOneOf),
        identity_of(&schema, "shut/a.md", &unknown, Breach::NotOneOf)
    );
    assert_eq!(
        identity_of(&schema, "left/a.md", &unknown, Breach::NotOneOf),
        identity_of(&schema, "right/a.md", &unknown, Breach::NotOneOf)
    );
}

/// **An offending value's identity is its equality key**: `#Play` and `play`
/// under the tag carrier are one value, and an element a write leaves in a
/// list keeps its identity whatever its neighbours become.
#[test]
fn an_offending_value_keeps_its_identity_across_spellings_and_neighbours() {
    let schema = schema(
        "version: 1
rules:
  closed: { one_of: { tags: { values: [work] }, status: { values: [done] } } }
",
    );
    assert_eq!(
        identity_of(
            &schema,
            "a.md",
            &[("tags", list(&["#Play"]))],
            Breach::NotOneOf
        ),
        identity_of(
            &schema,
            "a.md",
            &[("tags", list(&["play"]))],
            Breach::NotOneOf
        )
    );
    let bogus = |before: &[&str]| {
        judged(&schema, "a.md", &[("status", list(before))])
            .into_iter()
            .find(|finding| {
                finding
                    .value()
                    .and_then(AuthoredValue::scalar_text)
                    .as_deref()
                    == Some("bogus")
            })
            .expect("a finding naming bogus")
            .identity()
            .clone()
    };
    assert_eq!(bogus(&["complete", "bogus"]), bogus(&["done", "bogus"]));
    assert_ne!(
        identity_of(
            &schema,
            "a.md",
            &[("status", text("bogus"))],
            Breach::NotOneOf
        ),
        identity_of(
            &schema,
            "a.md",
            &[("status", text("other"))],
            Breach::NotOneOf
        )
    );
}

/// **A length limit's identity is the smallest limit**, and a placement's the
/// allowed paths of each rule stating them, each rule's list as a set.
#[test]
fn a_smaller_limit_or_other_allowed_paths_are_another_identity() {
    let schema = schema(
        "version: 1
rules:
  wide: { match: { path: 'wide/**' }, max_length: { title: 10 } }
  narrow: { match: { path: 'narrow/**' }, max_length: { title: 5 } }
  also: { match: { path: 'also/**' }, max_length: { title: 10 } }
  tasks: { match: { frontmatter: { kind: task } }, allowed_paths: { paths: ['tasks/**', 'work/**'] } }
  jobs: { match: { frontmatter: { kind: job } }, allowed_paths: { paths: ['work/**', 'tasks/**'] } }
  notes: { match: { frontmatter: { kind: note } }, allowed_paths: { paths: ['notes/**'] } }
",
    );
    let title = [("title", text("a long title"))];
    assert_ne!(
        identity_of(&schema, "wide/a.md", &title, Breach::TooLong),
        identity_of(&schema, "narrow/a.md", &title, Breach::TooLong)
    );
    assert_eq!(
        identity_of(&schema, "wide/a.md", &title, Breach::TooLong),
        identity_of(&schema, "also/a.md", &title, Breach::TooLong)
    );
    let placed = |kind: &str| {
        identity_of(
            &schema,
            "x/a.md",
            &[("kind", text(kind))],
            Breach::Misplaced,
        )
    };
    assert_eq!(placed("task"), placed("job"));
    assert_ne!(placed("task"), placed("note"));
}

/// **A whole-value finding is told apart by the whole value it names**: a
/// forbidden field, a shape mismatch and a conflict over a field are one
/// finding per field, and another value under the same field is another
/// identity — a forbidden field gone from a value to null among them, since
/// a null names no value. The same value, its map entries written in another
/// order, is the same identity.
#[test]
fn a_whole_value_finding_is_another_identity_for_another_value() {
    let schema = schema(
        "version: 1
fields:
  tags: { type: tags, shape: list }
rules:
  bans: { forbidden: { scratch: } }
  needs: { match: { frontmatter: { area: y } }, required: { owner: } }
  never: { match: { frontmatter: { kind: x } }, forbidden: { owner: } }
",
    );
    let forbidden = |value: AuthoredValue| {
        identity_of(
            &schema,
            "a.md",
            &[("scratch", value), ("owner", text("o"))],
            Breach::Forbidden,
        )
    };
    assert_ne!(forbidden(text("a")), forbidden(text("b")));
    assert_ne!(forbidden(text("a")), forbidden(AuthoredValue::Null));
    assert_eq!(forbidden(text("a")), forbidden(text("a")));
    let map = |entries: &[(&str, &str)]| {
        AuthoredValue::Map(
            ValueMap::new(
                entries
                    .iter()
                    .map(|(key, value)| ((*key).to_string(), text(value))),
            )
            .expect("each key once"),
        )
    };
    assert_eq!(
        forbidden(map(&[("x", "1"), ("y", "2")])),
        forbidden(map(&[("y", "2"), ("x", "1")]))
    );
    let misshaped = |value: &str| {
        identity_of(
            &schema,
            "a.md",
            &[("tags", text(value)), ("owner", text("o"))],
            Breach::ShapeMismatch,
        )
    };
    assert_ne!(misshaped("a"), misshaped("b"));
    let conflicted = |owner: &str| {
        identity_of(
            &schema,
            "a.md",
            &[
                ("area", text("y")),
                ("kind", text("x")),
                ("owner", text(owner)),
            ],
            Breach::FieldRulesConflict,
        )
    };
    assert_ne!(conflicted("ana"), conflicted("bo"));
    assert_eq!(conflicted("ana"), conflicted("ana"));
}

/// **A schema reads where a document stands only through a rule's
/// `match.path`, `exclude.path` or `allowed_paths`**: a schema whose rules
/// select and constrain by frontmatter alone does not.
#[test]
fn a_schema_reads_document_paths_only_through_a_rules_path_parts() {
    for (bytes, reads) in [
        ("version: 1\n", false),
        (
            "version: 1\nrules:\n  r: { match: { frontmatter: { kind: x } }, required: { owner: } }\n",
            false,
        ),
        (
            "version: 1\nrules:\n  r: { match: { path: 'a/**' } }\n",
            true,
        ),
        (
            "version: 1\nrules:\n  r: { exclude: { path: ['a/**'] } }\n",
            true,
        ),
        (
            "version: 1\nrules:\n  r: { allowed_paths: { paths: ['a/**'] } }\n",
            true,
        ),
    ] {
        assert_eq!(schema(bytes).reads_document_paths(), reads, "{bytes}");
    }
}
