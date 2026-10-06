//! The `vault migrate` verb: a registered vault's control files brought up
//! to the versions this build reads, planned and applied through the one
//! planner and the one applier.
//!
//! **What a migration reads, and what it plans.** It reads the vault schema
//! where the registration reads it — the default `.norn/schema.yaml`, or the
//! registration's `schema_source` — and the vault config, each as it stands
//! on disk now, and walks each up its ladder ([`norn_config::migration`]).
//! A file at the version this build reads plans nothing, and so does a file
//! that is not there: a migration never creates one. Each file behind is one
//! `write_control_file` of its rewritten text, which lands where the file was
//! read, a `schema_source` outside the vault included (ADR 0034), and where
//! the vault's shadow home cannot publish into that source's folder the plan
//! does not resolve, naming why. The rewrites of both files are one plan,
//! previewed through the one `apply` seam as an apply would plan and judge
//! it. Where no file is behind, the vault is already current.
//! A file whose version cannot be read, one ahead of this build or with no
//! step from its version, and a rewrite that would lose a comment are refused
//! `vault/migration-refused`, naming the file, before anything is planned.
//!
//! **A rewrite is composed from the bytes it replaces, and guarded by
//! them.** A plan's before-state is read again when it is previewed, so a
//! file another writer changed between the migration's read and the preview
//! is refused as changed rather than planned over: its rewrite was composed
//! from bytes the file no longer holds. An apply plans afresh within the one
//! call, as a preview does, and sends that resolved plan; a file changed
//! after that, before anything landed, refuses the apply by drift, which a
//! migration answers as changed too, never with the refusal's fresh plan,
//! whose whole-file content was composed from the bytes that drifted and
//! would replace the other writer's edit with it. A file changed after the
//! other file's rewrite landed is no refusal: the apply is answered as the
//! applier answers it, `vault/plan-interrupted` naming what landed, since a
//! migration refusal would hide a rewrite that stands on disk.
//!
//! **The vault is reloaded under what landed**, as `init` reloads it: the
//! watcher's control-file facts are discarded by design, and the migration is
//! the caller's explicit act on both files, so once its apply lands it takes
//! the boundary `vault reload` takes, in the same call. A reload that refuses
//! is answered beside the landing.
//!
//! **What a migration costs.** A vault already current costs a read of each
//! control file and its version, and no plan. One behind adds a step per
//! version, the comment comparison of each rewrite with its file, and the
//! preview of one plan over at most two files; an apply that lands adds the
//! reload's re-pin where the schema changed.

use std::path::Path;

use norn_config::migration::Ladder;
use norn_store::Snapshot;
use norn_wire::{
    ApplyMode, ApplyReport, AuthoredPlan, ContentHash, ControlFile, ErrorDetail, ErrorEnvelope,
    MigrateParams, MigrateReport, MigrationRefusal, Operation, OperationKind, PlanDocument,
    RefusedCheck, ResolvedPlan, VaultAddress, VaultName,
};

use crate::address::registered_name;
use crate::apply::{PlanGround, unreadable};
use crate::lifecycle::{EntryOps, Host, ReadSource, SnapshotSource};
use crate::planner::control::{SchemaPlace, control_path, role_at};
use crate::planner::view::{Entry, TreeView, VaultView, wire_hash};

/// The ladder each control file is walked up.
struct Ladders<'a> {
    schema: &'a Ladder,
    config: &'a Ladder,
}

impl Ladders<'_> {
    fn of(&self, file: ControlFile) -> &Ladder {
        match file {
            ControlFile::Schema => self.schema,
            ControlFile::Config => self.config,
        }
    }
}

/// One control file as a migration read it: its bytes' hash, which the
/// plan's before-state must name, and its rewrite.
struct Rewrite {
    file: ControlFile,
    read: ContentHash,
    content: String,
}

/// A control file standing where the vault reads it: its bytes, and their
/// hash, which a plan's before-state names.
struct Standing {
    bytes: Vec<u8>,
    hash: ContentHash,
}

/// What planning a migration came to: its preview, or every file current.
enum Planned {
    Previewed(Box<ResolvedPlan>, norn_wire::Forecast),
    Current,
}

