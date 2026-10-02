//! Filling a creation rule's templates: the values a document is made with.
//!
//! A fill reads no clock: every case hands over one local timestamp, so the
//! same inputs fill to the same text on every machine.

use std::collections::BTreeMap;

use norn_config::schema::{
    FillError, LocalTimestamp, NotALocalTimestamp, Template, TemplateValues, UnsafeValue,
    VaultSchema,
};
use norn_wire::{AuthoredValue, DocumentPath, PathProblem, ValueMap};

/// 2026-10-01 19:00:05, two hours east of UTC.
fn evening() -> LocalTimestamp {
    LocalTimestamp::new(2026, 10, 1, 19, 0, 5, 120).expect("a local timestamp")
}

fn values(variables: &[(&str, &str)]) -> TemplateValues {
    TemplateValues::new(
        variables
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect::<BTreeMap<_, _>>(),
        evening(),
    )
}

fn filled(source: &str, values: &TemplateValues) -> Result<String, FillError> {
    Template::parse(source).expect("a template").fill(values)
}

// ---- the tokens ----

#[test]
fn each_token_fills_to_its_value() {
    let values = values(&[("title", "Plan the week")]).with_seq(42);
    for (source, expected) in [
        ("{{now}}", "2026-10-01T19:00:05+02:00"),
        ("{{date}}", "2026-10-01"),
        ("{{time}}", "19:00"),
        ("{{seq}}", "42"),
        ("{{var.title}}", "Plan the week"),
        (
            "# {{var.title}} ({{date}} {{time}})\n",
            "# Plan the week (2026-10-01 19:00)\n",
        ),
        ("no tokens at all }}", "no tokens at all }}"),
    ] {
        assert_eq!(filled(source, &values).as_deref(), Ok(expected), "{source}");
    }
}

/// **`{{now}}` states the local offset**, west of UTC as `-`, and UTC itself
/// as `+00:00`; single-digit fields are padded.
#[test]
fn now_states_the_local_offset_whatever_its_sign() {
    for (offset, expected) in [
        (-330, "2026-01-02T03:04:05-05:30"),
        (0, "2026-01-02T03:04:05+00:00"),
        (765, "2026-01-02T03:04:05+12:45"),
    ] {
        let at = LocalTimestamp::new(2026, 1, 2, 3, 4, 5, offset).expect("a local timestamp");
        let values = TemplateValues::new(BTreeMap::new(), at);
        assert_eq!(filled("{{now}}", &values).as_deref(), Ok(expected));
        assert_eq!(
            filled("{{date}} {{time}}", &values).as_deref(),
            Ok("2026-01-02 03:04")
        );
    }
}

/// `{{seq}}` is a plain integer: no padding, however small.
#[test]
fn seq_fills_as_a_plain_integer() {
    for (seq, expected) in [(0, "0"), (7, "7"), (1000, "1000")] {
        assert_eq!(
            filled("{{seq}}", &values(&[]).with_seq(seq)).as_deref(),
            Ok(expected)
        );
    }
}

// ---- the slug filter ----

/// **`|slug` lowercases, keeps letters and digits, and joins the rest with one
/// `-`**: every run of anything else — spaces, punctuation, symbols — is one
/// `-`, none leads or trails, and letters outside ASCII are letters.
#[test]
fn slug_keeps_letters_and_digits_and_joins_the_rest_with_one_dash() {
    for (title, expected) in [
        ("Plan the week", "plan-the-week"),
        ("  Hello,   World!  ", "hello-world"),
        ("Ünïcode — Straße 42", "ünïcode-straße-42"),
        ("日本語 テキスト", "日本語-テキスト"),
        ("a/b\\c..d", "a-b-c-d"),
        ("ALL CAPS", "all-caps"),
        ("already-a-slug", "already-a-slug"),
    ] {
        assert_eq!(
            filled("{{var.title|slug}}", &values(&[("title", title)])).as_deref(),
            Ok(expected),
            "{title}"
        );
    }
}

