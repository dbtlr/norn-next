//! **`vault migrate`, end to end**: `Host::vault_migrate` over a real
//! registered vault and a real attachment.
//!
//! What is pinned here is what a caller of `vault migrate` sees. The ladders
//! this build ships hold no step, so a vault whose control files read is
//! already current, nothing is planned and nothing written, and a file ahead
//! of this build or one that does not read is refused, naming it. Behind
//! `induced-failure`, which opens the host's harness-reachable surface, a
//! migration runs over test-only ladders whose steps rewrite both files: one
//! plan previews both rewrites, an apply lands them and the vault reloads
//! under what landed, a migration run again is already current with the
//! files byte-identical, and a step that would lose a comment is refused.
#![cfg(unix)]
#![allow(clippy::disallowed_methods)] // Harness scaffolding: this suite's own tree.

mod attach;

use std::path::Path;

use norn_testkit::process::Sandbox;
use norn_wire::{
    ApplyMode, ControlFile, ErrorDetail, MigrateParams, MigrateReport, MigrationRefusal,
    ReasonCode, SchemaSource, VaultAddress,
};

const SCHEMA: &str = ".norn/schema.yaml";
const CONFIG: &str = ".norn/config.toml";

/// A registered vault holding exactly `files`.
fn a_vault(label: &str, files: &[(&str, &str)]) -> (Sandbox, attach::Vault) {
    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), label).expect("a sandbox");
    let root = sandbox.work_dir().join("attached");
    std::fs::create_dir_all(root.join("vault")).expect("the vault root");
    for (at, content) in files {
        let path = root.join("vault").join(at);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("make the parent");
        std::fs::write(path, content).expect("write a file");
    }
    let vault = attach::Vault::adopt(&root);
    (sandbox, vault)
}

fn params(vault: &attach::Vault, mode: ApplyMode) -> MigrateParams {
    MigrateParams::new(VaultAddress::name(vault.name().clone()), mode)
}

fn read(vault: &attach::Vault, at: &str) -> Option<String> {
    std::fs::read_to_string(vault.path().join(at)).ok()
}

/// The `vault/migration-refused` detail `refused` carries.
fn refusal(refused: &norn_wire::ErrorEnvelope) -> (ControlFile, MigrationRefusal) {
    assert_eq!(
        refused.code(),
        &ReasonCode::VaultMigrationRefused,
        "{refused:?}"
    );
    let ErrorDetail::MigrationRefused { file, reason, .. } = refused.detail() else {
        panic!("refused with {:?}", refused.detail());
    };
    (*file, reason.clone())
}

/// **A vault whose control files are at the versions this build reads is
/// already current**, previewed or applied: nothing is planned, and both
/// files hold exactly the bytes they held, comments and all.
#[test]
fn a_current_vault_is_already_current_and_nothing_is_written() {
    let schema = "version: 1\n# the owner's note\nfields:\n  status:\n    type: text\n";
    let config = "# engines\n[engine.sample]\nlimit = 4\n";
    let (_sandbox, vault) = a_vault(
        "host-migrate-current",
        &[(SCHEMA, schema), (CONFIG, config), ("a.md", "A\n")],
    );
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());

    for mode in [ApplyMode::Preview, ApplyMode::Apply] {
        assert_eq!(
            host.vault_migrate(&params(&vault, mode))
                .expect("a migrate answers"),
            MigrateReport::already_current()
        );
    }
    assert_eq!(read(&vault, SCHEMA).as_deref(), Some(schema));
    assert_eq!(read(&vault, CONFIG).as_deref(), Some(config));
}

/// **A migration never creates a control file**: a vault with neither a
/// schema nor a config is already current, and neither file appears.
#[test]
fn a_vault_with_no_control_files_is_already_current_and_none_is_created() {
    let (_sandbox, vault) = a_vault("host-migrate-none", &[("a.md", "A\n")]);
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());

    for mode in [ApplyMode::Preview, ApplyMode::Apply] {
        assert_eq!(
            host.vault_migrate(&params(&vault, mode))
                .expect("a migrate answers"),
            MigrateReport::already_current()
        );
    }
    assert_eq!(read(&vault, SCHEMA), None);
    assert_eq!(read(&vault, CONFIG), None);
}