/// The refusal of `file`, for `reason`, with a message saying so.
fn refused(file: ControlFile, reason: MigrationRefusal) -> ErrorEnvelope {
    let role = match file {
        ControlFile::Schema => "schema",
        ControlFile::Config => "config",
    };
    ErrorEnvelope::new(
        format!("the vault {role} cannot be migrated as it stands, so nothing was planned"),
        ErrorDetail::migration_refused(file, reason),
    )
}

/// The refusal an apply of a migration's own plan answers where `refusal`
/// says a control file drifted after it was planned and before anything
/// landed: the file changed, and the migration composed from what it held
/// is not sent on. Any other answer is given as it is — an interruption
/// after a rewrite landed included, which names what landed.
fn changed_since(refusal: ErrorEnvelope) -> ErrorEnvelope {
    let ErrorDetail::PlanRefused { checks, .. } = refusal.detail() else {
        return refusal;
    };
    let drifted = checks.iter().find_map(|check| match check {
        RefusedCheck::Drifted { path, .. } => role_at(path.as_str()),
        _ => None,
    });
    match drifted {
        Some(file) => refused(file, MigrationRefusal::changed()),
        None => refusal,
    }
}

impl<O> Host<O>
where
    O: EntryOps,
    <O::Attachment as SnapshotSource>::Reader: ReadSource<Snapshot = Snapshot>,
{
    /// Answer a `vault migrate`: preview or apply the rewrite of every
    /// control file of the registered vault `params` names that is behind
    /// the version this build reads, or answer that none is.
    ///
    /// An apply plans afresh, as a preview does, sends that resolved plan,
    /// and then reloads the vault under what landed, as `vault reload` does.
    /// A reload that refuses is answered beside the landing, the files on
    /// disk and reported as a reload pending.
    ///
    /// **Answered synchronously**, as `init` is: the call waits for its apply
    /// and its reload, and answers once both have.
    pub fn vault_migrate(&self, params: &MigrateParams) -> Result<MigrateReport, ErrorEnvelope> {
        self.migrate_up(
            params,
            &Ladders {
                schema: &Ladder::schema(),
                config: &Ladder::config(),
            },
        )
    }

    /// Answer a `vault migrate` over the ladders `schema` and `config`
    /// rather than the ones this build ships.
    ///
    /// **Behind `induced-failure`, with the rest of the harness-reachable
    /// surface.** The shipped ladders hold no step, so no vault a suite can
    /// build is behind them; a suite reaches a real rewrite — planned,
    /// applied, reloaded and run again — through ladders of its own.
    #[cfg(feature = "induced-failure")]
    pub fn vault_migrate_with_ladders(
        &self,
        params: &MigrateParams,
        schema: &Ladder,
        config: &Ladder,
    ) -> Result<MigrateReport, ErrorEnvelope> {
        self.migrate_up(params, &Ladders { schema, config })
    }

    fn migrate_up(
        &self,
        params: &MigrateParams,
        ladders: &Ladders<'_>,
    ) -> Result<MigrateReport, ErrorEnvelope> {
        let name = registered_name(&params.vault)?.clone();
        let (plan, forecast) = match self.plan_migration(&name, &params.vault, ladders)? {
            Planned::Current => return Ok(MigrateReport::already_current()),
            Planned::Previewed(plan, forecast) => (*plan, forecast),
        };
        match params.mode {
            ApplyMode::Preview => Ok(MigrateReport::migrated(ApplyReport::previewed(
                plan, forecast,
            ))),
            ApplyMode::Apply => {
                let applied = self
                    .admit_apply(&name, PlanDocument::resolved(plan))?
                    .wait()
                    .map_err(changed_since)?;
                // What landed stands whatever the reload answers, so a
                // refused reload is answered beside the landing; a host gone
                // before its reload ran answers the landing alone, since the
                // next attach reads what the apply wrote.
                if let Err(refusal) = self.reload(&name)
                    && let Ok(refused) = refusal.answer(&name)
                {
                    return Ok(MigrateReport::migrated_reload_refused(
                        applied.report,
                        refused,
                    ));
                }
                Ok(MigrateReport::migrated(applied.report))
            }
        }
    }

    /// Plan the migration of `name`, addressed as `vault`: each control file
    /// read where the vault reads it and walked up its ladder, the rewrites
    /// of those behind previewed as one plan through the one `apply` seam,
    /// and the plan's before-states held to the bytes each rewrite was
    /// composed from.
    fn plan_migration(
        &self,
        name: &VaultName,
        vault: &VaultAddress,
        ladders: &Ladders<'_>,
    ) -> Result<Planned, ErrorEnvelope> {
        let mut rewrites = Vec::new();
        for (file, read) in self.control_files(name)? {
            let Some(standing) = read else {
                continue;
            };
            match ladders.of(file).migrate(&standing.bytes) {
                Ok(None) => {}
                Ok(Some(content)) => rewrites.push(Rewrite {
                    file,
                    read: standing.hash,
                    content,
                }),
                Err(reason) => return Err(refused(file, reason)),
            }
        }
        if rewrites.is_empty() {
            return Ok(Planned::Current);
        }
        let authored = AuthoredPlan::new(
            vault.clone(),
            rewrites
                .iter()
                .map(|rewrite| {
                    Operation::new(OperationKind::write_control_file(
                        rewrite.file,
                        rewrite.content.clone(),
                    ))
                })
                .collect(),
        );
        let ApplyReport::Previewed { plan, forecast, .. } = self
            .preview(name, PlanDocument::operations(authored))?
            .report
        else {
            unreachable!("a preview answers a previewed plan or a refusal");
        };
        // A file another writer changed since the migration read it makes
        // the rewrite one composed from bytes it no longer holds.
        for rewrite in &rewrites {
            let path = control_path(rewrite.file);
            let read_again = plan
                .transitions
                .iter()
                .find(|transition| transition.path == *path)
                .and_then(|transition| transition.before.hash());
            if read_again != Some(&rewrite.read) {
                return Err(refused(rewrite.file, MigrationRefusal::changed()));
            }
        }
        Ok(Planned::Previewed(Box::new(plan), forecast))
    }

    /// Each control file of `name` as it stands now — its bytes and their
    /// hash, or `None` where nothing stands — on a hold of the entry and the
    /// ground its coverage stands on, as the gate hold that established the
    /// hold read it, so the read takes the gate no second time.
    ///
    /// The schema is read where a plan over that ground reads it
    /// ([`PlanGround::schema`]), never by the registry's record, which may
    /// name a file the coverage has not taken in yet: the bytes a rewrite is
    /// composed from and the before-state its plan names are one file's.
    /// The default schema and the config are read as the planner reads a
    /// control target ([`VaultView::control_entry`]); a `schema_source` is
    /// read as a reload reads it, which refuses a source naming nothing.
    /// Something at a control file's path that is no file reads as no
    /// version, which the migration refuses naming it.
    fn control_files(
        &self,
        name: &VaultName,
    ) -> Result<[(ControlFile, Option<Standing>); 2], ErrorEnvelope> {
        let hold = self
            .begin_read(name)
            .map_err(|refusal| refusal.answer(name))?;
        hold.reading().answer_reading(name)?;
        let ground = hold.plan_ground().ok_or_else(|| {
            ErrorEnvelope::new(
                "the entry records no ground to plan against, so the migration was not planned",
                ErrorDetail::reader_unavailable(
                    "the entry's ops record no plan ground over its coverage",
                ),
            )
        })?;
        ground.standing(name)?;
        let schema = schema_standing(name, ground)?;
        let config = in_vault(name, ground, ControlFile::Config)?;
        drop(hold);
        Ok([(ControlFile::Schema, schema), (ControlFile::Config, config)])
    }
}