/// **Canonically equal text slugs the same, and a combining mark stays with
/// its letter.** A slug reads its text normalized first, so an accent written
/// as its own mark slugs as the accented letter does; and a mark is part of
/// the word it is written in, so a script that writes vowels and viramas as
/// marks keeps its words whole, and the dot `İ` lowercases to keeps its `i`.
/// A mark that follows no word is part of the run between words.
#[test]
fn slug_keeps_combining_marks_with_their_letters() {
    for (title, expected) in [
        ("cafe\u{301}", "café"),
        ("café", "café"),
        ("Cafe\u{301} Noir", "café-noir"),
        ("हिन्दी", "हिन्दी"),
        ("हिन्दी भाषा", "हिन्दी-भाषा"),
        ("İstanbul", "i\u{307}stanbul"),
        ("a \u{301}b", "a-b"),
        ("\u{301}a", "a"),
    ] {
        assert_eq!(
            filled("{{var.title|slug}}", &values(&[("title", title)])).as_deref(),
            Ok(expected),
            "{title}"
        );
    }
}

/// **A slug of nothing slugable is empty**, and an empty value is no refusal
/// outside a target.
#[test]
fn a_slug_of_nothing_slugable_is_empty() {
    assert_eq!(
        filled("[{{var.title|slug}}]", &values(&[("title", " !?— ")])).as_deref(),
        Ok("[]")
    );
}

/// `|slug` applies to every token, not only to variables.
#[test]
fn slug_applies_to_every_token() {
    let values = values(&[]).with_seq(3);
    assert_eq!(
        filled(
            "{{now|slug}} {{date|slug}} {{time|slug}} {{seq|slug}}",
            &values
        )
        .as_deref(),
        Ok("2026-10-01t19-00-05-02-00 2026-10-01 19-00 3")
    );
}

// ---- what a fill refuses ----

#[test]
fn a_variable_no_value_is_supplied_for_is_named() {
    assert_eq!(
        filled("# {{var.title}}", &values(&[("project", "norn")])),
        Err(FillError::MissingVariable {
            name: "title".to_string()
        })
    );
}

#[test]
fn seq_with_no_sequence_number_supplied_is_refused() {
    assert_eq!(filled("{{seq}}", &values(&[])), Err(FillError::NoSeq));
}

/// **A body and a frontmatter string never refuse for their content**: a
/// value that would break a path, and an empty one, are text like any other
/// there.
#[test]
fn a_body_holds_any_value_it_is_supplied() {
    assert_eq!(
        filled(
            "{{var.a}}|{{var.b}}|{{var.c}}",
            &values(&[("a", "../x/y"), ("b", ""), ("c", "\\")])
        )
        .as_deref(),
        Ok("../x/y||\\")
    );
}

/// A schema whose `task` rule targets `target` and declares `project`.
fn task(target: &str) -> VaultSchema {
    VaultSchema::parse(
        format!(
            "version: 1\ncreatable:\n  task:\n    target: \"{target}\"\n    variables: [project]\n"
        )
        .as_bytes(),
    )
    .expect("a schema with a task rule")
}

fn filled_target(target: &str, values: &TemplateValues) -> Result<String, FillError> {
    task(target)
        .creation_rule("task")
        .expect("the task rule")
        .target()
        .fill(values)
        .map(|path| path.as_str().to_string())
}

#[test]
fn a_target_fills_to_the_path_a_document_is_written_at() {
    assert_eq!(
        filled_target(
            "tasks/{{var.project|slug}}/{{date}}-{{seq}}.md",
            &values(&[("project", "Norn Core")]).with_seq(12)
        )
        .as_deref(),
        Ok("tasks/norn-core/2026-10-01-12.md")
    );
}

