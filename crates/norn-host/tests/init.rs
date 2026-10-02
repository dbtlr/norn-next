//! **`init`, end to end**: `Host::init` over a real registered vault and a
//! real attachment.
//!
//! What is pinned here is what a caller of `init` sees: a preview on a vault
//! that declares no schema plans one starter schema, listing every field the
//! vault's documents carry and writing nothing; an apply lands it and the
//! vault serves under it with its findings as they were; a vault whose schema
//! stands is already set up; a registration reading its schema from a
//! source is sent there; and a schema another writer creates after a preview
//! refuses the previewed plan rather than being replaced.
#![cfg(unix)]
#![allow(clippy::disallowed_methods)] // Harness scaffolding: this suite's own tree.

mod attach;

use std::path::Path;

use norn_config::schema::VaultSchema;
use norn_testkit::process::Sandbox;
use norn_wire::{
    AppliedTarget, ApplyMode, ApplyParams, ApplyReport, ControlFile, DocumentPath, ErrorDetail,
    FileState, FindingKind, InitParams, InitReport, OperationKind, PlanDocument, ReasonCode,
    RefusedCheck, ResolvedPlan, SchemaSource, TargetResult, ValidateParams, ValidateReport,
    VaultAddress,
};

const SCHEMA: &str = ".norn/schema.yaml";

/// A registered vault holding exactly `files`, with no schema.
fn a_vault(label: &str, files: &[(&str, &str)]) -> (Sandbox, attach::Vault) {
    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), label).expect("a sandbox");
    let root = sandbox.work_dir().join("attached");
    for (at, content) in files {
        let path = root.join("vault").join(at);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("make the parent");
        std::fs::write(path, content).expect("write a document");
    }
    std::fs::create_dir_all(root.join("vault")).expect("the vault root");
    let vault = attach::Vault::adopt(&root);
    (sandbox, vault)
}

/// Documents carrying `status` twice as a scalar, `tags` as a sequence and a
/// scalar, and `owner` as a map, beside one with no frontmatter and one whose
/// link names nothing.
const FILES: &[(&str, &str)] = &[
    ("a.md", "---\nstatus: draft\ntags: [x]\n---\nA\n"),
    ("b.md", "---\nstatus: done\ntags: y\n---\nB\n"),
    ("c.md", "---\nowner:\n  name: n\n---\nC\n"),
    ("d.md", "plain\n"),
    ("e.md", "links [[missing]]\n"),
];

/// The observed-field lines the starter over [`FILES`] ends with.
const OBSERVED: &str = "\
# \"owner\": 1 document; map
# \"status\": 2 documents; scalar
# \"tags\": 2 documents; scalar, sequence
";

fn params(vault: &attach::Vault, mode: ApplyMode) -> InitParams {
    InitParams::new(VaultAddress::name(vault.name().clone()), mode)
}

fn schema() -> DocumentPath {
    DocumentPath::new(SCHEMA).expect("a vault path")
}

/// The resolved plan a scaffolded preview answered.
fn previewed(report: InitReport) -> ResolvedPlan {
    let InitReport::Scaffolded {
        report: scaffolded, ..
    } = report
    else {
        panic!("an init preview answered {report:?}");
    };
    let ApplyReport::Previewed { plan, .. } = *scaffolded else {
        panic!("an init preview answered {scaffolded:?}");
    };
    plan
}

/// The content the starter's one operation writes.
fn starter_of(plan: &ResolvedPlan) -> String {
    let [operation] = plan.operations.as_slice() else {
        panic!("a starter plan is one operation: {:?}", plan.operations);
    };
    let OperationKind::WriteControlFile {
        file: ControlFile::Schema,
        content,
    } = &operation.kind
    else {
        panic!("a starter plan writes the schema: {:?}", operation.kind);
    };
    content.clone()
}

/// The findings standing over the vault, by kind and path.
fn findings(host: &attach::ServingHost, vault: &attach::Vault) -> Vec<(FindingKind, String)> {
    let answered = host
        .validate(&ValidateParams::new(VaultAddress::name(
            vault.name().clone(),
        )))
        .expect("a served vault answers a validate");
    let ValidateReport::Findings { page, .. } = answered.answer.report else {
        panic!("a validate answered {:?}", answered.answer.report);
    };
    page.rows
        .iter()
        .map(|row| (row.kind, row.path.as_str().to_string()))
        .collect()
}