/// The control file `file` at the path its role lives at in the vault on
/// `ground`, read as the planner reads one.
fn in_vault(
    name: &VaultName,
    ground: &PlanGround,
    file: ControlFile,
) -> Result<Option<Standing>, ErrorEnvelope> {
    let view = TreeView::open(&ground.root, &ground.exclusions, &ground.schema)
        .map_err(|error| unreadable(name, error))?;
    let path = control_path(file);
    let Ok(identity) = view.normalizer().normalize(Path::new(path.as_str())) else {
        return Err(refused(
            file,
            MigrationRefusal::unreadable(format!("`{path}` names no place in the vault")),
        ));
    };
    match view
        .control_entry(&identity)
        .map_err(|error| unreadable(name, error))?
    {
        Entry::Document { hash, body, .. } => Ok(Some(Standing {
            bytes: body.held().expect("a control file is read whole").to_vec(),
            hash,
        })),
        Entry::Absent { .. } => Ok(None),
        Entry::Folder => Err(refused(
            file,
            MigrationRefusal::unreadable(format!("a folder stands at `{path}`")),
        )),
        Entry::Blocked { detail, .. } => Err(refused(file, MigrationRefusal::unreadable(detail))),
    }
}

/// The schema of the vault on `ground`, read where a plan over that ground
/// reads and writes it ([`PlanGround::schema`]): the default as the planner
/// reads a control target, nothing where none stands; a `schema_source`,
/// inside the vault or outside it, as a reload reads one, refused where it
/// names nothing.
fn schema_standing(
    name: &VaultName,
    ground: &PlanGround,
) -> Result<Option<Standing>, ErrorEnvelope> {
    let unreadable_source =
        |detail: String| refused(ControlFile::Schema, MigrationRefusal::unreadable(detail));
    let (anchor, file) = match &ground.schema {
        place if *place == SchemaPlace::default() => {
            return in_vault(name, ground, ControlFile::Schema);
        }
        SchemaPlace::InVault(relative) => (ground.root.as_path(), relative.as_path()),
        SchemaPlace::Outside { folder, name, .. } => (folder.as_path(), name.as_path()),
        SchemaPlace::NoFile(source) => {
            return Err(unreadable_source(format!(
                "the schema source `{}` names no file",
                source.display()
            )));
        }
    };
    let read = norn_fs::read_and_hash(anchor, file).map_err(|refusal| {
        unreadable_source(format!("the schema source cannot be read: {refusal}"))
    })?;
    let (bytes, hash) = read.into_parts();
    Ok(Some(Standing {
        bytes,
        hash: wire_hash(hash),
    }))
}

