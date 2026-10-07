//! Control-file writes judged and applied by the applier over a vault on
//! disk whose walk does not enter the schema's path, as the host's does not:
//! what lands, what the changeset records, and what refuses.

use norn_wire::{
    ControlFile, ErrorDetail, FileState, Operation, OperationKind, PlanFault, ReasonCode,
    RefusedCheck, TargetResult, Transition,
};

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use norn_fs::ShadowHome;

use super::super::{Applier, ApplyOutcome};
use super::{Fixture, applied, deleting, path, refused, results};
use crate::planner::compose::content_hash;
use crate::planner::control::SchemaPlace;
use crate::planner::view::{TreeView, VaultView};

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
        "version: 1\nfields:\n  status:\n    type: text\nrules:\n  every:\n    required:\n      status:\ntags:\n  declared: [ok]\n  undeclared: report\n",
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

/// **A schema write lands where the registration reads the schema** (ADR
/// 0034), its transition still naming the schema's role: at a source
/// outside the vault, with no own write recorded, since the ledger names
/// vault paths only, and nothing at the default path; and a source another
/// writer changed since the plan was resolved refuses it as drift at the
/// role's path, writing nothing.
#[test]
#[allow(clippy::disallowed_methods)] // Harness scaffolding: the source a case arranges.
fn a_schema_write_lands_at_a_source_outside_the_vault() {
    let mut fixture = Fixture::new(&[("a.md", "# A\n")]);
    let folder = fixture.vault.parent().expect("a parent").join("shared");
    std::fs::create_dir_all(&folder).expect("the source's folder");
    std::fs::write(folder.join("schema.yaml"), "version: 1\n").expect("the source");
    fixture.schema = SchemaPlace::Outside {
        folder: folder.clone(),
        name: "schema.yaml".into(),
        shadows: fixture.shadows.clone(),
    };
    let schema = "version: 1\n# shared\n";
    let plan = fixture.plan(vec![writing(ControlFile::Schema, schema)]);
    assert_eq!(
        plan.transitions,
        vec![Transition::new(
            path(SCHEMA),
            present("version: 1\n"),
            present(schema)
        )]
    );
    let stale = plan.clone();
    let applied = applied(fixture.apply(plan));
    assert_eq!(
        results(&applied),
        vec![(SCHEMA.to_string(), TargetResult::Wrote)]
    );
    assert_eq!(
        std::fs::read_to_string(folder.join("schema.yaml"))
            .ok()
            .as_deref(),
        Some(schema)
    );
    assert_eq!(fixture.read(SCHEMA), None);
    assert!(fixture.recorded.calls.borrow().is_empty());
    assert!(fixture.shadows_left().is_empty());

    std::fs::write(folder.join("schema.yaml"), "version: 1\n# theirs\n").expect("an edit");
    let refused = refused(fixture.apply(stale));
    assert_eq!(
        refused.checks,
        vec![RefusedCheck::drifted(
            path(SCHEMA),
            present("version: 1\n# theirs\n")
        )]
    );
    assert_eq!(
        std::fs::read_to_string(folder.join("schema.yaml"))
            .ok()
            .as_deref(),
        Some("version: 1\n# theirs\n")
    );
}

/// **A schema write lands at a source inside the vault**, at the source's
/// own path, recorded as an own write there; the default path is left as it
/// stands.
#[test]
fn a_schema_write_lands_at_a_source_inside_the_vault() {
    let source = "schemas/notes.yaml";
    let mut fixture = Fixture::new(&[("a.md", "# A\n")]);
    fixture.write(source, "version: 1\n");
    fixture.exclusions.push(source.into());
    fixture.schema = SchemaPlace::InVault(source.into());
    let schema = "version: 1\n# inside\n";
    let plan = fixture.plan(vec![writing(ControlFile::Schema, schema)]);
    let applied = applied(fixture.apply(plan));
    assert_eq!(
        results(&applied),
        vec![(SCHEMA.to_string(), TargetResult::Wrote)]
    );
    assert_eq!(fixture.read(source).as_deref(), Some(schema));
    assert_eq!(fixture.read(SCHEMA), None);
    assert_eq!(
        *fixture.recorded.calls.borrow(),
        vec![(std::path::PathBuf::from(source), true)]
    );
}