/// **A migration reads the schema where the registration reads it**: a
/// registration naming a `schema_source` outside the vault, at the version
/// this build reads, is already current, and nothing is written there or at
/// the default path.
#[test]
fn a_schema_source_at_the_current_version_is_already_current() {
    let (sandbox, vault) = a_vault("host-migrate-source-current", &[("a.md", "A\n")]);
    let source = sandbox.work_dir().join("shared/schema.yaml");
    std::fs::create_dir_all(source.parent().expect("a parent")).expect("the folder");
    std::fs::write(&source, "version: 1\n# shared\n").expect("the schema");
    let host = vault.host_reading_schema_from(SchemaSource::new(&source).expect("a source"));
    let _lease = attach::attach_and_wait(&host, vault.name());

    assert_eq!(
        host.vault_migrate(&params(&vault, ApplyMode::Apply))
            .expect("a migrate answers"),
        MigrateReport::already_current()
    );
    assert_eq!(
        std::fs::read_to_string(&source).expect("the source stands"),
        "version: 1\n# shared\n"
    );
    assert_eq!(read(&vault, SCHEMA), None);
}

/// **A control file this build cannot migrate is refused, naming it and
/// why**, and nothing is written: a schema edited on disk to a version ahead
/// of this build, and a config edited to text that is not TOML — each read
/// as it stands now, not as the vault last took it into service.
#[test]
fn a_file_ahead_of_this_build_or_unreadable_is_refused_naming_it() {
    let (_sandbox, vault) = a_vault(
        "host-migrate-refused",
        &[(SCHEMA, "version: 1\n"), ("a.md", "A\n")],
    );
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());

    std::fs::write(vault.path().join(SCHEMA), "version: 2\n").expect("a later schema");
    for mode in [ApplyMode::Preview, ApplyMode::Apply] {
        let refused = host
            .vault_migrate(&params(&vault, mode))
            .expect_err("a schema ahead is refused");
        assert_eq!(
            refusal(&refused),
            (ControlFile::Schema, MigrationRefusal::version_ahead(2, 1))
        );
    }
    assert_eq!(read(&vault, SCHEMA).as_deref(), Some("version: 2\n"));

    std::fs::write(vault.path().join(SCHEMA), "version: 1\n").expect("the schema back");
    std::fs::write(vault.path().join(CONFIG), "[engine\n").expect("a broken config");
    let refused = host
        .vault_migrate(&params(&vault, ApplyMode::Apply))
        .expect_err("a config that does not read is refused");
    let (file, reason) = refusal(&refused);
    assert_eq!(file, ControlFile::Config);
    assert!(
        matches!(&reason, MigrationRefusal::Unreadable { detail, .. } if detail.contains("TOML")),
        "{reason:?}"
    );
    assert_eq!(read(&vault, CONFIG).as_deref(), Some("[engine\n"));
}

/// **A vault address naming no registration is refused as every
/// vault-scope request refuses it.**
#[test]
fn a_name_this_host_serves_no_vault_under_is_refused() {
    let (_sandbox, vault) = a_vault("host-migrate-unknown", &[("a.md", "A\n")]);
    let host = vault.host();
    let refused = host
        .vault_migrate(&MigrateParams::new(
            VaultAddress::name(norn_wire::VaultName::new("elsewhere").expect("a name")),
            ApplyMode::Preview,
        ))
        .expect_err("an unknown vault is refused");
    assert_eq!(refused.code(), &ReasonCode::HostUnknownVault);
}

/// The test-only ladders: what a grammar change would ship, rewriting both
/// control files, so the whole verb is exercised before a real step exists.
#[cfg(feature = "induced-failure")]
mod over_test_ladders {
    use norn_config::migration::{Format, Ladder, Step};
    use norn_wire::{
        AppliedTarget, ApplyReport, ContentHash, FileState, OperationKind, ResolvedPlan,
        TargetResult,
    };

    use super::*;

    /// A schema at the test ladder's first version: its field is `legacy`.
    const OLD_SCHEMA: &str = "version: 1\n# the owner's note\nfields:\n  legacy: # renamed by the step\n    type: text\n";
    /// The same schema at the test ladder's second version.
    const NEW_SCHEMA: &str = "version: 1\n# the owner's note\nfields:\n  status: # renamed by the step\n    type: text\n";
    /// A config at the test ladder's first version: its engine is `old`.
    const OLD_CONFIG: &str = "# engines\n[engine.old]\nlimit = 4 # a bound\n";
    /// The same config at the test ladder's second version.
    const NEW_CONFIG: &str = "# engines\n[engine.new]\nlimit = 4 # a bound\n";

