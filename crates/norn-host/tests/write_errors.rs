//! Write outcomes under protocol faults and generated caller input.
//!
//! Faults run in children because the write kernel reads its arms once per
//! process. Every request goes through its host handler, including init and
//! migrate, whose handling of a foreign write differs from a bare apply.
#![cfg(all(unix, feature = "induced-failure"))]
#![allow(clippy::disallowed_methods)] // Harness files and child processes.

mod attach;
#[path = "write_errors/faults.rs"]
mod faults;
#[path = "write_errors/inputs.rs"]
mod inputs;

use norn_config::migration::{Format, Ladder, Step};
use norn_wire::*;
use serde_json::{Value, json};

const SCHEMA: &str = "version: 1\ncreatable:\n  note:\n    target: \"new/rule-{{seq}}.md\"\ninbox:\n  target: \"new/inbox-{{seq}}.md\"\n";
const SUBJECT: &str =
    "---\nstatus: draft\nitems: [one]\n---\n# Subject\ndraft text\n## Section\nsection text\n";

#[derive(Clone, Copy, Debug)]
enum Verb {
    Set,
    Edit,
    New,
    NewAs,
    Move,
    Delete,
    Rewrite,
    Apply,
    Init,
    Migrate,
}

impl Verb {
    const ALL: [Self; 10] = [
        Self::Set,
        Self::Edit,
        Self::New,
        Self::NewAs,
        Self::Move,
        Self::Delete,
        Self::Rewrite,
        Self::Apply,
        Self::Init,
        Self::Migrate,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Set => "set",
            Self::Edit => "edit",
            Self::New => "new",
            Self::NewAs => "new --as",
            Self::Move => "move",
            Self::Delete => "delete",
            Self::Rewrite => "rewrite-wikilink",
            Self::Apply => "apply",
            Self::Init => "init",
            Self::Migrate => "vault migrate",
        }
    }
}

fn path(text: &str) -> DocumentPath {
    DocumentPath::new(text).expect("a document path")
}

fn target(text: &str) -> ResolutionTarget {
    ResolutionTarget::new(text).expect("a target")
}

fn address() -> VaultAddress {
    VaultAddress::name(VaultName::new("notes").expect("a name"))
}

fn apply_request(operations: Vec<Operation>, mode: ApplyMode) -> Value {
    json!(ApplyParams::new(
        mode,
        PlanDocument::operations(AuthoredPlan::new(address(), operations))
    ))
}

fn request(verb: Verb, mode: ApplyMode) -> Value {
    match verb {
        Verb::Set => json!(SetParams::new(
            address(),
            mode,
            WriteTarget::path(path("subject.md")),
            vec![FieldChange::set("status", AuthoredValue::string("final"))]
        )),
        Verb::Edit => json!(EditParams::new(
            address(),
            mode,
            path("subject.md"),
            vec![DocumentEdit::str_replace("draft text", "final text")]
        )),
        Verb::New => json!(NewParams::new(
            address(),
            mode,
            path("new/document.md"),
            "# New\n"
        )),
        Verb::NewAs => json!(NewParams::for_subject(
            address(),
            mode,
            NewSubject::by_rule(
                "note",
                Variables::default(),
                ValueMap::default(),
                Some("# Rule\n".into())
            )
        )),
        Verb::Move => json!(MoveParams::new(
            address(),
            mode,
            MoveSubject::new("source.md", "new/moved.md").expect("a move")
        )),
        Verb::Delete => json!(
            DeleteParams::new(address(), mode, path("source.md"))
                .rewriting_to(target("destination"))
        ),
        Verb::Rewrite => json!(RewriteWikilinkParams::new(
            address(),
            mode,
            target("source"),
            target("destination")
        )),
        Verb::Apply => apply_request(
            vec![
                Operation::new(OperationKind::create_document(
                    path("new/document.md"),
                    "# New\n",
                )),
                Operation::new(OperationKind::str_replace(
                    path("subject.md"),
                    "draft text",
                    "final text",
                )),
                Operation::new(OperationKind::delete_document(path("gone.md"))),
            ],
            mode,
        ),
        Verb::Init => json!(InitParams::new(address(), mode)),
        Verb::Migrate => json!(MigrateParams::new(address(), mode)),
    }
}