/// A vault holding `a.md`, its schema read from `schema.yaml` in a folder
/// `shared` outside it — holding `source` where it is `Some` — staged
/// through `shadows`, or the vault's own home where that is `None`.
#[allow(clippy::disallowed_methods)] // Harness scaffolding: the source a case arranges.
fn outside(source: Option<&str>) -> (Fixture, PathBuf) {
    let mut fixture = Fixture::new(&[("a.md", "# A\n")]);
    let folder = fixture.vault.parent().expect("a parent").join("shared");
    std::fs::create_dir_all(&folder).expect("the source's folder");
    if let Some(source) = source {
        std::fs::write(folder.join("schema.yaml"), source).expect("the source");
    }
    fixture.schema = SchemaPlace::Outside {
        folder: folder.clone(),
        name: "schema.yaml".into(),
        shadows: fixture.shadows.clone(),
    };
    (fixture, folder)
}

#[allow(clippy::disallowed_methods)] // Harness scaffolding: the source a case reads back.
fn source_at(folder: &Path) -> Option<String> {
    std::fs::read_to_string(folder.join("schema.yaml")).ok()
}

/// **A source folder replaced between staging and publication publishes
/// nothing** (ADR 0034): its identity, recorded when the schema was staged,
/// no longer names the folder at its path, and the apply is answered as a
/// write that failed naming that folder — never as the vault root changing,
/// which it did not. Neither the folder staged against nor its replacement
/// is written, and no shadow is left.
#[test]
#[allow(clippy::disallowed_methods)] // Harness scaffolding: the swap a case arranges.
fn a_source_folder_replaced_before_publication_publishes_nothing() {
    let (mut fixture, folder) = outside(Some("version: 1\n"));
    let plan = fixture.plan(vec![writing(ControlFile::Schema, "version: 1\n# shared\n")]);
    let moved = folder.with_file_name("shared-old");
    let swap = || {
        std::fs::rename(&folder, &moved).expect("the folder moves away");
        std::fs::create_dir(&folder).expect("another folder in its place");
        std::fs::write(folder.join("schema.yaml"), "version: 1\n").expect("the same bytes");
        true
    };
    let links = fixture.links();
    let applier = Applier {
        anchor: &fixture.vault,
        root: fixture.root,
        exclusions: &fixture.exclusions,
        schema: &fixture.schema,
        shadows: &fixture.shadows,
        own_writes: &fixture.recorded,
        publishing: &swap,
        links: &links.index(),
    };
    match applier.apply(plan, &RefCell::new(&mut fixture.store)) {
        ApplyOutcome::WriteFailed { detail, .. } => {
            assert!(detail.contains("was replaced"), "{detail}");
            assert!(detail.contains(&folder.display().to_string()), "{detail}");
        }
        other => panic!("the apply answered {other:?}"),
    }
    assert_eq!(source_at(&moved).as_deref(), Some("version: 1\n"));
    assert_eq!(source_at(&folder).as_deref(), Some("version: 1\n"));
    assert!(fixture.recorded.calls.borrow().is_empty());
    assert!(
        fixture.shadows_left().is_empty(),
        "{:?}",
        fixture.shadows_left()
    );
}