    /// A schema ladder whose first version declares `legacy`, and whose one
    /// step renames it `status`, touching nothing else.
    fn schema_ladder() -> Ladder {
        Ladder {
            format: Format::Yaml,
            current: 2,
            version_of: |text| Ok(if text.contains("legacy:") { 1 } else { 2 }),
            steps: vec![Step {
                from: 1,
                rewrite: |text| text.replacen("legacy:", "status:", 1),
            }],
        }
    }

    /// A config ladder whose first version names the engine `old`, and whose
    /// one step renames it `new`.
    fn config_ladder() -> Ladder {
        Ladder {
            format: Format::Toml,
            current: 2,
            version_of: |text| Ok(if text.contains("[engine.old]") { 1 } else { 2 }),
            steps: vec![Step {
                from: 1,
                rewrite: |text| text.replacen("[engine.old]", "[engine.new]", 1),
            }],
        }
    }

    fn migrate(
        host: &attach::ServingHost,
        vault: &attach::Vault,
        mode: ApplyMode,
    ) -> Result<MigrateReport, norn_wire::ErrorEnvelope> {
        host.vault_migrate_with_ladders(&params(vault, mode), &schema_ladder(), &config_ladder())
    }

    fn previewed(report: MigrateReport) -> ResolvedPlan {
        let MigrateReport::Migrated { report, .. } = report else {
            panic!("a migrate preview answered {report:?}");
        };
        let ApplyReport::Previewed { plan, .. } = *report else {
            panic!("a migrate preview answered {report:?}");
        };
        plan
    }

    /// What each operation of `plan` writes, by role.
    fn writes(plan: &ResolvedPlan) -> Vec<(ControlFile, String)> {
        plan.operations
            .iter()
            .map(|operation| match &operation.kind {
                OperationKind::WriteControlFile { file, content } => (*file, content.clone()),
                kind => panic!("a migration planned {kind:?}"),
            })
            .collect()
    }

    fn hash(text: &str) -> ContentHash {
        ContentHash::new(format!(
            "sha256:{}",
            norn_fs::ContentHash::of(text.as_bytes()).to_hex()
        ))
        .expect("a content hash")
    }

    /// **A preview plans both rewrites as one plan**: one
    /// `write_control_file` per file behind, each the file's own text with
    /// the step's edit alone, each replacing the bytes it was composed from;
    /// two previews plan the same, and neither writes.
    #[test]
    fn a_preview_plans_both_rewrites_as_one_plan_and_writes_nothing() {
        let (_sandbox, vault) = a_vault(
            "host-migrate-preview",
            &[(SCHEMA, OLD_SCHEMA), (CONFIG, OLD_CONFIG), ("a.md", "A\n")],
        );
        let host = vault.host();
        let _lease = attach::attach_and_wait(&host, vault.name());

        let plan = previewed(migrate(&host, &vault, ApplyMode::Preview).expect("a preview"));
        assert_eq!(
            writes(&plan),
            vec![
                (ControlFile::Schema, NEW_SCHEMA.to_string()),
                (ControlFile::Config, NEW_CONFIG.to_string()),
            ]
        );
        let befores: Vec<(&str, &FileState)> = plan
            .transitions
            .iter()
            .map(|transition| (transition.path.as_str(), &transition.before))
            .collect();
        assert_eq!(
            befores,
            vec![
                (CONFIG, &FileState::present(hash(OLD_CONFIG))),
                (SCHEMA, &FileState::present(hash(OLD_SCHEMA))),
            ]
        );
        let again = previewed(migrate(&host, &vault, ApplyMode::Preview).expect("a preview"));
        assert_eq!(again, plan);
        assert_eq!(read(&vault, SCHEMA).as_deref(), Some(OLD_SCHEMA));
        assert_eq!(read(&vault, CONFIG).as_deref(), Some(OLD_CONFIG));
    }

