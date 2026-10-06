//! Schema rules: the grammar, what schema read refuses of a rule, selection
//! and the combined constraint.
//!
//! Every case reads schema bytes a vault author could have written. The read
//! is pure, so a case needs no directory and no clock.

use norn_config::schema::{
    CaseFold, CreationProblem, ElementProblem, FieldType, ForbiddenFix, GlobProblem,
    PLACEMENT_CEILING, RuleProblem, RuleWork, RulesConflict, Shape, TypedValue, VaultSchema,
    VaultSchemaError,
};
use norn_wire::{AuthoredValue, PathProblem, Severity, ValueMap};

/// A schema holding one rule of every key the grammar has.
const TASK: &[u8] = b"version: 1
fields:
  status: { type: text, shape: single }
  due: { type: date }
rules:
  task:
    description: A tracked task
    severity: error
    match:
      frontmatter: { type: [task, chore] }
      path: 'projects/<project>/**'
    exclude:
      path: ['projects/*/archive/**']
    required:
      status: { default: todo }
      title:
    forbidden:
      due_date: { rename_to: due }
      scratch: remove
      legacy:
    one_of:
      status: { values: [todo, doing, done], synonyms: { complete: done } }
    max_length: { title: 120 }
    allowed_paths:
      paths: ['projects/*/tasks/**']
      route: 'projects/{{path.project}}/tasks/'
";

fn refused(bytes: &[u8]) -> VaultSchemaError {
    VaultSchema::parse(bytes).expect_err("a schema refused at read")
}