/// **A source folder gone between the check and staging stops the plan
/// before anything is staged, naming why**: a replace of the source no
/// longer finds the bytes it was checked against, and refuses as drift at
/// the role's path; a create has no folder to make its file in, and fails
/// naming that folder.
#[test]
#[allow(clippy::disallowed_methods)] // Harness scaffolding: the removal a case arranges.
fn a_source_folder_gone_before_staging_stops_the_plan() {
    let stage_after_removal = |source: Option<&str>| {
        let (mut fixture, folder) = outside(source);
        let plan = fixture.plan(vec![writing(ControlFile::Schema, "version: 1\n# shared\n")]);
        let view =
            TreeView::open(&fixture.vault, &fixture.exclusions, &fixture.schema).expect("a vault");
        let declared = crate::production::pinned_declaration(&mut fixture.store).expect("a schema");
        let links = fixture.links();
        let checked = super::super::stage::check(
            &plan,
            &view,
            &declared,
            &links.index(),
            &mut super::super::schema::Citations::default(),
        )
        .expect("the plan checks");
        std::fs::remove_dir_all(&folder).expect("the folder goes");
        let ground = super::super::place::Ground {
            vault: &fixture.vault,
            root: fixture.root,
            schema: &fixture.schema,
        };
        let stopped = super::super::stage::stage(
            &ground,
            &fixture.shadows,
            &plan,
            view.normalizer(),
            checked,
        )
        .expect_err("nothing stands to stage against");
        assert!(
            fixture.shadows_left().is_empty(),
            "{:?}",
            fixture.shadows_left()
        );
        (stopped, folder)
    };
    match stage_after_removal(Some("version: 1\n")) {
        (super::super::stage::Stop::Refused(checks), _) => assert_eq!(
            checks,
            vec![RefusedCheck::drifted(path(SCHEMA), FileState::absent())]
        ),
        (other, _) => panic!("a replace stopped with {other:?}"),
    }
    match stage_after_removal(None) {
        (super::super::stage::Stop::Failed(detail), folder) => {
            assert!(detail.contains("is gone"), "{detail}");
            assert!(detail.contains(&folder.display().to_string()), "{detail}");
        }
        (other, _) => panic!("a create stopped with {other:?}"),
    }
}

/// **A schema write to a source outside the vault does not resolve where
/// the vault's shadow home is the in-vault fallback** (ADR 0034): the write
/// kernel reaches that home only through the vault root, so nothing it
/// stages there can be renamed into the source's folder. The operation is
/// left unresolved naming why, and the source is not written.
#[test]
fn a_schema_write_outside_the_vault_does_not_resolve_from_the_fallback_home() {
    let (mut fixture, folder) = outside(Some("version: 1\n"));
    let fallback = ShadowHome::resolve_stating(
        &fixture.vault,
        &fixture.data.join("tmp"),
        &norn_fs::MaintainershipKey::new("test", "vault", "data").expect("a key"),
        false,
    )
    .expect("the fallback home");
    fixture.schema = SchemaPlace::Outside {
        folder: folder.clone(),
        name: "schema.yaml".into(),
        shadows: fallback,
    };
    let view =
        TreeView::open(&fixture.vault, &fixture.exclusions, &fixture.schema).expect("a vault");
    let links = fixture.links();
    let authored = norn_wire::AuthoredPlan::new(
        norn_wire::VaultAddress::name(norn_wire::VaultName::new("notes").expect("a name")),
        vec![writing(ControlFile::Schema, "version: 1\n# shared\n")],
    );
    let resolution = crate::planner::resolve::resolve(
        authored,
        fixture.root_identity(),
        &std::collections::BTreeSet::new(),
        &view,
        &links.index(),
    )
    .expect("the plan is planned");
    assert!(
        resolution.plan.transitions.is_empty(),
        "{:?}",
        resolution.plan.transitions
    );
    assert_eq!(
        resolution.unresolved.len(),
        1,
        "{:?}",
        resolution.unresolved
    );
    let reason = format!("{:?}", resolution.unresolved[0].reason);
    assert!(
        reason.contains("no write outside the vault can publish from"),
        "{reason}"
    );
    assert_eq!(source_at(&folder).as_deref(), Some("version: 1\n"));
}