/// **A value that would break the target's path is refused**, judged after
/// its filter: one holding a `/` or a `\`, one that is a `.` or `..`
/// segment, and one that is empty — written empty, or slugged to nothing.
#[test]
fn a_target_refuses_a_value_that_breaks_its_path() {
    for (target, project, unsafe_value) in [
        ("tasks/{{var.project}}.md", "a/b", UnsafeValue::Separator),
        ("tasks/{{var.project}}.md", "a\\b", UnsafeValue::Separator),
        ("tasks/{{var.project}}/x.md", "..", UnsafeValue::DotSegment),
        ("tasks/{{var.project}}/x.md", ".", UnsafeValue::DotSegment),
        ("tasks/{{var.project}}.md", "", UnsafeValue::Empty),
        ("tasks/{{var.project|slug}}.md", "?!", UnsafeValue::Empty),
    ] {
        let refused = filled_target(target, &values(&[("project", project)]))
            .expect_err("a value that breaks the path");
        let FillError::UnsafeValue {
            token,
            value,
            problem,
        } = &refused
        else {
            panic!("{refused}");
        };
        assert_eq!(problem, &unsafe_value, "{project:?}");
        assert!(token.starts_with("var.project"), "{token}");
        assert!(refused.to_string().contains("var.project"), "{refused}");
        let expected_value = if target.contains("slug") { "" } else { project };
        assert_eq!(value, expected_value);
    }
}

/// **A value holding `:` is refused in a target**, since `:` is not portable
/// in a file name; the same value is text like any other in a body.
#[test]
fn a_target_refuses_a_value_holding_a_colon() {
    for project in ["a:b", "C:", ":"] {
        let refused = filled_target("tasks/{{var.project}}.md", &values(&[("project", project)]))
            .expect_err("a value holding a colon");
        assert!(
            matches!(
                &refused,
                FillError::UnsafeValue {
                    problem: UnsafeValue::Colon,
                    ..
                }
            ),
            "{refused}"
        );
    }
}

/// **The empty value is refused before it can join two dots**: in
/// `..{{var.a}}`, an empty `a` would leave the segment `..`.
#[test]
fn an_empty_value_beside_dots_is_refused() {
    let refused = filled_target("..{{var.project}}/x.md", &values(&[("project", "")]))
        .expect_err("an empty value");
    assert!(
        matches!(
            &refused,
            FillError::UnsafeValue {
                problem: UnsafeValue::Empty,
                ..
            }
        ),
        "{refused}"
    );
}

/// **The whole filled target is judged as a document path**, so no value
/// reaches the store as a path it cannot hold, whatever literal text it
/// stands beside. A control character is no value rule's, and is refused
/// here; a numbered target is judged the same way, through its slot, and
/// names the path with `{{seq}}` as written.
#[test]
fn a_target_that_fills_to_no_document_path_is_refused() {
    for (target, project, path) in [
        ("tasks/{{var.project}}.md", "a\0b", "tasks/a\0b.md"),
        ("tasks/{{var.project}}.md", "a\u{7f}", "tasks/a\u{7f}.md"),
        (
            "tasks/{{var.project}}-{{seq}}.md",
            "a\tb",
            "tasks/a\tb-{{seq}}.md",
        ),
    ] {
        let refused = filled_target(target, &values(&[("project", project)]).with_seq(3))
            .expect_err("a target that fills to no document path");
        assert_eq!(
            refused,
            FillError::NotADocumentPath {
                path: path.to_string(),
                problem: PathProblem::ControlCharacter,
            },
            "{target}"
        );
    }
}

/// **No value joins the literal dots beside it into a `..`**: `.{{var.a}}`
/// filled with `.`, and `x/.{{var.a}}.md` filled with `.`, would climb out
/// of a folder or name a file the store keys no document by.
#[test]
fn a_value_joining_literal_dots_into_a_dot_segment_is_refused() {
    for target in [".{{var.project}}/x.md", "x/.{{var.project}}.md"] {
        filled_target(target, &values(&[("project", ".")]))
            .expect_err("a value that joins dots into `..`");
    }
}

/// The control: a value holding dots beside other text is a name like any
/// other, and so is one the slug makes safe.
#[test]
fn a_target_holds_a_value_whose_dots_stand_beside_other_text() {
    assert_eq!(
        filled_target("tasks/{{var.project}}.md", &values(&[("project", "v1..2")])).as_deref(),
        Ok("tasks/v1..2.md")
    );
    assert_eq!(
        filled_target(
            "tasks/{{var.project|slug}}.md",
            &values(&[("project", "a/b")])
        )
        .as_deref(),
        Ok("tasks/a-b.md")
    );
}