#[cfg(test)]
mod tests {
    use norn_wire::{FileState, Forecast, RootIdentity};

    use super::*;

    fn refused_by(checks: Vec<RefusedCheck>) -> ErrorEnvelope {
        let vault = VaultAddress::name(VaultName::new("notes").expect("a name"));
        ErrorEnvelope::new(
            "refused",
            ErrorDetail::plan_refused(
                ResolvedPlan::new(
                    vault,
                    RootIdentity::from_device_and_inode(1, 2),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                ),
                Forecast::new(Vec::new(), Vec::new(), Vec::new()),
                checks,
                Vec::new(),
                Vec::new(),
                Vec::new(),
            ),
        )
    }

    /// **An apply of a migration refused for a control file that drifted is
    /// answered as that file changed**, never with the refusal's fresh plan,
    /// whose content was composed from the bytes that drifted; a refusal
    /// naming no drifted control file is answered as it is.
    #[test]
    fn a_drifted_control_file_is_answered_as_changed() {
        for file in [ControlFile::Schema, ControlFile::Config] {
            let drifted = refused_by(vec![RefusedCheck::drifted(
                control_path(file).clone(),
                FileState::absent(),
            )]);
            assert_eq!(
                changed_since(drifted).detail(),
                &ErrorDetail::migration_refused(file, MigrationRefusal::changed())
            );
        }
        let elsewhere = refused_by(vec![RefusedCheck::name_taken(
            norn_wire::DocumentPath::new("a.md").expect("a path"),
        )]);
        assert_eq!(changed_since(elsewhere.clone()), elsewhere);
        let unwritten = ErrorEnvelope::new("held", ErrorDetail::reload_busy());
        assert_eq!(changed_since(unwritten.clone()), unwritten);
    }
}
