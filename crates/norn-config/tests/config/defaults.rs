//! The defaults fixpoint: the rule defaults a created document takes for the
//! required fields its caller and its creation rule left missing.
//!
//! The fixpoint is a pure function of the composed frontmatter, the created
//! path, one clock reading and the schema, so a case hands each over and
//! needs no directory and no clock.

use norn_config::schema::{CaseFold, LocalTimestamp, RuleDefaultsRefusal, VaultSchema};
use norn_wire::{AuthoredValue, ValueMap};

/// The one clock reading every case fills from.
fn at() -> LocalTimestamp {
    LocalTimestamp::new(2026, 10, 6, 9, 30, 15, 120).expect("a local timestamp")
}

fn text(value: &str) -> AuthoredValue {
    AuthoredValue::string(value)
}

fn frontmatter(entries: &[(&str, AuthoredValue)]) -> ValueMap {
    ValueMap::new(
        entries
            .iter()
            .map(|(key, value)| (key.to_string(), value.clone())),
    )
    .expect("each key once")
}

fn fill(
    schema: &[u8],
    path: &str,
    entries: &[(&str, AuthoredValue)],
) -> Result<Vec<(String, AuthoredValue)>, RuleDefaultsRefusal> {
    VaultSchema::parse(schema)
        .expect("a schema with rule defaults")
        .fill_rule_defaults(&frontmatter(entries), path, at(), CaseFold::Exact)
}

fn filled(entries: &[(&str, AuthoredValue)]) -> Vec<(String, AuthoredValue)> {
    entries
        .iter()
        .map(|(key, value)| (key.to_string(), value.clone()))
        .collect()
}

/// Each conflicting field a refusal names, with each candidate value and
/// the rules proposing it.
type Conflicts = Vec<(String, Vec<(AuthoredValue, Vec<String>)>)>;

/// The conflicts `refusal` names, or a panic where it names none.
fn conflicts(refusal: &RuleDefaultsRefusal) -> Conflicts {
    let RuleDefaultsRefusal::Conflict { fields } = refusal else {
        panic!("a conflict: {refusal}");
    };
    fields
        .iter()
        .map(|conflict| {
            (
                conflict.field().to_string(),
                conflict
                    .candidates()
                    .iter()
                    .map(|candidate| (candidate.value().clone(), candidate.rules().to_vec()))
                    .collect(),
            )
        })
        .collect()
}

/// One conflicting field, each candidate written as `(value, rules)`.
fn conflict(field: &str, candidates: &[(AuthoredValue, &[&str])]) -> Conflicts {
    vec![(
        field.to_string(),
        candidates
            .iter()
            .map(|(value, rules)| {
                (
                    value.clone(),
                    rules.iter().map(|rule| rule.to_string()).collect(),
                )
            })
            .collect(),
    )]
}

/// **A default a filled default brings into scope is filled.** A vault-wide
/// rule fills `kind: task`, which brings in the rule on `kind: task`, whose
/// default for `priority` is filled in the next round.
#[test]
fn a_default_a_filled_default_brings_into_scope_is_filled() {
    let schema = b"version: 1
rules:
  every: { required: { kind: { default: task } } }
  task: { match: { frontmatter: { kind: task } }, required: { priority: { default: normal } } }
";
    assert_eq!(
        fill(schema, "notes/a.md", &[]),
        Ok(filled(&[
            ("kind", text("task")),
            ("priority", text("normal"))
        ]))
    );
    // The caller's own `kind` selects the same rule without a cascade.
    assert_eq!(
        fill(schema, "notes/a.md", &[("kind", text("note"))]),
        Ok(Vec::new())
    );
}

/// **A fill a later round contradicts is a conflict.** A vault-wide rule
/// fills `kind: task` and `status: todo`; the rule on `kind: task` it brings
/// in defaults `status: done`. The round that filled `status` agreed, so the
/// re-check of the settled frontmatter names `status` and both candidates.
#[test]
fn a_default_a_later_round_contradicts_refuses_naming_the_field_and_both_candidates() {
    let schema = b"version: 1
rules:
  every: { required: { kind: { default: task }, status: { default: todo } } }
  task: { match: { frontmatter: { kind: task } }, required: { status: { default: done } } }
";
    let refusal = fill(schema, "notes/a.md", &[]).expect_err("a late conflict");
    assert_eq!(
        conflicts(&refusal),
        conflict(
            "status",
            &[(text("todo"), &["every"]), (text("done"), &["task"])]
        )
    );
    let said = refusal.to_string();
    assert!(
        said.contains("`status`") && said.contains("todo") && said.contains("done"),
        "{said}"
    );
}

#[test]
fn conflicting_defaults_in_one_round_refuse() {
    let schema = b"version: 1
rules:
  a: { required: { status: { default: todo }, owner: { default: drew } } }
  b: { match: { path: 'tasks/**' }, required: { status: { default: open }, owner: { default: drew } } }
";
    assert_eq!(
        conflicts(&fill(schema, "tasks/a.md", &[]).expect_err("a conflict in one round")),
        conflict("status", &[(text("todo"), &["a"]), (text("open"), &["b"])])
    );
    // Where the rules agree, the one value is filled.
    assert_eq!(
        fill(schema, "notes/a.md", &[]),
        Ok(filled(&[("owner", text("drew")), ("status", text("todo"))]))
    );
}

