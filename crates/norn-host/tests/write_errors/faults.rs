use std::path::Path;
use std::time::Duration;

use norn_testkit::process::{Run, RunStatus, Sandbox};

use super::*;

const CHILD: &str = "NORN_WRITE_ERROR_CHILD";
const ROOT: &str = "NORN_WRITE_ERROR_ROOT";
const VERB: &str = "NORN_WRITE_ERROR_VERB";
const COMMIT: &str = "NORN_WRITE_ERROR_COMMIT";

#[derive(Clone, Copy)]
pub(super) enum Publication {
    Create(&'static str),
    Replace(&'static str),
    Remove(&'static str),
    Respell(&'static str, bool),
}

impl Publication {
    fn path(self) -> &'static str {
        match self {
            Self::Create(path)
            | Self::Replace(path)
            | Self::Remove(path)
            | Self::Respell(path, _) => path,
        }
    }

    fn stage(self) -> &'static str {
        match self {
            Self::Create(_) | Self::Replace(_) => "swap",
            Self::Remove(_) => "unlink",
            Self::Respell(_, _) => "respell",
        }
    }
}

struct Case {
    name: String,
    verb: Verb,
    request: Value,
    publications: Vec<Publication>,
    /// Only these fixtures create a missing folder or leave one empty.
    mkdir: bool,
    rmdir: bool,
    document_changeset: bool,
}

#[derive(Debug)]
enum Expected {
    Code(ReasonCode),
    Applied,
    Healing,
    AlreadySetUp,
}

fn cases() -> Vec<Case> {
    use Publication::*;
    let mut cases = Vec::new();
    for verb in Verb::ALL {
        let (publications, mkdir, document_changeset) = match verb {
            Verb::Set | Verb::Edit => (vec![Replace("subject.md")], false, true),
            Verb::New => (vec![Create("new/document.md")], true, true),
            Verb::NewAs => (vec![Create("new/rule-1.md")], true, true),
            Verb::Move => (
                vec![
                    Create("new/moved.md"),
                    Replace("h.md"),
                    Replace("k.md"),
                    Remove("source.md"),
                ],
                true,
                true,
            ),
            Verb::Delete => (
                vec![Replace("h.md"), Replace("k.md"), Remove("source.md")],
                false,
                true,
            ),
            Verb::Rewrite => (vec![Replace("h.md"), Replace("k.md")], false, true),
            Verb::Apply => (
                vec![
                    Create("new/document.md"),
                    Replace("subject.md"),
                    Remove("gone.md"),
                ],
                true,
                true,
            ),
            Verb::Init => (vec![Create(".norn/schema.yaml")], true, false),
            Verb::Migrate => (
                vec![Replace(".norn/config.toml"), Replace(".norn/schema.yaml")],
                false,
                false,
            ),
        };
        cases.push(Case {
            name: verb.name().into(),
            verb,
            request: request(verb, ApplyMode::Apply),
            publications,
            mkdir,
            rmdir: false,
            document_changeset,
        });
    }
    // The verbs cover their primary forms above. Apply also admits every
    // authored operation kind, so each kind gets its own changed-file row.
    for (kind, publications) in operation_cases() {
        let name = kind_name(&kind);
        // The schema already holds .norn open; only new is missing in these
        // apply fixtures, so a control-file create reaches no mkdir.
        let mkdir = publications
            .iter()
            .any(|publication| matches!(publication, Create(at) if at.starts_with("new/")));
        let rmdir = matches!(
            kind,
            OperationKind::MoveFolder { .. }
                | OperationKind::MoveDocument { .. }
                | OperationKind::DeleteDocument { .. }
        );
        let document_changeset = !matches!(kind, OperationKind::WriteControlFile { .. });
        cases.push(Case {
            name: format!("apply/{name}"),
            verb: Verb::Apply,
            request: apply_request(vec![Operation::new(kind)], ApplyMode::Apply),
            publications,
            mkdir,
            rmdir,
            document_changeset,
        });
    }
    let scratch = norn_testkit::scratch::Scratch::new("write-error-folding");
    let folding = norn_testkit::churn::folding(scratch.root()).expect("the volume answers");
    if norn_testkit::churn::runs_where_the_volume_folds(folding, "write-error-table-respell") {
        for edited in [false, true] {
            let verb = if edited { Verb::Apply } else { Verb::Move };
            let request = if edited {
                apply_request(
                    vec![
                        Operation::new(OperationKind::str_replace(
                            path("Note.md"),
                            "Note",
                            "Final",
                        )),
                        Operation::new(OperationKind::move_document(
                            path("Note.md"),
                            path("note.md"),
                        )),
                    ],
                    ApplyMode::Apply,
                )
            } else {
                json!(MoveParams::new(
                    address(),
                    ApplyMode::Apply,
                    MoveSubject::new("Note.md", "note.md").unwrap()
                ))
            };
            cases.push(Case {
                name: format!("{}/respell-edited-{edited}", verb.name()),
                verb,
                request,
                publications: vec![Respell("Note.md", edited)],
                mkdir: false,
                rmdir: false,
                document_changeset: true,
            });
        }
    }
    cases
}

/// An exhaustive match keeps a newly minted operation kind from silently
/// acquiring a name through a wildcard. Each named kind has an authored row.
pub(super) fn kind_name(kind: &OperationKind) -> &'static str {
    match kind {
        OperationKind::CreateDocument { .. } => "create_document",
        OperationKind::CreateByRule { .. } => "create_by_rule",
        OperationKind::StrReplace { .. } => "str_replace",
        OperationKind::MoveDocument { .. } => "move_document",
        OperationKind::DeleteDocument { .. } => "delete_document",
        OperationKind::MoveFolder { .. } => "move_folder",
        OperationKind::RewriteLink { .. } => "rewrite_link",
        OperationKind::RewriteWikilink { .. } => "rewrite_wikilink",
        OperationKind::SetFrontmatter { .. } => "set_frontmatter",
        OperationKind::RemoveFrontmatter { .. } => "remove_frontmatter",
        OperationKind::PushFrontmatter { .. } => "push_frontmatter",
        OperationKind::PopFrontmatter { .. } => "pop_frontmatter",
        OperationKind::ReplaceBody { .. } => "replace_body",
        OperationKind::ReplaceSection { .. } => "replace_section",
        OperationKind::AppendToSection { .. } => "append_to_section",
        OperationKind::DeleteSection { .. } => "delete_section",
        OperationKind::InsertBeforeHeading { .. } => "insert_before_heading",
        OperationKind::InsertAfterHeading { .. } => "insert_after_heading",
        OperationKind::WriteControlFile { .. } => "write_control_file",
    }
}

pub(super) fn operation_cases() -> Vec<(OperationKind, Vec<Publication>)> {
    use Publication::*;
    let at = || path("subject.md");
    let field_target = || WriteTarget::path(at());
    let replace = || vec![Replace("subject.md")];
    vec![
        (
            OperationKind::create_document(path("new/document.md"), "new\n"),
            vec![Create("new/document.md")],
        ),
        (
            OperationKind::create_by_rule(
                Some("note".into()),
                Variables::default(),
                ValueMap::default(),
                None,
            ),
            vec![Create("new/rule-1.md")],
        ),
        (
            OperationKind::str_replace(at(), "draft text", "final text"),
            replace(),
        ),
        (
            OperationKind::move_document(path("old/a.md"), path("new/a.md")),
            vec![Create("new/a.md"), Remove("old/a.md")],
        ),
        (
            OperationKind::delete_document(path("old/a.md")),
            vec![Remove("old/a.md")],
        ),
        (
            OperationKind::move_folder(
                FolderPath::new("old").unwrap(),
                FolderPath::new("new").unwrap(),
            ),
            vec![
                Create("new/a.md"),
                Create("new/b.md"),
                Remove("old/a.md"),
                Remove("old/b.md"),
            ],
        ),
        (
            OperationKind::rewrite_link(
                path("h.md"),
                LinkFamily::Wikilink,
                "source",
                "destination",
            ),
            vec![Replace("h.md")],
        ),
        (
            OperationKind::rewrite_wikilink(target("source"), target("destination")),
            vec![Replace("h.md"), Replace("k.md")],
        ),
        (
            OperationKind::set_frontmatter(
                field_target(),
                "status",
                AuthoredValue::string("final"),
            ),
            replace(),
        ),
        (
            OperationKind::remove_frontmatter(field_target(), "status"),
            replace(),
        ),
        (
            OperationKind::push_frontmatter(field_target(), "items", AuthoredValue::string("two")),
            replace(),
        ),
        (
            OperationKind::pop_frontmatter(field_target(), "items", AuthoredValue::string("one")),
            replace(),
        ),
        (
            OperationKind::replace_body(at(), "replacement\n"),
            replace(),
        ),
        (
            OperationKind::replace_section(at(), "Section", "replacement\n"),
            replace(),
        ),
        (
            OperationKind::append_to_section(at(), "Section", "appended\n"),
            replace(),
        ),
        (OperationKind::delete_section(at(), "Section"), replace()),
        (
            OperationKind::insert_before_heading(at(), "Section", "before\n"),
            replace(),
        ),
        (
            OperationKind::insert_after_heading(at(), "Section", "after\n"),
            replace(),
        ),
        (
            OperationKind::write_control_file(ControlFile::Config, "# new config\n"),
            vec![Create(".norn/config.toml")],
        ),
    ]
}

pub(super) fn tree(root: &Path, verb: Verb) {
    let vault = root.join("vault");
    for (at, content) in [
        ("subject.md", SUBJECT),
        ("source.md", "# Source\n"),
        ("destination.md", "# Destination\n"),
        ("h.md", "[[source]]\n"),
        ("k.md", "see [[source]]\n"),
        ("gone.md", "gone\n"),
        ("old/a.md", "# A\n"),
        ("old/b.md", "# B\n"),
        ("Note.md", "# Note\n"),
    ] {
        let path = vault.join(at);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    if !matches!(verb, Verb::Init) {
        std::fs::create_dir_all(vault.join(".norn")).unwrap();
        let schema = if matches!(verb, Verb::Migrate) {
            "version: 1\n# old\n"
        } else {
            SCHEMA
        };
        std::fs::write(vault.join(".norn/schema.yaml"), schema).unwrap();
    }
    if matches!(verb, Verb::Migrate) {
        std::fs::write(vault.join(".norn/config.toml"), "# old\n").unwrap();
    }
}

pub(super) fn child() {
    let Some(root) = std::env::var_os(CHILD) else {
        return;
    };
    let root = attach::accepted_harness_root(&root, ROOT);
    let verb_name = std::env::var(VERB).unwrap();
    let verb = Verb::ALL
        .into_iter()
        .find(|verb| verb.name() == verb_name)
        .unwrap();
    let vault = attach::Vault::adopt(&root);
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    if std::env::var_os(COMMIT).is_some() {
        let mut store = vault.store();
        // Persistent triggers reach the worker's connection too. They are
        // installed only after attachment, and stop only document writes.
        norn_store::induced_failure::execute_out_of_band(&mut store,
            "CREATE TRIGGER refuse_insert BEFORE INSERT ON documents BEGIN SELECT RAISE(ABORT, 'induced write matrix'); END;
             CREATE TRIGGER refuse_update BEFORE UPDATE ON documents BEGIN SELECT RAISE(ABORT, 'induced write matrix'); END;
             CREATE TRIGGER refuse_delete BEFORE DELETE ON documents BEGIN SELECT RAISE(ABORT, 'induced write matrix'); END;").unwrap();
    }
    let request = std::fs::read_to_string(root.join("request.json")).unwrap();
    let outcome = dispatch(&host, verb, &request, true).expect("the matrix request decodes");
    std::fs::write(
        root.join("outcome.json"),
        serde_json::to_vec(&outcome).unwrap(),
    )
    .unwrap();
}

struct Answer {
    outcome: Result<Value, ErrorEnvelope>,
    hits: String,
}

fn run_child(case: &Case, armed: &str, commit: bool) -> Answer {
    let sandbox =
        Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), "write-fault-table").unwrap();
    let root = sandbox.work_dir().join("attached");
    tree(&root, case.verb);
    if armed.starts_with("foreign@") {
        // The foreign writer acts before publication makes missing folders.
        // Give it a standing parent so taking a create's name really writes.
        for publication in &case.publications {
            if let Publication::Create(at) = publication {
                std::fs::create_dir_all(root.join("vault").join(at).parent().unwrap()).unwrap();
            }
        }
    }
    if case.rmdir && case.name != "apply/move_folder" {
        std::fs::remove_file(root.join("vault/old/b.md")).unwrap();
    }
    let token = attach::issue_harness_token(&root);
    std::fs::write(
        root.join("request.json"),
        serde_json::to_vec(&case.request).unwrap(),
    )
    .unwrap();
    let hits = root.join("hits");
    std::fs::write(&hits, "").unwrap();
    let mut run = Run::new(&sandbox, std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("write_fault_child")
        .arg("--nocapture")
        .env(CHILD, &root)
        .env(ROOT, token)
        .env(VERB, case.verb.name())
        .env("NORN_FS_ARMED_STAGES", armed)
        .env("NORN_FS_ARM_HITS", &hits)
        .deadline(Duration::from_secs(300));
    if commit {
        run = run.env(COMMIT, "1");
    }
    let output = run.wait().unwrap();
    assert_eq!(
        output.status,
        RunStatus::Exited(0),
        "{} {armed}: {} {}",
        case.name,
        output.stdout_text(),
        output.stderr_text()
    );
    Answer {
        outcome: serde_json::from_slice(&std::fs::read(root.join("outcome.json")).unwrap())
            .unwrap(),
        hits: std::fs::read_to_string(hits).unwrap(),
    }
}

fn report(value: &Value) -> &Value {
    value.get("report").unwrap_or(value)
}

fn assert_answer(case: &Case, armed: &str, answer: &Answer, expected: Expected) {
    let label = format!("{} {armed}", case.name);
    match expected {
        Expected::Code(code) => {
            let error = answer.outcome.as_ref().expect_err(&label);
            assert_eq!(error.code(), &code, "{label}: {error:?}");
            // Round-trip the whole envelope, so a code/detail mismatch is
            // refused by the same reader a client would use.
            let written = serde_json::to_vec(error).unwrap();
            assert_eq!(
                serde_json::from_slice::<ErrorEnvelope>(&written).unwrap(),
                *error
            );
        }
        Expected::Applied | Expected::Healing => {
            let value = report(
                answer
                    .outcome
                    .as_ref()
                    .unwrap_or_else(|error| panic!("{label}: {error:?}")),
            );
            assert_eq!(value["outcome"], "applied", "{label}: {value}");
            assert_eq!(
                value["changeset"],
                if matches!(expected, Expected::Healing) {
                    "healing"
                } else {
                    "committed"
                },
                "{label}: {value}"
            );
            let actual: std::collections::BTreeSet<_> = value["targets"]
                .as_array()
                .unwrap()
                .iter()
                .map(|target| target["path"].as_str().unwrap())
                .collect();
            let mut wanted: std::collections::BTreeSet<_> = case
                .publications
                .iter()
                .map(|publication| publication.path())
                .collect();
            if case
                .publications
                .iter()
                .any(|p| matches!(p, Publication::Respell(_, _)))
            {
                wanted.insert("note.md");
            }
            assert_eq!(actual, wanted, "{label}: every authored position lands");
        }
        Expected::AlreadySetUp => {
            assert_eq!(
                answer.outcome.as_ref().expect(&label)["outcome"],
                "already_set_up",
                "{label}"
            );
        }
    }
}

fn fault(case: &Case, stage: &str, ordinal: Option<usize>, action: &str, expected: Expected) {
    let selector =
        ordinal.map_or_else(|| stage.to_string(), |ordinal| format!("{stage}@{ordinal}"));
    let armed = format!("{selector}={action}");
    let answer = run_child(case, &armed, false);
    let lines: Vec<_> = answer.hits.lines().collect();
    assert!(
        !lines.is_empty(),
        "{} {armed}: the declared arm must fire",
        case.name
    );
    // One publication can sync several ancestors. Each record must still
    // name the declared stage and position, never another fault's firing.
    for line in lines {
        assert!(
            line.contains(&format!("stage={stage} ")),
            "{} {armed}: {}",
            case.name,
            answer.hits
        );
        assert!(
            line.contains(&format!(
                "ordinal={} ",
                ordinal.map_or_else(|| "-".into(), |n| n.to_string())
            )),
            "{} {armed}: {}",
            case.name,
            answer.hits
        );
        if let Some(ordinal) = ordinal {
            let at = case.publications[ordinal - 1].path();
            let expected_path = if stage == "mkdir" {
                Path::new(at).parent().unwrap().to_str().unwrap()
            } else {
                at
            };
            assert!(
                line.contains(&format!("path={expected_path} ")),
                "{} {armed}: {}",
                case.name,
                answer.hits
            );
        }
    }
    assert_answer(case, &armed, &answer, expected);
}

#[allow(clippy::disallowed_macros)] // Harness evidence, not product rendering.
pub(super) fn run() {
    let mut rows = 0;
    for case in cases() {
        let baseline = run_child(&case, "", false);
        assert!(baseline.hits.is_empty());
        assert_answer(&case, "baseline", &baseline, Expected::Applied);
        rows += 1;
        // Content writes stage a shadow, including a byte-identical move's
        // copy. Removals and unchanged case-only respells stage none.
        if case.publications.iter().any(|p| {
            matches!(
                p,
                Publication::Create(_) | Publication::Replace(_) | Publication::Respell(_, true)
            )
        }) {
            for stage in ["stage-create", "stage-write", "stage-sync"] {
                fault(
                    &case,
                    stage,
                    None,
                    "fails",
                    Expected::Code(ReasonCode::VaultWriteFailed),
                );
                rows += 1;
            }
            let armed = "stage-sync=fails,cleanup=fails";
            let answer = run_child(&case, armed, false);
            assert!(
                answer.hits.contains("stage=stage-sync ") && answer.hits.contains("stage=cleanup "),
                "{}: {}",
                case.name,
                answer.hits
            );
            assert_answer(
                &case,
                armed,
                &answer,
                Expected::Code(ReasonCode::VaultWriteFailed),
            );
            rows += 1;
        }
        for (index, publication) in case.publications.iter().enumerate() {
            let ordinal = index + 1;
            let before_landing =
                if index == 0 && !matches!(publication, Publication::Respell(_, true)) {
                    ReasonCode::VaultWriteFailed
                } else {
                    ReasonCode::VaultPlanInterrupted
                };
            fault(
                &case,
                publication.stage(),
                Some(ordinal),
                "fails",
                Expected::Code(before_landing.clone()),
            );
            if !matches!(
                publication,
                Publication::Remove(_) | Publication::Respell(_, false)
            ) {
                let armed = format!(
                    "{}@{ordinal}=fails,cleanup@{ordinal}=fails",
                    publication.stage()
                );
                let answer = run_child(&case, &armed, false);
                assert!(
                    answer
                        .hits
                        .contains(&format!("stage={} ordinal={ordinal} ", publication.stage()))
                        && answer
                            .hits
                            .contains(&format!("stage=cleanup ordinal={ordinal} ")),
                    "{} {armed}: {}",
                    case.name,
                    answer.hits
                );
                assert_answer(&case, &armed, &answer, Expected::Code(before_landing));
                rows += 1;
            }
            if matches!(publication, Publication::Respell(_, true)) {
                fault(
                    &case,
                    "swap",
                    Some(ordinal),
                    "fails",
                    Expected::Code(ReasonCode::VaultWriteFailed),
                );
                rows += 1;
            }
            fault(
                &case,
                "parent-sync",
                Some(ordinal),
                "fails",
                Expected::Code(ReasonCode::VaultPlanInterrupted),
            );
            let foreign = match (index, case.verb) {
                (0, Verb::Init) => Expected::AlreadySetUp,
                (0, Verb::Migrate) => Expected::Code(ReasonCode::VaultMigrationRefused),
                (0, _) => Expected::Code(ReasonCode::VaultPlanRefused),
                _ => Expected::Code(ReasonCode::VaultPlanInterrupted),
            };
            let action = if matches!(publication, Publication::Create(_)) {
                "take"
            } else {
                "edit"
            };
            fault(&case, "foreign", Some(ordinal), action, foreign);
            rows += 3;
        }
        if case.mkdir {
            fault(
                &case,
                "mkdir",
                Some(1),
                "fails",
                Expected::Code(ReasonCode::VaultWriteFailed),
            );
            rows += 1;
        }
        if case.rmdir {
            fault(&case, "rmdir", None, "fails", Expected::Applied);
            rows += 1;
        }
        // Control-file plans commit no document changeset. Their reload is
        // a separate act, so it is not a store-commit position in this table.
        if case.document_changeset {
            let answer = run_child(&case, "", true);
            assert_answer(&case, "store-commit", &answer, Expected::Healing);
            rows += 1;
        }
    }
    println!("write-fault-table: {rows} checked rows");
}