    /// **An apply lands both rewrites and the vault reloads under them**,
    /// and a migration run again is already current, previewed or applied,
    /// with both files byte-identical to what landed.
    #[test]
    fn an_apply_lands_both_rewrites_and_a_rerun_is_already_current() {
        let (_sandbox, vault) = a_vault(
            "host-migrate-apply",
            &[(SCHEMA, OLD_SCHEMA), (CONFIG, OLD_CONFIG), ("a.md", "A\n")],
        );
        let host = vault.host();
        let _lease = attach::attach_and_wait(&host, vault.name());

        let MigrateReport::Migrated {
            report,
            reload_refused,
            ..
        } = migrate(&host, &vault, ApplyMode::Apply).expect("an apply")
        else {
            panic!("a migrate apply answered another outcome");
        };
        assert_eq!(reload_refused, None);
        let ApplyReport::Applied { targets, .. } = *report else {
            panic!("a migrate apply answered {report:?}");
        };
        let path = |at: &str| norn_wire::DocumentPath::new(at).expect("a vault path");
        assert_eq!(
            targets,
            vec![
                AppliedTarget::new(path(CONFIG), TargetResult::Wrote),
                AppliedTarget::new(path(SCHEMA), TargetResult::Wrote),
            ]
        );
        assert_eq!(read(&vault, SCHEMA).as_deref(), Some(NEW_SCHEMA));
        assert_eq!(read(&vault, CONFIG).as_deref(), Some(NEW_CONFIG));
        let active = host
            .inspect(vault.name())
            .expect("the vault is served")
            .active_fingerprints
            .expect("the vault serves fingerprints");
        assert_eq!(
            active.schema,
            norn_fs::ContentHash::of(NEW_SCHEMA.as_bytes())
        );

        for mode in [ApplyMode::Preview, ApplyMode::Apply] {
            assert_eq!(
                migrate(&host, &vault, mode).expect("a migrate answers"),
                MigrateReport::already_current()
            );
        }
        assert_eq!(read(&vault, SCHEMA).as_deref(), Some(NEW_SCHEMA));
        assert_eq!(read(&vault, CONFIG).as_deref(), Some(NEW_CONFIG));
    }

    /// **Only the files behind are rewritten**: a vault whose schema is
    /// current and whose config is behind plans the config's rewrite alone.
    #[test]
    fn only_the_files_behind_are_rewritten() {
        let (_sandbox, vault) = a_vault(
            "host-migrate-config-only",
            &[(SCHEMA, NEW_SCHEMA), (CONFIG, OLD_CONFIG)],
        );
        let host = vault.host();
        let _lease = attach::attach_and_wait(&host, vault.name());

        let plan = previewed(migrate(&host, &vault, ApplyMode::Preview).expect("a preview"));
        assert_eq!(
            writes(&plan),
            vec![(ControlFile::Config, NEW_CONFIG.to_string())]
        );
    }

    /// The schema `vault` reads: its active fingerprint.
    fn active_schema(host: &attach::ServingHost, vault: &attach::Vault) -> norn_fs::ContentHash {
        host.inspect(vault.name())
            .expect("the vault is served")
            .active_fingerprints
            .expect("the vault serves fingerprints")
            .schema
    }

    /// **A schema read from a `schema_source` outside the vault is rewritten
    /// where it lives** (ADR 0034): the plan names the schema's role at its
    /// canonical path, the apply lands the rewrite at the source, nothing
    /// is written at the default path, the vault reloads under what landed,
    /// and a migration run again is already current.
    #[test]
    fn a_schema_source_outside_the_vault_is_rewritten_where_it_lives() {
        let (sandbox, vault) = a_vault("host-migrate-source-outside", &[("a.md", "A\n")]);
        let source = sandbox.work_dir().join("shared/schema.yaml");
        std::fs::create_dir_all(source.parent().expect("a parent")).expect("the folder");
        std::fs::write(&source, OLD_SCHEMA).expect("the schema");
        let host = vault.host_reading_schema_from(SchemaSource::new(&source).expect("a source"));
        let _lease = attach::attach_and_wait(&host, vault.name());

        let plan = previewed(migrate(&host, &vault, ApplyMode::Preview).expect("a preview"));
        assert_eq!(
            writes(&plan),
            vec![(ControlFile::Schema, NEW_SCHEMA.to_string())]
        );
        assert_eq!(
            plan.transitions
                .iter()
                .map(|transition| (transition.path.as_str(), &transition.before))
                .collect::<Vec<_>>(),
            vec![(SCHEMA, &FileState::present(hash(OLD_SCHEMA)))]
        );

        let MigrateReport::Migrated { reload_refused, .. } =
            migrate(&host, &vault, ApplyMode::Apply).expect("an apply")
        else {
            panic!("a migrate apply answered another outcome");
        };
        assert_eq!(reload_refused, None);
        assert_eq!(
            std::fs::read_to_string(&source).expect("the source stands"),
            NEW_SCHEMA
        );
        assert_eq!(read(&vault, SCHEMA), None);
        assert_eq!(
            active_schema(&host, &vault),
            norn_fs::ContentHash::of(NEW_SCHEMA.as_bytes())
        );
        assert_eq!(
            migrate(&host, &vault, ApplyMode::Apply).expect("a migrate answers"),
            MigrateReport::already_current()
        );
    }