/// The node a rule refusal names and what it says is wrong there.
fn rule_problem(bytes: &[u8]) -> (String, RuleProblem) {
    match refused(bytes) {
        VaultSchemaError::Rule { at, problem } => (at, problem),
        other => panic!("a rule refusal: {other}"),
    }
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

// ---- the grammar ----

#[test]
fn a_rule_reads_every_key_of_the_grammar() {
    let schema = VaultSchema::parse(TASK).expect("a schema with a rule");
    let names: Vec<&str> = schema.rules().map(|rule| rule.name()).collect();
    assert_eq!(names, ["task"]);
    let task = schema.rule("task").expect("the task rule");
    assert_eq!(task.description(), Some("A tracked task"));
    assert_eq!(task.severity(), Severity::Error);

    let selector = task.selector();
    let selecting: Vec<(&str, Vec<&str>)> = selector
        .frontmatter()
        .map(|(key, values)| (key, values.iter().map(String::as_str).collect()))
        .collect();
    assert_eq!(selecting, [("type", vec!["task", "chore"])]);
    let path = selector.path().expect("a match.path");
    assert_eq!(path.as_str(), "projects/<project>/**");
    assert_eq!(path.captures().collect::<Vec<_>>(), ["project"]);
    assert_eq!(
        selector
            .exclude()
            .iter()
            .map(|glob| glob.as_str())
            .collect::<Vec<_>>(),
        ["projects/*/archive/**"]
    );
    assert!(!selector.is_selectorless());

    let required: Vec<(&str, Option<AuthoredValue>)> = task
        .required()
        .map(|(field, default)| (field, default.map(|default| default.source())))
        .collect();
    assert_eq!(required, [("status", Some(text("todo"))), ("title", None)]);
    let forbidden: Vec<(&str, &ForbiddenFix)> = task.forbidden().collect();
    assert_eq!(
        forbidden,
        [
            ("due_date", &ForbiddenFix::RenameTo("due".to_string())),
            ("legacy", &ForbiddenFix::Unfixed),
            ("scratch", &ForbiddenFix::Remove),
        ]
    );
    let (field, status) = task.one_of().next().expect("a closed set");
    assert_eq!(field, "status");
    assert_eq!(status.values(), ["todo", "doing", "done"]);
    assert_eq!(
        status.synonyms().collect::<Vec<_>>(),
        [("complete", "done")]
    );
    assert_eq!(task.max_length().collect::<Vec<_>>(), [("title", 120)]);
    let allowed = task.allowed_paths().expect("allowed paths");
    assert_eq!(
        allowed
            .paths()
            .iter()
            .map(|glob| glob.as_str())
            .collect::<Vec<_>>(),
        ["projects/*/tasks/**"]
    );
    assert_eq!(
        allowed.route().map(|route| route.as_str()),
        Some("projects/{{path.project}}/tasks/")
    );
    assert_eq!(
        task.placement_weight(),
        "projects/*/tasks/**".len() as u64 + 1
    );
}

/// **A rule states its severity or is a warning, and a rule written with
/// nothing under its name selects every document and constrains none.**
#[test]
fn a_rule_without_a_severity_is_a_warning_and_an_empty_rule_is_selectorless() {
    let schema = VaultSchema::parse(b"version: 1\nrules:\n  quiet:\n  noted: { description: x }\n")
        .expect("two bare rules");
    for name in ["quiet", "noted"] {
        let rule = schema.rule(name).expect("the rule");
        assert_eq!(rule.severity(), Severity::Warning);
        assert!(rule.selector().is_selectorless());
        assert_eq!(rule.required().count(), 0);
        assert_eq!(rule.placement_weight(), 0);
    }
}

#[test]
fn a_field_declares_a_shape_or_admits_either() {
    let schema = VaultSchema::parse(
        b"version: 1\nfields:\n  one: { type: text, shape: single }\n  many: { type: tags, shape: list }\n  either: { type: number }\n",
    )
    .expect("shapes");
    let shape = |key: &str| schema.field(key).expect("declared").shape();
    assert_eq!(shape("one"), Some(Shape::Single));
    assert_eq!(shape("many"), Some(Shape::List));
    assert_eq!(shape("either"), None);
    for spelling in ["single", "list"] {
        assert_eq!(Shape::named(spelling).map(Shape::as_str), Some(spelling));
    }
}

#[test]
fn a_shape_the_grammar_does_not_name_is_refused() {
    assert_eq!(
        refused(b"version: 1\nfields:\n  status: { shape: many }\n"),
        VaultSchemaError::Section {
            at: "fields.status.shape".to_string(),
            wanted: "`single` or `list`",
            found: "a string".to_string(),
        }
    );
}

/// **Two rules of one name are refused, naming the name.** `rules:` is a
/// mapping keyed by name, so the second is a key written twice, which the
/// YAML reader refuses rather than keeping either.
#[test]
fn two_rules_of_one_name_are_refused_naming_the_name() {
    let error = refused(
        b"version: 1\nrules:\n  task:\n    required: { status: }\n  task:\n    forbidden: { status: }\n",
    );
    assert!(
        matches!(&error, VaultSchemaError::NotYaml { .. }),
        "{error:?}"
    );
    assert!(error.to_string().contains("\"task\""), "{error}");
}

#[test]
fn a_rule_name_that_is_not_an_identifier_is_refused() {
    let (at, problem) = rule_problem(b"version: 1\nrules:\n  'two words': {}\n");
    assert_eq!(at, "rules");
    assert_eq!(
        problem,
        RuleProblem::Name {
            name: "two words".to_string()
        }
    );
}

/// **An unknown key is refused at every level of a rule**, as it is
/// everywhere in the grammar: each names the key and the node holding it.
#[test]
fn an_unknown_key_is_refused_at_every_level_of_a_rule() {
    let cases: &[(&[u8], &str, &str)] = &[
        (
            b"version: 1\nrules:\n  t: { requires: {} }\n",
            "rules.t",
            "requires",
        ),
        (
            b"version: 1\nrules:\n  t: { match: { tags: x } }\n",
            "rules.t.match",
            "tags",
        ),
        (
            b"version: 1\nrules:\n  t: { exclude: { glob: [] } }\n",
            "rules.t.exclude",
            "glob",
        ),
        (
            b"version: 1\nrules:\n  t: { required: { a: { value: 1 } } }\n",
            "rules.t.required.a",
            "value",
        ),
        (
            b"version: 1\nrules:\n  t: { forbidden: { a: { move_to: b } } }\n",
            "rules.t.forbidden.a",
            "move_to",
        ),
        (
            b"version: 1\nrules:\n  t: { one_of: { a: { values: [x], aliases: {} } } }\n",
            "rules.t.one_of.a",
            "aliases",
        ),
        (
            b"version: 1\nrules:\n  t: { allowed_paths: { paths: [a], fix: b/ } }\n",
            "rules.t.allowed_paths",
            "fix",
        ),
    ];
    for (bytes, section, key) in cases {
        let error = refused(bytes);
        let VaultSchemaError::UnknownKey {
            section: refused_section,
            key: refused_key,
            ..
        } = &error
        else {
            panic!("{error}");
        };
        assert_eq!(
            (refused_section.as_str(), refused_key.as_str()),
            (*section, *key)
        );
    }
}

/// **Every node of the wrong shape names itself**: there are no list
/// shorthands, a severity is one of two words, a length limit is a positive
/// integer, a closed set and the allowed paths hold at least one member, and
/// an any-of list at least one value.
#[test]
fn a_rule_node_of_the_wrong_shape_names_itself() {
    let cases: &[(&[u8], &str)] = &[
        (b"version: 1\nrules: [task]\n", "rules"),
        (b"version: 1\nrules:\n  t: [a]\n", "rules.t"),
        (
            b"version: 1\nrules:\n  t: { description: 3 }\n",
            "rules.t.description",
        ),
        (
            b"version: 1\nrules:\n  t: { severity: fatal }\n",
            "rules.t.severity",
        ),
        (
            b"version: 1\nrules:\n  t: { required: [status] }\n",
            "rules.t.required",
        ),
        (
            b"version: 1\nrules:\n  t: { required: { status: todo } }\n",
            "rules.t.required.status",
        ),
        (
            b"version: 1\nrules:\n  t: { required: { status: { default: null } } }\n",
            "rules.t.required.status.default",
        ),
        (
            b"version: 1\nrules:\n  t: { required: { status: { default: { a: 1 } } } }\n",
            "rules.t.required.status.default",
        ),
        (
            b"version: 1\nrules:\n  t: { required: { status: { default: [[a]] } } }\n",
            "rules.t.required.status.default.0",
        ),
        (
            b"version: 1\nrules:\n  t: { forbidden: [status] }\n",
            "rules.t.forbidden",
        ),
        (
            b"version: 1\nrules:\n  t: { forbidden: { status: drop } }\n",
            "rules.t.forbidden.status",
        ),
        (
            b"version: 1\nrules:\n  t: { forbidden: { status: { rename_to: '' } } }\n",
            "rules.t.forbidden.status.rename_to",
        ),
        (
            b"version: 1\nrules:\n  t: { one_of: { status: [a, b] } }\n",
            "rules.t.one_of.status",
        ),
        (
            b"version: 1\nrules:\n  t: { one_of: { status: { values: [] } } }\n",
            "rules.t.one_of.status.values",
        ),
        (
            b"version: 1\nrules:\n  t: { one_of: { status: { synonyms: { a: b } } } }\n",
            "rules.t.one_of.status.values",
        ),
        (
            b"version: 1\nrules:\n  t: { one_of: { status: { values: [a], synonyms: [a] } } }\n",
            "rules.t.one_of.status.synonyms",
        ),
        (
            b"version: 1\nrules:\n  t: { max_length: { title: 0 } }\n",
            "rules.t.max_length.title",
        ),
        (
            b"version: 1\nrules:\n  t: { max_length: { title: -3 } }\n",
            "rules.t.max_length.title",
        ),
        (
            b"version: 1\nrules:\n  t: { max_length: { title: '120' } }\n",
            "rules.t.max_length.title",
        ),
        (
            b"version: 1\nrules:\n  t: { allowed_paths: { route: a/ } }\n",
            "rules.t.allowed_paths.paths",
        ),
        (
            b"version: 1\nrules:\n  t: { allowed_paths: { paths: [] } }\n",
            "rules.t.allowed_paths.paths",
        ),
        (
            b"version: 1\nrules:\n  t: { allowed_paths: { paths: a } }\n",
            "rules.t.allowed_paths.paths",
        ),
        (
            b"version: 1\nrules:\n  t: { exclude: { path: 'a/**' } }\n",
            "rules.t.exclude.path",
        ),
        (
            b"version: 1\nrules:\n  t: { match: { frontmatter: { type: [] } } }\n",
            "rules.t.match.frontmatter.type",
        ),
        (
            b"version: 1\nrules:\n  t: { match: { frontmatter: { type: null } } }\n",
            "rules.t.match.frontmatter.type",
        ),
        (
            b"version: 1\nrules:\n  t: { match: { frontmatter: { type: { a: 1 } } } }\n",
            "rules.t.match.frontmatter.type",
        ),
        (
            b"version: 1\nrules:\n  t: { match: { path: '' } }\n",
            "rules.t.match.path",
        ),
    ];
    for (bytes, node) in cases {
        let error = refused(bytes);
        let VaultSchemaError::Section { at, .. } = &error else {
            panic!("{node}: {error}");
        };
        assert_eq!(at, node, "{error}");
        assert!(error.to_string().contains(node), "{error}");
    }
}

/// **A selector value that does not read as its key's declared type is
/// refused**: no value would ever equal it.
#[test]
fn a_selector_value_that_does_not_read_as_its_type_is_refused() {
    let (at, problem) = rule_problem(
        b"version: 1\nfields:\n  priority: { type: number }\nrules:\n  t: { match: { frontmatter: { priority: [1, high] } } }\n",
    );
    assert_eq!(at, "rules.t.match.frontmatter.priority");
    assert_eq!(
        problem,
        RuleProblem::SelectorValue {
            value: "high".to_string(),
            declared: FieldType::Number,
        }
    );
}

// ---- captures ----

fn glob_problem(bytes: &[u8]) -> (String, String, GlobProblem) {
    match refused(bytes) {
        VaultSchemaError::Glob { at, glob, problem } => (at, glob, problem),
        other => panic!("a glob refusal: {other}"),
    }
}

#[test]
fn a_capture_is_named_by_an_identifier() {
    for (glob, name) in [("<2x>/**", "2x"), ("<a b>/**", "a b"), ("<>/**", "")] {
        let bytes = format!("version: 1\nrules:\n  t: {{ match: {{ path: '{glob}' }} }}\n");
        let (at, written, problem) = glob_problem(bytes.as_bytes());
        assert_eq!(
            (at.as_str(), written.as_str()),
            ("rules.t.match.path", glob)
        );
        assert_eq!(
            problem,
            GlobProblem::CaptureName {
                name: name.to_string()
            }
        );
    }
}

/// **A capture binds a whole segment or nothing**: `p-<id>.md` is refused
/// rather than read as a literal, so a capture spelling never silently
/// matches as text.
#[test]
fn a_capture_is_a_whole_segment() {
    for (glob, segment) in [
        ("tasks/p-<id>.md", "p-<id>.md"),
        ("a/<x>y", "<x>y"),
        ("a/b>", "b>"),
    ] {
        let bytes = format!("version: 1\nrules:\n  t: {{ match: {{ path: '{glob}' }} }}\n");
        let (_, _, problem) = glob_problem(bytes.as_bytes());
        assert_eq!(
            problem,
            GlobProblem::CaptureNotWholeSegment {
                segment: segment.to_string()
            }
        );
    }
}

#[test]
fn a_capture_is_written_once_in_one_match_path() {
    let (_, _, problem) =
        glob_problem(b"version: 1\nrules:\n  t: { match: { path: '<a>/x/<a>/**' } }\n");
    assert_eq!(
        problem,
        GlobProblem::CaptureTwice {
            name: "a".to_string()
        }
    );
}

/// **Only `match.path` binds a capture.** In a rule glob `<` and `>` spell a
/// capture, so the rule's excluded and allowed paths refuse them rather than
/// read them literally.
#[test]
fn a_capture_binds_only_in_match_path() {
    let cases: &[(&[u8], &str)] = &[
        (
            b"version: 1\nrules:\n  t: { exclude: { path: ['<a>/**'] } }\n",
            "rules.t.exclude.path",
        ),
        (
            b"version: 1\nrules:\n  t: { allowed_paths: { paths: ['<a>/**'] } }\n",
            "rules.t.allowed_paths.paths",
        ),
        (
            b"version: 1\nrules:\n  t: { allowed_paths: { paths: ['a/b>/*.md'] } }\n",
            "rules.t.allowed_paths.paths",
        ),
    ];
    for (bytes, node) in cases {
        let (at, _, problem) = glob_problem(bytes);
        assert_eq!(
            (at.as_str(), problem),
            (*node, GlobProblem::CaptureOutsideMatch)
        );
    }
    // A wildcard matches a literal `<x>` segment where no capture binds.
    let schema =
        VaultSchema::parse(b"version: 1\nrules:\n  t: { exclude: { path: ['?x?/**'] } }\n")
            .expect("a wildcard in place of `<` and `>`");
    let rule = schema.rule("t").expect("the rule");
    assert!(!schema.selects(rule, "<x>/a.md", &frontmatter(&[]), CaseFold::Exact));
}

/// **The tag patterns and the ambiguity-ignore set read no capture**, so
/// `<name>` there is the literal text it spells, as it was before rules.
#[test]
fn a_glob_outside_the_rules_reads_angle_brackets_literally() {
    let schema = VaultSchema::parse(
        b"version: 1\ntags:\n  patterns: ['<a>/**']\npaths:\n  ambiguity_ignore: ['<a>/**']\n",
    )
    .expect("angle brackets in the tag patterns and the ambiguity-ignore set");
    let ignored = schema.ambiguity_ignore();
    assert_eq!(ignored.len(), 1);
    assert!(ignored[0].matches("<a>/x.md", CaseFold::Exact));
    assert!(!ignored[0].matches("b/x.md", CaseFold::Exact));
    assert_eq!(ignored[0].captures().count(), 0);
}

#[test]
fn a_rule_glob_holds_no_empty_segment() {
    for glob in ["/a/**", "a//b", "a/"] {
        for node in [
            "match: { path: 'GLOB' }",
            "exclude: { path: ['GLOB'] }",
            "allowed_paths: { paths: ['GLOB'] }",
        ] {
            let bytes = format!(
                "version: 1\nrules:\n  t: {{ {} }}\n",
                node.replace("GLOB", glob)
            );
            let (_, _, problem) = glob_problem(bytes.as_bytes());
            assert_eq!(problem, GlobProblem::EmptySegment, "{bytes}");
        }
    }
}

// ---- a rule's templates ----

/// **A rule's default or route admits the clock's tokens and its own
/// captures alone.** `{{seq}}` and `{{var.NAME}}` belong to creation rules.
#[test]
fn a_token_a_rule_default_or_route_does_not_admit_is_refused() {
    for (token, spelled) in [
        ("{{seq}}", "seq"),
        ("{{var.project}}", "var.project"),
        ("{{var.project|slug}}", "var.project"),
    ] {
        let default = format!(
            "version: 1\nrules:\n  t: {{ required: {{ status: {{ default: 'x-{token}' }} }} }}\n"
        );
        let (at, problem) = rule_problem(default.as_bytes());
        assert_eq!(at, "rules.t.required.status.default");
        assert_eq!(
            problem,
            RuleProblem::InadmissibleToken {
                token: spelled.to_string()
            }
        );
        let route = format!(
            "version: 1\nrules:\n  t: {{ allowed_paths: {{ paths: ['**'], route: 'a/{token}/' }} }}\n"
        );
        let (at, problem) = rule_problem(route.as_bytes());
        assert_eq!(at, "rules.t.allowed_paths.route");
        assert_eq!(
            problem,
            RuleProblem::InadmissibleToken {
                token: spelled.to_string()
            }
        );
    }
}

/// **A capture a default or route reads is one its own rule's `match.path`
/// defines** — not another rule's, and not one no path captures.
#[test]
fn a_capture_its_own_match_path_does_not_define_is_refused() {
    let cases: &[&[u8]] = &[
        b"version: 1\nrules:\n  t: { required: { project: { default: '{{path.project}}' } } }\n",
        b"version: 1\nrules:\n  t: { match: { path: 'p/<area>/**' }, required: { project: { default: '{{path.project}}' } } }\n",
        b"version: 1\nrules:\n  o: { match: { path: '<project>/**' } }\n  t: { required: { project: { default: [a, '{{path.project}}'] } } }\n",
        b"version: 1\nrules:\n  t: { match: { path: 'p/<area>/**' }, allowed_paths: { paths: ['**'], route: '{{path.project}}/' } }\n",
    ];
    for bytes in cases {
        let (_, problem) = rule_problem(bytes);
        assert_eq!(
            problem,
            RuleProblem::UndefinedCapture {
                name: "project".to_string()
            }
        );
    }
    VaultSchema::parse(
        b"version: 1\nrules:\n  t: { match: { path: 'p/<project>/**' }, required: { project: { default: '[[{{path.project|slug}}]]' } } }\n",
    )
    .expect("a default reading its own rule's capture");
}

/// **A creation rule reads no path capture**, in its target, its body or its
/// defaults, and neither does the inbox: a creation rule matches no path.
#[test]
fn a_path_capture_is_refused_in_a_creation_rule_and_the_inbox() {
    let cases: &[(&[u8], &str)] = &[
        (
            b"version: 1\ncreatable:\n  task:\n    target: 'tasks/{{path.project}}.md'\n",
            "creatable.task.target",
        ),
        (
            b"version: 1\ncreatable:\n  task:\n    target: 'tasks/a.md'\n    body: '{{path.project}}'\n",
            "creatable.task.body",
        ),
        (
            b"version: 1\ncreatable:\n  task:\n    target: 'tasks/a.md'\n    frontmatter_defaults: { project: '{{path.project}}' }\n",
            "creatable.task.frontmatter_defaults.project",
        ),
        (
            b"version: 1\ninbox:\n  target: 'inbox/{{path.project}}-{{seq}}.md'\n",
            "inbox.target",
        ),
    ];
    for (bytes, node) in cases {
        assert_eq!(
            refused(bytes),
            VaultSchemaError::Creation {
                at: node.to_string(),
                problem: CreationProblem::PathCapture {
                    name: "project".to_string()
                },
            }
        );
    }
}

// ---- a rule against itself ----

/// **An untemplated default is judged as the whole value it fills**: its
/// type and shape against the field's declaration, and each element against
/// its own rule's `one_of` and `max_length`.
#[test]
fn an_untemplated_default_its_own_rule_refuses_is_refused() {
    let cases: &[(&[u8], RuleProblem)] = &[
        (
            b"version: 1\nfields:\n  priority: { type: number }\nrules:\n  t: { required: { priority: { default: high } } }\n",
            RuleProblem::Default {
                value: "high".to_string(),
                problem: ElementProblem::NotType(FieldType::Number),
            },
        ),
        (
            b"version: 1\nfields:\n  done: { type: boolean }\nrules:\n  t: { required: { done: { default: 1 } } }\n",
            RuleProblem::Default {
                value: "1".to_string(),
                problem: ElementProblem::NotType(FieldType::Boolean),
            },
        ),
        (
            b"version: 1\nfields:\n  status: { shape: single }\nrules:\n  t: { required: { status: { default: [todo] } } }\n",
            RuleProblem::DefaultShape {
                declared: Shape::Single,
            },
        ),
        (
            b"version: 1\nfields:\n  owners: { shape: list }\nrules:\n  t: { required: { owners: { default: drew } } }\n",
            RuleProblem::DefaultShape {
                declared: Shape::List,
            },
        ),
        (
            b"version: 1\nfields:\n  owners: { shape: list }\nrules:\n  t: { required: { owners: { default: '{{date}}' } } }\n",
            RuleProblem::DefaultShape {
                declared: Shape::List,
            },
        ),
        (
            b"version: 1\nrules:\n  t: { required: { status: { default: open } }, one_of: { status: { values: [todo, done] } } }\n",
            RuleProblem::Default {
                value: "open".to_string(),
                problem: ElementProblem::OutsideOneOf,
            },
        ),
        (
            b"version: 1\nrules:\n  t: { required: { title: { default: untitled } }, max_length: { title: 5 } }\n",
            RuleProblem::Default {
                value: "untitled".to_string(),
                problem: ElementProblem::TooLong(5),
            },
        ),
        (
            b"version: 1\nrules:\n  t: { required: { status: { default: [todo, open] } }, one_of: { status: { values: [todo, done] } } }\n",
            RuleProblem::Default {
                value: "open".to_string(),
                problem: ElementProblem::OutsideOneOf,
            },
        ),
    ];
    for (bytes, expected) in cases {
        let (at, problem) = rule_problem(bytes);
        assert!(at.ends_with(".default"), "{at}");
        assert_eq!(&problem, expected);
    }
}

/// **A templated default is judged where a document is, and a length counts
/// Unicode scalar values.** A template's literal text is no value, and a
/// limit of four admits a four-character word however many bytes it takes.
#[test]
fn a_templated_default_loads_and_a_length_counts_scalar_values() {
    VaultSchema::parse(
        b"version: 1\nfields:\n  due: { type: date }\nrules:\n  t: { required: { due: { default: '{{date}}' }, title: { default: '{{now}} and more' } }, max_length: { title: 4 } }\n",
    )
    .expect("templated defaults are judged where a document is");
    VaultSchema::parse(
        "version: 1\nrules:\n  t: { required: { title: { default: 'café' } }, max_length: { title: 4 } }\n"
            .as_bytes(),
    )
    .expect("four scalar values in five bytes");
}

/// **A list-shaped field takes scalar members, and a list default is judged
/// whole.** Collection shape is judged on the composed field; each member,
/// synonym target and default element is one element value.
#[test]
fn a_list_shaped_field_takes_scalar_members_and_a_list_default_is_judged_whole() {
    let schema = VaultSchema::parse(
        b"version: 1
fields:
  status: { type: text, shape: list }
rules:
  t:
    required:
      status: { default: [todo, done] }
    one_of:
      status: { values: [todo, done], synonyms: { complete: done } }
",
    )
    .expect("scalar members of a list-shaped field");
    let rule = schema.rule("t").expect("the rule");
    let (_, default) = rule.required().next().expect("a requirement");
    assert_eq!(
        default.expect("a default").source(),
        AuthoredValue::list([text("todo"), text("done")])
    );
}

#[test]
fn a_one_of_member_its_own_rule_refuses_is_refused() {
    let cases: &[(&[u8], &str, RuleProblem)] = &[
        (
            b"version: 1\nfields:\n  priority: { type: number }\nrules:\n  t: { one_of: { priority: { values: [1, 2, high] } } }\n",
            "rules.t.one_of.priority.values",
            RuleProblem::Member {
                value: "high".to_string(),
                problem: ElementProblem::NotType(FieldType::Number),
            },
        ),
        (
            b"version: 1\nrules:\n  t: { one_of: { status: { values: [todo, finished] } }, max_length: { status: 4 } }\n",
            "rules.t.one_of.status.values",
            RuleProblem::Member {
                value: "finished".to_string(),
                problem: ElementProblem::TooLong(4),
            },
        ),
    ];
    for (bytes, node, expected) in cases {
        assert_eq!(rule_problem(bytes), (node.to_string(), expected.clone()));
    }
}

/// **A synonym's target is a member of its own set**, of the field's type and
/// within the rule's length limit.
#[test]
fn a_synonym_target_its_own_rule_refuses_is_refused() {
    let cases: &[(&[u8], &str, RuleProblem)] = &[
        (
            b"version: 1\nrules:\n  t: { one_of: { status: { values: [todo, done], synonyms: { complete: finished } } } }\n",
            "rules.t.one_of.status.synonyms",
            RuleProblem::SynonymTarget {
                value: "finished".to_string(),
                problem: ElementProblem::OutsideOneOf,
            },
        ),
        (
            b"version: 1\nfields:\n  priority: { type: number }\nrules:\n  t: { one_of: { priority: { values: [1, 2], synonyms: { high: top } } } }\n",
            "rules.t.one_of.priority.synonyms",
            RuleProblem::SynonymTarget {
                value: "top".to_string(),
                problem: ElementProblem::NotType(FieldType::Number),
            },
        ),
        // A target past the limit while its member is within it: the two
        // are one number written two ways.
        (
            b"version: 1\nfields:\n  priority: { type: number }\nrules:\n  t: { one_of: { priority: { values: [1, 2], synonyms: { top: '1.00' } } }, max_length: { priority: 2 } }\n",
            "rules.t.one_of.priority.synonyms",
            RuleProblem::SynonymTarget {
                value: "1.00".to_string(),
                problem: ElementProblem::TooLong(2),
            },
        ),
    ];
    for (bytes, node, expected) in cases {
        assert_eq!(rule_problem(bytes), (node.to_string(), expected.clone()));
    }
}

/// **A route names a folder some document could stand directly inside under
/// its own rule's allowed paths.** An untemplated route is judged exactly; a
/// templated one with each token standing as one segment.
#[test]
fn a_route_outside_its_own_allowed_paths_is_refused() {
    for route in [
        "archive/",
        "projects/norn/",
        "projects/norn/tasks/done/x/",
        "{{path.p}}/tasks/",
    ] {
        let bytes = format!(
            "version: 1\nrules:\n  t:\n    match: {{ path: '<p>/**' }}\n    allowed_paths: {{ paths: ['projects/*/tasks/*.md'], route: '{route}' }}\n"
        );
        let (at, problem) = rule_problem(bytes.as_bytes());
        assert_eq!(
            (at.as_str(), problem),
            (
                "rules.t.allowed_paths.route",
                RuleProblem::RouteOutsideAllowedPaths
            ),
            "{route}"
        );
    }
    for route in ["projects/norn/tasks/", "projects/{{path.p}}/tasks/"] {
        let bytes = format!(
            "version: 1\nrules:\n  t:\n    match: {{ path: 'projects/<p>/**' }}\n    allowed_paths: {{ paths: ['projects/*/tasks/*.md'], route: '{route}' }}\n"
        );
        VaultSchema::parse(bytes.as_bytes()).unwrap_or_else(|error| panic!("{route}: {error}"));
    }
}

/// **A route is judged in its own spelling.** `Notes/` against `notes/**`
/// is one author disagreeing with themselves, refused whether or not the
/// vault's root folds case.
#[test]
fn a_route_spelled_in_another_case_than_its_allowed_paths_is_refused() {
    assert_eq!(
        rule_problem(
            b"version: 1\nrules:\n  r: { allowed_paths: { paths: ['notes/**'], route: 'Notes/' } }\n"
        ),
        (
            "rules.r.allowed_paths.route".to_string(),
            RuleProblem::RouteOutsideAllowedPaths
        )
    );
    VaultSchema::parse(
        b"version: 1\nrules:\n  r: { allowed_paths: { paths: ['notes/**'], route: 'notes/' } }\n",
    )
    .expect("a route in its allowed paths' own spelling");
}

/// **A route's tokens are judged as if filled apart — a declared limit.** Two
/// tokens reading one capture each stand as their own `*`, so this route
/// loads though no document's capture writes both `red` and `blue`: the
/// filled route is judged per document when repair moves one. An exact
/// judgment at read would refuse it, and changing this test is that
/// decision.
#[test]
fn a_route_reading_one_capture_twice_is_judged_as_two_independent_fills() {
    VaultSchema::parse(
        b"version: 1\nrules:\n  r:\n    match: { path: '<p>/**' }\n    allowed_paths: { paths: ['red/blue/*.md'], route: '{{path.p}}/{{path.p}}/' }\n",
    )
    .expect("a route some independent filling of its tokens admits");
}

/// **A route's judgment is weighed before it is walked.** A long route
/// against heavy allowed paths weighs past the ceiling and is refused,
/// naming the rule, without its walk; the same route against light paths
/// loads, at once.
#[test]
fn a_route_weighing_past_the_ceiling_with_its_allowed_paths_is_refused() {
    let route = "a/".repeat(50);
    let heavy = format!("{}b/*.md", "**/".repeat(20_000));
    let bytes = format!(
        "version: 1\nrules:\n  r: {{ allowed_paths: {{ paths: ['{heavy}'], route: '{route}' }} }}\n"
    );
    let started = std::time::Instant::now();
    let (at, problem) = rule_problem(bytes.as_bytes());
    assert_eq!(at, "rules.r.allowed_paths.route");
    let route_weight = u64::try_from(route.len() + "*.md".len() + 1).expect("a u64");
    let paths_weight = u64::try_from(heavy.len() + 1).expect("a u64");
    assert_eq!(
        problem,
        RuleProblem::RouteWeight {
            weight: route_weight * paths_weight
        }
    );
    assert!(route_weight * paths_weight > PLACEMENT_CEILING);
    let light = format!("{}*.md", "**/".repeat(100));
    let bytes = format!(
        "version: 1\nrules:\n  r: {{ allowed_paths: {{ paths: ['{light}'], route: '{route}' }} }}\n"
    );
    VaultSchema::parse(bytes.as_bytes()).expect("a route under the ceiling");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn a_route_that_names_no_folder_is_refused() {
    let cases: &[(&str, RuleProblem)] = &[
        ("tasks", RuleProblem::RouteNotFolder),
        ("tasks/a.md", RuleProblem::RouteNotFolder),
        ("/tasks/", RuleProblem::RoutePath(PathProblem::Absolute)),
        ("a/../b/", RuleProblem::RoutePath(PathProblem::DotSegment)),
        ("a//b/", RuleProblem::RoutePath(PathProblem::EmptySegment)),
        ("a:b/", RuleProblem::RouteCharacter { character: ':' }),
        ("a*/", RuleProblem::RouteCharacter { character: '*' }),
        (
            "log/{{time}}/",
            RuleProblem::RouteClockWithColon {
                token: "time".to_string(),
            },
        ),
    ];
    for (route, expected) in cases {
        let bytes = format!(
            "version: 1\nrules:\n  t: {{ allowed_paths: {{ paths: ['**'], route: '{route}' }} }}\n"
        );
        assert_eq!(
            rule_problem(bytes.as_bytes()),
            ("rules.t.allowed_paths.route".to_string(), expected.clone()),
            "{route}"
        );
    }
    VaultSchema::parse(
        b"version: 1\nrules:\n  t: { allowed_paths: { paths: ['**'], route: 'log/{{date}}-{{time|slug}}/' } }\n",
    )
    .expect("a slugged clock in a route");
}

#[test]
fn a_rename_onto_a_field_the_same_rule_forbids_or_renames_onto_is_refused() {
    assert_eq!(
        rule_problem(
            b"version: 1\nrules:\n  t: { forbidden: { due_date: { rename_to: deadline }, deadline: remove } }\n"
        ),
        (
            "rules.t.forbidden.due_date.rename_to".to_string(),
            RuleProblem::RenameOntoForbidden {
                target: "deadline".to_string()
            }
        )
    );
    assert_eq!(
        rule_problem(b"version: 1\nrules:\n  t: { forbidden: { a: { rename_to: a } } }\n").1,
        RuleProblem::RenameOntoForbidden {
            target: "a".to_string()
        }
    );
    assert_eq!(
        rule_problem(
            b"version: 1\nrules:\n  t: { forbidden: { due_date: { rename_to: due }, deadline: { rename_to: due } } }\n"
        ),
        (
            "rules.t.forbidden.due_date.rename_to".to_string(),
            RuleProblem::RenameTwice {
                target: "due".to_string(),
                other: "deadline".to_string(),
            }
        )
    );
    // Another rule forbidding the target is a conflict that depends on the
    // document, judged where one is.
    VaultSchema::parse(
        b"version: 1\nrules:\n  t: { forbidden: { due_date: { rename_to: due } } }\n  u: { match: { frontmatter: { kind: x } }, forbidden: { due: remove } }\n",
    )
    .expect("a rename onto a field another rule forbids");
}

// ---- the allowed-paths ceiling ----

/// A glob of exactly `length` characters admitting one document path under
/// `prefix`, so the rule stating it alone admits a place to stand.
fn glob_of(prefix: &str, length: usize) -> String {
    format!("{prefix}/{}.md", "x".repeat(length - prefix.len() - 4))
}

#[test]
fn a_single_allowed_paths_list_past_the_ceiling_is_refused() {
    let length = usize::try_from(PLACEMENT_CEILING).expect("a usize");
    let bytes = format!(
        "version: 1\nrules:\n  wide: {{ allowed_paths: {{ paths: ['{}'] }} }}\n",
        glob_of("a", length)
    );
    let error = refused(bytes.as_bytes());
    assert_eq!(
        error,
        VaultSchemaError::PlacementCeiling {
            rule: "wide".to_string(),
            rules: vec!["wide".to_string()],
            weight: PLACEMENT_CEILING + 1,
        }
    );
    assert!(error.to_string().contains("`wide`"), "{error}");
    let under = format!(
        "version: 1\nrules:\n  wide: {{ allowed_paths: {{ paths: ['{}'] }} }}\n",
        glob_of("a", length - 1)
    );
    VaultSchema::parse(under.as_bytes()).expect("a list at the ceiling");
}

/// **Rules that may select one document together are weighed together.**
/// Three path-only rules may all select one document, so their product is
/// weighed, though each pair is under the ceiling.
#[test]
fn co_selecting_rules_past_the_ceiling_are_refused_naming_them() {
    let mut bytes = String::from("version: 1\nrules:\n");
    for (name, area) in [("alpha", "a"), ("beta", "b"), ("gamma", "c")] {
        bytes.push_str(&format!(
            "  {name}: {{ match: {{ path: '{area}/**' }}, allowed_paths: {{ paths: ['{}'] }} }}\n",
            glob_of("**", 100)
        ));
    }
    assert_eq!(
        refused(bytes.as_bytes()),
        VaultSchemaError::PlacementCeiling {
            rule: "alpha".to_string(),
            rules: vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()],
            weight: 101 * 101 * 101,
        }
    );
}

/// The rules of [`many_rules_with_disjoint_single_shaped_selectors_load`],
/// each selecting its own `type` and allowing paths that weigh a thousand,
/// so any two together pass the ceiling.
fn disjoint_rules(type_declaration: &str) -> String {
    let mut bytes = format!("version: 1\nfields:\n  type: {type_declaration}\nrules:\n");
    for at in 0..50 {
        bytes.push_str(&format!(
            "  r{at:02}: {{ match: {{ frontmatter: {{ type: t{at:02} }} }}, allowed_paths: {{ paths: ['{}'] }} }}\n",
            glob_of(&format!("t{at:02}"), 999)
        ));
    }
    bytes
}

/// **Rules whose selectors are disjoint on a single-shaped key are not
/// weighed together**: one value equals at most one of them.
#[test]
fn many_rules_with_disjoint_single_shaped_selectors_load() {
    let schema = VaultSchema::parse(disjoint_rules("{ type: text, shape: single }").as_bytes())
        .expect("fifty disjoint rules, each under the ceiling");
    assert_eq!(schema.rules().count(), 50);
    assert!(schema.rules().all(|rule| rule.placement_weight() == 1000));
}

/// **A key that may hold a list proves nothing disjoint**: a document whose
/// `type` lists two values selects both rules.
#[test]
fn disjoint_selectors_on_a_key_that_may_hold_a_list_are_weighed_together() {
    for declaration in ["{ type: text }", "{ type: text, shape: list }"] {
        let error = refused(disjoint_rules(declaration).as_bytes());
        assert!(
            matches!(&error, VaultSchemaError::PlacementCeiling { rule, rules, .. }
                if rule == "r00" && rules.len() == 50),
            "{declaration}: {error}"
        );
    }
}

// ---- statically unavoidable conflicts ----

fn conflict(bytes: &[u8]) -> RulesConflict {
    match refused(bytes) {
        VaultSchemaError::RulesConflict { conflict } => conflict,
        other => panic!("a rules conflict: {other}"),
    }
}

/// **A selectorless rule selects with every rule**, so a field it requires
/// and another rule forbids is a conflict on every document that rule
/// selects, refused naming every contributing rule in name order.
#[test]
fn a_selectorless_rule_requiring_what_another_forbids_is_refused_naming_every_rule() {
    let bytes = b"version: 1
rules:
  zeta: { required: { status: } }
  alpha: { match: { frontmatter: { type: task } }, forbidden: { status: remove } }
  middle: { match: { path: 'tasks/**' }, required: { status: } }
";
    let error = refused(bytes);
    assert_eq!(
        error,
        VaultSchemaError::RulesConflict {
            conflict: RulesConflict::RequiredAndForbidden {
                field: "status".to_string(),
                rules: vec!["alpha".to_string(), "zeta".to_string()],
            }
        }
    );
    assert!(error.to_string().contains("`alpha`, `zeta`"), "{error}");
}

#[test]
fn a_rule_requiring_and_forbidding_one_field_is_refused() {
    assert_eq!(
        conflict(
            b"version: 1\nrules:\n  t: { match: { frontmatter: { type: task } }, required: { status: }, forbidden: { status: } }\n"
        ),
        RulesConflict::RequiredAndForbidden {
            field: "status".to_string(),
            rules: vec!["t".to_string()],
        }
    );
}

/// **Selectors identical after normalization select together**: any-of and
/// excluded lists compare as sets, capture names are erased, a `match.path`
/// of `**` is no path, and typed values compare by their value.
#[test]
fn identical_selectors_after_normalization_conflict_as_one() {
    let pairs: &[(&str, &str)] = &[
        (
            "match: { frontmatter: { type: [task, chore] }, path: 'projects/<p>/**' }, exclude: { path: ['x/**', 'y/**'] }",
            "match: { frontmatter: { type: [chore, task] }, path: 'projects/<q>/**' }, exclude: { path: ['y/**', 'x/**'] }",
        ),
        (
            "match: { frontmatter: { type: task }, path: '**' }",
            "match: { frontmatter: { type: task } }",
        ),
        (
            "match: { frontmatter: { priority: [1, 2] } }",
            "match: { frontmatter: { priority: [2.0, 1] } }",
        ),
    ];
    for (left, right) in pairs {
        let bytes = format!(
            "version: 1\nfields:\n  priority: {{ type: number }}\nrules:\n  b: {{ {left}, required: {{ status: }} }}\n  a: {{ {right}, forbidden: {{ status: }} }}\n"
        );
        assert_eq!(
            conflict(bytes.as_bytes()),
            RulesConflict::RequiredAndForbidden {
                field: "status".to_string(),
                rules: vec!["a".to_string(), "b".to_string()],
            },
            "{left} / {right}"
        );
    }
    // A `match.path` of `**` alone is no selector at all.
    assert_eq!(
        conflict(
            b"version: 1\nrules:\n  a: { match: { path: '**' }, required: { status: } }\n  b: { match: { frontmatter: { kind: x } }, forbidden: { status: } }\n"
        ),
        RulesConflict::RequiredAndForbidden {
            field: "status".to_string(),
            rules: vec!["a".to_string(), "b".to_string()],
        }
    );
}

#[test]
fn disjoint_allowed_paths_of_rules_selecting_together_are_refused() {
    assert_eq!(
        conflict(
            b"version: 1\nrules:\n  b: { match: { frontmatter: { type: task } }, allowed_paths: { paths: ['tasks/**'] } }\n  a: { allowed_paths: { paths: ['notes/**', 'journal/*.md'] } }\n"
        ),
        RulesConflict::DisjointPlacement {
            rules: vec!["a".to_string(), "b".to_string()],
        }
    );
    // Paths that share one document load, a fold apart included: some root
    // folds case, and a refusal holds on every root.
    VaultSchema::parse(
        b"version: 1\nrules:\n  b: { match: { frontmatter: { type: task } }, allowed_paths: { paths: ['Tasks/*.md'] } }\n  a: { allowed_paths: { paths: ['tasks/**', 'notes/**'] } }\n",
    )
    .expect("allowed paths sharing a path");
}

/// **A rule whose allowed paths admit no document path is refused**, naming
/// that rule alone: no document it selects could stand anywhere, whichever
/// rules select beside it. A glob of another extension, a path no document's
/// file name spells and a path holding a `..` segment each admit none; one
/// admitting glob among them is enough to load.
#[test]
fn a_rule_whose_allowed_paths_admit_no_document_path_is_refused_naming_it() {
    for allowed in ["'*.txt'", "shared", "'area/../*.md'"] {
        let bytes = format!(
            "version: 1\nrules:\n  q: {{ match: {{ frontmatter: {{ kind: x }} }}, allowed_paths: {{ paths: ['**'] }} }}\n  r: {{ match: {{ frontmatter: {{ kind: x }} }}, allowed_paths: {{ paths: [{allowed}] }} }}\n"
        );
        let error = refused(bytes.as_bytes());
        assert_eq!(
            error,
            VaultSchemaError::RulesConflict {
                conflict: RulesConflict::DisjointPlacement {
                    rules: vec!["r".to_string()],
                }
            },
            "allowed {allowed}"
        );
        assert!(
            error
                .to_string()
                .contains("the allowed paths of the rule `r` admit no document path"),
            "{error}"
        );
    }
    VaultSchema::parse(
        b"version: 1\nrules:\n  r: { match: { frontmatter: { kind: x } }, allowed_paths: { paths: ['*.txt', 'shared', 'notes/*.md'] } }\n",
    )
    .expect("allowed paths admitting a document path");
}

/// **Allowed paths share a path only where a document could stand at it.**
/// Two selectorless rules, each admitting a document path, meeting only at
/// `shared` — no document's file name — or only at a path holding a `..`
/// segment leave every document unplaceable, and are refused as disjoint.
#[test]
fn allowed_paths_sharing_only_a_path_no_document_stands_at_are_refused() {
    for (left, right) in [
        ("['a/*.md', 'shared']", "['b/*.md', 'shared']"),
        ("['a/.?/x.md']", "['a/?./x.md']"),
    ] {
        let bytes = format!(
            "version: 1\nrules:\n  left: {{ allowed_paths: {{ paths: {left} }} }}\n  right: {{ allowed_paths: {{ paths: {right} }} }}\n"
        );
        assert_eq!(
            conflict(bytes.as_bytes()),
            RulesConflict::DisjointPlacement {
                rules: vec!["left".to_string(), "right".to_string()],
            },
            "{left} / {right}"
        );
    }
}

/// **An empty intersection refuses only where some contributing rule requires
/// the field**: a document holding no value meets every closed set.
#[test]
fn an_empty_one_of_intersection_refuses_only_on_a_required_field() {
    assert_eq!(
        conflict(
            b"version: 1\nrules:\n  b: { one_of: { status: { values: [todo] } } }\n  a: { match: { frontmatter: { type: task } }, one_of: { status: { values: [open] } } }\n  c: { match: { frontmatter: { type: task } }, required: { status: } }\n"
        ),
        RulesConflict::EmptyOneOf {
            field: "status".to_string(),
            required: true,
            rules: vec!["a".to_string(), "b".to_string(), "c".to_string()],
        }
    );
    VaultSchema::parse(
        b"version: 1\nrules:\n  b: { one_of: { status: { values: [todo] } } }\n  a: { match: { frontmatter: { type: task } }, one_of: { status: { values: [open] } } }\n",
    )
    .expect("an empty intersection on a field none requires");
}

/// **Rules whose selectors differ conflict only on documents**: a field one
/// requires and the other forbids is judged where a document selects both,
/// which no selector alone decides.
#[test]
fn rules_with_different_selectors_do_not_conflict_at_read() {
    for (left, right) in [
        (
            "match: { frontmatter: { type: task } }",
            "match: { frontmatter: { type: note } }",
        ),
        (
            "match: { path: 'tasks/**' }",
            "match: { frontmatter: { type: task } }",
        ),
        ("match: { path: 'a/**' }", "exclude: { path: ['b/**'] }"),
        (
            "match: { frontmatter: { type: task } }",
            "match: { frontmatter: { type: task }, path: 'x/**' }",
        ),
    ] {
        let bytes = format!(
            "version: 1\nrules:\n  a: {{ {left}, required: {{ status: }}, allowed_paths: {{ paths: ['a/**'] }} }}\n  b: {{ {right}, forbidden: {{ status: }}, allowed_paths: {{ paths: ['b/**'] }} }}\n"
        );
        VaultSchema::parse(bytes.as_bytes())
            .unwrap_or_else(|error| panic!("{left} / {right}: {error}"));
    }
}

// ---- selection and the combined constraint ----

/// A schema of rules selecting in every way a selector can.
const SELECTING: &[u8] = b"version: 1
fields:
  topics: { type: tags }
  priority: { type: number }
  due: { type: date }
  kind: { type: text, shape: single }
rules:
  every: { severity: warning, required: { title: } }
  area: { match: { path: 'projects/<project>/**' }, exclude: { path: ['projects/*/archive/**'] } }
  tagged: { match: { frontmatter: { topics: Work } } }
  urgent: { match: { frontmatter: { priority: [1, 2] } } }
  dated: { match: { frontmatter: { due: '2026-10-01' } } }
  kinded: { match: { frontmatter: { kind: task } } }
  typed: { match: { frontmatter: { type: [task, chore] } }, severity: error }
";

fn selected(schema: &VaultSchema, path: &str, entries: &[(&str, AuthoredValue)]) -> Vec<String> {
    schema
        .selecting_rules(
            path,
            &frontmatter(entries),
            CaseFold::Exact,
            &mut RuleWork::default(),
        )
        .iter()
        .map(|rule| rule.name().to_string())
        .collect()
}

#[test]
fn a_selectorless_rule_selects_every_document_and_a_path_only_rule_its_area() {
    let schema = VaultSchema::parse(SELECTING).expect("selecting rules");
    assert_eq!(selected(&schema, "notes/a.md", &[]), ["every"]);
    assert_eq!(
        selected(&schema, "projects/norn/a.md", &[]),
        ["area", "every"]
    );
    assert_eq!(
        selected(&schema, "projects/norn/archive/a.md", &[]),
        ["every"]
    );
    assert_eq!(selected(&schema, "projects", &[]), ["every"]);
}

/// **Each key compares as a find's equality part does**: a tag under the
/// tag fold, a typed key by its typed value, and any other key exactly as
/// written. Every key a rule names must match.
#[test]
fn a_frontmatter_selector_compares_as_find_equality_compares() {
    let schema = VaultSchema::parse(SELECTING).expect("selecting rules");
    let at = "notes/a.md";
    assert!(selected(&schema, at, &[("topics", text("work"))]).contains(&"tagged".to_string()));
    assert!(selected(&schema, at, &[("topics", text("WORK"))]).contains(&"tagged".to_string()));
    assert!(!selected(&schema, at, &[("topics", text("wörk"))]).contains(&"tagged".to_string()));
    let urgent = AuthoredValue::float(2.0).expect("a finite float");
    assert!(selected(&schema, at, &[("priority", urgent)]).contains(&"urgent".to_string()));
    assert!(selected(&schema, at, &[("priority", text("1"))]).contains(&"urgent".to_string()));
    assert!(
        !selected(&schema, at, &[("priority", AuthoredValue::Integer(3))])
            .contains(&"urgent".to_string())
    );
    assert!(selected(&schema, at, &[("due", text("2026-10-01"))]).contains(&"dated".to_string()));
    assert!(!selected(&schema, at, &[("due", text("2026-10-02"))]).contains(&"dated".to_string()));
    assert!(selected(&schema, at, &[("type", text("chore"))]).contains(&"typed".to_string()));
    assert!(!selected(&schema, at, &[("type", text("Task"))]).contains(&"typed".to_string()));
    assert!(
        !selected(&schema, at, &[("type", AuthoredValue::Null)]).contains(&"typed".to_string())
    );
    assert!(!selected(&schema, at, &[]).contains(&"typed".to_string()));
    assert_eq!(
        schema.equality_key("topics", "Work"),
        Some(TypedValue::Text("work".to_string()))
    );
    assert_eq!(schema.equality_key("priority", "high"), None);
}

/// **A list value selects where any element does, as a find's equality
/// matches a list field — except under a key declared single**, where a list
/// is a value of the wrong shape and selects no rule on that key.
#[test]
fn a_list_value_selects_where_any_element_does_unless_its_key_is_single() {
    let schema = VaultSchema::parse(SELECTING).expect("selecting rules");
    let at = "notes/a.md";
    let listed = AuthoredValue::list([text("note"), text("task")]);
    assert!(selected(&schema, at, &[("type", listed.clone())]).contains(&"typed".to_string()));
    assert!(!selected(&schema, at, &[("kind", listed)]).contains(&"kinded".to_string()));
    assert!(selected(&schema, at, &[("kind", text("task"))]).contains(&"kinded".to_string()));
}

/// **The path fold is the caller's**: the store's recorded path order names
/// it, as for every glob over a vault path.
#[test]
fn path_selection_compares_under_the_fold_its_caller_names() {
    let schema = VaultSchema::parse(SELECTING).expect("selecting rules");
    let area = schema.rule("area").expect("the area rule");
    let none = ValueMap::default();
    assert!(!schema.selects(area, "Projects/norn/a.md", &none, CaseFold::Exact));
    assert!(schema.selects(area, "Projects/norn/a.md", &none, CaseFold::Ascii));
}

#[test]
fn the_combined_constraint_holds_every_selecting_rule_at_once() {
    let schema = VaultSchema::parse(
        b"version: 1
rules:
  wide: { required: { status: }, one_of: { status: { values: [todo, doing, done] } }, max_length: { title: 80 }, allowed_paths: { paths: ['**'] } }
  task: { severity: error, match: { frontmatter: { type: task } }, forbidden: { scratch: remove }, one_of: { status: { values: [done, todo, open] } }, max_length: { title: 40 }, allowed_paths: { paths: ['tasks/**'] } }
",
    )
    .expect("two rules");
    let at = "tasks/a.md";
    let document = frontmatter(&[("type", text("task"))]);
    let rules = schema.selecting_rules(at, &document, CaseFold::Exact, &mut RuleWork::default());
    let combined = schema.combined(&rules);
    assert_eq!(
        combined
            .rules()
            .iter()
            .map(|rule| rule.name())
            .collect::<Vec<_>>(),
        ["task", "wide"]
    );
    assert_eq!(combined.severity(), Severity::Error);
    let status = combined.field("status").expect("status is constrained");
    assert!(status.is_required());
    let intersection = status.one_of().expect("a closed set");
    assert_eq!(intersection.members().collect::<Vec<_>>(), ["done", "todo"]);
    assert!(intersection.admits(&TypedValue::Text("todo".to_string())));
    assert!(!intersection.admits(&TypedValue::Text("doing".to_string())));
    assert!(combined.field("scratch").expect("scratch").is_forbidden());
    assert_eq!(
        combined.field("title").and_then(|title| title.max_length()),
        Some(40)
    );
    let mut work = RuleWork::default();
    assert!(combined.admits_path("tasks/a.md", CaseFold::Exact, &mut work));
    assert!(!combined.admits_path("notes/a.md", CaseFold::Exact, &mut work));
    assert!(combined.conflicts(CaseFold::Exact).is_empty());

    let warning_only = schema.combined(&[schema.rule("wide").expect("wide")]);
    assert_eq!(warning_only.severity(), Severity::Warning);
}

/// **A combined constraint that depends on the document is judged there**:
/// an unrequired empty intersection is reported, carrying that no rule
/// requires the field, for a finding to report only where a value stands.
#[test]
fn a_combined_constraint_reports_every_empty_part_naming_its_rules() {
    let schema = VaultSchema::parse(
        b"version: 1
rules:
  a: { match: { frontmatter: { type: task } }, one_of: { status: { values: [todo] } }, allowed_paths: { paths: ['tasks/**'] } }
  b: { match: { path: 'notes/**' }, one_of: { status: { values: [open] } }, forbidden: { title: }, allowed_paths: { paths: ['notes/**'] } }
  c: { match: { frontmatter: { kind: x } }, required: { title: } }
",
    )
    .expect("rules conflicting only on some documents");
    let rules: Vec<_> = schema.rules().collect();
    let conflicts = schema.combined(&rules).conflicts(CaseFold::Exact);
    assert_eq!(
        conflicts,
        [
            RulesConflict::EmptyOneOf {
                field: "status".to_string(),
                required: false,
                rules: vec!["a".to_string(), "b".to_string()],
            },
            RulesConflict::RequiredAndForbidden {
                field: "title".to_string(),
                rules: vec!["b".to_string(), "c".to_string()],
            },
            RulesConflict::DisjointPlacement {
                rules: vec!["a".to_string(), "b".to_string()],
            },
        ]
    );
}

// ---- what a selector value compares by ----

/// The rules `a` to `l`, each selecting its own letter of `kind` and
/// allowing every path, under `kind` declared `declaration`. Any twelve of
/// them together weigh `3^12`, past the ceiling; eleven weigh under it.
fn twelve_kind_rules(declaration: &str) -> String {
    let mut bytes = format!("version: 1\nfields:\n  kind: {declaration}\nrules:\n");
    for letter in 'a'..='l' {
        bytes.push_str(&format!(
            "  {letter}: {{ match: {{ frontmatter: {{ kind: {letter} }} }}, allowed_paths: {{ paths: ['**'] }} }}\n"
        ));
    }
    bytes
}

/// **A list under a key declared single matches no selector on it**: the
/// value does not read as the key's declared shape, so it has nothing to
/// compare, as a value that fails its declared type has no typed value.
#[test]
fn a_list_or_map_under_a_single_shaped_key_selects_no_rule() {
    let schema = VaultSchema::parse(twelve_kind_rules("{ type: text, shape: single }").as_bytes())
        .expect("twelve rules on a single-shaped key");
    let listed = AuthoredValue::list([text("a"), text("b")]);
    assert!(selected(&schema, "a.md", &[("kind", listed)]).is_empty());
    let one = AuthoredValue::list([text("a")]);
    assert!(selected(&schema, "a.md", &[("kind", one)]).is_empty());
    let mapped = AuthoredValue::map([("a".to_string(), text("a"))]).expect("a map");
    assert!(selected(&schema, "a.md", &[("kind", mapped)]).is_empty());
    assert_eq!(selected(&schema, "a.md", &[("kind", text("a"))]), ["a"]);
}

/// **A scalar under a key declared list matches no selector on it**, the
/// mirror of a list under a single-shaped key; a one-element list there
/// matches.
#[test]
fn a_scalar_under_a_list_shaped_key_selects_no_rule() {
    let schema = VaultSchema::parse(
        b"version: 1\nfields:\n  kind: { type: text, shape: list }\nrules:\n  a: { match: { frontmatter: { kind: a } } }\n  b: { match: { frontmatter: { kind: b } } }\n",
    )
    .expect("two rules on a list-shaped key");
    assert!(selected(&schema, "a.md", &[("kind", text("a"))]).is_empty());
    let one = AuthoredValue::list([text("a")]);
    assert_eq!(selected(&schema, "a.md", &[("kind", one)]), ["a"]);
}

/// **A list under a key with no shape declared matches as find's equality
/// does**: every rule whose value is one of its elements selects it.
#[test]
fn a_list_under_an_unshaped_key_selects_every_rule_whose_value_it_holds() {
    let schema = VaultSchema::parse(
        b"version: 1\nfields:\n  kind: { type: text }\nrules:\n  a: { match: { frontmatter: { kind: a } } }\n  b: { match: { frontmatter: { kind: b } } }\n  c: { match: { frontmatter: { kind: c } } }\n",
    )
    .expect("three rules on an unshaped key");
    let listed = AuthoredValue::list([text("a"), text("c")]);
    assert_eq!(selected(&schema, "a.md", &[("kind", listed)]), ["a", "c"]);
}

/// **Disjoint values on a single-shaped key are provably disjoint for the
/// ceiling, and on an unshaped key they earn no credit**: twelve rules
/// allowing `**` load where `kind` is single, and the same rules refuse at
/// the ceiling where `kind` declares no shape.
#[test]
fn twelve_disjoint_rules_load_only_on_a_single_shaped_key() {
    let schema = VaultSchema::parse(twelve_kind_rules("{ type: text, shape: single }").as_bytes())
        .expect("twelve disjoint rules on a single-shaped key");
    assert_eq!(schema.rules().count(), 12);
    let error = refused(twelve_kind_rules("{ type: text }").as_bytes());
    assert!(
        matches!(&error, VaultSchemaError::PlacementCeiling { rule, rules, weight }
            if rule == "a" && rules.len() == 12 && *weight == 3u64.pow(12)),
        "{error}"
    );
}

/// **A key declared `tags` selects under the tag fold**, with or without its
/// `#` marker, as a frontmatter tag is read.
#[test]
fn a_key_declared_tags_selects_under_the_tag_fold() {
    let schema = VaultSchema::parse(
        b"version: 1\nfields:\n  area: { type: tags }\nrules:\n  r: { match: { frontmatter: { area: foo } } }\n",
    )
    .expect("a rule on a tags key");
    for written in ["Foo", "FOO", "#foo", "#Foo"] {
        assert_eq!(
            selected(&schema, "a.md", &[("area", text(written))]),
            ["r"],
            "{written}"
        );
    }
    assert!(selected(&schema, "a.md", &[("area", text("föo"))]).is_empty());
}

/// **The tag carrier `tags` selects under the tag fold though no field
/// declares it**, and as a list that may hold several tags.
#[test]
fn the_undeclared_tags_carrier_selects_under_the_tag_fold() {
    let schema = VaultSchema::parse(
        b"version: 1\nrules:\n  r: { match: { frontmatter: { tags: project } } }\n",
    )
    .expect("a rule on the tag carrier");
    let tagged = AuthoredValue::list([text("Project"), text("work")]);
    assert_eq!(selected(&schema, "a.md", &[("tags", tagged)]), ["r"]);
    assert_eq!(
        selected(&schema, "a.md", &[("tags", text("#PROJECT"))]),
        ["r"]
    );
    assert_eq!(
        schema.equality_key("tags", "#Project"),
        Some(TypedValue::Text("project".to_string()))
    );
    // Every other undeclared key compares exactly as written.
    assert_eq!(
        schema.equality_key("topic", "#Project"),
        Some(TypedValue::Text("#Project".to_string()))
    );
}

/// **Selector identity reads the tag fold**: two selectors whose tag values
/// fold to one tag are one selector, so a field one requires and the other
/// forbids is a conflict on every document either selects.
#[test]
fn tag_selectors_one_tag_apart_in_spelling_are_identical() {
    for (fields, left, right) in [
        ("  area: { type: tags }", "area: Foo", "area: '#foo'"),
        ("  other: { type: text }", "tags: Project", "tags: project"),
    ] {
        let bytes = format!(
            "version: 1\nfields:\n{fields}\nrules:\n  b: {{ match: {{ frontmatter: {{ {left} }} }}, required: {{ status: }} }}\n  a: {{ match: {{ frontmatter: {{ {right} }} }}, forbidden: {{ status: }} }}\n"
        );
        assert_eq!(
            conflict(bytes.as_bytes()),
            RulesConflict::RequiredAndForbidden {
                field: "status".to_string(),
                rules: vec!["a".to_string(), "b".to_string()],
            },
            "{left} / {right}"
        );
    }
}

/// **Disjointness reads the tag fold**: `tags: a` and `tags: A` name one tag,
/// so even on a single-shaped carrier the two rules may select one document
/// and are weighed together, while `tags: a` and `tags: b` are not.
#[test]
fn tag_selectors_one_tag_apart_in_spelling_are_not_disjoint() {
    let heavy = |name: &str, value: &str| {
        format!(
            "  {name}: {{ match: {{ frontmatter: {{ tags: {value} }} }}, allowed_paths: {{ paths: ['{}'] }} }}\n",
            glob_of(name, 999)
        )
    };
    let two = |fields: &str, left: &str, right: &str| {
        format!(
            "version: 1\nfields:\n{fields}\nrules:\n{}{}",
            heavy("left", left),
            heavy("right", right)
        )
    };
    for fields in [
        "  tags: { shape: single }",
        "  tags: { type: tags, shape: single }",
    ] {
        let error = refused(two(fields, "a", "A").as_bytes());
        assert!(
            matches!(&error, VaultSchemaError::PlacementCeiling { rules, .. } if rules.len() == 2),
            "{fields}: {error}"
        );
        VaultSchema::parse(two(fields, "a", "b").as_bytes())
            .unwrap_or_else(|error| panic!("{fields}: disjoint tags: {error}"));
    }
    // Undeclared, the carrier may hold a list, so no two of its values are
    // disjoint.
    let error = refused(two("  other: { type: text }", "a", "b").as_bytes());
    assert!(
        matches!(&error, VaultSchemaError::PlacementCeiling { rules, .. } if rules.len() == 2),
        "{error}"
    );
}

/// Selector normal forms: each pair names one selector where `identical`,
/// so a field one requires and the other forbids is refused, and two
/// selectors otherwise.
#[test]
fn selector_normal_forms_group_only_identical_selectors() {
    let cases: &[(&str, &str, &str, &str, bool)] = &[
        (
            "any-of order and repeats",
            "",
            "match: { frontmatter: { k: [a, b] } }",
            "match: { frontmatter: { k: [b, a, a] } }",
            true,
        ),
        (
            "capture names, empty frontmatter",
            "",
            "match: { path: 'p/<a>/**' }",
            "match: { path: 'p/<b>/**', frontmatter: {} }",
            true,
        ),
        (
            "a number's spellings",
            "  k: { type: number }",
            "match: { frontmatter: { k: 1 } }",
            "match: { frontmatter: { k: 1.0 } }",
            true,
        ),
        (
            "a path of ** is absent",
            "",
            "match: { frontmatter: { k: v }, path: '**' }",
            "match: { frontmatter: { k: v } }",
            true,
        ),
        (
            "a tag's fold and marker",
            "  area: { type: tags }",
            "match: { frontmatter: { area: Foo } }",
            "match: { frontmatter: { area: '#foo' } }",
            true,
        ),
        (
            "excluded globs as a set",
            "",
            "exclude: { path: ['x/**', 'y/**'] }",
            "exclude: { path: ['y/**', 'x/**', 'x/**'] }",
            true,
        ),
        (
            "selectorless by ** and an empty exclude",
            "",
            "match: { path: '**' }, exclude: { path: [] }",
            "match: { frontmatter: {} }",
            true,
        ),
        (
            "a subset is not identical",
            "",
            "match: { frontmatter: { k: a } }",
            "match: { frontmatter: { k: [a, b] } }",
            false,
        ),
        (
            "a capture is not a star",
            "",
            "match: { path: 'p/<a>/**' }",
            "match: { path: 'p/*/**' }",
            false,
        ),
    ];
    for (name, fields, left, right, identical) in cases {
        let bytes = format!(
            "version: 1\nfields:\n  zz: {{ type: text }}\n{fields}\nrules:\n  ra: {{ {left}, required: {{ x: }} }}\n  rb: {{ {right}, forbidden: {{ x: }} }}\n"
        );
        let read = VaultSchema::parse(bytes.as_bytes());
        assert_eq!(
            matches!(read, Err(VaultSchemaError::RulesConflict { .. })),
            *identical,
            "{name}: {read:?}"
        );
    }
}