// ---- the sequence slot ----

/// **A target numbered by `{{seq}}` names the slot its numbers fill**: the
/// folder, and the file name's text before and after the number, with every
/// other value already filled. Rendering the slot at a number is the target
/// filled at that number.
#[test]
fn a_numbered_target_names_the_slot_its_numbers_fill() {
    let schema = task("tasks/{{var.project|slug}}/{{date}}-{{seq}}-x.md");
    let target = schema.creation_rule("task").expect("the rule").target();
    let values = values(&[("project", "Norn")]);
    let slot = target
        .seq_slot(&values)
        .expect("the values fill")
        .expect("a numbered target");
    assert_eq!(slot.folder(), "tasks/norn");
    assert_eq!(slot.prefix(), "2026-10-01-");
    assert_eq!(slot.suffix(), "-x.md");
    assert_eq!(slot.path(7), "tasks/norn/2026-10-01-7-x.md");
    assert_eq!(
        target
            .fill(&values.clone().with_seq(7))
            .as_ref()
            .map(DocumentPath::as_str),
        Ok(slot.path(7).as_str())
    );
}

#[test]
fn a_numbered_target_at_the_vault_root_has_an_empty_folder() {
    let schema = task("{{seq}}.md");
    let slot = schema
        .creation_rule("task")
        .expect("the rule")
        .target()
        .seq_slot(&values(&[]))
        .expect("the values fill")
        .expect("a numbered target");
    assert_eq!(
        (slot.folder(), slot.prefix(), slot.suffix()),
        ("", "", ".md")
    );
    assert_eq!(slot.path(1), "1.md");
}

#[test]
fn a_target_with_no_seq_names_no_slot() {
    let schema = task("notes/{{date}}.md");
    let target = schema.creation_rule("task").expect("the rule").target();
    assert_eq!(target.seq_slot(&values(&[])), Ok(None));
}

/// The slot refuses what the target refuses: a value that breaks the path.
#[test]
fn a_slot_refuses_a_value_that_breaks_its_path() {
    let schema = task("tasks/{{var.project}}/{{seq}}.md");
    let target = schema.creation_rule("task").expect("the rule").target();
    assert!(matches!(
        target.seq_slot(&values(&[("project", "..")])),
        Err(FillError::UnsafeValue {
            problem: UnsafeValue::DotSegment,
            ..
        })
    ));
}

// ---- frontmatter defaults ----

fn map(entries: Vec<(&str, AuthoredValue)>) -> AuthoredValue {
    AuthoredValue::map(
        entries
            .into_iter()
            .map(|(key, value)| (key.to_string(), value)),
    )
    .expect("each key once")
}