/// **A preview on a registered vault with no schema plans exactly one
/// `write_control_file` creating `.norn/schema.yaml`**, whose content reads
/// as a schema declaring nothing and lists every observed field with how many
/// documents carry it and its shapes, in key order; two previews write the
/// same bytes, and neither writes anything.
#[test]
fn a_preview_on_a_vault_with_no_schema_plans_one_starter_listing_every_observed_field() {
    let (_sandbox, vault) = a_vault("host-init-preview", FILES);
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());

    let plan = previewed(
        host.init(&params(&vault, ApplyMode::Preview))
            .expect("an init preview answers"),
    );
    let starter = starter_of(&plan);
    assert_eq!(plan.transitions.len(), 1, "{:?}", plan.transitions);
    assert_eq!(plan.transitions[0].path, schema());
    assert_eq!(plan.transitions[0].before, FileState::absent());
    assert_eq!(
        VaultSchema::parse(starter.as_bytes()).expect("the starter parses"),
        VaultSchema::default(),
        "the starter declares something"
    );
    assert!(starter.contains("\nversion: 1\n"), "{starter}");
    assert!(
        starter.ends_with(OBSERVED),
        "the starter does not list the observed fields:\n{starter}"
    );

    let again = previewed(
        host.init(&params(&vault, ApplyMode::Preview))
            .expect("an init preview answers"),
    );
    assert_eq!(
        starter_of(&again),
        starter,
        "two previews wrote other bytes"
    );
    assert!(!vault.path().join(SCHEMA).exists(), "a preview wrote");
}

/// **An init applied lands the starter, the vault reloads under it, and its
/// findings are as they were**: the schema on disk is the previewed content,
/// the fingerprint the vault serves is that content's, the advisory that the
/// schema is absent is gone, and the broken link's finding stands as it did.
#[test]
fn an_init_applied_lands_the_starter_and_the_vault_reloads_under_it_findings_unchanged() {
    let (_sandbox, vault) = a_vault("host-init-apply", FILES);
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let before = findings(&host, &vault);
    assert!(
        before.contains(&(FindingKind::Broken, "e.md".to_string())),
        "{before:?}"
    );
    let starter = starter_of(&previewed(
        host.init(&params(&vault, ApplyMode::Preview))
            .expect("an init preview answers"),
    ));

    let InitReport::Scaffolded {
        report: scaffolded, ..
    } = host
        .init(&params(&vault, ApplyMode::Apply))
        .expect("an init applies")
    else {
        panic!("an init apply answered another outcome");
    };
    let ApplyReport::Applied { targets, .. } = *scaffolded else {
        panic!("an init apply answered {scaffolded:?}");
    };
    assert_eq!(
        targets,
        vec![AppliedTarget::new(schema(), TargetResult::Wrote)]
    );
    assert_eq!(
        std::fs::read_to_string(vault.path().join(SCHEMA)).expect("the schema landed"),
        starter
    );
    let active = host
        .inspect(vault.name())
        .expect("the vault is served")
        .active_fingerprints
        .expect("the vault serves fingerprints");
    assert_eq!(active.schema, norn_fs::ContentHash::of(starter.as_bytes()));
    let status = host
        .vault_status(
            &norn_wire::StatusParams::new().with_vault(VaultAddress::name(vault.name().clone())),
        )
        .expect("a status answers");
    let norn_wire::StatusReport::Vault { status, .. } = status else {
        panic!("a vault status answered {status:?}");
    };
    assert!(status.advisories.is_empty(), "{:?}", status.advisories);
    assert_eq!(status.drift, norn_wire::Drift::current());
    assert_eq!(findings(&host, &vault), before);
}

/// **Run again over a vault whose schema stands, init is already set up**:
/// nothing is planned and nothing written, previewed or applied, whether the
/// schema is the starter an init wrote or one the vault came with.
#[test]
fn an_init_over_a_standing_schema_is_already_set_up_and_writes_nothing() {
    let (_sandbox, vault) = a_vault("host-init-again", FILES);
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    host.init(&params(&vault, ApplyMode::Apply))
        .expect("an init applies");
    let written = std::fs::read(vault.path().join(SCHEMA)).expect("the schema landed");

    for mode in [ApplyMode::Preview, ApplyMode::Apply] {
        assert_eq!(
            host.init(&params(&vault, mode)).expect("an init answers"),
            InitReport::already_set_up(schema())
        );
    }
    std::fs::write(vault.path().join(SCHEMA), "version: 1\nfields: {}\n").expect("an edit");
    assert_eq!(
        host.init(&params(&vault, ApplyMode::Apply))
            .expect("an init answers"),
        InitReport::already_set_up(schema())
    );
    assert_ne!(
        std::fs::read(vault.path().join(SCHEMA)).expect("the schema stands"),
        written,
        "the vault's own schema was replaced"
    );
}