/// Both formats remain parseable by the running grammar. The test ladders
/// change comments so the migration handler has two real replacements to run.
fn ladder(format: Format) -> Ladder {
    Ladder {
        format,
        current: 2,
        version_of: |text| Ok(if text.contains("# migrated") { 2 } else { 1 }),
        steps: vec![Step {
            from: 1,
            rewrite: |text| format!("{text}# migrated\n"),
        }],
    }
}

enum WriteRequest {
    Set(SetParams),
    Edit(EditParams),
    New(NewParams),
    Move(MoveParams),
    Delete(DeleteParams),
    Rewrite(RewriteWikilinkParams),
    Apply(ApplyParams),
    Init(InitParams),
    Migrate(MigrateParams),
}

impl WriteRequest {
    fn mode(&self) -> ApplyMode {
        match self {
            Self::Set(params) => params.mode,
            Self::Edit(params) => params.mode,
            Self::New(params) => params.mode,
            Self::Move(params) => params.mode,
            Self::Delete(params) => params.mode,
            Self::Rewrite(params) => params.mode,
            Self::Apply(params) => params.mode,
            Self::Init(params) => params.mode,
            Self::Migrate(params) => params.mode,
        }
    }

    fn read(verb: Verb, text: &str) -> Result<Self, serde_json::Error> {
        Ok(match verb {
            Verb::Set => Self::Set(serde_json::from_str(text)?),
            Verb::Edit => Self::Edit(serde_json::from_str(text)?),
            Verb::New | Verb::NewAs => Self::New(serde_json::from_str(text)?),
            Verb::Move => Self::Move(serde_json::from_str(text)?),
            Verb::Delete => Self::Delete(serde_json::from_str(text)?),
            Verb::Rewrite => Self::Rewrite(serde_json::from_str(text)?),
            Verb::Apply => Self::Apply(serde_json::from_str(text)?),
            Verb::Init => Self::Init(serde_json::from_str(text)?),
            Verb::Migrate => Self::Migrate(serde_json::from_str(text)?),
        })
    }

    /// Preserve a generated root mutation. Only the seed's unchanged root
    /// follows the identical tree into the fresh fixture that executes it.
    fn bind_fixture(&mut self, seed_root: Option<&RootIdentity>, fixture_root: RootIdentity) {
        if let Self::Apply(params) = self
            && let PlanDocument::Resolved(plan) = &mut params.plan
            && Some(&plan.root) == seed_root
        {
            plan.root = fixture_root;
        }
    }

    fn answer(
        self,
        host: &attach::ServingHost,
        test_ladders: bool,
    ) -> Result<Value, ErrorEnvelope> {
        let pending = match self {
            Self::Set(params) => host.set(params),
            Self::Edit(params) => host.edit(params),
            Self::New(params) => host.new_document(params),
            Self::Move(params) => host.move_path(params),
            Self::Delete(params) => host.delete(params),
            Self::Rewrite(params) => host.rewrite_wikilink(params),
            Self::Apply(params) => host.apply(params),
            Self::Init(params) => return host.init(&params).map(|report| json!(report)),
            Self::Migrate(params) => {
                let answer = if test_ladders {
                    host.vault_migrate_with_ladders(
                        &params,
                        &ladder(Format::Yaml),
                        &ladder(Format::Toml),
                    )
                } else {
                    host.vault_migrate(&params)
                };
                return answer.map(|report| json!(report));
            }
        };
        pending
            .and_then(|pending| pending.wait())
            .map(|answer| json!(answer.report))
    }
}

/// Decode failures end before the host is entered.
fn dispatch(
    host: &attach::ServingHost,
    verb: Verb,
    request: &str,
    test_ladders: bool,
) -> Result<Result<Value, ErrorEnvelope>, serde_json::Error> {
    Ok(WriteRequest::read(verb, request)?.answer(host, test_ladders))
}

#[test]
fn write_fault_child() {
    faults::child();
}

/// Every verb and applicable fault position has an independently authored
/// expected wire outcome. A missing arm record cannot count as a passing row.
#[test]
fn every_write_failure_answers_a_code_from_its_operations_domain() {
    faults::run();
}

#[test]
fn generated_write_requests_answer_only_codes_from_their_operations_domain() {
    inputs::run();
}