/// **Defaults fill every string scalar, however deep, and nothing else**:
/// numbers, booleans and nulls keep their types, and every map keeps the
/// order it is written in.
#[test]
fn frontmatter_defaults_fill_every_string_and_keep_every_type_and_order() {
    let schema = VaultSchema::parse(
        b"version: 1
creatable:
  task:
    target: \"tasks/{{seq}}.md\"
    variables: [project]
    frontmatter_defaults:
      status: todo
      created: \"{{now}}\"
      rank: 3
      done: false
      due: ~
      meta:
        project: \"{{var.project|slug}}\"
        days: [\"{{date}}\", 1.5, [\"{{time}}\"]]
",
    )
    .expect("a schema with defaults");
    let filled = schema
        .creation_rule("task")
        .expect("the rule")
        .fill_frontmatter_defaults(&values(&[("project", "Norn Core")]))
        .expect("the defaults fill");
    let AuthoredValue::Map(expected) = map(vec![
        ("status", AuthoredValue::string("todo")),
        (
            "created",
            AuthoredValue::string("2026-10-01T19:00:05+02:00"),
        ),
        ("rank", AuthoredValue::Integer(3)),
        ("done", AuthoredValue::Bool(false)),
        ("due", AuthoredValue::Null),
        (
            "meta",
            map(vec![
                ("project", AuthoredValue::string("norn-core")),
                (
                    "days",
                    AuthoredValue::list([
                        AuthoredValue::string("2026-10-01"),
                        AuthoredValue::float(1.5).expect("a finite float"),
                        AuthoredValue::list([AuthoredValue::string("19:00")]),
                    ]),
                ),
            ]),
        ),
    ]) else {
        unreachable!("a map")
    };
    assert_eq!(filled, expected);
}

#[test]
fn frontmatter_defaults_name_a_variable_no_value_is_supplied_for() {
    let schema = VaultSchema::parse(
        b"version: 1\ncreatable:\n  task:\n    target: \"t/{{seq}}.md\"\n    variables: [who]\n    frontmatter_defaults:\n      owner: [\"{{var.who}}\"]\n",
    )
    .expect("a schema with defaults");
    assert_eq!(
        schema
            .creation_rule("task")
            .expect("the rule")
            .fill_frontmatter_defaults(&values(&[])),
        Err(FillError::MissingVariable {
            name: "who".to_string()
        })
    );
}

#[test]
fn a_rule_with_no_defaults_fills_to_an_empty_map() {
    assert_eq!(
        task("t/{{seq}}.md")
            .creation_rule("task")
            .expect("the rule")
            .fill_frontmatter_defaults(&values(&[])),
        Ok(ValueMap::default())
    );
}

// ---- one instant ----

/// **One fill reads one instant.** Every template a rule fills from one set
/// of values states the same moment, and the same values fill the same text
/// every time: nothing a fill reads moves between two calls.
#[test]
fn one_set_of_values_fills_to_one_instant_every_time() {
    let schema = VaultSchema::parse(
        b"version: 1\ncreatable:\n  task:\n    target: \"t/{{now|slug}}-{{seq}}.md\"\n    frontmatter_defaults:\n      created: \"{{now}}\"\n    body: \"{{now}}\"\n",
    )
    .expect("a schema stating the instant three times");
    let rule = schema.creation_rule("task").expect("the rule");
    let values = values(&[]).with_seq(1);
    let fill = || {
        (
            rule.target().fill(&values),
            rule.fill_frontmatter_defaults(&values),
            rule.body().map(|body| body.fill(&values)),
        )
    };
    let first = fill();
    assert_eq!(first, fill());
    let (target, defaults, body) = first;
    assert_eq!(
        target.as_ref().map(DocumentPath::as_str),
        Ok("t/2026-10-01t19-00-05-02-00-1.md")
    );
    assert_eq!(
        defaults.expect("the defaults fill").entries(),
        [(
            "created".to_string(),
            AuthoredValue::string("2026-10-01T19:00:05+02:00")
        )]
    );
    assert_eq!(body, Some(Ok("2026-10-01T19:00:05+02:00".to_string())));
}

// ---- the timestamp ----

/// **A local timestamp is a moment a calendar and a clock have**: a day the
/// month has, a time inside the day with no leap second, a year written in
/// four digits, and an offset inside a day.
#[test]
fn a_local_timestamp_outside_the_calendar_is_refused() {
    for (year, month, day, hour, minute, second, offset) in [
        (2026, 13, 1, 0, 0, 0, 0),
        (2026, 0, 1, 0, 0, 0, 0),
        (2026, 2, 29, 0, 0, 0, 0),
        (2026, 4, 31, 0, 0, 0, 0),
        (2026, 1, 0, 0, 0, 0, 0),
        (2026, 1, 1, 24, 0, 0, 0),
        (2026, 1, 1, 0, 60, 0, 0),
        (2026, 1, 1, 0, 0, 60, 0),
        (2026, 1, 1, 0, 0, 0, 1440),
        (2026, 1, 1, 0, 0, 0, -1440),
        (10_000, 1, 1, 0, 0, 0, 0),
        (-1, 1, 1, 0, 0, 0, 0),
    ] {
        assert_eq!(
            LocalTimestamp::new(year, month, day, hour, minute, second, offset),
            Err(NotALocalTimestamp),
            "{year}-{month}-{day} {hour}:{minute}:{second} {offset}"
        );
    }
    LocalTimestamp::new(2028, 2, 29, 23, 59, 59, -1439).expect("a leap day, at its last second");
}