/// **A registration reading its schema from a source writes nothing and
/// names the source**, whether the source lies outside the vault or inside
/// it at another path than the default.
#[test]
fn an_init_whose_registration_reads_its_schema_elsewhere_writes_nothing_and_names_it() {
    let (sandbox, vault) = a_vault("host-init-elsewhere", FILES);
    let outside = sandbox.work_dir().join("shared/schema.yaml");
    let inside = vault.path().join("schemas/vault.yaml");
    for source in [&outside, &inside] {
        std::fs::create_dir_all(source.parent().expect("a parent")).expect("the folder");
        std::fs::write(source, "version: 1\n").expect("the schema");
        let source = SchemaSource::new(source).expect("a schema source");
        let host = vault.host_reading_schema_from(source.clone());
        let _lease = attach::attach_and_wait(&host, vault.name());
        for mode in [ApplyMode::Preview, ApplyMode::Apply] {
            assert_eq!(
                host.init(&params(&vault, mode)).expect("an init answers"),
                InitReport::schema_elsewhere(source.clone())
            );
        }
        assert!(!vault.path().join(SCHEMA).exists(), "init wrote a schema");
    }
}

/// **A schema another writer creates between a preview and its apply refuses
/// the apply by create exclusivity**: the previewed plan sent back is refused
/// naming the schema as drifted, nothing of it is written, its fresh plan
/// starts from the schema that writer left, and init run again reports the
/// vault already set up.
#[test]
fn a_schema_created_after_the_preview_refuses_the_previewed_apply() {
    let (_sandbox, vault) = a_vault("host-init-race", FILES);
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let plan = previewed(
        host.init(&params(&vault, ApplyMode::Preview))
            .expect("an init preview answers"),
    );
    let theirs = "version: 1\n# theirs\n";
    std::fs::create_dir_all(vault.path().join(".norn")).expect("the control folder");
    std::fs::write(vault.path().join(SCHEMA), theirs).expect("another writer's schema");

    let refused = host
        .apply(ApplyParams::new(
            ApplyMode::Apply,
            PlanDocument::resolved(plan),
        ))
        .expect("an apply is admitted")
        .wait()
        .expect_err("a taken name refuses the create");
    assert_eq!(refused.code(), &ReasonCode::VaultPlanRefused);
    let ErrorDetail::PlanRefused { plan, checks, .. } = refused.detail() else {
        panic!("refused with {:?}", refused.detail());
    };
    let hash = |text: &str| {
        norn_wire::ContentHash::new(format!(
            "sha256:{}",
            norn_fs::ContentHash::of(text.as_bytes()).to_hex()
        ))
        .expect("a content hash")
    };
    assert_eq!(
        checks,
        &vec![RefusedCheck::drifted(
            schema(),
            FileState::present(hash(theirs))
        )]
    );
    assert_eq!(plan.transitions[0].before, FileState::present(hash(theirs)));
    assert_eq!(
        std::fs::read_to_string(vault.path().join(SCHEMA)).expect("their schema stands"),
        theirs
    );
    assert_eq!(
        host.init(&params(&vault, ApplyMode::Apply))
            .expect("an init answers"),
        InitReport::already_set_up(schema())
    );
}

/// **Over a schema that stands, init reads no field universe**: it learns the
/// schema stands before any describe or count, so it answers `already_set_up`
/// whatever keys the documents carry — one holding U+FFFF, which once made
/// the starter unreadable, included — and what it runs on its snapshot does
/// not grow with how many keys they carry.
#[test]
fn an_init_over_a_standing_schema_answers_before_reading_the_field_universe() {
    let statements = |label: &str, keys: usize| {
        let mut files = vec![
            (SCHEMA.to_string(), "version: 1\n".to_string()),
            (
                "odd.md".to_string(),
                "---\n\"x\\uFFFFy\": 1\n---\nA\n".to_string(),
            ),
        ];
        files.extend((0..keys).map(|key| (format!("k{key}.md"), format!("---\nk{key}: 1\n---\n"))));
        let files: Vec<(&str, &str)> = files
            .iter()
            .map(|(at, content)| (at.as_str(), content.as_str()))
            .collect();
        let (_sandbox, vault) = a_vault(label, &files);
        let host = vault.host();
        let _lease = attach::attach_and_wait(&host, vault.name());
        let run = norn_store::SnapshotReader::statements_run_on_this_thread;
        let before = run();
        for mode in [ApplyMode::Preview, ApplyMode::Apply] {
            assert_eq!(
                host.init(&params(&vault, mode)).expect("an init answers"),
                InitReport::already_set_up(schema())
            );
        }
        run() - before
    };
    assert_eq!(
        statements("host-init-standing-few", 1),
        statements("host-init-standing-many", 20),
        "an init over a standing schema read every key"
    );
}