    /// **A schema read from a `schema_source` inside the vault is rewritten
    /// where it lives**, and nothing is written at the default path.
    #[test]
    fn a_schema_source_inside_the_vault_is_rewritten_where_it_lives() {
        let (_sandbox, vault) = a_vault(
            "host-migrate-source-inside",
            &[("schemas/notes.yaml", OLD_SCHEMA), ("a.md", "A\n")],
        );
        let source = vault.path().join("schemas/notes.yaml");
        let host = vault.host_reading_schema_from(SchemaSource::new(&source).expect("a source"));
        let _lease = attach::attach_and_wait(&host, vault.name());

        let MigrateReport::Migrated { reload_refused, .. } =
            migrate(&host, &vault, ApplyMode::Apply).expect("an apply")
        else {
            panic!("a migrate apply answered another outcome");
        };
        assert_eq!(reload_refused, None);
        assert_eq!(
            read(&vault, "schemas/notes.yaml").as_deref(),
            Some(NEW_SCHEMA)
        );
        assert_eq!(read(&vault, SCHEMA), None);
        assert_eq!(
            active_schema(&host, &vault),
            norn_fs::ContentHash::of(NEW_SCHEMA.as_bytes())
        );
    }

    /// **A step whose rewrite would lose a comment is refused, naming the
    /// file and the comment**, and nothing of either file is written.
    #[test]
    fn a_step_that_would_lose_a_comment_is_refused_and_nothing_is_written() {
        let (_sandbox, vault) = a_vault(
            "host-migrate-comment-lost",
            &[(SCHEMA, OLD_SCHEMA), (CONFIG, OLD_CONFIG)],
        );
        let host = vault.host();
        let _lease = attach::attach_and_wait(&host, vault.name());
        let mut careless = schema_ladder();
        careless.steps[0].rewrite =
            |text| text.replacen("  legacy: # renamed by the step", "  status:", 1);

        for mode in [ApplyMode::Preview, ApplyMode::Apply] {
            let refused = host
                .vault_migrate_with_ladders(&params(&vault, mode), &careless, &config_ladder())
                .expect_err("a lost comment is refused");
            assert_eq!(
                refusal(&refused),
                (
                    ControlFile::Schema,
                    MigrationRefusal::comment_lost("# renamed by the step")
                )
            );
        }
        assert_eq!(read(&vault, SCHEMA).as_deref(), Some(OLD_SCHEMA));
        assert_eq!(read(&vault, CONFIG).as_deref(), Some(OLD_CONFIG));
    }

    /// Where the config ladder's meddling step writes: the config of the
    /// one case that runs it.
    static MIDWAY: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    /// What another writer leaves in the config while the migration runs.
    const THEIRS: &str =
        "# engines\n[engine.old]\nlimit = 4 # a bound\n# theirs, written mid-migration\n";

    /// **A file another writer changes between the migration's read and its
    /// plan is refused as changed**, and nothing is written: the config
    /// step, run after both files were read and before the plan is
    /// previewed, stands in for that writer. The other writer's bytes stand,
    /// and the schema, behind as well, is not rewritten either.
    #[test]
    fn a_file_changed_after_the_migration_read_it_is_refused_as_changed() {
        let (_sandbox, vault) = a_vault(
            "host-migrate-changed-midway",
            &[(SCHEMA, OLD_SCHEMA), (CONFIG, OLD_CONFIG)],
        );
        let host = vault.host();
        let _lease = attach::attach_and_wait(&host, vault.name());
        MIDWAY
            .set(vault.path().join(CONFIG))
            .expect("one case meddles");
        let mut meddling = config_ladder();
        meddling.steps[0].rewrite = |text| {
            std::fs::write(MIDWAY.get().expect("the config's path"), THEIRS)
                .expect("the other writer's edit");
            text.replacen("[engine.old]", "[engine.new]", 1)
        };

        let refused = host
            .vault_migrate_with_ladders(
                &params(&vault, ApplyMode::Apply),
                &schema_ladder(),
                &meddling,
            )
            .expect_err("a file changed since it was read is refused");
        assert_eq!(
            refusal(&refused),
            (ControlFile::Config, MigrationRefusal::changed())
        );
        assert_eq!(read(&vault, CONFIG).as_deref(), Some(THEIRS));
        assert_eq!(read(&vault, SCHEMA).as_deref(), Some(OLD_SCHEMA));
    }
}
