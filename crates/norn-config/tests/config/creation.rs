//! Creation rules and the inbox: the schema sections that say how a new
//! document is made.
//!
//! Every case reads schema bytes a vault author could have written. The read
//! is pure, so a case needs no directory and no clock.

use norn_config::schema::{
    CreationProblem, Template, TemplateError, VaultSchema, VaultSchemaError,
};
use norn_wire::{AuthoredValue, PathProblem, ValueMap};

/// A schema declaring two creation rules and the inbox.
const CREATING: &[u8] = b"version: 1
creatable:
  task:
    target: \"tasks/{{var.project}}-{{seq}}.md\"
    variables: [project, title]
    frontmatter_defaults:
      status: todo
      created: \"{{now}}\"
      rank: 3
      meta:
        owner: \"{{var.project|slug}}\"
        tags: [work, \"{{date}}\"]
    body: \"# {{var.title}}\\n\"
  note:
    target: \"notes/{{date}}.md\"
inbox:
  target: \"inbox/{{date}}-{{seq}}.md\"
";

fn map(entries: Vec<(&str, AuthoredValue)>) -> AuthoredValue {
    AuthoredValue::map(
        entries
            .into_iter()
            .map(|(key, value)| (key.to_string(), value)),
    )
    .expect("each key once")
}

#[test]
fn a_schema_reads_its_creation_rules_by_name_and_its_inbox() {
    let schema = VaultSchema::parse(CREATING).expect("a schema with creation rules");

    let names: Vec<&str> = schema.creation_rules().map(|rule| rule.name()).collect();
    assert_eq!(names, ["note", "task"]);

    let task = schema.creation_rule("task").expect("the task rule");
    assert_eq!(task.target().as_str(), "tasks/{{var.project}}-{{seq}}.md");
    assert_eq!(task.variables(), ["project", "title"]);
    assert_eq!(
        task.body().map(|body| body.as_str()),
        Some("# {{var.title}}\n")
    );
    let AuthoredValue::Map(expected) = map(vec![
        ("status", AuthoredValue::string("todo")),
        ("created", AuthoredValue::string("{{now}}")),
        ("rank", AuthoredValue::Integer(3)),
        (
            "meta",
            map(vec![
                ("owner", AuthoredValue::string("{{var.project|slug}}")),
                (
                    "tags",
                    AuthoredValue::list([
                        AuthoredValue::string("work"),
                        AuthoredValue::string("{{date}}"),
                    ]),
                ),
            ]),
        ),
    ]) else {
        unreachable!("a map")
    };
    assert_eq!(task.frontmatter_defaults(), expected);

    let note = schema.creation_rule("note").expect("the note rule");
    assert_eq!(note.target().as_str(), "notes/{{date}}.md");
    assert!(note.variables().is_empty());
    assert_eq!(note.body(), None);
    assert_eq!(note.frontmatter_defaults(), ValueMap::default());

    assert_eq!(
        schema.inbox().map(|inbox| inbox.target().as_str()),
        Some("inbox/{{date}}-{{seq}}.md")
    );
}

#[test]
fn a_schema_without_creation_sections_declares_no_rule_and_no_inbox() {
    let schema = VaultSchema::parse(b"version: 1\n").expect("a bare schema");

    assert_eq!(schema.creation_rules().count(), 0);
    assert!(schema.creation_rule("task").is_none());
    assert!(schema.inbox().is_none());
}

/// **Creation rules derive nothing.** A rule says how a document is made; no
/// row a document's derivation writes reads it, so a schema declaring rules
/// and an inbox alone owes no document a re-derivation.
#[test]
fn a_schema_declaring_only_creation_rules_rederives_no_document() {
    let schema = VaultSchema::parse(CREATING).expect("a schema with creation rules");

    assert!(!schema.rederives_documents());
}

/// The refusal `bytes` reads to, which must be a creation refusal at `at`.
fn creation_refusal(bytes: &[u8]) -> (String, CreationProblem, String) {
    let error = VaultSchema::parse(bytes).expect_err("a refused creation section");
    let message = error.to_string();
    let VaultSchemaError::Creation { at, problem } = error else {
        panic!("not a creation refusal: {message}");
    };
    (at, problem, message)
}

/// A schema whose one rule `task` has `target` and declares `[project]`.
fn task_targeting(target: &str) -> Vec<u8> {
    format!("version: 1\ncreatable:\n  task:\n    target: \"{target}\"\n    variables: [project]\n")
        .into_bytes()
}

/// A schema whose one rule `task` declares `[project]`, targets
/// `tasks/{{seq}}.md`, and carries `extra` beneath it.
fn task_with(extra: &str) -> Vec<u8> {
    format!(
        "version: 1\ncreatable:\n  task:\n    target: \"tasks/{{{{seq}}}}.md\"\n    variables: [project]\n{extra}"
    )
    .into_bytes()
}

// ---- unknown keys ----

/// **An unknown key is refused at every level**: in a rule, and in the
/// inbox, which holds `target` alone.
#[test]
fn a_key_a_creation_section_does_not_hold_is_refused() {
    for (bytes, section, key) in [
        (task_with("    templat: x\n"), "creatable.task", "templat"),
        (
            b"version: 1\ninbox:\n  target: \"inbox/{{seq}}.md\"\n  variables: [x]\n".to_vec(),
            "inbox",
            "variables",
        ),
    ] {
        let error = VaultSchema::parse(&bytes).expect_err("a key the grammar does not hold");
        let VaultSchemaError::UnknownKey {
            section: refused,
            key: refused_key,
            ..
        } = &error
        else {
            panic!("{error}");
        };
        assert_eq!((refused.as_str(), refused_key.as_str()), (section, key));
    }
}

/// **A rule is keyed by its name, once.** YAML refuses a mapping that writes
/// one key twice, so two rules of one name never reach the grammar.
#[test]
fn a_rule_name_written_twice_is_refused_as_yaml() {
    let error = VaultSchema::parse(
        b"version: 1\ncreatable:\n  task:\n    target: \"a/{{seq}}.md\"\n  task:\n    target: \"b/{{seq}}.md\"\n",
    )
    .expect_err("a rule written twice");
    assert!(
        matches!(&error, VaultSchemaError::NotYaml { message } if message.contains("duplicate")),
        "{error}"
    );
}

#[test]
fn a_rule_whose_name_is_not_an_identifier_is_refused() {
    let (at, problem, message) = creation_refusal(
        b"version: 1\ncreatable:\n  \"my task\":\n    target: \"tasks/{{seq}}.md\"\n",
    );
    assert_eq!(at, "creatable");
    assert_eq!(
        problem,
        CreationProblem::RuleName {
            name: "my task".to_string()
        }
    );
    assert!(message.contains("`my task`"), "{message}");
}

#[test]
fn a_rule_without_a_target_is_told_the_target_is_absent() {
    let error = VaultSchema::parse(b"version: 1\ncreatable:\n  task:\n    variables: [x]\n")
        .expect_err("a rule with no target");
    assert_eq!(
        error,
        VaultSchemaError::Section {
            at: "creatable.task.target".to_string(),
            wanted: "a target path",
            found: "absent".to_string(),
        }
    );
}

// ---- the target ----

#[test]
fn a_target_that_does_not_end_in_md_is_refused() {
    for target in [
        "tasks/{{seq}}.txt",
        "tasks/{{seq}}",
        "tasks/{{var.project}}",
    ] {
        let (at, problem, _) = creation_refusal(&task_targeting(target));
        assert_eq!(
            (at.as_str(), problem),
            ("creatable.task.target", CreationProblem::NotMarkdown),
            "{target}"
        );
    }
}

#[test]
fn a_target_whose_file_name_is_md_alone_is_refused() {
    let (_, problem, _) = creation_refusal(&task_targeting("tasks/.md"));
    assert_eq!(problem, CreationProblem::FileNameEmpty);
}

#[test]
fn a_target_starting_at_the_filesystem_root_is_refused() {
    let (_, problem, message) = creation_refusal(&task_targeting("/tasks/{{seq}}.md"));
    assert_eq!(problem, CreationProblem::Path(PathProblem::Absolute));
    assert_eq!(
        message,
        "`creatable.task.target` names no document path: it is absolute, and a vault path is relative to the vault root"
    );
}

/// **A target stays inside the vault.** A `..` segment climbs out of it, and
/// a `.` segment is a second spelling of a folder; both are judged on the
/// literal text, wherever in the path they stand.
#[test]
fn a_target_with_a_dot_segment_is_refused() {
    for target in [
        "../tasks/{{seq}}.md",
        "tasks/../../{{seq}}.md",
        "./tasks/{{seq}}.md",
    ] {
        let (_, problem, _) = creation_refusal(&task_targeting(target));
        assert_eq!(
            problem,
            CreationProblem::Path(PathProblem::DotSegment),
            "{target}"
        );
    }
}

/// The control: a segment holding dots beside other text, or beside a token,
/// is a name like any other.
#[test]
fn a_target_segment_with_dots_beside_other_text_reads() {
    for target in ["tasks/..x/{{seq}}.md", "tasks/{{var.project}}../{{seq}}.md"] {
        VaultSchema::parse(&task_targeting(target)).expect(target);
    }
}

/// **Every segment names something**: a `//` and a trailing `/` hold an
/// empty segment, and an empty target names nothing at all.
#[test]
fn a_target_with_an_empty_segment_is_refused() {
    for target in ["tasks//{{seq}}.md", "tasks/"] {
        let (_, problem, _) = creation_refusal(&task_targeting(target));
        assert_eq!(
            problem,
            CreationProblem::Path(PathProblem::EmptySegment),
            "{target}"
        );
    }
    let (_, problem, _) = creation_refusal(&task_targeting(""));
    assert_eq!(problem, CreationProblem::Path(PathProblem::Empty));
}

#[test]
fn a_target_with_a_backslash_is_refused() {
    let (_, problem, _) = creation_refusal(&task_targeting("tasks\\\\{{seq}}.md"));
    assert_eq!(problem, CreationProblem::Path(PathProblem::Backslash));
}

/// **A `:` is not portable in a file name**: Windows reads it as a drive or
/// a stream, and macOS's Finder shows it as `/`. A literal `:` anywhere in a
/// target is refused, and so is `{{now}}` or `{{time}}` standing in one
/// unfiltered, since each writes the clock with `:` in it.
#[test]
fn a_target_that_would_hold_a_colon_is_refused() {
    for target in ["C:/x.md", "C:x.md", "x/a:b.md"] {
        let (_, problem, _) = creation_refusal(&task_targeting(target));
        assert_eq!(problem, CreationProblem::Colon, "{target}");
    }
    for (target, token) in [("x/{{now}}.md", "now"), ("x/{{time}}.md", "time")] {
        let (_, problem, message) = creation_refusal(&task_targeting(target));
        assert_eq!(
            problem,
            CreationProblem::ClockWithColon {
                token: token.to_string()
            },
            "{target}"
        );
        assert!(message.contains("|slug"), "{message}");
    }
    let (at, problem, _) =
        creation_refusal(b"version: 1\ninbox:\n  target: \"in/{{now}}-{{seq}}.md\"\n");
    assert_eq!(
        (at.as_str(), problem),
        (
            "inbox.target",
            CreationProblem::ClockWithColon {
                token: "now".to_string()
            }
        )
    );
}

/// The control: `{{now|slug}}` writes the clock with no `:`, and `{{date}}`
/// writes none to begin with.
#[test]
fn a_target_holding_the_clock_slugged_reads() {
    for target in [
        "x/{{now|slug}}-{{seq}}.md",
        "x/{{time|slug}}.md",
        "x/{{date}}.md",
    ] {
        VaultSchema::parse(&task_targeting(target)).expect(target);
    }
}

/// **A target is a path the store can hold.** A file name that is `.` or
/// `..` once its `.md` is dropped names no document the store keys, and a
/// control character is a byte no reader can print back; both are judged on
/// the literal text.
#[test]
fn a_target_the_store_cannot_hold_is_refused() {
    for target in ["x/..md", "x/...md"] {
        let (_, problem, _) = creation_refusal(&task_targeting(target));
        assert_eq!(
            problem,
            CreationProblem::Path(PathProblem::DotStem),
            "{target}"
        );
    }
    // YAML's own escapes, so the schema bytes carry the character itself.
    for target in ["x/\\x01{{seq}}.md", "x/\\t{{seq}}.md", "x\\x7f/{{seq}}.md"] {
        let (_, problem, _) = creation_refusal(&task_targeting(target));
        assert_eq!(
            problem,
            CreationProblem::Path(PathProblem::ControlCharacter),
            "{target}"
        );
    }
}

/// The control: dots before the `.md` beside other text, or beside a token,
/// make a name like any other.
#[test]
fn a_target_file_name_with_dots_beside_other_text_reads() {
    for target in [
        "x/.a.md",
        "x/a...md",
        "x/..{{seq}}.md",
        "x/.{{var.project}}.md",
    ] {
        VaultSchema::parse(&task_targeting(target)).expect(target);
    }
}

// ---- {{seq}} ----

#[test]
fn a_target_holding_seq_twice_is_refused() {
    let (_, problem, _) = creation_refusal(&task_targeting("tasks/{{seq}}-{{seq}}.md"));
    assert_eq!(problem, CreationProblem::SeqTwice);
}

#[test]
fn seq_in_a_folder_of_the_target_is_refused() {
    let (_, problem, message) = creation_refusal(&task_targeting("tasks/{{seq}}/note.md"));
    assert_eq!(problem, CreationProblem::SeqOutsideFileName);
    assert!(message.contains("{{seq}}"), "{message}");
}

/// **`{{seq}}` stands only in a target**: a body or a frontmatter default
/// holding one, however deep, is refused at the node holding it.
#[test]
fn seq_outside_the_target_is_refused() {
    for (extra, at) in [
        ("    body: \"No. {{seq}}\"\n", "creatable.task.body"),
        (
            "    frontmatter_defaults:\n      id: \"{{seq}}\"\n",
            "creatable.task.frontmatter_defaults.id",
        ),
        (
            "    frontmatter_defaults:\n      meta:\n        ids: [a, \"{{seq|slug}}\"]\n",
            "creatable.task.frontmatter_defaults.meta.ids.1",
        ),
    ] {
        let (refused, problem, _) = creation_refusal(&task_with(extra));
        assert_eq!(
            (refused.as_str(), problem),
            (at, CreationProblem::SeqOutsideTarget)
        );
    }
}

// ---- variables ----

/// **Every variable a rule names is one it declares**, wherever the rule
/// names it: the target, the body, or a string anywhere in its defaults.
#[test]
fn a_variable_the_rule_does_not_declare_is_refused_wherever_it_is_named() {
    for (bytes, at) in [
        (
            task_targeting("tasks/{{var.owner}}-{{seq}}.md"),
            "creatable.task.target",
        ),
        (
            task_with("    body: \"# {{var.title}}\\n\"\n"),
            "creatable.task.body",
        ),
        (
            task_with("    frontmatter_defaults:\n      who: [\"{{var.owner|slug}}\"]\n"),
            "creatable.task.frontmatter_defaults.who.0",
        ),
    ] {
        let (refused, problem, message) = creation_refusal(&bytes);
        let name = if at.ends_with("body") {
            "title"
        } else {
            "owner"
        };
        assert_eq!(
            (refused.as_str(), problem),
            (
                at,
                CreationProblem::UndeclaredVariable {
                    name: name.to_string()
                }
            )
        );
        assert!(message.contains(name), "{message}");
    }
}

#[test]
fn a_declared_variable_whose_name_is_not_an_identifier_is_refused() {
    for name in ["1st", "a.b", "has space"] {
        let bytes = format!(
            "version: 1\ncreatable:\n  task:\n    target: \"t/{{{{seq}}}}.md\"\n    variables: [\"{name}\"]\n"
        );
        let (at, problem, _) = creation_refusal(bytes.as_bytes());
        assert_eq!(
            (at.as_str(), problem),
            (
                "creatable.task.variables",
                CreationProblem::VariableName {
                    name: name.to_string()
                }
            )
        );
    }
}

#[test]
fn a_variable_declared_twice_is_refused() {
    let (_, problem, _) = creation_refusal(
        b"version: 1\ncreatable:\n  task:\n    target: \"t/{{seq}}.md\"\n    variables: [a, a]\n",
    );
    assert_eq!(
        problem,
        CreationProblem::VariableTwice {
            name: "a".to_string()
        }
    );
}

// ---- frontmatter default keys ----

/// **A default's key is a field name, written as it lands.** An empty key
/// names no field; `<<` is the merge key norn's own frontmatter reader
/// expands, so a default written under it would read back as a merge; and a
/// key is not a template, so one holding `{{` is a mistake. Each is refused at
/// the map holding it, at the top level and nested.
#[test]
fn a_frontmatter_default_key_that_would_not_land_as_written_is_refused() {
    for (key, problem) in [
        ("\"\"", CreationProblem::EmptyDefaultKey),
        ("<<", CreationProblem::MergeDefaultKey),
        ("\"<<\"", CreationProblem::MergeDefaultKey),
        (
            "\"{{var.project}}\"",
            CreationProblem::TemplatedDefaultKey {
                key: "{{var.project}}".to_string(),
            },
        ),
    ] {
        for (extra, at) in [
            (
                format!("    frontmatter_defaults:\n      {key}: x\n"),
                "creatable.task.frontmatter_defaults",
            ),
            (
                format!("    frontmatter_defaults:\n      meta:\n        {key}: x\n"),
                "creatable.task.frontmatter_defaults.meta",
            ),
            (
                format!("    frontmatter_defaults:\n      list: [{{{key}: x}}]\n"),
                "creatable.task.frontmatter_defaults.list.0",
            ),
        ] {
            let (refused, refused_problem, _) = creation_refusal(&task_with(&extra));
            assert_eq!(
                (refused.as_str(), refused_problem),
                (at, problem.clone()),
                "{extra}"
            );
        }
    }
}

/// The control: a key holding a lone brace, or `<` beside other text, is a
/// field name like any other.
#[test]
fn a_frontmatter_default_key_with_braces_or_angles_beside_other_text_reads() {
    VaultSchema::parse(&task_with(
        "    frontmatter_defaults:\n      \"{a}\": x\n      \"<<a\": y\n      \"a}}\": z\n",
    ))
    .expect("keys that are field names");
}

// ---- the inbox ----

#[test]
fn an_inbox_target_without_seq_is_refused() {
    let (at, problem, _) =
        creation_refusal(b"version: 1\ninbox:\n  target: \"inbox/{{date}}.md\"\n");
    assert_eq!(
        (at.as_str(), problem),
        ("inbox.target", CreationProblem::InboxWithoutSeq)
    );
}

#[test]
fn an_inbox_target_naming_a_variable_is_refused() {
    let (_, problem, _) =
        creation_refusal(b"version: 1\ninbox:\n  target: \"inbox/{{var.who}}-{{seq}}.md\"\n");
    assert_eq!(
        problem,
        CreationProblem::InboxVariable {
            name: "who".to_string()
        }
    );
}

/// The inbox's target is a target, judged as a rule's is.
#[test]
fn an_inbox_target_is_judged_as_a_target() {
    let (at, problem, _) = creation_refusal(b"version: 1\ninbox:\n  target: \"../{{seq}}.md\"\n");
    assert_eq!(
        (at.as_str(), problem),
        (
            "inbox.target",
            CreationProblem::Path(PathProblem::DotSegment)
        )
    );
}

// ---- the template grammar ----

/// **Every token is closed, named and filtered by the grammar**, and a
/// template that is not is refused where it is written.
#[test]
fn a_template_outside_the_grammar_is_refused() {
    for (body, expected) in [
        (
            "{{title}}",
            TemplateError::UnknownToken {
                token: "title".to_string(),
            },
        ),
        (
            "{{ now }}",
            TemplateError::UnknownToken {
                token: " now ".to_string(),
            },
        ),
        (
            "{{now|upper}}",
            TemplateError::UnknownFilter {
                token: "now|upper".to_string(),
                filter: "upper".to_string(),
            },
        ),
        (
            "{{date|slug|slug}}",
            TemplateError::UnknownFilter {
                token: "date|slug|slug".to_string(),
                filter: "slug|slug".to_string(),
            },
        ),
        (
            "{{var.a b}}",
            TemplateError::VariableName {
                name: "a b".to_string(),
            },
        ),
        ("x {{date", TemplateError::Unclosed { at: 2 }),
    ] {
        let (at, problem, message) =
            creation_refusal(&task_with(&format!("    body: \"{body}\"\n")));
        assert_eq!(at, "creatable.task.body");
        assert_eq!(problem, CreationProblem::Template(expected), "{body}");
        assert!(message.starts_with("`creatable.task.body` "), "{message}");
    }
}

/// The control: every token the grammar names reads, with and without the
/// filter, and text holding a lone `}}` or a single brace is text.
#[test]
fn every_token_the_grammar_names_reads() {
    let template = Template::parse("{{seq}} {{var.a_b-1}} {{now}} {{date|slug}} {{time}} }} { x }")
        .expect("a template");
    assert_eq!(template.variables().collect::<Vec<_>>(), ["a_b-1"]);
    assert_eq!(template.seq_count(), 1);
}

// ---- the shapes the grammar wants ----

#[test]
fn a_creation_section_of_the_wrong_shape_names_itself() {
    for (bytes, at, wanted) in [
        (
            b"version: 1\ncreatable: [task]\n".to_vec(),
            "creatable",
            "a mapping of rule name to rule",
        ),
        (
            b"version: 1\ninbox: inbox/\n".to_vec(),
            "inbox",
            "a mapping",
        ),
        (
            task_with("    body: [a]\n"),
            "creatable.task.body",
            "a string",
        ),
        (
            task_with("    frontmatter_defaults: [a]\n"),
            "creatable.task.frontmatter_defaults",
            "a mapping",
        ),
        (
            task_with("    frontmatter_defaults:\n      when: !custom x\n"),
            "creatable.task.frontmatter_defaults.when",
            "a frontmatter value",
        ),
        (
            task_with("    frontmatter_defaults:\n      big: 18446744073709551615\n"),
            "creatable.task.frontmatter_defaults.big",
            "an integer that fits a signed 64-bit integer",
        ),
        (
            task_with("    frontmatter_defaults:\n      odd: .nan\n"),
            "creatable.task.frontmatter_defaults.odd",
            "a finite number",
        ),
    ] {
        let error = VaultSchema::parse(&bytes).expect_err("a section of the wrong shape");
        let VaultSchemaError::Section {
            at: refused,
            wanted: refused_wanted,
            ..
        } = &error
        else {
            panic!("{error}");
        };
        assert_eq!((refused.as_str(), *refused_wanted), (at, wanted));
    }
}

/// **A default keeps the type and order it is written in**: a number stays a
/// number and a boolean a boolean, nested maps and lists keep their order,
/// and only a string is a template.
#[test]
fn frontmatter_defaults_keep_their_types_and_order() {
    let schema = VaultSchema::parse(&task_with(
        "    frontmatter_defaults:\n      z: 1.5\n      a: true\n      m: ~\n      n: [2, \"{{date}}\", {k: v}]\n",
    ))
    .expect("defaults of every shape");
    let defaults = schema
        .creation_rule("task")
        .expect("the rule")
        .frontmatter_defaults();
    let expected = vec![
        (
            "z".to_string(),
            AuthoredValue::float(1.5).expect("a finite float"),
        ),
        ("a".to_string(), AuthoredValue::Bool(true)),
        ("m".to_string(), AuthoredValue::Null),
        (
            "n".to_string(),
            AuthoredValue::list([
                AuthoredValue::Integer(2),
                AuthoredValue::string("{{date}}"),
                map(vec![("k", AuthoredValue::string("v"))]),
            ]),
        ),
    ];
    assert_eq!(defaults.entries(), expected.as_slice());
}
