//! Control-file writes judged and applied by the applier over a vault on
//! disk whose walk does not enter the schema's path, as the host's does not:
//! what lands, what the changeset records, and what refuses.

use norn_wire::{
    ControlFile, ErrorDetail, FileState, Operation, OperationKind, PlanFault, ReasonCode,
    RefusedCheck, TargetResult, Transition,
};

use super::{Fixture, applied, deleting, path, refused, results};
use crate::planner::compose::content_hash;

const SCHEMA: &str = ".norn/schema.yaml";
const CONFIG: &str = ".norn/config.toml";

/// A vault holding `files` beside a schema declaring nothing, its walk not
/// entering the schema's path.
fn fixture(files: &[(&str, &str)]) -> Fixture {
    let mut fixture = Fixture::new(files);
    fixture.write(SCHEMA, "version: 1\n");
    fixture.exclusions.push(SCHEMA.into());
    fixture
}

fn writing(file: ControlFile, content: &str) -> Operation {
    Operation::new(OperationKind::write_control_file(file, content))
}

fn present(content: &str) -> FileState {
    FileState::present(content_hash(content.as_bytes()))
}

/// **A control-file write replacing the file its before-state names lands,
/// and the changeset records no document for it**: the schema is replaced,
/// the absent config created in the existing control folder, both reported
/// written, and the store is what a build from zero over the tree holds —
/// no row at either control file.
#[test]
fn a_control_file_write_lands_and_the_changeset_records_no_document_for_it() {
    let mut fixture = fixture(&[("a.md", "# A\n")]);
    let schema = "version: 1\n# a comment\n";
    let config = "[engine.sample]\n";
    let plan = fixture.plan(vec![
        writing(ControlFile::Schema, schema),
        writing(ControlFile::Config, config),
    ]);
    assert_eq!(
        plan.transitions,
        vec![
            Transition::new(path(CONFIG), FileState::absent(), present(config)),
            Transition::new(path(SCHEMA), present("version: 1\n"), present(schema)),
        ]
    );
    let applied = applied(fixture.apply(plan));
    assert_eq!(
        results(&applied),
        vec![
            (CONFIG.to_string(), TargetResult::Wrote),
            (SCHEMA.to_string(), TargetResult::Wrote),
        ]
    );
    assert!(
        applied.folders_made.is_empty(),
        "{:?}",
        applied.folders_made
    );
    assert_eq!(fixture.read(SCHEMA).as_deref(), Some(schema));
    assert_eq!(fixture.read(CONFIG).as_deref(), Some(config));
    assert_eq!(fixture.stored_paths(), vec!["a.md"]);
    fixture.assert_store_is_a_build_from_zero();
}

/// **A control file another writer created after the plan was resolved
/// refuses the plan's create**, as any create's name taken does, and the
/// fresh plan replaces what that writer left; where the writer left exactly
/// the plan's content, the create is landed and writes nothing.
#[test]
fn a_control_file_another_writer_created_refuses_the_plan_s_create() {
    let mut fixture = fixture(&[]);
    let plan = fixture.plan(vec![writing(ControlFile::Config, "[engine.mine]\n")]);
    fixture.write(CONFIG, "[engine.theirs]\n");
    let refused = refused(fixture.apply(plan.clone()));
    assert_eq!(
        refused.checks,
        vec![RefusedCheck::drifted(
            path(CONFIG),
            present("[engine.theirs]\n")
        )]
    );
    assert_eq!(
        refused.plan.transitions,
        vec![Transition::new(
            path(CONFIG),
            present("[engine.theirs]\n"),
            present("[engine.mine]\n")
        )]
    );
    assert_eq!(fixture.read(CONFIG).as_deref(), Some("[engine.theirs]\n"));

    fixture.write(CONFIG, "[engine.mine]\n");
    let landed = applied(fixture.apply(plan));
    assert_eq!(
        results(&landed),
        vec![(CONFIG.to_string(), TargetResult::Found)]
    );
}

/// **A resolved plan writing a control file beside a document operation is
/// invalid**, whatever its transitions say, and nothing is published.
#[test]
fn a_resolved_control_file_write_beside_a_document_operation_is_invalid() {
    let mut fixture = fixture(&[("a.md", "# A\n")]);
    let mut plan = fixture.plan(vec![writing(ControlFile::Config, "")]);
    plan.operations.push(deleting("a.md"));
    plan.transitions.push(Transition::new(
        path("a.md"),
        present("# A\n"),
        FileState::absent(),
    ));
    let envelope = fixture
        .apply(plan)
        .into_wire()
        .expect("a refusal is answered")
        .expect_err("the plan is refused");
    assert_eq!(envelope.code(), &ReasonCode::RequestPlanInvalid);
    assert_eq!(
        envelope.detail(),
        &ErrorDetail::plan_invalid(PlanFault::control_file_beside_documents(vec![0]))
    );
    assert_eq!(fixture.read("a.md").as_deref(), Some("# A\n"));
    assert_eq!(fixture.read(CONFIG), None);
}

/// **A resolved control-file write whose content does not read as its role
/// is invalid**: planning never resolves one, so a plan carrying one was
/// altered, and the applier refuses it whole rather than publishing a
/// control file the next reload would refuse.
#[test]
fn a_resolved_control_file_write_whose_content_does_not_read_as_its_role_is_invalid() {
    let mut fixture = fixture(&[]);
    let mut plan = fixture.plan(vec![writing(ControlFile::Schema, "version: 1\n# ok\n")]);
    let unreadable = "version: [\n";
    plan.operations = vec![writing(ControlFile::Schema, unreadable)];
    plan.transitions = vec![Transition::new(
        path(SCHEMA),
        present("version: 1\n"),
        present(unreadable),
    )];
    let envelope = fixture
        .apply(plan)
        .into_wire()
        .expect("a refusal is answered")
        .expect_err("the plan is refused");
    assert_eq!(envelope.code(), &ReasonCode::RequestPlanInvalid);
    assert_eq!(fixture.read(SCHEMA).as_deref(), Some("version: 1\n"));
}

/// **A control file is not judged under the vault schema**, though that
/// schema declares fields and tags no control file carries: the config and
/// the schema are written under a pinned schema requiring a field and
/// reporting undeclared tags, each holding a tag and a link a document
/// would be judged for, and both land.
#[test]
fn a_control_file_lands_under_a_schema_that_declares_what_it_does_not_carry() {
    let mut fixture = fixture(&[("a.md", "---\nstatus: x\n---\n#ok\n")]);
    fixture.pin(
        "version: 1\nfields:\n  status:\n    required: true\n    type: text\ntags:\n  declared: [ok]\n  undeclared: report\n",
    );
    let config = "# [[nowhere]] #nope\n";
    let schema = "version: 1\n# [[nowhere]] #nope\n";
    let plan = fixture.plan(vec![
        writing(ControlFile::Config, config),
        writing(ControlFile::Schema, schema),
    ]);
    let applied = applied(fixture.apply(plan));
    assert_eq!(
        results(&applied),
        vec![
            (CONFIG.to_string(), TargetResult::Wrote),
            (SCHEMA.to_string(), TargetResult::Wrote),
        ]
    );
    assert_eq!(fixture.read(CONFIG).as_deref(), Some(config));
    assert_eq!(fixture.read(SCHEMA).as_deref(), Some(schema));
}