/// **A round's disagreement is refused in that round, not settled by a
/// pick.** Two vault-wide rules default `kind` apart. Picking either would
/// bring in the rules on `kind: task`, whose `owner` defaults disagree too —
/// a conflict that exists only because of the pick. The refusal names `kind`
/// alone.
#[test]
fn a_disagreement_in_a_round_refuses_before_a_pick_brings_in_more_rules() {
    let schema = b"version: 1
rules:
  a: { required: { kind: { default: task } } }
  b: { required: { kind: { default: note } } }
  t1: { match: { frontmatter: { kind: task } }, required: { owner: { default: drew } } }
  t2: { match: { frontmatter: { kind: task } }, required: { owner: { default: sam } } }
";
    assert_eq!(
        conflicts(&fill(schema, "notes/a.md", &[]).expect_err("a conflict in round one")),
        conflict("kind", &[(text("task"), &["a"]), (text("note"), &["b"])])
    );
}

/// **Two defaults agree only where they write one value.** `1` and `1.0`
/// equal as numbers, but one is an integer and the other a float, which
/// write different bytes into the created document, so they conflict.
#[test]
fn an_integer_and_a_float_default_of_one_number_conflict() {
    let schema = b"version: 1
fields:
  n: { type: number }
rules:
  a: { required: { n: { default: 1 } } }
  b: { required: { n: { default: 1.0 } } }
";
    assert_eq!(
        conflicts(&fill(schema, "a.md", &[]).expect_err("an integer and a float disagree")),
        conflict(
            "n",
            &[
                (AuthoredValue::Integer(1), &["a"]),
                (
                    AuthoredValue::Float(norn_wire::FiniteFloat::new(1.0).expect("finite")),
                    &["b"]
                ),
            ]
        )
    );
}

/// **The caller's value wins and is never judged again**, though a rule it
/// brings in defaults the field differently; a creation rule's value is the
/// caller's frontmatter here alike.
#[test]
fn a_caller_value_wins_and_is_not_judged_again() {
    let schema = b"version: 1
rules:
  every: { required: { status: { default: todo } } }
  task: { match: { frontmatter: { kind: task } }, required: { status: { default: done } } }
";
    assert_eq!(
        fill(
            schema,
            "notes/a.md",
            &[("kind", text("task")), ("status", text("open"))]
        ),
        Ok(Vec::new())
    );
}

/// **A required field is missing where it is absent or null, and met by an
/// empty string or an empty list.**
#[test]
fn a_required_field_is_missing_where_absent_or_null() {
    let schema = b"version: 1\nrules:\n  every: { required: { status: { default: todo } } }\n";
    for missing in [vec![], vec![("status", AuthoredValue::Null)]] {
        assert_eq!(
            fill(schema, "a.md", &missing),
            Ok(filled(&[("status", text("todo"))])),
            "{missing:?}"
        );
    }
    for met in [text(""), AuthoredValue::list([])] {
        assert_eq!(
            fill(schema, "a.md", &[("status", met.clone())]),
            Ok(Vec::new()),
            "{met:?}"
        );
    }
}

/// **A required field nothing defaults stays missing**: refusing the
/// document is the write gate's, not the fixpoint's.
#[test]
fn a_required_field_nothing_defaults_stays_missing() {
    let schema =
        b"version: 1\nrules:\n  every: { required: { title: , status: { default: todo } } }\n";
    assert_eq!(
        fill(schema, "a.md", &[]),
        Ok(filled(&[("status", text("todo"))]))
    );
}

/// **A default reading a capture its match binds several ways refuses,
/// naming two of the bindings**; one bound uniquely fills.
#[test]
fn a_default_reading_a_capture_bound_several_ways_refuses_naming_the_bindings() {
    let schema = b"version: 1
rules:
  area: { match: { path: '**/<area>/**' }, required: { area: { default: '{{path.area}}' } } }
";
    let refusal = fill(schema, "red/blue/a.md", &[]).expect_err("an ambiguous capture");
    let RuleDefaultsRefusal::AmbiguousCapture {
        rule,
        field,
        bindings,
    } = &refusal
    else {
        panic!("{refusal}");
    };
    assert_eq!((rule.as_str(), field.as_str()), ("area", "area"));
    let bound: Vec<Option<&str>> = bindings
        .iter()
        .map(|captures| captures.get("area"))
        .collect();
    assert_eq!(bound, [Some("red"), Some("blue")]);
    let said = refusal.to_string();
    assert!(
        said.contains("area=red") && said.contains("area=blue"),
        "{said}"
    );

    let anchored = b"version: 1\nrules:\n  area: { match: { path: '<area>/**' }, required: { area: { default: '{{path.area}}' } } }\n";
    assert_eq!(
        fill(anchored, "red/blue/a.md", &[]),
        Ok(filled(&[("area", text("red"))]))
    );
    // A rule whose default reads no capture fills however its match binds.
    let plain = b"version: 1\nrules:\n  area: { match: { path: '**/<area>/**' }, required: { kind: { default: note } } }\n";
    assert_eq!(
        fill(plain, "red/blue/a.md", &[]),
        Ok(filled(&[("kind", text("note"))]))
    );
}

/// **Every default fills from the one clock reading handed in**, and each
/// capture from its own rule's binding of the created path.
#[test]
fn every_default_fills_from_the_one_clock_reading() {
    let schema = b"version: 1
rules:
  stamped:
    match: { path: 'projects/<project>/**' }
    required:
      created: { default: '{{now}}' }
      day: { default: '{{date}}' }
      clock: { default: '{{time}}' }
      project: { default: '[[{{path.project}}]]' }
      log: { default: ['{{date}}', fixed, 3] }
";
    assert_eq!(
        fill(schema, "projects/norn/a.md", &[]),
        Ok(filled(&[
            ("clock", text("09:30")),
            ("created", text("2026-10-06T09:30:15+02:00")),
            ("day", text("2026-10-06")),
            (
                "log",
                AuthoredValue::list([text("2026-10-06"), text("fixed"), AuthoredValue::Integer(3)])
            ),
            ("project", text("[[norn]]")),
        ]))
    );
}
